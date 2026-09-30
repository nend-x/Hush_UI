// HideTaskbar — in-process native Windows taskbar hider.
//
// Ported from https://github.com/sinjs/HideTaskbar (HideTaskbar.cpp).
// Replaces the old embedded HideTaskbar.exe helper — no more standalone
// exe, no more extraction to %LOCALAPPDATA%, no more child process.
//
// How it works (same as the original C++):
//   1. Enumerate all top-level windows.
//   2. Find windows whose class name is "Shell_TrayWnd" (primary taskbar)
//      or "Shell_SecondaryTrayWnd" (secondary taskbars on multi-monitor).
//   3. Add the WS_EX_LAYERED extended style to each.
//   4. Set the layered window alpha to 0 (hide) or 255 (show).
//   5. Redraw the window so the change takes effect immediately.
//
// A background thread calls set_taskbars_hidden(true) every second to keep
// the taskbar hidden — Windows occasionally re-shows it (e.g. when the
// shell restarts, when a fullscreen app exits, when the user presses the
// Win key and the Start menu tries to show the taskbar). The 1s poll is
// the same interval the original C++ tool used.

#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(windows)]
use windows::core::BOOL;
#[cfg(windows)]
use windows::Win32::Foundation::{HWND, LPARAM};
#[cfg(windows)]
use windows::Win32::Graphics::Gdi::{
    RedrawWindow, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowLongW, SetLayeredWindowAttributes, SetWindowLongW,
    GWL_EXSTYLE, LWA_ALPHA, WS_EX_LAYERED,
};

#[cfg(windows)]
static RUNNING: AtomicBool = AtomicBool::new(false);

/// HWNDs that are currently known-hidden (alpha already 0). Re-asserting
/// alpha + RedrawWindow on explorer's windows every second used to cost
/// real CPU (the redraw walks the tray's frame); a steady-state tick now
/// only ENUMERATES — style/alpha are touched only for new or drifted
/// windows (shell restart, freshly re-created tray).
#[cfg(windows)]
static HIDDEN_HWNS: std::sync::Mutex<Option<std::collections::HashSet<isize>>> =
    std::sync::Mutex::new(None);


/// Start the background thread that keeps the taskbar hidden.
/// Safe to call once at app startup. Calling again is a no-op.
#[cfg(windows)]
pub fn start() {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return; // already running
    }
    std::thread::Builder::new()
        .name("hide-taskbar".into())
        .spawn(|| {
            log::info!("HideTaskbar monitor started");
            loop {
                if !RUNNING.load(Ordering::SeqCst) {
                    break;
                }
                // Check RUNNING again right before hiding — stop() might
                // have been called between the check above and this line.
                if RUNNING.load(Ordering::SeqCst) {
                    set_taskbars_hidden(true);
                }
                // Sleep in short increments so stop() is responsive
                for _ in 0..10 {
                    if !RUNNING.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
            log::info!("HideTaskbar monitor stopped");
        })
        .ok();
}

/// Stop the background thread and restore the taskbar.
/// Waits briefly for the thread to exit, then sets alpha to 255.
#[cfg(windows)]
pub fn stop() {
    RUNNING.store(false, Ordering::SeqCst);
    // Wait a moment for the thread to notice RUNNING=false and exit.
    // The thread checks every 100ms, so 200ms is enough.
    std::thread::sleep(std::time::Duration::from_millis(200));
    // Now safely show the taskbar — no race with the background thread.
    set_taskbars_hidden(false);
    log::info!("HideTaskbar stopped and taskbar restored");
}

#[cfg(windows)]
fn set_taskbars_hidden(hidden: bool) {
    let alpha: u8 = if hidden { 0 } else { 255 };
    let found = find_taskbar_windows();
    if hidden {
        // Only touch windows that are new or have slipped out of the hidden
        // state — a steady-state tick costs one enumeration, nothing more.
        let mut guard = HIDDEN_HWNS.lock().ok();
        let known = guard.get_or_insert_with(Default::default);
        let mut still: std::collections::HashSet<isize> = std::collections::HashSet::new();
        for hwnd in found {
            let key = hwnd.0 as isize;
            still.insert(key);
            if known.contains(&key) {
                // Fast path: already layered-hidden. Double-check alpha
                // cheaply — if something else reset it, fall through.
                let mut cur_alpha: u8 = 255;
                let mut cur_flag: u32 = 0;
                unsafe {
                    if windows::Win32::UI::WindowsAndMessaging::GetLayeredWindowAttributes(
                        hwnd,
                        None,
                        Some(&mut cur_alpha),
                        Some(&mut cur_flag),
                    )
                    .is_ok()
                        && cur_alpha == 0
                        && (cur_flag & LWA_ALPHA.0 != 0)
                    {
                        continue;
                    }
                }
                // Reset — drop from the known set and re-apply below.
                known.remove(&key);
            }
            set_window_alpha(hwnd, alpha);
            known.insert(key);
        }
        // Windows that disappeared — forget them.
        known.retain(|k| still.contains(k));
    } else {
        for hwnd in found {
            set_window_alpha(hwnd, alpha);
        }
        if let Ok(mut guard) = HIDDEN_HWNS.lock() {
            guard.take();
        }
    }
}

#[cfg(windows)]
fn find_taskbar_windows() -> Vec<HWND> {
    // Single enumeration pass for BOTH tray classes. The old code ran
    // EnumWindows twice per tick (once per class) — every tick, forever.
    find_windows_by_classes(&["Shell_TrayWnd", "Shell_SecondaryTrayWnd"])
}

// Thread-local storage for the current enumeration results.
#[cfg(windows)]
thread_local! {
    static CURRENT_RESULTS: std::cell::RefCell<Vec<HWND>> = std::cell::RefCell::new(Vec::new());
    static CURRENT_CLASSES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(windows)]
unsafe extern "system" fn enum_proc(hwnd: HWND, _lparam: LPARAM) -> BOOL {
    let mut class_buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut class_buf) };
    if len > 0 {
        // Wide compare without allocating a String per window.
        let found = &class_buf[..len as usize];
        CURRENT_CLASSES.with(|cc| {
            let matches = cc.borrow().iter().any(|c| {
                let wide: Vec<u16> = c.encode_utf16().collect();
                wide.as_slice() == found
            });
            if matches {
                CURRENT_RESULTS.with(|cr| {
                    cr.borrow_mut().push(hwnd);
                });
            }
        });
    }
    BOOL(1)
}

