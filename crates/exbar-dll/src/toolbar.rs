//! Floating draggable toolbar window for Explorer folder shortcuts.

use std::panic::AssertUnwindSafe;
use std::sync::{Mutex, Once};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DEFAULT_GUI_FONT, DeleteObject,
    DrawTextW, EndPaint, FillRect, GetStockObject,
    InvalidateRect, PAINTSTRUCT, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT, DT_SINGLELINE, DT_VCENTER, DT_CENTER,
    ScreenToClient,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, PostMessageW, RegisterClassExW,
    SetWindowLongPtrW, GetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW, CS_HREDRAW,
    CS_VREDRAW, GWLP_USERDATA, WNDCLASSEXW, WM_CREATE, WM_DESTROY, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_PAINT, WS_POPUP, WS_VISIBLE, WS_EX_TOOLWINDOW,
    WM_NCHITTEST, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_NOACTIVATE, HTCAPTION,
    WS_EX_LAYERED, SetLayeredWindowAttributes, LWA_ALPHA,
    SystemParametersInfoW, SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    WM_MOVE, IsWindow, SW_HIDE, SW_SHOWNA, GetForegroundWindow,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows_core::PCWSTR;

use crate::config::{Config, FolderEntry, Layout};
use crate::theme;

// ── Constants ────────────────────────────────────────────────────────────────

static CLASS_REGISTERED: Once = Once::new();
const CLASS_NAME: &str = "ExbarToolbar";
const WM_USER_REFRESH: u32 = 0x0401;
const WM_DPICHANGED: u32 = 0x02E0;

// Layout constants (logical pixels, scale by DPI)
const BTN_PAD_H: i32 = 10;
const BTN_PAD_V: i32 = 4;
const BTN_GAP: i32 = 2;
const REFRESH_SIZE: i32 = 28;
/// Logical pixel width/height of the drag handle grip area.
const GRIP_SIZE: i32 = 12;

// ── Global state ──────────────────────────────────────────────────────────────

/// The single global toolbar HWND (None if not yet created or destroyed).
static GLOBAL_TOOLBAR: Mutex<Option<isize>> = Mutex::new(None);

/// The most recently activated Explorer (CabinetWClass) HWND.
static ACTIVE_EXPLORER: Mutex<Option<isize>> = Mutex::new(None);

pub fn set_active_explorer(hwnd: HWND) {
    *ACTIVE_EXPLORER.lock().unwrap() = Some(hwnd.0 as isize);
}

pub fn get_active_explorer() -> Option<HWND> {
    ACTIVE_EXPLORER.lock().unwrap().map(|h| HWND(h as *mut _))
}

/// Check whether the global toolbar already exists (window is still valid).
pub fn global_toolbar_exists() -> bool {
    let guard = GLOBAL_TOOLBAR.lock().unwrap();
    match *guard {
        None => false,
        Some(h) => {
            let hwnd = HWND(h as *mut _);
            unsafe { IsWindow(Some(hwnd)).as_bool() }
        }
    }
}

fn set_global_toolbar(hwnd: HWND) {
    *GLOBAL_TOOLBAR.lock().unwrap() = Some(hwnd.0 as isize);
}

fn clear_global_toolbar() {
    *GLOBAL_TOOLBAR.lock().unwrap() = None;
}

fn get_global_toolbar_hwnd() -> Option<HWND> {
    GLOBAL_TOOLBAR.lock().unwrap().map(|h| HWND(h as *mut _))
}

// ── Foreground window tracking ───────────────────────────────────────────────

static FOREGROUND_HOOK_INSTALLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const EVENT_SYSTEM_MINIMIZESTART: u32 = 0x0016;
const EVENT_SYSTEM_MINIMIZEEND: u32 = 0x0017;
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;

/// Check if the given HWND belongs to the current process (explorer.exe).
fn hwnd_in_our_process(hwnd: HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)); }
    pid != 0 && pid == std::process::id()
}

