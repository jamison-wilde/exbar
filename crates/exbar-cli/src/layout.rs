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
const BTN_HEIGHT_LOGICAL_PX: i32 = 26;
const BTN_PAD_H_LOGICAL_PX: i32 = 0;
const BTN_GAP_LOGICAL_PX: i32 = 0;
const ICON_WIDTH_LOGICAL_PX: i32 = 14;
const ICON_TEXT_GAP_LOGICAL_PX: i32 = 4;
const ADD_BUTTON_SIZE_LOGICAL_PX: i32 = 20;

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
                rect: Rect {
                    left: x,
                    top: 0,
                    right: x + add_size,
                    bottom: btn_h,
                },
                folder: synthesized_add_button(),
                is_add: true,
            });
            x += add_size + gap;

            for (entry, &w) in input.folders.iter().zip(folder_widths.iter()) {
                buttons.push(ButtonLayout {
                    rect: Rect {
                        left: x,
                        top: 0,
                        right: x + w,
                        bottom: btn_h,
                    },
                    folder: entry.clone(),
                    is_add: false,
                });
                x += w + gap;
            }

            let total_width = x - gap;
            let total_height = btn_h;
            Layout {
                buttons,
                total_width,
                total_height,
            }
        }
        Orientation::Vertical => {
            // Width of the toolbar = widest of add-button and all folder buttons.
            let max_width = folder_widths
                .iter()
                .copied()
                .max()
                .unwrap_or(0)
                .max(add_size);

            let mut y = grip;
            buttons.push(ButtonLayout {
                rect: Rect {
                    left: 0,
                    top: y,
                    right: max_width,
                    bottom: y + btn_h,
                },
                folder: synthesized_add_button(),
                is_add: true,
            });
            y += btn_h + gap;

            for entry in input.folders.iter() {
                buttons.push(ButtonLayout {
                    rect: Rect {
                        left: 0,
                        top: y,
                        right: max_width,
                        bottom: y + btn_h,
                    },
                    folder: entry.clone(),
                    is_add: false,
                });
                y += btn_h + gap;
            }

            let total_width = max_width;
            let total_height = y - gap;
            Layout {
                buttons,
                total_width,
                total_height,
            }
        }
    }
}

fn synthesized_add_button() -> FolderEntry {
    FolderEntry {
        name: "+".into(),
        path: String::new(),
        icon: None,
        kind: crate::config::FolderKind::Folder,
    }
}

/// Given a cursor position, compute the folder-index insertion point in
/// `0..=folder_count`.
pub fn compute_insertion_index(input: &InsertionInput) -> usize {
    let folder_buttons: Vec<&ButtonLayout> = input.buttons.iter().filter(|b| !b.is_add).collect();

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

/// Where a popup's pinned header row sits (it never scrolls).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderPin {
    None,
    Top,
    Bottom,
}

/// What lies under a client point of a submenu popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupHit {
    /// Display index (header included: Top = 0, Bottom = `total_count`).
    Item(usize),
    ArrowUp,
    ArrowDown,
    Nothing,
}

/// Result of `compute_submenu_layout`: the popup's overall size + each row's
/// rect (relative to the popup's client-area origin, i.e. (0,0) = top-left).
#[derive(Debug, Clone, PartialEq)]
pub struct SubmenuLayout {
    /// Visible scrolling rows only (not the header, not the arrows).
    pub item_rects: Vec<Rect>,
    pub popup_w: i32,
    pub popup_h: i32,
    /// Pure forgiveness zone at the top, in physical pixels. May be 0 when the
    /// top edge is flush with the toolbar.
    pub buffer_top_px: i32,
    /// Pure forgiveness zone at the bottom, in physical pixels.
    pub buffer_bottom_px: i32,
    /// Number of scrolling rows visible (<= `total_count`).
    pub visible_count: usize,
    /// Scrolling rows the caller wanted to show (excludes the pinned header).
    pub total_count: usize,
    pub header: HeaderPin,
    pub header_rect: Option<Rect>,
    /// Present only when the scrolling rows overflow.
    pub arrow_up_rect: Option<Rect>,
    pub arrow_down_rect: Option<Rect>,
}

