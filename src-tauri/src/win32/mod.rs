#![cfg(windows)]
// Win32 module — only compiled on Windows target

pub mod window;
pub mod apps;
pub mod shell;
pub mod icon;
pub mod peek;
pub mod subclass;
pub mod winevent;
pub mod hotkey;
pub mod table_mouse;
pub mod dimmer;

// Win-key tap-vs-combo handling lives in `hotkey.rs`. It installs a
// low-level keyboard hook (WH_KEYBOARD_LL) that SWALLOWS Win-down (so the
// OS shell can't start its "Win chord" detection → Start menu can't open),
// fires the launcher toggle on a Win tap, and re-injects Win-down if any
// other key is pressed mid-tap so combos (Win+D, Win+E, …) still work.
//
// `prevent-alt-win-menu` (installed in lib.rs) handles ONLY the Alt case
// (suppressing the focused window's menu bar on Alt release). Its
// `on_released` callback returns `None` for Win — this hook handles Win.
//
// Hook order matters: we install prevent-alt-win-menu FIRST (so it sits at
// the END of the LIFO hook chain) and this hook LAST (so it sits at the
// START of the chain — called first, can swallow Win events before
// prevent-alt-win-menu ever sees them).
