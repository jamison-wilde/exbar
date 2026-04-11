//! Owner-drawn toolbar window embedded in the Explorer command bar.

use std::panic::AssertUnwindSafe;
use std::sync::Once;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_3DFACE, CreatePen, CreateSolidBrush, DEFAULT_GUI_FONT, DeleteObject,
    DrawTextW, EndPaint, FillRect, GetStockObject, GetSysColorBrush, HBRUSH, HGDIOBJ,
    InvalidateRect, LineTo, MoveToEx, PAINTSTRUCT, PS_SOLID, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT, DT_SINGLELINE, DT_VCENTER,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetParent, PostMessageW, RegisterClassExW,
    SendMessageW, SetWindowLongPtrW, GetWindowLongPtrW, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
    GWLP_USERDATA, HMENU, WNDCLASSEXW, WM_CREATE, WM_DESTROY, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_PAINT, WS_CHILD, WS_VISIBLE, WM_GETFONT, WINDOW_EX_STYLE,
};
use windows::Win32::UI::Shell::IShellBrowser;
use windows_core::PCWSTR;

use crate::config::{Config, FolderEntry};
use crate::theme::{
    get_dpi, scale, text_color, hotlight_color,
    BUTTON_GAP, BUTTON_PADDING_H, BUTTON_ICON_SIZE, ICON_TEXT_GAP,
    REFRESH_BUTTON_SIZE, SEPARATOR_MARGIN, SEPARATOR_WIDTH,
};

// ── Window class name ─────────────────────────────────────────────────────────

static CLASS_REGISTERED: Once = Once::new();
const CLASS_NAME: &str = "TabplorerToolbar";

/// WM_USER + 1 — internal message to trigger a toolbar refresh.
const WM_USER_REFRESH: u32 = 0x0401; // WM_USER = 0x0400

// ── Data structures ───────────────────────────────────────────────────────────

struct ButtonLayout {
    rect: RECT,
    folder: FolderEntry,
    is_refresh: bool,
}

struct ToolbarState {
    buttons: Vec<ButtonLayout>,
    hover_index: Option<usize>,
    pressed_index: Option<usize>,
    dpi: u32,
    config: Option<Config>,
    shell_browser: Option<IShellBrowser>,
    tracking_mouse: bool,
}

impl ToolbarState {
    fn new(dpi: u32, config: Option<Config>, shell_browser: Option<IShellBrowser>) -> Self {
        ToolbarState {
            buttons: Vec::new(),
            hover_index: None,
            pressed_index: None,
            dpi,
            config,
            shell_browser,
            tracking_mouse: false,
        }
    }
}

// ── Layout ────────────────────────────────────────────────────────────────────

fn compute_layout(state: &mut ToolbarState, client_rect: &RECT) {
    state.buttons.clear();

    let dpi = state.dpi;
    let h = client_rect.bottom - client_rect.top;
    let refresh_size = scale(REFRESH_BUTTON_SIZE, dpi);
    let sep_margin = scale(SEPARATOR_MARGIN, dpi);
    let sep_width = scale(SEPARATOR_WIDTH, dpi);
    let pad_h = scale(BUTTON_PADDING_H, dpi);
    let gap = scale(BUTTON_GAP, dpi);
    let icon_w = scale(BUTTON_ICON_SIZE, dpi);
    let icon_text_gap = scale(ICON_TEXT_GAP, dpi);

    // Refresh button
    state.buttons.push(ButtonLayout {
        rect: RECT { left: 0, top: 0, right: refresh_size, bottom: h },
        folder: FolderEntry {
            name: "\u{27F3}".to_string(),
            path: String::new(),
            icon: None,
        },
        is_refresh: true,
    });

    // Starting x after separator
    let mut x = refresh_size + sep_margin + sep_width + sep_margin;

    if let Some(ref config) = state.config.clone() {
        for entry in &config.folders {
            // Approximate text width: 8 logical px per char, scaled
            let text_w = (entry.name.chars().count() as i32) * scale(8, dpi);
            let btn_w = pad_h + icon_w + icon_text_gap + text_w + pad_h;
            state.buttons.push(ButtonLayout {
                rect: RECT { left: x, top: 0, right: x + btn_w, bottom: h },
                folder: entry.clone(),
                is_refresh: false,
            });
            x += btn_w + gap;
        }
    }
}

// ── Hit test ──────────────────────────────────────────────────────────────────

fn hit_test(state: &ToolbarState, x: i32, y: i32) -> Option<usize> {
    for (i, btn) in state.buttons.iter().enumerate() {
        let r = &btn.rect;
        if x >= r.left && x < r.right && y >= r.top && y < r.bottom {
            return Some(i);
        }
    }
    None
}

// ── Painting ──────────────────────────────────────────────────────────────────

