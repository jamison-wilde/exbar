//! Stub Browser Helper Object implementing IObjectWithSite.
//! Real Explorer discovery logic is added in Task 5.

#![allow(non_snake_case)]

use std::sync::Mutex;
use windows::Win32::System::Ole::{IObjectWithSite, IObjectWithSite_Impl};
use windows_core::{implement, Interface, IUnknown, Ref, Result, GUID, HRESULT};

#[implement(IObjectWithSite)]
pub struct TabplorerBHO {
    site: Mutex<Option<IUnknown>>,
}

impl TabplorerBHO {
    pub fn new() -> Self {
        TabplorerBHO {
            site: Mutex::new(None),
        }
    }
}

impl IObjectWithSite_Impl for TabplorerBHO_Impl {
    fn SetSite(&self, punksite: Ref<'_, IUnknown>) -> Result<()> {
        let mut guard = self.site.lock().unwrap();
        *guard = punksite.as_ref().cloned();
        Ok(())
    }

    fn GetSite(&self, riid: *const GUID, ppvsite: *mut *mut core::ffi::c_void) -> Result<()> {
        let guard = self.site.lock().unwrap();
        match guard.as_ref() {
            Some(site) => unsafe {
                let hr: HRESULT = ((*(*site.as_raw()
                    .cast::<*const windows_core::IUnknown_Vtbl>()))
                    .QueryInterface)(site.as_raw(), riid, ppvsite);
                hr.ok()
            },
            None => {
                if !ppvsite.is_null() {
                    unsafe { *ppvsite = std::ptr::null_mut() };
                }
                // E_NOINTERFACE
                Err(windows_core::Error::from(HRESULT(0x80004002u32 as i32)))
            }
        }
    }
}
