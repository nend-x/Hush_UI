// Hush_UI — Tauri backend entry point
//
// Windows:
//   - taskbar: bottom, topmost, 44px, frameless, transparent, no-activate, registered as AppBar
//   - launcher: fullscreen overlay, hidden by default, opened when the user
//     taps the Win key alone — the `prevent-alt-win-menu` crate installs a
//     low-level keyboard hook in-process that both suppresses the native
//     Start menu and fires our `on_released` callback so we can toggle the
//     launcher (the old `flatwin.exe` AutoHotkey v2 helper + HTTP :2290
//     /toggle bridge has been removed); opening the launcher also minimizes
//     every visible window (show-desktop effect)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app_state;
pub mod crash_handler;
mod desktop_watcher;
mod elevation;
mod hide_taskbar;
mod persist;
#[cfg(windows)]
mod start_menu_killer;
#[cfg(windows)]
mod win32;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{Emitter, Manager, WindowEvent};

use app_state::AppState;

// ===== Launcher visibility state machine =====
//
// Logical open/close state for the launcher, decoupled from raw window
// visibility: during the close animation the window is STILL visible (the
// frontend plays the collapse before we hide the window), so toggling off
// `is_visible()` would mis-route a Win-key press that lands mid-animation.
//
// - LAUNCHER_OPEN — the launcher is logically open (or opening).
// - CLOSE_SEQ     — monotonic sequence for pending delayed hides. A show
//                   request bumps it, which cancels the pending hide.
//
// The frontend mirrors this with its own session tokens (see
// src/launcher/main.ts) — together they make rapid toggling (Win-key spam)
// glitch-free: every request cleanly cancels whatever is in flight.
static LAUNCHER_OPEN: AtomicBool = AtomicBool::new(false);
static CLOSE_SEQ: AtomicU64 = AtomicU64::new(0);

// ===== Tables state (pie Win-key picker) =====
//
// TABLES_HOVERED mirrors the button the picker webview currently has under
// the mouse. The low-level hook reads it on Win-up (hold-release gesture) —
// the picker frontend keeps it fresh via the `set_tables_hover` command:
//   0 = nothing hovered (release dismisses the picker)
//   1 = taskbar table   2 = settings table
//   3 = widgets table   4 = hushlight table   5 = desktop table
// The cursor can move between hover and Win-up by a few px; the frontend
// re-reports on every mouseenter/leave, so the race window is tiny and a
// missed report degrades gracefully to "dismiss".
static TABLES_HOVERED: AtomicI32 = AtomicI32::new(0);

// Monotonic picker generation — guards the animated dismiss delay.
static TABLES_SEQ: AtomicU64 = AtomicU64::new(0);

// Render handshake for the picker reveal (0.3.3): the webview sets this via
// the `tables_rendered` command once the pie has been laid out at the NEW
// anchor and a frame has been committed. The reveal waits (bounded) for it,
// so the window can never uncloak while still showing the stale last frame
// from the previous open — that stale frame is what made the pie appear at
// its old dismissal spot and teleport to the cursor a moment later.
static TABLES_RENDERED: AtomicBool = AtomicBool::new(false);
// Bounded wait for the render handshake before revealing anyway (webview
// wedged fallback — a stale frame still beats no pie at all).
const TABLES_RENDER_WAIT_MS: u64 = 150;

// Hide handshake (0.4.0) — the webview sets this via the `tables_hidden`
// command once the pie content is blanked (root visibility:hidden) and a
// frame with that blank state has been committed. The hide paths wait
// (bounded) for it BEFORE cloaking + suspending the controller, so the
// surface's LAST presented frame is always blank. That is the piece 0.3.3
// was missing: the instant-dismiss path suspended the controller before the
// page could render anything, freezing the full pie (at the old anchor)
// into the surface — and the show handshake's double-rAF confirm can fire
// before the compositor actually presents the re-laid-out frame, so the
// uncloak still raced a one-frame "pie at the old position" flash. With a
// blank last frame, resuming on the next open re-presents NOTHING visible
// — the stale frame can no longer exist, regardless of handshake timing.
static TABLES_HIDDEN: AtomicBool = AtomicBool::new(false);
// Bounded waits for the hide handshake (webview wedged fallback — cloaking
// a never-confirmed window just means the stale frame is the blank-in-
// progress one; both bounds exceed the normal confirm latency by a lot).
const TABLES_HIDE_ANIMATED_FLOOR_MS: u64 = 280; // pop-out must finish
const TABLES_HIDE_ANIMATED_CAP_MS: u64 = 340;
const TABLES_HIDE_INSTANT_CAP_MS: u64 = 120;

