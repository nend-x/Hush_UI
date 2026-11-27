// Tray mirror — mirrors the NATIVE Windows system tray (notification
// area) into the widgets table.
//
// Uses systray-util (extracted from the zebar project): its TraySpy
// creates a message window that watches the tray's message stream and
// reads the tray toolbars, so what we get are the real tray icons — same
// bitmaps, same tooltips, same visibility state — and clicks are
// forwarded to the owning windows exactly like clicking the native tray.
//
// Threading model:
//   - TraySpy::new() spawns its own message-pump thread internally.
//   - One tokio task owns the `Systray` and `tokio::select!`s between
//     icon events (from the spy) and click requests (queued by the
//     `tray_send_action` command). `send_action` needs `&mut Systray`,
//     so all access funnels through this single owner; both select arms
//     are cancel-safe (the mpsc recv is, and `events()` only awaits at
//     cancel-safe recv points — anything it processed is already in
//     `Systray`'s own map).
//   - A plain `Mutex<Vec<_>>` mirror of the current payloads backs the
//     `tray_get_icons` command (snapshot for a table opened later —
//     `tray://icons` events reach live pages anyway).
//
// Events and payload: every add/update/remove emits one `tray://icons`
// event carrying the FULL list (id, tooltip, PNG bytes base64,
// visibility). The frontend is a dumb re-render — no client-side diffing
// to get wrong. Hidden (overflow) icons are included in payloads with
// `is_visible: false` so a future overflow flyout is possible without
// touching the backend; the widget only renders visible ones.
//
// Only compiled on Windows (win32/mod.rs is cfg(windows), like dimmer.rs).

#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod imp {
    use std::{
        str::FromStr,
        sync::{
            atomic::{AtomicBool, Ordering},
            Mutex, OnceLock,
        },
    };

    use base64::Engine;
    use systray_util::{Systray, SystrayEvent, SystrayIcon, SystrayIconAction, StableId};
    use tauri::{AppHandle, Emitter};
    use tokio::sync::mpsc;

    use crate::app_state::TrayIconPayload;

    /// Queue click requests into the owner task (set by `start`).
    static ACTION_TX: OnceLock<mpsc::UnboundedSender<(StableId, SystrayIconAction)>> =
        OnceLock::new();

    /// Current payload mirror (discovery order preserved).
    static ICONS: Mutex<Vec<TrayIconPayload>> = Mutex::new(Vec::new());

    /// Guard against double-start.
    static STARTED: AtomicBool = AtomicBool::new(false);

    /// Start the tray spy + event loop. Call once at startup.
    pub fn start(app: AppHandle) {
        if STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let _ = ACTION_TX.set(tx);
        tauri::async_runtime::spawn(async move {
            if let Err(e) = run(app, rx).await {
                log::error!("tray: event loop failed: {e}");
            }
        });
    }

    /// The owner task: select over tray events and click requests.
    async fn run(
        app: AppHandle,
        mut action_rx: mpsc::UnboundedReceiver<(StableId, SystrayIconAction)>,
    ) -> systray_util::Result<()> {
        let mut systray = Systray::new()?;
        log::info!("tray: spy started");
        loop {
            tokio::select! {
                event = systray.events() => {
                    match event {
                        Some(ev) => {
                            apply_event(ev);
                            emit_snapshot(&app);
                        }
                        // Spy channel closed — nothing left to mirror.
                        None => break,
                    }
                }
                request = action_rx.recv() => {
                    match request {
                        Some((id, action)) => {
                            if let Err(e) = systray.send_action(&id, &action) {
                                // InoperableIcon usually means the owning
                                // process died; the spy's remove event cleans
                                // the mirror shortly after.
                                log::warn!("tray: send_action failed: {e}");
                            }
                        }
                        None => break,
                    }
                }
            }
        }
        Ok(())
    }

    /// Fold a spy event into the mirror.
    fn apply_event(ev: SystrayEvent) {
        match ev {
            SystrayEvent::IconAdd(icon) | SystrayEvent::IconUpdate(icon) => {
                let payload = to_payload(&icon);
                let mut icons = ICONS.lock().unwrap();
                match icons.iter_mut().find(|p| p.id == payload.id) {
                    // Update in place — keeps discovery order stable so the
                    // widget doesn't reshuffle icons on every tooltip change.
                    Some(existing) => *existing = payload,
                    None => icons.push(payload),
                }
            }
            SystrayEvent::IconRemove(id) => {
                let id = id.to_string();
                ICONS.lock().unwrap().retain(|p| p.id != id);
            }
        }
    }

    fn to_payload(icon: &SystrayIcon) -> TrayIconPayload {
        let png_base64 = icon
            .to_image_format(systray_util::ImageFormat::Png)
            .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes))
            .unwrap_or_default();
        TrayIconPayload {
            id: icon.stable_id.to_string(),
            tooltip: icon.tooltip.clone(),
            png_base64,
            is_visible: icon.is_visible,
        }
    }

    fn emit_snapshot(app: &AppHandle) {
        let icons = current_icons();
        if let Err(e) = app.emit("tray://icons", icons) {
            log::warn!("tray: emit failed: {e}");
        }
    }

    /// Snapshot for `tray_get_icons`.
    pub fn current_icons() -> Vec<TrayIconPayload> {
        ICONS.lock().unwrap().clone()
    }

    /// Queue a click for one icon. `action`: "left" | "right" | "middle"
    /// | "double". Returns Err for unknown ids/actions or when the spy
    /// never started.
    pub fn send_action(id: &str, action: &str) -> Result<(), String> {
        let tx = ACTION_TX
            .get()
            .ok_or_else(|| "tray not started".to_string())?;
        let stable_id = StableId::from_str(id).map_err(|e| e.to_string())?;
        let action = match action {
            "left" => SystrayIconAction::LeftClick,
            "right" => SystrayIconAction::RightClick,
            "middle" => SystrayIconAction::MiddleClick,
            "double" => SystrayIconAction::LeftDoubleClick,
            other => return Err(format!("unknown tray action: {other}")),
        };
        tx.send((stable_id, action)).map_err(|e| e.to_string())
    }
}

#[cfg(windows)]
pub use imp::{current_icons, send_action, start};
