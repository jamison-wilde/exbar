//! IDropTarget implementation for folder buttons.
//! Allows files to be dragged and dropped onto folder tabs to move/copy them.

#![allow(non_snake_case)]

use std::sync::Mutex;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{FORMATETC, TYMED_HGLOBAL, IDataObject};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{
    CF_HDROP, IDropTarget, IDropTarget_Impl,
    RegisterDragDrop, RevokeDragDrop,
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::{MODIFIERKEYS_FLAGS, MK_CONTROL, MK_SHIFT};
use windows::Win32::UI::Shell::{
    FileOperation, FOF_ALLOWUNDO, FOF_NOCONFIRMMKDIR,
    IFileOperation, IShellItemArray,
    SHCreateItemFromParsingName, SHCreateShellItemArrayFromDataObject,
};
use windows_core::{implement, Result, PCWSTR};

// ── FolderDropTarget ──────────────────────────────────────────────────────────

#[implement(IDropTarget)]
pub struct FolderDropTarget {
    target_path: String,
    current_effect: Mutex<DROPEFFECT>,
}

impl FolderDropTarget {
    fn new(target_path: String) -> Self {
        FolderDropTarget {
            target_path,
            current_effect: Mutex::new(DROPEFFECT_NONE),
        }
    }
}

// ── Drop effect logic ─────────────────────────────────────────────────────────

/// Returns the drive letter (uppercased) of the given path, if available.
fn drive_letter(path: &str) -> Option<char> {
    let mut chars = path.chars();
    let first = chars.next()?;
    if first.is_ascii_alphabetic() && chars.next() == Some(':') {
        Some(first.to_ascii_uppercase())
    } else {
        None
    }
}

/// Extract the first file path from CF_HDROP data in an IDataObject.
/// Returns None on any failure.
unsafe fn first_path_from_data_object(data_object: &IDataObject) -> Option<String> {
    let fmt = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: 1, // DVASPECT_CONTENT
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };

    let medium = unsafe { data_object.GetData(&fmt).ok()? };

    // medium.u is a union; for TYMED_HGLOBAL the field is hGlobal.
    let hglobal = unsafe { medium.u.hGlobal };
    if hglobal.is_invalid() {
        return None;
    }

    let ptr = unsafe { GlobalLock(hglobal) };
    if ptr.is_null() {
        return None;
    }

    // DROPFILES layout (all offsets from start of structure):
    //   offset  0: DWORD pFiles  — byte offset from structure start to filenames
    //   offset  4: POINT pt      — drop point (8 bytes)
    //   offset 12: BOOL  fNC     — 4 bytes
    //   offset 16: BOOL  fWide   — 4 bytes (non-zero = Unicode filenames)
    let bytes = ptr as *const u8;
    let p_files = unsafe { (bytes as *const u32).read_unaligned() } as usize;
    let f_wide = unsafe { bytes.add(16).read() } != 0;

    let result = if f_wide {
        let wide_ptr = unsafe { bytes.add(p_files) as *const u16 };
        let mut len = 0usize;
        unsafe {
            while *wide_ptr.add(len) != 0 {
                len += 1;
            }
        }
        if len == 0 {
            None
        } else {
            let slice = unsafe { std::slice::from_raw_parts(wide_ptr, len) };
            Some(String::from_utf16_lossy(slice))
        }
    } else {
        let ansi_ptr = unsafe { bytes.add(p_files) };
        let mut len = 0usize;
        unsafe {
            while *ansi_ptr.add(len) != 0 {
                len += 1;
            }
        }
        if len == 0 {
            None
        } else {
            let slice = unsafe { std::slice::from_raw_parts(ansi_ptr, len) };
            let s: String = slice.iter().map(|&b| b as char).collect();
            Some(s)
        }
    };

    // GlobalLock is reference-counted; balance each Lock with an Unlock.
    let _ = unsafe { GlobalUnlock(hglobal) };

    result
}

fn determine_effect(
    key_state: MODIFIERKEYS_FLAGS,
    data_object: &IDataObject,
    target_path: &str,
) -> DROPEFFECT {
    if key_state.contains(MK_CONTROL) {
        return DROPEFFECT_COPY;
    }
    if key_state.contains(MK_SHIFT) {
        return DROPEFFECT_MOVE;
    }

    let target_drive = drive_letter(target_path);
    let source_drive = unsafe { first_path_from_data_object(data_object) }
        .and_then(|p| drive_letter(&p));

    match (source_drive, target_drive) {
        (Some(s), Some(t)) if s == t => DROPEFFECT_MOVE,
        _ => DROPEFFECT_COPY,
    }
}

// ── Execute drop ──────────────────────────────────────────────────────────────

