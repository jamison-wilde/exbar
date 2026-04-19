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
    CREATESTRUCTW, DefWindowProcW, GWLP_USERDATA, GetCursorPos, GetForegroundWindow,
    GetWindowLongPtrW, GetWindowRect, HTCAPTION, KillTimer, PostMessageW, SW_HIDE, SWP_NOACTIVATE,
    SWP_NOZORDER, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_CAPTURECHANGED,
    WM_CREATE, WM_DESTROY, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOVE, WM_NCHITTEST,
    WM_PAINT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER,
};

use crate::hit_test;
use crate::layout;
use crate::pointer;
use crate::theme;
use crate::toolbar::{GRIP_SIZE, ToolbarState, WM_USER_RELOAD, toolbar_state};

const WM_DPICHANGED: u32 = 0x02E0;
const REORDER_THRESHOLD: i32 = 5;

// ── Submenu WM_USER messages ──────────────────────────────────────────────────

/// Posted to the toolbar HWND when the cursor enters a submenu item.
/// `WPARAM` = level (u8), `LPARAM` = item index (usize).
pub const WM_USER_SUBMENU_HOVER: u32 = 0x040A; // WM_USER + 10
/// Posted to the toolbar HWND when the user clicks a submenu item.
/// `WPARAM` = level (u8), `LPARAM` = item index (usize).
pub const WM_USER_SUBMENU_CLICK: u32 = 0x040B; // WM_USER + 11
/// Posted to the toolbar HWND to dismiss the entire submenu chain.
pub const WM_USER_SUBMENU_DISMISS: u32 = 0x040C; // WM_USER + 12
/// Safety timer tick for the submenu dismiss countdown (~30 ms period).
pub const WM_USER_SUBMENU_SAFETY_TICK: u32 = 0x040D; // WM_USER + 13
/// Posted to the toolbar HWND when the cursor enters/leaves a scroll band
/// inside a popup. `WPARAM` = popup HWND as usize; `LPARAM` = direction
/// (-1 = up band, 0 = none/cancel, 1 = down band).
pub const WM_USER_SUBMENU_BANDHOVER: u32 = 0x040F; // WM_USER + 15

const MENU_ID_EDIT_CONFIG: u32 = 101;
const MENU_ID_RELOAD_CONFIG: u32 = 102;
const MENU_ID_TOGGLE_RECENT: u32 = 103;
const MENU_ID_OPEN: u32 = 201;
const MENU_ID_OPEN_NEW_TAB: u32 = 202;
const MENU_ID_COPY_PATH: u32 = 203;
const MENU_ID_RENAME: u32 = 204;
const MENU_ID_REMOVE: u32 = 205;

