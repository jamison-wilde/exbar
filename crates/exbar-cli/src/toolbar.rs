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

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWLP_USERDATA, GetCursorPos, GetWindowLongPtrW, GetWindowRect, PostMessageW,
};

use std::sync::Arc;

use crate::clipboard::{Clipboard, Win32Clipboard};
use crate::config::{Config, ConfigStore, JsonFileStore, Orientation};
use crate::dialog_nav::{DialogNavigator, KeybdDialogNavigator};
use crate::dragdrop::{FileOperator, Win32FileOp};
use crate::layout::ButtonLayout;
use crate::picker::{FolderPicker, Win32Picker};
use crate::pointer;
use crate::shell_windows::{ShellBrowser, Win32Shell};
use crate::subfolder_enum::{SubfolderSource, Win32SubfolderSource};
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
/// Timer ID for long-press detection — 50 ms tick while a folder button is pressed.
pub(crate) const TIMER_LONGPRESS: usize = 2;
/// Timer ID for submenu cursor-tracking + dismiss countdown (30 ms tick while any popup is open).
pub(crate) const TIMER_SUBMENU_SAFETY: usize = 3;
/// Timer ID for the 2-second debounced write of recents.json.
pub(crate) const TIMER_RECENT_DEBOUNCE: usize = 4;
/// Timer ID for the 1-second dwell + active-tab-path polling tick.
/// Armed when Recent is enabled; disarmed when disabled.
pub(crate) const TIMER_DWELL_TICK: usize = 5;
/// One-shot timer that fires after springOpenDelayMs ms of cursor rest on a
/// folder button, opening its submenu without requiring a mouse press.
pub(crate) const TIMER_HOVER_OPEN: usize = 6;
/// Timer ID for hover-driven auto-scroll inside a submenu popup. Fires every
/// ~150 ms while the cursor rests in a scrollable band (top or bottom buffer
/// of a popup with off-screen items).
pub(crate) const TIMER_SUBMENU_AUTOSCROLL: usize = 7;

