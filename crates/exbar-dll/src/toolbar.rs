//! Floating draggable toolbar window for Explorer folder shortcuts.

use std::panic::AssertUnwindSafe;
use std::sync::{Mutex, Once};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DEFAULT_GUI_FONT, DeleteObject,
    DrawTextW, EndPaint, FillRect, GetStockObject,
    InvalidateRect, PAINTSTRUCT, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT, DT_SINGLELINE, DT_VCENTER, DT_CENTER,
    ScreenToClient, ClientToScreen,
};
use windows::Win32::UI::Controls::{WM_MOUSELEAVE, WC_EDITW};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, SetFocus,
    SetCapture, ReleaseCapture,
};
use windows::Win32::System::SystemServices::MK_CONTROL;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, PostMessageW, RegisterClassExW,
    SetWindowLongPtrW, GetWindowLongPtrW, SetWindowPos, ShowWindow, CREATESTRUCTW, CS_HREDRAW,
    CS_VREDRAW, GWLP_USERDATA, WNDCLASSEXW, WM_CREATE, WM_DESTROY, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_PAINT, WS_POPUP, WS_VISIBLE, WS_EX_TOOLWINDOW,
    WM_NCHITTEST, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_NOACTIVATE, HTCAPTION,
    WS_EX_LAYERED, SetLayeredWindowAttributes, LWA_ALPHA,
    SystemParametersInfoW, SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    WM_MOVE, IsWindow, SW_HIDE, SW_SHOWNA, GetForegroundWindow, WM_RBUTTONUP, WS_EX_NOACTIVATE,
    DestroyWindow, GetWindowTextLengthW, GetWindowTextW, SendMessageW,
    WS_CHILD, WS_BORDER, WM_KEYDOWN, WM_KILLFOCUS, WM_GETDLGCODE, DLGC_WANTALLKEYS,
    WM_CAPTURECHANGED,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows_core::PCWSTR;

use crate::config::{Config, FolderEntry, Layout};
use crate::theme;

// ── Constants ────────────────────────────────────────────────────────────────

static CLASS_REGISTERED: Once = Once::new();
const CLASS_NAME: &str = "ExbarToolbar";
const WM_USER_RELOAD: u32 = 0x0401;
const WM_DPICHANGED: u32 = 0x02E0;

// Layout constants (logical pixels, scale by DPI)
const BTN_PAD_H: i32 = 10;
const BTN_PAD_V: i32 = 4;
const BTN_GAP: i32 = 2;
const ADD_SIZE: i32 = 28;
/// Logical pixel width/height of the drag handle grip area.
const GRIP_SIZE: i32 = 12;

const REORDER_THRESHOLD: i32 = 5;

const MENU_ID_EDIT_CONFIG: u32 = 101;
const MENU_ID_RELOAD_CONFIG: u32 = 102;

const MENU_ID_OPEN: u32 = 201;
const MENU_ID_OPEN_NEW_TAB: u32 = 202;
const MENU_ID_COPY_PATH: u32 = 203;
const MENU_ID_RENAME: u32 = 204;
const MENU_ID_REMOVE: u32 = 205;

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

/// Stored HWINEVENTHOOK so we can UnhookWinEvent on DLL unload.
static FOREGROUND_HOOK: Mutex<Option<isize>> = Mutex::new(None);

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
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_MINIMIZEEND, // range covers FOREGROUND, MINIMIZESTART, MINIMIZEEND
            None,
            Some(foreground_event_proc),
            0, 0,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    *FOREGROUND_HOOK.lock().unwrap() = Some(hook.0 as isize);
    crate::log::info("Installed foreground event hook");
}

