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
    /// True when the user pinned this item to the top of the grid.
    /// Persisted separately (desktop_pins.json); always serialized so
    /// every consumer (frontend tiles included) sees a stable field.
    #[serde(default)]
    pub pinned: bool,
}

pub struct AppState {
    pub taskbar_apps: Vec<TaskbarApp>,
    pub desktop_items: Vec<DesktopItem>,
    pub app_order: Vec<String>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            taskbar_apps: Vec::new(),
            desktop_items: Vec::new(),
            app_order: Vec::new(),
        }
    }
}