#[cfg(windows)]
#[allow(dead_code)]
fn find_windows_by_class(class_name: &str) -> Vec<HWND> {
    find_windows_by_classes(&[class_name])
}

#[cfg(windows)]
fn find_windows_by_classes(class_names: &[&str]) -> Vec<HWND> {
    CURRENT_CLASSES.with(|cc| {
        *cc.borrow_mut() = class_names.iter().map(|s| s.to_string()).collect();
    });
    CURRENT_RESULTS.with(|cr| {
        cr.borrow_mut().clear();
    });

    // EnumWindows takes Option<unsafe extern "system" fn(HWND, LPARAM) -> BOOL>
    let _ = unsafe { EnumWindows(Some(enum_proc), LPARAM(0)) };

    CURRENT_RESULTS.with(|cr| cr.borrow().clone())
}

#[cfg(windows)]
fn set_window_alpha(hwnd: HWND, alpha: u8) {
    unsafe {
        // Add WS_EX_LAYERED if not already present
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex_style & WS_EX_LAYERED.0 == 0 {
            SetWindowLongW(hwnd, GWL_EXSTYLE, (ex_style | WS_EX_LAYERED.0) as i32);
        }
        // Set alpha
        let _ = SetLayeredWindowAttributes(hwnd, windows::Win32::Foundation::COLORREF(0), alpha, LWA_ALPHA);
        // Force a redraw of the taskbar window itself.
        //
        // Previously this passed RDW_ALLCHILDREN, which sends WM_PAINT /
        // WM_ERASEBKGND synchronously to every child window of Shell_TrayWnd
        // (Start button, tray, clock, etc.). Those child windows live on
        // explorer.exe's thread. When explorer is in a bad post-Modern-Standby
        // state (the same condition that hangs SHGetFileInfoW), this call
        // blocks the hide_taskbar background thread indefinitely — which in
        // turn backs up the SetLayeredWindowAttributes cadence and lets the
        // taskbar re-appear visibly during a freeze.
        //
        // RDW_INVALIDATE | RDW_FRAME without RDW_ALLCHILDREN invalidates
        // only the taskbar window's own frame, which is enough for the
        // layered-alpha change to take effect visually (the DWM re-composites
        // based on the layered attributes regardless of child paint state).
        let _ = RedrawWindow(
            Some(hwnd),
            None,
            None,
            RDW_ERASE | RDW_INVALIDATE | RDW_FRAME,
        );
    }
}

#[cfg(not(windows))]
pub fn start() {}
#[cfg(not(windows))]
pub fn stop() {}