unsafe extern "system" fn foreground_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    let tb = match get_global_toolbar_hwnd() {
        Some(h) => h,
        None => return,
    };

    let class = crate::explorer::get_class_name(hwnd);
    let is_explorer = class == "CabinetWClass";
    let in_our_process = hwnd_in_our_process(hwnd);

    if event == EVENT_SYSTEM_MINIMIZESTART {
        // Only hide if NOT our process (avoid hiding on Explorer's internal popups)
        if !in_our_process {
            update_toolbar_visibility(tb);
        }
        return;
    }

    if event == EVENT_SYSTEM_MINIMIZEEND {
        if is_explorer {
            show_above(tb, hwnd);
        }
        return;
    }

    // EVENT_SYSTEM_FOREGROUND
    // Keep toolbar visible if the foreground window is:
    //   - An Explorer window (re-raise above it)
    //   - OUR process (tooltips, tree view items, menus — all transient)
    // Hide only when a window in a DIFFERENT process takes foreground.
    if is_explorer {
        set_active_explorer(hwnd);
        show_above(tb, hwnd);
    } else if in_our_process {
        // Transient Explorer-owned window — don't hide, just ensure we stay topmost
        // (no-op — already topmost)
    } else {
        unsafe { let _ = ShowWindow(tb, SW_HIDE); }
    }
}

fn show_above(toolbar: HWND, _explorer: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST;
    unsafe {
        let _ = ShowWindow(toolbar, SW_SHOWNA);
        // Use HWND_TOPMOST so the toolbar stays above Explorer reliably.
        // When a non-Explorer app is foreground, the toolbar is hidden entirely,
        // so topmost won't intrude on other applications.
        let _ = SetWindowPos(
            toolbar,
            Some(HWND_TOPMOST),
            0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// Hide the toolbar if the foreground window is in a different process
/// (i.e., not Explorer or any of its helper windows).
fn update_toolbar_visibility(toolbar: HWND) {
    let fg = unsafe { GetForegroundWindow() };
    if !hwnd_in_our_process(fg) {
        unsafe { let _ = ShowWindow(toolbar, SW_HIDE); }
    }
}

fn install_foreground_hook() {
    use std::sync::atomic::Ordering;
    if FOREGROUND_HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_MINIMIZEEND, // range covers FOREGROUND, MINIMIZESTART, MINIMIZEEND
            None,
            Some(foreground_event_proc),
            0, 0,
            WINEVENT_OUTOFCONTEXT,
        );
    }
    crate::log::info("Installed foreground event hook");
}

// ── Position persistence ──────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedPos { x: i32, y: i32 }

fn pos_file_path() -> std::path::PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| "C:\\Users\\Default".into());
    let mut p = std::path::PathBuf::from(home);
    p.push(".exbar-pos.json");
    p
}

fn load_saved_pos() -> Option<(i32, i32)> {
    let bytes = std::fs::read(pos_file_path()).ok()?;
    let saved: SavedPos = serde_json::from_slice(&bytes).ok()?;
    Some((saved.x, saved.y))
}

fn save_pos(x: i32, y: i32) {
    let saved = SavedPos { x, y };
    if let Ok(json) = serde_json::to_string(&saved) {
        let _ = std::fs::write(pos_file_path(), json);
    }
}

// ── Screen bounds clamping ────────────────────────────────────────────────────

/// Return the work area of the monitor containing `ref_hwnd`, or the primary
/// monitor work area if that fails.
fn work_area_for(ref_hwnd: Option<HWND>) -> RECT {
    use windows::Win32::Graphics::Gdi::{MonitorFromWindow, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO};

    if let Some(hwnd) = ref_hwnd {
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        if !monitor.is_invalid() {
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if unsafe { GetMonitorInfoW(monitor, &mut mi) }.as_bool() {
                return mi.rcWork;
            }
        }
    }
    // Fallback: primary monitor work area
    let mut wa = RECT::default();
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut wa as *mut RECT as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    wa
}

fn clamp_to_work_area(x: i32, y: i32, w: i32, h: i32, ref_hwnd: Option<HWND>) -> (i32, i32) {
    let wa = work_area_for(ref_hwnd);
    let cx = x.max(wa.left).min((wa.right - w).max(wa.left));
    let cy = y.max(wa.top).min((wa.bottom - h).max(wa.top));
    (cx, cy)
}

// ── Data structures ──────────────────────────────────────────────────────────

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
    tracking_mouse: bool,
    layout: Layout,
    drop_registered: bool,
    /// Logical pixel size of the grip (already includes DPI scale factor).
    grip_size: i32,
}