// Layout constants (logical pixels, scale by DPI)
pub(crate) const BTN_PAD_H: i32 = 10;
/// Logical pixel width/height of the drag handle grip area.
pub(crate) const GRIP_SIZE: i32 = 12;
/// Submenu row height in logical pixels (DPI-scaled at render time).
/// Matches `crate::layout::BTN_HEIGHT_LOGICAL_PX` — both derive from the
/// same 26 px design token.
const SUBMENU_ROW_LOGICAL_PX: i32 = 26;

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
    /// Count of shell popup windows currently visible (e.g. Win11 context
    /// menus, class "Microsoft.UI.Content.PopupWindowSiteBridge"). While > 0
    /// the toolbar drops from HWND_TOPMOST to HWND_NOTOPMOST so the popups
    /// (also top-level windows) render above. Resets to topmost when count
    /// returns to 0.
    pub(crate) popup_open_count: u32,
    pub(crate) rename_state: Option<rename::RenameState>,
    // Submenu subsystem (SP-submenu Task 10):
    pub(crate) submenu_chain: crate::submenu::SubmenuChain,
    /// Popup HWND per open level. Index 0 = level 1. Null HWND means "slot vacated, will refill".
    pub(crate) submenu_popups: Vec<HWND>,
    pub(crate) subfolder_source: Box<dyn SubfolderSource>,
    /// Cached at construction from Config.submenu; mutated only on config reload.
    pub(crate) submenu_cfg: crate::config::SubmenuConfig,
    /// Cursor X at the moment long-press / drag-hover fired — used by level-1 placement.
    pub(crate) last_cursor_x_on_open: i32,
    /// Cursor Y at the moment long-press / drag-hover fired — used by level-1 reshow placement.
    pub(crate) last_cursor_y_on_open: i32,
    /// Triggering folder button center-Y — used by resolve_level1_orientation.
    pub(crate) last_button_center_y_on_open: i32,
    /// Triggering folder button screen rect — used for level-1 popup left-edge alignment.
    pub(crate) last_button_screen_rect: crate::layout::Rect,
    /// Instant when the last `WM_LBUTTONDOWN` landed on a folder button.
    /// Drives elapsed-ms computation for `LongPressTick` timer ticks.
    pub(crate) last_press_instant: Option<std::time::Instant>,
    /// True while the 30 ms submenu cursor-tracking timer is armed.
    /// Prevents double-arming if `open_popup_level` is called rapidly.
    pub(crate) submenu_timer_active: bool,
    /// Tracks whether the cursor was inside any popup on the PREVIOUS safety-timer tick.
    /// Used to emit `CursorExit`/`CursorReenter` only on transitions, not every tick.
    /// Initialized to `true` so that the first tick with cursor outside emits `CursorExit`.
    pub(crate) cursor_was_inside_popup: bool,
    /// Tracks whether any mouse button was pressed on the PREVIOUS safety-timer tick.
    /// Used to detect a fresh button-down for the click-outside-dismiss path.
    pub(crate) prev_mouse_button_down: bool,
    // Recent Folders (Plan B):
    pub(crate) recent_tracker: crate::recent_tracker::TrackerState,
    pub(crate) recent_list: Vec<crate::recent_list::RecentEntry>,
    pub(crate) recent_store: Box<dyn crate::recent_store::RecentStore>,
    pub(crate) clock: Box<dyn crate::clock::Clock>,
    /// Set when recent_list has been mutated since last successful save.
    pub(crate) recent_dirty: bool,
    /// Set when SetTimer(TIMER_RECENT_DEBOUNCE) is armed but not yet fired.
    pub(crate) recent_debounce_pending: bool,
    /// Button index the hover-open timer is waiting on. `None` when no wait is active.
    pub(crate) hover_open_pending_button: Option<usize>,
    /// HWND of the popup currently being auto-scrolled (`None` = no autoscroll).
    pub(crate) autoscroll_popup: Option<HWND>,
    /// Auto-scroll direction: -1 = scroll up (decrease offset), +1 = scroll down. 0 = inactive.
    pub(crate) autoscroll_dir: i32,
    // Reachability subsystem (Plan: network-folder-reachability):
    /// Shared cache of per-root reachability state. Read by wndproc on
    /// every paint/click/drop; written by the worker thread.
    pub(crate) reachability:
        std::sync::Arc<std::sync::RwLock<crate::reachability::ReachabilityCache>>,
    /// Sender end of the probe-request channel. The worker thread holds
    /// the receiver; dropping the sender on `WM_DESTROY` causes the worker
    /// to exit cleanly. `None` in test states (no worker spawned).
    pub(crate) probe_tx: Option<std::sync::mpsc::Sender<String>>,
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
            Box::new(Win32SubfolderSource::new()),
            Box::new(crate::recent_store::JsonRecentStore::new()),
            Box::new(crate::clock::SystemClock::new()),
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
        subfolder_source: Box<dyn SubfolderSource>,
        recent_store: Box<dyn crate::recent_store::RecentStore>,
        clock: Box<dyn crate::clock::Clock>,
    ) -> Self {
        let layout = config
            .as_ref()
            .map_or(Orientation::Horizontal, |c| c.layout);
        let submenu_cfg = config.as_ref().map(|c| c.submenu).unwrap_or_default();
        let recent_list = recent_store.load();
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
            popup_open_count: 0,
            rename_state: None,
            submenu_chain: crate::submenu::SubmenuChain::default(),
            submenu_popups: Vec::new(),
            subfolder_source,
            submenu_cfg,
            last_cursor_x_on_open: 0,
            last_cursor_y_on_open: 0,
            last_button_center_y_on_open: 0,
            last_button_screen_rect: crate::layout::Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            last_press_instant: None,
            submenu_timer_active: false,
            cursor_was_inside_popup: true,
            prev_mouse_button_down: false,
            recent_tracker: crate::recent_tracker::TrackerState::default(),
            recent_list,
            recent_store,
            clock,
            recent_dirty: false,
            recent_debounce_pending: false,
            hover_open_pending_button: None,
            autoscroll_popup: None,
            autoscroll_dir: 0,
            reachability: std::sync::Arc::new(std::sync::RwLock::new(
                crate::reachability::ReachabilityCache::new(),
            )),
            probe_tx: None,
        }
    }
}

// ── Reachability adapter methods ─────────────────────────────────────────────

