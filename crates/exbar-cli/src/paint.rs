//! Owner-drawn toolbar painting (GDI). Pure-Win32 leaf — driven by
//! `toolbar.rs` on `WM_PAINT`. Reads `ToolbarState` (via `&`) for display;
//! `compute_layout` takes `&mut ToolbarState` because it writes `state.buttons`.

use windows::Win32::Foundation::{COLORREF, HWND, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DEFAULT_GUI_FONT, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_RIGHT,
    DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FillRect, GetStockObject,
    GetTextExtentPoint32W, HDC, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

use crate::config::{FolderEntry, Orientation};
use crate::layout::{self, ButtonLayout, LayoutInput};
use crate::submenu::DisplayItem;
use crate::theme;
use crate::toolbar::{BTN_PAD_H, GRIP_SIZE, ToolbarState};

/// Build the toolbar-button label for `folder`. When `show_icons` is `false`
/// the leading `📁`/`🕘` emoji (and its trailing space) are omitted.
/// Used by both [`measure_folder_text_widths`] and the paint loop so the two
/// stay in sync.
pub(crate) fn folder_button_label(folder: &FolderEntry, show_icons: bool) -> String {
    match folder.kind {
        crate::config::FolderKind::Recent => {
            if show_icons {
                "\u{1F558} Recent".to_string()
            } else {
                "Recent".to_string()
            }
        }
        crate::config::FolderKind::Folder => {
            if show_icons {
                format!("\u{1F4C1} {}", folder.name)
            } else {
                folder.name.clone()
            }
        }
    }
}

/// Measure the rendered-pixel width of each folder's label ("📁 Name" or "🕘 Recent" — the
/// same format used in paint) using the currently-selected font in `hdc`.
/// `show_icons` mirrors `Config::show_icons`; when `false` the emoji prefix is dropped.
///
/// Caller must `SelectObject(hdc, font)` first. Returns a Vec the same
/// length as `folders`.
pub(crate) fn measure_folder_text_widths(
    hdc: HDC,
    folders: &[FolderEntry],
    show_icons: bool,
) -> Vec<i32> {
    folders
        .iter()
        .map(|f| {
            let label = folder_button_label(f, show_icons);
            let wide: Vec<u16> = label.encode_utf16().collect();
            let mut size = SIZE::default();
            let ok = unsafe { GetTextExtentPoint32W(hdc, &wide, &mut size) };
            if ok.as_bool() {
                size.cx
            } else {
                // Fallback: approximate — same as prior code.
                (label.chars().count() as i32) * 8
            }
        })
        .collect()
}

/// Convert a `layout::Rect` to a Win32 `RECT` for use with GDI APIs.
pub(crate) fn rect_to_win32(r: layout::Rect) -> RECT {
    RECT {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

/// Adapter: measures text widths via the given `hdc`, calls
/// `layout::compute_layout`, writes the resulting buttons into `state.buttons`,
/// and returns `(total_width, total_height)`.
pub(crate) fn compute_layout(hdc: HDC, state: &mut ToolbarState) -> (i32, i32) {
    let folders: Vec<FolderEntry> = state
        .config
        .as_ref()
        .map(|c| c.folders.clone())
        .unwrap_or_default();
    let show_icons = state.config.as_ref().map(|c| c.show_icons).unwrap_or(true);
    let widths = measure_folder_text_widths(hdc, &folders, show_icons);

    let input = LayoutInput {
        dpi: state.dpi,
        orientation: state.layout,
        folders: &folders,
        folder_text_widths_physical_px: &widths,
        grip_size_logical_px: GRIP_SIZE,
    };

    let computed = layout::compute_layout(&input);
    state.buttons = computed.buttons;
    (computed.total_width, computed.total_height)
}

/// Returns true if (x, y) is in the grip area.
pub(crate) fn in_grip(state: &ToolbarState, x: i32, y: i32) -> bool {
    match state.layout {
        Orientation::Horizontal => x < state.grip_size,
        Orientation::Vertical => y < state.grip_size,
    }
}

/// Render the toolbar into its window's DC. Called from WM_PAINT.
///
/// # Safety
///
/// Must be called from the WM_PAINT handler on the toolbar window's
/// message-pump thread. `hwnd` must be a valid toolbar HWND. The
/// function calls `BeginPaint`/`EndPaint` internally; callers must
/// not call those themselves.
pub(crate) unsafe fn paint(hwnd: HWND, state: &ToolbarState) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    if hdc.is_invalid() {
        return;
    }

    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }

    let is_dark = theme::is_dark_mode();

    // Background
    let bg_color = if is_dark {
        COLORREF(0x002D2D2D)
    } else {
        COLORREF(0x00F0F0F0)
    };
    let bg_brush = unsafe { CreateSolidBrush(bg_color) };
    unsafe {
        FillRect(hdc, &client, bg_brush);
    }
    unsafe {
        let _ = DeleteObject(bg_brush.into());
    }

    // Grip area — draw dots
    let grip = state.grip_size;
    let grip_color = if is_dark {
        COLORREF(0x00888888)
    } else {
        COLORREF(0x00999999)
    };
    let grip_brush = unsafe { CreateSolidBrush(grip_color) };
    let dot_size = theme::scale(2, state.dpi);
    let dot_gap = theme::scale(4, state.dpi);

    match state.layout {
        Orientation::Horizontal => {
            // Three vertical dots centered in the grip column
            let cx = grip / 2;
            let total_h = dot_size * 3 + dot_gap * 2;
            let start_y = (client.bottom - client.top - total_h) / 2;
            for i in 0..3i32 {
                let dy = start_y + i * (dot_size + dot_gap);
                let dot = RECT {
                    left: cx - dot_size / 2,
                    top: dy,
                    right: cx + dot_size / 2 + 1,
                    bottom: dy + dot_size,
                };
                unsafe {
                    FillRect(hdc, &dot, grip_brush);
                }
            }
        }
        Orientation::Vertical => {
            // Three horizontal dots centered in the grip row
            let cy = grip / 2;
            let total_w = dot_size * 3 + dot_gap * 2;
            let start_x = (client.right - client.left - total_w) / 2;
            for i in 0..3i32 {
                let dx = start_x + i * (dot_size + dot_gap);
                let dot = RECT {
                    left: dx,
                    top: cy - dot_size / 2,
                    right: dx + dot_size,
                    bottom: cy + dot_size / 2 + 1,
                };
                unsafe {
                    FillRect(hdc, &dot, grip_brush);
                }
            }
        }
    }
    unsafe {
        let _ = DeleteObject(grip_brush.into());
    }

    // Border
    let border_color = if is_dark {
        COLORREF(0x00555555)
    } else {
        COLORREF(0x00CCCCCC)
    };
    let border_brush = unsafe { CreateSolidBrush(border_color) };
    let top_border = RECT {
        left: client.left,
        top: client.top,
        right: client.right,
        bottom: client.top + 1,
    };
    unsafe {
        FillRect(hdc, &top_border, border_brush);
    }
    let bottom_border = RECT {
        left: client.left,
        top: client.bottom - 1,
        right: client.right,
        bottom: client.bottom,
    };
    unsafe {
        FillRect(hdc, &bottom_border, border_brush);
    }
    let left_border = RECT {
        left: client.left,
        top: client.top,
        right: client.left + 1,
        bottom: client.bottom,
    };
    unsafe {
        FillRect(hdc, &left_border, border_brush);
    }
    let right_border = RECT {
        left: client.right - 1,
        top: client.top,
        right: client.right,
        bottom: client.bottom,
    };
    unsafe {
        FillRect(hdc, &right_border, border_brush);
    }
    unsafe {
        let _ = DeleteObject(border_brush.into());
    }

    unsafe {
        SetBkMode(hdc, TRANSPARENT);
    }

    let text_cr = if is_dark {
        COLORREF(0x00FFFFFF)
    } else {
        COLORREF(0x00202020)
    };
    unsafe {
        SetTextColor(hdc, text_cr);
    }

    let default_font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
    let old_font = unsafe { SelectObject(hdc, default_font) };

    let hover_button = state.pointer.hover_button();
    let pressed_button = state.pointer.pressed_button();
    let drag_source = state.pointer.dragging_reorder().map(|(src, _ins)| src);
    let show_icons = state.config.as_ref().map(|c| c.show_icons).unwrap_or(true);

    for (i, btn) in state.buttons.iter().enumerate() {
        let is_hover = hover_button == Some(i);
        let is_pressed = pressed_button == Some(i);
        let is_dragging_source = drag_source == Some(i);

        // Compute reachability disabled flag — only meaningful for non-add
        // folder buttons whose path classifies as a network root.
        let is_disabled = if btn.is_add {
            false
        } else {
            crate::reachability::classify_root(&btn.folder.path)
                .and_then(|root| state.reachability.read().ok().map(|c| c.get(&root)))
                .map(|r| r == crate::reachability::Reachability::Unreachable)
                .unwrap_or(false)
        };

        if is_disabled {
            // No highlight for disabled (greyed) buttons.
        } else if is_dragging_source {
            // Don't draw hover/pressed highlight for the dragged button.
        } else if is_pressed {
            let hl = if is_dark {
                COLORREF(0x00505050)
            } else {
                COLORREF(0x00D0D0D0)
            };
            let hbr = unsafe { CreateSolidBrush(hl) };
            unsafe {
                FillRect(hdc, &rect_to_win32(btn.rect), hbr);
            }
            unsafe {
                let _ = DeleteObject(hbr.into());
            }
        } else if is_hover {
            let hl = if is_dark {
                COLORREF(0x003D3D3D)
            } else {
                COLORREF(0x00E0E0E0)
            };
            let hbr = unsafe { CreateSolidBrush(hl) };
            unsafe {
                FillRect(hdc, &rect_to_win32(btn.rect), hbr);
            }
            unsafe {
                let _ = DeleteObject(hbr.into());
            }
        }

        let label = if btn.is_add {
            "+".to_string()
        } else {
            folder_button_label(&btn.folder, show_icons)
        };

        // Disabled (Unreachable) and drag-source buttons render at mid-grey
        // on both themes (same shade — the visual cue is "inactive").
        let text_cr_this = if is_disabled || is_dragging_source {
            if is_dark {
                COLORREF(0x00808080)
            } else {
                COLORREF(0x00A0A0A0)
            }
        } else {
            text_cr
        };
        unsafe {
            SetTextColor(hdc, text_cr_this);
        }

        let mut label_wide: Vec<u16> = label.encode_utf16().collect();
        let mut draw_rect = rect_to_win32(btn.rect);
        let flags = if btn.is_add {
            DT_SINGLELINE | DT_VCENTER | DT_CENTER
        } else {
            DT_SINGLELINE | DT_VCENTER
        };
        if !btn.is_add {
            draw_rect.left += theme::scale(BTN_PAD_H, state.dpi);
        }
        unsafe {
            DrawTextW(hdc, &mut label_wide, &mut draw_rect, flags);
        }
    }

    if !old_font.is_invalid() {
        unsafe {
            SelectObject(hdc, old_font);
        }
    }

    // Reorder insertion caret (horizontal layout only).
    if let Some((_src, insertion)) = state.pointer.dragging_reorder()
        && state.layout == Orientation::Horizontal
    {
        let folder_buttons: Vec<&ButtonLayout> =
            state.buttons.iter().filter(|b| !b.is_add).collect();
        if !folder_buttons.is_empty() {
            // X coordinate of the caret.
            let caret_x = if insertion >= folder_buttons.len() {
                folder_buttons.last().unwrap().rect.right + 1
            } else {
                folder_buttons[insertion].rect.left - 1
            };
            let caret_w = theme::scale(2, state.dpi);
            let caret_color = if is_dark {
                COLORREF(0x00A0A0FF)
            } else {
                COLORREF(0x004040C0)
            };
            let caret_brush = unsafe { CreateSolidBrush(caret_color) };
            let caret_rect = RECT {
                left: caret_x,
                top: client.top + 2,
                right: caret_x + caret_w,
                bottom: client.bottom - 2,
            };
            unsafe {
                FillRect(hdc, &caret_rect, caret_brush);
            }
            unsafe {
                let _ = DeleteObject(caret_brush.into());
            }
        }
    }

    unsafe {
        let _ = EndPaint(hwnd, &ps);
    }
}

/// Build the display label string for a `DisplayItem`, consistent with paint rendering.
///
/// `at_max_depth` — when `true`, subfolders that have children append a
/// `(max subfolders)` suffix so the user can see the depth cap is reached.
/// Dotdot (upward `..`) is not affected because going up is never blocked.
///
/// Called by both [`paint_submenu_popup`] and [`measure_display_items_width`] so the
/// two code paths stay in sync.
fn display_item_label(item: &DisplayItem, at_max_depth: bool) -> String {
    match item {
        DisplayItem::Subfolder { entry } => {
            if at_max_depth && entry.has_children {
                format!("\u{1F4C1} {} (max subfolders)", entry.name)
            } else {
                format!("\u{1F4C1} {}", entry.name)
            }
        }
        DisplayItem::Dotdot { parent_name, .. } => format!("\u{2B06} {}", parent_name),
        DisplayItem::Header { name, .. } => format!("\u{1F4C2} {}", name),
        DisplayItem::Ellipsis => "\u{2026}(more)".to_string(),
        DisplayItem::Empty { message } => message.clone(),
    }
}

/// Measure the maximum rendered pixel width of the given `display_items` list
/// using `DrawTextW` with `DT_CALCRECT`. Used to compute a dynamic popup width.
///
/// `level` — the nesting depth of the popup being measured. Items at
/// [`crate::submenu::MAX_CHAIN_DEPTH`] get the `(max subfolders)` suffix
/// appended to their label, which widens the measurement accordingly.
///
/// Returns the measured max text width in physical pixels (without padding).
/// Caller must add left/right padding before using this as `max_width_px`.
pub fn measure_display_items_width(display_items: &[DisplayItem], dpi: u32, level: u8) -> i32 {
    use windows::Win32::Foundation::RECT as WinRect;
    use windows::Win32::Graphics::Gdi::{
        DT_CALCRECT, DT_SINGLELINE, GetDC, GetStockObject, ReleaseDC, SelectObject,
    };

    let at_max_depth = (level as usize) >= crate::submenu::MAX_CHAIN_DEPTH;

    let hdc = unsafe { GetDC(None) };
    if hdc.is_invalid() {
        return theme::scale(200, dpi);
    }
    let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
    let old = unsafe { SelectObject(hdc, font) };

    let mut max_w: i32 = 0;
    for item in display_items {
        let label = display_item_label(item, at_max_depth);
        let mut wide: Vec<u16> = label.encode_utf16().collect();
        let mut r = WinRect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        unsafe {
            DrawTextW(hdc, &mut wide, &mut r, DT_CALCRECT | DT_SINGLELINE);
        }
        if r.right > max_w {
            max_w = r.right;
        }
    }

    if !old.is_invalid() {
        unsafe {
            SelectObject(hdc, old);
        }
    }
    unsafe {
        let _ = ReleaseDC(None, hdc);
    }
    max_w
}

/// Colours shared by every row of a popup, computed once per paint.
struct PopupPalette {
    text: COLORREF,
    text_dim: COLORREF,
    accent_bg: COLORREF,
}

/// Paint one visible popup row: highlight, label, ▸ arrow, and the header
/// separator. `is_first`/`is_last` are display-list positions, used to put
/// the header's separator on its content-facing edge.
#[allow(clippy::too_many_arguments)]
fn paint_popup_row(
    hdc: HDC,
    item: &DisplayItem,
    win_rect: RECT,
    highlighted: bool,
    is_first: bool,
    is_last: bool,
    at_max_depth: bool,
    palette: &PopupPalette,
    dpi: u32,
) {
    // Accent background for the highlighted row.
    if highlighted {
        let accent_brush = unsafe { CreateSolidBrush(palette.accent_bg) };
        unsafe {
            FillRect(hdc, &win_rect, accent_brush);
            let _ = DeleteObject(accent_brush.into());
        }
    }

    // Build label text and determine display properties.
    let label = display_item_label(item, at_max_depth);
    // `has_children_visual` controls the ▸ arrow glyph. At max depth,
    // subfolders with children cannot open a deeper level, so the arrow
    // would be misleading — suppress it. Dotdot (upward ..) is unaffected
    // because going up is never blocked by the depth cap.
    let (is_disabled, has_children_visual) = match item {
        DisplayItem::Subfolder { entry } => (false, entry.has_children && !at_max_depth),
        DisplayItem::Dotdot { .. } => (false, true), // Dotdot always opens parent dir submenu
        DisplayItem::Header { .. } => (false, false),
        DisplayItem::Ellipsis | DisplayItem::Empty { .. } => (true, false),
    };

    let color = if is_disabled {
        palette.text_dim
    } else {
        palette.text
    };
    unsafe { SetTextColor(hdc, color) };

    // Draw label with left padding and end-ellipsis on overflow.
    let pad = theme::scale(8, dpi);
    let arrow_w = theme::scale(20, dpi);
    let text_right = if has_children_visual {
        win_rect.right - arrow_w
    } else {
        win_rect.right - theme::scale(8, dpi)
    };
    let mut text_rect = RECT {
        left: win_rect.left + pad,
        top: win_rect.top,
        right: text_right,
        bottom: win_rect.bottom,
    };
    let mut wide: Vec<u16> = label.encode_utf16().collect();
    unsafe {
        DrawTextW(
            hdc,
            &mut wide,
            &mut text_rect,
            DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS,
        );
    }

    // Trailing ▸ arrow for items that have children (suppressed at max depth).
    if has_children_visual {
        let mut arrow_rect = RECT {
            left: win_rect.right - arrow_w,
            top: win_rect.top,
            right: win_rect.right - theme::scale(4, dpi),
            bottom: win_rect.bottom,
        };
        let mut arrow: Vec<u16> = "\u{25B8}".encode_utf16().collect();
        unsafe {
            DrawTextW(
                hdc,
                &mut arrow,
                &mut arrow_rect,
                DT_SINGLELINE | DT_VCENTER | DT_RIGHT,
            );
        }
    }

    if matches!(item, DisplayItem::Header { .. }) {
        let h = theme::scale(1, dpi).max(1);
        // First row => content is below, separator on the bottom edge;
        // last row => content is above, separator on the top edge.
        let y = if is_last && !is_first {
            win_rect.top
        } else {
            win_rect.bottom - h
        };
        let sep = RECT {
            left: win_rect.left + theme::scale(8, dpi),
            top: y,
            right: win_rect.right - theme::scale(8, dpi),
            bottom: y + h,
        };
        unsafe {
            let brush = CreateSolidBrush(palette.text_dim);
            FillRect(hdc, &sep, brush);
            let _ = DeleteObject(brush.into());
        }
    }
}

/// Render a submenu popup. Called from the popup wndproc's `WM_PAINT` handler.
///
/// Not unit-tested (pure GDI) — covered by manual smoke in Task 17.
///
/// Design: fills the popup with bg_color under the layered-window alpha set at
/// popup creation. Iterates `display_items` in parallel with
/// `layout.item_rects`, drawing each row; the `highlighted_index` row gets an
/// opaque accent bar. Rows with `has_children` paint a right-aligned `▸` glyph.
/// Header and Dotdot items get distinctive markers. `Ellipsis` / `Empty`
/// rows render disabled. Font obtained via `GetStockObject(DEFAULT_GUI_FONT)`,
/// consistent with the toolbar button paint.
pub fn paint_submenu_popup(
    hdc: HDC,
    layout: &crate::layout::SubmenuLayout,
    display_items: &[DisplayItem],
    highlighted_index: Option<usize>,
    dpi: u32,
    level: u8,
    scroll_offset: usize,
) {
    let at_max_depth = (level as usize) >= crate::submenu::MAX_CHAIN_DEPTH;
    let dark = theme::is_dark_mode();

    let bg_color = if dark {
        COLORREF(0x00_26_26_26)
    } else {
        COLORREF(0x00_F0_F0_F0)
    };
    let text_color = if dark {
        COLORREF(0x00_E0_E0_E0)
    } else {
        COLORREF(0x00_20_20_20)
    };
    let text_color_dim = if dark {
        COLORREF(0x00_80_80_80)
    } else {
        COLORREF(0x00_A0_A0_A0)
    };
    let accent_bg = if dark {
        COLORREF(0x00_50_50_50)
    } else {
        COLORREF(0x00_D8_E4_F0)
    };

    let palette = PopupPalette {
        text: text_color,
        text_dim: text_color_dim,
        accent_bg,
    };

    // Background fill (full popup area).
    let full_rect = RECT {
        left: 0,
        top: 0,
        right: layout.popup_w,
        bottom: layout.popup_h,
    };
    let bg_brush = unsafe { CreateSolidBrush(bg_color) };
    unsafe {
        FillRect(hdc, &full_rect, bg_brush);
        let _ = DeleteObject(bg_brush.into());
    }

    // Use the same stock font as the toolbar button paint.
    let hfont = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
    let old_font = unsafe { SelectObject(hdc, hfont) };

    unsafe { SetBkMode(hdc, TRANSPARENT) };

    // Only paint the visible window of items (scroll_offset..scroll_offset+visible_count).
    let start = scroll_offset;
    let end = (start + layout.visible_count).min(display_items.len());
    for (vis_i, item) in display_items[start..end].iter().enumerate() {
        let Some(rect) = layout.item_rects.get(vis_i) else {
            continue;
        };
        let win_rect = RECT {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        };
        // highlighted_index is in display-items space; vis_i is visible-window space.
        let logical_i = start + vis_i;
        paint_popup_row(
            hdc,
            item,
            win_rect,
            highlighted_index == Some(logical_i),
            logical_i == 0,
            logical_i + 1 == display_items.len(),
            at_max_depth,
            &palette,
            dpi,
        );
    }

    // Scroll indicators: ▲ in top buffer band when more items above; ▼ in bottom band below.
    let can_scroll_up = scroll_offset > 0;
    let can_scroll_down = scroll_offset + layout.visible_count < display_items.len();

    unsafe { SetTextColor(hdc, text_color_dim) };

    if can_scroll_up {
        let mut up_glyph: Vec<u16> = "\u{25B2}".encode_utf16().collect(); // ▲
        // Glyph paints in the inner trigger band only (adjacent to items),
        // not the full buffer — keeps the outer forgiveness zone visually clean.
        let mut tr = RECT {
            left: 0,
            top: layout.buffer_top_px - layout.scroll_trigger_top_px,
            right: layout.popup_w,
            bottom: layout.buffer_top_px,
        };
        unsafe {
            DrawTextW(
                hdc,
                &mut up_glyph,
                &mut tr,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
        }
    }
    if can_scroll_down {
        let mut down_glyph: Vec<u16> = "\u{25BC}".encode_utf16().collect(); // ▼
        let mut tr = RECT {
            left: 0,
            top: layout.popup_h - layout.buffer_bottom_px,
            right: layout.popup_w,
            bottom: layout.popup_h - layout.buffer_bottom_px + layout.scroll_trigger_bottom_px,
        };
        unsafe {
            DrawTextW(
                hdc,
                &mut down_glyph,
                &mut tr,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
        }
    }

    if !old_font.is_invalid() {
        unsafe {
            SelectObject(hdc, old_font);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> FolderEntry {
        FolderEntry {
            name: name.to_string(),
            path: format!("C:\\{name}"),
            icon: None,
            kind: crate::config::FolderKind::Folder,
        }
    }

    fn recent() -> FolderEntry {
        FolderEntry {
            name: "Recent".to_string(),
            path: String::new(),
            icon: None,
            kind: crate::config::FolderKind::Recent,
        }
    }

    #[test]
    fn folder_button_label_folder_with_icon() {
        assert_eq!(folder_button_label(&folder("Docs"), true), "\u{1F4C1} Docs");
    }

    #[test]
    fn folder_button_label_folder_without_icon() {
        // No prefix and no leading space.
        assert_eq!(folder_button_label(&folder("Docs"), false), "Docs");
    }

    #[test]
    fn folder_button_label_recent_with_icon() {
        assert_eq!(folder_button_label(&recent(), true), "\u{1F558} Recent");
    }

    #[test]
    fn display_item_label_header_uses_open_folder_glyph() {
        let item = DisplayItem::Header {
            path: std::path::PathBuf::from("C:/AppData"),
            name: "AppData".to_string(),
        };
        assert_eq!(display_item_label(&item, false), "📂 AppData");
    }

    #[test]
    fn folder_button_label_recent_without_icon() {
        assert_eq!(folder_button_label(&recent(), false), "Recent");
    }
}
