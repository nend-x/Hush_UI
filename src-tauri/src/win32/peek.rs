#![cfg(windows)]
// Window list — enumerate windows of a running app.
//
// Used by the taskbar hover flyout (window switcher).
// We only return hwnd + title + icon_data_url (extracted from process exe).
// No thumbnail capture — that was unreliable cross-process.

use crate::win32::icon::extract_icon_for_path;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, IsWindowVisible, GetWindowThreadProcessId,
    GetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_NAME_FORMAT};
use windows::Win32::Foundation::CloseHandle;

#[derive(serde::Serialize)]
pub struct WindowPreview {
    pub hwnd: usize,
    pub title: String,
    pub icon_data_url: Option<String>,
}

fn resolve_process_full_path(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(handle);
        if ok.is_err() {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

pub fn activate_window_by_hwnd(hwnd_val: isize) -> windows::core::Result<()> {
    let hwnd = HWND(hwnd_val as *mut std::ffi::c_void);
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{
            IsIconic, ShowWindowAsync, SW_RESTORE, SetForegroundWindow,
            AllowSetForegroundWindow,
        };
        let _ = AllowSetForegroundWindow(0xFFFFFFFF);
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
    Ok(())
}

#[derive(serde::Serialize)]
pub struct WindowWithExe {
    pub hwnd: usize,
    pub title: String,
    pub icon_data_url: Option<String>,
    pub exe_path: String,
    pub pid: u32,
}

unsafe extern "system" fn enum_all_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let results = &mut *(lparam.0 as *mut Vec<(HWND, String, u32)>);

    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }

    let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    if ex_style & (WS_EX_TOOLWINDOW.0 as isize) != 0 {
        return BOOL(1);
    }

    let mut title = [0u16; 256];
    let len = GetWindowTextW(hwnd, &mut title);
    if len == 0 {
        return BOOL(1);
    }
    let title_str = String::from_utf16_lossy(&title[..len as usize]);
    if title_str.trim().is_empty() {
        return BOOL(1);
    }

    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));

    results.push((hwnd, title_str, pid));
    BOOL(1)
}

/// Get ALL visible windows with their exe paths.
pub fn get_all_windows_with_exe() -> Vec<WindowWithExe> {
    let mut results: Vec<(HWND, String, u32)> = Vec::new();
    let state_ptr: *mut Vec<(HWND, String, u32)> = &mut results;
    let lparam = LPARAM(state_ptr as isize);

    unsafe {
        let _ = EnumWindows(Some(enum_all_proc), lparam);
    }

    let mut previews = Vec::new();
    for (hwnd, title, pid) in &results {
        let hwnd_usize = hwnd.0 as usize;
        let exe_path = resolve_process_full_path(*pid).unwrap_or_default();
        let icon = if exe_path.is_empty() {
            None
        } else {
            extract_icon_for_path(&exe_path)
        };
        previews.push(WindowWithExe {
            hwnd: hwnd_usize,
            title: title.clone(),
            icon_data_url: icon,
            exe_path,
            pid: *pid,
        });
    }
    previews
}