unsafe fn execute_drop(
    data_object: &IDataObject,
    effect: DROPEFFECT,
    target_path: &str,
) -> Result<()> {
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

    let file_op: IFileOperation = unsafe {
        CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?
    };

    unsafe {
        file_op.SetOperationFlags(FOF_ALLOWUNDO | FOF_NOCONFIRMMKDIR)?;
    }

    let target_wide: Vec<u16> = target_path.encode_utf16().chain(Some(0)).collect();
    let target_item = unsafe {
        SHCreateItemFromParsingName::<_, _, windows::Win32::UI::Shell::IShellItem>(
            PCWSTR(target_wide.as_ptr()),
            None,
        )?
    };

    let source_items: IShellItemArray = unsafe {
        SHCreateShellItemArrayFromDataObject(data_object)?
    };

    if effect == DROPEFFECT_MOVE {
        unsafe { file_op.MoveItems(&source_items, &target_item)? };
    } else {
        unsafe { file_op.CopyItems(&source_items, &target_item)? };
    }

    unsafe { file_op.PerformOperations()? };

    Ok(())
}

// ── IDropTarget impl ──────────────────────────────────────────────────────────

impl IDropTarget_Impl for FolderDropTarget_Impl {
    fn DragEnter(
        &self,
        pdataobj: windows_core::Ref<'_, IDataObject>,
        grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &windows::Win32::Foundation::POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        let effect = if let Some(data_obj) = pdataobj.as_ref() {
            determine_effect(grfkeystate, data_obj, &self.target_path)
        } else {
            DROPEFFECT_NONE
        };

        *self.current_effect.lock().unwrap() = effect;
        if !pdweffect.is_null() {
            unsafe { *pdweffect = effect };
        }
        Ok(())
    }

    fn DragOver(
        &self,
        grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &windows::Win32::Foundation::POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        // Re-check modifier keys; keep existing drive-heuristic effect if no modifier.
        let stored = *self.current_effect.lock().unwrap();
        let effect = if grfkeystate.contains(MK_CONTROL) {
            DROPEFFECT_COPY
        } else if grfkeystate.contains(MK_SHIFT) {
            DROPEFFECT_MOVE
        } else {
            stored
        };

        *self.current_effect.lock().unwrap() = effect;
        if !pdweffect.is_null() {
            unsafe { *pdweffect = effect };
        }
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        *self.current_effect.lock().unwrap() = DROPEFFECT_NONE;
        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: windows_core::Ref<'_, IDataObject>,
        grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &windows::Win32::Foundation::POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        let Some(data_obj) = pdataobj.as_ref() else {
            if !pdweffect.is_null() {
                unsafe { *pdweffect = DROPEFFECT_NONE };
            }
            return Ok(());
        };

        let effect = determine_effect(grfkeystate, data_obj, &self.target_path);
        if !pdweffect.is_null() {
            unsafe { *pdweffect = effect };
        }

        unsafe { execute_drop(data_obj, effect, &self.target_path) }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Register a drop target for `hwnd` that drops files into `target_path`.
pub fn register_drop_target(hwnd: HWND, target_path: &str) -> Result<()> {
    let target = FolderDropTarget::new(target_path.to_owned());
    let drop_target: IDropTarget = target.into();
    unsafe { RegisterDragDrop(hwnd, &drop_target) }
}

/// Unregister the drop target for `hwnd`.
pub fn unregister_drop_target(hwnd: HWND) -> Result<()> {
    unsafe { RevokeDragDrop(hwnd) }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_letter_extracts_correctly() {
        assert_eq!(drive_letter("C:\\foo\\bar.txt"), Some('C'));
        assert_eq!(drive_letter("d:\\"), Some('D'));
        assert_eq!(drive_letter("\\\\server\\share"), None);
        assert_eq!(drive_letter(""), None);
        assert_eq!(drive_letter("relative\\path"), None);
    }

    #[test]
    fn drive_letter_same_drive_means_move() {
        // Simulate the same-drive heuristic without COM.
        let src = Some('C');
        let tgt = Some('C');
        let effect = match (src, tgt) {
            (Some(s), Some(t)) if s == t => DROPEFFECT_MOVE,
            _ => DROPEFFECT_COPY,
        };
        assert_eq!(effect, DROPEFFECT_MOVE);
    }

    #[test]
    fn drive_letter_different_drive_means_copy() {
        let src = Some('C');
        let tgt = Some('D');
        let effect = match (src, tgt) {
            (Some(s), Some(t)) if s == t => DROPEFFECT_MOVE,
            _ => DROPEFFECT_COPY,
        };
        assert_eq!(effect, DROPEFFECT_COPY);
    }

    #[test]
    fn drive_letter_unc_means_copy() {
        // UNC paths have no drive letter → COPY.
        let src = None::<char>;
        let tgt = Some('C');
        let effect = match (src, tgt) {
            (Some(s), Some(t)) if s == t => DROPEFFECT_MOVE,
            _ => DROPEFFECT_COPY,
        };
        assert_eq!(effect, DROPEFFECT_COPY);
    }
}
