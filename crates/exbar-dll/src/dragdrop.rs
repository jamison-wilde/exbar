//! IDropTarget implementation for folder buttons.
//! Allows files to be dragged and dropped onto folder tabs to move/copy them.

#![allow(non_snake_case)]

use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::{FORMATETC, TYMED_HGLOBAL, IDataObject};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{
    CF_HDROP, IDropTarget, IDropTarget_Impl,
    RegisterDragDrop, RevokeDragDrop,
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::{MODIFIERKEYS_FLAGS, MK_CONTROL, MK_SHIFT};
use windows::Win32::UI::Shell::{
    DragQueryFileW, FileOperation, FOF_ALLOWUNDO, FOF_NOCONFIRMMKDIR,
    HDROP, IFileOperation, IShellItemArray,
    SHCreateItemFromParsingName, SHCreateShellItemArrayFromDataObject,
    SHParseDisplayName, SHGetPathFromIDListW,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::System::Com::CoTaskMemFree;
use windows_core::{implement, Result, PCWSTR};

// ── FolderDropTarget ──────────────────────────────────────────────────────────

/// What should happen when a drop completes at a given toolbar location.
pub enum DropAction {
    /// Standard folder target: move or copy the dropped files into `target_path`.
    MoveCopyTo(String),
    /// The `+` button: append the dropped folder to `~/.exbar.json`.
    AddFolder,
}

/// Closure type: given client (x, y), returns the drop action for that location.
pub type DropResolver = Box<dyn Fn(i32, i32) -> Option<DropAction> + Send + Sync>;

#[implement(IDropTarget)]
pub struct FolderDropTarget {
    hwnd: HWND,
    resolver: DropResolver,
    current_effect: Mutex<DROPEFFECT>,
    current_action: Mutex<Option<DropActionSnapshot>>,
}

#[derive(Clone)]
enum DropActionSnapshot {
    MoveCopyTo(String),
    AddFolder,
}

impl From<&DropAction> for DropActionSnapshot {
    fn from(a: &DropAction) -> Self {
        match a {
            DropAction::MoveCopyTo(p) => DropActionSnapshot::MoveCopyTo(p.clone()),
            DropAction::AddFolder => DropActionSnapshot::AddFolder,
        }
    }
}

impl FolderDropTarget {
    pub fn new(hwnd: HWND, resolver: DropResolver) -> Self {
        FolderDropTarget {
            hwnd,
            resolver,
            current_effect: Mutex::new(DROPEFFECT_NONE),
            current_action: Mutex::new(None),
        }
    }
}

// ── Shell alias resolution ────────────────────────────────────────────────────

/// Resolve a path (including `shell:` aliases) to a real filesystem path.
/// Returns the original path if resolution fails or the path is already absolute.
fn resolve_to_real_path(path: &str) -> String {
    if !path.starts_with("shell:") && !path.is_empty() {
        // Already a filesystem path; check it starts with a drive letter.
        if path.len() >= 2 && path.as_bytes()[1] == b':' {
            return path.to_owned();
        }
    }

    // Try SHParseDisplayName + SHGetPathFromIDListW
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();

    let ok = unsafe {
        SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None).is_ok()
    };

    if !ok || pidl.is_null() {
        return path.to_owned();
    }

    let mut buf = [0u16; 260];
    let got_path = unsafe { SHGetPathFromIDListW(pidl as *const _, &mut buf) };
    unsafe { CoTaskMemFree(Some(pidl as *const _)); }

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
    let Some(first) = (unsafe { first_path_from_data_object(data_object) }) else { return false; };
    // We only count 1 here because first_path_from_data_object already returns just the first;
    // consult the raw HDROP for the count.
    let count = unsafe { hdrop_file_count(data_object) }.unwrap_or(0);
    if count != 1 { return false; }
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
    if hglobal.is_invalid() { return None; }

    let hdrop = HDROP(hglobal.0);
    // 0xFFFFFFFF asks for the count.
    let count = unsafe { DragQueryFileW(hdrop, 0xFFFF_FFFF, None) };
    Some(count)
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

    // Resolve shell aliases to real paths before comparing drive letters.
    let real_target = resolve_to_real_path(target_path);
    let target_drive = drive_letter(&real_target);

    let source_drive = unsafe { first_path_from_data_object(data_object) }
        .map(|p| resolve_to_real_path(&p))
        .and_then(|p| drive_letter(&p));

    match (source_drive, target_drive) {
        (Some(s), Some(t)) if s == t => DROPEFFECT_MOVE,
        // If resolution failed for target, default to MOVE (same-drive is more common).
        (_, None) => DROPEFFECT_MOVE,
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

impl FolderDropTarget_Impl {
    fn resolve_action(&self, pt: &windows::Win32::Foundation::POINTL) -> Option<DropAction> {
        let mut client_pt = POINT { x: pt.x, y: pt.y };
        unsafe { ScreenToClient(self.hwnd, &mut client_pt); }
        (self.resolver)(client_pt.x, client_pt.y)
    }
}

impl IDropTarget_Impl for FolderDropTarget_Impl {
    fn DragEnter(
        &self,
        pdataobj: windows_core::Ref<'_, IDataObject>,
        grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        let action = self.resolve_action(pt);
        *self.current_action.lock().unwrap() = action.as_ref().map(DropActionSnapshot::from);

        let effect = match (pdataobj.as_ref(), action.as_ref()) {
            (Some(d), Some(DropAction::MoveCopyTo(p))) => determine_effect(grfkeystate, d, p),
            (Some(d), Some(DropAction::AddFolder)) => {
                if dropped_is_single_directory(d) { DROPEFFECT_COPY } else { DROPEFFECT_NONE }
            }
            _ => DROPEFFECT_NONE,
        };

        *self.current_effect.lock().unwrap() = effect;
        if !pdweffect.is_null() { unsafe { *pdweffect = effect }; }
        Ok(())
    }

    fn DragOver(
        &self,
        grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        let action = self.resolve_action(pt);
        *self.current_action.lock().unwrap() = action.as_ref().map(DropActionSnapshot::from);

        let effect = match action.as_ref() {
            Some(DropAction::MoveCopyTo(_)) => {
                if grfkeystate.contains(MK_CONTROL) { DROPEFFECT_COPY }
                else if grfkeystate.contains(MK_SHIFT) { DROPEFFECT_MOVE }
                else { *self.current_effect.lock().unwrap() }
            }
            Some(DropAction::AddFolder) => {
                // Effect decided in DragEnter based on data; keep it.
                *self.current_effect.lock().unwrap()
            }
            None => DROPEFFECT_NONE,
        };

        *self.current_effect.lock().unwrap() = effect;
        if !pdweffect.is_null() { unsafe { *pdweffect = effect }; }
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        *self.current_effect.lock().unwrap() = DROPEFFECT_NONE;
        *self.current_action.lock().unwrap() = None;
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
            if !pdweffect.is_null() { unsafe { *pdweffect = DROPEFFECT_NONE }; }
            return Ok(());
        };

        let action = self.resolve_action(pt)
            .or_else(|| self.current_action.lock().unwrap().clone().map(|s| match s {
                DropActionSnapshot::MoveCopyTo(p) => DropAction::MoveCopyTo(p),
                DropActionSnapshot::AddFolder => DropAction::AddFolder,
            }));

        match action {
            Some(DropAction::MoveCopyTo(target_path)) => {
                let effect = determine_effect(grfkeystate, data_obj, &target_path);
                if !pdweffect.is_null() { unsafe { *pdweffect = effect }; }
                crate::log::info(&format!("drop: target={target_path:?} effect={effect:?}"));
                unsafe { execute_drop(data_obj, effect, &target_path) }
            }
            Some(DropAction::AddFolder) => {
                if let Some(folder) = unsafe { first_path_from_data_object(data_obj) } {
                    let pb = std::path::PathBuf::from(&folder);
                    if pb.is_dir() {
                        crate::log::info(&format!("drop: add-folder {folder:?}"));
                        // Best-effort: update config and notify the toolbar.
                        if let Some(mut cfg) = crate::config::Config::load() {
                            let name = pb.file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("")
                                .to_owned();
                            if !name.is_empty() {
                                cfg.add_folder(name, folder);
                                let _ = cfg.save();
                                if let Some(tb) = crate::toolbar::get_global_toolbar_hwnd_public() {
                                    unsafe {
                                        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                                            Some(tb),
                                            crate::toolbar::WM_USER_RELOAD_PUB,
                                            windows::Win32::Foundation::WPARAM(0),
                                            windows::Win32::Foundation::LPARAM(0),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                if !pdweffect.is_null() { unsafe { *pdweffect = DROPEFFECT_COPY }; }
                Ok(())
            }
            None => {
                if !pdweffect.is_null() { unsafe { *pdweffect = DROPEFFECT_NONE }; }
                Ok(())
            }
        }
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
