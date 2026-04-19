//! Submenu popup HWND lifecycle.
//!
//! Each open submenu level is its own top-level `WS_POPUP | WS_EX_TOOLWINDOW
//! | WS_EX_LAYERED | WS_EX_NOACTIVATE` window. A single shared window class
//! (`ExbarSubmenuPopup`) is registered once per process; per-popup state
//! lives in `GWLP_USERDATA` as a `Box<SubmenuPopup>`.
//!
//! `IDropTarget` registration happens in Task 10; this task creates and
//! destroys popup windows cleanly with a stub paint.

use std::path::PathBuf;
use std::sync::Once;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_BTNFACE, CreateSolidBrush, DeleteObject, EndPaint, FillRect,
    PAINTSTRUCT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow,
    GWLP_USERDATA, GetClientRect, GetWindowLongPtrW, IDC_ARROW, LWA_ALPHA, LoadCursorW,
    RegisterClassExW, SW_SHOWNOACTIVATE, SetLayeredWindowAttributes, SetWindowLongPtrW, ShowWindow,
    WM_DESTROY, WM_NCCREATE, WM_PAINT, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows_core::PCWSTR;

use crate::layout::SubmenuLayout;
use crate::lifecycle;
use crate::submenu::DisplayItem;

// ── Constants ────────────────────────────────────────────────────────────────

const CLASS_NAME: &str = "ExbarSubmenuPopup";
static CLASS_REGISTERED: Once = Once::new();

// ── Per-popup state ───────────────────────────────────────────────────────────

/// Per-popup state stored in `GWLP_USERDATA`.
///
/// Owned by the HWND — created via `Box::into_raw` in [`create_popup`] and
/// dropped either by [`destroy_popup`] (normal path) or by `WM_DESTROY`
/// as a last-resort leak guard.
pub struct SubmenuPopup {
    /// Nesting depth: 1 = root (opened from a toolbar button), 2–5 = nested.
    pub level: u8,
    /// Absolute path of the folder whose contents this popup displays.
    pub folder_path: PathBuf,
    /// Rendered item list (built by `submenu::build_display_list`).
    pub display_items: Vec<DisplayItem>,
    /// Pixel layout: item rects, overall popup width/height, buffer width.
    pub layout: SubmenuLayout,
    /// Highlighted row index, if any.
    pub highlighted_index: Option<usize>,
    /// Layered-window alpha fraction for non-chain-top levels (0.0–1.0).
    pub non_chain_opacity: f32,
    /// DPI of the monitor the popup is on.
    pub dpi: u32,
    /// HWND of the main toolbar that owns this popup chain (for message routing).
    pub toolbar_hwnd: HWND,
}

// ── Class registration ────────────────────────────────────────────────────────

/// Register the `ExbarSubmenuPopup` window class exactly once per process.
///
/// Subsequent calls are idempotent — `RegisterClassExW` with a duplicate name
/// returns 0 and sets last-error to `ERROR_CLASS_ALREADY_EXISTS` (0x582), which
/// the `Once` guard prevents from even being reached.
fn ensure_class_registered() {
    CLASS_REGISTERED.call_once(|| {
        let class_wide = wide_null(CLASS_NAME);
        // hCursor must be set so DefWindowProc handles WM_SETCURSOR cleanly.
        // Without it, the cursor "disappears" after a child control releases
        // the cursor — see CLAUDE.md gotcha "hCursor on the toolbar window class".
        let hcursor = unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() };
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(submenu_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: lifecycle::exe_hinstance(),
            hCursor: hcursor,
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(std::ptr::null_mut()),
            lpszClassName: PCWSTR(class_wide.as_ptr()),
            ..Default::default()
        };
        // Return value is ignored: failure (duplicate class) is acceptable
        // here because Once guarantees single-call; leave silently ignored.
        unsafe { RegisterClassExW(&wc) };
    });
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Create and show a popup window for one submenu level.
///
/// Ownership of `popup` is transferred to Win32's `GWLP_USERDATA` slot.
/// If window creation fails, the Box is reclaimed and `HWND(null)` is returned.
///
/// # Arguments
/// - `toolbar_hwnd` — the main toolbar window (for message routing).
/// - `popup` — fully initialised [`SubmenuPopup`] describing this level.
/// - `screen_x`, `screen_y` — top-left corner of the popup in screen coords.
pub fn create_popup(
    _toolbar_hwnd: HWND,
    popup: Box<SubmenuPopup>,
    screen_x: i32,
    screen_y: i32,
) -> HWND {
    ensure_class_registered();

    let w = popup.layout.popup_w;
    let h = popup.layout.popup_h;
    let alpha = (popup.non_chain_opacity.clamp(0.0, 1.0) * 255.0) as u8;

    // SAFETY: Box::into_raw transfers ownership to the CreateWindowExW
    // lpCreateParams slot, which Win32 delivers to WM_NCCREATE as
    // cs.lpCreateParams. If window creation fails the Err branch below
    // reclaims the box via Box::from_raw.
    let boxed_ptr = Box::into_raw(popup);

    let class_wide = wide_null(CLASS_NAME);

    let hwnd_result = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            PCWSTR(class_wide.as_ptr()),
            PCWSTR::null(),
            WS_POPUP,
            screen_x,
            screen_y,
            w,
            h,
            None, // no owner — independent top-level popup
            None,
            Some(lifecycle::exe_hinstance()),
            Some(boxed_ptr as *const std::ffi::c_void),
        )
    };

    match hwnd_result {
        Ok(hwnd) => {
            // Apply layered-window alpha using the opacity field.
            unsafe {
                crate::warn_on_err!(SetLayeredWindowAttributes(
                    hwnd,
                    windows::Win32::Foundation::COLORREF(0),
                    alpha,
                    LWA_ALPHA
                ));
            }
            unsafe {
                crate::warn_on_err!(ShowWindow(hwnd, SW_SHOWNOACTIVATE).ok());
            }
            hwnd
        }
        Err(e) => {
            log::error!("create_popup: CreateWindowExW failed: {e}");
            // SAFETY: CreateWindowExW never called WM_NCCREATE (window failed to
            // be created) so the pointer was not handed off to the window; we
            // reclaim it here to avoid a leak.
            drop(unsafe { Box::from_raw(boxed_ptr) });
            HWND(std::ptr::null_mut())
        }
    }
}

