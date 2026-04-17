//! Floating draggable toolbar window for Explorer folder shortcuts.
//!
//! This module is the central state container for the toolbar UI.
//! It owns:
//!
//! - **`ToolbarState`** — the per-toolbar-instance state struct
//!   carrying configuration, layout, pointer state, rename state,
//!   active Explorer HWND, and the trait-seam handles for navigation,
//!   file ops, clipboard, picker, and config persistence. See
//!   `docs/adrs/ADR-0005-toolbar-state-over-statics.md` for why
//!   state lives here instead of in module-level statics.
//! - **Adapter methods** — `execute_pointer_command` and
//!   `execute_rename_event` translate pure-controller commands into
//!   Win32 effects. See
//!   `docs/adrs/ADR-0003-pure-controller-adapter-pattern.md`.
//!
//! The Win32 window procedure (`toolbar_wndproc`) lives in
//! [`crate::wndproc`]. Foreground-window tracking, the WinEvent hook,
//! and `GLOBAL_TOOLBAR` live in [`crate::visibility`].
//!
//! ## Threading
//!
//! All state mutation happens on the message-pump thread (the one
//! that called `SetWinEventHook` and runs `GetMessage`). The
//! `unsafe { toolbar_state(hwnd) }` helper relies on this invariant
//! for soundness — it does not lock.

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{GWLP_USERDATA, GetWindowLongPtrW, PostMessageW};

use std::sync::Arc;

use crate::clipboard::{Clipboard, Win32Clipboard};
use crate::config::{Config, ConfigStore, JsonFileStore, Orientation};
use crate::dialog_nav::{DialogNavigator, KeybdDialogNavigator};
use crate::dragdrop::{FileOperator, Win32FileOp};
use crate::layout::ButtonLayout;
use crate::picker::{FolderPicker, Win32Picker};
use crate::pointer;
use crate::shell_windows::{ShellBrowser, Win32Shell};
use crate::theme;

// ── Safe wrappers for repetitive patterns ───────────────────────────────────

/// Retrieve the `ToolbarState` stored in the window's user data.
///
/// # Safety
/// - `hwnd` must be a toolbar window (same HWND that had
///   `SetWindowLongPtrW(GWLP_USERDATA, state)` called during its `WM_CREATE`).
/// - Caller must be on the toolbar's message-pump thread — Win32's
///   single-threaded message dispatch is the synchronization boundary.
/// - The returned reference borrows the state for the caller's scope; no
///   other code path may hold a mutable reference in parallel (guaranteed
///   by single-threaded message dispatch).
pub(crate) unsafe fn toolbar_state<'a>(hwnd: HWND) -> Option<&'a mut ToolbarState> {
    // SAFETY: GetWindowLongPtrW returns the value set by SetWindowLongPtrW;
    // we stored a Box::into_raw pointer in WM_CREATE.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ToolbarState;
    if ptr.is_null() {
        return None;
    }
    // SAFETY: ptr is non-null; state is owned by the window; caller is on
    // the message-pump thread (contract above).
    Some(unsafe { &mut *ptr })
}

/// Encode `s` as a null-terminated UTF-16 vector suitable for
/// `PCWSTR(v.as_ptr())`. The vec must outlive the PCWSTR usage.
pub(crate) fn wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ── Constants ────────────────────────────────────────────────────────────────

pub(crate) const WM_USER_RELOAD: u32 = 0x0401;
/// Timer ID for deferred reposition after maximize/restore animation.
pub(crate) const TIMER_REPOSITION: usize = 1;

// Layout constants (logical pixels, scale by DPI)
pub(crate) const BTN_PAD_H: i32 = 10;
/// Logical pixel width/height of the drag handle grip area.
pub(crate) const GRIP_SIZE: i32 = 12;

// ── Adapter helpers ──────────────────────────────────────────────────────────

/// Emit a `Cancelled` rename event so the controller cleans up any in-flight
/// rename. Called from `execute_pointer_command` (`CancelInlineRename` command)
/// and from `wndproc` on `WM_DESTROY` (parent teardown). The transition table
/// guarantees this is a noop when no rename is active.
pub(crate) fn cancel_inline_rename(state: &mut ToolbarState, toolbar: HWND) {
    state.execute_rename_event(toolbar, crate::rename::RenameEvent::Cancelled);
}

// ── Data structures ──────────────────────────────────────────────────────────

