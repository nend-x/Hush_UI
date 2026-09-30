// Hush_UI crash handler
// ====================
//
// Installs TWO layers of crash detection so that the user always gets a
// visible, readable error dialog instead of the app silently disappearing:
//
//   1. `std::panic::set_hook` — catches Rust panics. With `panic = "abort"`
//      (the release profile), the hook runs just before the process aborts,
//      giving us a window to spawn the crash reporter.
//
//   2. `SetUnhandledExceptionFilter` — catches native Win32 exceptions that
//      bypass Rust's panic machinery: access violations, stack overflows,
//      illegal instructions, divide-by-zero, etc.
//
// When a crash is caught, the handler does NOT try to render a UI itself —
// the process state is suspect (the heap may be corrupted, the stack may be
// exhausted, locks may be held). Instead it:
//
//   a) Writes the crash info (type, message, stack trace, version,
//      timestamp) to a temp file: `%TEMP%\hush-crash-<ms>.txt`.
//   b) Spawns `<self_exe> --crash-report <tmpfile>` as a DETACHED child
//      process (`DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`).
//   c) Returns — the parent then dies, but the child is independent and
//      stays alive, showing a `MessageBoxW` with the crash info until the
//      user clicks OK.
//
// Why a separate process instead of `MessageBoxW` in-process?
//   - The crashing process may have a corrupted heap / exhausted stack /
//     held loader lock — calling `MessageBoxW` from there can deadlock or
//     silently fail.
//   - A detached child starts from a clean state and is guaranteed to be
//     able to allocate, lock, and pump a message loop.
//   - The child IS the same `hush_ui.exe` binary — when invoked with
//     `--crash-report <file>` it short-circuits in `main.rs` and never
//     starts Tauri. No second binary to ship.
//
// Re-entrancy guard: an `AtomicBool` ensures only the FIRST crash path
// (panic or exception, whichever hits first) spawns the reporter — a second
// panic while we're already writing the file would otherwise produce two
// overlapping dialogs.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