// Logical cursor position (primary-monitor-relative, CSS px) captured when
// the picker opened — the taskbar table spawns next to it.
static TABLES_CURSOR: Mutex<(f64, f64)> = Mutex::new((0.0, 0.0));

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Install the crash handler BEFORE anything else — before logger init,
    // before Tauri builder, before any code that might panic. The handler
    // spawns a separate `hush_ui.exe --crash-report <file>` subprocess so the
    // crash dialog survives the parent's death. See `crash_handler.rs`.
    crash_handler::install();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    log::info!("Hush_UI starting up…");

    // UAC on launch: if not elevated, pop the standard Windows UAC prompt
    // via ShellExecuteW "runas". Two outcomes:
    //
    //   - Accepted  — an elevated relaunch is in flight; this (non-elevated)
    //     process exits and the elevated process takes over.
    //   - Declined — the app STILL LAUNCHES, but in non-elevated mode. The
    //     setup window surfaces a warning that the brightness dimmer may
    //     not cover system/elevated apps (UIPI keeps a non-elevated overlay
    //     below elevated windows; some elevated surfaces also block
    //     click-through rendering above them).
    #[cfg(windows)]
    if !elevation::is_elevated() {
        match elevation::request_elevation() {
            elevation::ElevationOutcome::Accepted => {
                // The elevated relaunch is in flight. Exit this process
                // immediately — the elevated process will show the setup
                // window and run normally.
                log::info!("Exiting non-elevated instance — elevated relaunch is in flight");
                std::process::exit(0);
            }
            elevation::ElevationOutcome::Declined => {
                // User declined the UAC prompt — keep running without
                // elevation.
                elevation::UAC_DECLINED.store(true, std::sync::atomic::Ordering::SeqCst);
                log::warn!(
                    "UAC declined — launching non-elevated; the brightness dimmer may not work on system apps"
                );
            }
            elevation::ElevationOutcome::AlreadyElevated => {
                // Shouldn't happen (we checked is_elevated above), but
                // continue if it does.
            }
        }
    }

    // Check for -rs flag (reset/clean start)
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "-rs") {
        log::info!("Clean start requested (-rs flag) — deleting all config files");
        wipe_configs();
    }

    // Native taskbar hider — the in-process monitor keeps Shell_TrayWnd /
    // Shell_SecondaryTrayWnd at alpha 0. Still needed: the custom shell
    // taskbar is gone, but Hush_UI is still a shell replacement and the
    // native taskbar must stay out of the way.
    #[cfg(windows)]
    hide_taskbar::start();

    // Start the brightness dimmer overlay thread (systemless software dim —
    // a click-through black layered window; see win32::dimmer). The persisted
    // dim level is applied once the overlay window exists.
    #[cfg(windows)]
    {
        let saved = persist::load_settings().dimmer_level;
        if saved > 0.0 {
            win32::dimmer::set_level(saved);
        }
        win32::dimmer::start();
    }

    let state = Arc::new(Mutex::new(AppState::new()));

    // The app is usually elevated at this point (we requested UAC above and
    // only continued when the user declined).

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(state.clone())
        .setup(move |app| {
            let handle = app.handle().clone();

            // First-run tutorial — a small looping animation window teaches
            // the Win-hold pie gesture on a genuinely first start (or after
            // "Reset config", which wipes tutorial_seen.json too).
            #[cfg(windows)]
            if !persist::load_tutorial_seen() {
                let tapp = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    show_tutorial(&tapp);
                });
            }

            // Returning users just get the usual startup notification.
            if persist::load_tutorial_seen() {
                let napp = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    show_notification(&napp, "Hush_UI", "Hello world!", 2000);
                });
            }

            // The shell taskbar was REMOVED — the bottom bar (and its AppBar
            // edge reservation) no longer exists. Only the taskbar TABLE
            // (Win-hold pie → strip) remains. The "taskbar" webview window is
            // still declared in tauri.conf.json (hidden at startup) so older
            // setups don't break, but nothing ever shows it.
            if let Some(taskbar) = app.get_webview_window("taskbar") {
                win32::window::hide_window(&taskbar);
            }

            if let Some(launcher) = app.get_webview_window("launcher") {
                win32::window::hide_window(&launcher);
            }

            // Tables (pie Win-key picker) + the transient taskbar table
            // strip are mouse-only overlays: NOACTIVATE + TOOLWINDOW so they
            // can never steal focus from the app the user was in while
            // holding Win. The settings/widgets tables are interactive
            // (text inputs, drag-to-move) and stay focusable.
            #[cfg(windows)]
            {
                if let Some(tables) = app.get_webview_window("tables") {
                    let _ = win32::window::apply_no_activate(&tables);
                    // Pre-fit the picker to the monitor and switch to the
                    // layered show/hide scheme (window stays visible with
                    // alpha 0 + click-through forever) so the WebView2
                    // composition never tears down/re-attaches — that
                    // re-attach was flashing the default light-blue
                    // background for a frame on every open.
                    prefit_tables_window(&tables);
                    let _ = win32::window::set_picker_visible(&tables, false);
                }
                if let Some(strip) = app.get_webview_window("table-taskbar") {
                    win32::window::hide_window(&strip);
                    let _ = win32::window::apply_no_activate(&strip);
                }
            }

            // 0.3.2: suspend every WebView2 controller at boot. wry boots
            // webviews as IsVisible(true) even when the window is hidden
            // (WebViewAttributes::default().visible == true), so ALL hidden
            // surfaces kept compositing through the WebView2 GPU process
            // from process start — measured ~14% CPU at idle with the shell
            // fully closed (rust side ~1%). show_window() and
            // set_picker_visible() resume the controller on demand; a
            // suspended page keeps loading and running JS (only rendering is
            // frozen), so hidden pages stay warm and opening feels identical.
            for label in [
                "taskbar",
                "launcher",
                "hushlight",
                "notification",
                "tables",
                "table-taskbar",
                "table-settings",
                "table-widgets",
                "table-desktop",
                "tutorial",
                "screensaver",
            ] {
                if let Some(wv) = app.get_webview_window(label) {
                    let _ = wv.as_ref().hide();
                    // Win11 paints a faint square border around every window
                    // RECT (independent of shadow:false). Transparent windows
                    // hosting rounded panels show it as a ghost frame — strip
                    // it from every window at boot (note windows get it at
                    // creation in open_note_window).
                    #[cfg(windows)]
                    win32::window::remove_dwm_border(&wv);
                }
            }

            // Load tables settings that live outside the Tauri state:
            // the hook's hold threshold and the saved window positions
            // for the movable tables.
            #[cfg(windows)]
            {
                let s = persist::load_settings();
                win32::hotkey::HOLD_MS
                    .store(s.tables_hold_ms.clamp(80, 1000), Ordering::SeqCst);
                restore_table_positions(&app.handle());
            }

            // Install the Win-key + Alt-key hooks. Two LL keyboard hooks
            // cooperate:
            //
            //   1. `prevent-alt-win-menu` (installed FIRST → called LAST in
            //      the LIFO chain) handles ONLY the Alt case: it suppresses
            //      the focused window's menu bar on a standalone Alt release.
            //      Its `on_released` callback returns `None` for Win — we
            //      handle Win in our own hook below.
            //
            //   2. `win32::hotkey::install` (installed LAST → called FIRST in
            //      the chain) SWALLOWS Win-down so the OS shell literally
            //      can't start its "Win chord" detection (which is what
            //      opens the Start menu). On a Win tap (Win down + Win up
            //      with no other key in between) it fires our toggle and
            //      swallows Win-up too. On a combo (Win+D, Win+E, …) it
            //      re-injects the Win-down so the shortcut still resolves
            //      natively.
            //
            // The previous integration relied on `prevent-alt-win-menu` for
            // BOTH Win and Alt. That crate's suppression strategy is to
            // inject a dummy key-up AFTER Win-up — which is racy under focus
            // changes (when the launcher webview has focus, the OS shell
            // processed Win-up before the synthetic dummy-up landed, so the
            // Start menu opened AND the close tap appeared to do nothing).
            // Swallowing Win-down is the only race-free fix.
            //
            // Replaces the old `flatwin.exe` AutoHotkey v2 helper + HTTP
            // :2290 /toggle bridge (both removed).
            #[cfg(windows)]
            {
                // (1) prevent-alt-win-menu is DISABLED.
                //
                // It was interfering with the Win-key close-tap: when the
                // launcher was open, tapping Win opened the Start menu
                // instead of closing the launcher. The crate installs its
                // own WH_KEYBOARD_LL hook on a separate thread, and the
                // two hooks' message pumps can interfere under focus
                // changes (when the launcher webview takes focus, the
                // crate's hook thread can stall the hook chain).
                //
                // Our own win32::hotkey hook (installed below) handles
                // BOTH Win (swallow + tap → toggle launcher) and Alt
                // (we add Alt menu suppression directly in the hook
                // callback — see hotkey.rs). No second hook needed.
                //
                // The crate is still in Cargo.toml for now (removing it
                // would require a Cargo.lock update); we just don't call
                // start().

                // (2) Install our own Win-key hook. It handles:
                //   - Win tap (press + release alone, quick) → toggle launcher
                //   - Win hold (tables_hold_ms) → pie table picker:
                //       around the cursor, or centered on screen for Ctrl+Win
                //   - Win release while picker is open → open the hovered
                //     table (or dismiss when nothing is hovered)
                //   - Win combo (Win+D, Win+E, ...) → dismiss picker, re-inject
                //     Win, let the combo resolve natively
                //   - Alt release alone → inject VK__none_ to suppress
                //     the focused window's menu bar (see hotkey.rs)
                let app_handle = app.handle().clone();
                win32::hotkey::install(win32::hotkey::HotkeyHandlers {
                    on_tap: {
                        let app = app_handle.clone();
                        Box::new(move || {
                            let app = app.clone();
                            // Run on a worker thread so the hook never blocks
                            // input processing — same pattern the old HTTP
                            // server used.
                            std::thread::spawn(move || {
                                log::info!("Win key tap (hotkey.rs) — toggling launcher");
                                toggle_launcher_impl(&app);
                            });
                        })
                    },
                    on_hold: {
                        let app = app_handle.clone();
                        Box::new(move |center| {
                            let app = app.clone();
                            std::thread::spawn(move || {
                                log::info!(
                                    "Win key hold (hotkey.rs) — showing table picker{}",
                                    if center { " (centered)" } else { "" }
                                );
                                show_tables_impl(&app, center);
                            });
                        })
                    },
                    on_tables_release: {
                        let app = app_handle.clone();
                        Box::new(move || {
                            let app = app.clone();
                            std::thread::spawn(move || {
                                let hovered = TABLES_HOVERED.swap(0, Ordering::SeqCst);
                                log::info!("Win released on table picker — hovered={hovered}");
                                if hovered >= 1 && hovered <= 5 {
                                    open_table_impl(&app, table_name_from_id(hovered));
                                } else {
                                    hide_tables_impl(&app);
                                }
                                // The picker closed (table opened or nothing
                                // hovered): clear any latched Win state in the
                                // OS with a synthetic Win-up. Our own hook
                                // ignores injected input, so this can't
                                // re-trigger the tap/hold/release logic.
                                win32::hotkey::inject_win_keyup();
                            });
                        })
                    },
                    on_tables_cancel: {
                        let app = app_handle.clone();
                        Box::new(move || {
                            let app = app.clone();
                            std::thread::spawn(move || {
                                log::info!("Win combo while table picker open — dismissing picker");
                                TABLES_HOVERED.store(0, Ordering::SeqCst);
                                hide_tables_impl(&app);
                            });
                        })
                    },
                });
            }

            // Start the Start menu killer monitor. This polls every 100ms
            // for StartMenuExperienceHost.exe (and SearchHost.exe) and kills
            // them on sight. This is the race-free way to prevent the Start
            // menu from appearing when the user taps Win — even if the
            // keyboard hook misses the Win-down event, the Start menu
            // process is terminated before it can render.
            #[cfg(windows)]
            {
                start_menu_killer::start();
            }

            // Initial icon scan
            #[cfg(windows)]
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    refresh_taskbar_apps(&handle);
                    refresh_desktop_items(&handle);
                });
            }

            // Desktop directory watcher (0.3.0) — files created/deleted/
            // renamed by ANY program now reach the desktop table
            // automatically. Debounced + emit-on-change; idle cost is zero
            // (the watcher thread parks inside ReadDirectoryChangesW).
            #[cfg(windows)]
            {
                desktop_watcher::start(app.handle().clone());
            }

            // ===== Periodic slow fallback refresh =====
            //
            // 0.3.0 CPU FIX: this loop used to call refresh_taskbar_apps
            // every 2.5 s — a full EnumWindows + process-path resolution +
            // icon pass, forever, even with nothing visible on screen. It
            // was one of the main contributors to the ~30% idle CPU burn.
            //
            // Refreshes are now EVENT-DRIVEN: the WinEvent foreground hook
            // (window switched) and the taskbar strip opening trigger
            // immediate refreshes. This loop is only a
            // slow SAFETY NET for events without a hook (window title
            // changes, apps that mutate windows subtly, missed events).
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(12));
                    #[cfg(windows)]
                    refresh_taskbar_apps(&handle);
                });
            }

            // Install WinEvent hook for instant foreground window change detection
            #[cfg(windows)]
            {
                let handle = app.handle().clone();
                let callback = Box::new(move || {
                    let h = handle.clone();
                    std::thread::spawn(move || {
                        // Debounce: a foreground switch often fires several
                        // events in a burst (alt-tab chains, focus dances).
                        // Each event used to spawn a full window scan — now
                        // only the first of a 300 ms burst does.
                        static LAST: parking_lot::Mutex<Option<std::time::Instant>> =
                            parking_lot::const_mutex(None);
                        let now = std::time::Instant::now();
                        {
                            let mut last = LAST.lock();
                            if let Some(t) = *last {
                                if now.duration_since(t) < std::time::Duration::from_millis(300) {
                                    *last = Some(now);
                                    return;
                                }
                            }
                            *last = Some(now);
                        }
                        // Small delay to let the new window settle
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        refresh_taskbar_apps(&h);
                    });
                });
                if let Err(e) = win32::winevent::install_foreground_hook(callback) {
                    log::error!("Failed to install foreground hook: {e}");
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            run_setup,
            get_taskbar_apps,
            get_desktop_items,
            activate_app,
            toggle_launcher,
            close_launcher,
            launcher_close_finished,
            launch_desktop_item,
            get_all_windows,
            activate_window,
            create_desktop_item,
            delete_desktop_item,
            rename_desktop_item,
            refresh_desktop,
            set_desktop_item_pinned,
            minimize_all_windows,
            execute_run,
            execute_run_admin,
            search_programs,
            end_task,
            save_clipboard,
            load_clipboard,
            notes_list,
            note_load,
            note_save,
            note_create,
            note_delete,
            open_note_window,
            note_window_ready,
            get_system_stats,
            get_battery_status,
            get_volume,
            set_volume,
            get_app_volumes,
            set_app_volume,
            save_widget_positions,
            load_widget_positions,
            save_widget_visibility,
            load_widget_visibility,
            save_icon_recolor,
            load_icon_recolor,
            save_settings,
            load_settings,
            clear_icon_cache,
            get_elevation_state,
            set_tables_hover,
            tables_rendered,
            tables_hidden,
            open_table,
            close_table,
            save_table_pos,
            get_language,
            save_clipboard_image,
            set_clipboard_image,
            get_clipboard_text,
            set_clipboard_text,
            reboot_system,
            shutdown_system,
            add_to_startup,
            remove_from_startup,
            notify,
            notification_close_finished,
            show_screensaver,
            hide_screensaver,
            exit_hush,
            reset_config,
            finish_tutorial,
            get_active_theme,
            get_all_themes,
            set_active_theme,
            set_dimmer_level,
        ])
        .on_window_event(|window, event| {
            match event {
                // Movable tables: persist their position while being dragged
                // (throttled — Moved fires for every px of the drag). Only
                // visible windows save, so the startup clamp/restore passes
                // don't overwrite a good position with a stale one.
                WindowEvent::Moved(pos) => {
                    let label = window.label();
                    if matches!(label, "table-settings" | "table-widgets" | "table-desktop")
                        && window.is_visible().unwrap_or(false)
                    {
                        let key = match label {
                            "table-settings" => "settings",
                            "table-widgets" => "widgets",
                            _ => "desktop",
                        };
                        moved_save::schedule(key.to_string(), pos.x, pos.y);
                    }
                }
                _ => {}
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Hush_UI");
}

// ===== Setup command (no-op — HideTaskbar runs in-process now) =====
#[tauri::command]
fn run_setup() -> Vec<(String, bool)> {
    Vec::new()
}

// ===== Command handlers =====

#[tauri::command]
fn get_taskbar_apps(state: tauri::State<'_, Arc<Mutex<AppState>>>) -> Vec<app_state::TaskbarApp> {
    state.lock().taskbar_apps.clone()
}

#[tauri::command]
fn get_desktop_items(state: tauri::State<'_, Arc<Mutex<AppState>>>) -> Vec<app_state::DesktopItem> {
    state.lock().desktop_items.clone()
}

#[tauri::command]
fn activate_app(app_id: String, app: tauri::AppHandle) {
    log::info!("activate_app: {app_id}");
    #[cfg(windows)]
    {
        if let Err(e) = win32::apps::activate_or_launch(&app_id) {
            log::error!("activate_app failed: {e}");
        }
    }
    let _ = app.emit("taskbar://icon-activated", app_id);
}

#[tauri::command]
fn toggle_launcher(app: tauri::AppHandle) {
    log::info!("toggle_launcher called (command)");
    toggle_launcher_impl(&app);
}

pub(crate) fn toggle_launcher_impl(app: &tauri::AppHandle) {
    // Logical state, not window visibility: during the close animation the
    // window is still visible, so is_visible() would mis-route a toggle.
    if LAUNCHER_OPEN.load(Ordering::SeqCst) {
        hide_launcher_animated(app);
    } else {
        show_launcher(app);
    }
}

/// Show the hushlight: window FIRST, event AFTER.
///
/// 0.2: hushlight is no longer a fullscreen overlay — it is a medium
/// centered window with just the search bar (desktop icons moved to the
/// desktop table, widgets to the widgets table). The fullscreen "launcher"
/// window stays configured as the generic fullscreen surface.
fn show_launcher(app: &tauri::AppHandle) {
    let Some(hushlight) = app.get_webview_window("hushlight") else {
        log::error!("show_launcher: hushlight window not found");
        return;
    };

    // Cancel any pending delayed hide from an interrupted close.
    CLOSE_SEQ.fetch_add(1, Ordering::SeqCst);
    LAUNCHER_OPEN.store(true, Ordering::SeqCst);

    // Center on the primary monitor (physical px).
    if let Ok(Some(monitor)) = hushlight.primary_monitor() {
        let pos = monitor.position();
        let size = monitor.size();
        let win = hushlight.outer_size().unwrap_or_default();
        let x = pos.x + (size.width as i32 - win.width as i32) / 2;
        let y = pos.y + (size.height as i32 - win.height as i32) / 3;
        let _ = hushlight.set_position(tauri::PhysicalPosition::new(x, y));
    }

    // Show-desktop effect hardcoded OFF.

    let _ = hushlight.set_always_on_top(true);
    win32::window::show_window(&hushlight);
    let _ = hushlight.set_focus();
    // Emit AFTER the window is on screen — the frontend's pop-in then starts
    // from a presented frame.
    let _ = app.emit("hushlight://shown", ());
}

/// Hide hushlight with its pop-out animation.
///
/// Emits `hushlight://hidden` (the frontend plays its pop-out, THEN reports
/// back via `launcher_close_finished`, at which point we hide the window).
/// A fallback thread hides the window after 1.2 s in case that report is
/// ever lost — superseded by any new show via CLOSE_SEQ.
fn hide_launcher_animated(app: &tauri::AppHandle) {
    if !LAUNCHER_OPEN.swap(false, Ordering::SeqCst) {
        return; // already closing or closed — nothing to animate
    }
    let seq = CLOSE_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    let _ = app.emit("hushlight://hidden", ());

    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1200));
        if CLOSE_SEQ.load(Ordering::SeqCst) == seq {
            if let Some(hushlight) = handle.get_webview_window("hushlight") {
                win32::window::hide_window(&hushlight);
            }
            log::info!("hushlight hidden via fallback timer");
        }
    });
}

/// Called by the hushlight frontend the moment its pop-out animation
/// finished — hide the window at exactly the right time instead of a
/// wall-clock guess. Guarded so a late report can never hide a freshly
/// re-opened hushlight.
#[tauri::command]
fn launcher_close_finished(app: tauri::AppHandle) {
    if LAUNCHER_OPEN.load(Ordering::SeqCst) {
        return; // a new show superseded the close while the report was in flight
    }
    CLOSE_SEQ.fetch_add(1, Ordering::SeqCst);
    if let Some(hushlight) = app.get_webview_window("hushlight") {
        win32::window::hide_window(&hushlight);
    }
}

#[tauri::command]
fn close_launcher(app: tauri::AppHandle) {
    // Two close paths share this command: the hushlight state machine and
    // the fullscreen "launcher" window used as a plain window surface. When
    // that window is the one visible, close it directly without touching
    // the hushlight state.
    if let Some(launcher) = app.get_webview_window("launcher") {
        if launcher.is_visible().unwrap_or(false) && !LAUNCHER_OPEN.load(Ordering::SeqCst) {
            CLOSE_SEQ.fetch_add(1, Ordering::SeqCst);
            win32::window::hide_window(&launcher);
            let _ = app.emit("launcher://force-hidden", ());
            return;
        }
    }
    hide_launcher_animated(&app);
}

// ===== Tables (pie Win-key picker + its four tables) =====
//
// Holding Win (tables_hold_ms) opens the "tables" overlay: a pie menu of
// five wedges radiating from the mouse cursor (Ctrl+Win → screen center).
// Hovering a slice and releasing Win opens that table:
//
//   1 taskbar   — the taskbar icons on a small vertical line strip
//                 (max 5 visible, wheel-scrollable, macOS-style magnify)
//   2 settings  — the settings panel as a movable window (pos → tables.json)
//   3 widgets   — the launcher widgets in a small movable window
//                 (pos → tables.json)
//   4 hushlight — the hushlight launcher itself (searchbar-only overlay)
//   5 desktop   — the desktop icons in a medium movable window
//                 (pos → tables.json)
//
// The picker overlay is transient: it exists only while Win is held.

/// Map the picker's table id (set by `set_tables_hover`) to its window name.
fn table_name_from_id(id: i32) -> &'static str {
    match id {
        1 => "taskbar",
        2 => "settings",
        3 => "widgets",
        4 => "hushlight",
        5 => "desktop",
        _ => "",
    }
}