unsafe fn paint(hwnd: HWND, state: &ToolbarState) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    if hdc.is_invalid() {
        return;
    }

    let mut client = RECT::default();
    unsafe { let _ = GetClientRect(hwnd, &mut client); }

    // Fill background with system face color
    let bg_brush = unsafe { GetSysColorBrush(COLOR_3DFACE) };
    unsafe { FillRect(hdc, &client, bg_brush); }

    unsafe { SetBkMode(hdc, TRANSPARENT); }

    // Text color
    let (r, g, b) = text_color();
    let text_cr = COLORREF(r as u32 | ((g as u32) << 8) | ((b as u32) << 16));
    unsafe { SetTextColor(hdc, text_cr); }

    // Try to inherit parent font; fall back to DEFAULT_GUI_FONT
    let old_font: HGDIOBJ = {
        let parent_result = unsafe { GetParent(hwnd) };
        let hfont_lresult = if let Ok(parent) = parent_result {
            unsafe { SendMessageW(parent, WM_GETFONT, Some(WPARAM(0)), Some(LPARAM(0))) }
        } else {
            LRESULT(0)
        };

        if hfont_lresult.0 != 0 {
            let hfont = windows::Win32::Graphics::Gdi::HFONT(hfont_lresult.0 as *mut _);
            unsafe { SelectObject(hdc, hfont.into()) }
        } else {
            let default_font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
            unsafe { SelectObject(hdc, default_font) }
        }
    };

    // Draw each button
    for (i, btn) in state.buttons.iter().enumerate() {
        let is_hover = state.hover_index == Some(i);
        let is_pressed = state.pressed_index == Some(i);

        if is_pressed {
            let (hr, hg, hb) = hotlight_color();
            let cr = COLORREF(
                (hr as u32).saturating_sub(30)
                    | (((hg as u32).saturating_sub(30)) << 8)
                    | (((hb as u32).saturating_sub(30)) << 16),
            );
            let hbr = unsafe { CreateSolidBrush(cr) };
            unsafe { FillRect(hdc, &btn.rect, hbr); }
            unsafe { let _ = DeleteObject(hbr.into()); }
        } else if is_hover {
            let (hr, hg, hb) = hotlight_color();
            let cr = COLORREF(
                ((hr as u32 + 255) / 2)
                    | ((((hg as u32 + 255) / 2)) << 8)
                    | ((((hb as u32 + 255) / 2)) << 16),
            );
            let hbr = unsafe { CreateSolidBrush(cr) };
            unsafe { FillRect(hdc, &btn.rect, hbr); }
            unsafe { let _ = DeleteObject(hbr.into()); }
        }

        // Draw label
        let label = if btn.is_refresh {
            "\u{27F3}".to_string()
        } else {
            format!("\u{1F4C1} {}", btn.folder.name)
        };

        let mut label_wide: Vec<u16> = label.encode_utf16().collect();
        let mut draw_rect = btn.rect;
        unsafe {
            DrawTextW(hdc, &mut label_wide, &mut draw_rect, DT_SINGLELINE | DT_VCENTER);
        }
    }

    // Draw separator after refresh button
    if !state.buttons.is_empty() {
        let dpi = state.dpi;
        let sep_margin = scale(SEPARATOR_MARGIN, dpi);
        let refresh_size = scale(REFRESH_BUTTON_SIZE, dpi);
        let sep_x = refresh_size + sep_margin;

        let (r, g, b) = text_color();
        let sep_cr = COLORREF(r as u32 | ((g as u32) << 8) | ((b as u32) << 16));
        let hpen = unsafe { CreatePen(PS_SOLID, 1, sep_cr) };
        let old_pen = unsafe { SelectObject(hdc, hpen.into()) };
        unsafe {
            let _ = MoveToEx(hdc, sep_x, client.top + scale(4, dpi), None);
            let _ = LineTo(hdc, sep_x, client.bottom - scale(4, dpi));
            SelectObject(hdc, old_pen);
            let _ = DeleteObject(hpen.into());
        }
    }

    // Restore font
    if !old_font.is_invalid() {
        unsafe { SelectObject(hdc, old_font); }
    }

    unsafe { let _ = EndPaint(hwnd, &ps); }
}

// ── Window procedure ──────────────────────────────────────────────────────────