pub(crate) struct ToolbarState {
    pub(crate) buttons: Vec<ButtonLayout>,
    pub(crate) dpi: u32,
    pub(crate) config: Option<Config>,
    pub(crate) layout: Orientation,
    pub(crate) drop_registered: bool,
    /// Logical pixel size of the grip (already includes DPI scale factor).
    pub(crate) grip_size: i32,
    pub(crate) pointer: pointer::PointerState,
    pub(crate) mouse_tracking_started: bool,
    pub(crate) self_release_pending: bool,
    // SP3 trait seams
    pub(crate) clipboard: Box<dyn Clipboard>,
    pub(crate) config_store: Box<dyn ConfigStore>,
    pub(crate) dialog_nav: Box<dyn DialogNavigator>,
    pub(crate) file_operator: Arc<dyn FileOperator>,
    pub(crate) folder_picker: Box<dyn FolderPicker>,
    pub(crate) shell_browser: Box<dyn ShellBrowser>,
    // SP4 consolidation — populated in Tasks 2-3:
    pub(crate) active_target: Option<crate::target::ActiveTarget>,
    /// Last-seen Explorer visible origin — used to detect moves/maximize/restore.
    pub(crate) last_explorer_origin: Option<(i32, i32)>,
    /// True while an Explorer window is being moved/resized (between
    /// MOVESIZESTART and MOVESIZEEND). Used to suppress CAPTUREEND
    /// repositioning during drag — MOVESIZEEND handles that instead.
    pub(crate) explorer_moving: bool,
    pub(crate) rename_state: Option<rename::RenameState>,
}

