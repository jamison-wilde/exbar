//! The Win32 window procedure for the toolbar window. Pure dispatch:
//! translates `WM_*` messages to `PointerEvent`s / `RenameEvent`s
//! and calls the adapter methods on `ToolbarState`. All business
//! logic lives in pure controller modules (`pointer`, `rename`) or
//! in sibling Win32 modules (`paint`, `actions`, `rename_edit`,
//! `lifecycle`).

use std::panic::AssertUnwindSafe;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, DEFAULT_GUI_FONT, EndPaint, GetDC, GetStockObject, InvalidateRect,
    PAINTSTRUCT, ReleaseDC, ScreenToClient, SelectObject,
};
use windows::Win32::System::SystemServices::MK_CONTROL;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, DefWindowProcW, GWLP_USERDATA, GetForegroundWindow, GetWindowLongPtrW,
    HTCAPTION, KillTimer, PostMessageW, SW_HIDE, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, WM_CAPTURECHANGED, WM_CREATE, WM_DESTROY, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOVE, WM_NCHITTEST, WM_PAINT, WM_RBUTTONUP, WM_TIMER,
};

use crate::hit_test;
use crate::layout;
use crate::pointer;
use crate::theme;
use crate::toolbar::{GRIP_SIZE, ToolbarState, WM_USER_RELOAD, toolbar_state};

const WM_DPICHANGED: u32 = 0x02E0;
const REORDER_THRESHOLD: i32 = 5;

const MENU_ID_EDIT_CONFIG: u32 = 101;
const MENU_ID_RELOAD_CONFIG: u32 = 102;
const MENU_ID_OPEN: u32 = 201;
const MENU_ID_OPEN_NEW_TAB: u32 = 202;
const MENU_ID_COPY_PATH: u32 = 203;
const MENU_ID_RENAME: u32 = 204;
const MENU_ID_REMOVE: u32 = 205;

/// Extract `(x, y)` from a WM_* LPARAM whose layout is
/// `(y << 16) | (x & 0xFFFF)` with signed 16-bit components.
fn lparam_point(lparam: LPARAM) -> (i32, i32) {
    let x = (lparam.0 & 0xFFFF) as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
    (x, y)
}

