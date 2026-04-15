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

/// Layout constants (logical pixels — scaled internally by `theme::scale`).
const BTN_HEIGHT_LOGICAL_PX: i32 = 28;
const BTN_PAD_H_LOGICAL_PX: i32 = 10;
const BTN_GAP_LOGICAL_PX: i32 = 2;
const ICON_WIDTH_LOGICAL_PX: i32 = 14;
const ICON_TEXT_GAP_LOGICAL_PX: i32 = 4;
const ADD_BUTTON_SIZE_LOGICAL_PX: i32 = 28;

/// Compute button positions and total toolbar dimensions.
pub fn compute_layout(input: &LayoutInput) -> Layout {
    assert_eq!(
        input.folders.len(),
        input.folder_text_widths_physical_px.len(),
        "folders and text widths slices must have the same length",
    );

    let dpi = input.dpi;
    let s = |px: i32| crate::theme::scale(px, dpi);

    let btn_h = s(BTN_HEIGHT_LOGICAL_PX);
    let pad_h = s(BTN_PAD_H_LOGICAL_PX);
    let gap = s(BTN_GAP_LOGICAL_PX);
    let icon_w = s(ICON_WIDTH_LOGICAL_PX);
    let icon_gap = s(ICON_TEXT_GAP_LOGICAL_PX);
    let add_size = s(ADD_BUTTON_SIZE_LOGICAL_PX);
    let grip = s(input.grip_size_logical_px);

    // Compute each folder button's width: padding + icon + gap + text + padding.
    let folder_widths: Vec<i32> = input
        .folder_text_widths_physical_px
        .iter()
        .map(|&tw| pad_h + icon_w + icon_gap + tw + pad_h)
        .collect();

    let mut buttons = Vec::with_capacity(input.folders.len() + 1);

    match input.orientation {
        Orientation::Horizontal => {
            let mut x = grip;
            buttons.push(ButtonLayout {
                rect: Rect { left: x, top: 0, right: x + add_size, bottom: btn_h },
                folder: synthesized_add_button(),
                is_add: true,
            });
            x += add_size + gap;

            for (entry, &w) in input.folders.iter().zip(folder_widths.iter()) {
                buttons.push(ButtonLayout {
                    rect: Rect { left: x, top: 0, right: x + w, bottom: btn_h },
                    folder: entry.clone(),
                    is_add: false,
                });
                x += w + gap;
            }

            let total_width = x - gap;
            let total_height = btn_h;
            Layout { buttons, total_width, total_height }
        }
        Orientation::Vertical => {
            // Width of the toolbar = widest of add-button and all folder buttons.
            let max_width = folder_widths.iter().copied().max().unwrap_or(0).max(add_size);

            let mut y = grip;
            buttons.push(ButtonLayout {
                rect: Rect { left: 0, top: y, right: max_width, bottom: y + btn_h },
                folder: synthesized_add_button(),
                is_add: true,
            });
            y += btn_h + gap;

            for entry in input.folders.iter() {
                buttons.push(ButtonLayout {
                    rect: Rect { left: 0, top: y, right: max_width, bottom: y + btn_h },
                    folder: entry.clone(),
                    is_add: false,
                });
                y += btn_h + gap;
            }

            let total_width = max_width;
            let total_height = y - gap;
            Layout { buttons, total_width, total_height }
        }
    }
}

fn synthesized_add_button() -> FolderEntry {
    FolderEntry {
        name: "+".into(),
        path: String::new(),
        icon: None,
    }
}

