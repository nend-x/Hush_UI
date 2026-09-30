// Start menu killer — kill-on-show monitor for StartMenuExperienceHost.exe.
//
// WHY: the low-level keyboard hook (WH_KEYBOARD_LL) approach for blocking
// the Win key is racy. The hook callback is delivered via SendMessage to
// the hook thread, and under message traffic (especially when a WebView2
// window in the same process has focus), the hook can miss the Win-down
// event. The OS then launches StartMenuExperienceHost.exe (the Start menu
// process), which renders the Start menu.
//
// Instead of trying to prevent Win-down from reaching the OS, we take the
// opposite approach: let the OS do whatever it wants, but kill
// StartMenuExperienceHost.exe before it can render a single frame.
//
// HISTORY / CPU FIX (0.3.0): the original implementation polled
// CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS) every 100ms — a full
// process-table snapshot TEN times per second, forever, even when the user
// was not touching the keyboard. That snapshot enumerates every process on
// the system and was a major contributor to the ~30% idle CPU burn. The
// monitor is now two-mode:
//
//   ARMED (within ARMED_WINDOW_MS of a real Win-key press) — the old
//     behavior: 100ms snapshot polling, exactly when the Start menu race
//     can actually happen (the gesture window). This mode is short-lived.
//
//   IDLE (all other times) — no snapshots at all. A cheap 250ms
//     FindWindowExW walk looks for "Windows.UI.Core.CoreWindow" windows
//     (the UWP shell flyout class: Start menu, search host). Typically
//     zero such windows exist, so a tick is 1-3 syscalls. If one appears,
//     we open its owning process, verify the image name and terminate.
//     The Start menu cannot render without its window, so kill-on-window
//     is as effective as kill-on-spawn — without the constant snapshot
//     cost. (Windows pre-launches StartMenuExperienceHost in the
//     background; a pre-launched process with NO window is harmless and
//     left alone — killing it on sight, as before, only guaranteed a
//     spawn/kill war with the OS.)
//
// We also kill SearchHost.exe (the search flyout that can be triggered by
// Win+S). Window titles are locale-dependent, so detection is by process
// image name — locale-independent.

#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[cfg(windows)]
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
#[cfg(windows)]
use windows::Win32::System::Threading::{
    OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};

/// Whether the monitor should keep running. Set to false by stop().
#[cfg(windows)]
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Millis (unix epoch) until which the monitor stays ARMED. Updated by
/// arm() whenever the user actually presses the Win key.
#[cfg(windows)]
static ARMED_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

/// How long after a Win-key press the monitor keeps snapshot-polling.
#[cfg(windows)]
const ARMED_WINDOW_MS: u64 = 2500;

/// Idle-mode poll interval for the cheap window walk.
#[cfg(windows)]
const IDLE_POLL_MS: u64 = 250;

/// Armed-mode poll interval (the historical 100ms snapshot cadence).
#[cfg(windows)]
const ARMED_POLL_MS: u64 = 100;

/// The processes to kill on sight. These are the UWP shell surfaces that
/// the Win key can trigger.
#[cfg(windows)]
const KILL_TARGETS: &[&str] = &[
    "StartMenuExperienceHost.exe",
    "SearchHost.exe",
];

/// Class of the UWP shell flyout windows (Start menu, search host).
#[cfg(windows)]
const CORE_WINDOW_CLASS: windows::core::PCWSTR =
    windows::core::w!("Windows.UI.Core.CoreWindow");

/// Arm the monitor: for the next ARMED_WINDOW_MS the monitor polls with
/// full snapshots at 100ms (the historical behavior). Called on every real
/// (non-injected) Win-key down from the keyboard hook — the only moment the
/// Start menu race exists.
#[cfg(windows)]
pub fn arm() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    ARMED_UNTIL_MS.store(now + ARMED_WINDOW_MS, Ordering::SeqCst);
}

#[cfg(not(windows))]
pub fn arm() {}

/// Start the monitor on a background thread. Runs forever (until stop() is
/// called or the process exits). Safe to call once at app startup.
#[cfg(windows)]
pub fn start() {
    RUNNING.store(true, Ordering::SeqCst);
    std::thread::Builder::new()
        .name("start-menu-killer".into())
        .spawn(monitor_loop)
        .ok();
}

