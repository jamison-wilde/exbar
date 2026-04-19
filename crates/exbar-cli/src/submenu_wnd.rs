//! Submenu popup HWND lifecycle.
//!
//! Each open submenu level is its own top-level `WS_POPUP | WS_EX_TOOLWINDOW
//! | WS_EX_LAYERED | WS_EX_NOACTIVATE` window. A single shared window class
//! (`ExbarSubmenuPopup`) is registered once per process; per-popup state
//! lives in `GWLP_USERDATA` as a `Box<SubmenuPopup>`.
//!
//! `IDropTarget` registration via [`create_popup`] / [`destroy_popup`] routes
//! drag-hover and drop events to the toolbar HWND as `WM_USER_SUBMENU_HOVER`
//! and `WM_USER_SUBMENU_CLICK`.

use std::path::PathBuf;
use std::sync::Once;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow,
    GWLP_USERDATA, GetWindowLongPtrW, HWND_TOPMOST, IDC_ARROW, LWA_ALPHA, LoadCursorW,
    PostMessageW, RegisterClassExW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_DESTROY,
    WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_PAINT, WNDCLASSEXW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
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
    /// Layered-window alpha fraction (0.0–1.0) — driven by `config.background_opacity`.
    pub layered_alpha: f32,
    /// DPI of the monitor the popup is on.
    pub dpi: u32,
    /// HWND of the main toolbar that owns this popup chain (for message routing).
    pub toolbar_hwnd: HWND,
    /// Set to `true` after `RegisterDragDrop` succeeds. Gates `RevokeDragDrop`
    /// in [`destroy_popup`] so we don't log spurious `DRAGDROP_E_NOTREGISTERED`
    /// warnings in test contexts where OleInitialize was never called.
    pub drop_registered: bool,
    /// Index of the first `display_items` entry that is currently painted.
    /// `0` when all items fit; adjusted by `WM_MOUSEWHEEL`.
    pub scroll_offset: usize,
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
/// - `file_operator` — passed to [`crate::dragdrop::SubmenuDropTarget`] so
///   Task 14 can execute move/copy synchronously inside `IDropTarget::Drop`.
pub fn create_popup(
    toolbar_hwnd: HWND,
    popup: Box<SubmenuPopup>,
    screen_x: i32,
    screen_y: i32,
    file_operator: std::sync::Arc<dyn crate::dragdrop::FileOperator>,
) -> HWND {
    ensure_class_registered();

    let w = popup.layout.popup_w;
    let h = popup.layout.popup_h;
    let alpha = (popup.layered_alpha.clamp(0.0, 1.0) * 255.0) as u8;

    // Capture level before Box::into_raw so we can pass it to
    // SubmenuDropTarget::new after window creation (option A).
    let level_at_creation = popup.level;

    log::info!(
        "submenu: create_popup level={} size={}x{} at screen=({},{})",
        popup.level,
        popup.layout.popup_w,
        popup.layout.popup_h,
        screen_x,
        screen_y
    );

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
            // Promote popup to topmost to avoid being rendered behind Explorer's
            // WinUI 3 XAML content (see CLAUDE.md gotcha about HWND_TOPMOST).
            // SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE ensures the popup keeps
            // its requested position and size without affecting input focus.
            unsafe {
                crate::warn_on_err!(SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                ));
            }
            // Register the OLE drop target so drag-hover events can reach the
            // toolbar's wndproc via WM_USER_SUBMENU_HOVER. RegisterDragDrop
            // requires OleInitialize (see CLAUDE.md gotcha) — satisfied once in
            // run_hook before the message pump starts.
            let drop_target: windows::Win32::System::Ole::IDropTarget =
                crate::dragdrop::SubmenuDropTarget::new(
                    toolbar_hwnd,
                    hwnd,
                    level_at_creation,
                    file_operator,
                )
                .into();
            unsafe {
                match windows::Win32::System::Ole::RegisterDragDrop(hwnd, &drop_target) {
                    Ok(()) => {
                        if let Some(popup) = popup_state(hwnd) {
                            popup.drop_registered = true;
                        }
                    }
                    Err(e) => {
                        log::warn!("submenu: RegisterDragDrop failed for hwnd={hwnd:?}: {e:?}");
                    }
                }
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

    // Revoke before destroying the window. Only call RevokeDragDrop when we
    // know registration succeeded (tracked by drop_registered) to avoid
    // DRAGDROP_E_NOTREGISTERED noise in tests where OleInitialize was never
    // called.
    unsafe {
        let should_revoke = popup_state(hwnd)
            .map(|p| p.drop_registered)
            .unwrap_or(false);
        if should_revoke && let Err(e) = windows::Win32::System::Ole::RevokeDragDrop(hwnd) {
            log::warn!("submenu: RevokeDragDrop failed for hwnd={hwnd:?}: {e:?}");
        }
    }

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
            if let Some(popup) = unsafe { popup_state(hwnd) } {
                crate::paint::paint_submenu_popup(
                    hdc,
                    &popup.layout,
                    &popup.display_items,
                    popup.highlighted_index,
                    popup.dpi,
                    popup.level,
                    popup.scroll_offset,
                );
            }
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }

        WM_MOUSEMOVE => {
            let Some(popup) = (unsafe { popup_state(hwnd) }) else {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            };
            // Extract client coords from lparam: LOWORD = x, HIWORD = y,
            // both sign-extended from i16 to handle negative coords correctly.
            let x = (lparam.0 as i16) as i32;
            let y = ((lparam.0 >> 16) as i16) as i32;
            let item_idx = hit_test_inner(&popup.layout, x, y, popup.scroll_offset);
            // Pack level into the high 16 bits of wparam; item index into lparam.
            let wparam_level = WPARAM((popup.level as usize) << 16);
            let toolbar_hwnd = popup.toolbar_hwnd;
            unsafe {
                let _ = PostMessageW(
                    Some(toolbar_hwnd),
                    crate::wndproc::WM_USER_SUBMENU_HOVER,
                    wparam_level,
                    LPARAM(item_idx),
                );
            }
            LRESULT(0)
        }

        WM_LBUTTONUP => {
            let Some(popup) = (unsafe { popup_state(hwnd) }) else {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            };
            let x = (lparam.0 as i16) as i32;
            let y = ((lparam.0 >> 16) as i16) as i32;
            let item_idx = hit_test_inner(&popup.layout, x, y, popup.scroll_offset);
            if item_idx < 0 {
                // Click landed in the translucent buffer band (not on any item). Treat
                // as a dismissal signal — user clicked "near but not on" any folder.
                let Some(popup) = (unsafe { popup_state(hwnd) }) else {
                    return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
                };
                unsafe {
                    let _ = PostMessageW(
                        Some(popup.toolbar_hwnd),
                        crate::wndproc::WM_USER_SUBMENU_DISMISS,
                        windows::Win32::Foundation::WPARAM(0),
                        windows::Win32::Foundation::LPARAM(0),
                    );
                }
                return windows::Win32::Foundation::LRESULT(0);
            }
            // Ctrl detection: bit 15 of GetKeyState is 1 when the key is down.
            let ctrl = unsafe { (GetKeyState(VK_CONTROL.0 as i32) as u16) & 0x8000 != 0 };
            let level = popup.level;
            let toolbar_hwnd = popup.toolbar_hwnd;
            let wparam_encoded = ((level as usize) << 16) | if ctrl { 1 } else { 0 };
            unsafe {
                let _ = PostMessageW(
                    Some(toolbar_hwnd),
                    crate::wndproc::WM_USER_SUBMENU_CLICK,
                    windows::Win32::Foundation::WPARAM(wparam_encoded),
                    windows::Win32::Foundation::LPARAM(item_idx),
                );
            }
            LRESULT(0)
        }

        WM_MOUSEWHEEL => {
            let Some(popup) = (unsafe { popup_state(hwnd) }) else {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            };
            let total = popup.display_items.len();
            let visible = popup.layout.visible_count;
            if total <= visible {
                return windows::Win32::Foundation::LRESULT(0);
            }
            // wparam high-word = wheel delta (signed i16).
            // Positive delta = wheel rolled away from user = scroll content up (decrease offset).
            let delta = (((wparam.0 >> 16) as i16) as i32) / 120; // WHEEL_DELTA = 120 per notch
            let max_offset = total - visible;
            let new_offset = if delta > 0 {
                popup.scroll_offset.saturating_sub(delta as usize)
            } else {
                (popup.scroll_offset + (-delta) as usize).min(max_offset)
            };
            if new_offset != popup.scroll_offset {
                popup.scroll_offset = new_offset;
                unsafe {
                    let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
                }
            }
            windows::Win32::Foundation::LRESULT(0)
        }

        WM_KEYDOWN => {
            // Best-effort: popups use WS_EX_NOACTIVATE so WM_KEYDOWN may never arrive
            // here in practice. Primary dismissal mechanisms are click-outside (Task 16)
            // and cursor-exit safety timer (Task 11).
            if wparam.0 == VK_ESCAPE.0 as usize {
                let Some(popup) = (unsafe { popup_state(hwnd) }) else {
                    return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
                };
                let toolbar_hwnd = popup.toolbar_hwnd;
                unsafe {
                    let _ = PostMessageW(
                        Some(toolbar_hwnd),
                        crate::wndproc::WM_USER_SUBMENU_DISMISS,
                        windows::Win32::Foundation::WPARAM(0),
                        windows::Win32::Foundation::LPARAM(0),
                    );
                }
                return LRESULT(0);
            }
            // Fall through for other keys.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
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

/// Return the logical `display_items` index containing `(x, y)` in client
/// coords, or `-1` if the point is outside all item rects (e.g., inside the
/// buffer band). The returned index accounts for `scroll_offset` so callers
/// can index directly into `display_items`.
fn hit_test_inner(
    layout: &crate::layout::SubmenuLayout,
    x: i32,
    y: i32,
    scroll_offset: usize,
) -> isize {
    for (i, r) in layout.item_rects.iter().enumerate() {
        if x >= r.left && x < r.right && y >= r.top && y < r.bottom {
            return (i + scroll_offset) as isize;
        }
    }
    -1
}

/// Encode `s` as a null-terminated UTF-16 vector suitable for `PCWSTR(v.as_ptr())`.
fn wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
