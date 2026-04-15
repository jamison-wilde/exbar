//! Pure-data toolbar layout: given folder names, their measured text widths,
//! DPI, orientation, and the grip size, compute the positions of every
//! button.
//!
//! No Win32 dependencies. No string measurement (caller pre-measures).
//! Fully unit-testable and `proptest`-friendly.

use crate::config::{FolderEntry, Orientation};

/// Plain POD rect in physical pixels, origin top-left.
/// Exists so layout.rs has zero Win32 dependencies. The toolbar adapter
/// converts to/from `windows::Win32::Foundation::RECT` at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// Fully-specified input for `compute_layout`.
pub struct LayoutInput<'a> {
    /// Physical DPI of the target monitor (96 = 100%).
    pub dpi: u32,
    /// Orientation of the toolbar.
    pub orientation: Orientation,
    /// User-configured folders. The `+` button is synthesized by layout as
    /// the first slot; do not include it here.
    pub folders: &'a [FolderEntry],
    /// Measured text width (physical pixels at the input DPI) for each
    /// folder's rendered label. Same length as `folders`.
    pub folder_text_widths_physical_px: &'a [i32],
    /// Grip-area size in logical pixels (scaled internally by `theme::scale`).
    pub grip_size_logical_px: i32,
}

/// Placement of one button in toolbar-client coordinates (physical pixels).
#[derive(Debug, Clone, PartialEq)]
pub struct ButtonLayout {
    pub rect: Rect,
    pub folder: FolderEntry,
    pub is_add: bool,
}

/// Result of `compute_layout`.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub buttons: Vec<ButtonLayout>,
    pub total_width: i32,
    pub total_height: i32,
}

/// Input for `compute_insertion_index`.
pub struct InsertionInput<'a> {
    pub buttons: &'a [ButtonLayout],
    pub orientation: Orientation,
    pub cursor_x: i32,
    pub cursor_y: i32,
}

/// Compute button positions and total toolbar dimensions.
pub fn compute_layout(_input: &LayoutInput) -> Layout {
    unimplemented!("Task 6 implements compute_layout")
}

/// Given a cursor position, compute the folder-index insertion point in
/// `0..=folder_count`.
pub fn compute_insertion_index(_input: &InsertionInput) -> usize {
    unimplemented!("Task 7 implements compute_insertion_index")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_width_and_height() {
        let r = Rect { left: 10, top: 20, right: 50, bottom: 60 };
        assert_eq!(r.width(), 40);
        assert_eq!(r.height(), 40);
    }

    #[test]
    fn rect_contains_point() {
        let r = Rect { left: 10, top: 20, right: 50, bottom: 60 };
        assert!(r.contains(10, 20));         // top-left corner inclusive
        assert!(r.contains(30, 40));         // center
        assert!(!r.contains(50, 40));        // right edge exclusive
        assert!(!r.contains(30, 60));        // bottom edge exclusive
        assert!(!r.contains(9, 30));         // outside left
        assert!(!r.contains(30, 19));        // outside top
    }

    #[test]
    fn rect_zero_size_contains_nothing() {
        let r = Rect { left: 10, top: 10, right: 10, bottom: 10 };
        assert!(!r.contains(10, 10));
        assert!(!r.contains(0, 0));
    }
}
