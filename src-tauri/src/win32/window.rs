#![cfg(windows)]
// Window helpers — topmost + WS_EX_NOACTIVATE + WS_EX_TOOLWINDOW + AppBar

use tauri::{Manager, WebviewWindow};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_FRAMECHANGED, SWP_SHOWWINDOW, GWL_EXSTYLE, GWL_STYLE,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_LAYERED, WS_EX_TRANSPARENT,
    WS_OVERLAPPEDWINDOW, WS_POPUP, WS_VISIBLE,
    IsWindowVisible,};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK, DWMWA_BORDER_COLOR};

fn hwnd_of(window: &WebviewWindow) -> HWND {
    window.hwnd().expect("hwnd() failed")
}

// ===== Pie picker overlay show/hide via DWM cloaking =====
//
// The picker is a fullscreen transparent WebView2. Hiding/showing it with
// ShowWindow makes WebView2's composition surface tear down/re-attach and
// flash its default background for a frame — the intermittent light-blue
// flicker on open. Instead the window is created visible ONCE and is
// thereafter toggled with DWM cloaking:
//   hidden → WS_EX_TRANSPARENT (click-through) + DWMWA_CLOAK (composed
//            offscreen — surface stays alive, nothing to re-attach)
//   shown  → uncloak, clear WS_EX_TRANSPARENT
// No layered redirection is involved (WS_EX_LAYERED on a WebView2 surface
// is itself a flicker source), so there is nothing left to flash.
pub fn set_picker_visible(window: &WebviewWindow, visible: bool) -> windows::core::Result<()> {
    let hwnd = hwnd_of(window);
    // 0.3.2: pair the cloak toggle with the WebView2 controller toggle
    // (ICoreWebView2Controller::SetIsVisible via wry's Webview::hide/show).
    // Cloaking only moves the window offscreen — the controller stayed
    // IsVisible=true and kept feeding the WebView2 GPU process forever
    // (measured ~14% CPU with the shell closed). Resume BEFORE uncloaking
    // so the surface re-presents its last frame while still hidden; suspend
    // AFTER cloaking so nothing visible can flash.
    if visible {
        resume_picker_webview(window);
    }
    let res = set_picker_visible_impl(visible, hwnd);
    if !visible {
        if let Some(wv) = window.app_handle().get_webview_window(window.label()) {
            let _ = wv.as_ref().hide();
        }
    }
    res
}

/// Resume ONLY the picker's WebView2 controller while the window stays
/// cloaked (0.3.3 show handshake). Resuming re-presents the last frame and
/// lets the page lay out + paint the NEW pie anchor into the still-invisible
/// surface, so the subsequent uncloak shows the correct frame immediately —
/// the stale "pie at the old anchor" frame is never on screen.
pub fn resume_picker_webview(window: &WebviewWindow) {
    if let Some(wv) = window.app_handle().get_webview_window(window.label()) {
        let _ = wv.as_ref().show();
    }
}

fn set_picker_visible_impl(
    visible: bool,
    hwnd: HWND,
) -> windows::core::Result<()> {
    unsafe {
        // NOTE: this used to toggle WS_EX_LAYERED + SetLayeredWindowAttributes
        // alpha 0/255. Redirecting a WebView2 child surface through layered
        // windows makes DWM drop/recompose the WebView2's own swapchain on
        // every toggle — that recomposition is the residual light-blue
        // flicker on open. DWM cloaking keeps the window fully composed
        // offscreen instead: no layered redirection, no surface teardown, no
        // flash.
        let mut ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        ex &= !(WS_EX_LAYERED.0 as isize);
        if visible {
            ex &= !(WS_EX_TRANSPARENT.0 as isize);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);
            let cloak: i32 = 0;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_CLOAK,
                &cloak as *const _ as *const _,
                std::mem::size_of::<i32>() as u32,
            );
            let _ = SetWindowPos(
                hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        } else {
            ex |= WS_EX_TRANSPARENT.0 as isize;
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);
            // Cloak BEFORE removing from the topmost z-order so the window
            // never paints a stale frame while transitioning out.
            let cloak: i32 = 1;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_CLOAK,
                &cloak as *const _ as *const _,
                std::mem::size_of::<i32>() as u32,
            );
        }
    }
    Ok(())
}

// ===== WebView2 render suspension (0.3.2) =====
//
// ShowWindow(SW_HIDE) and DWM cloaking only hide the WINDOW — the WebView2
// controller inside it kept IsVisible=true, and wry even boots every webview
// visible (WebViewAttributes::default().visible == true) regardless of the
// window's visible:false config. Net effect: every surface this shell owns
// kept compositing through the WebView2 GPU process even with everything
// "closed" — measured ~14% CPU at idle (rust side ~1%). The documented
// off-switch is controller.SetIsVisible(false), exposed by wry as
// Webview::hide()/show(). Every show/hide path now goes through these two
// helpers so the window-level and controller-level toggles stay paired:
//   hide → window first, then suspend the webview (nothing visible changes,
//          then the renderer stops producing frames)
//   show → resume the webview first (it re-presents its last frame
//          instantly, so the reveal never shows a dead surface), then
//          reveal the window
// Suspending a page freezes its rendering but NOT its page load or JS —
// hidden pages stay warm, so opening feels identical to before.
pub fn show_window(win: &WebviewWindow) {
    // AsRef<Webview> — the webview-level show() calls controller.SetIsVisible(true).
    win.as_ref().show().ok();
    let _ = win.show();
}

