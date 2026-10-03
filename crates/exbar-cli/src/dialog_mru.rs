//! Recent Folders from file-dialog Save/Open.
//!
//! A Common Item Dialog (`IFileDialog`) exposes no `IShellBrowser`, so the
//! dwell poll that tracks Explorer can't see it. The signal used instead is
//! the shell's own history: on every successful OK the dialog writes
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\ComDlg32\LastVisitedPidlMRU`,
//! and on Cancel it writes nothing — so a change to that key *is* a
//! completed Save/Open.
//!
//! Value format (undocumented, unchanged since Vista):
//! - `MRUListEx` — little-endian `u32` entry indices, newest first,
//!   terminated by `0xFFFF_FFFF`.
//! - `"<n>"` — NUL-terminated UTF-16LE app name (usually an exe basename,
//!   sometimes a GUID), immediately followed by the folder's absolute PIDL.

use crate::target::TargetKind;
use std::path::PathBuf;
use windows::Win32::Foundation::HWND;
use windows::core::{PCWSTR, w};

const MRU_TERMINATOR: u32 = 0xFFFF_FFFF;

/// Index of the newest entry in an `MRUListEx` blob, or `None` when the list
/// is empty or too short to hold one index.
pub fn newest_index(mrulistex: &[u8]) -> Option<u32> {
    let first: [u8; 4] = mrulistex.get(..4)?.try_into().ok()?;
    let idx = u32::from_le_bytes(first);
    (idx != MRU_TERMINATOR).then_some(idx)
}

/// Split a `LastVisitedPidlMRU` entry into its app name and PIDL bytes.
///
/// The PIDL is walked before it is returned — every `SHITEMID.cb` must stay
/// inside the buffer and the list must end in a zero `cb` — so the caller
/// can hand it to the shell without risking a read past the end. A
/// terminator-only PIDL is valid: it names the Desktop.
pub fn parse_entry(blob: &[u8]) -> Option<(String, &[u8])> {
    let nul = blob.chunks_exact(2).position(|unit| unit == [0, 0])?;
    if nul == 0 {
        return None;
    }
    let name: Vec<u16> = blob[..nul * 2]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let app = String::from_utf16(&name).ok()?;
    let pidl = &blob[(nul + 1) * 2..];
    let len = pidl_len(pidl)?;
    Some((app, &pidl[..len]))
}

/// Byte length of the ITEMIDLIST at the start of `pidl`, including its zero
/// terminator, or `None` if any item runs past the buffer.
fn pidl_len(pidl: &[u8]) -> Option<usize> {
    let mut off = 0usize;
    loop {
        let cb_bytes: [u8; 2] = pidl.get(off..off + 2)?.try_into().ok()?;
        let cb = usize::from(u16::from_le_bytes(cb_bytes));
        if cb == 0 {
            return Some(off + 2);
        }
        // cb counts its own two bytes; anything smaller can't advance.
        if cb < 2 {
            return None;
        }
        off = off.checked_add(cb)?;
    }
}

/// Whether a dialog MRU write should become a Recent Folders commit.
///
/// Only writes made while exbar is attached to a file dialog count. An
/// exe-named entry must come from that dialog's process (this rejects
/// writes from exbar's own `+` picker and from dialogs exbar never saw);
/// a GUID-named entry has nothing to compare, so the target kind decides.
pub fn should_commit(active: Option<TargetKind>, dialog_exe: Option<&str>, mru_app: &str) -> bool {
    if active != Some(TargetKind::FileDialog) {
        return false;
    }
    let app = mru_app.to_lowercase();
    if !app.ends_with(".exe") {
        return true;
    }
    let Some(exe) = dialog_exe else {
        return false;
    };
    let base = exe.rsplit(['\\', '/']).next().unwrap_or(exe);
    base.to_lowercase() == app
}

const MRU_KEY: PCWSTR =
    w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\ComDlg32\\LastVisitedPidlMRU");

/// Source of the newest dialog-MRU entry. Trait seam so the adapter is
/// testable without a registry.
pub trait DialogMruSource {
    /// Newest `LastVisitedPidlMRU` entry as (app name, filesystem folder).
    /// `None` when the key is unreadable, the entry is malformed, or the
    /// folder has no filesystem path (Libraries, This PC).
    fn read_newest(&self) -> Option<(String, PathBuf)>;
}

/// Production [`DialogMruSource`]: reads `HKCU` and resolves the PIDL with
/// `SHGetPathFromIDListW`.
#[derive(Default)]
pub struct Win32DialogMru;

impl Win32DialogMru {
    pub fn new() -> Self {
        Self
    }
}

impl DialogMruSource for Win32DialogMru {
    fn read_newest(&self) -> Option<(String, PathBuf)> {
        let order = read_binary_value("MRUListEx")?;
        let idx = newest_index(&order)?;
        let blob = read_binary_value(&idx.to_string())?;
        let Some((app, pidl)) = parse_entry(&blob) else {
            log::debug!(
                "dialog-mru: entry {idx} is malformed ({} bytes)",
                blob.len()
            );
            return None;
        };
        let Some(path) = pidl_to_path(pidl) else {
            log::debug!("dialog-mru: entry {idx} ({app}) has no filesystem path");
            return None;
        };
        Some((app, path))
    }
}

/// Read a `REG_BINARY` value from the MRU key.
fn read_binary_value(name: &str) -> Option<Vec<u8>> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegGetValueW};

    let name_w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let name_p = PCWSTR(name_w.as_ptr());
    let mut size: u32 = 0;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            MRU_KEY,
            name_p,
            RRF_RT_REG_BINARY,
            None,
            None,
            Some(&mut size),
        )
    };
    if rc.is_err() {
        log::debug!("dialog-mru: size query for {name} failed: {rc:?}");
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            MRU_KEY,
            name_p,
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if rc.is_err() {
        log::debug!("dialog-mru: read of {name} failed: {rc:?}");
        return None;
    }
    buf.truncate(size as usize);
    Some(buf)
}

/// Resolve a validated PIDL (from [`parse_entry`]) to a filesystem path.
fn pidl_to_path(pidl: &[u8]) -> Option<PathBuf> {
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::SHGetPathFromIDListW;

    let mut buf = [0u16; 260];
    // SAFETY: `parse_entry` walked the cb chain, so every item and the zero
    // terminator lie inside `pidl`; ITEMIDLIST is byte-packed, so the
    // pointer needs no alignment. The shell only reads through it.
    let ok = unsafe { SHGetPathFromIDListW(pidl.as_ptr().cast::<ITEMIDLIST>(), &mut buf) };
    if !ok.as_bool() {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    (len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len])))
}

#[cfg(test)]
pub(crate) mod test_mocks {
    use super::DialogMruSource;
    use std::path::PathBuf;

    /// Returns a fixed newest entry.
    pub struct MockDialogMru {
        pub newest: Option<(String, PathBuf)>,
    }

    impl DialogMruSource for MockDialogMru {
        fn read_newest(&self) -> Option<(String, PathBuf)> {
            self.newest.clone()
        }
    }
}

// ── ToolbarState adapter ─────────────────────────────────────────────────────

#[allow(dead_code)] // called from wndproc in Task 4
impl crate::toolbar::ToolbarState {
    /// Handle `WM_USER_DIALOG_MRU_CHANGED`: commit the newest dialog-MRU
    /// folder to Recents if it came from the dialog exbar is attached to.
    /// `execute_tracker_event` no-ops when Recents is disabled.
    pub(crate) fn on_dialog_mru_changed(&mut self, toolbar: HWND) {
        let Some((app, path)) = self.dialog_mru.read_newest() else {
            return;
        };
        let kind = self.active_target.map(|t| t.kind);
        if !should_commit(kind, self.active_dialog_exe.as_deref(), &app) {
            log::debug!(
                "dialog-mru: {app} -> {} skipped (target={kind:?} dialog_exe={:?})",
                path.display(),
                self.active_dialog_exe
            );
            return;
        }
        log::debug!("dialog-mru: {app} -> {} committed", path.display());
        self.execute_tracker_event(
            toolbar,
            crate::recent_tracker::TrackerEvent::ActionInFolder(path),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UTF-16LE bytes for `s` followed by a NUL unit.
    fn utf16z(s: &str) -> Vec<u8> {
        s.encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect()
    }

    /// One SHITEMID with `payload` plus the zero terminator.
    fn one_item_pidl(payload: &[u8]) -> Vec<u8> {
        let cb = (payload.len() + 2) as u16;
        let mut v = cb.to_le_bytes().to_vec();
        v.extend_from_slice(payload);
        v.extend_from_slice(&[0, 0]);
        v
    }

    // ── newest_index ────────────────────────────────────────────────────────

    #[test]
    fn newest_index_reads_first_entry() {
        // Real sample prefix: 0x12, 0x00, 0x07, … terminator.
        let blob = [
            0x12, 0, 0, 0, 0, 0, 0, 0, 0x07, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF,
        ];
        assert_eq!(newest_index(&blob), Some(0x12));
    }

    #[test]
    fn newest_index_none_for_terminator_only() {
        assert_eq!(newest_index(&[0xFF, 0xFF, 0xFF, 0xFF]), None);
    }

    #[test]
    fn newest_index_none_for_empty_or_short() {
        assert_eq!(newest_index(&[]), None);
        assert_eq!(newest_index(&[1, 0, 0]), None);
    }

    // ── parse_entry ─────────────────────────────────────────────────────────

    #[test]
    fn parse_entry_splits_exe_name_and_pidl() {
        let pidl = one_item_pidl(&[0x1F, 0x50, 0xAA, 0xBB]);
        let mut blob = utf16z("NAPS2.exe");
        blob.extend_from_slice(&pidl);
        let (app, got) = parse_entry(&blob).expect("valid entry");
        assert_eq!(app, "NAPS2.exe");
        assert_eq!(got, &pidl[..]);
    }

    #[test]
    fn parse_entry_accepts_guid_app_name() {
        let mut blob = utf16z("{32237796-1509-49D1-BB7E-63AD36AE868C}");
        blob.extend_from_slice(&one_item_pidl(&[1, 2]));
        let (app, _) = parse_entry(&blob).expect("valid entry");
        assert_eq!(app, "{32237796-1509-49D1-BB7E-63AD36AE868C}");
    }

    #[test]
    fn parse_entry_accepts_terminator_only_pidl_as_desktop() {
        let mut blob = utf16z("notepad.exe");
        blob.extend_from_slice(&[0, 0]);
        let (_, pidl) = parse_entry(&blob).expect("desktop PIDL is valid");
        assert_eq!(pidl, &[0, 0]);
    }

    #[test]
    fn parse_entry_trims_trailing_bytes_after_terminator() {
        let pidl = one_item_pidl(&[9, 9]);
        let mut blob = utf16z("a.exe");
        blob.extend_from_slice(&pidl);
        blob.extend_from_slice(&[0xDE, 0xAD]);
        let (_, got) = parse_entry(&blob).expect("valid entry");
        assert_eq!(got, &pidl[..]);
    }

    #[test]
    fn parse_entry_rejects_missing_name_terminator() {
        let blob: Vec<u8> = "abc".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(parse_entry(&blob), None);
    }

    #[test]
    fn parse_entry_rejects_empty_app_name() {
        let mut blob = vec![0, 0];
        blob.extend_from_slice(&[0, 0]);
        assert_eq!(parse_entry(&blob), None);
    }

    #[test]
    fn parse_entry_rejects_pidl_with_cb_past_end() {
        let mut blob = utf16z("a.exe");
        blob.extend_from_slice(&[0x40, 0x00, 1, 2, 3]); // cb=64, only 5 bytes
        assert_eq!(parse_entry(&blob), None);
    }

    #[test]
    fn parse_entry_rejects_pidl_without_terminator() {
        let mut blob = utf16z("a.exe");
        blob.extend_from_slice(&[0x04, 0x00, 7, 7]); // one item, no zero cb
        assert_eq!(parse_entry(&blob), None);
    }

    #[test]
    fn parse_entry_rejects_cb_of_one() {
        let mut blob = utf16z("a.exe");
        blob.extend_from_slice(&[0x01, 0x00, 0, 0]);
        assert_eq!(parse_entry(&blob), None);
    }

    #[test]
    fn parse_entry_rejects_odd_length_with_no_pidl() {
        let mut blob = utf16z("a.exe");
        blob.push(0x00); // one stray byte, no complete cb
        assert_eq!(parse_entry(&blob), None);
    }

    // ── should_commit ───────────────────────────────────────────────────────

    const NOTEPAD: &str = "C:\\Windows\\System32\\notepad.exe";

    #[test]
    fn commit_when_dialog_exe_matches() {
        assert!(should_commit(
            Some(TargetKind::FileDialog),
            Some(NOTEPAD),
            "notepad.exe"
        ));
    }

    #[test]
    fn exe_match_is_case_insensitive() {
        assert!(should_commit(
            Some(TargetKind::FileDialog),
            Some(NOTEPAD),
            "NOTEPAD.EXE"
        ));
    }

    #[test]
    fn no_commit_when_exe_differs() {
        assert!(!should_commit(
            Some(TargetKind::FileDialog),
            Some(NOTEPAD),
            "Code.exe"
        ));
    }

    #[test]
    fn no_commit_for_exbar_own_picker() {
        assert!(!should_commit(
            Some(TargetKind::FileDialog),
            Some(NOTEPAD),
            "exbar.exe"
        ));
    }

    #[test]
    fn no_commit_when_dialog_exe_unknown() {
        assert!(!should_commit(
            Some(TargetKind::FileDialog),
            None,
            "notepad.exe"
        ));
    }

    #[test]
    fn no_commit_when_target_is_explorer() {
        assert!(!should_commit(
            Some(TargetKind::Explorer),
            Some(NOTEPAD),
            "notepad.exe"
        ));
    }

    #[test]
    fn no_commit_without_target() {
        assert!(!should_commit(None, Some(NOTEPAD), "notepad.exe"));
    }

    #[test]
    fn guid_app_commits_on_target_kind_alone() {
        let guid = "{32237796-1509-49D1-BB7E-63AD36AE868C}";
        assert!(should_commit(Some(TargetKind::FileDialog), None, guid));
        assert!(!should_commit(Some(TargetKind::Explorer), None, guid));
    }
}

#[cfg(test)]
mod adapter_tests {
    use super::test_mocks::MockDialogMru;
    use crate::target::ActiveTarget;
    use crate::test_helpers::{make_test_state, mk_config_with_folders, mk_deps};
    use crate::toolbar::ToolbarState;
    use std::path::PathBuf;
    use windows::Win32::Foundation::HWND;

    // An invalid HWND: SetTimer in the debounce path fails harmlessly.
    fn toolbar() -> HWND {
        HWND(std::ptr::dangling_mut())
    }
    const NOTEPAD: &str = "C:\\Windows\\System32\\notepad.exe";

    fn state_with(
        recent_enabled: bool,
        excluded: &[&str],
        newest: Option<(&str, &str)>,
    ) -> ToolbarState {
        let deps = mk_deps();
        let mut cfg = mk_config_with_folders(&[("A", "C:\\A")]);
        cfg.recent.enabled = recent_enabled;
        cfg.recent.excluded_paths = excluded.iter().map(|s| (*s).to_owned()).collect();
        let mut state = make_test_state(&deps, Some(cfg));
        state.recent_list.clear();
        state.dialog_mru = Box::new(MockDialogMru {
            newest: newest.map(|(app, p)| (app.to_owned(), PathBuf::from(p))),
        });
        state.active_target = Some(ActiveTarget::file_dialog(HWND(99 as *mut _)));
        state.active_dialog_exe = Some(NOTEPAD.to_owned());
        state
    }

    #[test]
    fn matching_save_commits_folder_to_recents() {
        let mut state = state_with(true, &[], Some(("notepad.exe", "C:\\Notes")));
        state.on_dialog_mru_changed(toolbar());
        assert_eq!(state.recent_list.len(), 1);
        assert_eq!(state.recent_list[0].path, PathBuf::from("C:\\Notes"));
    }

    #[test]
    fn entry_from_other_app_is_not_committed() {
        let mut state = state_with(true, &[], Some(("exbar.exe", "C:\\Notes")));
        state.on_dialog_mru_changed(toolbar());
        assert!(state.recent_list.is_empty());
    }

    #[test]
    fn explorer_target_is_not_committed() {
        let mut state = state_with(true, &[], Some(("notepad.exe", "C:\\Notes")));
        state.active_target = Some(ActiveTarget::explorer(HWND(42 as *mut _)));
        state.on_dialog_mru_changed(toolbar());
        assert!(state.recent_list.is_empty());
    }

    #[test]
    fn recents_disabled_commits_nothing() {
        let mut state = state_with(false, &[], Some(("notepad.exe", "C:\\Notes")));
        state.on_dialog_mru_changed(toolbar());
        assert!(state.recent_list.is_empty());
    }

    #[test]
    fn excluded_folder_is_not_committed() {
        let mut state = state_with(
            true,
            &["C:\\Secret"],
            Some(("notepad.exe", "C:\\Secret\\Tax")),
        );
        state.on_dialog_mru_changed(toolbar());
        assert!(state.recent_list.is_empty());
    }

    #[test]
    fn unreadable_entry_commits_nothing() {
        let mut state = state_with(true, &[], None);
        state.on_dialog_mru_changed(toolbar());
        assert!(state.recent_list.is_empty());
    }
}
