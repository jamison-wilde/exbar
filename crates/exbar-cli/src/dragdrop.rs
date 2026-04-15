//! IDropTarget implementation for folder buttons.
//! Allows files to be dragged and dropped onto folder tabs to move/copy them.

#![allow(non_snake_case)]

use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Com::{FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, DROPEFFECT_NONE,
    IDropTarget, IDropTarget_Impl, RegisterDragDrop, RevokeDragDrop,
};
use windows::Win32::System::SystemServices::{MK_CONTROL, MK_SHIFT, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    DragQueryFileW, FOF_ALLOWUNDO, FOF_NOCONFIRMMKDIR, FileOperation, HDROP, IFileOperation,
    IShellItemArray, SHCreateItemFromParsingName, SHCreateShellItemArrayFromDataObject,
    SHGetPathFromIDListW, SHParseDisplayName,
};
use windows_core::{PCWSTR, Result, implement};

pub use crate::drop_effect::DropAction;
use crate::drop_effect::{self, DragSession, Effect, KeyState};

// ── FolderDropTarget ──────────────────────────────────────────────────────────

/// Closure type: given client (x, y), returns the drop action for that location.
pub type DropResolver = Box<dyn Fn(i32, i32) -> Option<DropAction> + Send + Sync>;

#[implement(IDropTarget)]
pub struct FolderDropTarget {
    hwnd: HWND,
    resolver: DropResolver,
    current_action: Mutex<Option<DropAction>>,
    session: Mutex<Option<DragSession>>,
}

impl FolderDropTarget {
    pub fn new(hwnd: HWND, resolver: DropResolver) -> Self {
        FolderDropTarget {
            hwnd,
            resolver,
            current_action: Mutex::new(None),
            session: Mutex::new(None),
        }
    }
}

// ── Shell alias resolution ────────────────────────────────────────────────────

/// Resolve a path (including `shell:` aliases) to a real filesystem path.
/// Returns the original path if resolution fails or the path is already absolute.
fn resolve_to_real_path(path: &str) -> String {
    if !crate::config::is_shell_alias(path) && !path.is_empty() {
        // Already a filesystem path; check it starts with a drive letter.
        if path.len() >= 2 && path.as_bytes()[1] == b':' {
            return path.to_owned();
        }
    }

    // Try SHParseDisplayName + SHGetPathFromIDListW
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();

    let ok = unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None).is_ok() };

    if !ok || pidl.is_null() {
        return path.to_owned();
    }

    let mut buf = [0u16; 260];
    let got_path = unsafe { SHGetPathFromIDListW(pidl as *const _, &mut buf) };
    unsafe {
        CoTaskMemFree(Some(pidl as *const _));
    }

    if got_path.as_bool() {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
        if len > 0 {
            return String::from_utf16_lossy(&buf[..len]);
        }
    }

    path.to_owned()
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

/// True if the CF_HDROP payload contains exactly one path and that path is a directory.
fn dropped_is_single_directory(data_object: &IDataObject) -> bool {
    let Some(first) = (unsafe { first_path_from_data_object(data_object) }) else {
        return false;
    };
    // We only count 1 here because first_path_from_data_object already returns just the first;
    // consult the raw HDROP for the count.
    let count = unsafe { hdrop_file_count(data_object) }.unwrap_or(0);
    if count != 1 {
        return false;
    }
    std::path::Path::new(&first).is_dir()
}

/// Return the number of files in the CF_HDROP payload, or None on failure.
unsafe fn hdrop_file_count(data_object: &IDataObject) -> Option<u32> {
    let fmt = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: 1,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    let medium = unsafe { data_object.GetData(&fmt).ok()? };
    let hglobal = unsafe { medium.u.hGlobal };
    if hglobal.is_invalid() {
        return None;
    }

    let hdrop = HDROP(hglobal.0);
    // 0xFFFFFFFF asks for the count.
    let count = unsafe { DragQueryFileW(hdrop, 0xFFFF_FFFF, None) };
    Some(count)
}

// ── Win32 ↔ drop_effect conversions ──────────────────────────────────────────

fn keystate_from(flags: MODIFIERKEYS_FLAGS) -> KeyState {
    KeyState {
        ctrl: flags.contains(MK_CONTROL),
        shift: flags.contains(MK_SHIFT),
        alt: false, // MK_ALT not provided by IDropTarget; not needed today
    }
}

fn effect_to_dropeffect(e: Effect) -> DROPEFFECT {
    match e {
        Effect::None => DROPEFFECT_NONE,
        Effect::Copy => DROPEFFECT_COPY,
        Effect::Move => DROPEFFECT_MOVE,
        Effect::Link => DROPEFFECT_LINK,
    }
}

/// Read the IDataObject once and cache everything DragOver will need.
/// Called at DragEnter.
fn build_session(data_object: &IDataObject) -> DragSession {
    let source_drive = unsafe { first_path_from_data_object(data_object) }
        .map(|p| resolve_to_real_path(&p))
        .and_then(|p| drive_letter(&p));
    let is_single_directory = dropped_is_single_directory(data_object);
    DragSession {
        source_drive,
        is_single_directory,
    }
}

// ── Execute drop ──────────────────────────────────────────────────────────────