/// Returns `true` if the given screen-coord cursor is inside any open popup's
/// rendered bounds (the HWND rect already includes the buffer band, so no
/// extra inflation is needed — spec §3.8 says the buffer is the inner padding
/// inside the popup window, not an additional outer halo).
fn cursor_inside_any_padded_popup(state: &crate::toolbar::ToolbarState, cx: i32, cy: i32) -> bool {
    use windows::Win32::Foundation::RECT;

    for &h in state.submenu_popups.iter() {
        if h.0.is_null() {
            continue;
        }
        let mut rect = RECT::default();
        if unsafe { GetWindowRect(h, &mut rect).is_err() } {
            continue;
        }
        if cx >= rect.left && cx < rect.right && cy >= rect.top && cy < rect.bottom {
            return true;
        }
    }
    false
}

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
                // Flush any pending recent-folders write before teardown.
                unsafe { &mut *ptr }.flush_recent(hwnd);
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

                // Hover-open: arm a one-shot timer when the cursor rests on a folder button.
                // If already waiting on the same button, do nothing. If on a different button,
                // reset. If not on any button, cancel.
                match state.pointer {
                    pointer::PointerState::Hovering { button } if button >= 1 => {
                        // button 0 is the '+' button — only fire for folder buttons.
                        if state.hover_open_pending_button != Some(button) {
                            // Kill any previous timer.
                            unsafe {
                                let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(
                                    Some(hwnd),
                                    crate::toolbar::TIMER_HOVER_OPEN,
                                );
                            }
                            state.hover_open_pending_button = Some(button);
                            // Don't arm if a submenu chain is already open (avoid re-opening on hover drift).
                            if !state.submenu_chain.is_open() {
                                let delay = state.submenu_cfg.long_hover_open_ms;
                                unsafe {
                                    let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(
                                        Some(hwnd),
                                        crate::toolbar::TIMER_HOVER_OPEN,
                                        delay,
                                        None,
                                    );
                                }
                            }
                        }
                    }
                    _ => {
                        // Cursor no longer on a folder button — cancel pending hover-open.
                        if state.hover_open_pending_button.is_some() {
                            unsafe {
                                let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(
                                    Some(hwnd),
                                    crate::toolbar::TIMER_HOVER_OPEN,
                                );
                            }
                            state.hover_open_pending_button = None;
                        }
                    }
                }
            }
            LRESULT(0)
        }

        x if x == WM_MOUSELEAVE => {
            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                state.mouse_tracking_started = false; // next hover will need to re-arm.
                state.apply_pointer_event(hwnd, pointer::PointerEvent::Leave);
                // Cancel pending hover-open.
                if state.hover_open_pending_button.is_some() {
                    unsafe {
                        let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(
                            Some(hwnd),
                            crate::toolbar::TIMER_HOVER_OPEN,
                        );
                    }
                    state.hover_open_pending_button = None;
                }
            }
            LRESULT(0)
        }

        x if x == WM_CAPTURECHANGED => {
            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                if state.self_release_pending {
                    // Our own ReleaseCapture() dispatched this; consume the flag.
                    state.self_release_pending = false;
                } else {
                    // External capture loss — kill long-press timer and feed to machine.
                    unsafe {
                        let _ = KillTimer(Some(hwnd), crate::toolbar::TIMER_LONGPRESS);
                    }
                    state.last_press_instant = None;
                    state.apply_pointer_event(hwnd, pointer::PointerEvent::CaptureLost);
                }
            }
            LRESULT(0)
        }

        WM_LBUTTONDOWN => {
            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                // If submenus are open, a toolbar click starts a new gesture.
                // Dismiss the current chain before processing the press so the
                // new press-release cycle works cleanly (and a subsequent
                // long-press on the same button re-opens a fresh chain).
                if state.submenu_chain.is_open() {
                    state.execute_submenu_event(hwnd, crate::submenu::SubmenuEvent::Dismiss);
                }

                let (x, y) = lparam_point(lparam);
                let hit = hit_test::hit_test(&state.buttons, x, y).map(|idx| pointer::HitResult {
                    button: idx,
                    is_folder: !state.buttons[idx].is_add,
                });
                state.apply_pointer_event(hwnd, pointer::PointerEvent::Press { x, y, hit });

                // If we landed in PressedFolder, start the long-press detection timer.
                if matches!(state.pointer, pointer::PointerState::PressedFolder { .. }) {
                    state.last_press_instant = Some(std::time::Instant::now());
                    unsafe {
                        let _ = SetTimer(Some(hwnd), crate::toolbar::TIMER_LONGPRESS, 50, None);
                    }
                }
            }
            LRESULT(0)
        }

        WM_LBUTTONUP => {
            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                // Stop the long-press timer — release ends the gesture regardless of outcome.
                unsafe {
                    let _ = KillTimer(Some(hwnd), crate::toolbar::TIMER_LONGPRESS);
                }
                state.last_press_instant = None;

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

        WM_RBUTTONDOWN => {
            // Cancel any pending hover-open timer before the right-click cascades
            // into a context-menu modal loop. Win32 dispatches WM_TIMER inside
            // TrackPopupMenu's modal pump, so without this cancel the hover-open
            // timer would fire and open a submenu on top of the context menu.
            if let Some(state) = unsafe { toolbar_state(hwnd) }
                && state.hover_open_pending_button.is_some()
            {
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(
                        Some(hwnd),
                        crate::toolbar::TIMER_HOVER_OPEN,
                    );
                }
                state.hover_open_pending_button = None;
            }
            LRESULT(0)
        }

        WM_RBUTTONUP => {
            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                // Defense-in-depth: also cancel here in case WM_RBUTTONDOWN was
                // missed (e.g. mouse was captured elsewhere when the button went down).
                if state.hover_open_pending_button.is_some() {
                    unsafe {
                        let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(
                            Some(hwnd),
                            crate::toolbar::TIMER_HOVER_OPEN,
                        );
                    }
                    state.hover_open_pending_button = None;
                }
                let (x, y) = lparam_point(lparam);
                if let Some(idx) = hit_test::hit_test(&state.buttons, x, y) {
                    let mut pt = POINT { x, y };
                    unsafe {
                        let _ = ClientToScreen(hwnd, &mut pt);
                    }
                    if state.buttons[idx].is_add {
                        let recent_enabled = state
                            .config
                            .as_ref()
                            .map(|c| c.recent.enabled)
                            .unwrap_or(false);
                        let toggle_label = if recent_enabled {
                            "Disable Recent Folders"
                        } else {
                            "Enable Recent Folders"
                        };
                        let items = [
                            crate::contextmenu::MenuItem {
                                id: MENU_ID_EDIT_CONFIG,
                                label: "Edit config",
                            },
                            crate::contextmenu::MenuItem {
                                id: MENU_ID_RELOAD_CONFIG,
                                label: "Reload config",
                            },
                            crate::contextmenu::SEPARATOR,
                            crate::contextmenu::MenuItem {
                                id: MENU_ID_TOGGLE_RECENT,
                                label: toggle_label,
                            },
                        ];
                        let chosen = crate::contextmenu::show_menu(hwnd, pt, &items);
                        match chosen {
                            MENU_ID_EDIT_CONFIG => crate::actions::open_config_in_editor(),
                            MENU_ID_RELOAD_CONFIG => unsafe {
                                let _ =
                                    PostMessageW(Some(hwnd), WM_USER_RELOAD, WPARAM(0), LPARAM(0));
                            },
                            MENU_ID_TOGGLE_RECENT => {
                                handle_toggle_recent(state, hwnd);
                            }
                            _ => {}
                        }
                    } else if state.buttons[idx].folder.kind == crate::config::FolderKind::Recent {
                        // Recent button: trimmed menu — only Remove (= disable Recent).
                        let items = [crate::contextmenu::MenuItem {
                            id: MENU_ID_REMOVE,
                            label: "Remove",
                        }];
                        let chosen = crate::contextmenu::show_menu(hwnd, pt, &items);
                        if chosen == MENU_ID_REMOVE {
                            handle_toggle_recent(state, hwnd);
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

        x if x == WM_USER_SUBMENU_CLICK => {
            // WPARAM: high 16 bits = popup level, bit 0 = ctrl held.
            // LPARAM: item index (usize).
            let level = (wparam.0 >> 16) as u8;
            let ctrl = (wparam.0 & 1) != 0;
            let idx = lparam.0 as usize;

            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                // Look up the popup HWND for this level (level is 1-based, vec is 0-based).
                let popup_hwnd = state
                    .submenu_popups
                    .get((level as usize).saturating_sub(1))
                    .copied();

                let display_item = popup_hwnd.and_then(|h| {
                    if h.0.is_null() {
                        return None;
                    }
                    unsafe {
                        crate::submenu_wnd::popup_state(h)
                            .and_then(|p| p.display_items.get(idx).cloned())
                    }
                });

                match display_item {
                    Some(crate::submenu::DisplayItem::Subfolder { entry }) => {
                        // Mark as toolbar-initiated before navigating so dwell
                        // tracker skips our own submenu clicks.
                        state.execute_tracker_event(
                            hwnd,
                            crate::recent_tracker::TrackerEvent::SelfInitiated,
                        );
                        state.navigate_or_new_window_or_tab(&entry.path.to_string_lossy(), ctrl);
                    }
                    Some(crate::submenu::DisplayItem::Dotdot { parent_path, .. }) => {
                        state.execute_tracker_event(
                            hwnd,
                            crate::recent_tracker::TrackerEvent::SelfInitiated,
                        );
                        state.navigate_or_new_window_or_tab(&parent_path.to_string_lossy(), ctrl);
                    }
                    Some(crate::submenu::DisplayItem::ParentReshow { path, .. }) => {
                        state.execute_tracker_event(
                            hwnd,
                            crate::recent_tracker::TrackerEvent::SelfInitiated,
                        );
                        state.navigate_or_new_window_or_tab(&path.to_string_lossy(), ctrl);
                    }
                    _ => {
                        // Ellipsis / Empty / None — no action, but still dismiss.
                    }
                }

                state.execute_submenu_event(hwnd, crate::submenu::SubmenuEvent::Commit);
            }
            LRESULT(0)
        }

        x if x == WM_USER_SUBMENU_DISMISS => {
            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                state.execute_submenu_event(hwnd, crate::submenu::SubmenuEvent::Dismiss);
            }
            LRESULT(0)
        }

        x if x == WM_USER_SUBMENU_HOVER => {
            // WPARAM high 16 bits = popup level (u8); LPARAM = item index (isize, -1 = buffer).
            let level = (wparam.0 >> 16) as u8;
            let idx = lparam.0; // signed; -1 means "no item hit"
            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                if idx < 0 {
                    state.execute_submenu_event(hwnd, crate::submenu::SubmenuEvent::HoverBufferAt);
                } else {
                    // Recover the display item at (level, idx) from the popup's state.
                    let popup_hwnd = state
                        .submenu_popups
                        .get((level as usize).saturating_sub(1))
                        .copied();
                    let display_item = popup_hwnd.and_then(|h| {
                        if h.0.is_null() {
                            return None;
                        }
                        unsafe {
                            crate::submenu_wnd::popup_state(h)
                                .and_then(|p| p.display_items.get(idx as usize).cloned())
                        }
                    });
                    match display_item {
                        Some(crate::submenu::DisplayItem::Subfolder { entry }) => {
                            state.execute_submenu_event(
                                hwnd,
                                crate::submenu::SubmenuEvent::HoverChildItem {
                                    level,
                                    index: idx as usize,
                                    child_path: entry.path,
                                    is_dotdot: false,
                                },
                            );
                        }
                        Some(crate::submenu::DisplayItem::Dotdot { parent_path, .. }) => {
                            state.execute_submenu_event(
                                hwnd,
                                crate::submenu::SubmenuEvent::HoverChildItem {
                                    level,
                                    index: idx as usize,
                                    child_path: parent_path,
                                    is_dotdot: true,
                                },
                            );
                        }
                        // ParentReshow, Ellipsis, Empty → treat as buffer
                        // (highlight stays, dismiss cancelled).
                        _ => {
                            state.execute_submenu_event(
                                hwnd,
                                crate::submenu::SubmenuEvent::HoverBufferAt,
                            );
                        }
                    }
                }
            }
            LRESULT(0)
        }

        x if x == WM_USER_SUBMENU_BANDHOVER => {
            let popup_hwnd = windows::Win32::Foundation::HWND(wparam.0 as *mut _);
            let dir = lparam.0 as i32;
            if let Some(state) = unsafe { toolbar_state(hwnd) } {
                if dir == 0 {
                    // Cancel autoscroll.
                    if state.autoscroll_dir != 0 {
                        unsafe {
                            let _ = KillTimer(Some(hwnd), crate::toolbar::TIMER_SUBMENU_AUTOSCROLL);
                        }
                        state.autoscroll_popup = None;
                        state.autoscroll_dir = 0;
                    }
                } else {
                    // Arm (or re-target) autoscroll.
                    state.autoscroll_popup = Some(popup_hwnd);
                    state.autoscroll_dir = dir;
                    unsafe {
                        let _ = SetTimer(
                            Some(hwnd),
                            crate::toolbar::TIMER_SUBMENU_AUTOSCROLL,
                            150,
                            None,
                        );
                    }
                }
            }
            LRESULT(0)
        }

        WM_TIMER => {
            let timer_id = wparam.0;
            if timer_id == crate::toolbar::TIMER_REPOSITION {
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
            } else if timer_id == crate::toolbar::TIMER_LONGPRESS {
                if let Some(state) = unsafe { toolbar_state(hwnd) } {
                    let elapsed_ms = state
                        .last_press_instant
                        .map(|t| t.elapsed().as_millis() as u32)
                        .unwrap_or(0);

                    state.apply_pointer_event(
                        hwnd,
                        pointer::PointerEvent::LongPressTick { elapsed_ms },
                    );

                    // Stop the timer if long-press has fired OR state left PressedFolder
                    // (e.g. drag-reorder kicked in via Move events).
                    let should_stop = match &state.pointer {
                        pointer::PointerState::PressedFolder {
                            long_press_fired, ..
                        } => *long_press_fired,
                        _ => true,
                    };
                    if should_stop {
                        unsafe {
                            let _ = KillTimer(Some(hwnd), crate::toolbar::TIMER_LONGPRESS);
                        }
                        state.last_press_instant = None;
                    }
                }
                LRESULT(0)
            } else if timer_id == crate::toolbar::TIMER_SUBMENU_SAFETY {
                // Cursor-tracking safety tick — fires at 30 ms while any popup is open.
                //
                // Ordering is important (see fix notes in CLAUDE.md):
                //   1. Escape check — immediate dismiss.
                //   2. Cursor poll — compute inside_any.
                //   3. Mouse-button check — instant dismiss if newly pressed outside.
                //   4. Transition-only cursor events (CursorExit / CursorReenter) — NOT every tick.
                //   5. SafetyTick — always emitted, drives the dismiss countdown.
                //   6. Kill timer if chain closed.
                if let Some(state) = unsafe { toolbar_state(hwnd) } {
                    use windows::Win32::UI::Input::KeyboardAndMouse::{
                        GetAsyncKeyState, VK_ESCAPE, VK_LBUTTON, VK_RBUTTON,
                    };

                    // 1. Escape — WS_EX_NOACTIVATE means WM_KEYDOWN rarely arrives; poll here.
                    let esc_pressed =
                        unsafe { (GetAsyncKeyState(VK_ESCAPE.0 as i32) as u16) & 0x8000 != 0 };
                    if esc_pressed {
                        state.execute_submenu_event(hwnd, crate::submenu::SubmenuEvent::Dismiss);
                        unsafe {
                            let _ = KillTimer(Some(hwnd), crate::toolbar::TIMER_SUBMENU_SAFETY);
                        }
                        state.submenu_timer_active = false;
                        return LRESULT(0);
                    }

                    // 2. Cursor poll.
                    let mut cursor = POINT::default();
                    let cursor_ok = unsafe { GetCursorPos(&mut cursor).is_ok() };
                    let inside_any =
                        cursor_ok && cursor_inside_any_padded_popup(state, cursor.x, cursor.y);

                    // 3. Mouse-button check — instant dismiss on a fresh click outside all popups.
                    let lb =
                        unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16) & 0x8000 != 0 };
                    let rb =
                        unsafe { (GetAsyncKeyState(VK_RBUTTON.0 as i32) as u16) & 0x8000 != 0 };
                    let btn_now = lb || rb;
                    let btn_just_pressed = btn_now && !state.prev_mouse_button_down;
                    state.prev_mouse_button_down = btn_now;

                    if btn_just_pressed && !inside_any {
                        state.execute_submenu_event(hwnd, crate::submenu::SubmenuEvent::Dismiss);
                        unsafe {
                            let _ = KillTimer(Some(hwnd), crate::toolbar::TIMER_SUBMENU_SAFETY);
                        }
                        state.submenu_timer_active = false;
                        return LRESULT(0);
                    }

                    // 4. Transition-only cursor events.
                    // Emitting CursorExit on every tick was resetting dismiss_pending_ticks=5
                    // each tick, preventing the countdown from ever reaching zero.
                    if inside_any != state.cursor_was_inside_popup {
                        let ev = if inside_any {
                            crate::submenu::SubmenuEvent::CursorReenter
                        } else {
                            crate::submenu::SubmenuEvent::CursorExit
                        };
                        state.execute_submenu_event(hwnd, ev);
                        state.cursor_was_inside_popup = inside_any;
                    }

                    // 5. SafetyTick — always; drives the dismiss countdown.
                    state.execute_submenu_event(hwnd, crate::submenu::SubmenuEvent::SafetyTick);

                    // 6. Kill timer if chain closed (belt-and-suspenders alongside
                    //    maybe_kill_safety_timer which runs inside close_all_popups).
                    if !state.submenu_chain.is_open() && state.submenu_timer_active {
                        unsafe {
                            let _ = KillTimer(Some(hwnd), crate::toolbar::TIMER_SUBMENU_SAFETY);
                        }
                        state.submenu_timer_active = false;
                    }
                }
                LRESULT(0)
            } else if timer_id == crate::toolbar::TIMER_RECENT_DEBOUNCE {
                if let Some(state) = unsafe { toolbar_state(hwnd) } {
                    state.flush_recent(hwnd);
                }
                LRESULT(0)
            } else if timer_id == crate::toolbar::TIMER_HOVER_OPEN {
                // Kill the one-shot timer regardless.
                unsafe {
                    let _ = KillTimer(Some(hwnd), crate::toolbar::TIMER_HOVER_OPEN);
                }
                if let Some(state) = unsafe { toolbar_state(hwnd) } {
                    let pending = state.hover_open_pending_button.take();
                    // Verify cursor is still on the same button we were waiting for.
                    let still_same = match state.pointer {
                        pointer::PointerState::Hovering { button } => Some(button) == pending,
                        _ => false,
                    };
                    if !still_same {
                        return LRESULT(0);
                    }
                    let Some(button_idx) = pending else {
                        return LRESULT(0);
                    };
                    // Don't open if chain already open or user is pressing.
                    if state.submenu_chain.is_open() {
                        return LRESULT(0);
                    }
                    // Translate button index → folder index (button 0 is '+').
                    let folder_button = button_idx.saturating_sub(1);
                    let Some(cfg) = state.config.as_ref() else {
                        return LRESULT(0);
                    };
                    let Some(folder) = cfg.folders.get(folder_button) else {
                        return LRESULT(0);
                    };
                    let is_recent = matches!(folder.kind, crate::config::FolderKind::Recent);
                    let raw_path = folder.path.clone();

                    // Record trigger context (same as FireLongPress arm in execute_pointer_command).
                    let btn_rect = state.button_screen_rect(hwnd, folder_button);
                    state.last_button_screen_rect = btn_rect;
                    state.last_button_center_y_on_open = (btn_rect.top + btn_rect.bottom) / 2;
                    let mut cursor = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut cursor);
                    }
                    state.last_cursor_x_on_open = cursor.x;
                    state.last_cursor_y_on_open = cursor.y;

                    log::info!(
                        "hover_open: fire folder_button={} at cursor=({},{})",
                        folder_button,
                        cursor.x,
                        cursor.y
                    );

                    state.execute_submenu_event(
                        hwnd,
                        crate::submenu::SubmenuEvent::OpenRoot {
                            path: std::path::PathBuf::from(raw_path),
                            button_center_y: state.last_button_center_y_on_open,
                            is_recent,
                        },
                    );
                }
                LRESULT(0)
            } else if timer_id == crate::toolbar::TIMER_SUBMENU_AUTOSCROLL {
                if let Some(state) = unsafe { toolbar_state(hwnd) } {
                    let Some(popup_hwnd) = state.autoscroll_popup else {
                        return LRESULT(0);
                    };
                    let dir = state.autoscroll_dir;
                    if dir == 0 {
                        return LRESULT(0);
                    }
                    // Advance scroll on the target popup by 1 step, respecting clamps.
                    let done = unsafe {
                        match crate::submenu_wnd::popup_state(popup_hwnd) {
                            Some(popup) => {
                                let total = popup.display_items.len();
                                let visible = popup.layout.visible_count;
                                if total <= visible {
                                    true
                                } else {
                                    let max_offset = total - visible;
                                    let new_offset = if dir < 0 {
                                        popup.scroll_offset.saturating_sub(1)
                                    } else {
                                        (popup.scroll_offset + 1).min(max_offset)
                                    };
                                    if new_offset != popup.scroll_offset {
                                        popup.scroll_offset = new_offset;
                                        let _ = windows::Win32::Graphics::Gdi::InvalidateRect(
                                            Some(popup_hwnd),
                                            None,
                                            false,
                                        );
                                        // Recompute highlight at current cursor
                                        // position — scroll moved items under it.
                                        crate::submenu_wnd::refresh_highlight_from_cursor(
                                            popup_hwnd,
                                        );
                                    }
                                    new_offset == 0 || new_offset == max_offset
                                }
                            }
                            None => true,
                        }
                    };
                    if done {
                        unsafe {
                            let _ = KillTimer(Some(hwnd), crate::toolbar::TIMER_SUBMENU_AUTOSCROLL);
                        }
                        state.autoscroll_popup = None;
                        state.autoscroll_dir = 0;
                    }
                }
                LRESULT(0)
            } else if timer_id == crate::toolbar::TIMER_DWELL_TICK {
                // 1 Hz poll: detect active-tab navigation + drive dwell tracking.
                if let Some(state) = unsafe { toolbar_state(hwnd) } {
                    // Resolve borrow: clone path before calling mutable execute_tracker_event.
                    let active_path = state.current_active_tab_path();
                    let prev_path = state.recent_tracker.current_path.clone();

                    match (active_path, prev_path) {
                        (Some(new), Some(ref old)) if &new != old => {
                            state.execute_tracker_event(
                                hwnd,
                                crate::recent_tracker::TrackerEvent::NavigationTo(new),
                            );
                        }
                        (Some(new), None) => {
                            state.execute_tracker_event(
                                hwnd,
                                crate::recent_tracker::TrackerEvent::NavigationTo(new),
                            );
                        }
                        (None, Some(_)) => {
                            state.execute_tracker_event(
                                hwnd,
                                crate::recent_tracker::TrackerEvent::ForegroundLost,
                            );
                        }
                        _ => { /* same path or both None — no navigation event */ }
                    }
                    // DwellTick every tick regardless of path changes.
                    state.execute_tracker_event(
                        hwnd,
                        crate::recent_tracker::TrackerEvent::DwellTick,
                    );
                }
                LRESULT(0)
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
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

/// Toggle `config.recent.enabled`: enable adds the Recent pseudo-entry to `folders[]`,
/// disable removes it, deletes `recents.json`, clears in-memory state, and
/// disarms the 1 Hz dwell tick. Either branch saves config and refreshes the toolbar.
fn handle_toggle_recent(state: &mut crate::toolbar::ToolbarState, toolbar: HWND) {
    let now_enabled = match crate::actions::toggle_recent_in_state(state) {
        Ok(enabled) => enabled,
        Err(()) => return, // config unavailable or save failed; already logged
    };

    if now_enabled {
        // Hydrate recent_list from disk in case recents.json survived a prior disable.
        state.recent_list = state.recent_store.load();
        state.arm_dwell_tick(toolbar);
    } else {
        // Privacy-critical: delete recents.json.
        if let Err(e) = state.recent_store.delete() {
            log::warn!("toggle recent: recents.json delete failed: {e:?}");
        }
        state.recent_list.clear();
        state.recent_dirty = false;
        state.recent_debounce_pending = false;
        unsafe {
            let _ = KillTimer(Some(toolbar), crate::toolbar::TIMER_RECENT_DEBOUNCE);
        }
        state.disarm_dwell_tick(toolbar);
        // Dismiss any open submenu that may reference the Recent button.
        state.execute_submenu_event(toolbar, crate::submenu::SubmenuEvent::Dismiss);
    }

    crate::lifecycle::refresh_toolbar(toolbar);
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