/// Given a cursor position, compute the folder-index insertion point in
/// `0..=folder_count`.
pub fn compute_insertion_index(input: &InsertionInput) -> usize {
    let folder_buttons: Vec<&ButtonLayout> =
        input.buttons.iter().filter(|b| !b.is_add).collect();

    if folder_buttons.is_empty() {
        return 0;
    }

    match input.orientation {
        Orientation::Horizontal => {
            for (i, b) in folder_buttons.iter().enumerate() {
                let mid = (b.rect.left + b.rect.right) / 2;
                if input.cursor_x < mid {
                    return i;
                }
            }
            folder_buttons.len()
        }
        Orientation::Vertical => {
            for (i, b) in folder_buttons.iter().enumerate() {
                let mid = (b.rect.top + b.rect.bottom) / 2;
                if input.cursor_y < mid {
                    return i;
                }
            }
            folder_buttons.len()
        }
    }
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

    fn mk_folder(name: &str) -> FolderEntry {
        FolderEntry { name: name.into(), path: "C:\\test".into(), icon: None }
    }

    #[test]
    fn empty_horizontal_only_has_add_button() {
        let input = LayoutInput {
            dpi: 96,
            orientation: Orientation::Horizontal,
            folders: &[],
            folder_text_widths_physical_px: &[],
            grip_size_logical_px: 12,
        };
        let layout = compute_layout(&input);
        assert_eq!(layout.buttons.len(), 1);
        assert!(layout.buttons[0].is_add);
        assert_eq!(layout.buttons[0].rect, Rect { left: 12, top: 0, right: 40, bottom: 28 });
        assert_eq!(layout.total_height, 28);
    }

    #[test]
    fn empty_vertical_only_has_add_button() {
        let input = LayoutInput {
            dpi: 96,
            orientation: Orientation::Vertical,
            folders: &[],
            folder_text_widths_physical_px: &[],
            grip_size_logical_px: 12,
        };
        let layout = compute_layout(&input);
        assert_eq!(layout.buttons.len(), 1);
        assert!(layout.buttons[0].is_add);
        assert_eq!(layout.buttons[0].rect, Rect { left: 0, top: 12, right: 28, bottom: 40 });
        assert_eq!(layout.total_width, 28);
    }

    #[test]
    fn one_folder_horizontal_at_96_dpi() {
        let folders = [mk_folder("Downloads")];
        let widths = [50];
        let input = LayoutInput {
            dpi: 96,
            orientation: Orientation::Horizontal,
            folders: &folders,
            folder_text_widths_physical_px: &widths,
            grip_size_logical_px: 12,
        };
        let layout = compute_layout(&input);
        assert_eq!(layout.buttons.len(), 2);
        // buttons[0] = +, buttons[1] = Downloads.
        assert!(layout.buttons[0].is_add);
        assert!(!layout.buttons[1].is_add);
        // Downloads x: left edge = grip(12) + add(28) + gap(2) = 42
        // Downloads width: pad(10) + icon(14) + icon_gap(4) + text(50) + pad(10) = 88
        assert_eq!(layout.buttons[1].rect, Rect { left: 42, top: 0, right: 42 + 88, bottom: 28 });
    }

    #[test]
    fn one_folder_horizontal_at_150_percent_dpi() {
        let folders = [mk_folder("Downloads")];
        let widths = [75]; // caller's pre-scaled measurement at 144 DPI
        let input = LayoutInput {
            dpi: 144,
            orientation: Orientation::Horizontal,
            folders: &folders,
            folder_text_widths_physical_px: &widths,
            grip_size_logical_px: 12,
        };
        let layout = compute_layout(&input);
        // Every non-text constant scales by 144/96 = 1.5.
        // grip: 12*1.5 = 18
        // add_size: 28*1.5 = 42
        // gap: 2*1.5 = 3
        // pad: 10*1.5 = 15
        // icon: 14*1.5 = 21
        // icon_gap: 4*1.5 = 6
        // Downloads width = 15+21+6+75+15 = 132
        assert_eq!(layout.buttons[0].rect, Rect { left: 18, top: 0, right: 60, bottom: 42 });
        assert_eq!(layout.buttons[1].rect, Rect { left: 63, top: 0, right: 63 + 132, bottom: 42 });
    }

    #[test]
    fn three_folders_horizontal_pack_left_to_right() {
        let folders = [mk_folder("A"), mk_folder("B"), mk_folder("C")];
        let widths = [20, 40, 60];
        let input = LayoutInput {
            dpi: 96,
            orientation: Orientation::Horizontal,
            folders: &folders,
            folder_text_widths_physical_px: &widths,
            grip_size_logical_px: 12,
        };
        let layout = compute_layout(&input);
        assert_eq!(layout.buttons.len(), 4);
        for pair in layout.buttons.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            assert!(a.rect.right <= b.rect.left, "buttons must not overlap: {:?} then {:?}", a.rect, b.rect);
        }
    }

    #[test]
    fn three_folders_vertical_pack_top_to_bottom() {
        let folders = [mk_folder("A"), mk_folder("BB"), mk_folder("CCC")];
        let widths = [20, 40, 60];
        let input = LayoutInput {
            dpi: 96,
            orientation: Orientation::Vertical,
            folders: &folders,
            folder_text_widths_physical_px: &widths,
            grip_size_logical_px: 12,
        };
        let layout = compute_layout(&input);
        assert_eq!(layout.buttons.len(), 4);
        for pair in layout.buttons.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            assert!(a.rect.bottom <= b.rect.top);
        }
        // All buttons share the same width in vertical orientation.
        let w = layout.buttons[0].rect.width();
        for b in &layout.buttons {
            assert_eq!(b.rect.width(), w);
        }
        assert_eq!(layout.total_width, w);
    }

    #[test]
    fn vertical_total_width_matches_widest_folder() {
        let folders = [mk_folder("short"), mk_folder("a very long folder name")];
        let widths = [30, 200]; // pre-measured widths
        let input = LayoutInput {
            dpi: 96,
            orientation: Orientation::Vertical,
            folders: &folders,
            folder_text_widths_physical_px: &widths,
            grip_size_logical_px: 12,
        };
        let layout = compute_layout(&input);
        // Widest folder width: pad + icon + gap + 200 + pad = 10 + 14 + 4 + 200 + 10 = 238
        assert_eq!(layout.total_width, 238);
        assert_eq!(layout.buttons[0].rect.width(), 238); // + button also uses max width
    }

    #[test]
    fn zero_text_width_does_not_panic() {
        let folders = [mk_folder("")];
        let widths = [0];
        let input = LayoutInput {
            dpi: 96,
            orientation: Orientation::Horizontal,
            folders: &folders,
            folder_text_widths_physical_px: &widths,
            grip_size_logical_px: 12,
        };
        let layout = compute_layout(&input);
        // Even with zero text width, button still has padding + icon width.
        assert!(layout.buttons[1].rect.width() > 0);
    }

    #[test]
    #[should_panic(expected = "folders and text widths slices must have the same length")]
    fn mismatched_slice_lengths_panic() {
        let folders = [mk_folder("A"), mk_folder("B")];
        let widths = [50]; // only one width for two folders
        let input = LayoutInput {
            dpi: 96,
            orientation: Orientation::Horizontal,
            folders: &folders,
            folder_text_widths_physical_px: &widths,
            grip_size_logical_px: 12,
        };
        compute_layout(&input);
    }

    fn mk_button(is_add: bool, rect: Rect) -> ButtonLayout {
        ButtonLayout {
            rect,
            folder: mk_folder("x"),
            is_add,
        }
    }

    #[test]
    fn insertion_index_empty_folders_returns_zero() {
        let only_add = [mk_button(true, Rect { left: 0, top: 0, right: 30, bottom: 28 })];
        let input = InsertionInput {
            buttons: &only_add,
            orientation: Orientation::Horizontal,
            cursor_x: 100,
            cursor_y: 0,
        };
        assert_eq!(compute_insertion_index(&input), 0);
    }

    #[test]
    fn insertion_index_horizontal_left_of_first_folder() {
        // + at x=0..30, folder0 at x=40..100 (mid=70), folder1 at x=110..170 (mid=140)
        let buttons = [
            mk_button(true,  Rect { left: 0, top: 0, right: 30, bottom: 28 }),
            mk_button(false, Rect { left: 40, top: 0, right: 100, bottom: 28 }),
            mk_button(false, Rect { left: 110, top: 0, right: 170, bottom: 28 }),
        ];
        let input = InsertionInput {
            buttons: &buttons,
            orientation: Orientation::Horizontal,
            cursor_x: 50,
            cursor_y: 0,
        };
        assert_eq!(compute_insertion_index(&input), 0);
    }

    #[test]
    fn insertion_index_horizontal_right_of_last_folder() {
        let buttons = [
            mk_button(true,  Rect { left: 0, top: 0, right: 30, bottom: 28 }),
            mk_button(false, Rect { left: 40, top: 0, right: 100, bottom: 28 }),
            mk_button(false, Rect { left: 110, top: 0, right: 170, bottom: 28 }),
        ];
        let input = InsertionInput {
            buttons: &buttons,
            orientation: Orientation::Horizontal,
            cursor_x: 500,
            cursor_y: 0,
        };
        assert_eq!(compute_insertion_index(&input), 2);
    }

    #[test]
    fn insertion_index_horizontal_between_folders() {
        // folder0 mid = (40+100)/2 = 70; folder1 mid = (110+170)/2 = 140
        let buttons = [
            mk_button(true,  Rect { left: 0, top: 0, right: 30, bottom: 28 }),
            mk_button(false, Rect { left: 40, top: 0, right: 100, bottom: 28 }),
            mk_button(false, Rect { left: 110, top: 0, right: 170, bottom: 28 }),
        ];
        let input = InsertionInput {
            buttons: &buttons,
            orientation: Orientation::Horizontal,
            cursor_x: 130,
            cursor_y: 0,
        };
        assert_eq!(compute_insertion_index(&input), 1);
    }

    #[test]
    fn insertion_index_vertical_above_first_folder() {
        let buttons = [
            mk_button(true,  Rect { left: 0, top: 0,  right: 50, bottom: 30 }),
            mk_button(false, Rect { left: 0, top: 40, right: 50, bottom: 68 }),
            mk_button(false, Rect { left: 0, top: 70, right: 50, bottom: 98 }),
        ];
        let input = InsertionInput {
            buttons: &buttons,
            orientation: Orientation::Vertical,
            cursor_x: 25,
            cursor_y: 20,
        };
        assert_eq!(compute_insertion_index(&input), 0);
    }

    #[test]
    fn insertion_index_vertical_below_last_folder() {
        let buttons = [
            mk_button(true,  Rect { left: 0, top: 0,  right: 50, bottom: 30 }),
            mk_button(false, Rect { left: 0, top: 40, right: 50, bottom: 68 }),
            mk_button(false, Rect { left: 0, top: 70, right: 50, bottom: 98 }),
        ];
        let input = InsertionInput {
            buttons: &buttons,
            orientation: Orientation::Vertical,
            cursor_x: 25,
            cursor_y: 500,
        };
        assert_eq!(compute_insertion_index(&input), 2);
    }
}
