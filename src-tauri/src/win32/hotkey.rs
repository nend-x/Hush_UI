// Win-key tap-vs-combo-vs-hold interception.
//
// Behavior (Hush_UI tables update):
//   - Win TAP (down + up alone, quick)          -> toggle the launcher
//   - Win HOLD (held for HOLD_MS)               -> pie table picker appears
//       - around the mouse cursor, or centered on the screen when Ctrl is
//         also down (Ctrl+Win)
//       - hovering a picker button and RELEASING Win opens that table
//   - Win held with ANY other key (Win+D, Win+E, Win+Tab, ...) -> the combo
//     works exactly like on stock Windows (and dismisses the picker if it
//     was already showing)
//
// How it works:
//   A low-level keyboard hook (WH_KEYBOARD_LL) SWALLOWS the native Win-down
//   and remembers it is "pending". The moment any other key is pressed while
//   Win is pending, the tap AND the hold are cancelled, an open picker is
//   dismissed, and the suppressed Win-down is re-injected (marked injected so
//   this hook ignores it) — from then on the rest of the combo flows through
//   Windows untouched. On Win-up the pending flag decides: tables open →
//   release handler (open hovered table); still pending → it was a tap →
//   toggle handler.
//
//   The HOLD detection does NOT run on the hook thread: Win-down spawns a
//   one-shot detector thread that sleeps HOLD_MS, then (only if Win is still
//   pending and no other key arrived) flips TABLES_OPEN and fires on_hold.
//   The hook callback itself never blocks — SendInput-free fast path.
//
// Swallowing Win-down (vs the `prevent-alt-win-menu` crate's approach of
// injecting a dummy key-up AFTER Win-up) is what makes the Start menu
// literally unable to open — the OS shell never sees Win-down, so it can't
// start its "Win chord" detection. The dummy-key-up approach is racy under
// focus changes (when the launcher webview has focus, the shell processed
// Win-up before the synthetic dummy-up landed, so the Start menu opened and
// the close tap appeared to do nothing). This hook fixes both symptoms.
//
// We install this hook LAST so it is called FIRST in the LIFO hook chain —
// it swallows Win events before `prevent-alt-win-menu`'s hook (which is
// installed first) ever sees them. `prevent-alt-win-menu` is configured to
// return None for Win in its on_released callback (we handle Win here), and
// only handles Alt (menu bar suppression on Alt release).
//
// The hook runs on its own thread with a standard message pump (required for
// low-level hooks). All handler callbacks are invoked from short-lived
// worker threads so the hook never blocks input processing.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use once_cell::sync::OnceCell;
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_LMENU, VK_LWIN, VK_MENU, VK_RMENU, VK_RWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP, UnhookWindowsHookEx,
};

/// Win-key behavior for the Hush_UI tables update.
///
/// Three user-visible gestures share one swallowed Win-down:
///   - TAP    (Win down → up with no other key in between, released before
///             HOLD_MS elapses)                 → `on_tap`    (toggle launcher)
///   - HOLD   (Win held for >= HOLD_MS)        → `on_hold(ctrl)` — shows the
///             pie table picker, around the cursor, or centered on the
///             screen when Ctrl is also held (Ctrl+Win)
///   - RELEASE while the picker is open        → `on_tables_release` — the
///             backend opens whichever table button is under the cursor
///             (hover state is tracked backend-side via set_tables_hover)
///
/// Combos are untouched: any other key while Win is held cancels the tap AND
/// the pending hold, hides an already-open picker, and re-injects the
/// swallowed Win-down so Win+D / Win+E / Win+Ctrl+… resolve natively.
pub struct HotkeyHandlers {
    pub on_tap: Box<dyn Fn() + Send + Sync>,
    /// `ctrl` = show the picker centered on the screen instead of at the cursor.
    pub on_hold: Box<dyn Fn(bool) + Send + Sync>,
    pub on_tables_release: Box<dyn Fn() + Send + Sync>,
    pub on_tables_cancel: Box<dyn Fn() + Send + Sync>,
}

type Handlers = OnceCell<HotkeyHandlers>;

static HANDLERS: Handlers = Handlers::new();
static WIN_PENDING: AtomicBool = AtomicBool::new(false);
static TABLES_OPEN: AtomicBool = AtomicBool::new(false);

/// Magic dwExtraInfo stamped onto every key event we inject ourselves.
/// Low-level hooks (ours AND other apps') can read this to tell synthetic
/// input apart — we rely on LLKHF_INJECTED for the ignore decision, but the
/// marker makes debugging input traces unambiguous.
pub const OWN_INJECT_MARKER: usize = 0x_F1A7_0001;

