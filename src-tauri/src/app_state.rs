// AppState — shared mutable state

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskbarApp {
    pub id: String,
    pub name: String,
    #[serde(rename = "icon_data_url")]
    pub icon_data_url: Option<String>,
    pub running: bool,
    pub pinned: bool,
    /// True if this app currently has the foreground (active) window
    pub is_foreground: bool,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct DesktopItem {
    pub id: String,
    pub name: String,
    pub path: String,
    #[serde(rename = "icon_data_url")]
    pub icon_data_url: Option<String>,
    pub is_folder: bool,
}

pub struct AppState {
    pub taskbar_apps: Vec<TaskbarApp>,
    pub desktop_items: Vec<DesktopItem>,
    /// Window blacklist — identified by PERMANENT traits (title + exe_path),
    pub blacklisted: Vec<BlacklistEntry>,
    pub blacklisted_hwnds: Vec<usize>,
    pub app_order: Vec<String>,
}

/// A persistent blacklist entry — matches by exe_path (permanent).
/// Title is optional (for display). HWND is the live window handle (transient).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct BlacklistEntry {
    /// Process exe path — the permanent identifier
    pub exe_path: String,
    /// Window title (optional, for display only)
    pub title: Option<String>,
    /// Live HWND (set on each scan; not persisted)
    #[serde(skip)]
    pub hwnd: usize,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            taskbar_apps: Vec::new(),
            desktop_items: Vec::new(),
            blacklisted: Vec::new(),
            blacklisted_hwnds: Vec::new(),
            app_order: Vec::new(),
        }
    }
}
