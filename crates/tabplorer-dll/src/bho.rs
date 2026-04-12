//! Browser Helper Object implementing IObjectWithSite.
//! On SetSite, discovers the Explorer command-bar slot and logs the result.

#![allow(non_snake_case)]

use std::io::Write as _;
use std::sync::Mutex;

use windows::Win32::System::Com::IServiceProvider;
use windows::Win32::System::Ole::{IObjectWithSite, IObjectWithSite_Impl, IOleWindow};
use windows::Win32::UI::Shell::{IShellBrowser, SID_STopLevelBrowser};
use windows_core::{implement, Interface, IUnknown, Ref, Result, GUID, HRESULT};

use crate::explorer;

// ── Logging helpers ──────────────────────────────────────────────────────────

fn log_path() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push("tabplorer.log");
    p
}

fn log_to_file(msg: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        let _ = writeln!(f, "{msg}");
    }
}

fn log_info(msg: &str) {
    log_to_file(&format!("[INFO ] {msg}"));
}

fn log_error(msg: &str) {
    log_to_file(&format!("[ERROR] {msg}"));
}

// ── BHO ──────────────────────────────────────────────────────────────────────

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

        match punksite.as_ref() {
            None => {
                log_info("SetSite: site cleared");
            }
            Some(site) => {
                log_info("SetSite: site provided — discovering toolbar slot");
                if let Err(e) = discover_toolbar_slot(site) {
                    log_error(&format!("SetSite: discovery failed: {e:?}"));
                }
            }
        }

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

/// Attempts to find the Explorer command-bar toolbar slot via the COM site.
fn discover_toolbar_slot(site: &IUnknown) -> windows_core::Result<()> {
    // Step 1: QI for IServiceProvider.
    let sp: IServiceProvider = site.cast()?;

    // Step 2: Query for IShellBrowser using SID_STopLevelBrowser.
    // (SID_SShellBrowser is not exported by windows 0.61; SID_STopLevelBrowser
    // is the documented service ID for the top-level shell browser.)
    let browser: IShellBrowser = unsafe { sp.QueryService(&SID_STopLevelBrowser)? };

    // Step 3: Get the cabinet HWND via IOleWindow (IShellBrowser inherits it).
    let ole_window: IOleWindow = browser.cast()?;
    let hwnd = unsafe { ole_window.GetWindow()? };

    log_info(&format!("SetSite: cabinet HWND = {hwnd:?}"));

    // Step 4: Walk the window hierarchy.
    match explorer::check_explorer_ready(hwnd) {
        Some(info) => {
            let r = info.default_pos;
            log_info(&format!(
                "SetSite: explorer ready — owner={:?} pos=({},{},{},{})",
                info.cabinet_hwnd, r.left, r.top, r.right, r.bottom
            ));

            let hinstance = unsafe { crate::HMODULE };
            match crate::toolbar::create_toolbar(info.cabinet_hwnd, &info.default_pos, hinstance) {
                Some(toolbar_hwnd) => {
                    log_info(&format!("SetSite: toolbar created — hwnd={toolbar_hwnd:?}"));
                }
                None => {
                    log_error("SetSite: create_toolbar returned None");
                }
            }
        }
        None => {
            log_info("SetSite: toolbar slot not found (Explorer version may differ)");
        }
    }

    Ok(())
}
