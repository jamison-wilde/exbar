//! Folder-button action handlers. Each action splits into a pure
//! `*_to_state` core (testable against `MockConfigStore`) plus a thin
//! wrapper that posts `WM_USER_RELOAD` to trigger a toolbar rebuild.
//!
//! Pattern preserved from pre-SP8 `toolbar.rs`:
//!   load → mutate via `Config::*` method → save → assign `state.config`

use crate::toolbar::ToolbarState;
use std::path::Path;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

fn post_reload(hwnd: HWND) {
    unsafe {
        let _ = PostMessageW(Some(hwnd), crate::toolbar::WM_USER_RELOAD, WPARAM(0), LPARAM(0));
    }
}

// ── Append folder ─────────────────────────────────────────────────────

/// Core: load → append → save → assign. Returns `true` on success.
/// Does not post `WM_USER_RELOAD` or touch any HWND.
pub(crate) fn append_folder_to_state(state: &mut ToolbarState, path: &Path) -> bool {
    let name = match path.file_name().and_then(|s| s.to_str()) {
        Some(n) if !n.is_empty() => n.to_owned(),
        _ => return false,
    };
    let path_str = match path.to_str() {
        Some(s) => s.to_owned(),
        None => return false,
    };

    // Load → mutate → save. If load fails (no file yet), start from a minimal config.
    let mut cfg = state.config_store.load().unwrap_or_else(|| {
        crate::config::Config::from_str(r#"{"folders":[]}"#).expect("default config parses")
    });
    cfg.add_folder(name, path_str);
    if let Err(e) = state.config_store.save(&cfg) {
        log::error!("append_folder_and_reload: save failed: {e}");
        return false;
    }
    state.config = Some(cfg);
    true
}

/// Append a folder to `~/.exbar.json` using its basename as the label, then reload.
/// No-op on empty / invalid paths.
pub(crate) fn append_folder_and_reload(state: &mut ToolbarState, path: &Path) {
    if append_folder_to_state(state, path)
        && let Some(hwnd) = crate::visibility::get_global_toolbar_hwnd()
    {
        post_reload(hwnd);
    }
}

/// Drop-target entry point: looks up the global toolbar state and delegates to
/// `append_folder_and_reload`. Called from `dragdrop.rs` which has no direct access to state.
pub(crate) fn append_folder_and_reload_global(path: &Path) {
    let hwnd = match crate::visibility::get_global_toolbar_hwnd() {
        Some(h) => h,
        None => return,
    };
    if let Some(state) = unsafe { crate::toolbar::toolbar_state(hwnd) } {
        append_folder_and_reload(state, path);
    }
}

// ── Remove folder ─────────────────────────────────────────────────────

/// Core: load → remove at `folder_index` → save → assign. Returns `true` on success.
/// Does not post `WM_USER_RELOAD` or touch any HWND.
pub(crate) fn remove_folder_from_state(state: &mut ToolbarState, folder_index: usize) -> bool {
    let mut cfg = match state.config_store.load() {
        Some(c) => c,
        None => return false,
    };
    if folder_index >= cfg.folders.len() {
        return false;
    }
    cfg.remove_folder(folder_index);
    if let Err(e) = state.config_store.save(&cfg) {
        log::error!("remove_folder_at: save failed: {e}");
        return false;
    }
    state.config = Some(cfg);
    true
}

/// Remove the folder at `button_index` (which includes the + button at 0),
/// then post `WM_USER_RELOAD`.
pub(crate) fn remove_folder_at(state: &mut ToolbarState, hwnd: HWND, button_index: usize) {
    if button_index == 0 {
        return; // safety: + button never reaches here (is_add branch)
    }
    let folder_index = button_index - 1;
    if remove_folder_from_state(state, folder_index) {
        post_reload(hwnd);
    }
}

// ── Reorder ──────────────────────────────────────────────────────────

/// Core: load → move `from` to `to` → save → assign. Returns `true` on success.
/// Does not post `WM_USER_RELOAD` or touch any HWND.
pub(crate) fn commit_reorder_in_state(
    state: &mut ToolbarState,
    from: usize,
    to: usize,
) -> bool {
    let mut cfg = match state.config_store.load() {
        Some(c) => c,
        None => {
            log::error!("commit_reorder: config load failed");
            return false;
        }
    };
    cfg.move_folder(from, to);
    if let Err(e) = state.config_store.save(&cfg) {
        log::error!("commit_reorder: save failed: {e}");
        return false;
    }
    state.config = Some(cfg);
    true
}

/// Reorder folders, then post `WM_USER_RELOAD`.
pub(crate) fn commit_reorder(state: &mut ToolbarState, hwnd: HWND, from: usize, to: usize) {
    if commit_reorder_in_state(state, from, to) {
        post_reload(hwnd);
    }
}

// ── Copy path ────────────────────────────────────────────────────────

pub(crate) fn copy_folder_path_to_clipboard(state: &mut ToolbarState, folder_button: usize) {
    let btn_slot = folder_button + 1;
    if btn_slot < state.buttons.len() {
        let path = state.buttons[btn_slot].folder.path.clone();
        crate::warn_on_err!(state.clipboard.set_text(&path));
    }
}

// ── Edit config ──────────────────────────────────────────────────────

pub(crate) fn open_config_in_editor() {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let path = crate::config::default_config_path();
    let path_wide: Vec<u16> = crate::toolbar::wide_null(&path);
    let verb_wide: Vec<u16> = crate::toolbar::wide_null("open");

    unsafe {
        let _ = ShellExecuteW(
            None,
            PCWSTR(verb_wide.as_ptr()),
            PCWSTR(path_wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{make_test_state, mk_config_with_folders, mk_deps, TestDeps};

    fn current_folders(state: &ToolbarState) -> Vec<String> {
        state.config.as_ref().map_or_else(Vec::new, |c| {
            c.folders.iter().map(|f| f.name.clone()).collect()
        })
    }

    fn last_saved_folders(deps: &TestDeps) -> Vec<String> {
        let saves = deps.cfg_store.save_calls.lock().unwrap();
        saves.last().map_or_else(Vec::new, |c| {
            c.folders.iter().map(|f| f.name.clone()).collect()
        })
    }

    // ── append ────────────────────────────────────────────────────────

    #[test]
    fn append_folder_loads_mutates_saves_and_assigns() {
        let deps = mk_deps();
        *deps.cfg_store.load_value.lock().unwrap() =
            Some(mk_config_with_folders(&[("Existing", "C:\\old")]));
        let mut state = make_test_state(&deps, None);

        let saved = append_folder_to_state(&mut state, Path::new("C:\\new\\folder"));

        assert!(saved);
        assert_eq!(*deps.cfg_store.load_calls.lock().unwrap(), 1);
        assert_eq!(deps.cfg_store.save_calls.lock().unwrap().len(), 1);
        assert_eq!(last_saved_folders(&deps), vec!["Existing", "folder"]);
        assert_eq!(current_folders(&state), vec!["Existing", "folder"]);
    }

    #[test]
    fn append_folder_save_error_does_not_assign_state_config() {
        let deps = mk_deps();
        *deps.cfg_store.load_value.lock().unwrap() =
            Some(mk_config_with_folders(&[("Existing", "C:\\old")]));
        *deps.cfg_store.save_should_err.lock().unwrap() = true;
        let mut state = make_test_state(&deps, None);

        let saved = append_folder_to_state(&mut state, Path::new("C:\\new\\folder"));

        assert!(!saved);
        assert!(state.config.is_none());
    }

    #[test]
    fn append_folder_with_no_filename_returns_false() {
        let deps = mk_deps();
        *deps.cfg_store.load_value.lock().unwrap() = Some(mk_config_with_folders(&[]));
        let mut state = make_test_state(&deps, None);

        let saved = append_folder_to_state(&mut state, Path::new(""));

        assert!(!saved);
        assert!(deps.cfg_store.save_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn append_folder_with_load_none_uses_empty_default() {
        let deps = mk_deps();
        let mut state = make_test_state(&deps, None);

        let saved = append_folder_to_state(&mut state, Path::new("C:\\new\\folder"));

        assert!(saved);
        assert_eq!(last_saved_folders(&deps), vec!["folder"]);
        assert_eq!(current_folders(&state), vec!["folder"]);
    }

    // ── remove ────────────────────────────────────────────────────────

    #[test]
    fn remove_folder_in_range_removes_and_saves() {
        let deps = mk_deps();
        *deps.cfg_store.load_value.lock().unwrap() = Some(mk_config_with_folders(&[
            ("A", "C:\\a"),
            ("B", "C:\\b"),
            ("C", "C:\\c"),
        ]));
        let mut state = make_test_state(&deps, None);

        let saved = remove_folder_from_state(&mut state, 1);

        assert!(saved);
        assert_eq!(last_saved_folders(&deps), vec!["A", "C"]);
        assert_eq!(current_folders(&state), vec!["A", "C"]);
    }

    #[test]
    fn remove_folder_out_of_range_returns_false_and_does_not_save() {
        let deps = mk_deps();
        *deps.cfg_store.load_value.lock().unwrap() =
            Some(mk_config_with_folders(&[("A", "C:\\a")]));
        let mut state = make_test_state(&deps, None);

        let saved = remove_folder_from_state(&mut state, 5);

        assert!(!saved);
        assert!(deps.cfg_store.save_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn remove_folder_when_load_fails_returns_false() {
        let deps = mk_deps();
        let mut state = make_test_state(&deps, None);

        let saved = remove_folder_from_state(&mut state, 0);

        assert!(!saved);
        assert!(deps.cfg_store.save_calls.lock().unwrap().is_empty());
    }

    // ── reorder ───────────────────────────────────────────────────────

    #[test]
    fn reorder_forward_moves_entry() {
        let deps = mk_deps();
        *deps.cfg_store.load_value.lock().unwrap() = Some(mk_config_with_folders(&[
            ("A", "C:\\a"),
            ("B", "C:\\b"),
            ("C", "C:\\c"),
        ]));
        let mut state = make_test_state(&deps, None);

        let saved = commit_reorder_in_state(&mut state, 0, 2);

        assert!(saved);
        assert_eq!(current_folders(&state), vec!["B", "A", "C"]);
    }

    #[test]
    fn reorder_backward_moves_entry() {
        let deps = mk_deps();
        *deps.cfg_store.load_value.lock().unwrap() = Some(mk_config_with_folders(&[
            ("A", "C:\\a"),
            ("B", "C:\\b"),
            ("C", "C:\\c"),
        ]));
        let mut state = make_test_state(&deps, None);

        let saved = commit_reorder_in_state(&mut state, 2, 0);

        assert!(saved);
        assert_eq!(current_folders(&state), vec!["C", "A", "B"]);
    }

    #[test]
    fn reorder_save_error_does_not_assign_state_config() {
        let deps = mk_deps();
        *deps.cfg_store.load_value.lock().unwrap() = Some(mk_config_with_folders(&[
            ("A", "C:\\a"),
            ("B", "C:\\b"),
        ]));
        *deps.cfg_store.save_should_err.lock().unwrap() = true;
        let mut state = make_test_state(&deps, None);

        let saved = commit_reorder_in_state(&mut state, 0, 2);

        assert!(!saved);
        assert!(state.config.is_none());
    }
}