impl ToolbarState {
    fn new(dpi: u32, config: Option<Config>) -> Self {
        let layout = config.as_ref().map_or(Layout::Horizontal, |c| c.layout);
        ToolbarState {
            buttons: Vec::new(),
            hover_index: None,
            pressed_index: None,
            dpi,
            config,
            tracking_mouse: false,
            layout,
            drop_registered: false,
            grip_size: theme::scale(GRIP_SIZE, dpi),
        }
    }
}

// ── Layout computation ───────────────────────────────────────────────────────

/// Compute button positions. Returns the required (width, height) for the window.
fn compute_layout(state: &mut ToolbarState) -> (i32, i32) {
    state.buttons.clear();
    let dpi = state.dpi;
    let s = |px: i32| theme::scale(px, dpi);

    let btn_h = s(REFRESH_SIZE);
    let pad_h = s(BTN_PAD_H);
    let gap = s(BTN_GAP);
    let grip = state.grip_size;

    let is_vertical = state.layout == Layout::Vertical;

    let folder_names: Vec<String> = state.config.as_ref()
        .map_or(Vec::new(), |c| c.folders.iter().map(|f| f.name.clone()).collect());

    let char_w = s(8);

    let mut max_btn_w = s(REFRESH_SIZE);
    for name in &folder_names {
        let w = pad_h + s(14) + s(4) + (name.chars().count() as i32 * char_w) + pad_h;
        if w > max_btn_w { max_btn_w = w; }
    }

    if is_vertical {
        // Grip at top, then refresh, then folder buttons
        let mut y = grip; // skip grip row

        state.buttons.push(ButtonLayout {
            rect: RECT { left: 0, top: y, right: max_btn_w, bottom: y + btn_h },
            folder: FolderEntry { name: "\u{21BB}".into(), path: String::new(), icon: None },
            is_refresh: true,
        });
        y += btn_h + gap;

        if let Some(ref config) = state.config.clone() {
            for entry in &config.folders {
                state.buttons.push(ButtonLayout {
                    rect: RECT { left: 0, top: y, right: max_btn_w, bottom: y + btn_h },
                    folder: entry.clone(),
                    is_refresh: false,
                });
                y += btn_h + gap;
            }
        }

        (max_btn_w, y - gap)
    } else {
        // Grip on left, then refresh, then folder buttons
        let mut x = grip; // skip grip column

        let refresh_w = s(REFRESH_SIZE);
        state.buttons.push(ButtonLayout {
            rect: RECT { left: x, top: 0, right: x + refresh_w, bottom: btn_h },
            folder: FolderEntry { name: "\u{21BB}".into(), path: String::new(), icon: None },
            is_refresh: true,
        });
        x += refresh_w + gap;

        if let Some(ref config) = state.config.clone() {
            for entry in &config.folders {
                let w = pad_h + (entry.name.chars().count() as i32 * char_w) + pad_h;
                state.buttons.push(ButtonLayout {
                    rect: RECT { left: x, top: 0, right: x + w, bottom: btn_h },
                    folder: entry.clone(),
                    is_refresh: false,
                });
                x += w + gap;
            }
        }

        (x - gap, btn_h)
    }
}

// ── Hit test ─────────────────────────────────────────────────────────────────

fn hit_test(state: &ToolbarState, x: i32, y: i32) -> Option<usize> {
    state.buttons.iter().position(|b| {
        x >= b.rect.left && x < b.rect.right && y >= b.rect.top && y < b.rect.bottom
    })
}

/// Returns true if (x, y) is in the grip area.
fn in_grip(state: &ToolbarState, x: i32, y: i32) -> bool {
    match state.layout {
        Layout::Horizontal => x < state.grip_size,
        Layout::Vertical   => y < state.grip_size,
    }
}

// ── Painting ─────────────────────────────────────────────────────────────────