impl ToolbarState {
    /// Spawn the reachability worker thread. Idempotent: returns immediately
    /// if `probe_tx` is already populated. Called from `WM_CREATE`.
    ///
    /// `toolbar_hwnd` is captured by the worker so it can `PostMessageW`
    /// `WM_USER_REACHABILITY_UPDATED` after each probe completes.
    pub(crate) fn spawn_reachability_worker(
        &mut self,
        toolbar_hwnd: HWND,
        probe: std::sync::Arc<dyn crate::reachability_probe::ReachabilityProbe>,
    ) {
        if self.probe_tx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let cache = std::sync::Arc::clone(&self.reachability);
        // HWND is `Send`-unsafe; pass as raw isize and reconstruct in worker.
        let hwnd_raw = toolbar_hwnd.0 as isize;
        let _ = std::thread::Builder::new()
            .name("exbar-reachability".into())
            .spawn(move || {
                while let Ok(root) = rx.recv() {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        probe.probe(&root)
                    }))
                    .unwrap_or(false);
                    let r = if result {
                        crate::reachability::Reachability::Reachable
                    } else {
                        crate::reachability::Reachability::Unreachable
                    };
                    if let Ok(mut c) = cache.write() {
                        c.set(&root, r);
                    }
                    // PostMessage is thread-safe; HWND validity is the wndproc
                    // thread's responsibility (we exit via channel disconnect
                    // before the toolbar is destroyed).
                    let hwnd = HWND(hwnd_raw as *mut _);
                    unsafe {
                        let _ = PostMessageW(
                            Some(hwnd),
                            crate::wndproc::WM_USER_REACHABILITY_UPDATED,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                }
            });
        self.probe_tx = Some(tx);
    }

    /// Mark `root` as `Probing` and send a probe request to the worker.
    /// No-op if the worker isn't spawned (test states).
    #[allow(dead_code)] // Wired in by Task 7; remove allow when first caller lands.
    pub(crate) fn request_probe(&self, root: &str) {
        if let Ok(mut c) = self.reachability.write() {
            c.set(root, crate::reachability::Reachability::Probing);
        }
        if let Some(tx) = self.probe_tx.as_ref() {
            let _ = tx.send(root.to_owned());
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
                // Start the picker in the active Explorer tab's folder if we
                // can resolve it; otherwise fall back to the picker's default
                // (%SystemDrive%\).
                let start = self.current_active_tab_path();
                if let Some(path) = self.folder_picker.pick_folder(start.as_deref()) {
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
                    // Mark as toolbar-initiated so the dwell tracker skips counting
                    // our own navigation as a user-discovered folder.
                    self.execute_tracker_event(
                        hwnd,
                        crate::recent_tracker::TrackerEvent::SelfInitiated,
                    );
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
            FireLongPress { folder_button } => {
                // folder_button is a folder-index (0-based); buttons[0] is the + button,
                // so the folder slot is at buttons[folder_button + 1].
                let Some(cfg) = self.config.as_ref() else {
                    return;
                };
                let Some(folder) = cfg.folders.get(folder_button) else {
                    return;
                };
                let raw_path = folder.path.clone();
                // Shell aliases (e.g. "shell:downloads") are not resolved here;
                // Task 15 will refine. Pass the raw string as the path.
                let resolved = raw_path.clone();

                // Record the trigger context before the mutable borrow below.
                let btn_slot = folder_button + 1;
                let button_center_y = if btn_slot < self.buttons.len() {
                    let r = &self.buttons[btn_slot].rect;
                    r.top + r.height() / 2
                } else {
                    0
                };

                let mut pt = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut pt);
                }

                self.last_button_center_y_on_open = button_center_y;
                self.last_cursor_x_on_open = pt.x;
                self.last_cursor_y_on_open = pt.y;
                self.last_button_screen_rect = self.button_screen_rect(hwnd, folder_button);

                let is_recent = matches!(folder.kind, crate::config::FolderKind::Recent);
                self.execute_submenu_event(
                    hwnd,
                    crate::submenu::SubmenuEvent::OpenRoot {
                        path: std::path::PathBuf::from(resolved),
                        button_center_y,
                        is_recent,
                    },
                );
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

// ── Recent Folders adapter methods ───────────────────────────────────────────

impl ToolbarState {
    /// Returns the filesystem path of the active Explorer tab's current folder,
    /// or `None` if no Explorer is active, the target is a file dialog, or the
    /// path can't be resolved.
    ///
    /// Uses `IShellBrowser::QueryActiveShellView` → `IFolderView::GetFolder::<IPersistFolder2>`
    /// → `GetCurFolder` → `SHGetPathFromIDListW`.
    ///
    /// # Safety
    ///
    /// Must be called on the toolbar's COM/STA thread.
    pub(crate) fn current_active_tab_path(&self) -> Option<std::path::PathBuf> {
        use crate::target::TargetKind;
        let target = self.active_target.as_ref()?;
        if target.kind != TargetKind::Explorer {
            return None;
        }
        // SAFETY: get_shell_browser_for requires STA + COM init; the wndproc
        // message-pump thread owns both.
        let browser = unsafe { crate::shell_windows::get_shell_browser_for(target.hwnd) }?;
        unsafe { crate::shell_windows::active_folder_path(&browser) }
    }

    /// Arm the 1 Hz dwell-tick timer. Idempotent — `SetTimer` with the same
    /// ID replaces an existing timer, so calling this when it's already armed
    /// is harmless.
    pub(crate) fn arm_dwell_tick(&mut self, toolbar: HWND) {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(
                Some(toolbar),
                TIMER_DWELL_TICK,
                1000,
                None,
            );
        }
    }

    /// Disarm the 1 Hz dwell-tick timer. No-op if already disarmed.
    #[allow(dead_code)]
    pub(crate) fn disarm_dwell_tick(&mut self, toolbar: HWND) {
        unsafe {
            let _ =
                windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(toolbar), TIMER_DWELL_TICK);
        }
    }
}