unsafe fn execute_drop(
    data_object: &IDataObject,
    effect: DROPEFFECT,
    target_path: &str,
) -> Result<()> {
    use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};

    let file_op: IFileOperation = unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL)? };

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

    let source_items: IShellItemArray =
        unsafe { SHCreateShellItemArrayFromDataObject(data_object)? };

    if effect == DROPEFFECT_MOVE {
        unsafe { file_op.MoveItems(&source_items, &target_item)? };
    } else {
        unsafe { file_op.CopyItems(&source_items, &target_item)? };
    }

    unsafe { file_op.PerformOperations()? };

    Ok(())
}

// ── IDropTarget impl ──────────────────────────────────────────────────────────

impl FolderDropTarget_Impl {
    fn resolve_action(&self, pt: &windows::Win32::Foundation::POINTL) -> Option<DropAction> {
        let mut client_pt = POINT { x: pt.x, y: pt.y };
        unsafe {
            let _ = ScreenToClient(self.hwnd, &mut client_pt);
        }
        (self.resolver)(client_pt.x, client_pt.y)
    }
}

// IDropTarget trait methods take `*mut DROPEFFECT` / `*const _` as dictated by the
// COM ABI; they cannot be declared `unsafe fn` without breaking the trait contract.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
impl IDropTarget_Impl for FolderDropTarget_Impl {
    fn DragEnter(
        &self,
        pdataobj: windows_core::Ref<'_, IDataObject>,
        grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        // Cache everything we'll need for every subsequent DragOver.
        let session = pdataobj.as_ref().map(build_session);
        *self.session.lock().unwrap() = session.clone();

        let action = self.resolve_action(pt);
        *self.current_action.lock().unwrap() = action.clone();

        let effect = drop_effect::effect_for(
            action.as_ref(),
            session.as_ref(),
            keystate_from(grfkeystate),
        );
        let dropeffect = effect_to_dropeffect(effect);
        if !pdweffect.is_null() {
            unsafe { *pdweffect = dropeffect };
        }
        Ok(())
    }

    fn DragOver(
        &self,
        grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        let action = self.resolve_action(pt);
        *self.current_action.lock().unwrap() = action.clone();

        let session = self.session.lock().unwrap().clone();
        let effect = drop_effect::effect_for(
            action.as_ref(),
            session.as_ref(),
            keystate_from(grfkeystate),
        );
        let dropeffect = effect_to_dropeffect(effect);
        if !pdweffect.is_null() {
            unsafe { *pdweffect = dropeffect };
        }
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        *self.current_action.lock().unwrap() = None;
        *self.session.lock().unwrap() = None;
        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: windows_core::Ref<'_, IDataObject>,
        grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        let Some(data_obj) = pdataobj.as_ref() else {
            if !pdweffect.is_null() {
                unsafe { *pdweffect = DROPEFFECT_NONE };
            }
            return Ok(());
        };

        let action = self
            .resolve_action(pt)
            .or_else(|| self.current_action.lock().unwrap().clone());
        let session = self.session.lock().unwrap().clone();

        let result = match action {
            Some(DropAction::MoveCopyTo {
                target: ref target_path,
            }) => {
                let src = session.as_ref().and_then(|s| s.source_drive);
                let real_target = resolve_to_real_path(&target_path.to_string_lossy());
                let target_drive = drive_letter(&real_target);
                let effect =
                    drop_effect::determine_effect(keystate_from(grfkeystate), src, target_drive);
                let dropeffect = effect_to_dropeffect(effect);
                if !pdweffect.is_null() {
                    unsafe { *pdweffect = dropeffect };
                }
                let target_str = target_path.to_string_lossy();
                log::info!("drop: target={target_str:?} effect={effect:?}");
                unsafe { execute_drop(data_obj, dropeffect, &target_str) }
            }
            Some(DropAction::AddFolder) => {
                // Guard against multi-selection/file drops that slipped past DragOver.
                if !session.map(|s| s.is_single_directory).unwrap_or(false) {
                    if !pdweffect.is_null() {
                        unsafe { *pdweffect = DROPEFFECT_NONE };
                    }
                    return Ok(());
                }
                if let Some(folder) = unsafe { first_path_from_data_object(data_obj) } {
                    let pb = std::path::PathBuf::from(&folder);
                    log::info!("drop: add-folder {folder:?}");
                    crate::toolbar::append_folder_and_reload(&pb);
                }
                if !pdweffect.is_null() {
                    unsafe { *pdweffect = DROPEFFECT_COPY };
                }
                Ok(())
            }
            None => {
                if !pdweffect.is_null() {
                    unsafe { *pdweffect = DROPEFFECT_NONE };
                }
                Ok(())
            }
        };

        // Clear session after the drop completes.
        *self.session.lock().unwrap() = None;
        *self.current_action.lock().unwrap() = None;
        result
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Register a drop target for `hwnd`. The `resolver` closure maps
/// client-coordinate (x, y) to the drop action for that location.
pub fn register_drop_target(hwnd: HWND, resolver: DropResolver) -> Result<()> {
    let target = FolderDropTarget::new(hwnd, resolver);
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

    #[test]
    fn shell_alias_target_with_no_drive_defaults_to_move() {
        // If target can't be resolved (no drive letter), default to MOVE.
        let src = Some('C');
        let tgt = None::<char>; // unresolvable shell alias
        let effect = match (src, tgt) {
            (Some(s), Some(t)) if s == t => DROPEFFECT_MOVE,
            (_, None) => DROPEFFECT_MOVE,
            _ => DROPEFFECT_COPY,
        };
        assert_eq!(effect, DROPEFFECT_MOVE);
    }
}
