// Desktop watcher — automatic desktop item refresh (0.3.0).
//
// The desktop table used to receive updates ONLY from manual actions
// (create/rename/delete through Hush_UI) or the context-menu "Refresh".
// Files created/deleted/renamed by any other program never appeared until
// the user refreshed by hand.
//
// This module watches both desktop directories (user + public) with
// ReadDirectoryChangesW — a blocking kernel call, so an idle watch costs
// ZERO CPU (the thread is parked in the kernel). When a change lands we
// debounce briefly (editors touch files in bursts), then rescan; the rescan
// result is compared against the previous snapshot and only a REAL change
// is emitted to the frontend — so the table never re-renders for noise.

#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(windows)]
static DIRTY: AtomicBool = AtomicBool::new(false);

/// Called once at startup: spawn one watcher thread per desktop directory
/// plus a scanner thread that drains the dirty flag.
#[cfg(windows)]
pub fn start(app: tauri::AppHandle) {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(up) = std::env::var_os("USERPROFILE") {
        let d = std::path::PathBuf::from(up).join("Desktop");
        if d.exists() {
            dirs.push(d);
        }
    }
    if let Some(pubdir) = std::env::var_os("PUBLIC") {
        let d = std::path::PathBuf::from(pubdir).join("Desktop");
        if d.exists() {
            dirs.push(d);
        }
    }

    for dir in dirs {
        let app = app.clone();
        std::thread::Builder::new()
            .name("desktop-watch".into())
            .spawn(move || watch_dir(dir, app))
            .ok();
    }

    // Scanner: when a watcher marks DIRTY, debounce has already happened in
    // watch_dir — scan once and clear. Coalesces bursts from both threads.
    let app = app.clone();
    std::thread::Builder::new()
        .name("desktop-scan".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(250));
            if DIRTY.swap(false, Ordering::SeqCst) {
                crate::refresh_desktop_items(&app);
            }
        })
        .ok();
}

#[cfg(windows)]
fn watch_dir(dir: std::path::PathBuf, _app: tauri::AppHandle) {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, ReadDirectoryChangesW, FILE_ACTION_ADDED, FILE_ACTION_MODIFIED,
        FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_NEW_NAME, FILE_ACTION_RENAMED_OLD_NAME,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_ATTRIBUTES,
        FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
        FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::Security::SECURITY_ATTRIBUTES;

    let wide: Vec<u16> = std::ffi::OsStr::new(&dir)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let handle = CreateFileW(
            windows::core::PCWSTR(wide.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            Some(SECURITY_ATTRIBUTES::default()),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
        .unwrap_or_else(|_| HANDLE(std::ptr::null_mut()));

        if handle.is_invalid() || handle == INVALID_HANDLE_VALUE {
            log::warn!("desktop-watch: CreateFileW failed for {}", dir.display());
            return;
        }

        const BUF_LEN: usize = 4096;
        let mut buffer = [0u8; BUF_LEN];
        let notify_filter = FILE_NOTIFY_CHANGE_FILE_NAME
            | FILE_NOTIFY_CHANGE_DIR_NAME
            | FILE_NOTIFY_CHANGE_ATTRIBUTES
            | FILE_NOTIFY_CHANGE_SIZE
            | FILE_NOTIFY_CHANGE_LAST_WRITE
            | FILE_NOTIFY_CHANGE_CREATION;

        loop {
            let mut bytes_returned: u32 = 0;
            let ok = ReadDirectoryChangesW(
                handle,
                buffer.as_mut_ptr() as *mut _,
                BUF_LEN as u32,
                true, // watch subtrees (desktop folders are displayed too)
                notify_filter,
                Some(&mut bytes_returned),
                None,
                None,
            );
            if !ok.as_bool() {
                // Directory gone (user deleted Desktop?) — stop watching.
                log::warn!("desktop-watch: ReadDirectoryChangesW failed for {}", dir.display());
                break;
            }
            // Any relevant event marks the desktop dirty. Debounce: editors
            // and installers touch files in bursts, so wait a beat before
            // the rescan. Multiple events collapse into one scan.
            DIRTY.store(true, Ordering::SeqCst);
            let _ = FILE_ACTION_ADDED;
            let _ = FILE_ACTION_REMOVED;
            let _ = FILE_ACTION_MODIFIED;
            let _ = FILE_ACTION_RENAMED_OLD_NAME;
            let _ = FILE_ACTION_RENAMED_NEW_NAME;
            std::thread::sleep(std::time::Duration::from_millis(350));
        }

        let _ = windows::Win32::Foundation::CloseHandle(handle);
    }
}

#[cfg(not(windows))]
pub fn start(_app: tauri::AppHandle) {}
