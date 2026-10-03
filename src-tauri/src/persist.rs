// Persistence — load/save config files
//
// Config path (Hush_UI): %LOCALAPPDATA%\Hush_UI\
//   clipboard.json, notes.txt, widgets.json, desktop_pins.json,
//   widget_visibility.json, icon_recolor.json, settings.json,
//   themes.json, tables.json
//
// (Hush_UI pre-0.2 used %LOCALAPPDATA%\Hush_UI — the rename to Hush_UI
// ships a fresh config directory; no migration for the pre-release.)

use std::path::PathBuf;
use std::fs;

/// Root config directory. Pub because lib.rs's reset_config (-rs flag) walks
/// the same directory.
pub fn data_dir() -> PathBuf {
    // Always use LOCALAPPDATA — no admin required, user-specific
    if let Some(lad) = std::env::var_os("LOCALAPPDATA") {
        let dir = PathBuf::from(&lad).join("Hush_UI");
        let _ = fs::create_dir_all(&dir);
        return dir;
    }
    if let Some(pd) = std::env::var_os("PROGRAMDATA") {
        let dir = PathBuf::from(&pd).join("Hush_UI");
        let _ = fs::create_dir_all(&dir);
        return dir;
    }
    PathBuf::from(".")
}

// ===== Clipboard history =====
pub fn load_clipboard() -> Vec<String> {
    let path = data_dir().join("clipboard.json");
    match fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

pub fn save_clipboard(items: &[String]) {
    let path = data_dir().join("clipboard.json");
    if let Ok(s) = serde_json::to_string_pretty(items) {
        let _ = fs::write(&path, s);
    }
}

// ===== Notes =====
pub fn load_notes() -> String {
    let path = data_dir().join("notes.txt");
    fs::read_to_string(&path).unwrap_or_default()
}

pub fn save_notes(text: &str) {
    let path = data_dir().join("notes.txt");
    let _ = fs::write(&path, text);
}

// ===== Widget positions =====
pub fn load_widget_positions() -> std::collections::HashMap<String, (f64, f64)> {
    let path = data_dir().join("widgets.json");
    match fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => std::collections::HashMap::new(),
    }
}

pub fn save_widget_positions(positions: &std::collections::HashMap<String, (f64, f64)>) {
    let path = data_dir().join("widgets.json");
    if let Ok(s) = serde_json::to_string_pretty(positions) {
        let _ = fs::write(&path, s);
    }
}

// ===== Widget visibility =====
pub fn load_widget_visibility() -> std::collections::HashMap<String, bool> {
    let path = data_dir().join("widget_visibility.json");
    match fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => std::collections::HashMap::new(),
    }
}

pub fn save_widget_visibility(visibility: &std::collections::HashMap<String, bool>) {
    let path = data_dir().join("widget_visibility.json");
    if let Ok(s) = serde_json::to_string_pretty(visibility) {
        let _ = fs::write(&path, s);
    }
}

// ===== Icon recolor =====
pub fn load_icon_recolor() -> bool {
    let path = data_dir().join("icon_recolor.json");
    match fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str::<bool>(&s).unwrap_or(false),
        Err(_) => false,
    }
}

pub fn save_icon_recolor(enabled: bool) {
    let path = data_dir().join("icon_recolor.json");
    if let Ok(s) = serde_json::to_string_pretty(&enabled) {
        let _ = fs::write(&path, s);
    }
}

// ===== Settings =====
// Extended for the Hush_UI 0.2 tables update. New fields all use
// #[serde(default)] so a settings.json written by an older build (or a
// hand-edited one) still deserializes.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
pub struct Settings {
    pub theme: String,
    pub auto_fullscreen: bool,
    pub refresh_interval: u64,
    pub cube_animation: bool,
    // --- New in 0.2 (tables update) ---
    /// How long Win must be held before the radial table picker appears.
    /// A shorter tap still toggles the launcher. 80–1000 ms.
    #[serde(default = "default_tables_hold_ms")]
    pub tables_hold_ms: u64,
    /// Taskbar clock format — true = 24h, false = 12h (segmented control).
    #[serde(default = "default_true")]
    pub clock_24h: bool,
    /// Show the local-time clock in the center hub of the pie picker.
    #[serde(default = "default_true")]
    pub pie_clock: bool,
    /// Hide the desktop icons inside the launcher grid (toggle).
    #[serde(default = "default_true")]
    pub show_desktop_grid: bool,
    /// Brightness dimmer strength — a systemless software dim overlay (see
    /// win32::dimmer). 0.0 = no dim, 1.0 = max dim (~86%, never fully
    /// opaque). The overlay is a click-through black layered window;
    /// nothing on the system is modified. Default: off.
    #[serde(default)]
    pub dimmer_level: f64,
}