pub fn hide_window(win: &WebviewWindow) {
    let _ = win.hide();
    win.as_ref().hide().ok();
}

// DWMWA_COLOR_NONE (0xFFFFFFFE) tells Windows 11 to draw NO window border at
// all. Defined locally — not every windows crate version re-exports it.
const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;

/// Strip the Windows 11 DWM border from a window's whole rectangle.
///
/// Win11 paints a faint 1px border around the WINDOW RECT of every top-level
/// window — independent of the DWM drop shadow, so `shadow: false` (set on
/// every window here) does NOT remove it. All Hush_UI windows are transparent
/// rectangles hosting rounded panels, so that square hairline floats around
/// the rounded content: the "almost invisible square frame" on every window.
/// Windows 10 draws no such border and rejects the attribute (ignored).
pub fn remove_dwm_border(window: &WebviewWindow) {
    let hwnd = hwnd_of(window);
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &DWMWA_COLOR_NONE as *const _ as *const _,
            std::mem::size_of::<u32>() as u32,
        );
    }
}

pub fn apply_no_activate(window: &WebviewWindow) -> windows::core::Result<()> {
    let hwnd = hwnd_of(window);
    unsafe {
        // === DWM attributes — fully remove the caption strip ===
        use windows::Win32::Graphics::Dwm::{
            DwmSetWindowAttribute,
            DWMWA_NCRENDERING_POLICY, DWMNCRENDERINGPOLICY, DWMNCRP_DISABLED,
            DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_BORDER_COLOR,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
        };

        // 1. Disable DWM non-client rendering
        let policy = DWMNCRENDERINGPOLICY(DWMNCRP_DISABLED.0);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_NCRENDERING_POLICY,
            &policy as *const _ as *const _,
            std::mem::size_of::<DWMNCRENDERINGPOLICY>() as u32,
        );

        // 2. Set caption color to espresso dark (BGR format: 0x00BBGGRR)
        let dark_colorref: u32 = 0x001A2A3A; // B=26, G=42, R=58
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR,
            &dark_colorref as *const _ as *const _,
            std::mem::size_of::<u32>() as u32,
        );

        // 3. Caption text color
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_TEXT_COLOR,
            &dark_colorref as *const _ as *const _,
            std::mem::size_of::<u32>() as u32,
        );

        // 4. Border color — NONE. The old espresso COLORREF was still a
        // square hairline hugging the WINDOW RECT (not the rounded panel):
        // the "ghost frame". COLOR_NONE removes the Win11 border entirely
        // (Win10 has no per-window border and rejects the attribute).
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &DWMWA_COLOR_NONE as *const _ as *const _,
            std::mem::size_of::<u32>() as u32,
        );

        // 5. Immersive dark mode
        let dark: i32 = 1;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const _,
            std::mem::size_of::<i32>() as u32,
        );

        // === Window styles ===
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new_ex = ex
            | (WS_EX_NOACTIVATE.0 as isize)
            | (WS_EX_TOOLWINDOW.0 as isize)
            | (WS_EX_TOPMOST.0 as isize);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_ex);

        // Strip WS_OVERLAPPEDWINDOW (caption + sysmenu + minmax + thickframe) and
        // replace with WS_POPUP. This removes the Windows-drawn caption buttons.
        //
        // WS_VISIBLE is NOT force-set here: this helper runs at startup on
        // windows that are still hidden (the tables overlays) — OR-ing
        // WS_VISIBLE into the style made a hidden fullscreen overlay window
        // VISIBLE-but-transparent, which silently swallowed every mouse
        // click on the desktop until the user opened + closed the quick
        // menu (the only path that called hide() for real).
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let mut new_style = (style & !(WS_OVERLAPPEDWINDOW.0 as isize)) | (WS_POPUP.0 as isize);
        if IsWindowVisible(hwnd).as_bool() {
            new_style |= WS_VISIBLE.0 as isize;
        }
        SetWindowLongPtrW(hwnd, GWL_STYLE, new_style);

        // === Set window region to client area only ===
        // This physically clips away the non-client area (caption bar) so
        // Windows cannot draw it even if it tries.
        use windows::Win32::Graphics::Gdi::{CreateRectRgn, SetWindowRgn};
        use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
        use windows::Win32::Foundation::RECT;
        let mut rc = RECT::default();
        if GetClientRect(hwnd, &mut rc).is_ok() {
            let rgn = CreateRectRgn(rc.left, rc.top, rc.right, rc.bottom);
            let _ = SetWindowRgn(hwnd, Some(rgn), true);
        }

        // === Install subclass to intercept WM_NCACTIVATE / WM_NCPAINT ===
        // This is the nuclear option — prevents Windows from drawing the caption
        // bar even when the window receives focus via click on empty areas.
        let _ = crate::win32::subclass::install_caption_suppression(hwnd);

        // Force topmost refresh with frame change.
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0, 0, 0, 0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_FRAMECHANGED,
        );
    }
    Ok(())
}

