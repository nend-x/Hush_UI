#![cfg(windows)]
// Shell helpers — read Desktop, launch files, create/rename/delete items.

use crate::app_state::DesktopItem;
use crate::win32::icon::extract_icon_for_path;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows::core::{self, PCWSTR, HRESULT};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

pub fn scan_desktop() -> core::Result<Vec<DesktopItem>> {
    let desktop = std::env::var("USERPROFILE")
        .map(std::path::PathBuf::from)
        .map(|p| p.join("Desktop"))
        .map_err(|_| core::Error::new(HRESULT(-1), "USERPROFILE not set"))?;

    if !desktop.exists() {
        return Ok(Vec::new());
    }

    let mut items: Vec<DesktopItem> = Vec::new();
    let mut dirs_to_scan: Vec<std::path::PathBuf> = vec![desktop];

    if let Some(p) = std::env::var("PUBLIC").ok().map(std::path::PathBuf::from).map(|p| p.join("Desktop")) {
        if p.exists() {
            dirs_to_scan.push(p);
        }
    }

    for dir in dirs_to_scan {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .or_else(|| path.file_name().and_then(|s| s.to_str()))
                    .unwrap_or("?")
                    .to_string();

                if let Ok(meta) = entry.metadata() {
                    use std::os::windows::fs::MetadataExt;
                    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
                    if meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0 {
                        continue;
                    }
                }

                let is_folder = path.is_dir();
                let icon = extract_icon_for_path(&path.to_string_lossy());

                items.push(DesktopItem {
                    id: path.to_string_lossy().to_string(),
                    name,
                    path: path.to_string_lossy().to_string(),
                    icon_data_url: icon,
                    is_folder,
                });
            }
        }
    }

    // Disambiguate colliding display names. Two items whose display names
    // are identical — e.g. a "widget" folder and a "widget.exe" beside it —
    // share the same file stem, which made the two grid tiles
    // indistinguishable and invited launching the wrong item. When a
    // non-folder item shares its display name with any other item, show its
    // full file name (with extension) instead — Explorer does the same.
    {
        let mut name_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for it in items.iter() {
            *name_counts.entry(it.name.to_lowercase()).or_default() += 1;
        }
        for it in items.iter_mut() {
            let count = name_counts.get(&it.name.to_lowercase()).copied().unwrap_or(0);
            if count > 1 && !it.is_folder {
                if let Some(fname) = std::path::Path::new(&it.path)
                    .file_name()
                    .and_then(|s| s.to_str())
                {
                    it.name = fname.to_string();
                }
            }
        }
    }

    // User pins float to the top of the grid (in pin order — the first
    // pin lands first). Unpinned items keep the deterministic sort:
    // folders first, then alphabetical by display name.
    let pins = crate::persist::load_desktop_pins();
    for it in items.iter_mut() {
        it.pinned = pins.contains(&it.id);
    }
    let pin_rank = |it: &DesktopItem| {
        pins.iter().position(|p| p == &it.id).unwrap_or(usize::MAX)
    };
    items.sort_by(|a, b| match (a.pinned, b.pinned) {
        (true, true) => pin_rank(a).cmp(&pin_rank(b)),
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        (false, false) => match (a.is_folder, b.is_folder) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        },
    });

    Ok(items)
}

