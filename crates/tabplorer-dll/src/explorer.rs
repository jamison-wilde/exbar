//! Explorer window hierarchy walker.
//! Finds the toolbar slot (command bar area) within a CabinetWClass window.

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetClassNameW, GetWindowRect};
use windows_core::BOOL;

const SHELL_TAB_WINDOW_CLASS: &str = "ShellTabWindowClass";
const XAML_HOST_CLASS: &str = "XamlExplorerHostIslandWindow";

/// Represents a slot where the tabplorer toolbar can be placed.
pub struct ToolbarSlot {
    /// The parent window that will host the toolbar.
    pub parent: HWND,
    /// Screen-space bounds of the XAML host island window.
    pub bounds: RECT,
}

/// Walks the Explorer window hierarchy starting from `cabinet_hwnd` to find
/// the command bar area.
///
/// Hierarchy: CabinetWClass → ShellTabWindowClass → XamlExplorerHostIslandWindow
pub fn find_toolbar_slot(cabinet_hwnd: HWND) -> Option<ToolbarSlot> {
    let shell_tab = find_child_by_class(cabinet_hwnd, SHELL_TAB_WINDOW_CLASS)?;
    let xaml_host = find_child_by_class(shell_tab, XAML_HOST_CLASS)?;

    let mut bounds = RECT::default();
    unsafe {
        GetWindowRect(xaml_host, &mut bounds).ok()?;
    }

    Some(ToolbarSlot {
        parent: shell_tab,
        bounds,
    })
}

/// Finds the first direct or indirect child window whose class name matches
/// `target_class`. Returns `None` if no match is found.
pub fn find_child_by_class(parent: HWND, target_class: &str) -> Option<HWND> {
    struct SearchState {
        target: String,
        found: Option<HWND>,
    }

    let mut state = SearchState {
        target: target_class.to_string(),
        found: None,
    };

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let state = unsafe { &mut *(lparam.0 as *mut SearchState) };
        let class = get_class_name(hwnd);
        if class == state.target {
            state.found = Some(hwnd);
            // Return FALSE to stop enumeration.
            return BOOL(0);
        }
        BOOL(1)
    }

    let lparam = LPARAM(&mut state as *mut SearchState as isize);
    unsafe {
        let _ = EnumChildWindows(Some(parent), Some(enum_proc), lparam);
    }

    state.found
}

/// Returns the window class name for `hwnd`, or an empty string on failure.
pub fn get_class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}