/// Destroy a popup window, dropping its [`SubmenuPopup`] state.
///
/// Must be called on the message-pump thread. After this call the HWND is
/// invalid.
pub fn destroy_popup(hwnd: HWND) {
    if hwnd.0.is_null() {
        return;
    }

    // TODO(task-10): RevokeDragDrop here

    // Drop the Box before DestroyWindow so that state is freed while the
    // HWND is still technically alive (avoids any re-entrant WM_DESTROY
    // path trying to double-free the same pointer).
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut SubmenuPopup;
    if !ptr.is_null() {
        // Zero the slot first to prevent double-drop if WM_DESTROY re-fires.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
        // SAFETY: ptr was set from Box::into_raw in WM_NCCREATE; Box::from_raw
        // reclaims it so Drop runs and the SubmenuPopup is freed.
        drop(unsafe { Box::from_raw(ptr) });
    }

    unsafe {
        crate::warn_on_err!(DestroyWindow(hwnd));
    }
}

/// Recover a `&mut SubmenuPopup` from a popup HWND.
///
/// Returns `None` if the slot is 0 (window not yet initialised or already
/// destroyed).
///
/// # Safety
///
/// - `hwnd` must be a submenu popup window (one whose `GWLP_USERDATA` was set
///   by [`submenu_wndproc`] during `WM_NCCREATE`).
/// - Caller must be on the message-pump thread — Win32's single-threaded
///   message dispatch is the synchronisation boundary; no lock is taken.
#[allow(dead_code)] // Task 10: used by paint_submenu_popup when real paint impl arrives
pub(crate) unsafe fn popup_state<'a>(hwnd: HWND) -> Option<&'a mut SubmenuPopup> {
    // SAFETY: GetWindowLongPtrW returns the value written by SetWindowLongPtrW
    // in WM_NCCREATE; we stored a Box::into_raw pointer there.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut SubmenuPopup;
    if ptr.is_null() {
        return None;
    }
    // SAFETY: ptr is non-null and valid; caller is on the message-pump thread
    // (contract above) so no aliasing is possible.
    Some(unsafe { &mut *ptr })
}

// ── Paint helpers ─────────────────────────────────────────────────────────────

/// Stub fill — paints the popup client area with the system button-face color
/// (light mode) or a dark neutral (dark mode) so the window renders visibly
/// during the Task 9 scaffold. Replaced by `paint::paint_submenu_popup` in
/// Task 10.
unsafe fn stub_fill_rect(hwnd: HWND, hdc: windows::Win32::Graphics::Gdi::HDC) {
    let mut client_rect = windows::Win32::Foundation::RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client_rect) };

    let bg_color = if crate::theme::is_dark_mode() {
        windows::Win32::Foundation::COLORREF(0x00_2B_2B_2B)
    } else {
        windows::Win32::Foundation::COLORREF(unsafe {
            windows::Win32::Graphics::Gdi::GetSysColor(COLOR_BTNFACE)
        })
    };
    let bg_brush = unsafe { CreateSolidBrush(bg_color) };
    unsafe { FillRect(hdc, &client_rect, bg_brush) };
    unsafe {
        let _ = DeleteObject(bg_brush.into());
    }
}

// ── Window procedure ──────────────────────────────────────────────────────────

/// Window procedure for `ExbarSubmenuPopup` windows.
///
/// # Safety
///
/// Must only be installed as a `WNDCLASSEXW.lpfnWndProc` and called by Win32's
/// message dispatch — do not invoke directly. All mutable state access is via
/// [`popup_state`].
unsafe extern "system" fn submenu_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            // SAFETY: Win32 guarantees lparam is a valid CREATESTRUCTW pointer
            // during WM_NCCREATE; lpCreateParams is the value passed to
            // CreateWindowExW.
            let cs = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
            let popup_ptr = cs.lpCreateParams as *mut SubmenuPopup;
            // SAFETY: Box::into_raw transferred ownership to the lpCreateParams
            // slot in create_popup; we store the pointer here. The matching
            // Box::from_raw runs in destroy_popup (or WM_DESTROY below).
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, popup_ptr as isize) };
            // Return TRUE to allow window creation to proceed.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };

            // Task 9: stub FillRect only. Task 10 replaces this call (not adds alongside!)
            // with paint::paint_submenu_popup(hdc, &popup.layout, ...) once that fn has a real body.
            unsafe { stub_fill_rect(hwnd, hdc) };

            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }

        WM_DESTROY => {
            // Normal path: destroy_popup drops the Box before calling
            // DestroyWindow, so GWLP_USERDATA is already 0 here.
            //
            // Edge-case guard: if WM_DESTROY somehow arrives without
            // destroy_popup having run (e.g. the OS forcibly closes the
            // window), free the Box here to avoid a leak.
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut SubmenuPopup;
            if !ptr.is_null() {
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
                drop(unsafe { Box::from_raw(ptr) });
            }
            LRESULT(0)
        }

        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Encode `s` as a null-terminated UTF-16 vector suitable for `PCWSTR(v.as_ptr())`.
fn wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