fn default_tables_hold_ms() -> u64 { 80 }
fn default_true() -> bool { true }

pub fn load_settings() -> Settings {
    let path = data_dir().join("settings.json");
    match fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|_| default_settings()),
        Err(_) => default_settings(),
    }
}

fn default_settings() -> Settings {
    Settings {
        theme: "material3-dark".to_string(),
        auto_fullscreen: true,
        dimmer_level: 0.0,
        refresh_interval: 2,
        cube_animation: true,
        tables_hold_ms: 80,
        clock_24h: true,
        pie_clock: true,
        show_desktop_grid: true,
    }
}

pub fn save_settings(settings: &Settings) {
    let path = data_dir().join("settings.json");
    if let Ok(s) = serde_json::to_string_pretty(settings) {
        let _ = fs::write(&path, s);
    }
}

// ===== Tables (the radial-picker windows) =====
// tables.json stores the restored position of the movable tables — the
// settings table and the widgets table. Keys are table names, values are
// [x, y] in PHYSICAL screen pixels (matches Tauri's WindowEvent::Moved
// payload, so saving needs no conversion and multi-monitor coords — which
// can be negative — round-trip exactly).
//
// {
//   "settings": [1420, 260],
//   "widgets":  [320, 180]
// }

pub fn load_table_positions() -> std::collections::HashMap<String, (i32, i32)> {
    let path = data_dir().join("tables.json");
    match fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => std::collections::HashMap::new(),
    }
}

pub fn save_table_positions(positions: &std::collections::HashMap<String, (i32, i32)>) {
    let path = data_dir().join("tables.json");
    if let Ok(s) = serde_json::to_string(&positions) {
        let _ = fs::write(&path, s);
    }
}

// ===== Desktop pins =====
//
// "Pin to top" on the desktop-table context menu. Pins are stored as the
// item ids (absolute paths — same id the scan produces), in pin order:
// the first pinned id floats to the very top of the grid, the rest keep
// their pin order behind it.
fn desktop_pins_path() -> PathBuf {
    data_dir().join("desktop_pins.json")
}

