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

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWLP_USERDATA, GetCursorPos, GetWindowLongPtrW, PostMessageW,
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
/// Timer ID for the periodic foreground watchdog. Armed once in WM_CREATE
/// when `foreground_watchdog_ms > 0`; hides a toolbar left visible over a
/// foreign app (and optionally re-shows it). Fires every `foreground_watchdog_ms`.
pub(crate) const TIMER_FOREGROUND_WATCHDOG: usize = 8;
/// Timer ID for the foreground-storm settle re-check. One-shot, armed (and
/// re-armed) while `fg_debounce` is suppressing events; on fire it inspects
/// the real foreground once and picks the toolbar's final show/hide state.
pub(crate) const TIMER_FG_SETTLE: usize = 9;

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
    /// True between scheduling a deferred reposition (TIMER_REPOSITION, on
    /// Explorer maximize/restore/snap) and that timer firing. The foreground
    /// watchdog skips while set so its opt-in re-show can't flash the toolbar
    /// at a half-settled position mid-animation.
    pub(crate) reposition_pending: bool,
    /// Event-rate tracker for foreground-change storms. While it is settling
    /// the toolbar is forced hidden and TIMER_FG_SETTLE decides the final
    /// state once the traffic stops.
    pub(crate) fg_debounce: crate::fg_debounce::DebounceState,
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
    /// Cursor Y at the moment long-press / drag-hover fired — recorded at open time (diagnostics; level-1 placement now uses the toolbar band).
    pub(crate) last_cursor_y_on_open: i32,
    /// Triggering folder button center-Y.
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
    /// Esc was still held when the safety tick was re-armed after the Remove
    /// menu closed; the tick ignores Esc until it reads up, so cancelling the
    /// menu with Esc does not also dismiss the chain.
    pub(crate) esc_latched: bool,
    /// Tracks whether any mouse button was pressed on the PREVIOUS safety-timer tick.
    /// Used to detect a fresh button-down for the click-outside-dismiss path.
    pub(crate) prev_mouse_button_down: bool,
    // Recent Folders (Plan B):
    pub(crate) recent_tracker: crate::recent_tracker::TrackerState,
    /// Instant last-two-folders history for the Recent button toggle.
    pub(crate) toggle_history: crate::toggle_history::ToggleHistory,
    /// Folder exbar last navigated a file dialog to (dialog HWND, path); lets
    /// the toggle know where an unreadable dialog currently is.
    pub(crate) dialog_last_nav: Option<(isize, std::path::PathBuf)>,
    pub(crate) recent_list: Vec<crate::recent_list::RecentEntry>,
    pub(crate) recent_store: Box<dyn crate::recent_store::RecentStore>,
    pub(crate) clock: Box<dyn crate::clock::Clock>,
    /// Set when recent_list has been mutated since last successful save.
    pub(crate) recent_dirty: bool,
    /// Set when SetTimer(TIMER_RECENT_DEBOUNCE) is armed but not yet fired.
    pub(crate) recent_debounce_pending: bool,
    /// Hover-open controller state (rest wait, open/suppressed/away per button).
    pub(crate) hover: crate::hover_open::HoverState,
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

    // Dialog Recents (Plan: dialog-recents):
    /// Reads the newest shell dialog-MRU entry. Tests replace it with
    /// `dialog_mru::test_mocks::MockDialogMru` by assigning the field.
    pub(crate) dialog_mru: Box<dyn crate::dialog_mru::DialogMruSource>,
    /// Exe path of the file dialog last attached as `active_target`.
    /// Captured at attach time because the dialog is usually destroyed
    /// before its Save/Open reaches the MRU. Never cleared: the commit gate
    /// checks the target kind first, and the next dialog overwrites it.
    pub(crate) active_dialog_exe: Option<String>,
    /// Dialog-MRU watcher; dropping it (with this state) stops the thread.
    /// `None` in test states and if the watcher failed to start.
    pub(crate) dialog_mru_watcher: Option<crate::dialog_mru::DialogMruWatcher>,
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
            reposition_pending: false,
            fg_debounce: crate::fg_debounce::DebounceState::default(),
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
            esc_latched: false,
            prev_mouse_button_down: false,
            recent_tracker: crate::recent_tracker::TrackerState::default(),
            toggle_history: Default::default(),
            dialog_last_nav: None,
            recent_list,
            recent_store,
            clock,
            recent_dirty: false,
            recent_debounce_pending: false,
            hover: Default::default(),
            autoscroll_popup: None,
            autoscroll_dir: 0,
            reachability: std::sync::Arc::new(std::sync::RwLock::new(
                crate::reachability::ReachabilityCache::new(),
            )),
            probe_tx: None,
            dialog_mru: Box::new(crate::dialog_mru::Win32DialogMru::new()),
            active_dialog_exe: None,
            dialog_mru_watcher: None,
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
    pub(crate) fn request_probe(&self, root: &str) {
        if let Ok(mut c) = self.reachability.write() {
            c.set(root, crate::reachability::Reachability::Probing);
        }
        if let Some(tx) = self.probe_tx.as_ref() {
            let _ = tx.send(root.to_owned());
        }
    }

    /// Walk current `config.folders`, classify each path's network root,
    /// drop unreferenced cache entries, and fire a probe for any root that
    /// `needs_probe`. Idempotent — safe to call on every reload.
    pub(crate) fn request_probes_for_current_folders(&self) {
        let Some(cfg) = self.config.as_ref() else {
            return;
        };
        let mut roots: Vec<String> = Vec::new();
        for f in &cfg.folders {
            if let Some(r) = crate::reachability::classify_root(&f.path)
                && !roots.contains(&r)
            {
                roots.push(r);
            }
        }
        // Drop entries no longer referenced.
        if let Ok(mut c) = self.reachability.write() {
            c.drop_unreferenced(&roots);
        }
        // Fire probes for any root that's currently Unknown (i.e. no entry).
        for r in &roots {
            let needs = self
                .reachability
                .read()
                .map(|c| c.needs_probe(r))
                .unwrap_or(false);
            if needs {
                self.request_probe(r);
            }
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
                    if self.buttons[btn_slot].folder.kind == crate::config::FolderKind::Recent {
                        self.on_recent_button_click(hwnd, ctrl);
                    } else {
                        let path = std::path::PathBuf::from(&self.buttons[btn_slot].folder.path);
                        self.navigate_folder(hwnd, &path, ctrl);
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
        // Instant toggle history sees every navigation, even ones the dwell
        // tracker will skip (self-initiated, excluded, pinned).
        if let crate::recent_tracker::TrackerEvent::NavigationTo(ref p) = event {
            self.record_toggle_history(p);
        }
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
    pub(crate) fn schedule_recent_debounce(&mut self, toolbar: HWND) {
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
    /// Navigate the active target to `path` the way a folder-button click does:
    /// marks it self-initiated for the dwell tracker, refuses Unreachable network
    /// roots, and handles ctrl (new tab / new window) and file dialogs.
    pub(crate) fn navigate_folder(&mut self, toolbar: HWND, path: &std::path::Path, ctrl: bool) {
        // Mark as toolbar-initiated so the dwell tracker skips counting
        // our own navigation as a user-discovered folder.
        self.execute_tracker_event(toolbar, crate::recent_tracker::TrackerEvent::SelfInitiated);
        // Reachability gate: skip navigation entirely if the
        // folder's network root is currently Unreachable.
        let path_str = path.to_string_lossy();
        if let Some(root) = crate::reachability::classify_root(&path_str) {
            let r = self
                .reachability
                .read()
                .map(|c| c.get(&root))
                .unwrap_or(crate::reachability::Reachability::Unknown);
            if r == crate::reachability::Reachability::Unreachable {
                log::info!("click on unreachable folder: {path_str}");
                return;
            }
        }
        // Only a real navigation counts; with no target nothing moves.
        if self.active_target.is_some() {
            self.record_toggle_history(path);
        }
        if ctrl {
            match self.active_target.map(|t| t.kind) {
                Some(crate::target::TargetKind::FileDialog) => {
                    self.shell_browser.open_in_new_window(path);
                }
                Some(crate::target::TargetKind::Explorer) => {
                    let timeout = self
                        .config
                        .as_ref()
                        .map(|c| c.new_tab_timeout_ms_zero_disables)
                        .unwrap_or(500);
                    if let Some(explorer) = self.active_target.map(|t| t.hwnd) {
                        self.shell_browser.open_in_new_tab(explorer, path, timeout);
                    }
                }
                None => {
                    log::debug!("navigate_folder(ctrl): no active target");
                }
            }
        } else {
            match self.active_target.map(|t| t.kind) {
                Some(crate::target::TargetKind::FileDialog) => {
                    if let Some(target) = self.active_target
                        && let Err(e) = self.dialog_nav.navigate(target.hwnd, path)
                    {
                        log::warn!("dialog navigate failed: {e:?}");
                    }
                    self.note_dialog_nav(path);
                }
                Some(crate::target::TargetKind::Explorer) => {
                    crate::warn_on_err!(
                        self.shell_browser
                            .navigate(self.active_target.unwrap().hwnd, path)
                    );
                }
                None => {
                    log::debug!("navigate_folder: no active target");
                }
            }
        }
    }

    /// Navigate the active target to `path`, or open a new window (FileDialog mode)
    /// or new tab (Explorer mode with ctrl held).
    ///
    /// Used by the submenu click handler in `wndproc` to dispatch `WM_USER_SUBMENU_CLICK`.
    pub(crate) fn navigate_or_new_window_or_tab(&mut self, path: &str, ctrl: bool) {
        use crate::target::TargetKind;
        let path = std::path::Path::new(path);
        self.record_toggle_history(path);
        match (self.active_target.map(|t| t.kind), ctrl) {
            (Some(TargetKind::FileDialog), true) => {
                // Dialogs have no tabs; ctrl degrades to a new Explorer window.
                self.shell_browser.open_in_new_window(path);
            }
            (Some(TargetKind::FileDialog), false) => {
                // Plain click: drive the dialog's folder via Ctrl+L injection,
                // matching the top-level folder-button click behaviour.
                if let Some(target) = self.active_target
                    && let Err(e) = self.dialog_nav.navigate(target.hwnd, path)
                {
                    log::warn!("dialog navigate failed: {e:?}");
                }
                self.note_dialog_nav(path);
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

    /// True iff the folder button at `folder_button_index` (folder-space, not
    /// button-space) points at a network root currently `Unreachable`. Used by
    /// the right-click context menu (to disable Open / show Retry).
    /// (Paint reads the cache directly via `classify_root` instead of calling
    /// this — the indirection isn't worth the extra method call there.)
    pub(crate) fn folder_is_unreachable(&self, folder_button_index: usize) -> bool {
        let Some(cfg) = self.config.as_ref() else {
            return false;
        };
        let Some(entry) = cfg.folders.get(folder_button_index) else {
            return false;
        };
        let Some(root) = crate::reachability::classify_root(&entry.path) else {
            return false;
        };
        self.reachability
            .read()
            .map(|c| c.get(&root) == crate::reachability::Reachability::Unreachable)
            .unwrap_or(false)
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
    fn folder_is_unreachable_true_for_unreachable_unc_button() {
        use crate::reachability::Reachability;
        let deps = mk_deps();
        let cfg = mk_config_with_folders(&[("UNC", "\\\\srv\\share\\foo")]);
        let state = make_test_state(&deps, Some(cfg));
        state
            .reachability
            .write()
            .unwrap()
            .set("\\\\srv\\share", Reachability::Unreachable);

        assert!(state.folder_is_unreachable(0));
    }

    #[test]
    fn folder_is_unreachable_false_for_reachable_unc_button() {
        use crate::reachability::Reachability;
        let deps = mk_deps();
        let cfg = mk_config_with_folders(&[("UNC", "\\\\srv\\share\\foo")]);
        let state = make_test_state(&deps, Some(cfg));
        state
            .reachability
            .write()
            .unwrap()
            .set("\\\\srv\\share", Reachability::Reachable);

        assert!(!state.folder_is_unreachable(0));
    }

    #[test]
    fn folder_is_unreachable_false_for_local_path() {
        let deps = mk_deps();
        let cfg = mk_config_with_folders(&[("Local", "C:\\Users\\me")]);
        let state = make_test_state(&deps, Some(cfg));
        // Local paths classify_root → None → never unreachable.
        assert!(!state.folder_is_unreachable(0));
    }

    #[test]
    fn fire_folder_click_on_unreachable_does_not_navigate() {
        use crate::reachability::Reachability;
        let deps = mk_deps();
        let cfg = mk_config_with_folders(&[("UNC", "\\\\srv\\share\\foo")]);
        let mut state = make_test_state(&deps, Some(cfg));
        state.active_target = Some(crate::target::ActiveTarget::explorer(HWND(42 as *mut _)));
        state.buttons = vec![
            mk_add_button(),
            mk_folder_button("UNC", "\\\\srv\\share\\foo", 42),
        ];
        // Pre-seed cache as Unreachable for the network root.
        state
            .reachability
            .write()
            .unwrap()
            .set("\\\\srv\\share", Reachability::Unreachable);

        state.execute_pointer_command(
            HWND(std::ptr::dangling_mut()),
            pointer::PointerCommand::FireFolderClick {
                folder_button: 0,
                ctrl: false,
            },
        );

        // navigate should NOT have been called.
        assert_eq!(deps.navigate_calls.lock().unwrap().len(), 0);
    }

    #[test]
    fn recent_button_click_routes_to_toggle_not_pseudo_entry_path() {
        let deps = mk_deps();
        let cfg = Config::from_str(
            r#"{"folders":[{"name":"Recent","kind":"Recent"}],"recent":{"enabled":true}}"#,
        )
        .unwrap();
        let mut state = make_test_state(&deps, Some(cfg));
        state.active_target = Some(ActiveTarget::file_dialog(HWND(99 as *mut _)));
        let mut recent = mk_folder_button("Recent", "", 42);
        recent.folder.kind = crate::config::FolderKind::Recent;
        state.buttons = vec![mk_add_button(), recent];
        state
            .toggle_history
            .record(std::path::Path::new("C:\\Hist"));

        state.execute_pointer_command(
            HWND(std::ptr::dangling_mut()),
            pointer::PointerCommand::FireFolderClick {
                folder_button: 0,
                ctrl: false,
            },
        );

        let dlg_calls = deps.dialog_nav.calls.borrow();
        assert_eq!(dlg_calls.len(), 1);
        assert_eq!(dlg_calls[0].1, PathBuf::from("C:\\Hist"));
    }

    #[test]
    fn request_probes_marks_unc_root_probing_and_skips_local() {
        // Non-UNC paths classify_root → None and are never added to the cache.
        // UNC paths classify deterministically (no host-state dependency on
        // GetDriveTypeW), so we only assert about the UNC root here.
        use crate::reachability::Reachability;
        let deps = mk_deps();
        let cfg = mk_config_with_folders(&[
            ("UNC1", "\\\\srv\\share\\foo"),
            ("UNC2", "\\\\srv\\share\\other"), // same root → still 1 probe
            ("Local", "C:\\Users\\me"),
        ]);
        let state = make_test_state(&deps, Some(cfg));

        state.request_probes_for_current_folders();

        let cache = state.reachability.read().unwrap();
        assert_eq!(cache.get("\\\\srv\\share"), Reachability::Probing);
        // Local path was never classified as a network root → not in cache.
        assert_eq!(cache.get("C:"), Reachability::Unknown);
    }

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
    fn submenu_click_in_dialog_mode_navigates_dialog_not_new_window() {
        let deps = mk_deps();
        let mut state = make_test_state(&deps, None);
        state.active_target = Some(ActiveTarget::file_dialog(HWND(99 as *mut _)));

        // Plain (non-ctrl) submenu click should set the dialog's folder.
        state.navigate_or_new_window_or_tab("C:\\Sub\\Folder", false);

        let dlg_calls = deps.dialog_nav.calls.borrow();
        assert_eq!(dlg_calls.len(), 1, "dialog_nav should be called once");
        assert_eq!(dlg_calls[0].0, 99, "dialog_nav gets the file-dialog HWND");
        assert_eq!(dlg_calls[0].1, PathBuf::from("C:\\Sub\\Folder"));
        assert!(
            deps.new_window_calls.lock().unwrap().is_empty(),
            "must NOT open a new Explorer window for a plain dialog click"
        );
    }

    #[test]
    fn ctrl_submenu_click_in_dialog_mode_opens_new_window() {
        let deps = mk_deps();
        let mut state = make_test_state(&deps, None);
        state.active_target = Some(ActiveTarget::file_dialog(HWND(99 as *mut _)));

        // Ctrl submenu click: dialogs have no tabs → degrade to a new window.
        state.navigate_or_new_window_or_tab("C:\\Sub\\Folder", true);

        let calls = deps.new_window_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], PathBuf::from("C:\\Sub\\Folder"));
        assert!(deps.dialog_nav.calls.borrow().is_empty());
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
