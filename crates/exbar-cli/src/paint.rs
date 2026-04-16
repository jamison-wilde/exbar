//! Owner-drawn toolbar painting (GDI). Pure-Win32 leaf — driven by
//! `toolbar.rs` on `WM_PAINT`. Reads `ToolbarState` (via `&`) for display;
//! `compute_layout` takes `&mut ToolbarState` because it writes `state.buttons`.

use windows::Win32::Foundation::{COLORREF, HWND, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DEFAULT_GUI_FONT, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    DeleteObject, DrawTextW, EndPaint, FillRect, GetStockObject, GetTextExtentPoint32W, HDC,
    PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

use crate::config::{FolderEntry, Orientation};
use crate::layout::{self, ButtonLayout, LayoutInput};
use crate::theme;
use crate::toolbar::{BTN_PAD_H, GRIP_SIZE, ToolbarState};

/// Measure the rendered-pixel width of each folder's label ("📁 Name" — the
/// same format used in paint) using the currently-selected font in `hdc`.
///
/// Caller must `SelectObject(hdc, font)` first. Returns a Vec the same
/// length as `folders`.
pub(crate) fn measure_folder_text_widths(hdc: HDC, folders: &[FolderEntry]) -> Vec<i32> {
    folders
        .iter()
        .map(|f| {
            // Match the label format used in paint: "📁 Name".
            let label = format!("\u{1F4C1} {}", f.name);
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
    let widths = measure_folder_text_widths(hdc, &folders);

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

    for (i, btn) in state.buttons.iter().enumerate() {
        let is_hover = hover_button == Some(i);
        let is_pressed = pressed_button == Some(i);
        let is_dragging_source = drag_source == Some(i);

        if is_dragging_source {
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
            format!("\u{1F4C1} {}", btn.folder.name)
        };

        // Dim text for the button being dragged.
        let text_cr_this = if is_dragging_source {
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
