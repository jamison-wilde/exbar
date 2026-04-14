//! Folder picker wrapping IFileOpenDialog with FOS_PICKFOLDERS.

use std::path::PathBuf;

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog,
    IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
use windows_core::PCWSTR;

/// Show a folder picker. Returns `Some(path)` on OK, `None` on cancel or any failure.
/// Starts at `%SystemDrive%\` (typically `C:\`).
pub fn pick_folder() -> Option<PathBuf> {
    unsafe {
        // Idempotent if COM was already initialised on this thread.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;

        let opts = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)
            .ok()?;

        let start = system_drive_root();
        let start_wide: Vec<u16> = start.encode_utf16().chain(std::iter::once(0)).collect();
        if let Ok(item) =
            SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(start_wide.as_ptr()), None)
        {
            let _ = dialog.SetFolder(&item);
        }

        dialog.Show(None).ok()?;

        let result: IShellItem = dialog.GetResult().ok()?;
        let pwstr = result.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        if pwstr.is_null() {
            return None;
        }
        // Always free the COM-allocated buffer, even if to_string fails.
        let parsed = pwstr.to_string();
        windows::Win32::System::Com::CoTaskMemFree(Some(pwstr.0 as *const _));
        Some(PathBuf::from(parsed.ok()?))
    }
}

fn system_drive_root() -> String {
    std::env::var("SystemDrive")
        .map(|d| format!("{d}\\"))
        .unwrap_or_else(|_| "C:\\".to_owned())
}