pub const CRASH_REPORT_FLAG: &str = "--crash-report";

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::os::windows::process::CommandExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use windows::core::PCWSTR;
    use windows::Win32::System::Diagnostics::Debug::{
        SetUnhandledExceptionFilter, EXCEPTION_POINTERS, RtlCaptureStackBackTrace,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TASKMODAL, MB_TOPMOST,
        MESSAGEBOX_STYLE,
    };

    /// `DETACHED_PROCESS` — child doesn't inherit our (non-existent) console.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    /// `CREATE_NEW_PROCESS_GROUP` — child is independent of parent's Ctrl-C group.
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    /// Re-entrancy guard — only the first crash path spawns the reporter.
    static CRASH_FIRED: AtomicBool = AtomicBool::new(false);

    /// `EXCEPTION_EXECUTE_HANDLER` — tells the OS to terminate the process
    /// after our filter returns. (We've already spawned the reporter.)
    const EXCEPTION_EXECUTE_HANDLER: i32 = 1;

    // ---------------------------------------------------------------------------
    // Public API
    // ---------------------------------------------------------------------------

    /// Install the crash handler. MUST be called at the very start of `run()`,
    /// before any code that might panic or trigger a native exception.
    pub fn install() {
        install_panic_hook();
        install_exception_filter();
    }

    /// Entry point for the crash-report subprocess.
    ///
    /// `path` is the temp file written by the crashing parent. We read it,
    /// delete it (best-effort), and display its contents in a `MessageBoxW`.
    /// Returns the process exit code.
    pub fn show_crash_dialog(path: &str) -> i32 {
        let info = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                // We can't read the crash file — show a minimal fallback box
                // so the user at least knows *something* happened.
                let msg = format!(
                    "Hush_UI crashed, but the crash report could not be read:\n  {path}\n  {e}"
                );
                message_box("Hush_UI Crashed", &msg);
                return 1;
            }
        };

        // Best-effort cleanup of the temp file.
        let _ = std::fs::remove_file(path);

        message_box("Hush_UI Crashed", &info);
        0
    }

    // ---------------------------------------------------------------------------
    // Installation
    // ---------------------------------------------------------------------------

    fn install_panic_hook() {
        std::panic::set_hook(Box::new(|info| {
            if CRASH_FIRED.swap(true, Ordering::SeqCst) {
                // Another crash path already fired — let the process die quietly
                // to avoid spawning duplicate dialogs.
                return;
            }

            // Extract the panic message: payload may be &str, String, or arbitrary.
            let payload = info.payload();
            let msg = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(|s| s.as_str()))
                .unwrap_or("<non-string panic payload>");

            let location = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "<unknown location>".to_string());

            // force_capture works even when RUST_BACKTRACE is unset.
            let backtrace = std::backtrace::Backtrace::force_capture().to_string();

            let detail = format!("Panic message: {msg}\nLocation:       {location}");
            let info_text = format_crash_info("Rust panic", &detail, Some(&backtrace));

            spawn_crash_reporter(&info_text);
        }));
    }

    fn install_exception_filter() {
        unsafe { SetUnhandledExceptionFilter(Some(exception_handler)) };
    }

    // ---------------------------------------------------------------------------
    // Native exception filter
    // ---------------------------------------------------------------------------

    unsafe extern "system" fn exception_handler(exc_info: *const EXCEPTION_POINTERS) -> i32 {
        if CRASH_FIRED.swap(true, Ordering::SeqCst) {
            return EXCEPTION_EXECUTE_HANDLER;
        }

        let detail = if !exc_info.is_null() {
            let record = (*exc_info).ExceptionRecord;
            if !record.is_null() {
                // NTSTATUS is a transparent newtype around i32 — read .0 for
                // the raw value. Convert via `as u32` because exception code
                // constants are conventionally printed as unsigned hex.
                let code = (*record).ExceptionCode.0 as u32;
                let addr = (*record).ExceptionAddress;
                let code_str = exception_code_to_string(code);
                format!(
                    "Exception code:    0x{code:08X}\nException name:    {code_str}\nFaulting address:  0x{addr:p}"
                )
            } else {
                "Exception record was null".to_string()
            }
        } else {
            "EXCEPTION_POINTERS was null (no debug info available)".to_string()
        };

        // Capture stack trace via RtlCaptureStackBackTrace — this works even
        // when the symbol loader is unavailable (we get raw addresses).
        const MAX_FRAMES: usize = 64;
        let mut frames: [*mut c_void; MAX_FRAMES] = [std::ptr::null_mut(); MAX_FRAMES];
        let n = unsafe {
            RtlCaptureStackBackTrace(0, &mut frames, None)
        };
        let trace: Vec<String> = (0..n as usize)
            .map(|i| format!("  #{i:02}: 0x{:016X}", frames[i] as usize))
            .collect();
        let backtrace = if trace.is_empty() {
            "<no stack frames captured>".to_string()
        } else {
            trace.join("\n")
        };

        let info_text = format_crash_info("Unhandled native exception", &detail, Some(&backtrace));
        spawn_crash_reporter(&info_text);

        EXCEPTION_EXECUTE_HANDLER
    }

    /// Translate a Win32 `NTSTATUS` exception code to a human-readable name.
    fn exception_code_to_string(code: u32) -> &'static str {
        match code {
            0xC0000005 => "ACCESS_VIOLATION (read/write/execute of invalid memory)",
            0xC000001D => "ILLEGAL_INSTRUCTION",
            0xC0000025 => "NONCONTINUABLE_EXCEPTION",
            0xC0000094 => "INT_DIVIDE_BY_ZERO",
            0xC0000096 => "PRIVILEGED_INSTRUCTION",
            0xC00000FD => "STACK_OVERFLOW",
            0xC0000409 => "STACK_BUFFER_OVERRUN (__fastfail / FAILFAST)",
            0xC000041D => "UNHANDLED_EXCEPTION_IN_CALLBACK",
            0xC0000420 => "ASSERTION_FAILURE",
            0xC0000374 => "HEAP_CORRUPTION",
            0xC00000E5 => "IN_PAGE_ERROR (paged out / disk failure)",
            0xC000001C => "INVALID_DISPOSITION",
            0xC0000093 => "FLOAT_INVALID_OPERATION",
            0xC0000092 => "FLOAT_DIVIDE_BY_ZERO",
            0xC000008C => "ARRAY_BOUNDS_EXCEEDED",
            0xC0000095 => "INT_OVERFLOW",
            0xE06D7363 => "C++ EH exception (thrown via throw)",
            0xE0434352 => "CLR managed exception",
            _ => "(unknown — see https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-erref)",
        }
    }

    // ---------------------------------------------------------------------------
    // Crash info formatting + reporter spawn
    // ---------------------------------------------------------------------------

    fn format_crash_info(kind: &str, detail: &str, backtrace: Option<&str>) -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| {
                let secs = d.as_secs();
                let days = secs / 86_400;
                let rem = secs % 86_400;
                let h = rem / 3600;
                let m = (rem % 3600) / 60;
                let s = rem % 60;
                format!("{secs} (≈ {days}d {h:02}:{m:02}:{s:02} UTC since Unix epoch)")
            })
            .unwrap_or_else(|_| "<unknown time>".to_string());

        let version = env!("CARGO_PKG_VERSION");
        let os = std::env::consts::OS;

        let mut s = String::with_capacity(2048);
        s.push_str("Hush_UI has encountered a fatal error and needs to close.\n");
        s.push_str("We're sorry for the inconvenience. Details below.\n\n");
        s.push_str("============================================\n");
        s.push_str(" ERROR DETAILS\n");
        s.push_str("============================================\n");
        s.push_str(&format!("Error type:   {kind}\n"));
        s.push_str(&format!("App version:  {version}\n"));
        s.push_str(&format!("Target OS:    {os}\n"));
        s.push_str(&format!("Timestamp:    {timestamp}\n"));
        s.push_str("\n");
        s.push_str(detail);
        s.push_str("\n\n");
        s.push_str("============================================\n");
        s.push_str(" STACK TRACE\n");
        s.push_str("============================================\n");
        if let Some(bt) = backtrace {
            if bt.trim().is_empty() {
                s.push_str("<empty stack trace>\n");
            } else {
                s.push_str(bt);
                if !bt.ends_with('\n') {
                    s.push('\n');
                }
            }
        } else {
            s.push_str("<no stack trace captured>\n");
        }
        s.push('\n');
        s.push_str("============================================\n");
        s.push_str(" HOW TO REPORT THIS\n");
        s.push_str("============================================\n");
        s.push_str(
            "Please open an issue and paste the entire contents of this dialog:\n",
        );
        s.push_str("  https://github.com/nend-x/Hush_UI/issues\n\n");
        s.push_str("If you can reproduce the crash, note the steps you took\n");
        s.push_str("right before this dialog appeared.\n");
        s
    }

    fn spawn_crash_reporter(info: &str) {
        // Resolve our own exe path — the same binary, invoked with
        // --crash-report, will display the dialog.
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(_) => return,
        };

        // Use a unique filename so concurrent crashes don't collide (in case
        // the re-entrancy guard ever lets a second one through).
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let mut tmp = std::env::temp_dir();
        tmp.push(format!("hush-crash-{ts}.txt"));

        if std::fs::write(&tmp, info).is_err() {
            return;
        }

        let Some(tmp_str) = tmp.to_str().map(|s| s.to_string()) else {
            // Path wasn't valid UTF-8 — clean up and give up.
            let _ = std::fs::remove_file(&tmp);
            return;
        };

        // Spawn detached. We intentionally do NOT wait — the parent is about
        // to die and we want the child to outlive it.
        let spawn_result = std::process::Command::new(exe)
            .arg(super::CRASH_REPORT_FLAG)
            .arg(&tmp_str)
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn();
        if spawn_result.is_err() {
            // Best-effort cleanup so we don't leak the temp file.
            let _ = std::fs::remove_file(&tmp);
        }
    }

    // ---------------------------------------------------------------------------
    // MessageBox helper
    // ---------------------------------------------------------------------------

    fn message_box(title: &str, body: &str) {
        let title_w = to_wide(title);
        let body_w = to_wide(body);
        let flags: MESSAGEBOX_STYLE = MB_ICONERROR | MB_OK | MB_SETFOREGROUND | MB_TOPMOST
            | MB_TASKMODAL;
        unsafe {
            // No owner window — `None` makes the message box top-level.
            // MB_TASKMODAL disables all top-level windows of the calling
            // thread (we have none, so it's a no-op) and gives the box
            // proper foreground activation.
            let _ = MessageBoxW(None, PCWSTR::from_raw(body_w.as_ptr()), PCWSTR::from_raw(title_w.as_ptr()), flags);
        }
    }

    /// Encode a Rust &str as a NUL-terminated UTF-16 wide string suitable for
    /// `PCWSTR::from_raw`.
    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(not(windows))]
mod imp {
    //! Non-Windows stub — the real handler is Windows-only. On other targets
    //! we degrade gracefully: `install()` is a no-op, `show_crash_dialog`
    //! just prints the file path so dev builds on Linux still produce
    //! *some* signal in the terminal.

    pub fn install() {
        // No-op outside Windows.
    }

    pub fn show_crash_dialog(path: &str) -> i32 {
        eprintln!("hush: crash report at {path} (non-Windows: no dialog)");
        if let Ok(s) = std::fs::read_to_string(path) {
            eprintln!("--- crash report ---\n{s}\n--------------------");
        }
        let _ = std::fs::remove_file(path);
        0
    }
}

pub use imp::*;
