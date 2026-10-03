//! Submenu (spring-open popup chain) adapter on `ToolbarState`.
//!
//! Translates [`crate::submenu`] events/commands into popup-window side
//! effects: opening and placing popups, closing them, highlight repaint, and
//! the safety-timer lifecycle. Split out of `toolbar.rs` (2026-10-03) to keep
//! that file under the 1,500-line cap; behaviour is unchanged by the move.

use crate::theme;
use crate::toolbar::{BTN_PAD_H, TIMER_SUBMENU_SAFETY, ToolbarState};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

/// Submenu row height in logical pixels (DPI-scaled at render time).
/// Matches `crate::layout::BTN_HEIGHT_LOGICAL_PX` — both derive from the
/// same 26 px design token.
const SUBMENU_ROW_LOGICAL_PX: i32 = 26;

/// What a popup level lists, loaded before its header position is known.
enum LevelContents {
    /// Level-1 Recent root: the filtered recent list.
    Recent(Vec<crate::recent_list::RecentEntry>),
    /// A real folder's subfolders.
    Folder(Vec<crate::subfolder_enum::SubfolderEntry>),
}

const MENU_ID_REMOVE_RECENT: u32 = 301;

impl ToolbarState {
    /// Translate a `SubmenuEvent` into pure state-machine transitions + Win32 side effects.
    pub(crate) fn execute_submenu_event(
        &mut self,
        toolbar: HWND,
        ev: crate::submenu::SubmenuEvent,
    ) {
        // Reachability gate: suppress root-popup opens on Unreachable network
        // folders so we never trigger the read_dir hang on a disconnected share.
        if let crate::submenu::SubmenuEvent::OpenRoot { ref path, .. } = ev {
            let path_str = path.to_string_lossy();
            if let Some(root) = crate::reachability::classify_root(&path_str) {
                let r = self
                    .reachability
                    .read()
                    .map(|c| c.get(&root))
                    .unwrap_or(crate::reachability::Reachability::Unknown);
                if r == crate::reachability::Reachability::Unreachable {
                    log::info!("submenu open suppressed (unreachable): {path_str}");
                    return;
                }
            }
        }
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
        use crate::submenu::{ReshowPosition, build_display_list, level1_y, place_level1};

        let work = self.submenu_work_area();
        let item_px = self.submenu_item_px();
        let buffer_px = self.submenu_cfg.hover_buffer_px as i32;

        let Some(contents) = self.load_level_contents(level, &folder_path, is_recent) else {
            // OpenRoot/HoverChildItem already pushed this level into the pure
            // chain; drop it so no phantom open chain outlives the refusal.
            self.submenu_chain.levels.truncate(level as usize - 1);
            if self.submenu_chain.levels.is_empty() {
                self.submenu_chain.flow = None;
            }
            return;
        };
        let folder_display_name = folder_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| folder_path.to_string_lossy().to_string());
        let build = |pos: ReshowPosition| -> Vec<crate::submenu::DisplayItem> {
            match &contents {
                LevelContents::Recent(list) => crate::submenu::build_recent_display_list(list),
                LevelContents::Folder(entries) => build_display_list(
                    level,
                    &folder_path,
                    &folder_display_name,
                    ancestor_mode,
                    entries,
                    pos,
                ),
            }
        };
        let width_for = |items: &[crate::submenu::DisplayItem]| -> i32 {
            let measured_w = crate::paint::measure_display_items_width(items, self.dpi, level);
            (measured_w + crate::theme::scale(32, self.dpi))
                .min(crate::theme::scale(400, self.dpi))
                .max(crate::theme::scale(100, self.dpi))
        };

        let mut start_scrolled_to_end = false;
        let (display_items, layout, sx, sy) = if level == 1 {
            // Level 1 sits beside the toolbar band, never over it; the
            // toolbar-facing side carries no hover buffer.
            let (band_top, band_bottom) = self.level1_band(toolbar);
            let count = build(ReshowPosition::First).len() as i32;
            let needed_h = count * item_px + buffer_px;
            let placement = place_level1(band_top, band_bottom, needed_h, work);
            let display_items = build(placement.side.header_position());
            let (buffer_top, buffer_bottom) = match placement.side {
                crate::submenu::Side::Below => (0, buffer_px),
                crate::submenu::Side::Above => (buffer_px, 0),
            };
            let layout = crate::layout::compute_submenu_layout(
                display_items.len(),
                item_px,
                width_for(&display_items),
                buffer_top,
                buffer_bottom,
                placement.max_h,
            );
            // Pixel-perfect text alignment with the button:
            //   Button text_x = btn.left + scale(BTN_PAD_H, dpi)
            //   Popup text_x  = popup.left + buffer_top + scale(8, dpi)
            let btn = self.last_button_screen_rect;
            let align_offset = crate::theme::scale(BTN_PAD_H - 8, self.dpi);
            let x = (btn.left + align_offset - buffer_top)
                .max(work.left)
                .min(work.right - layout.popup_w);
            let y = level1_y(placement, band_top, band_bottom, layout.popup_h);
            // Above popups put the header at the toolbar-facing bottom end;
            // start at the end of the list so it is not hidden when scrolling.
            start_scrolled_to_end = placement.side == crate::submenu::Side::Above;
            (display_items, layout, x, y)
        } else {
            let display_items = build(ReshowPosition::None);
            let layout = crate::layout::compute_submenu_layout(
                display_items.len(),
                item_px,
                width_for(&display_items),
                buffer_px,
                buffer_px,
                work.bottom - work.top,
            );
            let (x, y) = self.leveln_origin(level, &layout, buffer_px, work);
            (display_items, layout, x, y)
        };