/// Show the pie picker. `center` = Ctrl+Win → center of the primary
/// monitor instead of around the cursor.
#[cfg(windows)]
fn show_tables_impl(app: &tauri::AppHandle, center: bool) {
    let Some(tables) = app.get_webview_window("tables") else {
        log::error!("show_tables: tables window not found");
        return;
    };

    // Fit to the primary monitor and compute the anchor point in logical
    // (CSS) px relative to the window client area.
    let Some(monitor) = tables.primary_monitor().ok().flatten() else {
        return;
    };
    let mon_pos = monitor.position();
    let mon_size = monitor.size();
    let scale = monitor.scale_factor();
    // Only move/resize when it actually drifted (monitor change, DPI
    // switch). A redundant resize of a transparent WebView2 right before
    // show() is the source of the one-frame light-blue flicker — skip it.
    prefit_tables_window(&tables);

    let (ax, ay) = if center {
        (
            mon_size.width as f64 / 2.0,
            mon_size.height as f64 / 2.0,
        )
    } else {
        // Physical cursor pos → monitor-relative logical px.
        let mut pt = windows::Win32::Foundation::POINT::default();
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
        }
        let phys_x = pt.x as f64 - mon_pos.x as f64;
        let phys_y = pt.y as f64 - mon_pos.y as f64;
        (phys_x / scale, phys_y / scale)
    };

    *TABLES_CURSOR.lock() = (ax, ay);
    TABLES_HOVERED.store(0, Ordering::SeqCst);
    // Bump the picker generation — cancels any in-flight animated hide so a
    // rapid hold → release → hold never races the dismiss delay.
    TABLES_SEQ.fetch_add(1, Ordering::SeqCst);
    let seq = TABLES_SEQ.load(Ordering::SeqCst);
    TABLES_RENDERED.store(false, Ordering::SeqCst);

    // 0.3.3 reveal handshake — kills the "pie spawns at its old dismissal
    // spot and teleports to the cursor" microframe glitch:
    //
    // 0.3.2 suspends the picker's WebView2 controller while hidden, and
    // resuming the controller re-presents the LAST rendered frame — the pie
    // still at the anchor of the previous open. The old order (reveal the
    // window, THEN emit tables://show) uncloaked that stale frame and only
    // let the frontend move the pie a few frames later: a visible one-shot
    // teleport on every open after a table was chosen (the instant-dismiss
    // path suspends before the pop-out even starts, so the stale frame was
    // always the full pie at the old anchor).
    //
    // New order: resume the controller while the window is still cloaked →
    // emit the new anchor → wait (bounded) until the webview confirms via
    // `tables_rendered` that the pie is laid out at the NEW anchor and a
    // frame is committed → uncloak. The first visible frame is always the
    // correct one. DWM-cloak reveal (never hidden/shown — no WebView2
    // surface re-attach, no light-blue flash) is unchanged.
    let _ = win32::window::resume_picker_webview(&tables);
    let _ = app.emit(
        "tables://show",
        serde_json::json!({ "x": ax, "y": ay, "center": center, "seq": seq }),
    );
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_millis(TABLES_RENDER_WAIT_MS);
    while !TABLES_RENDERED.load(Ordering::SeqCst)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    // Reveal only if the picker is still meant to be open — a Win combo or
    // a release-dismiss may have landed while we waited (TABLES_SEQ bumped);
    // in that case the dismiss path owns the window state.
    if TABLES_SEQ.load(Ordering::SeqCst) == seq {
        let _ = win32::window::set_picker_visible(&tables, true);
    }
}

#[cfg(not(windows))]
fn show_tables_impl(_app: &tauri::AppHandle, _center: bool) {}

/// Size the tables window to the primary monitor, but ONLY when it has
/// actually drifted from that geometry. Skipping the no-op resize keeps
/// WebView2's transparent surface untouched — resizing/re-showing it is
/// what produces the one-frame light-blue flash when the pie opens.
#[cfg(windows)]
fn prefit_tables_window(tables: &tauri::WebviewWindow) {
    let Some(monitor) = tables.primary_monitor().ok().flatten() else {
        return;
    };
    let mon_pos = monitor.position();
    let mon_size = monitor.size();
    let cur_pos = tables.outer_position().unwrap_or_default();
    let cur_size = tables.outer_size().unwrap_or_default();
    if cur_pos.x != mon_pos.x || cur_pos.y != mon_pos.y {
        let _ = tables.set_position(tauri::PhysicalPosition::new(mon_pos.x, mon_pos.y));
    }
    if cur_size.width != mon_size.width || cur_size.height != mon_size.height {
        let _ = tables.set_size(tauri::PhysicalSize::new(mon_size.width, mon_size.height));
    }
}

/// Dismiss the picker with its pop-out animation: emit `tables://hide` (the
/// frontend scales the buttons back down, blanks the root once it ends and
/// confirms via `tables_hidden`), then hide the window once the pop-out had
/// time to play AND the blank frame is committed. Generation-guarded against
/// a re-open landing inside the delay.
#[cfg(windows)]
fn hide_tables_impl(app: &tauri::AppHandle) {
    if let Some(_tables) = app.get_webview_window("tables") {
        let seq = TABLES_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
        TABLES_HIDDEN.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "tables://hide",
            serde_json::json!({ "seq": seq, "instant": false }),
        );
        let handle = app.clone();
        std::thread::spawn(move || {
            // Hide handshake (animated): wait at least the pop-out duration
            // (280ms floor — 140ms transitions + 80ms stagger + margin), and
            // normally until the page confirms the blank frame; the 340ms
            // cap only matters if the webview is wedged (a transparent
            // overlay with blank content is invisible either way, so the
            // worst case costs nothing visible).
            let start = std::time::Instant::now();
            let floor = std::time::Duration::from_millis(TABLES_HIDE_ANIMATED_FLOOR_MS);
            let cap = std::time::Duration::from_millis(TABLES_HIDE_ANIMATED_CAP_MS);
            while start.elapsed() < cap {
                if start.elapsed() >= floor && TABLES_HIDDEN.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            if TABLES_SEQ.load(Ordering::SeqCst) == seq {
                let _ = handle
                    .get_webview_window("tables")
                    .map(|w| win32::window::set_picker_visible(&w, false));
            }
        });
    }
}

#[cfg(not(windows))]
fn hide_tables_impl(_app: &tauri::AppHandle) {}

/// Instantly dismiss the picker (used when a table is opening right away —
/// the new table provides the visual transition).
///
/// 0.4.0: the old behavior cloaked + suspended the controller RIGHT HERE —
/// before the page could render anything — so the surface's last presented
/// frame stayed the full pie at the current anchor. Resuming on the next
/// open re-presented exactly that stale frame, and the show handshake's
/// double-rAF confirm can win its race BEFORE the compositor presents the
/// re-laid-out frame: a one-frame "pie at the old position" flash survived
/// 0.3.3. Now: emit the instant hide (the page blanks its root immediately)
/// and wait (bounded) for the `tables_hidden` confirm before cloak+suspend —
/// the surface freezes on a blank frame and no stale pie can ever be
/// re-presented. Visible cost: the pie vanishes ~2 frames later than before
/// (the opened table provides the transition anyway).
#[cfg(windows)]
fn hide_tables_now(app: &tauri::AppHandle) {
    let seq = TABLES_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    if let Some(_tables) = app.get_webview_window("tables") {
        TABLES_HIDDEN.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "tables://hide",
            serde_json::json!({ "seq": seq, "instant": true }),
        );
        let handle = app.clone();
        std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let cap = std::time::Duration::from_millis(TABLES_HIDE_INSTANT_CAP_MS);
            while start.elapsed() < cap && !TABLES_HIDDEN.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            if TABLES_SEQ.load(Ordering::SeqCst) == seq {
                let _ = handle
                    .get_webview_window("tables")
                    .map(|w| win32::window::set_picker_visible(&w, false));
            }
        });
    }
}

/// Open one of the four tables and dismiss the picker.
#[cfg(windows)]
fn open_table_impl(app: &tauri::AppHandle, name: &str) {
    hide_tables_now(app);
    log::info!("open_table: {name}");
    match name {
        "taskbar" => open_taskbar_table(app),
        "settings" => show_movable_table(app, "table-settings", "settings", None),
        "widgets" => show_movable_table(app, "table-widgets", "widgets", None),
        "desktop" => show_movable_table(app, "table-desktop", "desktop", None),
        "hushlight" => {
            // Table 4 IS the hushlight — show the search window itself. If
            // it's already open, leave it alone.
            if !LAUNCHER_OPEN.load(Ordering::SeqCst) {
                show_launcher(app);
            }
        }
        _ => {}
    }
}

#[cfg(not(windows))]
fn open_table_impl(_app: &tauri::AppHandle, _name: &str) {}

/// Spawn the vertical taskbar strip next to the cursor position captured
/// when the picker opened. Always cursor-anchored (this table is transient —
/// its position is intentionally NOT persisted).
#[cfg(windows)]
fn open_taskbar_table(app: &tauri::AppHandle) {
    // The window is 260px wide but only the 62px rail is visible; clamp
    // against the VISIBLE rail so it can hug the screen edge (the invisible
    // right-hand zone may hang off-screen — it's fully transparent).
    const RAIL_W: f64 = 66.0;
    const STRIP_H: f64 = 5.0 * 62.0 + 14.0; // 5 icon slots + rail padding

    // 0.3.0: refreshes are event-driven (no more 2.5s background scan) —
    // opening the strip IS the event. Kick a fresh scan so the strip opens
    // with current data; the result lands via taskbar://apps-updated and
    // the strip reconciles incrementally.
    {
        let app2 = app.clone();
        std::thread::spawn(move || refresh_taskbar_apps(&app2));
    }

    let Some(strip) = app.get_webview_window("table-taskbar") else {
        log::error!("open_taskbar_table: table-taskbar window not found");
        return;
    };
    let Some(monitor) = strip.primary_monitor().ok().flatten() else {
        return;
    };
    let mon_pos = monitor.position();
    let mon_size = monitor.size();
    let scale = monitor.scale_factor();

    // Anchor: picker cursor position (logical) → physical px, then offset so
    // the strip's vertical center sits next to the cursor, clamped on-screen.
    let (cx, cy) = *TABLES_CURSOR.lock();
    let mut x = mon_pos.x as f64 + (cx + 26.0) * scale;
    let mut y = mon_pos.y as f64 + (cy - STRIP_H / 2.0) * scale;
    x = x.clamp(
        mon_pos.x as f64 + 8.0,
        mon_pos.x as f64 + mon_size.width as f64 - (RAIL_W + 8.0) * scale,
    );
    y = y.clamp(
        mon_pos.y as f64 + 8.0,
        mon_pos.y as f64 + mon_size.height as f64 - (STRIP_H + 8.0) * scale,
    );

    let _ = strip.set_size(tauri::LogicalSize::new(260.0, STRIP_H));
    let _ = strip.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
    let _ = strip.set_always_on_top(true);
    win32::window::show_window(&strip);
    let _ = app.emit("table://taskbar-shown", ());

    // While the strip is open, any click outside its rect closes it with
    // the pop-out animation (the click itself passes through to whatever is
    // under the cursor). The frontend plays the animation and calls
    // close_table, which stops this watcher.
    if let Ok(hwnd) = strip.hwnd() {
        let app2 = app.clone();
        win32::table_mouse::start(
            hwnd.0 as isize,
            Arc::new(move || {
                let app3 = app2.clone();
                std::thread::spawn(move || {
                    let _ = app3.emit("table://taskbar-outside", ());
                });
            }),
        );
    }
}

/// Show a movable table (settings / widgets) at its persisted position,
/// defaulting to a gentle cascade from the picker anchor when it has never
/// been placed. Physical coords round-trip through tables.json.
#[cfg(windows)]
fn show_movable_table(
    app: &tauri::AppHandle,
    label: &str,
    key: &str,
    fallback: Option<(f64, f64)>,
) {
    let Some(win) = app.get_webview_window(label) else {
        log::error!("show_movable_table: {label} window not found");
        return;
    };
    let Some(monitor) = win.primary_monitor().ok().flatten() else {
        return;
    };
    let mon_pos = monitor.position();
    let mon_size = monitor.size();

    let saved = persist::load_table_positions().remove(key);
    let (x, y) = match saved {
        Some((sx, sy)) => (sx as f64, sy as f64),
        None => {
            let (cx, cy) = *TABLES_CURSOR.lock();
            let (fx, fy) = fallback.unwrap_or((0.0, 0.0));
            (
                mon_pos.x as f64 + (cx - 120.0) + fx,
                mon_pos.y as f64 + (cy - 80.0) + fy,
            )
        }
    };
    // Clamp fully on-screen (10px margin).
    let size = win.outer_size().unwrap_or_default();
    let x = x.clamp(
        mon_pos.x as f64 + 10.0,
        (mon_pos.x + mon_size.width as i32) as f64 - size.width as f64 - 10.0,
    );
    let y = y.clamp(
        mon_pos.y as f64 + 10.0,
        (mon_pos.y + mon_size.height as i32) as f64 - size.height as f64 - 10.0,
    );

    let _ = win.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
    let _ = win.set_always_on_top(true);
    win32::window::show_window(&win);
    let _ = win.set_focus();
    let _ = app.emit(&format!("table://{key}-shown"), ());
}

/// Restore persisted positions for the movable tables at startup (pure
/// sizing pass — windows stay hidden until opened).
#[cfg(windows)]
fn restore_table_positions(app: &tauri::AppHandle) {
    let positions = persist::load_table_positions();
    for (key, label) in [
        ("settings", "table-settings"),
        ("widgets", "table-widgets"),
        ("desktop", "table-desktop"),
    ] {
        if let (Some(win), Some(&(x, y))) =
            (app.get_webview_window(label), positions.get(key))
        {
            let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
        }
    }
}

