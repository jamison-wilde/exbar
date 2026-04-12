#![allow(non_snake_case)]

use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{HINSTANCE, TRUE};
use windows_core::BOOL;

mod config;
mod theme;
pub mod log;
pub mod dragdrop;
pub mod explorer;
pub mod toolbar;
pub mod navigate;
pub mod hook;

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static mut HMODULE: HINSTANCE = HINSTANCE(std::ptr::null_mut());

// ── Process check ────────────────────────────────────────────────────────────

fn is_explorer_process() -> bool {
    let path = std::env::current_exe().unwrap_or_default();
    let name = path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    name.eq_ignore_ascii_case("explorer.exe")
}

// ── DllMain ──────────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
unsafe extern "system" fn DllMain(
    hinstance: HINSTANCE,
    reason: u32,
    _reserved: *mut std::ffi::c_void,
) -> BOOL {
    use windows::Win32::System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};
    match reason {
        DLL_PROCESS_ATTACH => {
            if !is_explorer_process() {
                return TRUE;
            }
            unsafe { HMODULE = hinstance };
            INITIALIZED.store(true, Ordering::SeqCst);
            log::info("DllMain: DLL_PROCESS_ATTACH (explorer.exe)");
        }
        DLL_PROCESS_DETACH => {
            if !INITIALIZED.load(Ordering::SeqCst) {
                return TRUE;
            }
            INITIALIZED.store(false, Ordering::SeqCst);
            log::info("DllMain: DLL_PROCESS_DETACH");
        }
        _ => {}
    }
    TRUE
}

// ── CBT hook export ───────────────────────────────────────────────────────────

/// Global CBT hook callback installed by `exbar.exe hook`.
///
/// Called for every CBT event in every thread of every process. Passes
/// through immediately in non-Explorer processes (INITIALIZED is false
/// unless this DLL was loaded into explorer.exe).
#[unsafe(no_mangle)]
pub unsafe extern "system" fn ExbarCBTHook(
    code: i32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    if !INITIALIZED.load(Ordering::SeqCst) {
        return unsafe {
            windows::Win32::UI::WindowsAndMessaging::CallNextHookEx(None, code, wparam, lparam)
        };
    }

    use std::sync::Once;
    static FIRST_CALL: Once = Once::new();
    FIRST_CALL.call_once(|| {
        log::info("ExbarCBTHook: first invocation in explorer.exe");
    });

    unsafe { hook::cbt_hook_proc(code, wparam, lparam) }
}
