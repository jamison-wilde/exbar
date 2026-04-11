//! CBT hook callback — injected into every process via SetWindowsHookExW.
//!
//! On HCBT_ACTIVATE, checks whether the activating window is a CabinetWClass
//! (Explorer) window that we haven't yet injected, then creates the toolbar.

use std::collections::HashSet;
use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{CallNextHookEx, HCBT_ACTIVATE};
use windows::Win32::System::Com::{
    CoInitializeEx, CoCreateInstance, CLSCTX_LOCAL_SERVER, IServiceProvider, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{IShellBrowser, IShellWindows, IWebBrowserApp, SID_STopLevelBrowser};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_I4, VARENUM};
use windows_core::Interface;

use crate::explorer;

// ── Injected-HWND registry ────────────────────────────────────────────────────

static INJECTED: Mutex<Option<HashSet<isize>>> = Mutex::new(None);

fn already_injected(hwnd: HWND) -> bool {
    let guard = INJECTED.lock().unwrap();
    guard.as_ref().map_or(false, |s| s.contains(&(hwnd.0 as isize)))
}

fn mark_injected(hwnd: HWND) {
    let mut guard = INJECTED.lock().unwrap();
    guard.get_or_insert_with(HashSet::new).insert(hwnd.0 as isize);
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

/// Try to get `IShellBrowser` for `cabinet_hwnd` by enumerating `IShellWindows`.
///
/// Returns `None` on any failure; the toolbar will still be created but
/// navigation buttons won't work.
unsafe fn get_shell_browser(cabinet_hwnd: HWND) -> Option<IShellBrowser> {
    // Ensure COM is initialised on this thread (idempotent if already done).
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };

    // ShellWindows CLSID = {9BA05972-F6A8-11CF-A442-00A0C90A8F39}
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

        // QI for IWebBrowserApp to read the HWND
        let wba = match disp.cast::<IWebBrowserApp>().ok() {
            Some(w) => w,
            None => continue,
        };

        let win_handle = match unsafe { wba.HWND().ok() } {
            Some(h) => h,
            None => continue,
        };

        // SHANDLE_PTR.0 is isize; HWND.0 is *mut c_void — compare as isize
        if win_handle.0 != cabinet_hwnd.0 as isize {
            continue;
        }

        // Matched — get IServiceProvider -> IShellBrowser
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

// ── CBT hook proc ─────────────────────────────────────────────────────────────

pub unsafe fn cbt_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // Per MSDN: if code < 0, call next hook and return.
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }

    if code as u32 == HCBT_ACTIVATE {
        let hwnd = HWND(wparam.0 as *mut _);

        if !already_injected(hwnd) {
            let class = crate::explorer::get_class_name(hwnd);
            if class == "CabinetWClass" {
                if let Some(slot) = explorer::find_toolbar_slot(hwnd) {
                    let hinstance = unsafe { crate::HMODULE };
                    let shell_browser = unsafe { get_shell_browser(hwnd) };

                    if crate::toolbar::create_toolbar(
                        slot.parent,
                        &slot.bounds,
                        hinstance,
                        shell_browser,
                    )
                    .is_some()
                    {
                        mark_injected(hwnd);
                    }
                }
            }
        }
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