/// Stop the monitor. The background thread will exit on its next poll cycle.
#[cfg(windows)]
pub fn stop() {
    RUNNING.store(false, Ordering::SeqCst);
    log::info!("Start menu killer monitor stopped");
}

#[cfg(windows)]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(windows)]
fn monitor_loop() {
    log::info!(
        "Start menu killer monitor started (targets: {:?}, armed-poll {ARMED_POLL_MS}ms / idle-poll {IDLE_POLL_MS}ms)",
        KILL_TARGETS
    );
    loop {
        if !RUNNING.load(Ordering::SeqCst) {
            break;
        }
        let armed = now_ms() < ARMED_UNTIL_MS.load(Ordering::SeqCst);
        if armed {
            // Gesture window — historical full protection.
            kill_targets_snapshot();
            kill_visible_core_windows();
            std::thread::sleep(std::time::Duration::from_millis(ARMED_POLL_MS));
        } else {
            // Idle — near-zero cost: look for visible CoreWindows only.
            kill_visible_core_windows();
            std::thread::sleep(std::time::Duration::from_millis(IDLE_POLL_MS));
        }
    }
    log::info!("Start menu killer monitor exiting");
}

/// Kill the target processes if any of them owns a VISIBLE CoreWindow
/// right now. This is the idle-mode check: FindWindowExW chaining is
/// 1-3 syscalls when no flyout exists (the common case).
#[cfg(windows)]
fn kill_visible_core_windows() {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowExW, GetWindowThreadProcessId,
    };

    unsafe {
        // Walk the (usually empty) list of CoreWindows.
        let mut prev = HWND::default();
        loop {
            let hwnd = match FindWindowExW(None, Some(prev), CORE_WINDOW_CLASS, PCWSTR::null()) {
                Ok(h) => h,
                Err(_) => break,
            };
            if hwnd.is_invalid() {
                break;
            }
            prev = hwnd;

            // Only act on windows that are actually shown. IsWindowVisible
            // is one syscall and avoids opening processes for cached/
            // pre-created (hidden) CoreWindows.
            let visible = windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd);
            if !visible.as_bool() {
                continue;
            }

            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 {
                continue;
            }
            if let Some(image) = process_image_name(pid) {
                if KILL_TARGETS.iter().any(|t| image.eq_ignore_ascii_case(t)) {
                    kill_pid(pid, &image);
                }
            }
        }
    }
}

/// Resolve a pid to its full image name (no snapshot needed).
#[cfg(windows)]
fn process_image_name(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = windows::Win32::System::Threading::QueryFullProcessImageNameW(
            handle,
            windows::Win32::System::Threading::PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        if ok.is_err() {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        // We only need the file name component.
        full.rsplit(['\\', '/']).next().map(|s| s.to_string())
    }
}

#[cfg(windows)]
fn kill_pid(pid: u32, image: &str) {
    unsafe {
        if let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE, false, pid) {
            let _ = TerminateProcess(handle, 1);
            let _ = windows::Win32::Foundation::CloseHandle(handle);
            log::info!("Killed {} (pid: {})", image, pid);
        }
    }
}

/// Snapshot sweep — the historical behavior, now only used while ARMED
/// (i.e. shortly after a real Win-key press). Kills target processes even
/// before they create a window, which is the strongest protection.
#[cfg(windows)]
fn kill_targets_snapshot() {
    unsafe {
        let snapshot = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(s) => s,
            Err(e) => {
                log::error!("CreateToolhelp32Snapshot failed: {e}");
                return;
            }
        };

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        if Process32FirstW(snapshot, &mut entry).is_err() {
            let _ = windows::Win32::Foundation::CloseHandle(snapshot);
            return;
        }

        loop {
            let name = String::from_utf16_lossy(
                &entry.szExeFile[..entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(0)],
            );

            if KILL_TARGETS.iter().any(|t| name.eq_ignore_ascii_case(t)) {
                // Found a target — open it and terminate it.
                let pid = entry.th32ProcessID;
                kill_pid(pid, &name);
            }

            if Process32NextW(snapshot, &mut entry).is_err() {
                break;
            }
        }

        let _ = windows::Win32::Foundation::CloseHandle(snapshot);
    }
}

#[cfg(not(windows))]
pub fn start() {}

#[cfg(not(windows))]
pub fn stop() {}