/// How long Win must be held before the table picker appears. Mirrors
/// `settings.tables_hold_ms` — updated by lib.rs on startup and whenever
/// settings are saved. Kept here so the hold-detector never touches the
/// config file from the hook thread.
pub static HOLD_MS: AtomicU64 = AtomicU64::new(220);

// Alt menu suppression state — replaces the prevent-alt-win-menu crate.
// ALT_PENDING is set on Alt-down and cleared when any OTHER key is pressed
// (meaning it was an Alt+X combo, not a standalone Alt tap). On Alt-up, if
// ALT_PENDING is still true, we inject a dummy VK__none_ key-up which
// prevents the focused window's menu bar from activating.
static ALT_PENDING: AtomicBool = AtomicBool::new(false);

/// Tracks whether the hook thread is alive. The health-check timer
/// monitors this — if the thread dies (e.g. Windows removed the hook
/// after a LowLevelHooksTimeout, or the thread panicked), the timer
/// re-installs the hook on a fresh thread.
static HOOK_ALIVE: AtomicBool = AtomicBool::new(false);

/// Install the hook. `handlers` receives the three Win gestures (tap, hold,
/// release-while-picker-open) plus combo dismissal. Safe to call once at app
/// startup. Also starts a background health-check timer that re-installs the
/// hook if it ever dies (Windows can silently remove low-level hooks if the
/// callback takes too long, if the hook thread's message pump stalls, or if
/// an AV interferes).
pub fn install(handlers: HotkeyHandlers) {
    if HANDLERS.set(handlers).is_err() {
        log::warn!("win hotkey hook already installed");
        return;
    }
    spawn_hook_thread();

    // Health-check timer: every 5s, check if the hook thread is alive.
    // If not, re-install the hook on a fresh thread. This makes the hook
    // self-healing — if Windows removes it for any reason (timeout, AV
    // interference, thread panic), it comes back within 5 seconds.
    std::thread::Builder::new()
        .name("win-key-hook-health".into())
        .spawn(|| {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(5));
                if !HOOK_ALIVE.load(Ordering::SeqCst) {
                    log::warn!("Win key hook thread died — re-installing");
                    spawn_hook_thread();
                }
            }
        })
        .ok();
}

fn spawn_hook_thread() {
    std::thread::Builder::new()
        .name("win-key-hook".into())
        .spawn(|| unsafe {
            HOOK_ALIVE.store(true, Ordering::SeqCst);

            // Per MSDN, WH_KEYBOARD_LL's hMod can be NULL because the hook is
            // not injected into another process — but using the EXE's HMODULE
            // (via GetModuleHandleW(NULL)) is the more robust form that
            // matches what `prevent-alt-win-menu` does and survives more
            // edge cases (e.g. some AV software that validates hMod).
            let hmod = match GetModuleHandleW(None) {
                Ok(h) => Some(HINSTANCE::from(h)),
                Err(_) => None,
            };
            let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), hmod, 0) {
                Ok(h) => h,
                Err(e) => {
                    log::error!("SetWindowsHookExW(WH_KEYBOARD_LL) failed: {e}");
                    HOOK_ALIVE.store(false, Ordering::SeqCst);
                    return;
                }
            };
            log::info!("Win key hook installed (tap -> launcher, hold -> tables, combos -> native)");
            // CRITICAL: the message loop MUST call TranslateMessage + DispatchMessageW.
            //
            // WH_KEYBOARD_LL hooks are delivered via SendMessage to the thread
            // that installed the hook. The hook callback fires during message
            // dispatch — without DispatchMessageW, the hook message is
            // retrieved by GetMessageW but never dispatched to the hook
            // procedure. This causes the hook to silently stop firing under
            // message traffic (e.g. when a WebView2 window in the same
            // process takes focus), which is exactly the bug where the Start
            // menu opens when tapping Win to close the launcher.
            //
            // This matches the standard Microsoft pattern and what the
            // prevent-alt-win-menu crate does.
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = UnhookWindowsHookEx(hook);

            // If we reach here, GetMessageW returned 0 (WM_QUIT) or -1 (error).
            // Mark the hook as dead so the health-check timer re-installs it.
            HOOK_ALIVE.store(false, Ordering::SeqCst);
            log::warn!("Win key hook thread exiting — will be re-installed by health check");
        })
        .ok();
}

