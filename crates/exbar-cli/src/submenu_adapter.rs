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
                // Header row that is meant to sit "in place" over the toolbar
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