impl SubmenuLayout {
    fn header_offset(&self) -> usize {
        usize::from(self.header == HeaderPin::Top)
    }

    /// Map a client point to what is under it. All display-index <-> row
    /// mapping lives here; callers must not do scroll arithmetic themselves.
    pub fn hit(&self, x: i32, y: i32, scroll_offset: usize) -> PopupHit {
        if self.header_rect.is_some_and(|r| r.contains(x, y)) {
            return PopupHit::Item(match self.header {
                HeaderPin::Top => 0,
                _ => self.total_count,
            });
        }
        if self.arrow_up_rect.is_some_and(|r| r.contains(x, y)) {
            return PopupHit::ArrowUp;
        }
        if self.arrow_down_rect.is_some_and(|r| r.contains(x, y)) {
            return PopupHit::ArrowDown;
        }
        for (i, r) in self.item_rects.iter().enumerate() {
            if r.contains(x, y) {
                return PopupHit::Item(scroll_offset + i + self.header_offset());
            }
        }
        PopupHit::Nothing
    }

    /// Client rect of display item `idx` if currently visible (the header is
    /// always visible).
    pub fn rect_for_display_index(&self, idx: usize, scroll_offset: usize) -> Option<Rect> {
        let is_header = match self.header {
            HeaderPin::Top => idx == 0,
            HeaderPin::Bottom => idx == self.total_count,
            HeaderPin::None => false,
        };
        if is_header {
            return self.header_rect;
        }
        let scroll_idx = idx.checked_sub(self.header_offset())?;
        let row = scroll_idx.checked_sub(scroll_offset)?;
        if row >= self.visible_count {
            return None;
        }
        self.item_rects.get(row).copied()
    }

    pub fn can_scroll_up(&self, scroll_offset: usize) -> bool {
        scroll_offset > 0
    }

    pub fn can_scroll_down(&self, scroll_offset: usize) -> bool {
        scroll_offset + self.visible_count < self.total_count
    }

    pub fn max_scroll_offset(&self) -> usize {
        self.total_count.saturating_sub(self.visible_count)
    }
}

