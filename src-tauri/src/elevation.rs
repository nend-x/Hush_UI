// UAC elevation helpers for setup.
//
// WHY: the win-key block (low-level keyboard hook installed by
// `prevent-alt-win-menu` + `win32::hotkey`) cannot intercept keystrokes
// destined for elevated apps (UIPI — User Interface Privilege Isolation).
// A non-elevated Hush_UI can hook the Win key when the focused window is
// also non-elevated, but the moment an elevated app (Task Manager, an
// installer, anything Run as administrator) has focus, the hook is
// bypassed and the native Start menu opens on a Win tap.
//
// The fix is to run Hush_UI itself elevated. The setup flow calls
// `request_elevation()` once. Three outcomes:
//
//   - `AlreadyElevated` — the process is already admin; no prompt was
//     shown. The caller continues normally.
//   - `Accepted` — the user accepted the UAC prompt. A new elevated
//     Hush_UI process has been launched with the same args; the caller
//     should exit so the elevated process can take over.
//   - `Declined` — the user declined the UAC prompt (or ShellExecuteW
//     returned an error). The caller continues with basic rights; the
//     win-key block will still work against non-elevated windows.
//
// The check + relaunch is Windows-only. On other targets `is_elevated`
// returns false and `request_elevation` returns `Declined` (the setup
// step still emits its status string, so the UI flow is identical).

#[cfg(windows)]
use std::ffi::OsStr;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Foundation::CloseHandle;
#[cfg(windows)]
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
#[cfg(windows)]
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
#[cfg(windows)]
use windows::Win32::UI::Shell::ShellExecuteW;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

pub enum ElevationOutcome {
    /// The process is already elevated — no prompt was shown.
    AlreadyElevated,
    /// The user accepted the UAC prompt. A new elevated process has been
    /// launched; the caller should exit immediately so the new process
    /// can take over.
    Accepted,
    /// The user declined the UAC prompt (or an error occurred). The
    /// caller should continue with basic rights.
    Declined,
}

/// Static snapshot of the launch-time elevation state — read by the
/// `get_elevation_state` command so the brightness widget can surface the
/// "dim may not cover elevated apps" warning. (0.3.0: this command was
/// invoked by the widgets table but never registered in the backend —
/// the warning could never appear.)
#[cfg(windows)]
pub static UAC_DECLINED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(not(windows))]
pub static UAC_DECLINED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(windows)]
pub fn is_elevated() -> bool {
    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut ret_len = 0u32;
        let res = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        );
        let _ = CloseHandle(token);
        res.is_ok() && elevation.TokenIsElevated != 0
    }
}

#[cfg(windows)]
pub fn request_elevation() -> ElevationOutcome {
    if is_elevated() {
        return ElevationOutcome::AlreadyElevated;
    }

    // Build the command line for the elevated relaunch — same exe, same args.
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            log::error!("request_elevation: current_exe() failed: {e}");
            return ElevationOutcome::Declined;
        }
    };
    let wide_path: Vec<u16> = OsStr::new(&exe)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let verb: Vec<u16> = OsStr::new("runas")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // Re-pass the same arguments (excluding argv[0]) so flags like -rs survive.
    let params: String = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let wide_params: Vec<u16> = OsStr::new(&params)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(wide_path.as_ptr()),
            if params.is_empty() {
                PCWSTR::null()
            } else {
                PCWSTR(wide_params.as_ptr())
            },
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    // ShellExecuteW returns an HINSTANCE; values > 32 mean success.
    // Anything ≤ 32 is an error code (e.g. ERROR_CANCELLED = 1223 when the
    // user clicks No on the UAC prompt).
    if result.0 as usize > 32 {
        log::info!("request_elevation: user accepted, elevated relaunch in flight");
        ElevationOutcome::Accepted
    } else {
        log::warn!(
            "request_elevation: declined or failed (ShellExecuteW code {})",
            result.0 as usize
        );
        ElevationOutcome::Declined
    }
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
}

#[cfg(not(windows))]
pub fn request_elevation() -> ElevationOutcome {
    ElevationOutcome::Declined
}
