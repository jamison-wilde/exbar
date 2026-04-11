#![allow(non_snake_case)]

use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{HINSTANCE, TRUE};
use windows_core::BOOL;
use windows::Win32::System::SystemServices::{
    DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH,
};

mod config;
mod theme;

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static mut HMODULE: HINSTANCE = HINSTANCE(std::ptr::null_mut());

#[unsafe(no_mangle)]
unsafe extern "system" fn DllMain(
    hinstance: HINSTANCE,
    reason: u32,
    _reserved: *mut std::ffi::c_void,
) -> BOOL {
    match reason {
        DLL_PROCESS_ATTACH => {
            unsafe { HMODULE = hinstance };
            INITIALIZED.store(true, Ordering::SeqCst);
        }
        DLL_PROCESS_DETACH => {
            INITIALIZED.store(false, Ordering::SeqCst);
        }
        _ => {}
    }
    TRUE
}
