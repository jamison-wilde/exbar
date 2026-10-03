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
        /// Number of `read_newest` calls, shared so a test can keep a handle.
        pub calls: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    impl DialogMruSource for MockDialogMru {
        fn read_newest(&self) -> Option<(String, PathBuf)> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.newest.clone()
        }
    }
}

// ── Watcher thread ───────────────────────────────────────────────────────────

/// How long to wait for further writes before reporting a change. One Save
/// writes both `MRUListEx` and the entry value; reading after the burst
/// ends guarantees the newest index points at a fully written entry.
const MRU_SETTLE_MS: u32 = 150;

/// Owns the watcher thread's shutdown event. Dropping it (with
/// `ToolbarState` in `WM_DESTROY`) stops the thread.
pub(crate) struct DialogMruWatcher {
    shutdown: windows::Win32::Foundation::HANDLE,
}

impl DialogMruWatcher {
    /// Start watching. `None` (logged) if the thread or event can't be made;
    /// the feature is then simply off.
    pub(crate) fn spawn(toolbar: HWND) -> Option<Self> {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::Threading::CreateEventW;

        // Manual-reset: once set, every later wait sees it.
        let shutdown = match unsafe { CreateEventW(None, true, false, PCWSTR::null()) } {
            Ok(h) => h,
            Err(e) => {
                log::warn!("dialog-mru: CreateEventW failed: {e:?}; dialog Recents off");
                return None;
            }
        };
        // HWND/HANDLE aren't Send; pass raw values and rebuild in the thread.
        let hwnd_raw = toolbar.0 as isize;
        let shutdown_raw = shutdown.0 as isize;
        let spawned = std::thread::Builder::new()
            .name("exbar-dialog-mru".into())
            .spawn(move || {
                let toolbar = HWND(hwnd_raw as *mut _);
                let shutdown = HANDLE(shutdown_raw as *mut _);
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    watch_loop(toolbar, shutdown);
                }))
                .is_err()
                {
                    log::error!("dialog-mru: watcher panicked; dialog Recents off");
                }
            });
        if let Err(e) = spawned {
            log::warn!("dialog-mru: thread spawn failed: {e}; dialog Recents off");
            unsafe {
                let _ = CloseHandle(shutdown);
            }
            return None;
        }
        Some(Self { shutdown })
    }
}

impl Drop for DialogMruWatcher {
    fn drop(&mut self) {
        // Signal only. The handle is not closed: the thread may still be
        // waiting on it, and the process exits right after WM_DESTROY.
        unsafe {
            let _ = windows::Win32::System::Threading::SetEvent(self.shutdown);
        }
    }
}

/// Wait for changes to the MRU key; post one message per settled burst.
fn watch_loop(toolbar: HWND, shutdown: windows::Win32::Foundation::HANDLE) {
    use windows::Win32::Foundation::{
        CloseHandle, LPARAM, WAIT_EVENT, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
    };
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, REG_NOTIFY_CHANGE_LAST_SET, REG_OPTION_NON_VOLATILE,
        RegCloseKey, RegCreateKeyExW, RegNotifyChangeKeyValue,
    };
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForMultipleObjects};
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

    // Create-or-open: a profile that has never used a file dialog has no key
    // yet, and a privacy cleaner may delete it mid-session (re-open on demand).
    let open_mru_key = || -> Option<HKEY> {
        let mut key = HKEY::default();
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                MRU_KEY,
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_NOTIFY,
                None,
                &mut key,
                None,
            )
        };
        if rc.is_err() {
            log::warn!("dialog-mru: cannot open MRU key: {rc:?}; dialog Recents off");
            return None;
        }
        Some(key)
    };
    let Some(mut key) = open_mru_key() else {
        return;
    };
    let notify = match unsafe { CreateEventW(None, false, false, PCWSTR::null()) } {
        Ok(h) => h,
        Err(e) => {
            log::warn!("dialog-mru: CreateEventW failed: {e:?}; dialog Recents off");
            unsafe {
                let _ = RegCloseKey(key);
            }
            return;
        }
    };
    log::info!("dialog-mru: watching LastVisitedPidlMRU");

    // `armed` avoids stacking a second registration on the same key while
    // one is still pending (after a settle timeout).
    let mut armed = false;
    let mut pending = false;
    loop {
        if !armed {
            let rc = unsafe {
                RegNotifyChangeKeyValue(key, false, REG_NOTIFY_CHANGE_LAST_SET, Some(notify), true)
            };
            if rc.is_err() {
                // e.g. ERROR_KEY_DELETED: re-create the key once and retry.
                log::warn!("dialog-mru: RegNotifyChangeKeyValue failed: {rc:?}; reopening key");
                unsafe {
                    let _ = RegCloseKey(key);
                }
                let Some(new_key) = open_mru_key() else {
                    unsafe {
                        let _ = CloseHandle(notify);
                    }
                    return;
                };
                key = new_key;
                let rc = unsafe {
                    RegNotifyChangeKeyValue(
                        key,
                        false,
                        REG_NOTIFY_CHANGE_LAST_SET,
                        Some(notify),
                        true,
                    )
                };
                if rc.is_err() {
                    log::warn!(
                        "dialog-mru: RegNotifyChangeKeyValue retry failed: {rc:?}; stopping"
                    );
                    break;
                }
            }
            armed = true;
        }
        let timeout = if pending { MRU_SETTLE_MS } else { INFINITE };
        let woke = unsafe { WaitForMultipleObjects(&[notify, shutdown], false, timeout) };
        if woke == WAIT_OBJECT_0 {
            log::debug!("dialog-mru: key changed");
            armed = false;
            pending = true;
        } else if woke == WAIT_TIMEOUT {
            pending = false;
            unsafe {
                let _ = PostMessageW(
                    Some(toolbar),
                    crate::wndproc::WM_USER_DIALOG_MRU_CHANGED,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        } else if woke == WAIT_EVENT(WAIT_OBJECT_0.0 + 1) {
            break; // shutdown signalled
        } else {
            log::warn!("dialog-mru: wait failed: {woke:?}; dialog Recents off");
            break;
        }
    }
    unsafe {
        let _ = CloseHandle(notify);
        let _ = RegCloseKey(key);
    }
}

// ── ToolbarState adapter ─────────────────────────────────────────────────────

impl crate::toolbar::ToolbarState {
    /// Handle `WM_USER_DIALOG_MRU_CHANGED`: commit the newest dialog-MRU
    /// folder to Recents if it came from the dialog exbar is attached to.
    /// Returns early when Recents is disabled so no registry read or PIDL
    /// resolve happens per Save while the feature is off.
    pub(crate) fn on_dialog_mru_changed(&mut self, toolbar: HWND) {
        if !self.config.as_ref().is_some_and(|c| c.recent.enabled) {
            return;
        }
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
        log::debug!("dialog-mru: {app} -> {} sent to tracker", path.display());
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
            calls: Default::default(),
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
    fn recents_disabled_never_reads_the_registry() {
        let mut state = state_with(false, &[], Some(("notepad.exe", "C:\\Notes")));
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        state.dialog_mru = Box::new(MockDialogMru {
            newest: Some(("notepad.exe".to_owned(), PathBuf::from("C:\\Notes"))),
            calls: calls.clone(),
        });
        state.on_dialog_mru_changed(toolbar());
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 0);
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