unsafe extern "system" fn ll_keyboard_proc(
    n_code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    const SUPPRESS: LRESULT = LRESULT(1);

    if n_code == HC_ACTION as i32 {
        let kb = &*(l_param.0 as *const KBDLLHOOKSTRUCT);
        let injected = (kb.flags & LLKHF_INJECTED).0 != 0;
        let is_down = w_param.0 as u32 == WM_KEYDOWN || w_param.0 as u32 == WM_SYSKEYDOWN;
        let is_up = w_param.0 as u32 == WM_KEYUP || w_param.0 as u32 == WM_SYSKEYUP;
        let vk = VIRTUAL_KEY(kb.vkCode as u16);

        // Never touch synthetic input (incl. our own re-injected Win down
        // or the VK__none_ dummy key-up for Alt suppression).
        if !injected {
            if vk == VK_LWIN || vk == VK_RWIN {
                if is_down {
                    // Begin a potential tap AND a potential hold; swallow the
                    // native down for now so the OS shell can't start its
                    // "Win chord" detection (which is what opens the Start
                    // menu). The hold detector below decides if this becomes
                    // the pie table picker.
                    WIN_PENDING.store(true, Ordering::SeqCst);
                    spawn_hold_detector();
                    // Arm the Start-menu killer: only now (within a couple of
                    // seconds of a real Win press) does the Start menu race
                    // exist, and only now does it spend full process-snapshot
                    // polls. Idle cost outside this window is near zero.
                    crate::start_menu_killer::arm();
                    return SUPPRESS;
                }
                if is_up {
                    if TABLES_OPEN.swap(false, Ordering::SeqCst) {
                        // The picker is open: this release means "open the
                        // table under the cursor" (backend reads the hover
                        // state) or "dismiss" when nothing is hovered.
                        WIN_PENDING.store(false, Ordering::SeqCst);
                        if let Some(h) = HANDLERS.get() {
                            let f = &h.on_tables_release;
                            std::thread::spawn(f);
                        }
                        // Swallow the Win-up so the OS shell never sees a
                        // standalone Win release (Start-menu trigger).
                        return SUPPRESS;
                    }
                    if WIN_PENDING.swap(false, Ordering::SeqCst) {
                        // Tap: Win went down and up with no other key in
                        // between and before the hold threshold. Fire the
                        // toggle on a worker thread so the hook never blocks
                        // input processing, and swallow this Win-up so the
                        // OS shell definitely doesn't see a "Win release
                        // alone" event (which would otherwise re-trigger
                        // the Start menu).
                        if let Some(h) = HANDLERS.get() {
                            let f = &h.on_tap;
                            std::thread::spawn(f);
                        }
                        return SUPPRESS;
                    }
                    // Not pending: the down was re-injected for a combo —
                    // let the matching up through so modifier state stays
                    // sane for the OS shell.
                    return CallNextHookEx(None, n_code, w_param, l_param);
                }
            } else if vk == VK_LMENU || vk == VK_RMENU || vk == VK_MENU {
                // Alt menu suppression (replaces prevent-alt-win-menu crate).
                // Track Alt state: on Alt-down, mark pending. On Alt-up, if
                // still pending (no other key was pressed while Alt was held),
                // inject a dummy VK__none_ key-up to suppress the menu bar.
                if is_down {
                    ALT_PENDING.store(true, Ordering::SeqCst);
                } else if is_up {
                    if ALT_PENDING.swap(false, Ordering::SeqCst) {
                        // Standalone Alt tap — inject dummy key-up to
                        // prevent the focused window's menu bar from
                        // activating. This runs on the hook thread, but
                        // SendInput is fast (<1ms) so it won't trigger
                        // LowLevelHooksTimeout.
                        inject_dummy_keyup();
                    }
                }
                // Let Alt through normally — we don't suppress it, we just
                // add a dummy key-up after the release.
            } else if is_down && WIN_PENDING.load(Ordering::SeqCst) {
                // Combo detected (Win+D, Win+E, ...): cancel the tap AND the
                // pending hold, dismiss the picker if it already appeared,
                // and put the Win modifier back so Windows sees the real
                // shortcut.
                WIN_PENDING.store(false, Ordering::SeqCst);
                if TABLES_OPEN.swap(false, Ordering::SeqCst) {
                    if let Some(h) = HANDLERS.get() {
                        let f = &h.on_tables_cancel;
                        std::thread::spawn(f);
                    }
                }
                re_inject_win_down();
            } else if is_down {
                // Any non-Win, non-Alt key pressed — cancel any pending
                // Alt tap (it was an Alt+X combo, not a standalone Alt).
                ALT_PENDING.store(false, Ordering::SeqCst);
            }
        }
    }

    CallNextHookEx(None, n_code, w_param, l_param)
}