        let scroll_offset = if start_scrolled_to_end {
            layout.total_count.saturating_sub(layout.visible_count)
        } else {
            0
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
            scroll_offset,
            scroll_delta_accum: 0,
            last_bandhover_dir: 0,
        });
        self.install_popup(toolbar, level, popup, sx, sy);
    }

    /// Load what `folder_path` shows at `level`. `None` = refuse to open
    /// (empty shell alias — existing rule, moved here unchanged).
    fn load_level_contents(
        &self,
        level: u8,
        folder_path: &std::path::Path,
        is_recent: bool,
    ) -> Option<LevelContents> {
        // Level-1 Recent button: the tracked recent list, not subfolder
        // enumeration.
        if level == 1 && is_recent {
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
            return Some(LevelContents::Recent(crate::recent_list::for_display(
                &self.recent_list,
                &pinned,
                include_pinned,
            )));
        }
        let max_items = 200;
        let entries = match self.subfolder_source.list(folder_path, max_items) {
            Ok(e) => e,
            Err(e) => {
                log::warn!("subfolder list failed for {folder_path:?}: {e:?}");
                Vec::new()
            }
        };
        // Refuse to open an empty popup for a shell alias — path resolution
        // is a Task 15 follow-up.
        if entries.is_empty()
            && crate::config::is_shell_alias(folder_path.to_string_lossy().as_ref())
        {
            log::warn!(
                "submenu: refusing to open empty popup for shell alias {folder_path:?}; \
                 path resolution is a Task 15 follow-up"
            );
            return None;
        }
        Some(LevelContents::Folder(entries))
    }

    /// Screen band a level-1 popup must not cross: the toolbar window for a
    /// horizontal toolbar, the triggering button for a vertical one (side
    /// placement for vertical bars is out of scope; this keeps them usable).
    fn level1_band(&self, toolbar: HWND) -> (i32, i32) {
        if self.layout == crate::config::Orientation::Vertical {
            let b = self.last_button_screen_rect;
            return (b.top, b.bottom);
        }
        let r = self.get_window_screen_rect(toolbar);
        (r.top, r.bottom)
    }

    /// Level 2+: beside the parent popup, with the chain's flow-direction
    /// lock. Moved verbatim from `open_popup_level`.
    fn leveln_origin(
        &mut self,
        level: u8,
        layout: &crate::layout::SubmenuLayout,
        buffer_px: i32,
        work: crate::submenu::WorkArea,
    ) -> (i32, i32) {
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
    }

    /// Create the popup window, slot it at `level`, and arm the safety timer
    /// for a fresh chain. Moved verbatim from `open_popup_level`.
    fn install_popup(
        &mut self,
        toolbar: HWND,
        level: u8,
        popup: Box<crate::submenu_wnd::SubmenuPopup>,
        sx: i32,
        sy: i32,
    ) {
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

impl ToolbarState {
    /// Drop `path` from Recents and schedule the debounced save.
    pub(crate) fn remove_recent(&mut self, toolbar: HWND, path: &std::path::Path) -> bool {
        if !crate::recent_list::remove(&mut self.recent_list, path) {
            log::debug!("recent: remove {} - not in list", path.display());
            return false;
        }
        self.recent_dirty = true;
        self.schedule_recent_debounce(toolbar);
        true
    }

    /// Right-click on popup item `idx` at `level`. Only level-1 items of a
    /// Recent chain get a menu (Remove from Recents).
    pub(crate) fn on_popup_right_click(&mut self, toolbar: HWND, level: u8, idx: usize) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
        use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, KillTimer, SetTimer};
        if level != 1 {
            return;
        }
        let Some(root) = self.submenu_chain.levels.first().cloned() else {
            return;
        };
        if !root.is_recent {
            return;
        }
        let Some(&popup) = self.submenu_popups.first() else {
            return;
        };
        let item = unsafe {
            crate::submenu_wnd::popup_state(popup).and_then(|p| p.display_items.get(idx).cloned())
        };
        let Some(crate::submenu::DisplayItem::Subfolder { entry }) = item else {
            return;
        };

        // The modal loop of the menu would otherwise run the safety tick, which
        // dismisses the chain as the cursor moves onto the menu.
        unsafe {
            let _ = KillTimer(Some(toolbar), TIMER_SUBMENU_SAFETY);
        }
        self.submenu_timer_active = false;

        let mut pt = windows::Win32::Foundation::POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let items = [crate::contextmenu::MenuItem {
            id: MENU_ID_REMOVE_RECENT,
            label: "Remove from Recents",
            disabled: false,
        }];
        let chosen = crate::contextmenu::show_menu(toolbar, pt, &items);

        let removed = chosen == MENU_ID_REMOVE_RECENT && self.remove_recent(toolbar, &entry.path);
        // A chain closed during the modal loop must not reopen unprompted.
        if removed && self.submenu_chain.is_open() {
            // Reopen the Recents root so the list (or its placeholder) refreshes.
            self.execute_submenu_event(
                toolbar,
                crate::submenu::SubmenuEvent::OpenRoot {
                    path: root.path,
                    button_center_y: self.last_button_center_y_on_open,
                    is_recent: true,
                },
            );
        } else if self.submenu_chain.is_open() {
            self.cursor_was_inside_popup = true;
            // Esc that cancelled the menu may still read as down on the next tick.
            self.esc_latched =
                unsafe { (GetAsyncKeyState(VK_ESCAPE.0 as i32) as u16) & 0x8000 != 0 };
            unsafe {
                let _ = SetTimer(Some(toolbar), TIMER_SUBMENU_SAFETY, 30, None);
            }
            self.submenu_timer_active = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::recent_list::RecentEntry;
    use crate::test_helpers::{make_test_state, mk_config_with_folders, mk_deps};
    use crate::toolbar::ToolbarState;
    use std::path::{Path, PathBuf};
    use windows::Win32::Foundation::HWND;

    // Not a real window: SetTimer in the debounce path fails harmlessly.
    fn toolbar() -> HWND {
        HWND(std::ptr::dangling_mut())
    }

    fn recent_state(paths: &[&str]) -> ToolbarState {
        let deps = mk_deps();
        let mut cfg = mk_config_with_folders(&[("A", "C:\\A")]);
        cfg.recent.enabled = true;
        let mut state = make_test_state(&deps, Some(cfg));
        state.recent_list = paths
            .iter()
            .map(|p| RecentEntry {
                path: PathBuf::from(p),
                last_accessed_unix_ms: 0,
            })
            .collect();
        state.recent_dirty = false;
        state
    }

    #[test]
    fn remove_recent_drops_entry_and_marks_dirty() {
        let mut state = recent_state(&["C:\\A", "C:\\B"]);
        assert!(state.remove_recent(toolbar(), Path::new("C:\\A")));
        assert_eq!(state.recent_list.len(), 1);
        assert!(state.recent_dirty);
    }

    #[test]
    fn remove_recent_absent_is_noop() {
        let mut state = recent_state(&["C:\\A"]);
        assert!(!state.remove_recent(toolbar(), Path::new("C:\\Z")));
        assert!(!state.recent_dirty);
    }

    #[test]
    fn remove_last_recent_leaves_placeholder_list() {
        let mut state = recent_state(&["C:\\A"]);
        state.remove_recent(toolbar(), Path::new("C:\\A"));
        let items = crate::submenu::build_recent_display_list(&state.recent_list);
        assert!(matches!(
            items.as_slice(),
            [crate::submenu::DisplayItem::Empty { .. }]
        ));
    }

    #[test]
    fn empty_shell_alias_open_leaves_no_phantom_chain() {
        let deps = mk_deps();
        let mut state = make_test_state(
            &deps,
            Some(mk_config_with_folders(&[("D", "shell:downloads")])),
        );
        state.execute_submenu_event(
            toolbar(),
            crate::submenu::SubmenuEvent::OpenRoot {
                path: PathBuf::from("shell:downloads"),
                button_center_y: 10,
                is_recent: false,
            },
        );
        assert!(!state.submenu_chain.is_open());
        assert!(state.submenu_chain.flow.is_none());
        assert!(state.submenu_popups.is_empty());
    }

    #[test]
    fn remove_then_flush_saves_shortened_list() {
        use crate::recent_store::test_mocks::MockRecentStore;
        use crate::test_helpers::RecentStoreArc;
        let mut state = recent_state(&["C:\\A", "C:\\B"]);
        let store = std::sync::Arc::new(MockRecentStore::default());
        state.recent_store = Box::new(RecentStoreArc(store.clone()));
        assert!(state.remove_recent(toolbar(), Path::new("C:\\A")));
        state.flush_recent(toolbar());
        assert_eq!(*store.save_calls.lock().unwrap(), 1);
        let saved = store.stored.lock().unwrap().clone();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].path, PathBuf::from("C:\\B"));
    }
}