/// Compute a vertical-stack layout for a submenu popup.
///
/// `scroll_count` excludes a pinned header. Order top to bottom:
/// Top `header, up, rows, down`, Bottom `up, rows, down, header`, None
/// `up, rows, down`; the arrow rows exist only when the rows overflow
/// `max_popup_h`. Buffers are pure forgiveness zones. At least one row stays
/// visible even if that exceeds `max_popup_h`.
#[allow(clippy::too_many_arguments)]
pub fn compute_submenu_layout(
    scroll_count: usize,
    header: HeaderPin,
    item_px: i32,
    arrow_px: i32,
    max_width_px: i32,
    buffer_top_px: i32,
    buffer_bottom_px: i32,
    max_popup_h: i32,
) -> SubmenuLayout {
    let header_h = if header == HeaderPin::None {
        0
    } else {
        item_px
    };
    let buffers = buffer_top_px + buffer_bottom_px;
    let want_h = buffers + header_h + (scroll_count as i32) * item_px;
    let overflow = want_h > max_popup_h;
    let arrows_h = if overflow { 2 * arrow_px } else { 0 };
    let visible_count = if overflow {
        let avail = max_popup_h - buffers - header_h - arrows_h;
        ((avail / item_px.max(1)).max(1) as usize).min(scroll_count)
    } else {
        scroll_count
    };
    let popup_h = buffers + header_h + arrows_h + (visible_count as i32) * item_px;
    let left = buffer_top_px;
    let right = buffer_top_px + max_width_px;
    let row = |top: i32, h: i32| Rect {
        left,
        top,
        right,
        bottom: top + h,
    };
    let mut y = buffer_top_px;
    let mut header_rect = None;
    if header == HeaderPin::Top {
        header_rect = Some(row(y, item_px));
        y += item_px;
    }
    let arrow_up_rect = overflow.then(|| row(y, arrow_px));
    if overflow {
        y += arrow_px;
    }
    let item_rects: Vec<Rect> = (0..visible_count)
        .map(|i| row(y + (i as i32) * item_px, item_px))
        .collect();
    y += (visible_count as i32) * item_px;
    let arrow_down_rect = overflow.then(|| row(y, arrow_px));
    if overflow {
        y += arrow_px;
    }
    if header == HeaderPin::Bottom {
        header_rect = Some(row(y, item_px));
    }
    SubmenuLayout {
        item_rects,
        popup_w: max_width_px + buffers,
        popup_h,
        buffer_top_px,
        buffer_bottom_px,
        visible_count,
        total_count: scroll_count,
        header,
        header_rect,
        arrow_up_rect,
        arrow_down_rect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_width_and_height() {
        let r = Rect {
            left: 10,
            top: 20,
            right: 50,
            bottom: 60,
        };
        assert_eq!(r.width(), 40);
        assert_eq!(r.height(), 40);
    }

    #[test]
    fn rect_contains_point() {
        let r = Rect {
            left: 10,
            top: 20,
            right: 50,
            bottom: 60,
        };
        assert!(r.contains(10, 20)); // top-left corner inclusive
        assert!(r.contains(30, 40)); // center
        assert!(!r.contains(50, 40)); // right edge exclusive
        assert!(!r.contains(30, 60)); // bottom edge exclusive
        assert!(!r.contains(9, 30)); // outside left
        assert!(!r.contains(30, 19)); // outside top
    }

    #[test]
    fn rect_zero_size_contains_nothing() {
        let r = Rect {
            left: 10,
            top: 10,
            right: 10,
            bottom: 10,
        };
        assert!(!r.contains(10, 10));
        assert!(!r.contains(0, 0));
    }

    fn mk_folder(name: &str) -> FolderEntry {
        FolderEntry {
            name: name.into(),
            path: "C:\\test".into(),
            icon: None,
            kind: crate::config::FolderKind::Folder,
        }
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
        // add button: left=grip(12), width=20, height=26
        assert_eq!(
            layout.buttons[0].rect,
            Rect {
                left: 12,
                top: 0,
                right: 32,
                bottom: 26
            }
        );
        assert_eq!(layout.total_height, 26);
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
        // add button: top=grip(12), width=20, height=26
        assert_eq!(
            layout.buttons[0].rect,
            Rect {
                left: 0,
                top: 12,
                right: 20,
                bottom: 38
            }
        );
        assert_eq!(layout.total_width, 20);
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
        // Downloads x: left edge = grip(12) + add(20) + gap(0) = 32
        // Downloads width: pad(0) + icon(14) + icon_gap(4) + text(50) + pad(0) = 68
        assert_eq!(
            layout.buttons[1].rect,
            Rect {
                left: 32,
                top: 0,
                right: 32 + 68,
                bottom: 26
            }
        );
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
        // add_size: 20*1.5 = 30
        // gap: 0*1.5 = 0
        // pad: 0*1.5 = 0
        // icon: 14*1.5 = 21
        // icon_gap: 4*1.5 = 6
        // btn_height: 26*1.5 = 39
        // Downloads width = 0+21+6+75+0 = 102
        assert_eq!(
            layout.buttons[0].rect,
            Rect {
                left: 18,
                top: 0,
                right: 48,
                bottom: 39
            }
        );
        assert_eq!(
            layout.buttons[1].rect,
            Rect {
                left: 48,
                top: 0,
                right: 48 + 102,
                bottom: 39
            }
        );
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
            assert!(
                a.rect.right <= b.rect.left,
                "buttons must not overlap: {:?} then {:?}",
                a.rect,
                b.rect
            );
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
        // Widest folder width: pad + icon + gap + 200 + pad = 0 + 14 + 4 + 200 + 0 = 218
        assert_eq!(layout.total_width, 218);
        assert_eq!(layout.buttons[0].rect.width(), 218); // + button also uses max width
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
        let only_add = [mk_button(
            true,
            Rect {
                left: 0,
                top: 0,
                right: 30,
                bottom: 28,
            },
        )];
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
            mk_button(
                true,
                Rect {
                    left: 0,
                    top: 0,
                    right: 30,
                    bottom: 28,
                },
            ),
            mk_button(
                false,
                Rect {
                    left: 40,
                    top: 0,
                    right: 100,
                    bottom: 28,
                },
            ),
            mk_button(
                false,
                Rect {
                    left: 110,
                    top: 0,
                    right: 170,
                    bottom: 28,
                },
            ),
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
            mk_button(
                true,
                Rect {
                    left: 0,
                    top: 0,
                    right: 30,
                    bottom: 28,
                },
            ),
            mk_button(
                false,
                Rect {
                    left: 40,
                    top: 0,
                    right: 100,
                    bottom: 28,
                },
            ),
            mk_button(
                false,
                Rect {
                    left: 110,
                    top: 0,
                    right: 170,
                    bottom: 28,
                },
            ),
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
            mk_button(
                true,
                Rect {
                    left: 0,
                    top: 0,
                    right: 30,
                    bottom: 28,
                },
            ),
            mk_button(
                false,
                Rect {
                    left: 40,
                    top: 0,
                    right: 100,
                    bottom: 28,
                },
            ),
            mk_button(
                false,
                Rect {
                    left: 110,
                    top: 0,
                    right: 170,
                    bottom: 28,
                },
            ),
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
            mk_button(
                true,
                Rect {
                    left: 0,
                    top: 0,
                    right: 50,
                    bottom: 30,
                },
            ),
            mk_button(
                false,
                Rect {
                    left: 0,
                    top: 40,
                    right: 50,
                    bottom: 68,
                },
            ),
            mk_button(
                false,
                Rect {
                    left: 0,
                    top: 70,
                    right: 50,
                    bottom: 98,
                },
            ),
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
            mk_button(
                true,
                Rect {
                    left: 0,
                    top: 0,
                    right: 50,
                    bottom: 30,
                },
            ),
            mk_button(
                false,
                Rect {
                    left: 0,
                    top: 40,
                    right: 50,
                    bottom: 68,
                },
            ),
            mk_button(
                false,
                Rect {
                    left: 0,
                    top: 70,
                    right: 50,
                    bottom: 98,
                },
            ),
        ];
        let input = InsertionInput {
            buttons: &buttons,
            orientation: Orientation::Vertical,
            cursor_x: 25,
            cursor_y: 500,
        };
        assert_eq!(compute_insertion_index(&input), 2);
    }

    use proptest::prelude::*;

    /// Generator for a LayoutInput scenario: (dpi, orientation, folder names, widths, grip).
    fn arb_layout_scenario() -> impl Strategy<Value = (u32, Orientation, Vec<String>, Vec<i32>, i32)>
    {
        (
            prop_oneof![Just(96u32), Just(120), Just(144), Just(168), Just(192)],
            prop_oneof![Just(Orientation::Horizontal), Just(Orientation::Vertical)],
            prop::collection::vec("[A-Za-z0-9 ]{1,50}", 0..20),
            Just(Vec::<i32>::new()),
            8i32..=24,
        )
            .prop_flat_map(|(dpi, orient, names, _placeholder, grip)| {
                let n = names.len();
                (
                    Just(dpi),
                    Just(orient),
                    Just(names),
                    prop::collection::vec(10i32..=500, n..=n),
                    Just(grip),
                )
            })
    }

    proptest! {
        #[test]
        fn layout_rects_are_non_negative(
            (dpi, orient, names, widths, grip) in arb_layout_scenario()
        ) {
            let folders: Vec<FolderEntry> = names.iter().map(|n| mk_folder(n)).collect();
            let input = LayoutInput {
                dpi,
                orientation: orient,
                folders: &folders,
                folder_text_widths_physical_px: &widths,
                grip_size_logical_px: grip,
            };
            let layout = compute_layout(&input);
            for b in &layout.buttons {
                prop_assert!(b.rect.width() >= 0, "button width negative: {:?}", b.rect);
                prop_assert!(b.rect.height() >= 0, "button height negative: {:?}", b.rect);
            }
            prop_assert!(layout.total_width >= 0);
            prop_assert!(layout.total_height >= 0);
        }

        #[test]
        fn layout_first_button_is_add(
            (dpi, orient, names, widths, grip) in arb_layout_scenario()
        ) {
            let folders: Vec<FolderEntry> = names.iter().map(|n| mk_folder(n)).collect();
            let input = LayoutInput {
                dpi,
                orientation: orient,
                folders: &folders,
                folder_text_widths_physical_px: &widths,
                grip_size_logical_px: grip,
            };
            let layout = compute_layout(&input);
            prop_assert!(!layout.buttons.is_empty());
            prop_assert!(layout.buttons[0].is_add);
            for b in &layout.buttons[1..] {
                prop_assert!(!b.is_add);
            }
        }

        #[test]
        fn layout_buttons_do_not_overlap(
            (dpi, orient, names, widths, grip) in arb_layout_scenario()
        ) {
            let folders: Vec<FolderEntry> = names.iter().map(|n| mk_folder(n)).collect();
            let input = LayoutInput {
                dpi,
                orientation: orient,
                folders: &folders,
                folder_text_widths_physical_px: &widths,
                grip_size_logical_px: grip,
            };
            let layout = compute_layout(&input);
            for pair in layout.buttons.windows(2) {
                let (a, b) = (&pair[0], &pair[1]);
                match orient {
                    Orientation::Horizontal => {
                        prop_assert!(a.rect.right <= b.rect.left, "overlap: {:?} then {:?}", a.rect, b.rect);
                    }
                    Orientation::Vertical => {
                        prop_assert!(a.rect.bottom <= b.rect.top, "overlap: {:?} then {:?}", a.rect, b.rect);
                    }
                }
            }
        }

        #[test]
        fn layout_total_bounds_contain_all_buttons(
            (dpi, orient, names, widths, grip) in arb_layout_scenario()
        ) {
            let folders: Vec<FolderEntry> = names.iter().map(|n| mk_folder(n)).collect();
            let input = LayoutInput {
                dpi,
                orientation: orient,
                folders: &folders,
                folder_text_widths_physical_px: &widths,
                grip_size_logical_px: grip,
            };
            let layout = compute_layout(&input);
            for b in &layout.buttons {
                prop_assert!(b.rect.left >= 0);
                prop_assert!(b.rect.top >= 0);
                prop_assert!(b.rect.right <= layout.total_width);
                prop_assert!(b.rect.bottom <= layout.total_height);
            }
        }

        #[test]
        fn insertion_index_is_in_range(
            (dpi, orient, names, widths, grip) in arb_layout_scenario(),
            cursor_x in -1000i32..=5000,
            cursor_y in -1000i32..=5000,
        ) {
            let folders: Vec<FolderEntry> = names.iter().map(|n| mk_folder(n)).collect();
            let layout_input = LayoutInput {
                dpi,
                orientation: orient,
                folders: &folders,
                folder_text_widths_physical_px: &widths,
                grip_size_logical_px: grip,
            };
            let layout = compute_layout(&layout_input);
            let insertion = InsertionInput {
                buttons: &layout.buttons,
                orientation: orient,
                cursor_x,
                cursor_y,
            };
            let idx = compute_insertion_index(&insertion);
            prop_assert!(idx <= folders.len(), "index {} out of range 0..={}", idx, folders.len());
        }
    }

    fn lay(n: usize, h: HeaderPin, bt: i32, bb: i32, max_h: i32) -> SubmenuLayout {
        // item 30, arrow 18, width 200
        compute_submenu_layout(n, h, 30, 18, 200, bt, bb, max_h)
    }

    #[test]
    fn submenu_layout_stacks_vertically_with_buffer() {
        let layout = lay(3, HeaderPin::None, 10, 10, 10_000);
        assert_eq!(layout.item_rects.len(), 3);
        assert_eq!(layout.visible_count, 3);
        assert_eq!(layout.total_count, 3);
        assert_eq!(layout.item_rects[0].left, 10);
        assert_eq!(layout.item_rects[0].top, 10);
        assert_eq!(layout.item_rects[0].width(), 200);
        assert_eq!(layout.item_rects[0].height(), 30);
        assert_eq!(layout.item_rects[1].top, 40);
        assert_eq!(layout.item_rects[1].left, 10);
        assert_eq!(layout.item_rects[1].width(), 200);
        assert_eq!(layout.item_rects[1].bottom, 70);
        assert_eq!(layout.popup_w, 220);
        assert_eq!(layout.popup_h, 3 * 30 + 20);
        assert!(layout.arrow_up_rect.is_none() && layout.arrow_down_rect.is_none());
        assert!(layout.header_rect.is_none());
    }

    #[test]
    fn submenu_layout_zero_items_is_just_buffer() {
        let layout = lay(0, HeaderPin::None, 10, 10, 10_000);
        assert_eq!(layout.popup_w, 220);
        assert_eq!(layout.visible_count, 0);
        assert_eq!(layout.total_count, 0);
        assert_eq!(layout.popup_h, 20);
        assert!(layout.item_rects.is_empty());
    }

    #[test]
    fn submenu_layout_zero_buffer_tight_fit() {
        let layout = lay(2, HeaderPin::None, 0, 0, 10_000);
        assert_eq!(layout.popup_w, 200);
        assert_eq!(layout.popup_h, 60);
        assert_eq!(layout.visible_count, 2);
        assert_eq!(layout.total_count, 2);
        assert_eq!(
            layout.item_rects[0],
            Rect {
                left: 0,
                top: 0,
                right: 200,
                bottom: 30
            }
        );
        assert_eq!(
            layout.item_rects[1],
            Rect {
                left: 0,
                top: 30,
                right: 200,
                bottom: 60
            }
        );
    }

    #[test]
    fn submenu_layout_overflow_none_header_has_arrows_and_visible_math() {
        // buffers 20, arrows 36 → avail = 150-20-36 = 94 → 3 visible.
        let layout = lay(10, HeaderPin::None, 10, 10, 150);
        assert_eq!(layout.total_count, 10);
        assert_eq!(layout.visible_count, 3);
        assert_eq!(layout.popup_h, 20 + 36 + 90);
        let up = layout.arrow_up_rect.unwrap();
        let down = layout.arrow_down_rect.unwrap();
        assert_eq!((up.top, up.bottom), (10, 28));
        assert_eq!(layout.item_rects[0].top, 28);
        assert_eq!(layout.item_rects[2].bottom, 118);
        assert_eq!((down.top, down.bottom), (118, 136));
        assert!(layout.header_rect.is_none());
    }

    #[test]
    fn submenu_layout_overflow_top_header_order() {
        // buffers 0/10, header 30, arrows 36 → avail = 200-10-30-36 = 124 → 4.
        let layout = lay(10, HeaderPin::Top, 0, 10, 200);
        assert_eq!(layout.visible_count, 4);
        assert_eq!(layout.popup_h, 10 + 30 + 36 + 120);
        let h = layout.header_rect.unwrap();
        let up = layout.arrow_up_rect.unwrap();
        let down = layout.arrow_down_rect.unwrap();
        assert_eq!((h.top, h.bottom), (0, 30));
        assert_eq!((up.top, up.bottom), (30, 48));
        assert_eq!(layout.item_rects[0].top, 48);
        assert_eq!((down.top, down.bottom), (168, 186));
        assert_eq!(layout.popup_h, 186 + 10);
    }

    #[test]
    fn submenu_layout_overflow_bottom_header_order() {
        let layout = lay(10, HeaderPin::Bottom, 10, 0, 200);
        assert_eq!(layout.visible_count, 4);
        let up = layout.arrow_up_rect.unwrap();
        let down = layout.arrow_down_rect.unwrap();
        let h = layout.header_rect.unwrap();
        assert_eq!((up.top, up.bottom), (10, 28));
        assert_eq!(layout.item_rects[0].top, 28);
        assert_eq!((down.top, down.bottom), (148, 166));
        assert_eq!((h.top, h.bottom), (166, 196));
        assert_eq!(layout.popup_h, 196);
    }

    #[test]
    fn submenu_layout_fits_header_top_and_bottom_no_arrows() {
        let t = lay(3, HeaderPin::Top, 0, 10, 10_000);
        assert_eq!(t.header_rect.unwrap().top, 0);
        assert_eq!(t.item_rects[0].top, 30);
        assert_eq!(t.popup_h, 30 + 90 + 10);
        assert!(t.arrow_up_rect.is_none());
        let b = lay(3, HeaderPin::Bottom, 10, 0, 10_000);
        assert_eq!(b.item_rects[0].top, 10);
        assert_eq!(b.header_rect.unwrap().top, 100);
        assert_eq!(b.popup_h, 130);
    }

    #[test]
    fn submenu_layout_honors_min_one_visible_floor() {
        let layout = lay(10, HeaderPin::None, 10, 10, 20);
        assert_eq!(layout.visible_count, 1);
        assert_eq!(layout.popup_h, 20 + 36 + 30);
    }

    #[test]
    fn submenu_layout_visible_count_never_exceeds_total() {
        let layout = lay(3, HeaderPin::None, 10, 10, 10_000);
        assert_eq!(layout.visible_count, 3);
    }

    #[test]
    fn submenu_layout_asymmetric_buffer() {
        let layout = lay(3, HeaderPin::None, 0, 20, 10_000);
        assert_eq!(layout.popup_h, 110);
        assert_eq!(layout.item_rects[0].top, 0);
        assert_eq!(layout.item_rects[0].bottom, 30);
        assert_eq!(layout.item_rects[2].bottom, 90);
        assert_eq!(layout.buffer_top_px, 0);
        assert_eq!(layout.buffer_bottom_px, 20);
    }

    #[test]
    fn hit_header_arrows_and_buffers() {
        let l = lay(10, HeaderPin::Top, 0, 10, 200);
        assert_eq!(l.hit(50, 10, 0), PopupHit::Item(0));
        assert_eq!(l.hit(50, 40, 0), PopupHit::ArrowUp);
        assert_eq!(l.hit(50, 170, 0), PopupHit::ArrowDown);
        assert_eq!(l.hit(50, 190, 0), PopupHit::Nothing);
        assert_eq!(l.hit(-1, 10, 0), PopupHit::Nothing);
    }

    #[test]
    fn hit_top_maps_display_index_with_scroll() {
        let l = lay(10, HeaderPin::Top, 0, 10, 200);
        // first visible row (y=48) with offset 3 → scroll item 3 → display 4.
        assert_eq!(l.hit(50, 50, 3), PopupHit::Item(4));
        // last visible row (4th, y=138..168).
        assert_eq!(l.hit(50, 150, 3), PopupHit::Item(7));
    }

    #[test]
    fn hit_bottom_maps_display_index_with_scroll() {
        let l = lay(10, HeaderPin::Bottom, 10, 0, 200);
        assert_eq!(l.hit(50, 30, 3), PopupHit::Item(3));
        assert_eq!(l.hit(50, 147, 3), PopupHit::Item(6));
        // header is display total_count (10).
        assert_eq!(l.hit(50, 180, 3), PopupHit::Item(10));
        assert_eq!(l.hit(50, 5, 3), PopupHit::Nothing);
    }

    #[test]
    fn rect_for_display_index_header_visible_and_scrolled_out() {
        let l = lay(10, HeaderPin::Top, 0, 10, 200);
        assert_eq!(l.rect_for_display_index(0, 5), l.header_rect);
        assert_eq!(l.rect_for_display_index(6, 5), Some(l.item_rects[0]));
        assert_eq!(l.rect_for_display_index(2, 5), None);
        assert_eq!(l.rect_for_display_index(10, 5), None);
        let b = lay(10, HeaderPin::Bottom, 10, 0, 200);
        assert_eq!(b.rect_for_display_index(10, 0), b.header_rect);
        assert_eq!(b.rect_for_display_index(1, 0), Some(b.item_rects[1]));
        assert_eq!(b.rect_for_display_index(9, 0), None);
    }

    #[test]
    fn scroll_bounds_helpers() {
        let l = lay(10, HeaderPin::None, 10, 10, 150);
        assert_eq!(l.max_scroll_offset(), 7);
        assert!(!l.can_scroll_up(0));
        assert!(l.can_scroll_up(1));
        assert!(l.can_scroll_down(6));
        assert!(!l.can_scroll_down(7));
        let fit = lay(3, HeaderPin::None, 0, 0, 10_000);
        assert_eq!(fit.max_scroll_offset(), 0);
        assert!(!fit.can_scroll_down(0));
    }
}