unsafe fn paint(hwnd: HWND, state: &ToolbarState) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    if hdc.is_invalid() { return; }

    let mut client = RECT::default();
    unsafe { let _ = GetClientRect(hwnd, &mut client); }

    let is_dark = theme::is_dark_mode();

    // Background
    let bg_color = if is_dark { COLORREF(0x002D2D2D) } else { COLORREF(0x00F0F0F0) };
    let bg_brush = unsafe { CreateSolidBrush(bg_color) };
    unsafe { FillRect(hdc, &client, bg_brush); }
    unsafe { DeleteObject(bg_brush.into()); }

    // Grip area — draw dots
    let grip = state.grip_size;
    let grip_color = if is_dark { COLORREF(0x00888888) } else { COLORREF(0x00999999) };
    let grip_brush = unsafe { CreateSolidBrush(grip_color) };
    let dot_size = theme::scale(2, state.dpi);
    let dot_gap  = theme::scale(4, state.dpi);

    match state.layout {
        Layout::Horizontal => {
            // Three vertical dots centered in the grip column
            let cx = grip / 2;
            let total_h = dot_size * 3 + dot_gap * 2;
            let start_y = (client.bottom - client.top - total_h) / 2;
            for i in 0..3i32 {
                let dy = start_y + i * (dot_size + dot_gap);
                let dot = RECT {
                    left: cx - dot_size / 2,
                    top:  dy,
                    right:  cx + dot_size / 2 + 1,
                    bottom: dy + dot_size,
                };
                unsafe { FillRect(hdc, &dot, grip_brush); }
            }
        }
        Layout::Vertical => {
            // Three horizontal dots centered in the grip row
            let cy = grip / 2;
            let total_w = dot_size * 3 + dot_gap * 2;
            let start_x = (client.right - client.left - total_w) / 2;
            for i in 0..3i32 {
                let dx = start_x + i * (dot_size + dot_gap);
                let dot = RECT {
                    left:   dx,
                    top:    cy - dot_size / 2,
                    right:  dx + dot_size,
                    bottom: cy + dot_size / 2 + 1,
                };
                unsafe { FillRect(hdc, &dot, grip_brush); }
            }
        }
    }
    unsafe { DeleteObject(grip_brush.into()); }

    // Border
    let border_color = if is_dark { COLORREF(0x00555555) } else { COLORREF(0x00CCCCCC) };
    let border_brush = unsafe { CreateSolidBrush(border_color) };
    let top_border = RECT { left: client.left, top: client.top, right: client.right, bottom: client.top + 1 };
    unsafe { FillRect(hdc, &top_border, border_brush); }
    let bottom_border = RECT { left: client.left, top: client.bottom - 1, right: client.right, bottom: client.bottom };
    unsafe { FillRect(hdc, &bottom_border, border_brush); }
    let left_border = RECT { left: client.left, top: client.top, right: client.left + 1, bottom: client.bottom };
    unsafe { FillRect(hdc, &left_border, border_brush); }
    let right_border = RECT { left: client.right - 1, top: client.top, right: client.right, bottom: client.bottom };
    unsafe { FillRect(hdc, &right_border, border_brush); }
    unsafe { DeleteObject(border_brush.into()); }

    unsafe { SetBkMode(hdc, TRANSPARENT); }

    let text_cr = if is_dark { COLORREF(0x00FFFFFF) } else { COLORREF(0x00202020) };
    unsafe { SetTextColor(hdc, text_cr); }

    let default_font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
    let old_font = unsafe { SelectObject(hdc, default_font) };

    for (i, btn) in state.buttons.iter().enumerate() {
        let is_hover   = state.hover_index   == Some(i);
        let is_pressed = state.pressed_index == Some(i);

        if is_pressed {
            let hl = if is_dark { COLORREF(0x00505050) } else { COLORREF(0x00D0D0D0) };
            let hbr = unsafe { CreateSolidBrush(hl) };
            unsafe { FillRect(hdc, &btn.rect, hbr); }
            unsafe { DeleteObject(hbr.into()); }
        } else if is_hover {
            let hl = if is_dark { COLORREF(0x003D3D3D) } else { COLORREF(0x00E0E0E0) };
            let hbr = unsafe { CreateSolidBrush(hl) };
            unsafe { FillRect(hdc, &btn.rect, hbr); }
            unsafe { DeleteObject(hbr.into()); }
        }

        let label = if btn.is_refresh {
            "\u{21BB}".to_string()
        } else {
            format!("\u{1F4C1} {}", btn.folder.name)
        };

        let mut label_wide: Vec<u16> = label.encode_utf16().collect();
        let mut draw_rect = btn.rect;
        let flags = if btn.is_refresh {
            DT_SINGLELINE | DT_VCENTER | DT_CENTER
        } else {
            DT_SINGLELINE | DT_VCENTER
        };
        if !btn.is_refresh {
            draw_rect.left += theme::scale(BTN_PAD_H, state.dpi);
        }
        unsafe { DrawTextW(hdc, &mut label_wide, &mut draw_rect, flags); }
    }

    if !old_font.is_invalid() {
        unsafe { SelectObject(hdc, old_font); }
    }

    unsafe { let _ = EndPaint(hwnd, &ps); }
}

