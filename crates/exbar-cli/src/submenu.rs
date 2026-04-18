//! Pure chain-of-popups state machine for spring-open submenus.
//!
//! The adapter (`submenu_wnd.rs`) translates mouse / timer / drag events into
//! events, calls the transition function, and dispatches returned commands
//! against Win32. No Win32 types appear here.
//!
//! Chain model: at most 5 levels of nested popups. Level 0 is the toolbar
//! button itself (not a popup); levels 1–5 are popups. Each level records
//! its folder path, `ancestor_mode` flag, and horizontal flow direction
//! (which is locked at the chain level once the first right-overflow happens).

#[allow(unused_imports)]
use std::path::PathBuf;

pub const MAX_CHAIN_DEPTH: usize = 5;

/// Horizontal flow for levels 2+. Locked per chain once set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowDir {
    Right,
    Left,
}

/// Vertical orientation of a popup relative to its triggering point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertOrient {
    Upward,
    Downward,
}

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct WorkArea {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl WorkArea {
    pub fn width(self) -> i32 {
        self.right - self.left
    }
    pub fn height(self) -> i32 {
        self.bottom - self.top
    }
    pub fn vertical_midline(self) -> i32 {
        self.top + self.height() / 2
    }
}

/// Decides the vertical orientation for a LEVEL-1 submenu based on the
/// trigger button's center Y relative to the work-area midline + the
/// "direction-for-most-options" rule (spec §3.7).
///
/// Inputs:
/// - `btn_center_y` — triggering toolbar button's center Y (screen coords).
/// - `item_count` — number of items the submenu wants to show.
/// - `item_px` — per-item height + margins, DPI-scaled.
/// - `cursor_y` — cursor Y at open time (screen coords).
/// - `work` — monitor work area.
///
/// Returns the vertical orientation. The caller uses this + parent-reshow
/// policy (§3.6) to place the popup.
pub fn resolve_level1_orientation(
    btn_center_y: i32,
    item_count: i32,
    item_px: i32,
    cursor_y: i32,
    work: WorkArea,
) -> VertOrient {
    let needed = item_count * item_px;
    let space_up = cursor_y - work.top;
    let space_down = work.bottom - cursor_y;

    let fits_up = space_up >= needed;
    let fits_down = space_down >= needed;

    match (fits_up, fits_down) {
        (true, false) => VertOrient::Upward,
        (false, true) => VertOrient::Downward,
        (true, true) => {
            // Both fit → fall back to button-half rule (spec §3.6).
            if btn_center_y >= work.vertical_midline() {
                VertOrient::Upward
            } else {
                VertOrient::Downward
            }
        }
        (false, false) => {
            // Neither fits → direction-for-most-options.
            if space_up >= space_down {
                VertOrient::Upward
            } else {
                VertOrient::Downward
            }
        }
    }
}

/// Resolve the horizontal flow direction for a chain, given the proposed X
/// of the first nested (level-2) popup and its width. If it overflows right,
/// returns `Left`; otherwise `Right`. Once returned, the adapter locks this
/// direction for the remainder of the chain.
pub fn resolve_flow_direction(proposed_right_edge_x: i32, work: WorkArea) -> FlowDir {
    if proposed_right_edge_x > work.right {
        FlowDir::Left
    } else {
        FlowDir::Right
    }
}

#[cfg(test)]
mod tests_direction {
    use super::*;

    fn wa() -> WorkArea {
        WorkArea {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    }

    #[test]
    fn level1_up_when_below_midline_both_fit() {
        let o = resolve_level1_orientation(800, 5, 30, 700, wa());
        assert_eq!(o, VertOrient::Upward);
    }

    #[test]
    fn level1_down_when_above_midline_both_fit() {
        let o = resolve_level1_orientation(200, 5, 30, 200, wa());
        assert_eq!(o, VertOrient::Downward);
    }

    #[test]
    fn level1_prefers_direction_that_fits() {
        // Only 100 px above cursor, 980 below; 5*30=150 needed.
        let o = resolve_level1_orientation(200, 5, 30, 100, wa());
        assert_eq!(o, VertOrient::Downward);
    }

    #[test]
    fn level1_most_space_when_neither_fits() {
        // 40 items * 30 px = 1200; cursor at y=300 → up=300, down=780; pick down.
        let o = resolve_level1_orientation(200, 40, 30, 300, wa());
        assert_eq!(o, VertOrient::Downward);
    }

    #[test]
    fn flow_right_when_no_overflow() {
        assert_eq!(resolve_flow_direction(1800, wa()), FlowDir::Right);
    }

    #[test]
    fn flow_left_on_right_overflow() {
        assert_eq!(resolve_flow_direction(1921, wa()), FlowDir::Left);
    }
}
