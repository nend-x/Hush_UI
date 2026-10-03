#![cfg(windows)]
// Tray (BETA) — read the notification-area (tray) icons that EXPLORER hosts
// and let the tray widget click them.
//
// How it works (the same battle-tested route every tray utility takes):
//
//   1. Explorer keeps the Shell_NotifyIcon entries in one or more legacy
//      TOOLBAR windows (class ToolbarWindow32) — the main one lives under
//      Shell_TrayWnd → TrayNotifyWnd, the Win11 overflow island keeps its
//      own. Each toolbar button IS one tray icon.
//   2. The toolbar belongs to the explorer process, so TB_GETBUTTON returns
//      a pointer into EXPLORER'S address space (TBBUTTON.dwData → the
//      TRAYDATA header: owner HWND + uID + callback message). We allocate a
//      scratch buffer inside explorer (VirtualAllocEx) and copy the data out
//      with ReadProcessMemory — no DLL injection, no explorer modification,
//      nothing persisted. If UAC/elevations block the cross-process read,
//      enumeration simply returns fewer items (or none) and the widget shows
//      its empty state.
//   3. Icons come from the toolbar's image list (TB_GETIMAGELIST +
//      ImageList_GetIcon, which is handle-safe across processes) and are
//      re-encoded to PNG data URLs via the shared hicon_to_png helper.
//   4. Clicks: the user's own tray menus belong to the tray app, not to us.
//      Rather than rebuilding them (fragile, and they'd lose app-specific
//      styling), we resolve the button rect (TB_GETITEMRECT through the same
//      scratch buffer), map it to screen coordinates and synthesize a real
//      left/right click at that spot with SendInput. A right-click therefore
//      opens the app's REAL context menu. The cursor jumps and snaps back.
//
// x64 only (the whole shell is x64 — TBBUTTON/TRAYDATA layouts below assume
// 8-byte pointers).
//
// Beta caveats, on purpose:
//   - Win10 builds expose the classic toolbar with every icon; Win11 keeps
//     a legacy toolbar for the overflow area, and depending on the exact
//     build some always-visible icons may not be enumerable. The widget
//     degrades to "empty" instead of failing.
//   - The button→icon mapping is re-resolved on every click. If the tray
//     changed between the widget's last refresh and the click, the click
//     may land on a neighbor — the same trade-off every tray clicker makes.

use crate::win32::icon::hicon_to_png;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashMap;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{LPARAM, POINT, RECT, WPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ,
    PROCESS_VM_WRITE,
};
use windows::Win32::UI::Controls::{
    ImageList_GetIcon, ILD_TRANSPARENT, IMAGE_LIST_DRAW_STYLE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ClientToScreen, FindWindowExW, GetClassNameW, GetCursorPos, GetWindowThreadProcessId,
    SendMessageW, SetCursorPos, EnumChildWindows, DestroyIcon, HWND, LRESULT,
};
use windows::Win32::Graphics::Gdi::HIMAGELIST;
// Toolbar messages (WM_USER range) — stable since forever, defined here so
// we don't depend on codegen quirks for constants the crate may not expose.
const TB_GETBUTTON: u32 = 0x0400 + 23;
const TB_BUTTONCOUNT: u32 = 0x0400 + 24;
const TB_GETITEMRECT: u32 = 0x0400 + 29;
const TB_GETIMAGELIST: u32 = 0x0400 + 49;
const TBSTATE_HIDDEN: u8 = 0x08;

/// Icon decode cache — keyed by (owner hwnd, tray uID), the stable pair the
/// shell uses to identify a notification icon. The widget polls while open;
/// without this cache every poll re-decoded every HICON to PNG.
static ICON_CACHE: Lazy<Mutex<HashMap<(usize, u32), Option<String>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// One tray icon as exposed to the widget.
#[derive(Clone, Serialize)]
pub struct TrayItemInfo {
    /// Stable index across one enumeration; the click command takes this.
    pub index: usize,
    /// Owning process exe name (e.g. "Telegram.exe") — the widget's label.
    pub process: String,
    /// PNG data URL of the button icon (None → widget draws a fallback).
    pub icon_data_url: Option<String>,
}

// Remote layouts (x64). Defined locally — the crate's TBBUTTON mirrors the
// control's header, but keeping the exact byte layout we read via RPM in one
// place (with the reserved padding explicit) beats importing a struct whose
// field visibility differs between crate versions.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Tbbutton {
    i_bitmap: i32,
    id_command: i32,
    fs_state: u8,
    fs_style: u8,
    reserved: [u8; 6], // pointer alignment padding on x64
    dw_data: usize,    // → TRAYDATA inside explorer
    i_string: usize,
}

/// Start of the shell's TRAYDATA at TBBUTTON.dwData: the notification icon
/// identity (owner window + its uID).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Traydata {
    owner_hwnd: usize,
    uid: u32,
    callback_msg: u32,
}

#[derive(Clone, Copy)]
struct TrayEntry {
    toolbar: HWND,
    owner_hwnd: usize,
    uid: u32,
    local_index: usize,
}