#[allow(dead_code)] // callers land in Tasks 8+9
impl ToolbarState {
    /// Feed a `TrackerEvent` through the pure state machine and apply returned commands.
    /// No-op if Recent is disabled in config.
    pub(crate) fn execute_tracker_event(
        &mut self,
        toolbar: HWND,
        event: crate::recent_tracker::TrackerEvent,
    ) {
        let Some(cfg) = self.config.as_ref() else {
            return;
        };
        if !cfg.recent.enabled {
            return;
        }
        let ctx = crate::recent_tracker::TrackerContext {
            dwell_threshold_seconds: cfg.recent.dwell_seconds_to_track,
            excluded_paths: &cfg.recent.excluded_paths,
        };
        let cmds = crate::recent_tracker::transition(&mut self.recent_tracker, event, &ctx);
        for cmd in cmds {
            self.dispatch_tracker_command(toolbar, cmd);
        }
    }

    /// Hook called by drop targets after a successful file operation into `dest`.
    /// Emits `ActionInFolder` through the tracker adapter, which commits the
    /// destination to the recent list immediately (regardless of dwell).
    pub(crate) fn on_drop_committed(&mut self, toolbar: HWND, dest: std::path::PathBuf) {
        self.execute_tracker_event(
            toolbar,
            crate::recent_tracker::TrackerEvent::ActionInFolder(dest),
        );
    }

    fn dispatch_tracker_command(
        &mut self,
        toolbar: HWND,
        cmd: crate::recent_tracker::TrackerCommand,
    ) {
        match cmd {
            crate::recent_tracker::TrackerCommand::CommitRecent(path) => {
                // Extract config values into locals first to avoid borrow conflict.
                let (max_count, excluded_paths) = match self.config.as_ref() {
                    Some(cfg) => (
                        cfg.recent.max_count as usize,
                        cfg.recent.excluded_paths.clone(),
                    ),
                    None => return,
                };
                let now = self.clock.now_unix_ms();
                crate::recent_list::push(
                    &mut self.recent_list,
                    &path,
                    now,
                    max_count,
                    &excluded_paths,
                );
                self.recent_dirty = true;
                self.schedule_recent_debounce(toolbar);
            }
            crate::recent_tracker::TrackerCommand::ClearDwell => {
                // State already cleared inside transition; nothing to apply here.
            }
        }
    }

    /// Arm a 2-second debounce timer if not already armed. Idempotent.
    fn schedule_recent_debounce(&mut self, toolbar: HWND) {
        if self.recent_debounce_pending {
            return;
        }
        self.recent_debounce_pending = true;
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(
                Some(toolbar),
                TIMER_RECENT_DEBOUNCE,
                2000,
                None,
            );
        }
    }

    /// Called on `WM_TIMER(TIMER_RECENT_DEBOUNCE)` or on hook shutdown. Persists
    /// the list and clears dirty+pending flags. Safe to call when nothing is dirty.
    pub(crate) fn flush_recent(&mut self, toolbar: HWND) {
        if self.recent_debounce_pending {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(
                    Some(toolbar),
                    TIMER_RECENT_DEBOUNCE,
                );
            }
            self.recent_debounce_pending = false;
        }
        if !self.recent_dirty {
            return;
        }
        match self.recent_store.save(&self.recent_list) {
            Ok(()) => {
                self.recent_dirty = false;
            }
            Err(e) => {
                log::warn!("recents.json save failed: {e:?}");
            }
        }
    }
}

// ── Layout computation and painting — see paint.rs ───────────────────────────
// ── Window procedure — see wndproc.rs ────────────────────────────────────────
// ── Inline rename glue ───────────────────────────────────────────────────────

use crate::rename::{self, RenameAction, RenameEvent};

// ── Submenu adapter (Task 10a) ────────────────────────────────────────────────

impl ToolbarState {
    /// Navigate the active target to `path`, or open a new window (FileDialog mode)
    /// or new tab (Explorer mode with ctrl held).
    ///
    /// Used by the submenu click handler in `wndproc` to dispatch `WM_USER_SUBMENU_CLICK`.
    pub(crate) fn navigate_or_new_window_or_tab(&self, path: &str, ctrl: bool) {
        use crate::target::TargetKind;
        let path = std::path::Path::new(path);
        match (self.active_target.map(|t| t.kind), ctrl) {
            (Some(TargetKind::FileDialog), _) => {
                // Dialogs have no tabs; always open a new Explorer window.
                self.shell_browser.open_in_new_window(path);
            }
            (Some(TargetKind::Explorer), true) => {
                let active_hwnd = self.active_target.map(|t| t.hwnd).unwrap_or_default();
                let timeout = self
                    .config
                    .as_ref()
                    .map(|c| c.new_tab_timeout_ms_zero_disables)
                    .unwrap_or(500);
                self.shell_browser
                    .open_in_new_tab(active_hwnd, path, timeout);
            }
            (Some(TargetKind::Explorer), false) => {
                let active_hwnd = self.active_target.map(|t| t.hwnd).unwrap_or_default();
                crate::warn_on_err!(self.shell_browser.navigate(active_hwnd, path));
            }
            (None, _) => {
                // No active target — best-effort, open new window.
                self.shell_browser.open_in_new_window(path);
            }
        }
    }

