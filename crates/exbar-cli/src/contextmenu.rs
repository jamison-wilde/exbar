//! Thin wrappers around Win32 TrackPopupMenu for Exbar's context menus.

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, MF_SEPARATOR, MF_STRING, SetForegroundWindow,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
};
use windows_core::PCWSTR;

pub struct MenuItem {
    pub id: u32,
    pub label: &'static str,
}

pub const SEPARATOR: MenuItem = MenuItem { id: 0, label: "" };

/// Show a popup menu at screen coords `pt`. Returns the selected item id,
/// or 0 if the user dismissed the menu.
pub fn show_menu(owner: HWND, pt: POINT, items: &[MenuItem]) -> u32 {
    let hmenu = match unsafe { CreatePopupMenu() } {
        Ok(h) => h,
        Err(_) => return 0,
    };

    for item in items {
        if item.id == 0 && item.label.is_empty() {
            unsafe {
                let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());
            }
        } else {
            let wide: Vec<u16> = item
                .label
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            unsafe {
                let _ = AppendMenuW(hmenu, MF_STRING, item.id as usize, PCWSTR(wide.as_ptr()));
            }
        }
    }

    unsafe {
        let _ = SetForegroundWindow(owner);
    }

    let result = unsafe {
        TrackPopupMenu(
            hmenu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            Some(0),
            owner,
            None,
        )
    };

    unsafe {
        let _ = DestroyMenu(hmenu);
    }

    result.0 as u32
}