impl ToolbarState {
    pub(crate) fn new(dpi: u32, config: Option<Config>) -> Self {
        Self::with_deps(
            dpi,
            config,
            Box::new(Win32Shell::new()),
            Box::new(Win32Picker::new()),
            Arc::new(Win32FileOp::new()),
            Box::new(Win32Clipboard::new()),
            Box::new(JsonFileStore::new()),
            Box::new(KeybdDialogNavigator::new()),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_deps(
        dpi: u32,
        config: Option<Config>,
        shell_browser: Box<dyn ShellBrowser>,
        folder_picker: Box<dyn FolderPicker>,
        file_operator: Arc<dyn FileOperator>,
        clipboard: Box<dyn Clipboard>,
        config_store: Box<dyn ConfigStore>,
        dialog_nav: Box<dyn DialogNavigator>,
    ) -> Self {
        let layout = config
            .as_ref()
            .map_or(Orientation::Horizontal, |c| c.layout);
        ToolbarState {
            buttons: Vec::new(),
            dpi,
            config,
            layout,
            drop_registered: false,
            grip_size: theme::scale(GRIP_SIZE, dpi),
            pointer: pointer::PointerState::default(),
            mouse_tracking_started: false,
            self_release_pending: false,
            clipboard,
            config_store,
            dialog_nav,
            file_operator,
            folder_picker,
            shell_browser,
            active_target: None,
            last_explorer_origin: None,
            explorer_moving: false,
            rename_state: None,
        }
    }
}

// ── SP2b pointer adapter methods ─────────────────────────────────────────────

impl ToolbarState {
    /// Drive the pointer state machine with a single event, then execute the
    /// resulting commands against Win32.
    ///
    /// Safety note on `mem::take`: between `take` and the reassignment, `self.pointer`
    /// transiently reads `Idle`. Reentrancy into this wndproc during the gap would
    /// observe the wrong state. Today the only command that can pump Win32 state
    /// synchronously is `CancelInlineRename`, which calls `DestroyWindow` on the
    /// subclassed EDIT control — `WM_DESTROY` is dispatched to the EDIT's wndproc
    /// (not ours), so `toolbar_wndproc` is not re-entered. Any future command that
    /// might trigger a toolbar-directed WM must preserve this invariant.
    pub(crate) fn apply_pointer_event(&mut self, hwnd: HWND, event: pointer::PointerEvent) {
        let (new_state, commands) = pointer::transition(std::mem::take(&mut self.pointer), event);
        self.pointer = new_state;
        for cmd in commands {
            self.execute_pointer_command(hwnd, cmd);
        }
    }

    fn execute_pointer_command(&mut self, hwnd: HWND, cmd: pointer::PointerCommand) {
        use pointer::PointerCommand::*;
        match cmd {
            Redraw => unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            },
            StartMouseTracking => {
                if !self.mouse_tracking_started {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    crate::warn_on_err!(unsafe { TrackMouseEvent(&mut tme) });
                    self.mouse_tracking_started = true;
                }
            }
            CaptureMouse => unsafe {
                let _ = SetCapture(hwnd);
            },
            ReleaseMouse => {
                // Only set the pending flag if we actually hold capture —
                // else ReleaseCapture won't fire WM_CAPTURECHANGED and the
                // flag would strand. Calling ReleaseCapture unconditionally
                // on the `else` branch is safe: per MSDN, ReleaseCapture is
                // a no-op when the calling thread doesn't own capture (no
                // WM_CAPTURECHANGED is dispatched).
                let we_have_capture = unsafe { GetCapture() } == hwnd;
                if we_have_capture {
                    self.self_release_pending = true;
                }
                unsafe {
                    crate::warn_on_err!(ReleaseCapture());
                }
            }
            CancelInlineRename => cancel_inline_rename(self, hwnd),
            FireAddClick => {
                if let Some(path) = self.folder_picker.pick_folder() {
                    crate::actions::append_folder_and_reload(self, &path);
                }
            }
            FireFolderClick {
                folder_button,
                ctrl,
            } => {
                // folder_button is in folder-index space; buttons[0] is the + button.
                let btn_slot = folder_button + 1;
                if btn_slot < self.buttons.len() {
                    let path = std::path::PathBuf::from(&self.buttons[btn_slot].folder.path);
                    if ctrl {
                        match self.active_target.map(|t| t.kind) {
                            Some(crate::target::TargetKind::FileDialog) => {
                                self.shell_browser.open_in_new_window(&path);
                            }
                            Some(crate::target::TargetKind::Explorer) => {
                                let timeout = self
                                    .config
                                    .as_ref()
                                    .map(|c| c.new_tab_timeout_ms_zero_disables)
                                    .unwrap_or(500);
                                if let Some(explorer) = self.active_target.map(|t| t.hwnd) {
                                    self.shell_browser.open_in_new_tab(explorer, &path, timeout);
                                }
                            }
                            None => {
                                log::debug!("FireFolderClick(ctrl): no active target");
                            }
                        }
                    } else {
                        match self.active_target.map(|t| t.kind) {
                            Some(crate::target::TargetKind::FileDialog) => {
                                if let Some(target) = self.active_target
                                    && let Err(e) = self.dialog_nav.navigate(target.hwnd, &path)
                                {
                                    log::warn!("dialog navigate failed: {e:?}");
                                }
                            }
                            Some(crate::target::TargetKind::Explorer) => {
                                crate::warn_on_err!(
                                    self.shell_browser
                                        .navigate(self.active_target.unwrap().hwnd, &path)
                                );
                            }
                            None => {
                                log::debug!("FireFolderClick: no active target");
                            }
                        }
                    }
                }
            }
            CommitReorder {
                from_folder,
                to_folder,
            } => {
                crate::actions::commit_reorder(self, hwnd, from_folder, to_folder);
            }
        }
    }

    /// Drive the rename state machine with a single event, then execute the
    /// resulting actions against Win32 + the `config_store` trait seam.
    ///
    /// Mirrors `execute_pointer_command`'s shape (SP2b). Single-threaded by
    /// the message-pump invariant — no synchronisation needed.
    pub(crate) fn execute_rename_event(&mut self, toolbar: HWND, event: RenameEvent) {
        let prior = self.rename_state.clone();
        let (next, actions) = rename::transition(prior, event);
        self.rename_state = next;

        for action in actions {
            match action {
                RenameAction::ApplyRename {
                    folder_index,
                    new_name,
                } => {
                    if let Some(mut cfg) = self.config_store.load() {
                        cfg.rename_folder(folder_index, new_name);
                        if let Err(e) = self.config_store.save(&cfg) {
                            log::error!("rename: save failed: {e}");
                        } else {
                            self.config = Some(cfg);
                        }
                    }
                }
                RenameAction::DestroyEdit { edit_hwnd } => {
                    crate::rename_edit::destroy_rename_edit(HWND(edit_hwnd as *mut _));
                }
                RenameAction::ReloadToolbar => unsafe {
                    crate::warn_on_err!(PostMessageW(
                        Some(toolbar),
                        WM_USER_RELOAD,
                        WPARAM(0),
                        LPARAM(0)
                    ));
                },
            }
        }
    }
}

// ── Layout computation and painting — see paint.rs ───────────────────────────
// ── Window procedure — see wndproc.rs ────────────────────────────────────────
// ── Inline rename glue ───────────────────────────────────────────────────────

use crate::rename::{self, RenameAction, RenameEvent};

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pointer;
    use crate::target::ActiveTarget;
    use crate::test_helpers::{
        make_test_state, mk_add_button, mk_config_with_folders, mk_deps, mk_folder_button,
    };
    use std::path::PathBuf;
    use windows::Win32::Foundation::HWND;

