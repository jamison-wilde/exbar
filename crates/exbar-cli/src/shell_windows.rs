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
use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GA_PARENT, GetAncestor};
use windows_core::{BOOL, Interface};

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

    // Win11 tabbed Explorer: multiple IShellBrowser entries can share one
    // CabinetWClass HWND (one per tab). Find the active tab by locating
    // the topmost ShellTabWindowClass descendant of the cabinet (z-order
    // top = foreground tab), then match each browser's view-window
    // ancestor chain against it. Falls back to the first HWND match for
    // pre-tab Win10 / when the heuristic fails.
    let active_tab = unsafe { find_active_tab_window(cabinet_hwnd) };
    let mut first_match: Option<IShellBrowser> = None;

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

        let Some(sp) = wba.cast::<IServiceProvider>().ok() else {
            continue;
        };

        let Some(browser) =
            (unsafe { sp.QueryService::<IShellBrowser>(&SID_STopLevelBrowser).ok() })
        else {
            continue;
        };

        if let Some(tab) = active_tab
            && unsafe { browser_view_is_under(&browser, tab) }
        {
            return Some(browser);
        }
        if first_match.is_none() {
            first_match = Some(browser);
        }
    }

    first_match
}

/// Find the topmost (foreground) per-tab container window inside the
/// given cabinet. In Win11's tabbed File Explorer each tab has its own
/// `ShellTabWindowClass` descendant; `EnumChildWindows` enumerates in
/// z-order top-first so the first hit corresponds to the active tab.
///
/// Returns `None` on legacy / single-tab Explorer where the class
/// doesn't exist.
///
/// # Safety
///
/// Calls Win32 window enumeration APIs; safe to invoke from the
/// message-pump thread under the same conditions as
/// `get_shell_browser_for`.
unsafe fn find_active_tab_window(cabinet_hwnd: HWND) -> Option<HWND> {
    struct Search {
        found: Option<HWND>,
    }
    let mut search = Search { found: None };

    unsafe extern "system" fn cb(hwnd: HWND, lparam: windows::Win32::Foundation::LPARAM) -> BOOL {
        // SAFETY: lparam is a `&mut Search` passed below.
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let class = crate::explorer::get_class_name(hwnd);
        if class == "ShellTabWindowClass" {
            search.found = Some(hwnd);
            return BOOL(0); // stop enumeration
        }
        BOOL(1)
    }

    let lparam = windows::Win32::Foundation::LPARAM(&mut search as *mut Search as isize);
    unsafe {
        let _ = EnumChildWindows(Some(cabinet_hwnd), Some(cb), lparam);
    }
    search.found
}

/// True if `target` is an ancestor of `browser`'s active shell view HWND.
/// Used to match each `IShellBrowser` to its tab container.
///
/// # Safety
///
/// Calls into the cross-process `IShellBrowser`/`IShellView` interfaces
/// and `GetAncestor`; same thread/COM contract as
/// `get_shell_browser_for`.
unsafe fn browser_view_is_under(browser: &IShellBrowser, target: HWND) -> bool {
    let Ok(view) = (unsafe { browser.QueryActiveShellView() }) else {
        return false;
    };
    let Ok(view_hwnd) = (unsafe { view.GetWindow() }) else {
        return false;
    };

    let mut cur = view_hwnd;
    // Walk up at most ~16 levels — Explorer's hierarchy is shallow; this
    // bounds the loop in case GetAncestor ever returns a self-cycle.
    for _ in 0..16 {
        if cur.0 == target.0 {
            return true;
        }
        let parent = unsafe { GetAncestor(cur, GA_PARENT) };
        if parent.0.is_null() || parent.0 == cur.0 {
            return false;
        }
        cur = parent;
    }
    false
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

#[cfg(test)]
pub(crate) mod test_mocks {
    use super::ShellBrowser;
    use crate::error::ExbarResult;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use windows::Win32::Foundation::HWND;

    pub struct MockShellBrowser {
        pub navigate_calls: Arc<Mutex<Vec<(isize, PathBuf)>>>,
        pub new_tab_calls: Arc<Mutex<Vec<(isize, PathBuf, u32)>>>,
    }

    impl ShellBrowser for MockShellBrowser {
        fn navigate(&self, explorer: HWND, path: &Path) -> ExbarResult<()> {
            self.navigate_calls
                .lock()
                .unwrap()
                .push((explorer.0 as isize, path.to_path_buf()));
            Ok(())
        }
        fn open_in_new_tab(&self, explorer: HWND, path: &Path, timeout_ms: u32) {
            self.new_tab_calls.lock().unwrap().push((
                explorer.0 as isize,
                path.to_path_buf(),
                timeout_ms,
            ));
        }
    }
}
