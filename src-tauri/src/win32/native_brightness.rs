// Native brightness — REAL display brightness via the OS/hardware.
//
// Two paths, both applied where available (a machine can have a laptop
// panel AND external monitors):
//
//   1. WMI (internal panels / laptops): root\WMI,
//      WmiMonitorBrightness.CurrentBrightness for reads and
//      WmiMonitorBrightnessMethods.WmiSetBrightness for writes. This is
//      exactly what Windows' own brightness slider drives.
//
//   2. DDC/CI (external monitors): the Monitor Configuration API
//      (dxva2) — GetPhysicalMonitorsFromHMONITOR → Get/SetMonitorBrightness
//      over the VCP Brightness feature. This is what monitor OSDs and
//      utilities like clickmonitorddc drive.
//
// The systemless software dim overlay (win32/dimmer.rs) is untouched and
// both compose — the overlay sits on top of whatever the hardware shows.
//
// Unlike the dimmer, no persisted level: native brightness IS the
// monitor's own state — set it once and it survives restarts, same as
// the keyboard brightness keys.

#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod imp {
    use windows::core::{BSTR, PCWSTR};
    use windows::Win32::Foundation::{HANDLE, LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoSetProxyBlanket, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED, EOAC_NONE, RPC_C_AUTHN_LEVEL_PKT_PRIVACY,
        RPC_C_IMP_LEVEL_IMPERSONATE,
    };
    use windows::Win32::System::Rpc::{RPC_C_AUTHN_DEFAULT, RPC_C_AUTHZ_DEFAULT};
    use windows::Win32::System::Variant::{VARIANT, VT_UI1, VT_UI4};
    use windows::Win32::System::Wmi::{
        IWbemClassObject, IWbemLocator, IWbemServices, WbemLocator, WBEM_FLAG_FORWARD_ONLY,
        WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_GENERIC_FLAG_TYPE, WBEM_INFINITE,
    };

    /// Read the current native brightness across all monitors.
    ///
    /// Returns None when this machine exposes no brightness control at all
    /// (no WMI monitor instances, no DDC/CI-capable monitors) — the widget
    /// hides its native row in that case. When several monitors report
    /// levels, the maximum wins (a single slider value that won't
    /// surprise anyone).
    pub fn get_brightness() -> Option<f64> {
        let mut best: Option<f64> = None;
        let mut track = |pct: f64| {
            if pct.is_finite() && (0.0..=100.0).contains(&pct) && best.is_none_or(|b| pct > b) {
                best = Some(pct);
            }
        };

        for pct in wmi_current_levels() {
            track(pct);
        }
        for (_, current, _, max) in ddc_read_all() {
            if max > 0.0 {
                track(((current / max) * 100.0).clamp(0.0, 100.0));
            }
        }

        best
    }

    /// Set native brightness on every controllable monitor (0.0..=1.0).
    /// Best-effort per monitor: one unsupported panel never blocks the
    /// others. Returns how many outputs accepted the value.
    pub fn set_brightness(level: f64) -> usize {
        let level = level.clamp(0.0, 1.0);
        let mut applied = 0;

        // WMI wants a 0-100 byte.
        let pct100 = (level * 100.0).round() as u8;
        if wmi_set_brightness(pct100) {
            applied += 1;
        }

        // DDC/CI wants absolute units in [min, max] (usually [0, 100] but
        // some panels expose a different range).
        for (handle, _, _, min, max) in ddc_handles() {
            let value = (min + (max - min) * level).round() as u32;
            let ok = unsafe { windows::Win32::Devices::Display::SetMonitorBrightness(handle, value) };
            if ok == 0 {
                applied += 1;
            }
            unsafe {
                let _ = windows::Win32::Devices::Display::DestroyPhysicalMonitor(handle);
            }
        }

        applied
    }

    // ===== WMI (internal panels / laptops) =====

    /// All WMI monitor brightness levels (percent 0-100), empty when the
    /// query returns nothing (desktops without a laptop panel, access
    /// denied, WMI unavailable).
    fn wmi_current_levels() -> Vec<f64> {
        with_wmi_services(|services| unsafe {
            let mut out = Vec::new();
            let enumerator = services.ExecQuery(
                &BSTR::from("WQL"),
                &BSTR::from("SELECT CurrentBrightness FROM WmiMonitorBrightness"),
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None,
            )?;

            let mut batch: [Option<IWbemClassObject>; 1] = [None];
            let mut returned = 0u32;
            loop {
                enumerator
                    .Next(WBEM_INFINITE, &mut batch, &mut returned)
                    .ok()?;
                if returned == 0 {
                    break;
                }
                for obj in batch.iter_mut().flatten() {
                    let name = windows::core::HSTRING::from("CurrentBrightness");
                    let mut v = VARIANT::default();
                    if obj.Get(&name, 0, &mut v, None, None).is_ok() {
                        out.push(variant_f64(&v));
                    }
                }
            }
            Ok(out)
        })
        .unwrap_or_default()
    }

    /// WmiSetBrightness on every WMI monitor instance. True if at least
    /// one instance accepted the call.
    fn wmi_set_brightness(pct: u8) -> bool {
        with_wmi_services(|services| unsafe {
            let enumerator = services.ExecQuery(
                &BSTR::from("WQL"),
                &BSTR::from("SELECT * FROM WmiMonitorBrightnessMethods"),
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None,
            )?;

            // The in-parameter instance is built once from the class's
            // method signature, then reused for every instance path.
            let mut params: Option<IWbemClassObject> = None;
            let mut applied_any = false;

            let mut batch: [Option<IWbemClassObject>; 1] = [None];
            let mut returned = 0u32;
            loop {
                enumerator
                    .Next(WBEM_INFINITE, &mut batch, &mut returned)
                    .ok()?;
                if returned == 0 {
                    break;
                }
                for obj in batch.iter_mut().flatten() {
                    let Some(path) = variant_bstr(obj, "__PATH") else { continue };

                    if params.is_none() {
                        let Some(class_name) = variant_bstr(obj, "__CLASS") else { continue };
                        let mut class_obj: Option<IWbemClassObject> = None;
                        services
                            .GetObject(&class_name, WBEM_GENERIC_FLAG_TYPE(0), None, Some(&mut class_obj), None)?;
                        let Some(class_obj) = class_obj else { continue };
                        let mut sig: Option<IWbemClassObject> = None;
                        let mut out_sig: Option<IWbemClassObject> = None;
                        class_obj
                            .GetMethod(&windows::core::HSTRING::from("WmiSetBrightness"), 0, &mut sig, &mut out_sig)?;
                        let Some(sig) = sig else { continue };
                        let instance = sig.SpawnInstance(0)?;
                        // Timeout: uint32, Brightness: uint8.
                        instance
                            .Put(
                                &windows::core::HSTRING::from("Timeout"),
                                0,
                                &VARIANT::from(0u32),
                                VT_UI4.0 as i32,
                            )?;
                        instance
                            .Put(
                                &windows::core::HSTRING::from("Brightness"),
                                0,
                                &VARIANT::from(pct),
                                VT_UI1.0 as i32,
                            )?;
                        params = Some(instance);
                    }

                    if let Some(p) = &params {
                        if services
                            .ExecMethod(&path, &BSTR::from("WmiSetBrightness"), WBEM_GENERIC_FLAG_TYPE(0), None, Some(p), None, None)
                            .is_ok()
                        {
                            applied_any = true;
                        }
                    }
                }
            }
            Ok(applied_any)
        })
        .unwrap_or(false)
    }

    /// Run `f` with a connected ROOT\WMI IWbemServices on a COM-initialized
    /// thread. Tauri commands arrive on worker threads; MTA init is cheap
    /// and repeatable per thread.
    fn with_wmi_services<T>(
        f: impl FnOnce(&IWbemServices) -> windows::core::Result<T>,
    ) -> windows::core::Result<T> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

            let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
            let services = locator.ConnectServer(
                &BSTR::from("ROOT\\WMI"),
                &BSTR::new(),
                &BSTR::new(),
                &BSTR::new(),
                0,
                &BSTR::new(),
                None,
            )?;

            // Unmarshaled WMI proxies need an explicit security blanket or
            // calls fail with E_ACCESSDENIED.
            let _ = CoSetProxyBlanket(
                &services,
                RPC_C_AUTHN_DEFAULT as u32,
                RPC_C_AUTHZ_DEFAULT,
                PCWSTR::null(),
                RPC_C_AUTHN_LEVEL_PKT_PRIVACY,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            );

            f(&services)
        }
    }

    fn variant_f64(v: &VARIANT) -> f64 {
        // VariantToUInt32/Int32/UInt16 coerce between the numeric VTs, so a
        // small ladder covers VT_UI1 (uint8) and friends.
        if let Ok(x) = u32::try_from(v) {
            return x as f64;
        }
        if let Ok(x) = i32::try_from(v) {
            return x as f64;
        }
        if let Ok(x) = u16::try_from(v) {
            return x as f64;
        }
        if let Ok(x) = f64::try_from(v) {
            return x;
        }
        f64::NAN
    }

    fn variant_bstr(obj: &IWbemClassObject, name: &str) -> Option<BSTR> {
        let hname = windows::core::HSTRING::from(name);
        let mut v = VARIANT::default();
        unsafe { obj.Get(&hname, 0, &mut v, None, None) }.ok()?;
        BSTR::try_from(&v).ok()
    }

    // ===== DDC/CI (external monitors) =====

    /// (description, current, min, max) for every DDC/CI-capable monitor.
    fn ddc_read_all() -> Vec<(String, f64, f64, f64)> {
        ddc_handles()
            .into_iter()
            .map(|(handle, name, current, min, max)| {
                // Read-only path — close the handles we opened.
                unsafe {
                    let _ = windows::Win32::Devices::Display::DestroyPhysicalMonitor(handle);
                }
                (name, current, min, max)
            })
            .collect()
    }

    /// Open every physical monitor: (HANDLE, description, current, min,
    /// max). Handles stay OPEN — `set_brightness` consumes them to apply
    /// values; callers must DestroyPhysicalMonitor when done. Empty when
    /// no DDC/CI-capable monitor is present.
    fn ddc_handles() -> Vec<(HANDLE, String, f64, f64, f64)> {
        use windows::Win32::Devices::Display::{
            GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
            GetPhysicalMonitorsFromHMONITOR,
        };

        let mut found: Vec<HMONITOR> = Vec::new();

        unsafe extern "system" fn enum_cb(
            monitor: HMONITOR,
            _hdc: HDC,
            _rect: *mut RECT,
            state: LPARAM,
        ) -> windows::core::BOOL {
            let list = state.0 as *mut Vec<HMONITOR>;
            unsafe {
                (*list).push(monitor);
            }
            true.into()
        }

        unsafe {
            let state = LPARAM(&mut found as *mut _ as isize);
            let _ = EnumDisplayMonitors(None, None, Some(enum_cb), state);
        }

        let mut out = Vec::new();
        for monitor in found {
            unsafe {
                let mut count = 0u32;
                if GetNumberOfPhysicalMonitorsFromHMONITOR(monitor, &mut count).is_err()
                    || count == 0
                {
                    continue;
                }
                let mut monitors =
                    vec![windows::Win32::Devices::Display::PHYSICAL_MONITOR::default(); count as usize];
                if GetPhysicalMonitorsFromHMONITOR(monitor, &mut monitors).is_err() {
                    continue;
                }
                for pm in &monitors {
                    // Copy the description out first — PHYSICAL_MONITOR is
                    // a packed struct, so field references are unaligned.
                    let desc_arr = pm.szPhysicalMonitorDescription;
                    let desc: Vec<u16> = desc_arr
                        .iter()
                        .take_while(|&&c| c != 0)
                        .map(|&c| c)
                        .collect();
                    let name = String::from_utf16_lossy(&desc);

                    let mut min = 0u32;
                    let mut current = 0u32;
                    let mut max = 0u32;
                    if GetMonitorBrightness(pm.hPhysicalMonitor, &mut min, &mut current, &mut max) == 0
                    {
                        out.push((pm.hPhysicalMonitor, name, current as f64, min as f64, max as f64));
                    } else {
                        // Not DDC/CI-capable — close the handle we opened.
                        let _ = windows::Win32::Devices::Display::DestroyPhysicalMonitor(pm.hPhysicalMonitor);
                    }
                }
            }
        }
        out
    }
}

#[cfg(windows)]
pub use imp::{get_brightness, set_brightness};

#[cfg(not(windows))]
pub use not_windows::*;

#[cfg(not(windows))]
mod not_windows {
    pub fn get_brightness() -> Option<f64> {
        None
    }
    pub fn set_brightness(_level: f64) -> usize {
        0
    }
}
