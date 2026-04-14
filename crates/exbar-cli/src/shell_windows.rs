//! Cross-process Explorer IShellBrowser enumeration via IShellWindows.
//!
//! Works from any process via COM marshaling — Explorer runs the
//! IShellWindows CLSID service and hands us per-tab IShellBrowser proxies.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    IServiceProvider,
};
use windows::Win32::System::Variant::{
    VARENUM, VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_I4,
};
use windows::Win32::UI::Shell::{
    IShellBrowser, IShellWindows, IWebBrowserApp, SID_STopLevelBrowser, ShellWindows,
};
use windows_core::Interface;

/// Build a VT_I4 VARIANT holding value `n`.
unsafe fn variant_i4(n: i32) -> VARIANT {
    use core::mem::ManuallyDrop;
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VARENUM(VT_I4.0),
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { lVal: n },
            }),
        },
    }
}

/// Try to get `IShellBrowser` for `cabinet_hwnd` by enumerating `IShellWindows`.
///
/// Returns `None` on any failure; navigation buttons won't work.
pub unsafe fn get_shell_browser_for(cabinet_hwnd: HWND) -> Option<IShellBrowser> {
    // Ensure COM is initialised on this thread (idempotent if already done).
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };

    let shell_windows: IShellWindows =
        unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER).ok()? };

    let count = unsafe { shell_windows.Count().ok()? };

    for i in 0..count {
        let index = unsafe { variant_i4(i) };

        let disp = match unsafe { shell_windows.Item(&index).ok() } {
            Some(d) => d,
            None => continue,
        };

        let wba = match disp.cast::<IWebBrowserApp>().ok() {
            Some(w) => w,
            None => continue,
        };

        let win_handle = match unsafe { wba.HWND().ok() } {
            Some(h) => h,
            None => continue,
        };

        if win_handle.0 != cabinet_hwnd.0 as isize {
            continue;
        }

        let sp = wba.cast::<IServiceProvider>().ok()?;

        let browser: IShellBrowser = unsafe { sp.QueryService(&SID_STopLevelBrowser).ok()? };
        return Some(browser);
    }

    None
}

/// Return the list of all Explorer `IShellBrowser`s keyed by their HWND (as isize).
/// Used by the new-tab flow to detect which tab/window is newly created.
pub unsafe fn enumerate_shell_browsers() -> Vec<(isize, IShellBrowser)> {
    let mut out = Vec::new();
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };

    let shell_windows: IShellWindows =
        match unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER) } {
            Ok(s) => s,
            Err(_) => return out,
        };
    let count = match unsafe { shell_windows.Count() } {
        Ok(c) => c,
        Err(_) => return out,
    };

    for i in 0..count {
        let index = unsafe { variant_i4(i) };
        let Some(disp) = (unsafe { shell_windows.Item(&index).ok() }) else {
            continue;
        };
        let Some(wba) = disp.cast::<IWebBrowserApp>().ok() else {
            continue;
        };
        let Ok(hw) = (unsafe { wba.HWND() }) else {
            continue;
        };
        let Some(sp) = wba.cast::<IServiceProvider>().ok() else {
            continue;
        };
        let browser: IShellBrowser = match unsafe { sp.QueryService(&SID_STopLevelBrowser) } {
            Ok(b) => b,
            Err(_) => continue,
        };
        out.push((hw.0, browser));
    }
    out
}
