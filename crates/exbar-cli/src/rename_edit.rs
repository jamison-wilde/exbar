//! Inline-rename Win32 EDIT control management. Creation, subclass
//! proc (key/focus translation to `RenameEvent`), text read, destroy.
//! The pure `rename::transition` controller and the
//! `ToolbarState::execute_rename_event` adapter live elsewhere; this
//! module is purely the Win32 plumbing the adapter calls into.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{DEFAULT_GUI_FONT, GetStockObject};
use windows::Win32::UI::Controls::WC_EDITW;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DLGC_WANTALLKEYS, DestroyWindow, GetWindowTextLengthW, GetWindowTextW,
    SendMessageW, WM_GETDLGCODE, WM_KEYDOWN, WM_KILLFOCUS, WM_SETFONT, WS_BORDER, WS_CHILD,
    WS_VISIBLE,
};
use windows_core::PCWSTR;

use crate::lifecycle::exe_hinstance;
use crate::rename::RenameEvent;
use crate::toolbar::{toolbar_state, wide_null};

/// Create a Win32 EDIT control over `button_rect` (client coords on `toolbar`),
/// pre-populate it with `initial_name`, subclass it with `rename_subclass_proc`,
/// and fire `RenameEvent::Started` on the controller.
///
/// Any existing in-flight rename is cancelled first (idempotent).
pub(crate) fn start_inline_rename(
    toolbar: HWND,
    button_rect: RECT,
    folder_index: usize,
    initial_name: &str,
) {
    // 1. Cancel any in-flight rename via the controller (idempotent if none).
    // SAFETY: toolbar is the toolbar HWND; we are on the message-pump thread.
    if let Some(state) = unsafe { toolbar_state(toolbar) } {
        state.execute_rename_event(toolbar, RenameEvent::Cancelled);
    }

    // 2. Create the EDIT control over the button's screen rect.
    let hinstance = exe_hinstance();
    let wide_initial: Vec<u16> = wide_null(initial_name);
    // ES_AUTOHSCROLL = 0x0080
    const ES_AUTOHSCROLL: u32 = 0x0080;
    let style = WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | ES_AUTOHSCROLL;

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
    let Ok(edit) = edit else {
        return;
    };

    // 3. Match the EDIT control's font to the toolbar button text
    //    (DEFAULT_GUI_FONT, same as draw_buttons). Without this the
    //    control falls back to the ancient SYSTEM_FONT which renders
    //    ~8px on modern DPI — unreadable next to the button labels.
    let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
    unsafe {
        SendMessageW(
            edit,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
    }

    // 4. Select-all + focus.
    const EM_SETSEL: u32 = 0x00B1;
    unsafe {
        SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
        let _ = SetFocus(Some(edit));
    }

    // 5. Subclass with the toolbar HWND as ref_data — no Box::into_raw.
    // The subclass proc reads context (folder_index) from state.rename_state
    // via `toolbar_state(toolbar)`.
    unsafe {
        crate::warn_on_err!(
            SetWindowSubclass(edit, Some(rename_subclass_proc), 1, toolbar.0 as usize).ok()
        );
    }

    // 6. Notify the controller — Started transition records the active rename.
    // SAFETY: same single-thread invariant as the cancel call at the top.
    if let Some(state) = unsafe { toolbar_state(toolbar) } {
        state.execute_rename_event(
            toolbar,
            RenameEvent::Started {
                folder_index,
                edit_hwnd: edit.0 as isize,
            },
        );
    }
}

/// Subclass procedure for the inline-rename EDIT control.
///
/// Translates `WM_KEYDOWN` (Enter/Escape) and `WM_KILLFOCUS` into
/// `RenameEvent`s and dispatches them through `execute_rename_event`.
/// `ref_data` is the toolbar HWND (`usize`).
///
/// # Safety
/// Called by Win32 on the toolbar's message-pump thread — same
/// single-threaded invariant that `toolbar_state` relies on.
pub(crate) unsafe extern "system" fn rename_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    ref_data: usize,
) -> LRESULT {
    const VK_RETURN: usize = 0x0D;
    const VK_ESCAPE: usize = 0x1B;

    let toolbar = HWND(ref_data as *mut _);
    let event = match msg {
        WM_GETDLGCODE => return LRESULT(DLGC_WANTALLKEYS as isize),
        WM_KEYDOWN if wparam.0 == VK_RETURN => RenameEvent::CommitRequested {
            text: read_edit_text(hwnd),
        },
        WM_KEYDOWN if wparam.0 == VK_ESCAPE => RenameEvent::Cancelled,
        WM_KILLFOCUS => RenameEvent::CommitRequested {
            text: read_edit_text(hwnd),
        },
        _ => return unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    };

    // SAFETY: Subclass proc runs on the toolbar's message-pump thread —
    // same single-threaded invariant `toolbar_state` relies on elsewhere.
    if let Some(state) = unsafe { toolbar_state(toolbar) } {
        state.execute_rename_event(toolbar, event);
    }
    LRESULT(0)
}

/// Read the current text from a Win32 EDIT control as a `String`.
pub(crate) fn read_edit_text(edit: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(edit) } as usize;
    let mut buf = vec![0u16; len + 1];
    let got = unsafe { GetWindowTextW(edit, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..got])
}

/// Remove the subclass proc and destroy the EDIT control window.
///
/// `RemoveWindowSubclass` is called before `DestroyWindow` so the
/// `WM_DESTROY` re-entry cannot reach the subclass proc after the
/// HWND is invalid.
pub(crate) fn destroy_rename_edit(edit: HWND) {
    unsafe {
        let _ = RemoveWindowSubclass(edit, Some(rename_subclass_proc), 1);
        let _ = DestroyWindow(edit);
    }
}
