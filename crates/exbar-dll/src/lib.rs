#![allow(non_snake_case)]

use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{HINSTANCE, TRUE};
use windows::Win32::UI::WindowsAndMessaging::CallNextHookEx;
use windows_core::BOOL;

// Historical stub.
//
// Exbar is migrating to an out-of-process architecture. This DLL
// remains as a no-op cdylib so user machines with the v0.2.0 build
// (which still has exbar_dll.dll loaded in every process via the
// global CBT hook) continue to behave predictably after an upgrade:
// DllMain returns TRUE immediately in every process, the CBT export
// just calls CallNextHookEx, and the DLL never does anything else.
//
// The DLL file will be removed on MSI upgrade; the loaded instance
// lingers in explorer.exe until Explorer restarts or reboot.

static INITIALIZED: AtomicBool = AtomicBool::new(false);

#[unsafe(no_mangle)]
unsafe extern "system" fn DllMain(
    _hinstance: HINSTANCE,
    _reason: u32,
    _reserved: *mut std::ffi::c_void,
) -> BOOL {
    INITIALIZED.store(false, Ordering::SeqCst);
    TRUE
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn ExbarCBTHook(
    code: i32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