    // ── Tests ────────────────────────────────────────────────────────────

    #[test]
    fn fire_folder_click_without_ctrl_calls_navigate_with_folder_path() {
        let deps = mk_deps();
        let cfg = mk_config_with_folders(&[("Downloads", "C:\\Downloads")]);
        let mut state = make_test_state(&deps, Some(cfg));
        state.active_target = Some(crate::target::ActiveTarget::explorer(HWND(42 as *mut _)));
        state.buttons = vec![
            mk_add_button(),
            mk_folder_button("Downloads", "C:\\Downloads", 42),
        ];

        state.execute_pointer_command(
            HWND(std::ptr::dangling_mut()),
            pointer::PointerCommand::FireFolderClick {
                folder_button: 0,
                ctrl: false,
            },
        );

        let calls = deps.navigate_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, PathBuf::from("C:\\Downloads"));
        assert_eq!(deps.new_tab_calls.lock().unwrap().len(), 0);
    }

    #[test]
    fn fire_folder_click_with_ctrl_calls_open_in_new_tab_with_configured_timeout() {
        let deps = mk_deps();
        let cfg = Config::from_str(
            r#"{"folders":[{"name":"D","path":"C:\\D"}],"newTabTimeoutMsZeroDisables":750}"#,
        )
        .unwrap();
        let mut state = make_test_state(&deps, Some(cfg));
        state.active_target = Some(crate::target::ActiveTarget::explorer(HWND(42 as *mut _)));
        state.buttons = vec![mk_add_button(), mk_folder_button("D", "C:\\D", 42)];

        state.execute_pointer_command(
            HWND(std::ptr::dangling_mut()),
            pointer::PointerCommand::FireFolderClick {
                folder_button: 0,
                ctrl: true,
            },
        );

        let calls = deps.new_tab_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].2, 750);
        assert_eq!(deps.navigate_calls.lock().unwrap().len(), 0);
    }

    #[test]
    fn ctrl_click_in_dialog_mode_calls_open_in_new_window() {
        let deps = mk_deps();
        let cfg = mk_config_with_folders(&[("D", "C:\\D")]);
        let mut state = make_test_state(&deps, Some(cfg));
        state.active_target = Some(crate::target::ActiveTarget::file_dialog(HWND(99 as *mut _)));
        state.buttons = vec![mk_add_button(), mk_folder_button("D", "C:\\D", 42)];

        state.execute_pointer_command(
            HWND(std::ptr::dangling_mut()),
            pointer::PointerCommand::FireFolderClick {
                folder_button: 0,
                ctrl: true,
            },
        );

        let calls = deps.new_window_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], PathBuf::from("C:\\D"));
        assert!(deps.new_tab_calls.lock().unwrap().is_empty());
        assert!(deps.navigate_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn fire_folder_click_when_no_active_explorer_is_noop() {
        let deps = mk_deps();
        let cfg = mk_config_with_folders(&[("D", "C:\\D")]);
        let mut state = make_test_state(&deps, Some(cfg));
        state.buttons = vec![mk_add_button(), mk_folder_button("D", "C:\\D", 42)];
        // active_target intentionally left None.

        state.execute_pointer_command(
            HWND(std::ptr::dangling_mut()),
            pointer::PointerCommand::FireFolderClick {
                folder_button: 0,
                ctrl: false,
            },
        );

        assert!(
            deps.navigate_calls.lock().unwrap().is_empty(),
            "navigate should not be called when no active explorer"
        );
        assert!(
            deps.new_tab_calls.lock().unwrap().is_empty(),
            "open_in_new_tab should not be called when no active explorer"
        );
    }

    #[test]
    fn fire_add_click_when_picker_returns_some_appends_and_saves() {
        let deps = mk_deps();
        *deps.picker.next_result.lock().unwrap() = Some(PathBuf::from("C:\\NewFolder"));
        *deps.cfg_store.load_value.lock().unwrap() = Some(mk_config_with_folders(&[]));

        let mut state = make_test_state(&deps, None);
        state.execute_pointer_command(
            HWND(std::ptr::dangling_mut()),
            pointer::PointerCommand::FireAddClick,
        );

        assert_eq!(*deps.picker.calls.lock().unwrap(), 1);
        let saves = deps.cfg_store.save_calls.lock().unwrap();
        assert_eq!(saves.len(), 1);
        assert_eq!(saves[0].folders.len(), 1);
        assert_eq!(saves[0].folders[0].path, "C:\\NewFolder");
    }

    #[test]
    fn fire_add_click_when_picker_returns_none_is_noop() {
        let deps = mk_deps();
        *deps.picker.next_result.lock().unwrap() = None;
        let mut state = make_test_state(&deps, None);
        state.execute_pointer_command(
            HWND(std::ptr::dangling_mut()),
            pointer::PointerCommand::FireAddClick,
        );

        assert_eq!(*deps.picker.calls.lock().unwrap(), 1);
        assert_eq!(deps.cfg_store.save_calls.lock().unwrap().len(), 0);
    }

    #[test]
    fn commit_reorder_loads_modifies_saves_via_config_store() {
        let deps = mk_deps();
        *deps.cfg_store.load_value.lock().unwrap() = Some(mk_config_with_folders(&[
            ("A", "C:\\a"),
            ("B", "C:\\b"),
            ("C", "C:\\c"),
        ]));

        let mut state = make_test_state(&deps, None);
        state.execute_pointer_command(
            HWND(std::ptr::dangling_mut()),
            pointer::PointerCommand::CommitReorder {
                from_folder: 0,
                to_folder: 3,
            },
        );

        let saves = deps.cfg_store.save_calls.lock().unwrap();
        assert_eq!(saves.len(), 1);
        let names: Vec<&str> = saves[0].folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["B", "C", "A"]);
    }

    #[test]
    fn copy_folder_path_calls_clipboard_set_text_with_folder_path() {
        let deps = mk_deps();
        let mut state = make_test_state(&deps, None);
        state.buttons = vec![
            mk_add_button(),
            mk_folder_button("Target", "C:\\Target", 42),
        ];

        crate::actions::copy_folder_path_to_clipboard(&mut state, 0);

        let calls = deps.clipboard.set_text_calls.lock().unwrap();
        assert_eq!(*calls, vec!["C:\\Target".to_string()]);
    }

    // ── Rename adapter tests (SP6) ───────────────────────────────────────

    fn mk_active_rename_state(folder_index: usize) -> rename::RenameState {
        rename::RenameState {
            folder_index,
            edit_hwnd: 0xDEAD_BEEF,
        }
    }

    #[test]
    fn rename_apply_loads_mutates_saves() {
        let deps = mk_deps();
        *deps.cfg_store.load_value.lock().unwrap() =
            Some(mk_config_with_folders(&[("Old", "C:\\Old")]));
        let mut state = make_test_state(&deps, None);
        state.rename_state = Some(mk_active_rename_state(0));

        state.execute_rename_event(
            HWND(std::ptr::dangling_mut()),
            rename::RenameEvent::CommitRequested {
                text: "Renamed".into(),
            },
        );

        let saves = deps.cfg_store.save_calls.lock().unwrap();
        assert_eq!(saves.len(), 1);
        assert_eq!(saves[0].folders[0].name, "Renamed");
        assert!(
            state.rename_state.is_none(),
            "state should clear after commit"
        );
        assert_eq!(state.config.as_ref().unwrap().folders[0].name, "Renamed");
    }

    #[test]
    fn rename_apply_with_empty_text_keeps_old_name() {
        // End-to-end check that Config::rename_folder's trim-empty guard works
        // through the adapter — empty text must not change the saved name.
        let deps = mk_deps();
        *deps.cfg_store.load_value.lock().unwrap() =
            Some(mk_config_with_folders(&[("KeepMe", "C:\\K")]));
        let mut state = make_test_state(&deps, None);
        state.rename_state = Some(mk_active_rename_state(0));

        state.execute_rename_event(
            HWND(std::ptr::dangling_mut()),
            rename::RenameEvent::CommitRequested { text: "   ".into() },
        );

        let saves = deps.cfg_store.save_calls.lock().unwrap();
        assert_eq!(
            saves.len(),
            1,
            "save still runs even when name was unchanged"
        );
        assert_eq!(
            saves[0].folders[0].name, "KeepMe",
            "trim-empty kept old name"
        );
    }

    #[test]
    fn rename_apply_save_error_skips_state_update() {
        let deps = mk_deps();
        *deps.cfg_store.load_value.lock().unwrap() =
            Some(mk_config_with_folders(&[("Old", "C:\\Old")]));
        *deps.cfg_store.save_should_err.lock().unwrap() = true;
        let mut state = make_test_state(&deps, None);
        state.rename_state = Some(mk_active_rename_state(0));
        // Pre-populate state.config with the old config so we can detect non-update.
        state.config = Some(mk_config_with_folders(&[("Old", "C:\\Old")]));

        state.execute_rename_event(
            HWND(std::ptr::dangling_mut()),
            rename::RenameEvent::CommitRequested {
                text: "Renamed".into(),
            },
        );

        // save was attempted (and failed)
        assert_eq!(deps.cfg_store.save_calls.lock().unwrap().len(), 1);
        // state.config was NOT updated to the new name
        assert_eq!(state.config.as_ref().unwrap().folders[0].name, "Old");
    }

    #[test]
    fn rename_cancel_does_not_call_load_or_save() {
        let deps = mk_deps();
        let mut state = make_test_state(&deps, None);
        state.rename_state = Some(mk_active_rename_state(2));

        state.execute_rename_event(
            HWND(std::ptr::dangling_mut()),
            rename::RenameEvent::Cancelled,
        );

        assert_eq!(*deps.cfg_store.load_calls.lock().unwrap(), 0);
        assert_eq!(deps.cfg_store.save_calls.lock().unwrap().len(), 0);
        assert!(
            state.rename_state.is_none(),
            "state should clear after cancel"
        );
    }

    #[test]
    fn rename_started_when_already_active_replaces_state() {
        let deps = mk_deps();
        let mut state = make_test_state(&deps, None);

        state.execute_rename_event(
            HWND(std::ptr::dangling_mut()),
            rename::RenameEvent::Started {
                folder_index: 1,
                edit_hwnd: 0x111,
            },
        );
        state.execute_rename_event(
            HWND(std::ptr::dangling_mut()),
            rename::RenameEvent::Started {
                folder_index: 5,
                edit_hwnd: 0x555,
            },
        );

        let active = state.rename_state.as_ref().unwrap();
        assert_eq!(active.folder_index, 5);
        assert_eq!(active.edit_hwnd, 0x555);
    }

    #[test]
    fn rename_commit_when_idle_does_nothing() {
        let deps = mk_deps();
        let mut state = make_test_state(&deps, None);
        // rename_state intentionally None.

        state.execute_rename_event(
            HWND(std::ptr::dangling_mut()),
            rename::RenameEvent::CommitRequested {
                text: "ignored".into(),
            },
        );

        assert_eq!(*deps.cfg_store.load_calls.lock().unwrap(), 0);
        assert_eq!(deps.cfg_store.save_calls.lock().unwrap().len(), 0);
        assert!(state.rename_state.is_none());
    }

    #[test]
    fn click_dispatches_to_dialog_navigator_when_target_is_file_dialog() {
        let deps = mk_deps();
        let cfg = mk_config_with_folders(&[("Downloads", "C:\\Downloads")]);
        let mut state = make_test_state(&deps, Some(cfg));
        state.active_target = Some(ActiveTarget::file_dialog(HWND(99 as *mut _)));
        state.buttons = vec![
            mk_add_button(),
            mk_folder_button("Downloads", "C:\\Downloads", 42),
        ];

        state.execute_pointer_command(
            HWND(std::ptr::dangling_mut()),
            pointer::PointerCommand::FireFolderClick {
                folder_button: 0,
                ctrl: false,
            },
        );

        let dlg_calls = deps.dialog_nav.calls.borrow();
        assert_eq!(
            dlg_calls.len(),
            1,
            "dialog_nav should receive exactly one call"
        );
        assert_eq!(
            dlg_calls[0].0, 99,
            "dialog_nav should receive the file-dialog HWND"
        );
        assert_eq!(
            dlg_calls[0].1,
            PathBuf::from("C:\\Downloads"),
            "dialog_nav should receive the folder path"
        );
        // shell_browser must NOT be called
        assert_eq!(
            deps.navigate_calls.lock().unwrap().len(),
            0,
            "shell_browser.navigate must not be called for a FileDialog target"
        );
    }
}
