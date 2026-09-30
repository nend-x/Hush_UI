#![cfg(windows)]
// Icon extraction utilities — convert HICON to PNG base64 data URL.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::core::*;
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON, GetIconInfo, ICONINFO};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, GetObjectW, GetDC, ReleaseDC,
    DeleteObject, GetDIBits, BITMAPINFO, BITMAPINFOHEADER, BITMAP,
    DIB_RGB_COLORS, BI_RGB, RGBQUAD,
};
use image::ImageEncoder;
use base64::Engine;

/// Per-process icon cache: maps an absolute file path to (captured_at, data_url).
///
/// `extract_icon_for_path` is called per visible window on every taskbar
/// refresh (every ~2 s + on every foreground change), and per desktop item
/// on every launcher open. Each call invokes `SHGetFileInfoW` — a shell
/// COM call that goes through `explorer.exe`. When the shell is in a bad
/// post-Modern-Standby state, that call can take seconds per invocation,
/// which directly stalls anyone holding (or waiting for) the AppState lock.
///
/// The cache is keyed by absolute path (the icon for a given exe path
/// doesn't change during a session). The TTL is conservative (5 min) so
/// that any in-session icon change (e.g. an app self-updating) eventually
/// refreshes. The cache is also bounded — once it grows past 256 entries,
/// the oldest 64 are evicted.
static ICON_CACHE: Mutex<Option<IconCache>> = Mutex::new(None);

const ICON_TTL: Duration = Duration::from_secs(300);
const ICON_CACHE_MAX: usize = 256;
const ICON_CACHE_EVICT: usize = 64;

struct IconCache {
    map: HashMap<String, (Instant, Option<String>)>,
}

impl IconCache {
    fn new() -> Self {
        Self { map: HashMap::new() }
    }

    fn get(&self, path: &str) -> Option<(Instant, Option<String>)> {
        self.map.get(path).cloned()
    }

    fn insert(&mut self, path: String, value: Option<String>) {
        if self.map.len() >= ICON_CACHE_MAX {
            // Evict the oldest ICON_CACHE_EVICT entries by capture timestamp.
            let mut entries: Vec<(String, Instant)> =
                self.map.iter().map(|(k, (ts, _))| (k.clone(), *ts)).collect();
            entries.sort_by_key(|(_, ts)| *ts);
            for (k, _) in entries.into_iter().take(ICON_CACHE_EVICT) {
                self.map.remove(&k);
            }
        }
        self.map.insert(path, (Instant::now(), value));
    }
}

/// Extract icon for a file path. Returns PNG data URL (base64) if found.
///
/// Looks up the per-process cache first; on miss, calls the underlying
/// shell API and stores the result (including `None`s — a failed lookup
/// is also cached to avoid repeatedly hitting the shell for a broken
/// shortcut).
pub fn extract_icon_for_path(path: &str) -> Option<String> {
    // Cache lookup
    {
        let mut guard = ICON_CACHE.lock().ok()?;
        let cache = guard.get_or_insert_with(IconCache::new);
        if let Some((ts, cached)) = cache.get(path) {
            if ts.elapsed() < ICON_TTL {
                return cached;
            }
        }
    }
    // Cache miss (or stale) — do the real work
    let result = extract_icon_for_path_uncached(path);
    // Store in cache
    if let Ok(mut guard) = ICON_CACHE.lock() {
        let cache = guard.get_or_insert_with(IconCache::new);
        cache.insert(path.to_string(), result.clone());
    }
    result
}

/// Clear the icon cache (settings-table "Clear icon cache" button).
/// Returns the number of entries dropped.
pub fn clear_cache() -> usize {
    match ICON_CACHE.lock() {
        Ok(mut guard) => {
            let n = guard.as_ref().map(|c| c.map.len()).unwrap_or(0);
            *guard = None;
            n
        }
        Err(_) => 0,
    }
}

fn extract_icon_for_path_uncached(path: &str) -> Option<String> {
    let wide: Vec<u16> = OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let mut shfi = SHFILEINFOW::default();
        let flags = SHGFI_ICON | SHGFI_LARGEICON;
        let res = SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut shfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            flags,
        );

        if res == 0 || shfi.hIcon.is_invalid() {
            return None;
        }

        let result = hicon_to_png(shfi.hIcon);
        let _ = DestroyIcon(shfi.hIcon);
        result
    }
}

/// Convert an HICON to PNG data URL (base64).
pub fn hicon_to_png(hicon: HICON) -> Option<String> {
    unsafe {
        let mut icon_info = ICONINFO::default();
        if GetIconInfo(hicon, &mut icon_info).is_err() {
            return None;
        }

        let mut bmp = BITMAP::default();
        let bmp_handle = if !icon_info.hbmColor.is_invalid() {
            icon_info.hbmColor
        } else {
            icon_info.hbmMask
        };

        if GetObjectW(
            bmp_handle.into(),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bmp as *mut _ as *mut _),
        ) == 0
        {
            let _ = DeleteObject(icon_info.hbmColor.into());
            let _ = DeleteObject(icon_info.hbmMask.into());
            return None;
        }

        let width = bmp.bmWidth;
        let height = if icon_info.hbmColor.is_invalid() {
            bmp.bmHeight / 2
        } else {
            bmp.bmHeight
        };

        if width == 0 || height == 0 {
            let _ = DeleteObject(icon_info.hbmColor.into());
            let _ = DeleteObject(icon_info.hbmMask.into());
            return None;
        }

        let dc = GetDC(None);
        let mem_dc = CreateCompatibleDC(Some(dc));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [RGBQUAD::default()],
        };

        let mut pixels: Vec<u8> = vec![0u8; (width * height * 4) as usize];
        let copied = GetDIBits(
            mem_dc,
            bmp_handle.into(),
            0,
            height as u32,
            Some(pixels.as_mut_ptr() as *mut _),
            &bmi as *const _ as *mut _,
            DIB_RGB_COLORS,
        );

        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(None, dc);
        let _ = DeleteObject(icon_info.hbmColor.into());
        let _ = DeleteObject(icon_info.hbmMask.into());

        if copied == 0 {
            return None;
        }

        // Convert BGRA → RGBA
        for chunk in pixels.chunks_mut(4) {
            let b = chunk[0];
            chunk[0] = chunk[2];
            chunk[2] = b;
        }

        let img = image::RgbaImage::from_raw(width as u32, height as u32, pixels)?;
        let mut buf = std::io::Cursor::new(Vec::new());
        let png_enc = image::codecs::png::PngEncoder::new(&mut buf);
        png_enc
            .write_image(&img, width as u32, height as u32, image::ExtendedColorType::Rgba8)
            .ok()?;

        let b64 = base64::engine::general_purpose::STANDARD
            .encode(buf.into_inner());
        Some(format!("data:image/png;base64,{}", b64))
    }
}