/// Toolbar window procedure. Registered as a WNDPROC via
/// `RegisterClassW`; Win32 dispatches here from the message pump.
///
/// # Safety
///
/// Must be installed as the class wndproc via `RegisterClassW` and
/// invoked by Win32's message dispatch — do not call directly. All
/// mutable state access is routed through `toolbar_state(hwnd)`.
unsafe fn toolbar_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            // SAFETY: Win32 guarantees lparam is a valid CREATESTRUCTW pointer
            // during WM_CREATE; lpCreateParams is the value passed to CreateWindowExW.
            let cs = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
            let state_ptr = cs.lpCreateParams as *mut ToolbarState;
            // SAFETY: Box::into_raw transfers ownership to Win32's user-data slot.
            // The matching Box::from_raw in WM_DESTROY reclaims ownership.
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize) };

            // SAFETY: state_ptr was just set from a valid Box::into_raw in create_toolbar;
            // we are on the message-pump thread during WM_CREATE so no aliasing is possible.
            let state = unsafe { &mut *state_ptr };
            let hdc = unsafe { GetDC(Some(hwnd)) };
            let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
            let old_font = unsafe { SelectObject(hdc, font) };
            let (w, h) = crate::paint::compute_layout(hdc, state);
            unsafe {
                SelectObject(hdc, old_font);
                let _ = ReleaseDC(Some(hwnd), hdc);
            }

            // Now that we know the real size, re-clamp position to fit entirely
            // within the current monitor's work area (the Explorer's monitor).
            let mut current_rect = RECT::default();
            unsafe {
                let _ =
                    windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut current_rect);
            }
            let (final_x, final_y) = crate::position::clamp_to_work_area_for(
                current_rect.left,
                current_rect.top,
                w,
                h,
                Some(hwnd),
            );

            unsafe {
                crate::warn_on_err!(SetWindowPos(
                    hwnd,
                    None,
                    final_x,
                    final_y,
                    w,
                    h,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                ));
            }

            // Apply layered window transparency and register drop target.
            crate::lifecycle::setup_on_create(hwnd, state);

            // active_target is seeded in create_toolbar before Box::into_raw,
            // so it's always Some here. Fall back to GetForegroundWindow() only
            // as defence-in-depth in case that invariant is ever broken.
            let explorer_hwnd = state
                .active_target
                .map(|t| t.hwnd)
                .unwrap_or_else(|| unsafe { GetForegroundWindow() });
            let class = crate::explorer::get_class_name(explorer_hwnd);
            if class == "CabinetWClass" {
                log::info!("toolbar create: showing above explorer={explorer_hwnd:?}");
                crate::visibility::show_above(hwnd, explorer_hwnd);
            } else {
                log::info!("toolbar create: fg class={class}, hiding");
                unsafe {
                    crate::warn_on_err!(ShowWindow(hwnd, SW_HIDE).ok());
                }
            }

            LRESULT(0)
        }

        WM_DESTROY => {
            log::info!("toolbar: WM_DESTROY — exiting process");
            crate::visibility::clear_global_toolbar();
            let _ = crate::dragdrop::unregister_drop_target(hwnd);
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ToolbarState;
            if !ptr.is_null() {
                // Cancel any active inline rename before freeing state.
                // SAFETY: ptr is non-null and state is still live at this point;
                // we zero the USERDATA slot and drop state below.
                crate::toolbar::cancel_inline_rename(unsafe { &mut *ptr }, hwnd);
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
                // SAFETY: The pointer was produced by Box::into_raw in WM_CREATE;
                // Box::from_raw reclaims it so the Drop runs and state is freed.
                // The slot is zeroed first to prevent double-free if WM_DESTROY fires again.
                drop(unsafe { Box::from_raw(ptr) });
            }
            // Tell the message loop to exit — the toolbar is the only
            // reason exbar.exe runs, so its destruction should end the
            // process. This lets `taskkill /im exbar.exe` (polite) AND
            // the MSI's util:CloseApplication actually terminate us
            // cleanly instead of waiting for the force-terminate timeout.
            //
            // WS_EX_NOACTIVATE means Alt+F4 can never target our window,
            // and we're in our own process so Explorer's taskbar can't
            // touch us — so the only paths into WM_DESTROY are our own
            // cleanup code and legitimate close requests.
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0);
            }
            LRESULT(0)
        }

        WM_NCHITTEST => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;

            let mut pt = POINT { x, y };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut pt);
            }

            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                if crate::paint::in_grip(state, pt.x, pt.y) {
                    return LRESULT(HTCAPTION as isize);
                }
                if hit_test::hit_test(&state.buttons, pt.x, pt.y).is_some() {
                    return LRESULT(1); // HTCLIENT
                }
            }
            LRESULT(HTCAPTION as isize)
        }

        WM_PAINT => {
            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                unsafe {
                    crate::paint::paint(hwnd, state);
                }
            } else {
                let mut ps = PAINTSTRUCT::default();
                unsafe {
                    BeginPaint(hwnd, &mut ps);
                }
                unsafe {
                    let _ = EndPaint(hwnd, &ps);
                }
            }
            LRESULT(0)
        }

        WM_MOVE => {
            let (x, y) = lparam_point(lparam);
            if let Some(state) = unsafe { toolbar_state(hwnd) }
                && let Some(explorer) = state.active_target.map(|t| t.hwnd)
            {
                let (ox, oy) = crate::position::explorer_visible_origin(explorer);
                let (off_x, off_y) = crate::position::compute_offset(x, y, ox, oy);
                let kind = state
                    .active_target
                    .map(|t| t.kind)
                    .unwrap_or(crate::target::TargetKind::Explorer);
                crate::position::save_offset(kind, off_x, off_y);
            }
            LRESULT(0)
        }

        WM_MOUSEMOVE => {
            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                let (x, y) = lparam_point(lparam);

                let hit = hit_test::hit_test(&state.buttons, x, y).map(|idx| pointer::HitResult {
                    button: idx,
                    is_folder: !state.buttons[idx].is_add,
                });
                let reorder_threshold_px = theme::scale(REORDER_THRESHOLD, state.dpi);
                let insertion_if_reordering =
                    layout::compute_insertion_index(&layout::InsertionInput {
                        buttons: &state.buttons,
                        orientation: state.layout,
                        cursor_x: x,
                        cursor_y: y,
                    });

                state.apply_pointer_event(
                    hwnd,
                    pointer::PointerEvent::Move {
                        x,
                        y,
                        hit,
                        reorder_threshold_px,
                        insertion_if_reordering,
                    },
                );
            }
            LRESULT(0)
        }

        x if x == WM_MOUSELEAVE => {
            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                state.mouse_tracking_started = false; // next hover will need to re-arm.
                state.apply_pointer_event(hwnd, pointer::PointerEvent::Leave);
            }
            LRESULT(0)
        }

        x if x == WM_CAPTURECHANGED => {
            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                if state.self_release_pending {
                    // Our own ReleaseCapture() dispatched this; consume the flag.
                    state.self_release_pending = false;
                } else {
                    // External capture loss — feed to machine.
                    state.apply_pointer_event(hwnd, pointer::PointerEvent::CaptureLost);
                }
            }
            LRESULT(0)
        }

        WM_LBUTTONDOWN => {
            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                let (x, y) = lparam_point(lparam);
                let hit = hit_test::hit_test(&state.buttons, x, y).map(|idx| pointer::HitResult {
                    button: idx,
                    is_folder: !state.buttons[idx].is_add,
                });
                state.apply_pointer_event(hwnd, pointer::PointerEvent::Press { x, y, hit });
            }
            LRESULT(0)
        }

        WM_LBUTTONUP => {
            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                let (x, y) = lparam_point(lparam);
                let hit = hit_test::hit_test(&state.buttons, x, y).map(|idx| pointer::HitResult {
                    button: idx,
                    is_folder: !state.buttons[idx].is_add,
                });
                let ctrl = (wparam.0 & MK_CONTROL.0 as usize) != 0;
                state.apply_pointer_event(hwnd, pointer::PointerEvent::Release { x, y, hit, ctrl });
            }
            LRESULT(0)
        }

        WM_RBUTTONUP => {
            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                let (x, y) = lparam_point(lparam);
                if let Some(idx) = hit_test::hit_test(&state.buttons, x, y) {
                    let mut pt = POINT { x, y };
                    unsafe {
                        let _ = ClientToScreen(hwnd, &mut pt);
                    }
                    if state.buttons[idx].is_add {
                        let items = [
                            crate::contextmenu::MenuItem {
                                id: MENU_ID_EDIT_CONFIG,
                                label: "Edit config",
                            },
                            crate::contextmenu::MenuItem {
                                id: MENU_ID_RELOAD_CONFIG,
                                label: "Reload config",
                            },
                        ];
                        let chosen = crate::contextmenu::show_menu(hwnd, pt, &items);
                        match chosen {
                            MENU_ID_EDIT_CONFIG => crate::actions::open_config_in_editor(),
                            MENU_ID_RELOAD_CONFIG => unsafe {
                                let _ =
                                    PostMessageW(Some(hwnd), WM_USER_RELOAD, WPARAM(0), LPARAM(0));
                            },
                            _ => {}
                        }
                    } else {
                        let items = [
                            crate::contextmenu::MenuItem {
                                id: MENU_ID_OPEN,
                                label: "Open",
                            },
                            crate::contextmenu::MenuItem {
                                id: MENU_ID_OPEN_NEW_TAB,
                                label: "Open in new tab",
                            },
                            crate::contextmenu::MenuItem {
                                id: MENU_ID_COPY_PATH,
                                label: "Copy path",
                            },
                            crate::contextmenu::SEPARATOR,
                            crate::contextmenu::MenuItem {
                                id: MENU_ID_RENAME,
                                label: "Rename",
                            },
                            crate::contextmenu::MenuItem {
                                id: MENU_ID_REMOVE,
                                label: "Remove",
                            },
                        ];
                        let chosen = crate::contextmenu::show_menu(hwnd, pt, &items);
                        let path = std::path::PathBuf::from(&state.buttons[idx].folder.path);
                        match chosen {
                            MENU_ID_OPEN => match state.active_target.map(|t| t.kind) {
                                Some(crate::target::TargetKind::FileDialog) => {
                                    state.shell_browser.open_in_new_window(&path);
                                }
                                Some(crate::target::TargetKind::Explorer) => {
                                    if let Some(explorer) = state.active_target.map(|t| t.hwnd) {
                                        crate::warn_on_err!(
                                            state.shell_browser.navigate(explorer, &path)
                                        );
                                    }
                                }
                                None => {}
                            },
                            MENU_ID_OPEN_NEW_TAB => match state.active_target.map(|t| t.kind) {
                                Some(crate::target::TargetKind::FileDialog) => {
                                    state.shell_browser.open_in_new_window(&path);
                                }
                                Some(crate::target::TargetKind::Explorer) => {
                                    let timeout = state
                                        .config
                                        .as_ref()
                                        .map(|c| c.new_tab_timeout_ms_zero_disables)
                                        .unwrap_or(500);
                                    if let Some(explorer) = state.active_target.map(|t| t.hwnd) {
                                        state
                                            .shell_browser
                                            .open_in_new_tab(explorer, &path, timeout);
                                    }
                                }
                                None => {}
                            },
                            MENU_ID_COPY_PATH => {
                                let folder_button = idx - 1; // + button at index 0
                                crate::actions::copy_folder_path_to_clipboard(state, folder_button);
                            }
                            MENU_ID_RENAME => {
                                let rect = crate::paint::rect_to_win32(state.buttons[idx].rect);
                                let name = state.buttons[idx].folder.name.clone();
                                let folder_index = idx - 1; // + button at index 0
                                crate::rename_edit::start_inline_rename(
                                    hwnd,
                                    rect,
                                    folder_index,
                                    &name,
                                );
                            }
                            MENU_ID_REMOVE => {
                                crate::actions::remove_folder_at(state, hwnd, idx);
                            }
                            _ => {}
                        }
                    }
                }
            }
            LRESULT(0)
        }

        x if x == WM_USER_RELOAD => {
            crate::lifecycle::refresh_toolbar(hwnd);
            LRESULT(0)
        }

        WM_TIMER if wparam.0 == crate::toolbar::TIMER_REPOSITION => {
            // Deferred reposition after Explorer maximize/restore animation.
            // Kill the timer (one-shot) then reposition.
            unsafe {
                let _ = KillTimer(Some(hwnd), crate::toolbar::TIMER_REPOSITION);
            }
            if let Some(state) = unsafe { toolbar_state(hwnd) }
                && let Some(explorer) = state.active_target.map(|t| t.hwnd)
            {
                log::debug!("TIMER_REPOSITION: repositioning to explorer={explorer:?}");
                crate::visibility::reposition_and_show(hwnd, explorer);
            }
            LRESULT(0)
        }

        x if x == WM_DPICHANGED => {
            let new_dpi = (wparam.0 & 0xFFFF) as u32;
            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                state.dpi = new_dpi;
                state.grip_size = theme::scale(GRIP_SIZE, new_dpi);
                let hdc = unsafe { GetDC(Some(hwnd)) };
                let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
                let old_font = unsafe { SelectObject(hdc, font) };
                let (w, h) = crate::paint::compute_layout(hdc, state);
                unsafe {
                    SelectObject(hdc, old_font);
                    let _ = ReleaseDC(Some(hwnd), hdc);
                }
                unsafe {
                    crate::warn_on_err!(SetWindowPos(
                        hwnd,
                        None,
                        0,
                        0,
                        w,
                        h,
                        SWP_NOZORDER
                            | SWP_NOACTIVATE
                            | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE,
                    ));
                    let _ = InvalidateRect(Some(hwnd), None, true);
                }
            }
            LRESULT(0)
        }

        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub(crate) unsafe extern "system" fn toolbar_wndproc_safe(
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