    /// Translate a `SubmenuEvent` into pure state-machine transitions + Win32 side effects.
    pub(crate) fn execute_submenu_event(
        &mut self,
        toolbar: HWND,
        ev: crate::submenu::SubmenuEvent,
    ) {
        let cmds = crate::submenu::transition(&mut self.submenu_chain, ev);
        for cmd in cmds {
            self.dispatch_submenu_command(toolbar, cmd);
        }
    }

    fn dispatch_submenu_command(&mut self, toolbar: HWND, cmd: crate::submenu::SubmenuCommand) {
        match cmd {
            crate::submenu::SubmenuCommand::OpenLevel {
                level,
                path,
                ancestor_mode,
                is_recent,
            } => {
                self.open_popup_level(toolbar, level, path, ancestor_mode, is_recent);
            }
            crate::submenu::SubmenuCommand::CloseDeeperThan { level } => {
                self.close_popups_deeper_than(toolbar, level);
            }
            crate::submenu::SubmenuCommand::CloseAll => {
                self.close_all_popups(toolbar);
            }
            crate::submenu::SubmenuCommand::SetHighlight { level, index } => {
                self.set_popup_highlight(level, index);
            }
        }
    }

    fn open_popup_level(
        &mut self,
        toolbar: HWND,
        level: u8,
        folder_path: std::path::PathBuf,
        ancestor_mode: bool,
        is_recent: bool,
    ) {
        use crate::submenu::{
            ReshowPosition, VertOrient, build_display_list, resolve_level1_orientation,
        };

        let work = self.submenu_work_area();
        let cursor_y = self.last_cursor_y_on_open;
        let btn_center_y = self.last_button_center_y_on_open;
        let item_px = self.submenu_item_px();
        let buffer_px = self.submenu_cfg.hover_buffer_px as i32;

        // Level-1 Recent button: build from the tracked recent list, not from
        // subfolder enumeration. No ".." and no parent-reshow.
        let (display_items, reshow) = if level == 1 && is_recent {
            let (pinned, include_pinned) = self
                .config
                .as_ref()
                .map(|c| {
                    let pinned: Vec<String> = c
                        .folders
                        .iter()
                        .filter(|f| f.kind == crate::config::FolderKind::Folder)
                        .map(|f| f.path.clone())
                        .collect();
                    (pinned, c.recent.include_pinned)
                })
                .unwrap_or_default();
            let filtered =
                crate::recent_list::for_display(&self.recent_list, &pinned, include_pinned);
            // Recent has no parent to reshow; orient upward/downward from btn position.
            let reshow = {
                let item_count = filtered.len().max(1) as i32; // at least 1 (placeholder)
                let orient =
                    resolve_level1_orientation(btn_center_y, item_count, item_px, cursor_y, work);
                match orient {
                    VertOrient::Upward => ReshowPosition::Last,
                    VertOrient::Downward => ReshowPosition::First,
                }
            };
            (crate::submenu::build_recent_display_list(&filtered), reshow)
        } else {
            let max_items = 200;
            let entries = match self.subfolder_source.list(&folder_path, max_items) {
                Ok(e) => e,
                Err(e) => {
                    log::warn!("subfolder list failed for {folder_path:?}: {e:?}");
                    Vec::new()
                }
            };

            // Fix 3: refuse to open an empty popup for a shell alias — path
            // resolution is a Task 15 follow-up.
            if entries.is_empty()
                && crate::config::is_shell_alias(folder_path.to_string_lossy().as_ref())
            {
                log::warn!(
                    "submenu: refusing to open empty popup for shell alias {folder_path:?}; \
                     path resolution is a Task 15 follow-up"
                );
                return;
            }

            let reshow = if level == 1 {
                let orient = resolve_level1_orientation(
                    btn_center_y,
                    entries.len() as i32,
                    item_px,
                    cursor_y,
                    work,
                );
                match orient {
                    VertOrient::Upward => ReshowPosition::Last,
                    VertOrient::Downward => ReshowPosition::First,
                }
            } else {
                ReshowPosition::None
            };

            let folder_display_name = folder_path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| folder_path.to_string_lossy().to_string());

            (
                build_display_list(
                    level,
                    &folder_path,
                    &folder_display_name,
                    ancestor_mode,
                    &entries,
                    reshow,
                ),
                reshow,
            )
        };

        let measured_w = crate::paint::measure_display_items_width(&display_items, self.dpi, level);
        let max_width_px = (measured_w + crate::theme::scale(32, self.dpi))
            .min(crate::theme::scale(400, self.dpi))
            .max(crate::theme::scale(100, self.dpi));

        let max_popup_h = work.bottom - work.top;