/// Tear down the toolbar window and unhook the WinEvent hook.
/// Called from DllMain DLL_PROCESS_DETACH so explorer.exe doesn't crash
/// when our DLL gets unloaded (the toolbar's wndproc and the WinEvent
/// callback are both in our DLL — they MUST stop being referenced before
/// our code is unmapped).
pub fn cleanup_for_unload() {
    use windows::Win32::UI::Accessibility::UnhookWinEvent;
    use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

    // Unhook WinEvent first so no more events dispatch into our about-to-be-gone callback
    if let Some(hook_raw) = FOREGROUND_HOOK.lock().unwrap().take() {
        let hook = HWINEVENTHOOK(hook_raw as *mut _);
        unsafe { let _ = UnhookWinEvent(hook); }
    }

    // Destroy the toolbar window so its wndproc doesn't get called after unload
    if let Some(hwnd_raw) = GLOBAL_TOOLBAR.lock().unwrap().take() {
        let hwnd = HWND(hwnd_raw as *mut _);
        unsafe { let _ = DestroyWindow(hwnd); }
    }
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

#[derive(Clone, Copy)]
struct ReorderState {
    /// Source button index (NOT folder index). Always >= 1 since + is at 0.
    source_button: usize,
    press_x: i32,
    press_y: i32,
    /// False until mouse has moved REORDER_THRESHOLD logical pixels.
    active: bool,
    /// Insertion point in folder-index space: 0..=folders.len().
    insertion: usize,
}

struct ButtonLayout {
    rect: RECT,
    folder: FolderEntry,
    /// The synthetic "add folder" button (formerly the refresh glyph).
    is_add: bool,
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
    reorder: Option<ReorderState>,
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
            reorder: None,
        }
    }
}

// ── Layout computation ───────────────────────────────────────────────────────