// ===== Table commands (called by the table webviews) =====

/// The picker frontend reports hover changes; the keyboard hook reads this
/// on Win-up to decide which table to open. id: 0=none 1=taskbar 2=settings
/// 3=widgets 4=hushlight.
#[tauri::command]
fn set_tables_hover(id: i32) {
    TABLES_HOVERED.store(id.clamp(0, 5), Ordering::SeqCst);
}

/// The picker frontend confirms it laid the pie out at the anchor of the
/// `tables://show` with generation `seq` and committed a frame (double-rAF
/// after the DOM write). Only a confirmation matching the CURRENT show
/// generation is accepted — a stale one from an earlier open is ignored.
#[tauri::command]
fn tables_rendered(seq: u64) {
    if seq == TABLES_SEQ.load(Ordering::SeqCst) {
        TABLES_RENDERED.store(true, Ordering::SeqCst);
    }
}

/// The picker frontend confirms the pie content is BLANKED (root
/// visibility:hidden) and a frame with that blank state has been committed
/// (double-rAF after the visibility write). Only a confirmation matching the
/// CURRENT generation is accepted — a stale one from an earlier hide is
/// ignored, and a re-open bumps the generation so the show path owns the
/// window state again (see the hide handshake comment on TABLES_HIDDEN).
#[tauri::command]
fn tables_hidden(seq: u64) {
    if seq == TABLES_SEQ.load(Ordering::SeqCst) {
        TABLES_HIDDEN.store(true, Ordering::SeqCst);
    }
}

/// Open a table by name from the picker (click fallback — the primary
/// gesture is hover + release Win).
#[tauri::command]
fn open_table(app: tauri::AppHandle, name: String) {
    #[cfg(windows)]
    open_table_impl(&app, &name);
    #[cfg(not(windows))]
    let _ = (&app, &name);
}

/// Hide one table window (close buttons / Esc inside the tables). The
/// taskbar strip also tears down its outside-click watcher here — this is
/// the single funnel every strip-close path goes through (frontend Esc,
/// icon click, outside click, End task).
#[tauri::command]
fn close_table(app: tauri::AppHandle, name: String) {
    let label = match name.as_str() {
        "taskbar" => "table-taskbar",
        "settings" => "table-settings",
        "widgets" => "table-widgets",
        "desktop" => "table-desktop",
        "tables" => "tables",
        _ => return,
    };
    if name == "taskbar" {
        win32::table_mouse::stop();
    }
    if let Some(win) = app.get_webview_window(label) {
        win32::window::hide_window(&win);
    }
    // The pie picker itself was closed from the frontend (click dismiss /
    // Esc) rather than by a Win release — recover the Win-key latch the same
    // way the release path does. NO-OP for the other table windows.
    #[cfg(windows)]
    if name == "tables" {
        win32::hotkey::tables_closed_recover();
    }
}

/// Persist a movable table's position (physical px). Also called by the
/// WindowEvent::Moved throttle below — this command exists so a drag that
/// ends without a final Moved event still saves.
#[tauri::command]
fn save_table_pos(name: String, x: i32, y: i32) {
    #[cfg(windows)]
    persist_table_position(&name, x, y);
    #[cfg(not(windows))]
    let _ = (name, x, y);
}

#[cfg(windows)]
fn persist_table_position(name: &str, x: i32, y: i32) {
    let mut positions = persist::load_table_positions();
    positions.insert(name.to_string(), (x, y));
    persist::save_table_positions(&positions);
}

#[tauri::command]
fn launch_desktop_item(item_id: String, state: tauri::State<'_, Arc<Mutex<AppState>>>) {
    let path = {
        let s = state.lock();
        s.desktop_items.iter().find(|i| i.id == item_id).map(|i| i.path.clone())
    };
    // The desktop scan guarantees id == absolute path, so when the cached
    // state is stale (item created/renamed/deleted between scans) fall back
    // to the id itself. This keeps the click targeting the EXACT item that
    // was rendered — it must never degrade into a name-based lookup, which
    // could launch a different item that merely shares the display name.
    let target = match path {
        Some(p) => Some(p),
        None => {
            if std::path::Path::new(&item_id).exists() {
                log::warn!("launch_desktop_item: id not in cached state, using id as path: {item_id}");
                Some(item_id)
            } else {
                log::error!("launch_desktop_item: item not found: {item_id}");
                None
            }
        }
    };
    if let Some(p) = target {
        let kind = if std::path::Path::new(&p).is_dir() { "dir" } else { "file" };
        log::info!("launch_desktop_item: {p} (kind={kind})");
        #[cfg(windows)]
        {
            if let Err(e) = win32::shell::shell_execute(&p) {
                log::error!("launch_desktop_item failed: {e}");
            }
        }
        #[cfg(not(windows))]
        let _ = &p;
    }
}

#[tauri::command]
fn activate_window(hwnd: usize) {
    log::info!("activate_window: hwnd={}", hwnd);
    #[cfg(windows)]
    {
        if let Err(e) = win32::peek::activate_window_by_hwnd(hwnd as isize) {
            log::error!("activate_window failed: {e}");
        }
    }
}

#[tauri::command]
fn get_all_windows() -> Vec<win32::peek::WindowPreview> {
    #[cfg(windows)]
    {
        let windows_with_exe = win32::peek::get_all_windows_with_exe();
        return windows_with_exe
            .into_iter()
            .map(|w| win32::peek::WindowPreview {
                hwnd: w.hwnd,
                title: w.title,
                icon_data_url: w.icon_data_url,
            })
            .collect();
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

#[tauri::command]
fn create_desktop_item(name: String, is_folder: bool, app: tauri::AppHandle) {
    log::info!("create_desktop_item: name={} folder={}", name, is_folder);
    #[cfg(windows)]
    {
        match win32::shell::create_item(&name, is_folder) {
            Ok(_) => refresh_desktop_items(&app),
            Err(e) => log::error!("create_desktop_item failed: {e}"),
        }
    }
}

#[tauri::command]
fn delete_desktop_item(item_id: String, app: tauri::AppHandle) {
    log::info!("delete_desktop_item: {}", item_id);
    #[cfg(windows)]
    {
        match win32::shell::delete_item(&item_id) {
            Ok(_) => refresh_desktop_items(&app),
            Err(e) => log::error!("delete_desktop_item failed: {e}"),
        }
    }
}

#[tauri::command]
fn rename_desktop_item(item_id: String, new_name: String, app: tauri::AppHandle) {
    log::info!("rename_desktop_item: {} -> {}", item_id, new_name);
    #[cfg(windows)]
    {
        match win32::shell::rename_item(&item_id, &new_name) {
            Ok(_) => refresh_desktop_items(&app),
            Err(e) => log::error!("rename_desktop_item failed: {e}"),
        }
    }
}

#[tauri::command]
fn refresh_desktop(app: tauri::AppHandle) {
    refresh_desktop_items(&app);
}

// ===== Desktop pin/unpin ("Pin to top" in the desktop-table menu) =====
#[tauri::command]
fn set_desktop_item_pinned(item_id: String, pinned: bool, app: tauri::AppHandle) {
    log::info!("set_desktop_item_pinned: {item_id} pinned={pinned}");
    let mut pins = persist::load_desktop_pins();
    if pinned {
        if !pins.contains(&item_id) {
            pins.push(item_id);
        }
    } else {
        pins.retain(|p| p != &item_id);
    }
    persist::save_desktop_pins(&pins);
    // Rescan reapplies the pin flags + ordering and emits
    // launcher://items-updated only when the snapshot actually changed.
    refresh_desktop_items(&app);
}

// ===== Minimize all windows =====
#[tauri::command]
fn minimize_all_windows(app: tauri::AppHandle) {
    log::info!("minimize_all_windows");
    #[cfg(windows)]
    {
        // Close hushlight first so it doesn't get minimized or block. This
        // is an INSTANT hide (everything minimizes right now, so there is
        // nothing pretty to animate over) — fix the state machine
        // accordingly: cancel any pending animated close.
        if LAUNCHER_OPEN.swap(false, Ordering::SeqCst) {
            CLOSE_SEQ.fetch_add(1, Ordering::SeqCst);
            if let Some(hushlight) = app.get_webview_window("hushlight") {
                win32::window::hide_window(&hushlight);
            }
            let _ = app.emit("hushlight://hidden", ());
        }

        // Approach: enumerate all top-level windows and call ShowWindow(SW_MINIMIZE)
        // on each that's visible + not our own + not the desktop/explorer.
        use windows::Win32::Foundation::{HWND, LPARAM};
        use windows::core::BOOL;
        use windows::Win32::UI::WindowsAndMessaging::{
            EnumWindows, IsWindowVisible, GetWindowTextW, GetWindowThreadProcessId,
            GetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TOOLWINDOW, ShowWindowAsync, SW_MINIMIZE,
            IsIconic,
        };

        struct State { skip_pid: u32 }
        let mut state = State { skip_pid: 0 };
        // Find our own PID (hush_ui) so we skip our windows.
        if let Some(hushlight) = app.get_webview_window("hushlight") {
            if let Ok(hwnd) = hushlight.hwnd() {
                let mut pid: u32 = 0;
                unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)); }
                state.skip_pid = pid;
            }
        }
        let skip_pid = state.skip_pid;

        unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let skip_pid = *(lparam.0 as *const u32);
            if !IsWindowVisible(hwnd).as_bool() {
                return BOOL(1);
            }
            // Skip tool windows (like our own taskbar)
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            if ex & (WS_EX_TOOLWINDOW.0 as isize) != 0 {
                return BOOL(1);
            }
            // Skip already minimized windows
            if IsIconic(hwnd).as_bool() {
                return BOOL(1);
            }
            // Skip our own process
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == skip_pid {
                return BOOL(1);
            }
            // Skip explorer.exe (desktop) — best effort via window title
            let mut title = [0u16; 64];
            let len = GetWindowTextW(hwnd, &mut title);
            if len > 0 {
                let title_str = String::from_utf16_lossy(&title[..len as usize]);
                let lower = title_str.to_lowercase();
                if lower.contains("program manager") || lower.is_empty() {
                    return BOOL(1);
                }
            }
            let _ = ShowWindowAsync(hwnd, SW_MINIMIZE);
            BOOL(1)
        }

        let skip_pid_ptr: *const u32 = &skip_pid;
        let lparam = LPARAM(skip_pid_ptr as isize);
        unsafe {
            let _ = EnumWindows(Some(enum_proc), lparam);
        }
    }
}

// ===== Clipboard history =====
#[tauri::command]
fn save_clipboard(items: Vec<String>) {
    persist::save_clipboard(&items);
}

#[tauri::command]
fn load_clipboard() -> Vec<String> {
    persist::load_clipboard()
}

// ===== Notes (remade — numbered note buttons + independent note windows) =====
// The widget shows one button per note, labeled with the note's stable
// number. Clicking a button opens an independent always-on-top editor
// window (label `note-<num>`, URL note/index.html). Numbers never
// re-flow: with notes 1 2 3, deleting 2 leaves 1 and 3; the next + takes 4.

#[tauri::command]
fn notes_list() -> Vec<persist::NoteEntry> {
    persist::load_note_list()
}

#[tauri::command]
fn note_load(num: u32) -> String {
    persist::load_note_list()
        .into_iter()
        .find(|n| n.num == num)
        .map(|n| n.text)
        .unwrap_or_default()
}

#[tauri::command]
fn note_save(num: u32, text: String) {
    let mut notes = persist::load_note_list();
    // Update-only: a stale editor saving after its note was deleted must
    // not resurrect the entry (its window gets closed by note_delete).
    if let Some(n) = notes.iter_mut().find(|n| n.num == num) {
        n.text = text;
        persist::save_note_list(&notes);
    }
}

#[tauri::command]
fn note_create(app: tauri::AppHandle) -> u32 {
    let mut notes = persist::load_note_list();
    let next = notes.iter().map(|n| n.num).max().unwrap_or(0) + 1;
    notes.push(persist::NoteEntry {
        num: next,
        text: String::new(),
    });
    persist::save_note_list(&notes);
    let _ = app.emit("notes://changed", &notes);
    next
}

#[tauri::command]
fn note_delete(app: tauri::AppHandle, num: u32) {
    let mut notes = persist::load_note_list();
    let before = notes.len();
    notes.retain(|n| n.num != num);
    if notes.len() == before {
        return; // already gone — don't broadcast a no-op change
    }
    persist::save_note_list(&notes);
    // Tear down the note's editor window if it is open.
    if let Some(win) = app.get_webview_window(&format!("note-{num}")) {
        let _ = win.close();
    }
    let _ = app.emit("notes://changed", &notes);
}