        // For level-1 popups, collapse the buffer on the toolbar-facing side to 0.
        // The toolbar-facing side is determined by the reshow/orient:
        //   Downward (First) → popup opens below toolbar → top edge is flush → top buffer = 0.
        //   Upward   (Last)  → popup opens above toolbar → bottom edge is flush → bottom buffer = 0.
        // For levels 2+, use symmetric buffers (no guaranteed screen edge).
        let (buffer_top, buffer_bottom) = if level == 1 {
            match reshow {
                ReshowPosition::First => (0, buffer_px), // downward: top is flush
                ReshowPosition::Last => (buffer_px, 0),  // upward: bottom is flush
                ReshowPosition::None => (buffer_px, buffer_px),
            }
        } else {
            (buffer_px, buffer_px)
        };

        let layout = crate::layout::compute_submenu_layout(
            display_items.len(),
            item_px,
            max_width_px,
            buffer_top,
            buffer_bottom,
            max_popup_h,
        );

        // Popup placement: level-1 left-edge aligns to triggering button; deeper levels right of parent.
        let (sx, sy) = if level == 1 {
            let btn = self.last_button_screen_rect;
            // Pixel-perfect alignment. Derived directly from the paint code:
            //   Button text_x  = btn.left + scale(BTN_PAD_H=10, dpi)          [paint.rs:304]
            //   Popup text_x   = popup.left + buffer_top + scale(8, dpi)      [paint.rs:537]
            // Setting them equal and solving:
            //   popup.left = btn.left + scale(10 - 8, dpi) - buffer_top
            //              = btn.left + scale(2, dpi) - buffer_top
            let align_offset = crate::theme::scale(BTN_PAD_H - 8, self.dpi);
            let x = btn.left + align_offset - buffer_top;
            // TODO(vertical toolbar): if layout is Vertical, horizontal offset should
            // push the Recent popup left/right of the toolbar instead. For now assume
            // horizontal toolbars — the dominant case.
            let y = if is_recent {
                // Recent's root submenu sits entirely above or below the toolbar —
                // never overlapping the Recent button itself. Regular folders have a
                // ParentReshow row that is meant to sit "in place" over the toolbar
                // button; Recent has no such row, so overlapping serves no purpose.
                match reshow {
                    // Popup opens downward: sit below the button entirely. buffer_top=0,
                    // so popup.top = btn.bottom + 0 = btn.bottom (flush).
                    ReshowPosition::First => btn.bottom + buffer_top,
                    // Popup opens upward: sit above the button entirely. buffer_bottom=0,
                    // so popup.bottom = btn.top - 0 = btn.top (flush).
                    ReshowPosition::Last => btn.top - layout.popup_h + buffer_bottom,
                    // Defensive — Recent at level 1 always resolves First or Last.
                    ReshowPosition::None => btn.top,
                }
            } else {
                match reshow {
                    // Popup opens downward: reshow row (first) aligns with button top.
                    // buffer_top=0 → popup.top = btn.top (reshow row flush at top).
                    ReshowPosition::First => btn.top - buffer_top,
                    // Popup opens upward: reshow row (last) aligns with button bottom.
                    // buffer_bottom=0 → popup.bottom = btn.bottom (reshow row flush at bottom).
                    ReshowPosition::Last => btn.bottom - layout.popup_h + buffer_bottom,
                    ReshowPosition::None => btn.top,
                }
            };
            (
                x.max(work.left).min(work.right - layout.popup_w),
                y.max(work.top).min(work.bottom - layout.popup_h),
            )
        } else {
            // Level 2+: place beside the parent popup with flow-direction lock.
            let parent_idx = (level as usize) - 2; // parent is one level shallower
            let parent_hwnd = self
                .submenu_popups
                .get(parent_idx)
                .copied()
                .unwrap_or(HWND(std::ptr::null_mut()));
            let parent_rect = self.get_window_screen_rect(parent_hwnd);

            // Extract anchor_top from the parent popup's highlighted item before
            // any further mutable borrows of self. We do this in a separate block
            // so the immutable borrow of popup_state ends before we mutate
            // self.submenu_chain.flow below.
            let anchor_top: i32 = if parent_hwnd.0.is_null() {
                parent_rect.top
            } else {
                unsafe {
                    crate::submenu_wnd::popup_state(parent_hwnd)
                        .and_then(|p| {
                            p.highlighted_index.and_then(|hi| {
                                // highlighted_index is in display-items space;
                                // item_rects is in visible-window space (0..visible_count).
                                // Subtract scroll_offset to get the rect index.
                                let vis_i = hi.checked_sub(p.scroll_offset)?;
                                p.layout
                                    .item_rects
                                    .get(vis_i)
                                    .map(|r| parent_rect.top + r.top)
                            })
                        })
                        .unwrap_or(parent_rect.top)
                }
            };

            // Resolve (or reuse the locked) flow direction for this chain.
            // One-way ratchet: Right can flip to Left at any deeper level if
            // the proposed right edge overflows the work area. Once Left, it
            // stays Left for the remainder of the chain (no zigzag).
            let proposed_right_x = parent_rect.right + layout.popup_w;
            let flow = match self.submenu_chain.flow {
                Some(crate::submenu::FlowDir::Left) => {
                    // Already flipped — stays flipped for the rest of the chain.
                    crate::submenu::FlowDir::Left
                }
                _ => {
                    // Either first evaluation (None) OR still Right — re-check
                    // for overflow at THIS level. Flip to Left if needed.
                    let resolved = crate::submenu::resolve_flow_direction(proposed_right_x, work);
                    self.submenu_chain.flow = Some(resolved);
                    resolved
                }
            };

            // Slide inward by buffer_px so the two popups' painted regions touch
            // rather than being separated by a double-buffer gap. The clamp below
            // still applies at screen edges.
            let x = match flow {
                crate::submenu::FlowDir::Right => parent_rect.right - buffer_px,
                crate::submenu::FlowDir::Left => parent_rect.left + buffer_px - layout.popup_w,
            };

            // Clamp both axes to the monitor work area.
            let clamped_x = x.max(work.left).min(work.right - layout.popup_w);
            let clamped_y = anchor_top.max(work.top).min(work.bottom - layout.popup_h);
            (clamped_x, clamped_y)
        };

