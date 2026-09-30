// Search index — an on-disk snapshot of the searchable file tree.
//
// WHY: the hushlight search used to walk the Start Menu + Desktop trees on
// EVERY keystroke and ran SHGetFileInfoW (an explorer.exe round trip) per
// match. With a cold icon cache that made typing visibly laggy. The index
// flips the cost around: a slow, explicit, user-triggered "Index" build in
// the settings table (with a live progress notification) produces a JSON
// snapshot; the per-keystroke search then filters an in-memory list — no
// filesystem walking, and icons are resolved only for the final page of
// results.
//
// Storage: %LOCALAPPDATA%\Hush_UI\search_index.json
// {
//   "built_at": 1727...,            // unix epoch seconds
//   "items": [ { "name": "...", "path": "...", "is_folder": false }, ... ]
// }

#![cfg_attr(not(windows), allow(dead_code))]

use crate::persist::data_dir;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Emitter;

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct IndexItem {
    pub name: String,
    pub path: String,
    pub is_folder: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
pub struct SearchIndex {
    pub built_at: u64,
    pub items: Vec<IndexItem>,
}

static INDEX: std::sync::Mutex<Option<SearchIndex>> = std::sync::Mutex::new(None);
static BUILDING: AtomicBool = AtomicBool::new(false);

fn index_path() -> std::path::PathBuf {
    data_dir().join("search_index.json")
}

/// Load the index from disk once and keep it in memory. Returns true when
/// an index is available (memory or disk).
pub fn ensure_loaded() -> bool {
    let mut guard = match INDEX.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    if guard.is_some() {
        return true;
    }
    match std::fs::read_to_string(index_path()) {
        Ok(s) => match serde_json::from_str::<SearchIndex>(&s) {
            Ok(idx) => {
                log::info!("Search index loaded: {} items", idx.items.len());
                *guard = Some(idx);
                true
            }
            Err(e) => {
                log::warn!("Search index corrupt, ignoring: {e}");
                false
            }
        },
        Err(_) => false,
    }
}

/// A snapshot for searching, if available.
pub fn snapshot() -> Option<Vec<IndexItem>> {
    if !ensure_loaded() {
        return None;
    }
    INDEX.lock().ok().and_then(|g| g.as_ref().map(|i| i.items.clone()))
}

pub fn is_building() -> bool {
    BUILDING.load(Ordering::SeqCst)
}

/// Build timestamp of the loaded index (0 when not loaded).
pub fn built_at() -> u64 {
    INDEX.lock().ok().and_then(|g| g.as_ref().map(|i| i.built_at)).unwrap_or(0)
}

/// Delete the on-disk index and drop the in-memory copy.
pub fn clear() -> bool {
    if let Ok(mut guard) = INDEX.lock() {
        *guard = None;
    }
    let existed = index_path().exists();
    if existed {
        let _ = std::fs::remove_file(index_path());
    }
    existed
}

/// Build the index in the caller's thread. Emits `index://progress` events
/// (and drives the progress notification) via `app`.
pub fn build(app: &tauri::AppHandle) {
    use serde_json::json;

    if BUILDING.swap(true, Ordering::SeqCst) {
        log::info!("Index build already running");
        return;
    }

    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // roots: (display label, path)
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    let mut push_root = |env_key: &str, sub: &str| {
        if let Some(base) = std::env::var_os(env_key) {
            let p = std::path::PathBuf::from(base).join(sub);
            if p.exists() {
                roots.push(p);
            }
        }
    };
    push_root("PROGRAMDATA", "Microsoft\\Windows\\Start Menu\\Programs");
    push_root("APPDATA", "Microsoft\\Windows\\Start Menu\\Programs");
    push_root("USERPROFILE", "Desktop");
    push_root("PUBLIC", "Desktop");

    // Phase 1 — count matching files for smooth progress.
    let total = count_files(&roots);
    let mut scanned: usize = 0;
    let mut last_pct: u32 = 0;

    // Show the progress notification (0%).
    crate::show_notification_progress(app, "Hush_UI", "Indexing files…", 0.0);

    let mut items: Vec<IndexItem> = Vec::new();
    for root in &roots {
        collect(root, 0, &mut items, &mut |n| {
            scanned += n;
            if total > 0 {
                let pct = ((scanned as f64 / total as f64) * 100.0).round() as u32;
                if pct > last_pct && pct < 100 {
                    last_pct = pct;
                    let _ = app.emit("index://progress", json!({ "progress": pct }));
                    crate::update_notification_progress(app, pct as f32);
                }
            }
        });
    }

    let idx = SearchIndex {
        built_at: started,
        items,
    };
    let count = idx.items.len();

    // Persist.
    if let Ok(s) = serde_json::to_string_pretty(&idx) {
        let _ = std::fs::write(index_path(), s);
    }
    if let Ok(mut guard) = INDEX.lock() {
        *guard = Some(idx);
    }

    // 100% → hold the completed toast for one second → hide.
    let _ = app.emit("index://progress", json!({ "progress": 100 }));
    crate::finish_notification_progress(app, format!("Indexed {count} items"));
    log::info!("Search index built: {count} items ({total} files scanned)");

    BUILDING.store(false, Ordering::SeqCst);
}

/// Recursively collect index items for one root. `on_file` is called once
/// per visited FILE/DIR so the caller can drive progress.
fn collect(
    dir: &std::path::Path,
    depth: usize,
    items: &mut Vec<IndexItem>,
    on_file: &mut impl FnMut(usize),
) {
    const MAX_DEPTH: usize = 8;
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_dir = path.is_dir();
        on_file(1);

        if is_dir {
            if depth < MAX_DEPTH {
                collect(&path, depth + 1, items, on_file);
            }
            // Directory entries themselves are searchable (desktop folders,
            // start menu folders).
            if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                items.push(IndexItem {
                    name: name.to_string(),
                    path: path.to_string_lossy().to_string(),
                    is_folder: true,
                });
            }
        } else {
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if ext != "lnk" && ext != "exe" && ext != "url" {
                continue;
            }
            if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                items.push(IndexItem {
                    name: name.to_string(),
                    path: path.to_string_lossy().to_string(),
                    is_folder: false,
                });
            }
        }
    }
}

/// Pass 1: how many entries will the build visit (progress denominator).
fn count_files(roots: &[std::path::PathBuf]) -> usize {
    fn walk(dir: &std::path::Path, depth: usize, n: &mut usize) {
        const MAX_DEPTH: usize = 8;
        if depth > MAX_DEPTH {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                *n += 1;
                if entry.path().is_dir() {
                    walk(&entry.path(), depth + 1, n);
                }
            }
        }
    }
    let mut n = 0;
    for r in roots {
        walk(r, 0, &mut n);
    }
    n
}