/// Compute button positions. Returns the required (width, height) for the window.
fn compute_layout(state: &mut ToolbarState) -> (i32, i32) {
    state.buttons.clear();
    let dpi = state.dpi;
    let s = |px: i32| theme::scale(px, dpi);

    let btn_h = s(ADD_SIZE);
    let pad_h = s(BTN_PAD_H);
    let gap = s(BTN_GAP);
    let grip = state.grip_size;

    let is_vertical = state.layout == Layout::Vertical;

    let folder_names: Vec<String> = state.config.as_ref()
        .map_or(Vec::new(), |c| c.folders.iter().map(|f| f.name.clone()).collect());

    let char_w = s(8);

    let mut max_btn_w = s(ADD_SIZE);
    for name in &folder_names {
        let w = pad_h + s(14) + s(4) + (name.chars().count() as i32 * char_w) + pad_h;
        if w > max_btn_w { max_btn_w = w; }
    }

    if is_vertical {
        // Grip at top, then add-button, then folder buttons
        let mut y = grip; // skip grip row

        state.buttons.push(ButtonLayout {
            rect: RECT { left: 0, top: y, right: max_btn_w, bottom: y + btn_h },
            folder: FolderEntry { name: "+".into(), path: String::new(), icon: None },
            is_add: true,
        });
        y += btn_h + gap;

        if let Some(ref config) = state.config.clone() {
            for entry in &config.folders {
                state.buttons.push(ButtonLayout {
                    rect: RECT { left: 0, top: y, right: max_btn_w, bottom: y + btn_h },
                    folder: entry.clone(),
                    is_add: false,
                });
                y += btn_h + gap;
            }
        }

        (max_btn_w, y - gap)
    } else {
        // Grip on left, then add-button, then folder buttons
        let mut x = grip; // skip grip column

        let refresh_w = s(ADD_SIZE);
        state.buttons.push(ButtonLayout {
            rect: RECT { left: x, top: 0, right: x + refresh_w, bottom: btn_h },
            folder: FolderEntry { name: "+".into(), path: String::new(), icon: None },
            is_add: true,
        });
        x += refresh_w + gap;

        if let Some(ref config) = state.config.clone() {
            for entry in &config.folders {
                // Match the vertical formula: padding + icon + gap + text + padding.
                // The drawn label is "📁 Name"; omitting the icon width cuts
                // off short names like "3D".
                let w = pad_h + s(14) + s(4) + (entry.name.chars().count() as i32 * char_w) + pad_h;
                state.buttons.push(ButtonLayout {
                    rect: RECT { left: x, top: 0, right: x + w, bottom: btn_h },
                    folder: entry.clone(),
                    is_add: false,
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

/// Given a horizontal cursor position, compute the folder-index insertion
/// point in `0..=folders.len()`. Uses each folder button's midpoint.
///
/// Caller guarantees the reorder gesture started on a folder button; this
/// function always returns a valid folder-index insertion (never index 0
/// for the + button slot — the + stays pinned at button[0]).
fn compute_insertion_index(state: &ToolbarState, cursor_x: i32) -> usize {
    let folder_buttons: Vec<&ButtonLayout> = state.buttons.iter()
        .filter(|b| !b.is_add)
        .collect();
    if folder_buttons.is_empty() { return 0; }
    // For vertical layout, fall back to "end" — visual isn't supported.
    if state.layout == Layout::Vertical { return folder_buttons.len(); }

    for (i, b) in folder_buttons.iter().enumerate() {
        let mid = (b.rect.left + b.rect.right) / 2;
        if cursor_x < mid {
            return i;
        }
    }
    folder_buttons.len()
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
        let is_dragging_source = state.reorder.as_ref()
            .map(|r| r.active && r.source_button == i)
            .unwrap_or(false);

        if is_dragging_source {
            // Don't draw hover/pressed highlight for the dragged button.
        } else if is_pressed {
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

        let label = if btn.is_add {
            "+".to_string()
        } else {
            format!("\u{1F4C1} {}", btn.folder.name)
        };

        // Dim text for the button being dragged.
        let text_cr_this = if is_dragging_source {
            if is_dark { COLORREF(0x00808080) } else { COLORREF(0x00A0A0A0) }
        } else {
            text_cr
        };
        unsafe { SetTextColor(hdc, text_cr_this); }

        let mut label_wide: Vec<u16> = label.encode_utf16().collect();
        let mut draw_rect = btn.rect;
        let flags = if btn.is_add {
            DT_SINGLELINE | DT_VCENTER | DT_CENTER
        } else {
            DT_SINGLELINE | DT_VCENTER
        };
        if !btn.is_add {
            draw_rect.left += theme::scale(BTN_PAD_H, state.dpi);
        }
        unsafe { DrawTextW(hdc, &mut label_wide, &mut draw_rect, flags); }
    }

    if !old_font.is_invalid() {
        unsafe { SelectObject(hdc, old_font); }
    }

    // Reorder insertion caret (horizontal layout only).
    if let Some(r) = state.reorder {
        if r.active && state.layout == Layout::Horizontal {
            let folder_buttons: Vec<&ButtonLayout> = state.buttons.iter()
                .filter(|b| !b.is_add)
                .collect();
            if !folder_buttons.is_empty() {
                // X coordinate of the caret.
                let caret_x = if r.insertion >= folder_buttons.len() {
                    folder_buttons.last().unwrap().rect.right + 1
                } else {
                    folder_buttons[r.insertion].rect.left - 1
                };
                let caret_w = theme::scale(2, state.dpi);
                let caret_color = if is_dark { COLORREF(0x00A0A0FF) } else { COLORREF(0x004040C0) };
                let caret_brush = unsafe { CreateSolidBrush(caret_color) };
                let caret_rect = RECT {
                    left: caret_x,
                    top: client.top + 2,
                    right: caret_x + caret_w,
                    bottom: client.bottom - 2,
                };
                unsafe { FillRect(hdc, &caret_rect, caret_brush); }
                unsafe { DeleteObject(caret_brush.into()); }
            }
        }
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

            // Initial visibility: prefer the Explorer HWND that triggered creation
            // (tracked in ACTIVE_EXPLORER by the CBT hook). GetForegroundWindow() is
            // unreliable during HCBT_ACTIVATE handling because activation hasn't
            // completed yet.
            let explorer_hwnd = get_active_explorer().unwrap_or_else(|| unsafe { GetForegroundWindow() });
            let class = crate::explorer::get_class_name(explorer_hwnd);
            if class == "CabinetWClass" {
                crate::log::info(&format!("toolbar create: showing above explorer={explorer_hwnd:?}"));
                show_above(hwnd, explorer_hwnd);
            } else {
                crate::log::info(&format!("toolbar create: fg class={class}, hiding"));
                unsafe { let _ = ShowWindow(hwnd, SW_HIDE); }
            }

            LRESULT(0)
        }

        WM_DESTROY => {
            clear_global_toolbar();
            cancel_inline_rename();
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

                // Reorder tracking (runs before hover so active drag suppresses hover).
                if let Some(mut r) = state.reorder {
                    let moved = (x - r.press_x).abs() + (y - r.press_y).abs();
                    if !r.active && moved > theme::scale(REORDER_THRESHOLD, state.dpi) {
                        r.active = true;
                        // Capture was already taken on WM_LBUTTONDOWN.
                    }
                    if r.active {
                        r.insertion = compute_insertion_index(state, x);
                        state.reorder = Some(r);
                        state.hover_index = None;
                        unsafe { let _ = InvalidateRect(Some(hwnd), None, false); }
                        return LRESULT(0);
                    }
                    state.reorder = Some(r);
                }

                // Hover tracking (existing behavior)
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

        x if x == WM_CAPTURECHANGED => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ToolbarState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                state.reorder = None;
                state.pressed_index = None;
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
                // Only start a potential reorder on a folder button (not + and not grip).
                if let Some(idx) = state.pressed_index {
                    if !state.buttons[idx].is_add {
                        // Cancel any active inline rename before starting a reorder gesture.
                        cancel_inline_rename();
                        state.reorder = Some(ReorderState {
                            source_button: idx,
                            press_x: x,
                            press_y: y,
                            active: false,
                            insertion: idx - 1, // overwritten on first WM_MOUSEMOVE once active
                        });
                        // Capture on press so a fast flick out of the toolbar
                        // still routes WM_MOUSEMOVE / WM_LBUTTONUP back to us.
                        unsafe { let _ = SetCapture(hwnd); }
                    }
                }
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

                // Handle reorder first: if the gesture became active, commit and skip click.
                if let Some(r) = state.reorder.take() {
                    unsafe { let _ = ReleaseCapture(); }
                    if r.active {
                        let source_folder = r.source_button - 1; // button 0 is +
                        commit_reorder(hwnd, source_folder, r.insertion);
                        state.pressed_index = None;
                        unsafe { let _ = InvalidateRect(Some(hwnd), None, false); }
                        return LRESULT(0);
                    }
                    // Not active = plain click; fall through to existing click logic.
                }

                let clicked = hit_test(state, x, y);
                if clicked.is_some() && clicked == state.pressed_index {
                    let idx = clicked.unwrap();
                    if state.buttons[idx].is_add {
                        if let Some(path) = crate::picker::pick_folder() {
                            append_folder_and_reload(&path);
                        }
                    } else {
                        let path = state.buttons[idx].folder.path.clone();
                        let ctrl = (wparam.0 & MK_CONTROL.0 as usize) != 0;
                        if ctrl {
                            let timeout = state.config.as_ref()
                                .map(|c| c.new_tab_timeout_ms_zero_disables)
                                .unwrap_or(500);
                            crate::navigate::open_in_new_tab(get_active_explorer(), &path, timeout);
                        } else if let Some(explorer_hwnd) = get_active_explorer() {
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

        WM_RBUTTONUP => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ToolbarState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                if let Some(idx) = hit_test(state, x, y) {
                    let mut pt = POINT { x, y };
                    unsafe { ClientToScreen(hwnd, &mut pt); }
                    if state.buttons[idx].is_add {
                        let items = [
                            crate::contextmenu::MenuItem { id: MENU_ID_EDIT_CONFIG,   label: "Edit config" },
                            crate::contextmenu::MenuItem { id: MENU_ID_RELOAD_CONFIG, label: "Reload config" },
                        ];
                        let chosen = crate::contextmenu::show_menu(hwnd, pt, &items);
                        match chosen {
                            MENU_ID_EDIT_CONFIG => open_config_in_editor(),
                            MENU_ID_RELOAD_CONFIG => {
                                unsafe {
                                    let _ = PostMessageW(Some(hwnd), WM_USER_RELOAD, WPARAM(0), LPARAM(0));
                                }
                            }
                            _ => {}
                        }
                    } else {
                        let items = [
                            crate::contextmenu::MenuItem { id: MENU_ID_OPEN,         label: "Open" },
                            crate::contextmenu::MenuItem { id: MENU_ID_OPEN_NEW_TAB, label: "Open in new tab" },
                            crate::contextmenu::MenuItem { id: MENU_ID_COPY_PATH,    label: "Copy path" },
                            crate::contextmenu::SEPARATOR,
                            crate::contextmenu::MenuItem { id: MENU_ID_RENAME,       label: "Rename" },
                            crate::contextmenu::MenuItem { id: MENU_ID_REMOVE,       label: "Remove" },
                        ];
                        let chosen = crate::contextmenu::show_menu(hwnd, pt, &items);
                        let path = state.buttons[idx].folder.path.clone();
                        match chosen {
                            MENU_ID_OPEN => {
                                if let Some(explorer_hwnd) = get_active_explorer() {
                                    if let Some(sb) = unsafe { crate::hook::get_shell_browser_for(explorer_hwnd) } {
                                        let _ = crate::navigate::navigate_to(&sb, &path);
                                    }
                                }
                            }
                            MENU_ID_OPEN_NEW_TAB => {
                                let timeout = state.config.as_ref()
                                    .map(|c| c.new_tab_timeout_ms_zero_disables)
                                    .unwrap_or(500);
                                crate::navigate::open_in_new_tab(get_active_explorer(), &path, timeout);
                            }
                            MENU_ID_COPY_PATH => { copy_to_clipboard(&path); }
                            MENU_ID_RENAME => {
                                let rect = state.buttons[idx].rect;
                                let name = state.buttons[idx].folder.name.clone();
                                let folder_index = idx - 1; // + button at index 0
                                start_inline_rename(hwnd, rect, folder_index, &name);
                            }
                            MENU_ID_REMOVE => { remove_folder_at(hwnd, idx); }
                            _ => {}
                        }
                    }
                }
            }
            LRESULT(0)
        }

        x if x == WM_USER_RELOAD => {
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

    // Capture everything needed for the closure (must be Send+Sync, no borrows on state).
    #[derive(Clone)]
    struct Info { rect: RECT, action: ActionSource }
    #[derive(Clone)]
    enum ActionSource { Folder(String), Add }

    let button_info: Vec<Info> = state.buttons.iter().map(|b| Info {
        rect: b.rect,
        action: if b.is_add { ActionSource::Add } else { ActionSource::Folder(b.folder.path.clone()) },
    }).collect();

    let resolver = move |cx: i32, cy: i32| -> Option<crate::dragdrop::DropAction> {
        let hit = button_info.iter()
            .find(|i| cx >= i.rect.left && cx < i.rect.right && cy >= i.rect.top && cy < i.rect.bottom)?;
        Some(match &hit.action {
            ActionSource::Folder(p) => crate::dragdrop::DropAction::MoveCopyTo(p.clone()),
            ActionSource::Add => crate::dragdrop::DropAction::AddFolder,
        })
    };

    if crate::dragdrop::register_drop_target(hwnd, Box::new(resolver)).is_ok() {
        state.drop_registered = true;
        crate::log::info("Registered OLE drop target on toolbar");
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
        // WS_EX_NOACTIVATE: the toolbar is a companion window — clicking it
        // must NOT steal foreground focus from Explorer, or folder clicks
        // end up routed to a newly-activated toolbar and navigation fails.
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
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

/// Append a folder to `~/.exbar.json` using its basename as the label, then reload.
/// No-op on empty / invalid paths.
pub(crate) fn append_folder_and_reload(path: &std::path::Path) {
    let name = match path.file_name().and_then(|s| s.to_str()) {
        Some(n) if !n.is_empty() => n.to_owned(),
        _ => return,
    };
    let path_str = match path.to_str() {
        Some(s) => s.to_owned(),
        None => return,
    };

    // Load → mutate → save. If load fails (no file yet), start from a minimal config.
    let mut cfg = crate::config::Config::load().unwrap_or_else(|| {
        crate::config::Config::from_str(r#"{"folders":[]}"#).expect("default config parses")
    });
    cfg.add_folder(name, path_str);
    if let Err(e) = cfg.save() {
        crate::log::error(&format!("append_folder_and_reload: save failed: {e}"));
        return;
    }

    if let Some(hwnd) = get_global_toolbar_hwnd() {
        unsafe { let _ = PostMessageW(Some(hwnd), WM_USER_RELOAD, WPARAM(0), LPARAM(0)); }
    }
}

fn open_config_in_editor() {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let path = crate::config::default_config_path();
    let path_wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let verb_wide: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let _ = ShellExecuteW(
            None,
            PCWSTR(verb_wide.as_ptr()),
            PCWSTR(path_wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn copy_to_clipboard(text: &str) {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_size = wide.len() * std::mem::size_of::<u16>();

    unsafe {
        if OpenClipboard(None).is_err() { return; }
        let _ = EmptyClipboard();

        let hmem = match GlobalAlloc(GMEM_MOVEABLE, byte_size) {
            Ok(h) if !h.is_invalid() => h,
            _ => { let _ = CloseClipboard(); return; }
        };
        let dest = GlobalLock(hmem);
        if dest.is_null() {
            let _ = CloseClipboard();
            return;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, dest as *mut u8, byte_size);
        let _ = GlobalUnlock(hmem);

        // SetClipboardData takes ownership of the HGLOBAL on success.
        let _ = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(hmem.0)));
        let _ = CloseClipboard();
    }
}

fn commit_reorder(hwnd: HWND, from: usize, to: usize) {
    let mut cfg = match crate::config::Config::load() {
        Some(c) => c,
        None => return,
    };
    cfg.move_folder(from, to);
    if let Err(e) = cfg.save() {
        crate::log::error(&format!("commit_reorder: save failed: {e}"));
        return;
    }
    unsafe { let _ = PostMessageW(Some(hwnd), WM_USER_RELOAD, WPARAM(0), LPARAM(0)); }
}

fn remove_folder_at(hwnd: HWND, index: usize) {
    let mut cfg = match crate::config::Config::load() {
        Some(c) => c,
        None => return,
    };
    // The toolbar's button index includes the + button at position 0; adjust.
    if index == 0 { return; } // safety: + button never reaches here (is_add branch)
    let folder_index = index - 1;
    cfg.remove_folder(folder_index);
    if let Err(e) = cfg.save() {
        crate::log::error(&format!("remove_folder_at: save failed: {e}"));
        return;
    }
    unsafe { let _ = PostMessageW(Some(hwnd), WM_USER_RELOAD, WPARAM(0), LPARAM(0)); }
}

// ── Inline rename ───────────────────────────────────────────────────────────

/// Global: HWND of the active rename edit control, and the folder index it is editing.
static RENAME_STATE: std::sync::Mutex<Option<RenameState>> = std::sync::Mutex::new(None);

struct RenameState {
    edit_hwnd: isize,
    /// Raw `Box<RenameSubclassData>` pointer handed to `SetWindowSubclass`.
    /// Stored so `cancel_inline_rename` can reclaim the Box on parent teardown.
    /// `folder_index` and `toolbar_hwnd` live in the Box; the subclass proc
    /// reads them from `ref_data`.
    subclass_data: usize,
}

fn start_inline_rename(toolbar: HWND, button_rect: RECT, folder_index: usize, initial_name: &str) {
    // Cancel any existing rename first.
    cancel_inline_rename();

    let hinstance = unsafe { crate::HMODULE };

    let wide_initial: Vec<u16> = initial_name.encode_utf16().chain(std::iter::once(0)).collect();

    // ES_AUTOHSCROLL = 0x0080
    const ES_AUTOHSCROLL: u32 = 0x0080;
    let style = (WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | ES_AUTOHSCROLL) as u32;

    let edit = unsafe {
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            WC_EDITW,
            PCWSTR(wide_initial.as_ptr()),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(style),
            button_rect.left,
            button_rect.top,
            button_rect.right - button_rect.left,
            button_rect.bottom - button_rect.top,
            Some(toolbar),
            None,
            Some(hinstance),
            None,
        )
    };
    let Ok(edit) = edit else { return; };

    // Select all text
    const EM_SETSEL: u32 = 0x00B1;
    unsafe {
        SendMessageW(
            edit,
            EM_SETSEL,
            Some(WPARAM(0)),
            Some(LPARAM(-1)),
        );
        let _ = SetFocus(Some(edit));
    }

    // Subclass for Enter/Esc/KillFocus.
    let data: *mut RenameSubclassData = Box::into_raw(Box::new(RenameSubclassData {
        toolbar_hwnd: toolbar.0 as isize,
        folder_index,
    }));
    unsafe {
        use windows::Win32::UI::Shell::SetWindowSubclass;
        let _ = SetWindowSubclass(edit, Some(rename_subclass_proc), 1, data as usize);
    }

    *RENAME_STATE.lock().unwrap() = Some(RenameState {
        edit_hwnd: edit.0 as isize,
        subclass_data: data as usize,
    });
}

struct RenameSubclassData {
    toolbar_hwnd: isize,
    folder_index: usize,
}

unsafe extern "system" fn rename_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    ref_data: usize,
) -> LRESULT {
    use windows::Win32::UI::Shell::DefSubclassProc;

    const VK_RETURN: usize = 0x0D;
    const VK_ESCAPE: usize = 0x1B;

    match msg {
        WM_GETDLGCODE => {
            return LRESULT(DLGC_WANTALLKEYS as isize);
        }
        WM_KEYDOWN => {
            let vk = wparam.0 as usize;
            if vk == VK_RETURN {
                commit_rename(hwnd, ref_data);
                return LRESULT(0);
            }
            if vk == VK_ESCAPE {
                cancel_rename(hwnd, ref_data);
                return LRESULT(0);
            }
        }
        WM_KILLFOCUS => {
            commit_rename(hwnd, ref_data);
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

fn read_edit_text(edit: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(edit) } as usize;
    let mut buf = vec![0u16; len + 1];
    let got = unsafe { GetWindowTextW(edit, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..got])
}

fn commit_rename(edit: HWND, ref_data: usize) {
    let data = unsafe { Box::from_raw(ref_data as *mut RenameSubclassData) };
    let toolbar = HWND(data.toolbar_hwnd as *mut _);
    let text = read_edit_text(edit);

    if let Some(mut cfg) = crate::config::Config::load() {
        cfg.rename_folder(data.folder_index, text);
        let _ = cfg.save();
    }

    destroy_rename_edit(edit);
    *RENAME_STATE.lock().unwrap() = None;
    unsafe {
        let _ = PostMessageW(Some(toolbar), WM_USER_RELOAD, WPARAM(0), LPARAM(0));
    }
}

fn cancel_rename(edit: HWND, ref_data: usize) {
    let data = unsafe { Box::from_raw(ref_data as *mut RenameSubclassData) };
    let _ = data;
    destroy_rename_edit(edit);
    *RENAME_STATE.lock().unwrap() = None;
}

fn destroy_rename_edit(edit: HWND) {
    use windows::Win32::UI::Shell::RemoveWindowSubclass;
    unsafe {
        let _ = RemoveWindowSubclass(edit, Some(rename_subclass_proc), 1);
        let _ = DestroyWindow(edit);
    }
}

fn cancel_inline_rename() {
    let state = RENAME_STATE.lock().unwrap().take();
    if let Some(s) = state {
        let edit = HWND(s.edit_hwnd as *mut _);
        destroy_rename_edit(edit);
        // Reclaim the Box leaked into SetWindowSubclass; RemoveWindowSubclass
        // inside destroy_rename_edit ran before this, so no callback can race.
        if s.subclass_data != 0 {
            unsafe {
                drop(Box::from_raw(s.subclass_data as *mut RenameSubclassData));
            }
        }
    }
}