fn w(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

// ===== Toolbar discovery =====

/// Every ToolbarWindow32 found under the taskbar roots we know about.
fn collect_toolbars() -> Vec<HWND> {
    let mut out: Vec<HWND> = Vec::new();
    unsafe {
        // Main + secondary taskbars (Shell_TrayWnd chain).
        let class_main = w("Shell_TrayWnd");
        let mut after = HWND::default();
        loop {
            let tray = FindWindowExW(
                None,
                Some(after),
                PCWSTR(class_main.as_ptr()),
                PCWSTR::null(),
            );
            match tray {
                Ok(h) if h.0 as usize != 0 => {
                    enum_toolbars_under(h, &mut out);
                    after = h;
                }
                _ => break,
            }
        }
        // Win11 overflow island (hosts the legacy toolbar for hidden icons).
        let class_island = w("TopLevelWindowForOverflowXamlIsland");
        let mut after_island = HWND::default();
        loop {
            let island = FindWindowExW(
                None,
                Some(after_island),
                PCWSTR(class_island.as_ptr()),
                PCWSTR::null(),
            );
            match island {
                Ok(h) if h.0 as usize != 0 => {
                    enum_toolbars_under(h, &mut out);
                    after_island = h;
                }
                _ => break,
            }
        }
    }
    out
}

unsafe extern "system" fn enum_child(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    let list = &mut *(lparam.0 as *mut Vec<HWND>);
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut buf);
    let cls = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
    if cls == "ToolbarWindow32" {
        list.push(hwnd);
    }
    windows::core::BOOL(1)
}

fn enum_toolbars_under(root: HWND, out: &mut Vec<HWND>) {
    unsafe {
        let lparam = LPARAM(out as *mut Vec<HWND> as isize);
        let _ = EnumChildWindows(Some(root), Some(enum_child), lparam);
    }
}

// ===== Cross-process scratch buffer =====

struct Remote {
    handle: windows::Win32::Foundation::HANDLE,
    buf: *mut core::ffi::c_void,
}

impl Remote {
    fn open(pid: u32) -> Option<Remote> {
        unsafe {
            let handle = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION
                    | PROCESS_VM_OPERATION
                    | PROCESS_VM_READ
                    | PROCESS_VM_WRITE,
                false,
                pid,
            )
            .ok()?;
            let buf = VirtualAllocEx(
                handle,
                None,
                4096,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
            .ok()?;
            Some(Remote { handle, buf })
        }
    }