pub fn load_desktop_pins() -> Vec<String> {
    let path = desktop_pins_path();
    match fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

pub fn save_desktop_pins(pins: &[String]) {
    let path = desktop_pins_path();
    if let Ok(s) = serde_json::to_string(pins) {
        let _ = fs::write(&path, s);
    }
}

// ===== Themes =====
// themes.json format:
// {
//   "themes": [
//     {
//       "name": "sand-cream",
//       "active": true,
//       "colors": {
//         "bg-espresso": "#4B3621",
//         "bg-espresso-deep": "#3A2A1A",
//         "bg-espresso-raised": "#54402D",
//         "bg-espresso-frosted": "rgba(58,42,26,0.55)",
//         "bg-espresso-glass": "rgba(58,42,26,0.65)",
//         "sand": "#C2B280",
//         "sand-bright": "#D4C19C",
//         "sand-dim": "#8A7B5C",
//         "sand-cream": "#EDE4D3",
//         "accent-terracotta": "#B8835A",
//         "accent-caramel": "#D4A574",
//         "accent-soft": "rgba(184,131,90,0.18)",
//         "border-subtle": "rgba(194,178,128,0.08)",
//         "border-strong": "rgba(194,178,128,0.18)",
//         "status-running": "#D4A574",
//         "status-pinned": "#8A7B5C"
//       }
//     },
//     ... other themes
//   ]
// }

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct ThemeColors {
    #[serde(rename = "bg-espresso")]
    pub bg_espresso: String,
    #[serde(rename = "bg-espresso-rgb")]
    pub bg_espresso_rgb: String,
    #[serde(rename = "bg-espresso-deep")]
    pub bg_espresso_deep: String,
    #[serde(rename = "bg-espresso-deep-rgb")]
    pub bg_espresso_deep_rgb: String,
    #[serde(rename = "bg-espresso-raised")]
    pub bg_espresso_raised: String,
    #[serde(rename = "bg-espresso-raised-rgb")]
    pub bg_espresso_raised_rgb: String,
    #[serde(rename = "bg-espresso-frosted")]
    pub bg_espresso_frosted: String,
    #[serde(rename = "bg-espresso-glass")]
    pub bg_espresso_glass: String,
    pub sand: String,
    #[serde(rename = "sand-rgb")]
    pub sand_rgb: String,
    #[serde(rename = "sand-bright")]
    pub sand_bright: String,
    #[serde(rename = "sand-bright-rgb")]
    pub sand_bright_rgb: String,
    #[serde(rename = "sand-dim")]
    pub sand_dim: String,
    #[serde(rename = "sand-dim-rgb")]
    pub sand_dim_rgb: String,
    #[serde(rename = "sand-cream")]
    pub sand_cream: String,
    #[serde(rename = "sand-cream-rgb")]
    pub sand_cream_rgb: String,
    #[serde(rename = "accent-terracotta")]
    pub accent_terracotta: String,
    #[serde(rename = "accent-terracotta-rgb")]
    pub accent_terracotta_rgb: String,
    #[serde(rename = "accent-caramel")]
    pub accent_caramel: String,
    #[serde(rename = "accent-caramel-rgb")]
    pub accent_caramel_rgb: String,
    #[serde(rename = "accent-soft")]
    pub accent_soft: String,
    #[serde(rename = "border-subtle")]
    pub border_subtle: String,
    #[serde(rename = "border-strong")]
    pub border_strong: String,
    #[serde(rename = "status-running")]
    pub status_running: String,
    #[serde(rename = "status-pinned")]
    pub status_pinned: String,
    // Danger accent (destructive actions: End task, Delete, Exit) — themed
    // per palette so destructive UI never falls back to a hardcoded red.
    #[serde(rename = "danger")]
    pub danger: String,
    #[serde(rename = "danger-rgb")]
    pub danger_rgb: String,
    // Text colors (calibrated per-theme for readability)
    #[serde(rename = "text-primary")]
    pub text_primary: String,
    #[serde(rename = "text-secondary")]
    pub text_secondary: String,
    #[serde(rename = "text-muted")]
    pub text_muted: String,
    // Shadows (calibrated per-theme — light themes use softer shadows)
    #[serde(rename = "shadow-window")]
    pub shadow_window: String,
    #[serde(rename = "shadow-popup")]
    pub shadow_popup: String,
    #[serde(rename = "shadow-icon-hover")]
    pub shadow_icon_hover: String,
    #[serde(rename = "shadow-card")]
    pub shadow_card: String,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct Theme {
    pub name: String,
    pub active: bool,
    pub colors: ThemeColors,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
pub struct ThemesConfig {
    /// Schema version of this file. Files missing the field (or with an
    /// older version) carry stale color payloads — load_themes() rewrites
    /// them with the current defaults. Bump whenever default_themes()
    /// changes meaningfully.
    #[serde(default)]
    pub version: u32,
    pub themes: Vec<Theme>,
}

/// Current themes.json schema version.
const THEMES_VERSION: u32 = 2;

pub fn default_themes() -> ThemesConfig {
    ThemesConfig {
        version: THEMES_VERSION,
        themes: vec![
            // ===== material3-dark (active) — pure neutral gray =====
            // Flat gray surfaces (no hue tint), gray text ramp, a neutral
            // gray accent — NOT the M3 baseline purple. Fully opaque
            // surfaces — no blur, no frost, no grain.
            Theme {
                name: "material3-dark".to_string(),
                active: true,
                colors: ThemeColors {
                    // Surfaces — neutral gray: surface / lowest / high
                    bg_espresso: "#1C1C1C".to_string(),
                    bg_espresso_rgb: "28, 28, 28".to_string(),
                    bg_espresso_deep: "#131313".to_string(),
                    bg_espresso_deep_rgb: "19, 19, 19".to_string(),
                    bg_espresso_raised: "#2A2A2A".to_string(),
                    bg_espresso_raised_rgb: "42, 42, 42".to_string(),
                    // Opaque — this theme has NO blur/frosting
                    bg_espresso_frosted: "#1C1C1C".to_string(),
                    bg_espresso_glass: "#1C1C1C".to_string(),
                    // Gray ramp — outline / variant / on-surface
                    sand: "#8A8A8A".to_string(),
                    sand_rgb: "138, 138, 138".to_string(),
                    sand_bright: "#C6C6C6".to_string(),
                    sand_bright_rgb: "198, 198, 198".to_string(),
                    sand_dim: "#4A4A4A".to_string(),
                    sand_dim_rgb: "74, 74, 74".to_string(),
                    sand_cream: "#E8E8E8".to_string(),
                    sand_cream_rgb: "232, 232, 232".to_string(),
                    // Accents — neutral gray (hover/selection states)
                    accent_terracotta: "#ABABAB".to_string(),
                    accent_terracotta_rgb: "171, 171, 171".to_string(),
                    accent_caramel: "#C4C4C4".to_string(),
                    accent_caramel_rgb: "196, 196, 196".to_string(),
                    accent_soft: "rgba(171,171,171,0.14)".to_string(),
                    border_subtle: "rgba(232,232,232,0.06)".to_string(),
                    border_strong: "rgba(232,232,232,0.16)".to_string(),
                    status_running: "#C4C4C4".to_string(),
                    status_pinned: "#6E6E6E".to_string(),
                    danger: "#9C9C9C".to_string(),
                    danger_rgb: "156, 156, 156".to_string(),
                    // Text — primary / secondary / muted grays
                    text_primary: "#E8E8E8".to_string(),
                    text_secondary: "#C0C0C0".to_string(),
                    text_muted: "#8A8A8A".to_string(),
                    // Shadows — soft, tight elevation shadows
                    shadow_window: "0 1px 3px rgba(0, 0, 0, 0.24)".to_string(),
                    shadow_popup: "0 1px 3px rgba(0, 0, 0, 0.30), 0 4px 10px rgba(0, 0, 0, 0.25)".to_string(),
                    shadow_icon_hover: "0 1px 2px rgba(0, 0, 0, 0.20)".to_string(),
                    shadow_card: "0 8px 24px rgba(0, 0, 0, 0.45)".to_string(),
                },
            },
        ],
    }
}

pub fn load_themes() -> ThemesConfig {
    let path = data_dir().join("themes.json");
    let defaults = default_themes();
    match fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<ThemesConfig>(&s) {
            Ok(config) => {
                // Migration: rewrite files whose schema is older than the
                // current version — their color payloads are stale (this is
                // how retired-theme payloads and recolored defaults get
                // replaced). Current-version files are preserved untouched
                // so user edits survive.
                if config.version < THEMES_VERSION {
                    log::info!(
                        "themes.json predates material3-dark — rewriting with defaults"
                    );
                    if let Ok(s) = serde_json::to_string_pretty(&defaults) {
                        let _ = fs::write(&path, s);
                    }
                    return defaults;
                }
                config
            }
            Err(_) => {
                // File exists but is old format or corrupt — overwrite with
                // the current defaults so future loads succeed.
                log::warn!("themes.json was old format or corrupt — rewriting with defaults");
                if let Ok(s) = serde_json::to_string_pretty(&defaults) {
                    let _ = fs::write(&path, s);
                }
                defaults
            }
        },
        Err(_) => {
            // First run — create default themes file
            if let Ok(s) = serde_json::to_string_pretty(&defaults) {
                let _ = fs::write(&path, s);
            }
            defaults
        }
    }
}

pub fn save_themes(config: &ThemesConfig) {
    let path = data_dir().join("themes.json");
    if let Ok(s) = serde_json::to_string_pretty(config) {
        let _ = fs::write(&path, s);
    }
}

pub fn get_active_theme() -> Option<Theme> {
    let config = load_themes();
    config.themes.into_iter().find(|t| t.active)
}

pub fn set_active_theme(name: &str) {
    let mut config = load_themes();
    for theme in &mut config.themes {
        theme.active = theme.name == name;
    }
    save_themes(&config);
}


// ===== First-run tutorial =====
// tutorial_seen.json marks that the looping first-run tutorial has been
// acknowledged. The file is in the wipe list (see lib.rs wipe_configs), so
// "Reset config" (or the -rs flag) brings the tutorial back on next start.
pub fn load_tutorial_seen() -> bool {
    let path = data_dir().join("tutorial_seen.json");
    fs::read_to_string(&path).map(|s| s.trim() == "true").unwrap_or(false)
}

pub fn save_tutorial_seen(seen: bool) {
    let path = data_dir().join("tutorial_seen.json");
    let _ = fs::write(&path, if seen { "true" } else { "false" });
}