/// One-shot hold detector, spawned on every non-injected Win-down.
///
/// Sleeps HOLD_MS; if Win is STILL pending (no other key arrived, not yet
/// released, no combo re-inject) it turns the pending tap into the radial
/// table picker. Ctrl is sampled at the moment the picker appears — holding
/// Ctrl first or pressing it during the hold both land in center mode.
///
/// The hook thread itself never sleeps: a detector thread per Win-down is
/// cheap (lives for HOLD_MS at most, ~220 ms) and keeps input latency at zero.
fn spawn_hold_detector() {
    let hold_ms = HOLD_MS.load(Ordering::SeqCst).clamp(80, 1000);
    std::thread::Builder::new()
        .name("win-hold-detector".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(hold_ms));
            if !WIN_PENDING.load(Ordering::SeqCst) || TABLES_OPEN.load(Ordering::SeqCst) {
                return; // released early, or a combo re-injected the Win-down
            }
            TABLES_OPEN.store(true, Ordering::SeqCst);
            let ctrl = unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) < 0 };
            if let Some(h) = HANDLERS.get() {
                let f = &h.on_hold;
                std::thread::spawn(move || f(ctrl));
            }
        })
        .ok();
}

/// Re-send the Win-down we swallowed so an in-flight combo resolves natively.
/// Marked INJECTED: our own hook skips it, everything else sees a real Win.
fn re_inject_win_down() {
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_LWIN,
                wScan: 0,
                dwFlags: KEYEVENTF_EXTENDEDKEY,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        let sent = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        if sent != 1 {
            log::warn!("re_inject_win_down: SendInput sent {sent}");
        }
    }
}

/// Inject a synthetic Win KEY-UP (marked injected).
///
/// Why: the pie picker's Win-down is swallowed and its Win-up is swallowed
/// too (the OS shell must never see a standalone Win release). But some
/// Windows internals track modifier latched state from OUR SendInput'd
/// combos, so after a picker release the OS can still believe Win is down
/// (Start menu pops, Win-chord state sticks). Sending an injected Win-up
/// right after the picker closes clears that latch.
///
/// Our own hook ignores injected input unconditionally (the `!injected`
/// guard in ll_keyboard_proc), so this synthetic up can never re-trigger
/// tap/hold/release logic — and since the shell never saw a Win-down, a
/// lone injected up can't open the Start menu either.
pub fn inject_win_keyup() {
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_LWIN,
                wScan: 0,
                dwFlags: KEYEVENTF_KEYUP | KEYEVENTF_EXTENDEDKEY,
                time: 0,
                dwExtraInfo: OWN_INJECT_MARKER,
            },
        },
    };
    unsafe {
        let sent = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        if sent != 1 {
            log::warn!("inject_win_keyup: SendInput sent {sent}");
        }
    }
}

/// The pie picker was closed WITHOUT a Win release (frontend click-dismiss,
/// Esc, or any non-hook funnel). Clears the picker/latch flags so a later
/// real Win-up degrades to a no-op instead of a release gesture, then sends
/// the synthetic Win-up (see inject_win_keyup) so the OS never believes Win
/// is stuck down.
///
/// NOT called on the combo-cancel path — there Win is legitimately held and
/// was just re-injected; a synthetic up would break the in-flight combo.
pub fn tables_closed_recover() {
    TABLES_OPEN.store(false, Ordering::SeqCst);
    WIN_PENDING.store(false, Ordering::SeqCst);
    inject_win_keyup();
}

/// Inject a dummy VK__none_ key-up to suppress the focused window's menu
/// bar activation on a standalone Alt release. This is the same technique
/// the `prevent-alt-win-menu` crate used, but done in our own hook callback
/// — no second hook thread, no second message pump.
fn inject_dummy_keyup() {
    // VK__none_ = 0xFF — a virtual key that no real keyboard produces.
    // Sending its key-up after Alt-up causes Windows to cancel the menu
    // activation that would otherwise fire on a standalone Alt release.
    let vk_none = VIRTUAL_KEY(0xFF);
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk_none,
                wScan: 0,
                dwFlags: KEYEVENTF_KEYUP,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        let sent = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        if sent != 1 {
            log::warn!("inject_dummy_keyup: SendInput sent {sent}");
        }
    }
}