/// Open (or focus) the independent editor window for note `num`.
/// Windows cascade from the top-left of the primary monitor so several
/// open notes don't stack exactly on top of each other.
///
/// Show lifecycle (0.4.2 fix for the 0.4.1 "ghost window" bug): the
/// window is built HIDDEN and is revealed only by `note_window_ready`,
/// which the page invokes once its event listeners are wired. 0.4.1
/// showed the window and fired `note://shown` in the same tick as the
/// build — long before the freshly booted WebView2 page could listen —
/// so the pop-in never ran and the window stayed at its opacity:0
/// pre-animation state forever: invisible, always-on-top, swallowing
/// every click near it.
///
/// MUST stay an ASYNC command (0.4.3): this is the only runtime window
/// creation in the app, and a sync command executes ON the main thread
/// — inside the WebView2 IPC dispatch of the calling webview. Building
/// a new WebView2 inline from there has to pump a nested message loop,
/// which re-enters the IPC path with the widget table's own polling
/// invokes still in flight and wedges the main thread for good. Every
/// backend operation dies from that point: + / − hang, closing a table
/// plays its pop-out but the hide never runs (the "ghost" window), the
/// picker can't open any table. An async command runs on the async
/// runtime instead — build() then dispatches the actual creation to an
/// IDLE main loop, which is the safe, canonical path.
#[tauri::command(async)]
fn open_note_window(app: tauri::AppHandle, num: u32) {
    let label = format!("note-{num}");
    if let Some(existing) = app.get_webview_window(&label) {
        // The page is long-loaded here — resume the webview, reveal the
        // window and replay the pop-in (the page is subscribed, so this
        // event cannot be missed). Also self-heals a 0.4.1-era ghost.
        #[cfg(windows)]
        win32::window::show_window(&existing);
        #[cfg(not(windows))]
        let _ = existing.show();
        let _ = existing.set_focus();
        let _ = app.emit("note://shown", num);
        return;
    }
    let url = tauri::WebviewUrl::App("note/index.html".into());
    let built = tauri::webview::WebviewWindowBuilder::new(&app, &label, url)
        .title(format!("Hush_UI — Note {num}"))
        .inner_size(320.0, 400.0)
        .min_inner_size(240.0, 260.0)
        .decorations(false)
        .transparent(true)
        .resizable(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .shadow(false)
        .focused(false)
        .visible(false) // positioned + shown by us, like every table
        .build();
    let win = match built {
        Ok(w) => w,
        Err(e) => {
            log::error!("open_note_window: failed to build {label}: {e}");
            return;
        }
    };

    // Strip the Win11 square window-rect border (the ghost frame) from the
    // freshly created note window too — config windows get it in setup.
    #[cfg(windows)]
    win32::window::remove_dwm_border(&win);

    // Cascade position in PHYSICAL pixels (monitor coords are physical).
    if let Some(monitor) = win.primary_monitor().ok().flatten() {
        let mp = monitor.position();
        let ms = monitor.size();
        let size = win.outer_size().unwrap_or_default();
        let (w, h) = (size.width as i32, size.height as i32);
        let step = (num % 8) as i32;
        let x = (mp.x + 160 + step * 26).clamp(mp.x + 10, mp.x + ms.width as i32 - w - 10);
        let y = (mp.y + 120 + step * 26).clamp(mp.y + 10, mp.y + ms.height as i32 - h - 10);
        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    }

    // NOT shown here — the page calls note_window_ready when its listeners
    // are wired (see the doc comment above). Backstop: if the page never
    // reports (broken load, frozen renderer), reveal it after 4s anyway.
    // A visible window with a broken page beats an invisible click-eater.
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(4));
        if let Some(win) = app.get_webview_window(&format!("note-{num}")) {
            if !win.is_visible().unwrap_or(true) {
                log::warn!("open_note_window: note_window_ready never arrived for note-{num} — fallback show");
                #[cfg(windows)]
                win32::window::show_window(&win);
                #[cfg(not(windows))]
                let _ = win.show();
                let _ = win.set_focus();
                let _ = app.emit("note://shown", num);
            }
        }
    });
}

/// The note page reports its event listeners are wired — safe to reveal.
/// Called by `src/note/main.ts` after every `listen()` subscription has
/// resolved, so the `note://shown` pop-in event can never be missed.
#[tauri::command]
fn note_window_ready(app: tauri::AppHandle, num: u32) {
    let label = format!("note-{num}");
    if let Some(win) = app.get_webview_window(&label) {
        // Same webview-resume/window-reveal pairing every table uses.
        #[cfg(windows)]
        win32::window::show_window(&win);
        #[cfg(not(windows))]
        let _ = win.show();
        let _ = win.set_focus();
        let _ = app.emit("note://shown", num);
    }
}

// ===== System stats (CPU, RAM, GPU) =====
#[derive(serde::Serialize)]
struct SystemStats {
    cpu_usage: f32,
    ram_usage: f32,
    ram_total_gb: f32,
    ram_used_gb: f32,
}

#[tauri::command]
fn get_system_stats() -> SystemStats {
    #[cfg(windows)]
    {
        use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

        // RAM
        let mut mem_status = MEMORYSTATUSEX::default();
        mem_status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        unsafe { let _ = GlobalMemoryStatusEx(&mut mem_status); }
        let ram_usage = mem_status.dwMemoryLoad as f32;
        let ram_total_gb = (mem_status.ullTotalPhys as f64 / 1073741824.0) as f32;
        let ram_used_gb = ram_total_gb * ram_usage / 100.0;

        // CPU usage — simplified: use GetSystemTimes for idle/kernel/user
        // For a quick approximation, we use the memory load as a proxy.
        // Real CPU usage requires two samples of GetSystemTimes with a delay.
        let cpu_usage = get_cpu_usage();

        return SystemStats { cpu_usage, ram_usage, ram_total_gb, ram_used_gb };
    }
    #[cfg(not(windows))]
    {
        SystemStats { cpu_usage: 0.0, ram_usage: 0.0, ram_total_gb: 0.0, ram_used_gb: 0.0 }
    }
}

// ===== Battery status (laptops) =====
// Returns None on desktops (no battery) — GetSystemPowerStatus reports
// BatteryFlag 128 (no system battery) there.
#[derive(serde::Serialize)]
struct BatteryStatus {
    percent: u32,
    charging: bool,
}

#[tauri::command]
fn get_battery_status() -> Option<BatteryStatus> {
    #[cfg(windows)]
    {
        use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        let mut sps = SYSTEM_POWER_STATUS::default();
        unsafe {
            if GetSystemPowerStatus(&mut sps).is_err() {
                return None;
            }
        }
        // 128 = no battery, 255 = unknown. BatteryLifePercent 255 = unknown.
        if sps.BatteryFlag & 128 != 0 || sps.BatteryLifePercent == 255 {
            return None;
        }
        Some(BatteryStatus {
            percent: sps.BatteryLifePercent as u32,
            charging: sps.ACLineStatus == 1,
        })
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
fn get_cpu_usage() -> f32 {
    use windows::Win32::Foundation::FILETIME;

    extern "system" {
        fn GetSystemTimes(
            idle_time: *mut FILETIME,
            kernel_time: *mut FILETIME,
            user_time: *mut FILETIME,
        ) -> i32;
    }

    // CPU usage is computed from TWO samples of GetSystemTimes taken some
    // time apart. The previous implementation slept 100 ms between samples
    // every call — that blocks a Tauri worker thread for 100 ms every time
    // the launcher's sysmon widget polls (every 2 s), and during a post-idle
    // IPC burst many such calls queue up, saturating the worker pool and
    // amplifying the "Not Responding" hang.
    //
    // Instead, take a single sample here. We compare it against the
    // PREVIOUS sample (stored in a static). If enough time has passed since
    // the last sample (>= 200 ms), we compute usage from the deltas and
    // store the new sample as the new baseline. If not enough time has
    // passed (caller polling too fast), we return 0% rather than sleep.
    // First call returns 0% (no baseline yet) — the next call has a real
    // delta. This costs one extra sample on the second call but after that
    // steady-state is reached.
    static CPU_LAST: parking_lot::Mutex<Option<(std::time::Instant, u64, u64, u64)>> =
        parking_lot::const_mutex(None);

    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { let _ = GetSystemTimes(&mut idle, &mut kernel, &mut user); }
    let idle_v = filetime_to_u64(&idle);
    let kernel_v = filetime_to_u64(&kernel);
    let user_v = filetime_to_u64(&user);
    let now = std::time::Instant::now();

    let mut last = CPU_LAST.lock();
    if let Some((prev_ts, prev_idle, prev_kernel, prev_user)) = *last {
        let elapsed = now.duration_since(prev_ts);
        // Require at least 200 ms between samples for a meaningful delta.
        // The launcher polls every 2 s, so this is always satisfied in
        // practice after the first call; but it guards against pathological
        // tight-loops if the widget were ever to be invoked faster.
        if elapsed.as_secs_f64() >= 0.2 {
            let d_idle = idle_v.saturating_sub(prev_idle);
            let d_kernel = kernel_v.saturating_sub(prev_kernel);
            let d_user = user_v.saturating_sub(prev_user);
            *last = Some((now, idle_v, kernel_v, user_v));
            let total = d_kernel + d_user;
            if total > 0 {
                return (1.0 - (d_idle as f32 / total as f32)) * 100.0;
            }
            return 0.0;
        }
        // Not enough time elapsed — return the previous baseline's delta
        // against the current sample (a smaller, noisier delta but better
        // than sleeping).
        let d_idle = idle_v.saturating_sub(prev_idle);
        let d_kernel = kernel_v.saturating_sub(prev_kernel);
        let d_user = user_v.saturating_sub(prev_user);
        let total = d_kernel + d_user;
        if total > 0 {
            return (1.0 - (d_idle as f32 / total as f32)) * 100.0;
        }
        return 0.0;
    }

    // First ever call — store the sample, return 0% (no baseline yet).
    *last = Some((now, idle_v, kernel_v, user_v));
    0.0
}

#[cfg(windows)]
fn filetime_to_u64(ft: &windows::Win32::Foundation::FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
}

// ===== Volume control (Windows Core Audio API) =====
#[cfg(windows)]
fn get_volume_impl() -> f32 {
    use windows::Win32::Media::Audio::{
        IMMDeviceEnumerator, MMDeviceEnumerator, eRender, eConsole,
    };
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, CoCreateInstance, COINIT_MULTITHREADED, CLSCTX_ALL};
    

    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let vol = unsafe {
        let enumerator: IMMDeviceEnumerator = match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
            Ok(e) => e,
            Err(_) => { CoUninitialize(); return 0.5; }
        };
        let device = match enumerator.GetDefaultAudioEndpoint(eRender, eConsole) {
            Ok(d) => d,
            Err(_) => { CoUninitialize(); return 0.5; }
        };
        let endpoint_volume: IAudioEndpointVolume = match device.Activate(CLSCTX_ALL, None) {
            Ok(v) => v,
            Err(_) => { CoUninitialize(); return 0.5; }
        };
        endpoint_volume.GetMasterVolumeLevelScalar().unwrap_or(0.5)
    };
    unsafe { CoUninitialize() };
    vol
}

#[cfg(windows)]
fn set_volume_impl(volume: f32) {
    use windows::Win32::Media::Audio::{
        IMMDeviceEnumerator, MMDeviceEnumerator, eRender, eConsole,
    };
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, CoCreateInstance, COINIT_MULTITHREADED, CLSCTX_ALL};
    

    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    unsafe {
        if let Ok(enumerator) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) {
            if let Ok(device) = enumerator.GetDefaultAudioEndpoint(eRender, eConsole) {
                if let Ok(endpoint_volume) = device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) {
                    let _ = endpoint_volume.SetMasterVolumeLevelScalar(volume, &windows::core::GUID::zeroed());
                }
            }
        }
    }
    unsafe { CoUninitialize() };
}

#[tauri::command]
fn get_volume() -> f32 {
    #[cfg(windows)]
    { return get_volume_impl(); }
    #[cfg(not(windows))]
    { 0.5 }
}

#[tauri::command]
fn set_volume(volume: f32) {
    #[cfg(windows)]
    { set_volume_impl(volume); }
}

#[derive(serde::Serialize)]
struct AppVolume {
    pid: u32,
    name: String,
    volume: f32,
    icon: Option<String>,
}

/// Friendly short name + full image path for a pid ("chrome", "spotify").
/// None when the process already exited or refuses the limited query.
#[cfg(windows)]
fn app_volume_process_info(pid: u32) -> Option<(String, String)> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let res = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(handle);
        res.ok()?;
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        let stem = std::path::Path::new(&full)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("app")
            .to_string();
        Some((stem, full))
    }
}

// Icon data-URLs cost a shell round-trip each (win32::icon), and the widget
// refreshes every couple of seconds — cache them by full image path so a
// session list repaint never re-extracts.
#[cfg(windows)]
static APP_VOLUME_ICON_CACHE: std::sync::Mutex<
    Option<std::collections::HashMap<String, Option<String>>>,
> = std::sync::Mutex::new(None);

#[cfg(windows)]
fn app_volume_icon(exe_path: &str) -> Option<String> {
    let mut guard = APP_VOLUME_ICON_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(std::collections::HashMap::new);
    if let Some(hit) = map.get(exe_path) {
        return hit.clone();
    }
    let icon = crate::win32::icon::extract_icon_for_path(exe_path);
    map.insert(exe_path.to_string(), icon.clone());
    icon
}

// Shared WASAPI plumbing for both app-volume commands: COM init (the
// Tauri command threads have no apartment), default render device, then
// the session enumerator. IAudioSessionManager2 owns GetSessionEnumerator
// in the windows crate (COM inheritance is not flattened onto the parent)
// and hands out the sessions of the default render device — exactly what
// the master slider above already targets.
#[cfg(windows)]
fn with_app_sessions<T>(
    f: impl FnOnce(&windows::Win32::Media::Audio::IAudioSessionEnumerator) -> Option<T>,
) -> Option<T> {
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioSessionEnumerator, IAudioSessionManager2, IMMDeviceEnumerator,
        MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let mut result = None;
        // Labeled block instead of an inner closure — closure bodies do not
        // inherit the enclosing unsafe context, and every early exit still
        // has to reach the CoUninitialize below.
        'setup: {
            let enumerator: IMMDeviceEnumerator =
                match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
                    Ok(e) => e,
                    Err(_) => break 'setup,
                };
            let device = match enumerator.GetDefaultAudioEndpoint(eRender, eConsole) {
                Ok(d) => d,
                Err(_) => break 'setup,
            };
            let manager: IAudioSessionManager2 = match device.Activate(CLSCTX_ALL, None) {
                Ok(m) => m,
                Err(_) => break 'setup,
            };
            let sessions: IAudioSessionEnumerator = match manager.GetSessionEnumerator() {
                Ok(s) => s,
                Err(_) => break 'setup,
            };
            result = f(&sessions);
        }
        CoUninitialize();
        result
    }
}