        let base_opacity = self
            .config
            .as_ref()
            .map(|c| c.background_opacity)
            .unwrap_or(0.8);
        let popup = Box::new(crate::submenu_wnd::SubmenuPopup {
            level,
            folder_path,
            display_items,
            layout,
            highlighted_index: None,
            layered_alpha: base_opacity,
            dpi: self.dpi,
            toolbar_hwnd: toolbar,
            drop_registered: false,
            scroll_offset: 0,
            scroll_delta_accum: 0,
            last_bandhover_dir: 0,
        });

        let popup_hwnd =
            crate::submenu_wnd::create_popup(toolbar, popup, sx, sy, self.file_operator.clone());

        // Grow submenu_popups Vec to accommodate this level (1-indexed → vec index = level-1).
        if self.submenu_popups.len() < level as usize {
            self.submenu_popups
                .resize(level as usize, HWND(std::ptr::null_mut()));
        }
        // Guard against overwriting a live popup — can happen if HoverChildItem
        // fires at an already-open level (transition omits CloseDeeperThan when
        // no deeper levels exist but still emits OpenLevel).
        let slot = (level - 1) as usize;
        let existing = self.submenu_popups[slot];
        if !existing.0.is_null() {
            crate::submenu_wnd::destroy_popup(existing);
        }
        self.submenu_popups[slot] = popup_hwnd;

