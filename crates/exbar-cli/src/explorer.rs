//! Explorer window detection.
//! Detects CabinetWClass windows and provides a default toolbar position.

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetClassNameW, GetWindowRect};
use windows_core::BOOL;

/// Win11 uses DesktopChildSiteBridge for XAML content.
/// We wait for this to exist before injecting (means Explorer is fully loaded).
const DESKTOP_BRIDGE_CLASS: &str = "Microsoft.UI.Content.DesktopChildSiteBridge";

/// Info needed to create the toolbar for an Explorer window.
pub struct ExplorerInfo {
    /// The CabinetWClass window.
    pub cabinet_hwnd: HWND,
    /// Default screen position for the toolbar (top-right area of Explorer).
    pub default_pos: RECT,
}

/// Check if an Explorer window's UI bridge is ready.
/// Returns None if the DesktopChildSiteBridge doesn't exist yet
/// (Explorer hasn't finished initializing).
pub fn check_explorer_ready(cabinet_hwnd: HWND) -> Option<ExplorerInfo> {
    // Wait for the bridge to exist — ensures Explorer is fully loaded
    let _bridge = find_child_by_class(cabinet_hwnd, DESKTOP_BRIDGE_CLASS)?;

    // Get the Explorer window rect for default toolbar positioning
    let mut cabinet_rect = RECT::default();
    unsafe {
        GetWindowRect(cabinet_hwnd, &mut cabinet_rect).ok()?;
    }

    // Default position: near the top-left of Explorer's content area.
    // Real position will be clamped to monitor bounds after layout computes
    // the actual toolbar size in WM_CREATE. We intentionally pick top-left
    // so wide toolbars don't hang off the right side of the screen.
    let default_pos = RECT {
        left: cabinet_rect.left + 40,
        top: cabinet_rect.top + 120, // below title bar, tabs, and command bar
        right: cabinet_rect.left + 440,
        bottom: cabinet_rect.top + 160,
    };

    Some(ExplorerInfo {
        cabinet_hwnd,
        default_pos,
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