pub fn shell_execute(path: &str) -> core::Result<()> {
    // FOLDERS must never go through ShellExecuteW with the folder path as
    // lpFile. The whole launch chain above us is path-exact (the item id IS
    // the absolute path, matched against the scan state), yet users saw a
    // folder whose name collides with a same-named .exe sibling launch that
    // .exe. With every layer passing the exact path, the one remaining place
    // the two can swap is the final shell NAME RESOLUTION inside
    // ShellExecuteW: for an extensionless lpFile the shell resolver may bind
    // the sibling executable instead of the directory. Dispatch folders
    // explicitly — no name left to resolve.
    if std::path::Path::new(path).is_dir() {
        return open_folder_in_explorer(path);
    }

    let wide_path: Vec<u16> = OsStr::new(path).encode_wide().chain(std::iter::once(0)).collect();
    let verb: Vec<u16> = OsStr::new("open").encode_wide().chain(std::iter::once(0)).collect();

    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(wide_path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    if result.0 as usize <= 32 {
        return Err(core::Error::new(
            HRESULT(-1),
            format!("ShellExecuteW failed: error code {}", result.0 as usize),
        ));
    }
    Ok(())
}

/// Open `path` (a directory) in Windows Explorer with zero name resolution.
///
/// We launch `%SystemRoot%\explorer.exe` — resolved from an absolute path, so
/// it can never bind to anything else — and pass the folder path as a quoted
/// ARGUMENT. The folder path travels as data: `explorer.exe` is not asked to
/// "find" anything by name, and no shell resolver ever sees the extensionless
/// path as an executable candidate. As a safety net, if the explicit dispatch
/// fails we retry with the "explore" verb, which is folder-only by definition
/// (it fails on non-folders rather than launching something else).
fn open_folder_in_explorer(path: &str) -> core::Result<()> {
    log::info!("open_folder_in_explorer: {path}");

    let wide_dir: Vec<u16> = OsStr::new(path).encode_wide().chain(std::iter::once(0)).collect();

    // 1) Explicit dispatch: <SystemRoot>\explorer.exe "<folder>"
    let explorer_path = match std::env::var("SystemRoot") {
        Ok(root) => std::path::PathBuf::from(root).join("explorer.exe"),
        Err(_) => std::path::PathBuf::from("C:\\Windows\\explorer.exe"),
    };
    if explorer_path.is_file() {
        let wide_explorer: Vec<u16> = OsStr::new(&explorer_path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // Quoted argument — paths from read_dir never end in a backslash, so
        // the trailing quote cannot be escaped by the path itself.
        let params = format!("\"{}\"", path);
        let wide_params: Vec<u16> =
            OsStr::new(&params).encode_wide().chain(std::iter::once(0)).collect();
        let verb: Vec<u16> = OsStr::new("open").encode_wide().chain(std::iter::once(0)).collect();

        let result = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(verb.as_ptr()),
                PCWSTR(wide_explorer.as_ptr()),
                PCWSTR(wide_params.as_ptr()),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if result.0 as usize > 32 {
            return Ok(());
        }
        log::warn!(
            "open_folder_in_explorer: explicit explorer.exe dispatch failed (code {}), falling back to 'explore' verb",
            result.0 as usize
        );
    } else {
        log::warn!(
            "open_folder_in_explorer: explorer.exe not found at {}, falling back to 'explore' verb",
            explorer_path.display()
        );
    }

    // 2) Fallback: the "explore" verb is defined only for directories — it
    // cannot launch an executable, so the worst case here is a no-op error.
    let verb: Vec<u16> = OsStr::new("explore").encode_wide().chain(std::iter::once(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(wide_dir.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as usize <= 32 {
        return Err(core::Error::new(
            HRESULT(-1),
            format!("open_folder_in_explorer failed: error code {}", result.0 as usize),
        ));
    }
    Ok(())
}

/// Create a new folder or empty file on the Desktop.
pub fn create_item(name: &str, is_folder: bool) -> core::Result<DesktopItem> {
    use std::io::Write;
    let desktop = std::env::var("USERPROFILE")
        .map(std::path::PathBuf::from)
        .map(|p| p.join("Desktop"))
        .map_err(|_| core::Error::new(HRESULT(-1), "USERPROFILE not set"))?;

    // Find a non-colliding name
    let base_path = desktop.join(name);
    let final_path = if base_path.exists() {
        let stem = std::path::Path::new(name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(name);
        let ext = std::path::Path::new(name).extension().and_then(|s| s.to_str());
        let mut idx = 2;
        loop {
            let candidate_name = match ext {
                Some(e) => format!("{} ({}).{}", stem, idx, e),
                None => format!("{} ({})", stem, idx),
            };
            let candidate = desktop.join(&candidate_name);
            if !candidate.exists() {
                break candidate;
            }
            idx += 1;
        }
    } else {
        base_path
    };

    if is_folder {
        std::fs::create_dir(&final_path)
            .map_err(|e| core::Error::new(HRESULT(-1), format!("create_dir: {}", e)))?;
    } else {
        let _ = std::fs::File::create(&final_path)
            .map_err(|e| core::Error::new(HRESULT(-1), format!("create file: {}", e)))?
            .write_all(b"")?;
    }

    let final_name = final_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name)
        .to_string();

    Ok(DesktopItem {
        id: final_path.to_string_lossy().to_string(),
        name: final_name,
        path: final_path.to_string_lossy().to_string(),
        icon_data_url: extract_icon_for_path(&final_path.to_string_lossy()),
        is_folder,
    })
}

pub fn delete_item(item_id: &str) -> core::Result<()> {
    let path = std::path::Path::new(item_id);
    if !path.exists() {
        return Err(core::Error::new(HRESULT(-1), "not found"));
    }
    if path.is_dir() {
        std::fs::remove_dir_all(path)
            .map_err(|e| core::Error::new(HRESULT(-1), format!("remove_dir_all: {}", e)))?;
    } else {
        std::fs::remove_file(path)
            .map_err(|e| core::Error::new(HRESULT(-1), format!("remove_file: {}", e)))?;
    }
    Ok(())
}

pub fn rename_item(item_id: &str, new_name: &str) -> core::Result<DesktopItem> {
    let old_path = std::path::Path::new(item_id);
    if !old_path.exists() {
        return Err(core::Error::new(HRESULT(-1), "not found"));
    }
    let parent = old_path.parent().ok_or_else(|| core::Error::new(HRESULT(-1), "no parent"))?;
    let new_path = parent.join(new_name);

    std::fs::rename(old_path, &new_path)
        .map_err(|e| core::Error::new(HRESULT(-1), format!("rename: {}", e)))?;

    Ok(DesktopItem {
        id: new_path.to_string_lossy().to_string(),
        name: new_name.to_string(),
        path: new_path.to_string_lossy().to_string(),
        icon_data_url: extract_icon_for_path(&new_path.to_string_lossy()),
        is_folder: new_path.is_dir(),
    })
}