        // Arm the cursor-tracking safety timer if this is the first popup to open.
        if !self.submenu_timer_active {
            // Fresh chain: cursor is on the button that triggered the open, so we
            // start "inside" to avoid an immediate spurious CursorExit on the first tick.
            self.cursor_was_inside_popup = true;
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(
                    Some(toolbar),
                    TIMER_SUBMENU_SAFETY,
                    30,
                    None,
                );
            }
            self.submenu_timer_active = true;
        }
    }

    fn close_popups_deeper_than(&mut self, toolbar: HWND, level: u8) {
        while self.submenu_popups.len() > level as usize {
            if let Some(h) = self.submenu_popups.pop()
                && !h.0.is_null()
            {
                crate::submenu_wnd::destroy_popup(h);
            }
        }
        self.maybe_kill_safety_timer(toolbar);
    }

    fn close_all_popups(&mut self, toolbar: HWND) {
        while let Some(h) = self.submenu_popups.pop() {
            if !h.0.is_null() {
                crate::submenu_wnd::destroy_popup(h);
            }
        }
        // Cancel any active autoscroll timer — the target popup is gone.
        if self.autoscroll_dir != 0 {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(
                    Some(toolbar),
                    crate::toolbar::TIMER_SUBMENU_AUTOSCROLL,
                );
            }
            self.autoscroll_popup = None;
            self.autoscroll_dir = 0;
        }
        self.maybe_kill_safety_timer(toolbar);
    }

    /// Stop the cursor-tracking timer when no popups remain open.
    fn maybe_kill_safety_timer(&mut self, toolbar: HWND) {
        if self.submenu_timer_active && self.submenu_popups.is_empty() {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(
                    Some(toolbar),
                    TIMER_SUBMENU_SAFETY,
                );
            }
            self.submenu_timer_active = false;
        }
    }

    fn set_popup_highlight(&mut self, level: u8, index: Option<usize>) {
        use windows::Win32::Graphics::Gdi::InvalidateRect as InvalidateRectFn;

        let Some(&h) = self.submenu_popups.get((level as usize).saturating_sub(1)) else {
            return;
        };
        if h.0.is_null() {
            return;
        }

        // Collect the old + new item rects that need repainting, updating the
        // highlighted_index in the same pass. Only fires when the index actually
        // changed, so mouse micro-motion at the same item is a no-op.
        //
        // highlighted_index is in display-items space (0..total_count).
        // layout.item_rects is in visible-window space (0..visible_count).
        // When scroll_offset > 0 these spaces don't match — passing a display-space
        // index directly to item_rects.get() returns the WRONG row or silently
        // misses (causing stuck highlights or multi-highlight artifacts).
        // We map display→visible before lookup; off-screen items fall back to a
        // full InvalidateRect so no repaint is ever missed.
        enum Action {
            None,
            Full,
            Partial(Vec<crate::layout::Rect>),
        }

        let action = unsafe {
            match crate::submenu_wnd::popup_state(h) {
                Some(popup) if popup.highlighted_index != index => {
                    let old = popup.highlighted_index;
                    popup.highlighted_index = index;
                    let mut rects = Vec::new();
                    let mut any_offscreen = false;
                    for display_idx in [old, index].into_iter().flatten() {
                        // Map display-space index → visible-space index.
                        let vis = display_idx
                            .checked_sub(popup.scroll_offset)
                            .filter(|&v| v < popup.layout.visible_count);
                        match vis {
                            Some(v) => {
                                if let Some(r) = popup.layout.item_rects.get(v) {
                                    rects.push(*r);
                                }
                            }
                            None => any_offscreen = true,
                        }
                    }
                    if any_offscreen {
                        Action::Full
                    } else if rects.is_empty() {
                        Action::None
                    } else {
                        Action::Partial(rects)
                    }
                }
                _ => Action::None,
            }
        };

        // Invalidate only the two changed rows (old highlight + new highlight).
        // GDI's update region clips the paint loop in paint_submenu_popup so
        // unaffected rows are skipped with zero GDI work.
        // erase=false: WM_PAINT fills its own background, so no OS erase needed.
        // When either index is off-screen, fall back to full invalidation so no
        // repaint is ever missed.
        match action {
            Action::None => {}
            Action::Full => unsafe {
                let _ = InvalidateRectFn(Some(h), None, false);
            },
            Action::Partial(rects) => {
                for r in rects {
                    let win_rect = windows::Win32::Foundation::RECT {
                        left: r.left,
                        top: r.top,
                        right: r.right,
                        bottom: r.bottom,
                    };
                    unsafe {
                        let _ = InvalidateRectFn(Some(h), Some(&win_rect), false);
                    }
                }
            }
        }
    }

    /// Convert the toolbar-client-coord button rect at `folder_button` (folder index,
    /// 0-based) to screen coordinates. Returns a zero rect if the index is out of range.
    pub(crate) fn button_screen_rect(
        &self,
        toolbar: HWND,
        folder_button: usize,
    ) -> crate::layout::Rect {
        use windows::Win32::Graphics::Gdi::ClientToScreen;
        let Some(btn) = self.buttons.get(folder_button + 1) else {
            return crate::layout::Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
        };
        let mut tl = POINT {
            x: btn.rect.left,
            y: btn.rect.top,
        };
        let mut br = POINT {
            x: btn.rect.right,
            y: btn.rect.bottom,
        };
        unsafe {
            let _ = ClientToScreen(toolbar, &mut tl);
            let _ = ClientToScreen(toolbar, &mut br);
        }
        crate::layout::Rect {
            left: tl.x,
            top: tl.y,
            right: br.x,
            bottom: br.y,
        }
    }

    /// Return the work area of the primary monitor (via `SPI_GETWORKAREA`).
    /// A per-monitor variant using `active_target` can replace this in Task 13.
    fn submenu_work_area(&self) -> crate::submenu::WorkArea {
        // Reuse the existing work_area_for helper (which calls MonitorFromWindow when
        // given an HWND, or falls back to SPI_GETWORKAREA for the primary monitor).
        let ref_hwnd = self.active_target.map(|t| t.hwnd);
        let rect = crate::position::work_area_for(ref_hwnd);
        crate::submenu::WorkArea {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }

    /// DPI-scaled per-item row height for submenus.
    /// Uses [`SUBMENU_ROW_LOGICAL_PX`], which matches `layout::BTN_HEIGHT_LOGICAL_PX`.
    fn submenu_item_px(&self) -> i32 {
        theme::scale(SUBMENU_ROW_LOGICAL_PX, self.dpi)
    }

    /// Get the screen rect of a window. Returns a zero rect if hwnd is null.
    fn get_window_screen_rect(&self, hwnd: HWND) -> crate::submenu::WorkArea {
        if hwnd.0.is_null() {
            return crate::submenu::WorkArea {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
        }
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(hwnd, &mut r);
        }
        crate::submenu::WorkArea {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
}

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