#[cfg(windows)]
fn get_app_volumes_impl() -> Vec<AppVolume> {
    use windows::core::Interface;
    use windows::Win32::Media::Audio::{IAudioSessionControl2, ISimpleAudioVolume};

    with_app_sessions(|sessions| unsafe {
        let count = sessions.GetCount().unwrap_or(0);
        let mut out: Vec<AppVolume> = Vec::new();
        let mut seen: Vec<u32> = Vec::new();
        for i in 0..count {
            let ctrl = match sessions.GetSession(i) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let ctrl2 = match ctrl.cast::<IAudioSessionControl2>() {
                Ok(c) => c,
                Err(_) => continue,
            };
            let pid = match ctrl2.GetProcessId() {
                Ok(p) => p,
                Err(_) => continue,
            };
            // pid 0 is the system-sounds session — not a mixer row. One row
            // per process even when an app runs several sessions (the
            // setter below applies to every session of the pid anyway).
            if pid == 0 || seen.contains(&pid) {
                continue;
            }
            let simple = match ctrl.cast::<ISimpleAudioVolume>() {
                Ok(s) => s,
                Err(_) => continue,
            };
            let volume = simple.GetMasterVolume().unwrap_or(1.0);
            let Some((name, exe)) = app_volume_process_info(pid) else {
                continue;
            };
            seen.push(pid);
            out.push(AppVolume {
                pid,
                name,
                volume,
                icon: app_volume_icon(&exe),
            });
        }
        Some(out)
        })
    .unwrap_or_default()
}

#[cfg(windows)]
fn set_app_volume_impl(pid: u32, volume: f32) {
    use windows::core::Interface;
    use windows::Win32::Media::Audio::ISimpleAudioVolume;

    with_app_sessions(|sessions| unsafe {
        let count = sessions.GetCount().unwrap_or(0);
        for i in 0..count {
            let Ok(ctrl) = sessions.GetSession(i) else { continue };
            let Ok(simple) = ctrl.cast::<ISimpleAudioVolume>() else {
                continue;
            };
            // Match on the session's owning process — an app with multiple
            // streams gets all of them moved together.
            let Ok(ctrl2) = ctrl.cast::<windows::Win32::Media::Audio::IAudioSessionControl2>()
            else {
                continue;
            };
            if ctrl2.GetProcessId() != Ok(pid) {
                continue;
            }
            let _ = simple.SetMasterVolume(
                volume.clamp(0.0, 1.0),
                &windows::core::GUID::zeroed(),
            );
        }
        Some(())
        });
}

#[tauri::command]
fn get_app_volumes() -> Vec<AppVolume> {
    #[cfg(windows)]
    { return get_app_volumes_impl(); }
    #[cfg(not(windows))]
    { Vec::new() }
}

#[tauri::command]
fn set_app_volume(pid: u32, volume: f32) {
    #[cfg(windows)]
    { set_app_volume_impl(pid, volume); }
}

// ===== Widget positions =====
#[tauri::command]
fn save_widget_positions(positions: std::collections::HashMap<String, (f64, f64)>) {
    persist::save_widget_positions(&positions);
}

#[tauri::command]
fn load_widget_positions() -> std::collections::HashMap<String, (f64, f64)> {
    persist::load_widget_positions()
}

// ===== Widget visibility =====
#[tauri::command]
fn save_widget_visibility(visibility: std::collections::HashMap<String, bool>) {
    persist::save_widget_visibility(&visibility);
}

#[tauri::command]
fn load_widget_visibility() -> std::collections::HashMap<String, bool> {
    persist::load_widget_visibility()
}

// ===== Icon recolor =====
#[tauri::command]
fn save_icon_recolor(enabled: bool) {
    persist::save_icon_recolor(enabled);
}

#[tauri::command]
fn load_icon_recolor() -> bool {
    persist::load_icon_recolor()
}

// ===== Settings =====
#[tauri::command]
fn save_settings(settings: serde_json::Value, app: tauri::AppHandle) {
    let mut current = persist::load_settings();

    // Update fields from the JSON
    if let Some(theme) = settings.get("theme").and_then(|v| v.as_str()) {
        current.theme = theme.to_string();
    }
    if let Some(auto) = settings.get("auto_fullscreen").and_then(|v| v.as_bool()) {
        current.auto_fullscreen = auto;
    }
    if let Some(interval) = settings.get("refresh_interval").and_then(|v| v.as_u64()) {
        current.refresh_interval = interval;
    }
    if let Some(cube) = settings.get("cube_animation").and_then(|v| v.as_bool()) {
        current.cube_animation = cube;
    }
    // --- Tables update (0.2) ---
    if let Some(v) = settings.get("tables_hold_ms").and_then(|v| v.as_u64()) {
        current.tables_hold_ms = v.clamp(80, 1000);
    }
    if let Some(v) = settings.get("clock_24h").and_then(|v| v.as_bool()) {
        current.clock_24h = v;
    }
    if let Some(v) = settings.get("pie_clock").and_then(|v| v.as_bool()) {
        current.pie_clock = v;
    }
    if let Some(v) = settings.get("show_desktop_grid").and_then(|v| v.as_bool()) {
        current.show_desktop_grid = v;
    }
    if let Some(v) = settings.get("dimmer_level").and_then(|v| v.as_f64()) {
        current.dimmer_level = v.clamp(0.0, 1.0);
    }
    if let Some(v) = settings.get("language").and_then(|v| v.as_str()) {
        // Only the shipped dictionaries are accepted; anything else keeps
        // the current value (the frontend falls back to en regardless).
        if v == "en" || v == "ru" {
            current.language = v.to_string();
        }
    }
    persist::save_settings(&current);

    // The hold threshold lives in the keyboard hook — keep it in sync.
    #[cfg(windows)]
    win32::hotkey::HOLD_MS
        .store(current.tables_hold_ms.clamp(80, 1000), Ordering::SeqCst);

    // Brightness dimmer applies live (systemless overlay — see
    // win32::dimmer). The level is persisted so it survives restarts.
    #[cfg(windows)]
    win32::dimmer::set_level(current.dimmer_level);

    // Taskbar clock and the launcher react to format/behavior changes live.
    // language rides the same broadcast — every window's i18n module
    // listens for it and re-translates in place (src/shared/i18n.ts).
    let _ = app.emit(
        "settings://changed",
        serde_json::json!({
            "clock_24h": current.clock_24h,
            "pie_clock": current.pie_clock,
            "show_desktop_grid": current.show_desktop_grid,
            "language": current.language,
        }),
    );
    log::info!("Settings saved — theme: {}", current.theme);
}

#[tauri::command]
fn load_settings() -> persist::Settings {
    persist::load_settings()
}

/// Clear the icon cache (SHGetFileInfoW results cached per path).
#[tauri::command]
fn clear_icon_cache() -> usize {
    crate::win32::icon::clear_cache()
}

/// Launch-time elevation state — used by the brightness widget's warning.
/// (0.3.0: previously invoked but never registered, so the warning could
/// never render.)
#[derive(serde::Serialize)]
struct ElevationState {
    elevated: bool,
    uac_declined: bool,
}

#[tauri::command]
fn get_elevation_state() -> ElevationState {
    ElevationState {
        elevated: elevation::is_elevated(),
        uac_declined: elevation::UAC_DECLINED.load(std::sync::atomic::Ordering::SeqCst),
    }
}

// ===== Language indicator =====
#[tauri::command]
fn get_language() -> String {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayout, GetKeyState, VK_CAPITAL};

        unsafe {
            let hwnd = GetForegroundWindow();
            let mut pid: u32 = 0;
            let mut thread_id: u32 = 0;

            if !hwnd.is_invalid() {
                thread_id = GetWindowThreadProcessId(hwnd, Some(&mut pid));
            }

            // Get the keyboard layout for the foreground window's thread
            let hkl = GetKeyboardLayout(thread_id);
            // The low word of the HKL contains the language identifier
            let lang_id = (hkl.0 as usize & 0xFFFF) as u32;

            let lang_code = match lang_id {
                0x0409 => "en",
                0x0419 => "ru",
                0x040C => "fr",
                0x0407 => "de",
                0x0410 => "it",
                0x040A => "es",
                0x0415 => "pl",
                0x0422 => "uk",
                _ => "??",
            };

            // Check capslock
            let caps_state = GetKeyState(VK_CAPITAL.0 as i32);
            let caps_on = (caps_state & 0x0001) != 0;
            if caps_on {
                return lang_code.to_uppercase();
            }
            return lang_code.to_string();
        }
    }
    #[cfg(not(windows))]
    {
        "en".to_string()
    }
}

// ===== Save clipboard image =====
#[tauri::command]
fn save_clipboard_image(data_url: String) {
    // Decode base64 from data URL and save to clipboard
    // For now, save the data URL in clipboard history
    if let Some(b64) = data_url.strip_prefix("data:image/png;base64,") {
        use base64::Engine;
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) {
            // Save to %TEMP%\flatshot.png (overwrite)
            if let Ok(temp) = std::env::var("TEMP") {
                let path = std::path::PathBuf::from(temp).join("flatshot.png");
                let _ = std::fs::write(&path, &bytes);
            }
        }
    }
}


// ===== Clipboard image (Win32 — Lightshot-style: CF_DIB + registered PNG) =====
/// Puts a REAL image on the system clipboard so pasting into any app inserts
/// the picture, not a base64 text string.
#[tauri::command]
fn set_clipboard_image(data_url: String) {
    #[cfg(windows)]
    {
        use base64::Engine;
        
        use windows::Win32::System::DataExchange::{
            CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW,
            SetClipboardData,
        };
        use windows::Win32::System::Ole::CF_DIB;
        
        use windows::core::w;

        let Some(b64) = data_url.strip_prefix("data:image/png;base64,") else {
            log::warn!("set_clipboard_image: not a PNG data URL");
            return;
        };
        let Ok(png) = base64::engine::general_purpose::STANDARD.decode(b64) else {
            log::warn!("set_clipboard_image: invalid base64 payload");
            return;
        };
        let Ok(img) = image::load_from_memory(&png) else {
            log::warn!("set_clipboard_image: could not decode PNG");
            return;
        };
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width() as i32, rgba.height() as i32);
        if w == 0 || h == 0 {
            return;
        }

        // Bottom-up 32bpp CF_DIB (BI_RGB) — the format MS Paint / Office expect.
        let mut dib: Vec<u8> = Vec::with_capacity(40 + rgba.len());
        dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
        dib.extend_from_slice(&w.to_le_bytes()); // biWidth
        dib.extend_from_slice(&h.to_le_bytes()); // biHeight (positive = bottom-up)
        dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
        dib.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
        dib.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
        dib.extend_from_slice(&((w * h * 4) as u32).to_le_bytes()); // biSizeImage
        dib.extend_from_slice(&2835u32.to_le_bytes()); // biXPelsPerMeter (~72 DPI)
        dib.extend_from_slice(&2835u32.to_le_bytes()); // biYPelsPerMeter
        dib.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
        dib.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant
        for y in (0..h).rev() {
            let row_start = (y as usize) * (w as usize) * 4;
            let row = &rgba.as_raw()[row_start..row_start + (w as usize) * 4];
            for px in row.chunks_exact(4) {
                dib.push(px[2]); // B
                dib.push(px[1]); // G
                dib.push(px[0]); // R
                dib.push(255); // A
            }
        }

        unsafe {
            if OpenClipboard(None).is_err() {
                log::warn!("set_clipboard_image: OpenClipboard failed");
                return;
            }
            let _ = EmptyClipboard();
            let dib_ok = put_clipboard_data(CF_DIB.0 as u32, &dib);
            let png_fmt = RegisterClipboardFormatW(w!("PNG"));
            let png_ok = put_clipboard_data(png_fmt, &png);
            let _ = CloseClipboard();
            log::info!("set_clipboard_image: {}x{} -> dib={} png={}", w, h, dib_ok, png_ok);
        }

        unsafe fn put_clipboard_data(format: u32, data: &[u8]) -> bool {
            use windows::Win32::Foundation::HANDLE;
            use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

            let Ok(hg) = GlobalAlloc(GMEM_MOVEABLE, data.len()) else {
                return false;
            };
            let ptr = GlobalLock(hg);
            if ptr.is_null() {
                return false;
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr as *mut u8, data.len());
            let _ = GlobalUnlock(hg);
            match SetClipboardData(format, Some(HANDLE(hg.0))) {
                Ok(_) => true, // ownership transferred to the clipboard
                Err(e) => {
                    log::warn!("SetClipboardData(fmt {format}) failed: {e}");
                    false
                }
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = data_url;
    }
}

// ===== Notification framework =====
//
// A small bottom-right toast window ("notification", declared in
// tauri.conf.json, hidden by default). `notify` positions it just above the
// bottom-right corner of the primary monitor, shows it and emits the payload;
// the frontend plays a slide-in. After `duration_ms` the backend asks the
// frontend to play its slide-out (notify://hide) and hides the window when
// the frontend reports back via `notification_close_finished`.
//
// 0.3.0: the payload can carry an optional progress value (0-100); the
// frontend then renders a statusbar inside the toast (used by the search
// index build). A generation counter guards the hide timer — a toast shown
// while an older toast's timer is still pending is never hidden early.
#[derive(serde::Serialize, Clone)]
struct NotificationPayload {
    title: String,
    body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    progress: Option<f32>,
}

/// Monotonic toast generation — bumped on every show/update of the toast.
static NOTIFY_SEQ: AtomicU64 = AtomicU64::new(0);

/// Bottom-right toast anchor height (physical px math). Must match the
/// window height in tauri.conf.json.
const TOAST_LOGICAL_H: f64 = 140.0;

fn position_notification(win: &tauri::WebviewWindow) {
    if let Ok(Some(monitor)) = win.primary_monitor() {
        let mw = monitor.size().width as f64;
        let mh = monitor.size().height as f64;
        let scale = monitor.scale_factor();
        let lw = 340.0;
        let x = mw - lw * scale - 16.0 * scale;
        let y = mh - TOAST_LOGICAL_H * scale - 16.0 * scale;
        let _ = win.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
    }
}

fn show_notification(handle: &tauri::AppHandle, title: &str, body: &str, duration_ms: u64) {
    show_notification_payload(handle, NotificationPayload {
        title: title.to_string(),
        body: body.to_string(),
        progress: None,
    });
    schedule_notification_hide(handle, duration_ms);
}

fn show_notification_payload(handle: &tauri::AppHandle, payload: NotificationPayload) {
    let Some(win) = handle.get_webview_window("notification") else {
        log::warn!("notify: notification window not found");
        return;
    };

    NOTIFY_SEQ.fetch_add(1, Ordering::SeqCst);
    position_notification(&win);
    win32::window::show_window(&win);
    let _ = handle.emit("notify://show", payload);
}

/// Schedule the slide-out after `duration_ms`, guarded by the generation
/// counter so a toast shown later is never hidden by an older timer.
fn schedule_notification_hide(handle: &tauri::AppHandle, duration_ms: u64) {
    let seq = NOTIFY_SEQ.load(Ordering::SeqCst);
    let h = handle.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(duration_ms));
        if NOTIFY_SEQ.load(Ordering::SeqCst) == seq {
            let _ = h.emit("notify://hide", ());
        }
    });
}