unsafe fn toolbar_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
            let state_ptr = cs.lpCreateParams as *mut ToolbarState;
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize) };

            let state = unsafe { &mut *state_ptr };
            let mut client = RECT::default();
            unsafe { let _ = GetClientRect(hwnd, &mut client); }
            compute_layout(state, &client);

            LRESULT(0)
        }

        WM_DESTROY => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ToolbarState;
            if !ptr.is_null() {
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
                drop(unsafe { Box::from_raw(ptr) });
            }
            LRESULT(0)
        }

        WM_PAINT => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ToolbarState;
            if !ptr.is_null() {
                unsafe { paint(hwnd, &*ptr); }
            } else {
                let mut ps = PAINTSTRUCT::default();
                unsafe { BeginPaint(hwnd, &mut ps); }
                unsafe { let _ = EndPaint(hwnd, &ps); }
            }
            LRESULT(0)
        }

        WM_MOUSEMOVE => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ToolbarState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let new_hover = hit_test(state, x, y);

                if new_hover != state.hover_index {
                    state.hover_index = new_hover;
                    unsafe { let _ = InvalidateRect(Some(hwnd), None, false); }
                }

                if !state.tracking_mouse {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = unsafe { TrackMouseEvent(&mut tme) };
                    state.tracking_mouse = true;
                }
            }
            LRESULT(0)
        }

        x if x == WM_MOUSELEAVE => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ToolbarState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                state.hover_index = None;
                state.tracking_mouse = false;
                unsafe { let _ = InvalidateRect(Some(hwnd), None, false); }
            }
            LRESULT(0)
        }

        WM_LBUTTONDOWN => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ToolbarState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                state.pressed_index = hit_test(state, x, y);
                unsafe { let _ = InvalidateRect(Some(hwnd), None, false); }
            }
            LRESULT(0)
        }

        WM_LBUTTONUP => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ToolbarState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let clicked = hit_test(state, x, y);

                if clicked.is_some() && clicked == state.pressed_index {
                    let idx = clicked.unwrap();
                    if state.buttons[idx].is_refresh {
                        unsafe { let _ = PostMessageW(Some(hwnd), WM_USER_REFRESH, WPARAM(0), LPARAM(0)); }
                    } else {
                        let path = state.buttons[idx].folder.path.clone();
                        if let Some(ref sb) = state.shell_browser {
                            let _ = crate::navigate::navigate_to(sb, &path);
                        }
                    }
                }

                state.pressed_index = None;
                unsafe { let _ = InvalidateRect(Some(hwnd), None, false); }
            }
            LRESULT(0)
        }

        x if x == WM_USER_REFRESH => {
            refresh_toolbar(hwnd);
            LRESULT(0)
        }

        x if x == WM_DPICHANGED => {
            let new_dpi = (wparam.0 & 0xFFFF) as u32;
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ToolbarState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                state.dpi = new_dpi;
                let mut client = RECT::default();
                unsafe { let _ = GetClientRect(hwnd, &mut client); }
                compute_layout(state, &client);
                unsafe { let _ = InvalidateRect(Some(hwnd), None, true); }
            }
            LRESULT(0)
        }

        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// WM_DPICHANGED = 0x02E0
const WM_DPICHANGED: u32 = 0x02E0;

/// Safety wrapper — catches panics at the FFI boundary.
unsafe extern "system" fn toolbar_wndproc_safe(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match std::panic::catch_unwind(AssertUnwindSafe(|| unsafe {
        toolbar_wndproc(hwnd, msg, wparam, lparam)
    })) {
        Ok(r) => r,
        Err(_) => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Register (once) and create the toolbar window.
///
/// `shell_browser` may be `None` when called from the CBT hook path; navigation
/// will be unavailable but the visual toolbar will still appear.
pub fn create_toolbar(
    parent: HWND,
    bounds: &RECT,
    hinstance: windows::Win32::Foundation::HINSTANCE,
    shell_browser: Option<IShellBrowser>,
) -> Option<HWND> {
    CLASS_REGISTERED.call_once(|| {
        let class_name_wide: Vec<u16> = CLASS_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(toolbar_wndproc_safe),
            cbClsExtra: 0,
            cbWndExtra: std::mem::size_of::<*mut ToolbarState>() as i32,
            hInstance: hinstance,
            hIcon: windows::Win32::UI::WindowsAndMessaging::HICON::default(),
            hCursor: windows::Win32::UI::WindowsAndMessaging::HCURSOR::default(),
            hbrBackground: HBRUSH::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: PCWSTR(class_name_wide.as_ptr()),
            hIconSm: windows::Win32::UI::WindowsAndMessaging::HICON::default(),
        };

        unsafe { RegisterClassExW(&wc) };
    });

    let dpi = get_dpi(parent);
    let config = Config::load();
    let state = Box::new(ToolbarState::new(dpi, config, shell_browser));
    let state_ptr = Box::into_raw(state);

    let class_name_wide: Vec<u16> = CLASS_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let width = bounds.right - bounds.left;
    let height = bounds.bottom - bounds.top;

    let hwnd_result = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name_wide.as_ptr()),
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE,
            bounds.left,
            bounds.top,
            width,
            height,
            Some(parent),
            Some(HMENU::default()),
            Some(hinstance),
            Some(state_ptr as *const _ as *const std::ffi::c_void),
        )
    };

    match hwnd_result {
        Ok(hwnd) => Some(hwnd),
        Err(_) => {
            drop(unsafe { Box::from_raw(state_ptr) });
            None
        }
    }
}

/// Reload config and repaint the toolbar.
pub fn refresh_toolbar(hwnd: HWND) {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ToolbarState;
    if ptr.is_null() {
        return;
    }
    let state = unsafe { &mut *ptr };
    state.config = Config::load();

    let mut client = RECT::default();
    unsafe { let _ = GetClientRect(hwnd, &mut client); }
    compute_layout(state, &client);

    unsafe { let _ = InvalidateRect(Some(hwnd), None, true); }
}
