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

/// Construct a VT_I4 VARIANT wrapping an i32. Used for
/// IShellWindows::Item(vIndex) calls.
///
/// # Safety
///
/// The returned VARIANT is a simple numeric type (VT_I4) — no allocation
/// or interface pointer needs releasing. The caller may use it directly
/// as an input argument; Windows does not modify it.
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
///
/// # Safety
///
/// Must be called from an STA thread. Uses Win32 COM APIs
/// (`CoCreateInstance`, `IShellWindows` enumeration) that assume the
/// thread has already initialised COM via `CoInitializeEx`, which is
/// idempotently ensured on entry.
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
///
/// # Safety
///
/// Same contract as `get_shell_browser_for`: must be called from an STA
/// thread; the function idempotently ensures COM is initialised.
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

// ── SP3: ShellBrowser trait ─────────────────────────────────────────────────

use crate::error::{ExbarError, ExbarResult};
use std::path::Path;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{SBSP_SAMEBROWSER, SHParseDisplayName, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_SHOWNORMAL, WM_KEYDOWN, WM_KEYUP};
use windows_core::PCWSTR;

const VK_CONTROL: usize = 0x11;
const VK_T_KEY: usize = 0x54;

pub trait ShellBrowser: Send + Sync {
    /// Navigate the Explorer window identified by `explorer` to `path` (same-window).
    fn navigate(&self, explorer: HWND, path: &Path) -> ExbarResult<()>;

    /// Send Ctrl+T to `explorer`; poll IShellWindows for a new browser; on
    /// new-tab appearance, BrowseObject to `path`. On timeout or if
    /// `timeout_ms == 0`, fall back to ShellExecuteW opening a fresh Explorer
    /// window.
    fn open_in_new_tab(&self, explorer: HWND, path: &Path, timeout_ms: u32);

    // TODO(SP4): `active_explorer` / `set_active_explorer` don't semantically
    // belong on a trait named `ShellBrowser` — the trait's job is navigation.
    // Active-Explorer tracking is app-level state that `Win32Shell` today
    // delegates to module-level statics in `toolbar.rs`. SP4 (state
    // consolidation) should lift this pair off `ShellBrowser` and onto the
    // forthcoming `App` struct.

    /// The most-recently-activated Explorer (CabinetWClass) HWND.
    fn active_explorer(&self) -> Option<HWND>;

    /// Record the active Explorer HWND. Called by the foreground WinEvent hook.
    fn set_active_explorer(&self, hwnd: HWND);
}

#[derive(Default)]
pub struct Win32Shell;

impl Win32Shell {
    pub fn new() -> Self {
        Self
    }
}

impl ShellBrowser for Win32Shell {
    fn navigate(&self, explorer: HWND, path: &Path) -> ExbarResult<()> {
        // SAFETY: get_shell_browser_for is an unsafe fn requiring STA + COM init;
        // the wndproc / message-pump thread owns both.
        let browser = unsafe { get_shell_browser_for(explorer) }.ok_or_else(|| {
            ExbarError::Config(format!("no IShellBrowser for HWND {:?}", explorer.0))
        })?;

        let path_str = path.to_string_lossy();
        let wide: Vec<u16> = path_str.encode_utf16().chain(std::iter::once(0)).collect();
        let pcwstr = PCWSTR(wide.as_ptr());

        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        // SAFETY: SHParseDisplayName writes a PIDL we own; freed below regardless.
        unsafe {
            SHParseDisplayName(pcwstr, None, &mut pidl, 0, None)?;
        }
        if pidl.is_null() {
            return Err(ExbarError::Config(format!(
                "SHParseDisplayName returned null PIDL for {:?}",
                path_str
            )));
        }

        // SAFETY: BrowseObject reads the PIDL as input; ownership retained here.
        let browse_result = unsafe {
            browser
                .BrowseObject(pidl, SBSP_SAMEBROWSER)
                .map_err(ExbarError::from)
        };

        // SAFETY: balance the SHParseDisplayName allocation.
        unsafe {
            CoTaskMemFree(Some(pidl as *const core::ffi::c_void));
        }

        browse_result
    }

    #[allow(clippy::too_many_lines)]
    fn open_in_new_tab(&self, explorer: HWND, path: &Path, timeout_ms: u32) {
        let path_str_owned = path.to_string_lossy().into_owned();
        if timeout_ms == 0 {
            open_in_new_window(&path_str_owned);
            return;
        }

        // SAFETY: enumerate_shell_browsers is unsafe; STA + COM satisfied.
        let before: std::collections::HashSet<isize> = unsafe { enumerate_shell_browsers() }
            .into_iter()
            .map(|(h, _)| h)
            .collect();

        unsafe {
            crate::warn_on_err!(PostMessageW(
                Some(explorer),
                WM_KEYDOWN,
                WPARAM(VK_CONTROL),
                LPARAM(0)
            ));
            crate::warn_on_err!(PostMessageW(
                Some(explorer),
                WM_KEYDOWN,
                WPARAM(VK_T_KEY),
                LPARAM(0)
            ));
            crate::warn_on_err!(PostMessageW(
                Some(explorer),
                WM_KEYUP,
                WPARAM(VK_T_KEY),
                LPARAM(0)
            ));
            crate::warn_on_err!(PostMessageW(
                Some(explorer),
                WM_KEYUP,
                WPARAM(VK_CONTROL),
                LPARAM(0)
            ));
        }

        let start = std::time::Instant::now();
        loop {
            if start.elapsed() >= std::time::Duration::from_millis(u64::from(timeout_ms)) {
                log::info!("open_in_new_tab: timeout → falling back to new window");
                open_in_new_window(&path_str_owned);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
            // SAFETY: same as above.
            let current = unsafe { enumerate_shell_browsers() };
            for (hwnd, browser) in current {
                if !before.contains(&hwnd) {
                    let wide: Vec<u16> = path_str_owned
                        .encode_utf16()
                        .chain(std::iter::once(0))
                        .collect();
                    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
                    // SAFETY: SHParseDisplayName writes a PIDL we free below.
                    let parsed = unsafe {
                        SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None)
                    };
                    if parsed.is_ok() && !pidl.is_null() {
                        // SAFETY: BrowseObject same as navigate's contract.
                        let br = unsafe { browser.BrowseObject(pidl, SBSP_SAMEBROWSER) };
                        // SAFETY: free PIDL regardless of outcome.
                        unsafe {
                            CoTaskMemFree(Some(pidl as *const core::ffi::c_void));
                        }
                        if let Err(e) = br {
                            log::error!("open_in_new_tab: BrowseObject failed: {e}");
                            open_in_new_window(&path_str_owned);
                        }
                    } else {
                        log::error!(
                            "open_in_new_tab: SHParseDisplayName failed for {path_str_owned}"
                        );
                        open_in_new_window(&path_str_owned);
                    }
                    return;
                }
            }
        }
    }

    fn active_explorer(&self) -> Option<HWND> {
        // Delegate to the module-level static in toolbar.rs (the foreground
        // WinEvent hook writes to it; SP4 will consolidate fully).
        crate::toolbar::get_active_explorer()
    }

    fn set_active_explorer(&self, hwnd: HWND) {
        crate::toolbar::set_active_explorer(hwnd);
    }
}

fn open_in_new_window(path: &str) {
    let quoted = format!("\"{path}\"");
    let path_wide: Vec<u16> = quoted.encode_utf16().chain(std::iter::once(0)).collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    let exe: Vec<u16> = "explorer.exe"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let _ = ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(exe.as_ptr()),
            PCWSTR(path_wide.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}
