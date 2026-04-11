#![allow(non_snake_case)]

use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{HINSTANCE, TRUE};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows_core::{implement, Interface, BOOL, GUID, HRESULT, IUnknown, Ref, Result};

mod config;
mod theme;
pub mod bho;

use bho::TabplorerBHO;

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static mut HMODULE: HINSTANCE = HINSTANCE(std::ptr::null_mut());

/// CLSID for the Tabplorer BHO: {7D2B5E4A-89C1-4F3E-A6D8-1B9E0C5F2A73}
pub const CLSID_TABPLORER: GUID = GUID::from_values(
    0x7D2B5E4A,
    0x89C1,
    0x4F3E,
    [0xA6, 0xD8, 0x1B, 0x9E, 0x0C, 0x5F, 0x2A, 0x73],
);

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

// ── Class factory ─────────────────────────────────────────────────────────────

#[implement(IClassFactory)]
struct TabplorerClassFactory;

impl IClassFactory_Impl for TabplorerClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Ref<'_, IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut core::ffi::c_void,
    ) -> Result<()> {
        // Aggregation not supported
        if punkouter.is_some() {
            // CLASS_E_NOAGGREGATION
            return Err(windows_core::Error::from(HRESULT(0x80040110u32 as i32)));
        }

        let bho = TabplorerBHO::new();
        // #[implement] generates Into<IObjectWithSite> and Into<IUnknown>
        let bho_unk: IUnknown = bho.into();
        unsafe {
            let hr: HRESULT = ((*(*bho_unk.as_raw()
                .cast::<*const windows_core::IUnknown_Vtbl>()))
                .QueryInterface)(bho_unk.as_raw(), riid, ppvobject);
            hr.ok()
        }
    }

    fn LockServer(&self, _flock: BOOL) -> Result<()> {
        Ok(())
    }
}

// ── COM exports ───────────────────────────────────────────────────────────────

/// Called by the COM runtime to obtain a class factory.
#[unsafe(no_mangle)]
unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut core::ffi::c_void,
) -> HRESULT {
    if ppv.is_null() {
        return HRESULT(0x80004003u32 as i32); // E_POINTER
    }

    unsafe { *ppv = std::ptr::null_mut() };

    if unsafe { *rclsid } != CLSID_TABPLORER {
        return HRESULT(0x80040154u32 as i32); // REGDB_E_CLASSNOTREG
    }

    let factory = TabplorerClassFactory;
    let factory_unk: IUnknown = factory.into();
    unsafe {
        let hr: HRESULT = ((*(*factory_unk.as_raw()
            .cast::<*const windows_core::IUnknown_Vtbl>()))
            .QueryInterface)(factory_unk.as_raw(), riid, ppv);
        // QueryInterface adds a ref; our local IUnknown will drop one ref —
        // that's the correct behaviour for DllGetClassObject.
        hr
    }
}

/// Called by the COM runtime to check if the DLL can be unloaded.
/// Returns S_FALSE — keep the DLL loaded.
#[unsafe(no_mangle)]
unsafe extern "system" fn DllCanUnloadNow() -> HRESULT {
    HRESULT(1) // S_FALSE
}
