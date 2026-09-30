// Hush_UI entry point
// GUI subsystem in release: a shell replacement must never flash a console
// window on launch. (This attribute only has an effect here in the binary
// crate — having it only in lib.rs, as before, silently did nothing.)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Crash-report subprocess mode.
    //
    // When the main Hush_UI process crashes, the crash handler (see
    // `crash_handler.rs`) writes the crash info to a temp file and
    // re-launches THIS binary with `--crash-report <path>`. We detect that
    // here, before doing any Tauri setup, and short-circuit straight to the
    // crash dialog. This keeps the crash reporter in the same exe (no
    // second binary to ship) while still running in a clean, independent
    // process whose state isn't corrupted by whatever killed the parent.
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 3 && args[1] == hush_lib::crash_handler::CRASH_REPORT_FLAG {
        let exit_code = hush_lib::crash_handler::show_crash_dialog(&args[2]);
        std::process::exit(exit_code);
    }

    hush_lib::run()
}