// ── Window procedure ─────────────────────────────────────────────────────────

unsafe fn toolbar_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
            let state_ptr = cs.lpCreateParams as *mut ToolbarState;
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize) };

            let state = unsafe { &mut *state_ptr };
            let (w, h) = compute_layout(state);

            // Now that we know the real size, re-clamp position to fit entirely
            // within the current monitor's work area (the Explorer's monitor).
            let mut current_rect = RECT::default();
            unsafe { let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut current_rect); }
            let (final_x, final_y) = clamp_to_work_area(
                current_rect.left,
                current_rect.top,
                w, h,
                Some(hwnd),
            );

            unsafe {
                let _ = SetWindowPos(hwnd, None, final_x, final_y, w, h,
                    SWP_NOZORDER | SWP_NOACTIVATE);
            }

            // Apply layered window transparency
            apply_opacity(hwnd, state);

            // Register drop target
            register_drop_targets(hwnd, state);

            // Install foreground window hook to auto-show/hide the toolbar
            install_foreground_hook();

            // Initial visibility: only show if an Explorer window is foreground
            let fg = unsafe { GetForegroundWindow() };
            let fg_class = crate::explorer::get_class_name(fg);
            if fg_class != "CabinetWClass" {
                unsafe { let _ = ShowWindow(hwnd, SW_HIDE); }
            } else {
                show_above(hwnd, fg);
            }

            LRESULT(0)
        }

        WM_DESTROY => {
            clear_global_toolbar();
            crate::dragdrop::unregister_drop_target(hwnd);
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ToolbarState;
            if !ptr.is_null() {
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
                drop(unsafe { Box::from_raw(ptr) });
            }
            LRESULT(0)
        }

        WM_NCHITTEST => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;

            let mut pt = POINT { x, y };
            unsafe { ScreenToClient(hwnd, &mut pt); }

            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ToolbarState;
            if !ptr.is_null() {
                let state = unsafe { &*ptr };
                if in_grip(state, pt.x, pt.y) {
                    return LRESULT(HTCAPTION as isize);
                }
                if hit_test(state, pt.x, pt.y).is_some() {
                    return LRESULT(1); // HTCLIENT
                }
            }
            LRESULT(HTCAPTION as isize)
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

        WM_MOVE => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            save_pos(x, y);
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
                        // Get the active Explorer fresh at click time.
                        if let Some(explorer_hwnd) = get_active_explorer() {
                            if let Some(sb) = unsafe { crate::hook::get_shell_browser_for(explorer_hwnd) } {
                                let _ = crate::navigate::navigate_to(&sb, &path);
                            }
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
                state.grip_size = theme::scale(GRIP_SIZE, new_dpi);
                let (w, h) = compute_layout(state);
                unsafe {
                    let _ = SetWindowPos(hwnd, None, 0, 0, w, h,
                        SWP_NOZORDER | SWP_NOACTIVATE | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE);
                    let _ = InvalidateRect(Some(hwnd), None, true);
                }
            }
            LRESULT(0)
        }

        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe extern "system" fn toolbar_wndproc_safe(
    hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM,
) -> LRESULT {
    match std::panic::catch_unwind(AssertUnwindSafe(|| unsafe {
        toolbar_wndproc(hwnd, msg, wparam, lparam)
    })) {
        Ok(r) => r,
        Err(_) => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// ── Opacity ───────────────────────────────────────────────────────────────────

fn apply_opacity(hwnd: HWND, state: &ToolbarState) {
    let opacity = state.config.as_ref().map_or(0.8, |c| c.background_opacity);
    let alpha = (opacity.clamp(0.0, 1.0) * 255.0) as u8;
    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
    }
}

// ── Drop target registration ─────────────────────────────────────────────────

fn register_drop_targets(hwnd: HWND, state: &mut ToolbarState) {
    if state.drop_registered { return; }
    if state.config.as_ref().map_or(true, |c| c.folders.is_empty()) { return; }

    // Capture button rects + paths for the closure (avoids holding borrow on state).
    let button_info: Vec<(RECT, String)> = state.buttons.iter()
        .filter(|b| !b.is_refresh)
        .map(|b| (b.rect, b.folder.path.clone()))
        .collect();

    let resolver = move |cx: i32, cy: i32| -> Option<String> {
        button_info.iter()
            .find(|(r, _)| cx >= r.left && cx < r.right && cy >= r.top && cy < r.bottom)
            .map(|(_, p)| p.clone())
    };

    if crate::dragdrop::register_drop_target(hwnd, Box::new(resolver)).is_ok() {
        state.drop_registered = true;
        crate::log::info("Registered OLE drop target on toolbar (cursor-based)");
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

pub fn create_toolbar(
    owner: HWND,
    screen_pos: &RECT,
    hinstance: windows::Win32::Foundation::HINSTANCE,
) -> Option<HWND> {
    CLASS_REGISTERED.call_once(|| {
        let class_wide: Vec<u16> = CLASS_NAME.encode_utf16().chain(std::iter::once(0)).collect();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(toolbar_wndproc_safe),
            cbClsExtra: 0,
            cbWndExtra: std::mem::size_of::<*mut ToolbarState>() as i32,
            hInstance: hinstance,
            lpszClassName: PCWSTR(class_wide.as_ptr()),
            ..Default::default()
        };
        unsafe { RegisterClassExW(&wc) };
    });

    let dpi = theme::get_dpi(owner);
    let config = Config::load();
    let is_dark = theme::is_dark_mode();
    crate::log::info(&format!("create_toolbar: dark_mode={is_dark}"));

    let state = Box::new(ToolbarState::new(dpi, config));
    let state_ptr = Box::into_raw(state);

    let class_wide: Vec<u16> = CLASS_NAME.encode_utf16().chain(std::iter::once(0)).collect();

    // Determine initial window position: saved pos > default pos
    let (mut x, mut y) = load_saved_pos().unwrap_or((screen_pos.left, screen_pos.top));

    // Rough placeholder size for clamping; resized in WM_CREATE.
    // Clamp using the monitor that contains the triggering Explorer window.
    let placeholder_w = 400;
    let placeholder_h = 30;
    let clamped = clamp_to_work_area(x, y, placeholder_w, placeholder_h, Some(owner));
    x = clamped.0;
    y = clamped.1;

    crate::log::info(&format!("create_toolbar: screen x={x} y={y}"));

    // Create as a TOP-LEVEL popup (no owner) so it survives individual
    // Explorer window closures. The `owner` HWND is used for monitor
    // detection only, not as the parent/owner.
    let hwnd_result = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            PCWSTR(class_wide.as_ptr()),
            PCWSTR::null(),
            WS_POPUP | WS_VISIBLE,
            x, y,
            placeholder_w, placeholder_h,
            None, // no owner — independent top-level window
            None,
            Some(hinstance),
            Some(state_ptr as *const _ as *const std::ffi::c_void),
        )
    };

    // Prevent "unused" warning when the owner only informs monitor choice.
    let _ = owner;

    match hwnd_result {
        Ok(hwnd) => {
            set_global_toolbar(hwnd);
            Some(hwnd)
        }
        Err(_) => {
            drop(unsafe { Box::from_raw(state_ptr) });
            None
        }
    }
}

pub fn refresh_toolbar(hwnd: HWND) {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ToolbarState;
    if ptr.is_null() { return; }
    let state = unsafe { &mut *ptr };
    state.config = Config::load();
    state.layout = state.config.as_ref().map_or(Layout::Horizontal, |c| c.layout);

    // Re-apply opacity in case config changed.
    apply_opacity(hwnd, state);

    let (w, h) = compute_layout(state);
    unsafe {
        let _ = SetWindowPos(hwnd, None, 0, 0, w, h,
            SWP_NOZORDER | SWP_NOACTIVATE | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE);
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}