    fn read(&self, addr: usize, out: &mut [u8]) -> bool {
        unsafe {
            ReadProcessMemory(
                self.handle,
                addr as _,
                Some(out.as_mut_ptr() as _),
                out.len(),
                None,
            )
            .is_ok()
        }
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        unsafe {
            let _ = VirtualFreeEx(self.handle, self.buf, 0, MEM_RELEASE);
            let _ = windows::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

use windows::Win32::System::Memory::ReadProcessMemory;

/// pid → exe file name (one snapshot; same pattern as apps.rs).
fn build_pid_exe_map() -> HashMap<u32, String> {
    let mut map = HashMap::new();
    unsafe {
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(s) => s,
            Err(_) => return map,
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                map.insert(
                    entry.th32ProcessID,
                    String::from_utf16_lossy(&entry.szExeFile[..len]),
                );
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = windows::Win32::Foundation::CloseHandle(snap);
    }
    map
}

// ===== Enumeration =====

/// Walk every toolbar button (skipping hidden ones) and resolve the owner
/// identity via explorer's memory. Buttons whose TRAYDATA can't be read
/// (e.g. the scratch buffer was blocked) are skipped — the widget shows
/// what it could read instead of failing wholesale.
fn enumerate_entries() -> Vec<TrayEntry> {
    let mut entries = Vec::new();

    for toolbar in collect_toolbars() {
        let count = unsafe {
            SendMessageW(toolbar, TB_BUTTONCOUNT, WPARAM(0), LPARAM(0)).0 as usize
        };
        // The toolbar belongs to explorer — open the scratch buffer on ITS pid.
        let mut pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(toolbar, Some(&mut pid)) };
        if pid == 0 {
            continue;
        }
        let remote = match Remote::open(pid) {
            Some(r) => r,
            None => continue,
        };

        for i in 0..count {
            let mut tbb = Tbbutton::default();
            unsafe {
                SendMessageW(
                    toolbar,
                    TB_GETBUTTON,
                    WPARAM(i),
                    LPARAM(remote.buf as isize),
                );
            }
            let mut raw = [0u8; std::mem::size_of::<Tbbutton>()];
            if !remote.read(remote.buf as usize, &mut raw) {
                break;
            }
            // Straight byte-copy — the struct IS the remote layout.
            let tbb_bytes = &raw;
            tbb.i_bitmap = i32::from_le_bytes(tbb_bytes[0..4].try_into().unwrap_or([0; 4]));
            tbb.fs_state = tbb_bytes[8];
            tbb.dw_data = usize::from_le_bytes(tbb_bytes[16..24].try_into().unwrap_or([0; 8]));

            if tbb.fs_state & TBSTATE_HIDDEN != 0 {
                continue;
            }
            if tbb.dw_data == 0 {
                continue;
            }

            let mut td_raw = [0u8; std::mem::size_of::<Traydata>()];
            if !remote.read(tbb.dw_data, &mut td_raw) {
                continue;
            }
            let owner_hwnd =
                usize::from_le_bytes(td_raw[0..8].try_into().unwrap_or([0; 8]));
            let uid = u32::from_le_bytes(td_raw[8..12].try_into().unwrap_or([0; 4]));
            if owner_hwnd == 0 {
                continue;
            }

            entries.push(TrayEntry {
                toolbar,
                owner_hwnd,
                uid,
                local_index: i,
            });
        }
    }
    entries
}

/// Public enumeration for the widget: stable index → process + icon PNG.
pub fn enumerate_tray_icons() -> Vec<TrayItemInfo> {
    let entries = enumerate_entries();
    let pid_exe = build_pid_exe_map();
    let mut infos = Vec::new();

    // One scratch buffer per distinct owner process would be over-engineering
    // for the icon pass (icons don't need RPM at all) — only the identity read
    // above does. Icons come from the shared image list.
    for (index, e) in entries.iter().enumerate() {
        let mut pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(HWND(e.owner_hwnd as *mut _), Some(&mut pid)) };
        let process = pid_exe.get(&pid).cloned().unwrap_or_else(|| "unknown".into());

        let icon_data_url = *ICON_CACHE.lock().entry((e.owner_hwnd, e.uid)).or_insert_with(
            || extract_icon(e.toolbar, e.local_index),
        );
        infos.push(TrayItemInfo {
            index,
            process,
            icon_data_url,
        });
    }
    infos
}

/// TB_GETIMAGELIST + ImageList_GetIcon → PNG data URL (handle-safe across
/// processes — the same route AutoHotkey's tray helpers take).
fn extract_icon(toolbar: HWND, local_index: usize) -> Option<String> {
    unsafe {
        let himl = SendMessageW(toolbar, TB_GETIMAGELIST, WPARAM(0), LPARAM(0)).0;
        if himl == 0 {
            return None;
        }
        let hicon = ImageList_GetIcon(
            HIMAGELIST(himl as *mut _),
            local_index as i32,
            IMAGE_LIST_DRAW_STYLE(ILD_TRANSPARENT.0),
        )
        .ok()?;
        let png = hicon_to_png(hicon);
        let _ = DestroyIcon(hicon);
        png
    }
}

// ===== Click forwarding =====

/// Synthesize a left (or right) click on the tray button at `index`.
/// The cursor jumps to the button, clicks, and snaps back — tray apps
/// position their menus at the cursor.
pub fn click_tray_icon(index: usize, right: bool) -> Result<(), String> {
    let entries = enumerate_entries();
    let e = entries.get(index).ok_or("tray index out of range")?;

    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(e.toolbar, Some(&mut pid)) };
    if pid == 0 {
        return Err("toolbar pid unavailable".into());
    }
    let remote = Remote::open(pid).ok_or("cannot open explorer for the rect read")?;

    unsafe {
        SendMessageW(
            e.toolbar,
            TB_GETITEMRECT,
            WPARAM(e.local_index),
            LPARAM(remote.buf as isize),
        );
    }
    let mut raw = [0u8; std::mem::size_of::<RECT>()];
    if !remote.read(remote.buf as usize, &mut raw) {
        return Err("TB_GETITEMRECT read failed".into());
    }
    let rect = RECT {
        left: i32::from_le_bytes(raw[0..4].try_into().unwrap_or([0; 4])),
        top: i32::from_le_bytes(raw[4..8].try_into().unwrap_or([0; 4])),
        right: i32::from_le_bytes(raw[8..12].try_into().unwrap_or([0; 4])),
        bottom: i32::from_le_bytes(raw[12..16].try_into().unwrap_or([0; 4])),
    };
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return Err("empty button rect".into());
    }

    let mut point = POINT {
        x: (rect.left + rect.right) / 2,
        y: (rect.top + rect.bottom) / 2,
    };
    unsafe {
        ClientToScreen(e.toolbar, &mut point).map_err(|e| e.to_string())?;

        let mut orig = POINT::default();
        let _ = GetCursorPos(&mut orig);
        let _ = SetCursorPos(point.x, point.y);
        std::thread::sleep(std::time::Duration::from_millis(50));

        let (down, up) = if right {
            (MOUSEEVENTF_RIGHTDOWN.0, MOUSEEVENTF_RIGHTUP.0)
        } else {
            (MOUSEEVENTF_LEFTDOWN.0, MOUSEEVENTF_LEFTUP.0)
        };
        send_mouse(down);
        std::thread::sleep(std::time::Duration::from_millis(60));
        send_mouse(up);
        std::thread::sleep(std::time::Duration::from_millis(40));

        let _ = SetCursorPos(orig.x, orig.y);
    }
    Ok(())
}

fn send_mouse(flags: u32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        let sent = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        if sent != 1 {
            log::warn!("tray click: SendInput sent {sent}");
        }
    }
}
