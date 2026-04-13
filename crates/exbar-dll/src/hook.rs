//! CBT hook callback — injected into every process via SetWindowsHookExW.
//!
//! On HCBT_ACTIVATE for CabinetWClass windows:
//!   - Updates ACTIVE_EXPLORER so button clicks navigate the foreground Explorer.
//!   - Creates the single global toolbar on first ready Explorer.

use std::collections::HashSet;
use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{CallNextHookEx, HCBT_ACTIVATE, SetTimer, KillTimer};
use windows::Win32::System::Com::{
    CoInitializeEx, CoCreateInstance, CLSCTX_LOCAL_SERVER, IServiceProvider, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{IShellBrowser, IShellWindows, IWebBrowserApp, SID_STopLevelBrowser};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_I4, VARENUM};
use windows_core::Interface;

use crate::explorer;

// ── Retry state ────────────────────────────────────────────────────────────────

/// HWNDs that failed slot detection and need retry.
static PENDING_RETRY: Mutex<Option<HashSet<isize>>> = Mutex::new(None);
/// Timer ID for retry attempts.
const RETRY_TIMER_ID: usize = 0xBAD1;
/// Retry interval in milliseconds.
const RETRY_INTERVAL_MS: u32 = 500;
/// Max retries before giving up on a window.
const MAX_RETRIES: u32 = 20; // 10 seconds total
static RETRY_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn add_pending(hwnd: HWND) {
    let mut guard = PENDING_RETRY.lock().unwrap();
    guard.get_or_insert_with(HashSet::new).insert(hwnd.0 as isize);
}

fn remove_pending(hwnd: HWND) {
    let mut guard = PENDING_RETRY.lock().unwrap();
    if let Some(set) = guard.as_mut() {
        set.remove(&(hwnd.0 as isize));
    }
}

fn get_pending_hwnds() -> Vec<HWND> {
    let guard = PENDING_RETRY.lock().unwrap();
    guard.as_ref().map_or(Vec::new(), |s| {
        s.iter().map(|&h| HWND(h as *mut _)).collect()
    })
}

// ── IShellBrowser resolution ──────────────────────────────────────────────────

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

/// Return the list of all Explorer `IShellBrowser`s keyed by their HWND.
/// Used by the new-tab flow to detect which tab/window is newly created.
pub unsafe fn enumerate_shell_browsers() -> Vec<(isize, IShellBrowser)> {
    let mut out = Vec::new();
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    use windows::Win32::UI::Shell::ShellWindows;
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
        let disp = match unsafe { shell_windows.Item(&index).ok() } {
            Some(d) => d,
            None => continue,
        };
        let wba = match disp.cast::<IWebBrowserApp>().ok() {
            Some(w) => w,
            None => continue,
        };
        let hw = match unsafe { wba.HWND().ok() } {
            Some(h) => h,
            None => continue,
        };
        let sp = match wba.cast::<IServiceProvider>().ok() {
            Some(s) => s,
            None => continue,
        };
        let browser: IShellBrowser =
            match unsafe { sp.QueryService(&SID_STopLevelBrowser) } {
                Ok(b) => b,
                Err(_) => continue,
            };
        out.push((hw.0, browser));
    }
    out
}

/// Try to get `IShellBrowser` for `cabinet_hwnd` by enumerating `IShellWindows`.
///
/// Returns `None` on any failure; navigation buttons won't work.
pub unsafe fn get_shell_browser_for(cabinet_hwnd: HWND) -> Option<IShellBrowser> {
    // Ensure COM is initialised on this thread (idempotent if already done).
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };

    use windows::Win32::UI::Shell::ShellWindows;

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

        let sp = match wba.cast::<IServiceProvider>().ok() {
            Some(s) => s,
            None => return None,
        };

        let browser: IShellBrowser =
            unsafe { sp.QueryService(&SID_STopLevelBrowser).ok()? };
        return Some(browser);
    }

    None
}

// ── Injection logic ──────────────────────────────────────────────────────────

/// Attempt to create the single global toolbar (first time only).
/// Returns true if the toolbar was created or already exists.
fn try_inject(hwnd: HWND) -> bool {
    // If toolbar already exists, nothing to create.
    if crate::toolbar::global_toolbar_exists() {
        return true;
    }

    match explorer::check_explorer_ready(hwnd) {
        Some(info) => {
            crate::log::info(&format!(
                "try_inject: explorer ready, default_pos=({},{},{},{})",
                info.default_pos.left, info.default_pos.top,
                info.default_pos.right, info.default_pos.bottom
            ));
            let hinstance = unsafe { crate::HMODULE };

            match crate::toolbar::create_toolbar(
                info.cabinet_hwnd,
                &info.default_pos,
                hinstance,
            ) {
                Some(toolbar_hwnd) => {
                    crate::log::info(&format!(
                        "try_inject: global toolbar created hwnd={toolbar_hwnd:?}"
                    ));
                    remove_pending(hwnd);
                    true
                }
                None => {
                    crate::log::error("try_inject: create_toolbar returned None");
                    false
                }
            }
        }
        None => false,
    }
}

/// Called by WM_TIMER to retry injection on pending windows.
pub fn retry_pending() {
    let count = RETRY_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if count >= MAX_RETRIES {
        crate::log::info("retry_pending: max retries reached, giving up");
        let hwnds = get_pending_hwnds();
        for hwnd in &hwnds {
            remove_pending(*hwnd);
            unsafe { let _ = KillTimer(Some(*hwnd), RETRY_TIMER_ID); }
        }
        return;
    }

    let hwnds = get_pending_hwnds();
    for hwnd in hwnds {
        if crate::toolbar::global_toolbar_exists() {
            remove_pending(hwnd);
            unsafe { let _ = KillTimer(Some(hwnd), RETRY_TIMER_ID); }
            continue;
        }
        if try_inject(hwnd) {
            unsafe { let _ = KillTimer(Some(hwnd), RETRY_TIMER_ID); }
        }
    }
}

unsafe extern "system" fn retry_timer_proc(
    _hwnd: HWND,
    _msg: u32,
    _id: usize,
    _time: u32,
) {
    retry_pending();
}

// ── CBT hook proc ─────────────────────────────────────────────────────────────

pub unsafe fn cbt_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }

    if code as u32 == HCBT_ACTIVATE {
        let hwnd = HWND(wparam.0 as *mut _);
        let class = crate::explorer::get_class_name(hwnd);

        if class == "CabinetWClass" {
            // Always track the most recently activated Explorer.
            crate::toolbar::set_active_explorer(hwnd);
            crate::log::info(&format!("CBT: CabinetWClass activated hwnd={hwnd:?}"));

            // Only create toolbar once.
            if !crate::toolbar::global_toolbar_exists() {
                if try_inject(hwnd) {
                    // Success
                } else {
                    crate::log::info("CBT: bridge not ready, scheduling retry");
                    add_pending(hwnd);
                    RETRY_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);
                    unsafe {
                        let _ = SetTimer(Some(hwnd), RETRY_TIMER_ID, RETRY_INTERVAL_MS, Some(retry_timer_proc));
                    }
                }
            }
        }
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