#[tauri::command]
fn notify(app: tauri::AppHandle, title: String, body: String, duration_ms: Option<u64>) {
    show_notification(&app, &title, &body, duration_ms.unwrap_or(2000));
}

#[tauri::command]
fn notification_close_finished(app: tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("notification") {
        win32::window::hide_window(&win);
    }
}

// ===== Screensaver =====
//
// Fullscreen OLED window (pure black + a slow jelly ball and the clock).
// Launched via the "screensaver" shortcut in hushlight; any input dismisses
// it with a smooth fade handled by the frontend, which then reports back.
#[tauri::command]
fn show_screensaver(app: tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("screensaver") {
        let _ = win.set_fullscreen(true);
        win32::window::show_window(&win);
        let _ = win.set_focus();
        let _ = app.emit("screensaver://shown", ());
    }
}

#[tauri::command]
fn hide_screensaver(app: tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("screensaver") {
        win32::window::hide_window(&win);
        let _ = app.emit("screensaver://hidden", ());
    }
}

// ===== Brightness dimmer (systemless software dim overlay) =====
//
// The dimmer is a pure Win32 overlay — a black, topmost, fully
// click-through layered window whose alpha is the dim strength. Nothing on
// the system is modified (no WMI/DDC brightness, no registry, no power
// plan): closing Hush_UI or sliding to 0% removes the dim instantly and the
// display is exactly as before.
#[tauri::command]
fn set_dimmer_level(level: f64) {
    #[cfg(windows)]
    win32::dimmer::set_level(level.clamp(0.0, 1.0));
    #[cfg(not(windows))]
    let _ = level;
}

// ===== Exit Hush_UI — revert everything and quit =====
//
// Called when the user clicks the exit button in the settings table.
// Reverts all shell modifications:
//   1. Stop the start-menu killer (so StartMenuExperienceHost.exe can run)
//   2. Un-hide the native taskbar (stop the hider monitor, alpha back to 255)
//   3. Remove the brightness dim overlay (instantly restores full brightness)
//   4. Restart explorer.exe (restores the native shell: taskbar, Start menu,
//      desktop icons, tray)
//   5. Exit the Hush_UI process
#[tauri::command]
fn exit_hush() {
    log::info!("exit_hush: reverting everything and exiting");

    // 1. Stop the start-menu killer
    #[cfg(windows)]
    start_menu_killer::stop();

    // 2. Un-hide the native taskbar (stop the hide_taskbar monitor and set
    //    its alpha back to 255) so the shell comes back on exit.
    #[cfg(windows)]
    hide_taskbar::stop();

    // 3. Kill the brightness dim overlay so the user is never stuck dim
    //    after Hush_UI exits.
    #[cfg(windows)]
    win32::dimmer::set_level(0.0);

    // 4. Restart explorer.exe — this restores the native shell (taskbar,
    //    Start menu, desktop). We kill explorer first, then relaunch it.
    //    The relaunch uses ShellExecuteW with "open" on explorer.exe.
    #[cfg(windows)]
    {
        use std::process::Command;
        // Kill explorer — it will auto-restart, but we also relaunch it
        // explicitly to be sure.
        let _ = Command::new("taskkill")
            .args(["/f", "/im", "explorer.exe"])
            .spawn();
        // Give it a moment to die
        std::thread::sleep(std::time::Duration::from_millis(500));
        // Relaunch explorer
        let _ = Command::new("explorer.exe").spawn();
    }

    // 5. Exit the Hush_UI process
    std::process::exit(0);
}

// ===== System commands =====
#[tauri::command]
fn reboot_system() {
    log::info!("Rebooting system...");
    #[cfg(windows)]
    {
        use std::process::Command;
        let _ = Command::new("shutdown").args(["/r", "/t", "0"]).spawn();
    }
}

#[tauri::command]
fn shutdown_system() {
    log::info!("Shutting down system...");
    #[cfg(windows)]
    {
        use std::process::Command;
        let _ = Command::new("shutdown").args(["/s", "/t", "0"]).spawn();
    }
}

#[tauri::command]
fn add_to_startup() -> bool {
    log::info!("Adding Hush_UI to startup...");
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let startup_dir = std::path::PathBuf::from(&appdata)
                .join("Microsoft\\Windows\\Start Menu\\Programs\\Startup");
            let _ = std::fs::create_dir_all(&startup_dir);

            if let Ok(exe_path) = std::env::current_exe() {
                // Create a .lnk shortcut using PowerShell (reliable, no COM dependency)
                let lnk_path = startup_dir.join("Hush_UI.lnk");
                let exe_dir = exe_path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                let ps_script = format!(
                    "$ws = New-Object -ComObject WScript.Shell; $s = $ws.CreateShortcut('{}'); $s.TargetPath = '{}'; $s.WorkingDirectory = '{}'; $s.Save()",
                    lnk_path.display(),
                    exe_path.display(),
                    exe_dir.display()
                );
                match std::process::Command::new("powershell")
                    .args(["-NoProfile", "-Command", &ps_script])
                    .output()
                {
                    Ok(output) => {
                        if output.status.success() {
                            log::info!("Created Hush_UI.lnk shortcut in startup");
                            return true;
                        } else {
                            log::error!("PowerShell shortcut creation failed: {}", String::from_utf8_lossy(&output.stderr));
                        }
                    }
                    Err(e) => log::error!("Failed to run PowerShell: {}", e),
                }

                // Fallback: .bat file
                let bat_path = startup_dir.join("Hush_UI.bat");
                let exe_dir = exe_path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                let bat_content = format!(
                    "@echo off\ncd /d \"{}\"\nstart \"\" \"{}\"",
                    exe_dir.display(),
                    exe_path.display()
                );
                if std::fs::write(&bat_path, bat_content).is_ok() {
                    log::info!("Created Hush_UI.bat fallback in startup");
                    return true;
                }
            }
        }
        false
    }
    #[cfg(not(windows))]
    { false }
}

#[tauri::command]
fn remove_from_startup() -> bool {
    log::info!("Removing Hush_UI from startup...");
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let startup_dir = std::path::PathBuf::from(&appdata)
                .join("Microsoft\\Windows\\Start Menu\\Programs\\Startup");
            let bat_path = startup_dir.join("Hush_UI.bat");
            let lnk_path = startup_dir.join("Hush_UI.lnk");
            // Also clean up pre-rename flatui shortcuts (the app was called
            // flatui before Hush_UI — old startup entries must go too).
            let legacy_bat = startup_dir.join("flatui.bat");
            let legacy_lnk = startup_dir.join("flatui.lnk");
            let legacy_bat2 = startup_dir.join("Flat_UI.bat");
            let legacy_lnk2 = startup_dir.join("Flat_UI.lnk");
            let mut removed = false;
            for path in [bat_path, lnk_path, legacy_bat, legacy_lnk, legacy_bat2, legacy_lnk2] {
                if path.exists() {
                    let _ = std::fs::remove_file(&path);
                    removed = true;
                }
            }
            log::info!("Removed from startup: {}", removed);
            return removed;
        }
        false
    }
    #[cfg(not(windows))]
    { false }
}

// ===== Clipboard text (global, via Win32 API) =====
#[tauri::command]
fn get_clipboard_text() -> Option<String> {
    #[cfg(windows)]
    {
        use windows::Win32::System::DataExchange::{
            OpenClipboard, CloseClipboard, GetClipboardData,
        };
        use windows::Win32::System::Ole::CF_UNICODETEXT;

        unsafe {
            if OpenClipboard(None).is_err() {
                return None;
            }

            let result = match GetClipboardData(CF_UNICODETEXT.0 as u32) {
                Ok(handle) => {
                    let ptr = handle.0 as *const u16;
                    if ptr.is_null() {
                        None
                    } else {
                        let mut len = 0;
                        while *ptr.add(len) != 0 {
                            len += 1;
                        }
                        let slice = std::slice::from_raw_parts(ptr, len);
                        Some(String::from_utf16_lossy(slice))
                    }
                }
                Err(_) => None,
            };

            let _ = CloseClipboard();
            result
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[tauri::command]
fn set_clipboard_text(text: String) {
    #[cfg(windows)]
    {
        use windows::Win32::System::DataExchange::{
            OpenClipboard, CloseClipboard, EmptyClipboard, SetClipboardData,
        };
        use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
        use windows::Win32::System::Ole::CF_UNICODETEXT;
        use std::os::windows::ffi::OsStrExt;
        use std::ffi::OsStr;

        let wide: Vec<u16> = OsStr::new(&text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let byte_len = wide.len() * 2;

        unsafe {
            if OpenClipboard(None).is_err() {
                return;
            }
            let _ = EmptyClipboard();

            if let Ok(h_mem) = GlobalAlloc(GMEM_MOVEABLE, byte_len) {
                let ptr = GlobalLock(h_mem);
                if !ptr.is_null() {
                    std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr as *mut u8, byte_len);
                    let _ = GlobalUnlock(h_mem);
                    let handle = windows::Win32::Foundation::HANDLE(h_mem.0);
                    let _ = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(handle));
                }
            }

            let _ = CloseClipboard();
        }
    }
}

// ===== Theme commands =====
#[tauri::command]
fn get_active_theme() -> Option<persist::Theme> {
    persist::get_active_theme()
}

#[tauri::command]
fn get_all_themes() -> Vec<persist::Theme> {
    persist::load_themes().themes
}

#[tauri::command]
fn set_active_theme(name: String) {
    persist::set_active_theme(&name);
    log::info!("Active theme set to: {}", name);
}

// ===== End task — kill the process owning the window =====
#[tauri::command]
fn end_task(app_id: String) {
    #[cfg(windows)]
    {
        if let Some(hwnd_val) = app_id.strip_prefix("run:").and_then(|s| s.parse::<isize>().ok()) {
            let hwnd = windows::Win32::Foundation::HWND(hwnd_val as *mut std::ffi::c_void);
            unsafe {
                // Get the process ID, then open + terminate
                use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
                use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
                use windows::Win32::Foundation::CloseHandle;

                let mut pid: u32 = 0;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                if pid == 0 {
                    log::error!("end_task: failed to get pid");
                    return;
                }
                if let Ok(handle) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                    let _ = TerminateProcess(handle, 0);
                    let _ = CloseHandle(handle);
                    log::info!("end_task: terminated pid {}", pid);
                } else {
                    log::error!("end_task: OpenProcess failed for pid {}", pid);
                }
            }
        }
    }
}

// ===== Run dialog (Win+R replacement) =====
#[tauri::command]
fn execute_run(command: String, app: tauri::AppHandle) {
    log::info!("execute_run: {}", command);
    #[cfg(windows)]
    {
        if let Err(e) = win32::shell::shell_execute(&command) {
            log::error!("execute_run failed: {e}");
        }
    }
    let _ = app;
}

#[tauri::command]
fn execute_run_admin(command: String) {
    log::info!("execute_run_admin: {}", command);
    #[cfg(windows)]
    {
        // Hush internals and URI launches have no "runas" association — the
        // elevation verb only applies to real executable targets. Falling
        // back to the normal path keeps a Shift-press from silently no-op'ing
        // on those results (reboot/shutdown/screensaver just run; Settings
        // pages and shell: folders just open).
        if command.starts_with("hush:")
            || command.starts_with("ms-settings:")
            || command.starts_with("shell:")
        {
            if let Err(e) = win32::shell::shell_execute(&command) {
                log::error!("execute_run_admin: non-elevatable '{command}' fallback failed: {e}");
            }
            return;
        }

        // Elevation needs the right thread conditions. Tauri runs commands on
        // pooled worker threads that have NO COM apartment and NO message
        // pump — the plain ShellExecuteW("runas") that used to live here
        // failed silently in exactly that environment (no UAC, no launch,
        // the error swallowed by `let _ =`). The shell docs are explicit:
        // COM must be initialized before ShellExecuteEx, and
        // SEE_MASK_NOASYNC must be passed when the calling thread has no
        // message loop (it would otherwise wait on a conversation that can
        // never complete). The startup elevation path only ever worked
        // because run() executes on the main thread, which has both.
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        unsafe {
            // STA apartment for the shell call. S_OK / S_FALSE (already up)
            // are both fine; RPC_E_CHANGED_MODE means this pool thread
            // already initialized COM in another mode — usable either way,
            // so the result is intentionally ignored.
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }

        let wide_file: Vec<u16> = OsStr::new(&command)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let verb: Vec<u16> = OsStr::new("runas")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let mut sei = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(wide_file.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };

        // Synchronous (NOASYNC): returns after the elevation hand-off. The
        // crate maps "FALSE + GetLastError" into Err, so a declined UAC
        // (ERROR_CANCELLED) and hard failures both land in the log instead
        // of vanishing.
        if let Err(e) = unsafe { ShellExecuteExW(&mut sei) } {
            log::error!("execute_run_admin: ShellExecuteExW('runas') failed for '{command}': {e}");
        }
    }
}

// ===== Search installed programs (Start Menu shortcuts) + system shortcuts =====
#[derive(serde::Serialize)]
struct SearchResult {
    id: String,
    name: String,
    path: String,
    icon_data_url: Option<String>,
    is_folder: bool,
}

/// Filter + rank + disambiguate a candidate list. Shared by the indexed and
/// the fallback walk path. Icons are NOT resolved here — the caller attaches
/// them AFTER truncation so a keystroke costs ≤ 20 icon lookups (cache hits
/// after the first time), never one per match.
fn rank_search_results(
    mut results: Vec<SearchResult>,
    q: &str,
) -> Vec<SearchResult> {
    // Disambiguate colliding display names. When several results share a
    // label, non-folder items show their full file name with extension, so
    // a folder becomes the only entry named exactly like its siblings
    // (mirrors the desktop grid rule).
    {
        let mut name_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for r in results.iter() {
            *name_counts.entry(r.name.to_lowercase()).or_default() += 1;
        }
        for r in results.iter_mut() {
            let count = name_counts.get(&r.name.to_lowercase()).copied().unwrap_or(0);
            if count > 1 && !r.is_folder {
                if let Some(fname) =
                    std::path::Path::new(&r.path).file_name().and_then(|s| s.to_str())
                {
                    r.name = fname.to_string();
                }
            }
        }
    }

    // Rank: the position of the query inside the name (prefix matches
    // first), then alphabetically. Stable and cheap.
    let ql = q.to_lowercase();
    results.sort_by(|a, b| {
        let ia = a.name.to_lowercase().find(&ql).unwrap_or(usize::MAX);
        let ib = b.name.to_lowercase().find(&ql).unwrap_or(usize::MAX);
        ia.cmp(&ib).then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    results.truncate(20);
    results
}

#[tauri::command]
fn search_programs(query: String) -> Vec<SearchResult> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }

    #[cfg(windows)]
    {
        // 1. Built-in system shortcuts (control panel, add/remove, etc.)
        let system_shortcuts = [
            ("control panel", "control.exe"),
            ("device manager", "devmgmt.msc"),
            ("task manager", "taskmgr.exe"),
            ("registry editor", "regedit.exe"),
            ("services", "services.msc"),
            ("event viewer", "eventvwr.msc"),
            ("disk management", "diskmgmt.msc"),
            ("computer management", "compmgmt.msc"),
            ("local security policy", "secpol.msc"),
            ("group policy", "gpedit.msc"),
            ("command prompt", "cmd.exe"),
            ("cmd", "cmd.exe"),
            ("powershell", "powershell.exe"),
            ("windows terminal", "wt.exe"),
            ("settings", "ms-settings:"),
            ("display settings", "ms-settings:display"),
            ("network settings", "ms-settings:network"),
            ("bluetooth", "ms-settings:bluetooth"),
            ("apps", "ms-settings:appsfeatures"),
            ("personalization", "ms-settings:personalization"),
            ("windows update", "ms-settings:windowsupdate"),
            ("system information", "msinfo32.exe"),
            ("file explorer", "explorer.exe"),
            ("recycle bin", "explorer.exe shell:RecycleBinFolder"),
            ("this pc", "explorer.exe"),
            ("snipping tool", "snippingtool.exe"),
            ("calculator", "calc.exe"),
            ("notepad", "notepad.exe"),
            ("paint", "mspaint.exe"),
            ("magnifier", "magnify.exe"),
            ("narrator", "narrator.exe"),
            ("on-screen keyboard", "osk.exe"),
            // Hush_UI commands
            ("reboot", "hush:reboot"),
            ("restart", "hush:reboot"),
            ("shutdown", "hush:shutdown"),
            ("add hush to startup", "hush:addstartup"),
            ("remove hush from startup", "hush:removestartup"),
            ("screensaver", "hush:screensaver"),
        ];

        let mut results: Vec<SearchResult> = Vec::new();
        let mut seen_paths: std::collections::HashSet<String> = std::collections::HashSet::new();

        for (name, cmd) in system_shortcuts.iter() {
            if name.contains(&q) {
                results.push(SearchResult {
                    id: format!("sys:{}", cmd),
                    name: name.to_string(),
                    path: cmd.to_string(),
                    icon_data_url: None,
                    is_folder: false,
                });
            }
        }

        // 2. Filesystem candidates: walk Start Menu + Desktop directly. The
        // depth-limited walk on the idle-cooldown keystroke path is cheap
        // (and icons are only resolved for the final page of results).
        let mut matched: Vec<(String, String, bool)> = Vec::new(); // (name, path, is_folder)
        let start_menu_dirs: Vec<std::path::PathBuf> = vec![
            std::env::var("PROGRAMDATA")
                .map(std::path::PathBuf::from)
                .map(|p| p.join("Microsoft\\Windows\\Start Menu\\Programs"))
                .unwrap_or_default(),
            std::env::var("APPDATA")
                .map(std::path::PathBuf::from)
                .map(|p| p.join("Microsoft\\Windows\\Start Menu\\Programs"))
                .unwrap_or_default(),
        ];
        for dir in start_menu_dirs {
            walk_programs(&dir, &q, &mut matched, true);
        }
        // Desktop items (directories included so a desktop FOLDER is
        // never shadowed by a same-named .exe/.lnk).
        if let Ok(desktop_dir) = std::env::var("USERPROFILE")
            .map(std::path::PathBuf::from)
            .map(|p| p.join("Desktop"))
        {
            walk_programs(&desktop_dir, &q, &mut matched, true);
        }

        for (name, path, is_folder) in matched {
            let key = path.to_lowercase();
            if !seen_paths.insert(key) {
                continue;
            }
            results.push(SearchResult {
                id: path.clone(),
                name,
                path,
                icon_data_url: None,
                is_folder,
            });
        }

        let mut results = rank_search_results(results, &q);

        // Icons LAST: only for the visible page (≤ 20 results). The old code
        // ran SHGetFileInfoW for every match on every keystroke — that was
        // the lag. Repeated searches hit the icon cache.
        for r in results.iter_mut() {
            if r.id.starts_with("sys:") {
                continue;
            }
            r.icon_data_url = crate::win32::icon::extract_icon_for_path(&r.path);
        }

        return results;
    }

    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Depth-limited walk collecting (name, path, is_folder) matches. Icons are
/// resolved later by the caller — this function touches the filesystem only.
#[cfg(windows)]
fn walk_programs(
    dir: &std::path::Path,
    q: &str,
    results: &mut Vec<(String, String, bool)>,
    include_dirs: bool,
) {
    const MAX_DEPTH: usize = 6;
    walk_programs_inner(dir, q, results, include_dirs, 0, MAX_DEPTH);
}

#[cfg(windows)]
fn walk_programs_inner(
    dir: &std::path::Path,
    q: &str,
    results: &mut Vec<(String, String, bool)>,
    include_dirs: bool,
    depth: usize,
    max_depth: usize,
) {
    if depth > max_depth {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_dir = path.is_dir();
        if is_dir {
            // Always recurse so nested shortcuts are still found.
            walk_programs_inner(&path, q, results, include_dirs, depth + 1, max_depth);
            // Directory entries themselves are only returned for desktop
            // scans, where a folder must stay launchable even when an exe
            // with the same display name exists next to it.
            if !include_dirs {
                continue;
            }
        } else {
            // Only .lnk shortcuts and .exe files
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            if ext != "lnk" && ext != "exe" {
                continue;
            }
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        let name_lower = name.to_lowercase();
        if !name_lower.contains(q) {
            continue;
        }
        // Deduplicate by full PATH, not by display name. Deduping by name
        // made the first same-named match win, so a desktop folder could be
        // shadowed by (or resolve to) a completely different .exe/.lnk that
        // merely shares the file stem.
        let key = path.to_string_lossy().to_lowercase();
        if results.iter().any(|(_, p, _)| p.to_lowercase() == key) {
            continue;
        }
        results.push((name, path.to_string_lossy().to_string(), is_dir));
    }
}

// ===== First-run tutorial =====
fn show_tutorial(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("tutorial") {
        win32::window::show_window(&win);
        let _ = win.set_focus();
        let _ = app.emit("tutorial://start", ());
    }
}

/// Mark the tutorial acknowledged and close the tutorial window.
#[tauri::command]
fn finish_tutorial(app: tauri::AppHandle) {
    persist::save_tutorial_seen(true);
    if let Some(win) = app.get_webview_window("tutorial") {
        win32::window::hide_window(&win);
    }
}

// ===== Reset config (settings-table command + -rs flag) =====
fn wipe_configs() {
    let data_dir = persist::data_dir();

    let files = [
        "clipboard.json",
        "notes.txt",
        "notes.json",
        "widgets.json",
        "widget_visibility.json",
        "desktop_pins.json",
        "icon_recolor.json",
        "settings.json",
        "themes.json",
        "tables.json",
        "tutorial_seen.json",
    ];

    for file in &files {
        let path = data_dir.join(file);
        if path.exists() {
            match std::fs::remove_file(&path) {
                Ok(_) => log::info!("Deleted {}", file),
                Err(e) => log::warn!("Failed to delete {}: {}", file, e),
            }
        }
    }
}

// Command version: wipe + restart fresh.
#[tauri::command]
fn reset_config(app: tauri::AppHandle) {
    wipe_configs();
    let _ = app.restart();
}

// ===== Helpers =====

/// Debounced persistence of movable-table window positions. WindowEvent::Moved
/// fires for every pixel of a drag — writing tables.json (and doing the
/// read-modify-write) for each event would be wasteful, so events are parked
/// in a map and a single background thread flushes the latest position per
/// table every 400 ms.
mod moved_save {
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::Once;

    static PARKED: Mutex<Option<HashMap<String, (i32, i32)>>> = Mutex::new(None);
    static START: Once = Once::new();

    pub fn schedule(name: String, x: i32, y: i32) {
        {
            let mut parked = PARKED.lock();
            parked
                .get_or_insert_with(HashMap::new)
                .insert(name, (x, y));
        }
        START.call_once(|| {
            std::thread::Builder::new()
                .name("table-pos-save".into())
                .spawn(|| loop {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    let pending = PARKED.lock().take();
                    if let Some(map) = pending {
                        let mut all = crate::persist::load_table_positions();
                        for (name, pos) in map {
                            all.insert(name, pos);
                        }
                        crate::persist::save_table_positions(&all);
                    }
                })
                .ok();
        });
    }
}

#[cfg(windows)]
fn refresh_taskbar_apps(handle: &tauri::AppHandle) {
    match win32::apps::scan_taskbar() {
        Ok(apps) => {
            let state = handle.state::<Arc<Mutex<AppState>>>();
            let mut s = state.lock();

            let mut new_order: Vec<String> = Vec::new();
            for id in s.app_order.iter() {
                if apps.iter().any(|a| &a.id == id) {
                    new_order.push(id.clone());
                }
            }
            for a in apps.iter() {
                if !new_order.contains(&a.id) {
                    new_order.push(a.id.clone());
                }
            }
            s.app_order = new_order.clone();

            let mut sorted_apps = apps.clone();
            sorted_apps.sort_by_key(|a| {
                new_order.iter().position(|id| id == &a.id).unwrap_or(usize::MAX)
            });

            // 0.3.0: emit ONLY when the set actually changed. The old code
            // re-emitted identical payloads every 2.5 s, making the strip
            // webview diff/patch (and the IPC layer) busy for nothing.
            if s.taskbar_apps == sorted_apps {
                return;
            }
            s.taskbar_apps = sorted_apps.clone();
            drop(s);
            let _ = handle.emit("taskbar://apps-updated", sorted_apps);
        }
        Err(e) => log::error!("scan_taskbar failed: {e}"),
    }
}

#[cfg(windows)]
fn refresh_desktop_items(handle: &tauri::AppHandle) {
    match win32::shell::scan_desktop() {
        Ok(items) => {
            let state = handle.state::<Arc<Mutex<AppState>>>();
            let mut s = state.lock();
            // Emit ONLY on real changes (the desktop watcher calls this for
            // every filesystem event burst; identical snapshots must not
            // trigger desktop-table re-renders).
            if s.desktop_items == items {
                return;
            }
            s.desktop_items = items.clone();
            drop(s);
            let _ = handle.emit("launcher://items-updated", items);
        }
        Err(e) => log::error!("scan_desktop failed: {e}"),
    }
}

#[cfg(not(windows))]
fn refresh_taskbar_apps(_: &tauri::AppHandle) {}
#[cfg(not(windows))]
fn refresh_desktop_items(_: &tauri::AppHandle) {}
