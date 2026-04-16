//! Decides *when* the toolbar should be shown or hidden, based on
//! cross-process foreground-window changes. Hosts the
//! `WINEVENT_OUTOFCONTEXT` hook (`foreground_event_proc`) and the
//! shared `GLOBAL_TOOLBAR` static the hook uses to find the toolbar
//! HWND. Lifecycle (`create_toolbar`) lives in `lifecycle.rs`.

use std::sync::Mutex;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
    ShowWindow,
};

// ── Global state ──────────────────────────────────────────────────────────────

/// The single global toolbar HWND (None if not yet created or destroyed).
static GLOBAL_TOOLBAR: Mutex<Option<isize>> = Mutex::new(None);

pub(crate) fn set_global_toolbar(hwnd: HWND) {
    *GLOBAL_TOOLBAR.lock().unwrap() = Some(hwnd.0 as isize);
}

pub(crate) fn clear_global_toolbar() {
    *GLOBAL_TOOLBAR.lock().unwrap() = None;
}

pub(crate) fn get_global_toolbar_hwnd() -> Option<HWND> {
    GLOBAL_TOOLBAR.lock().unwrap().map(|h| HWND(h as *mut _))
}

// ── Foreground window tracking ───────────────────────────────────────────────

const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const EVENT_SYSTEM_MINIMIZESTART: u32 = 0x0016;
const EVENT_SYSTEM_MINIMIZEEND: u32 = 0x0017;
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const EVENT_SYSTEM_MOVESIZESTART: u32 = 0x000A;
const EVENT_SYSTEM_MOVESIZEEND: u32 = 0x000B;

// ── Pure classifier ───────────────────────────────────────────────────────────

/// Classification of a foreground-change target window.
#[derive(Debug, PartialEq, Eq)]
pub enum Foreground {
    /// The window belongs to our own process (exbar.exe).
    Ours,
    /// The window belongs to `explorer.exe`.
    Explorer,
    /// The window belongs to some other unrelated process.
    Other,
}

/// Pure function: classify a foreground window by PID and exe path.
///
/// `target_pid` — the PID of the window gaining foreground.
/// `target_exe` — full path of the exe for that PID (or `None` if unknown).
/// `our_pid`    — PID of the current exbar.exe process.
pub fn classify_foreground(target_pid: u32, target_exe: Option<&str>, our_pid: u32) -> Foreground {
    if target_pid == our_pid {
        return Foreground::Ours;
    }
    let exe_basename = target_exe
        .and_then(|full| full.rsplit(['\\', '/']).next())
        .map(str::to_ascii_lowercase);
    if exe_basename.as_deref() == Some("explorer.exe") {
        Foreground::Explorer
    } else {
        Foreground::Other
    }
}

// ── Win32 process helpers ─────────────────────────────────────────────────────

/// Return the full exe path for a given PID, or `None` on failure.
fn exe_path_for_pid(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::ProcessStatus::GetModuleFileNameExW;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 260];
    let len = unsafe { GetModuleFileNameExW(Some(h), None, &mut buf) } as usize;
    unsafe {
        let _ = CloseHandle(h);
    }
    if len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// PID of the process owning `hwnd`, or 0 on failure.
fn pid_for_hwnd(hwnd: HWND) -> u32 {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let mut pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid
}

/// True if `hwnd` belongs to our own (exbar.exe) process — e.g., our toolbar,
/// our popup menu, our rename edit, our folder picker dialog.
fn hwnd_in_our_process(hwnd: HWND) -> bool {
    let pid = pid_for_hwnd(hwnd);
    let our_pid = std::process::id();
    classify_foreground(pid, exe_path_for_pid(pid).as_deref(), our_pid) == Foreground::Ours
}

/// True if `hwnd` belongs to any process whose executable filename is `explorer.exe`.
/// Used by the foreground hook to keep the toolbar visible over Explorer's own
/// popups (tooltips, tree-view pop-outs, Quick Access breadcrumb flyouts, etc.).
fn hwnd_in_explorer_process(hwnd: HWND) -> bool {
    let pid = pid_for_hwnd(hwnd);
    let our_pid = std::process::id();
    classify_foreground(pid, exe_path_for_pid(pid).as_deref(), our_pid) == Foreground::Explorer
}

// ── WinEvent callback ─────────────────────────────────────────────────────────

unsafe extern "system" fn foreground_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    let tb_opt = get_global_toolbar_hwnd();

    let class = crate::explorer::get_class_name(hwnd);
    let is_explorer = class == "CabinetWClass";
    let in_our_process = hwnd_in_our_process(hwnd);

    if event == EVENT_SYSTEM_MINIMIZESTART {
        // Only hide if NOT our process (avoid hiding on Explorer's internal popups)
        if !in_our_process && let Some(tb) = tb_opt {
            update_toolbar_visibility(tb);
        }
        return;
    }

    if event == EVENT_SYSTEM_MINIMIZEEND {
        if is_explorer && let Some(tb) = tb_opt {
            show_above(tb, hwnd);
        }
        return;
    }

    if event == EVENT_SYSTEM_MOVESIZESTART {
        // Explorer is being moved/resized — hide toolbar to avoid it
        // sitting in the wrong position mid-drag.
        if !in_our_process && hwnd_in_explorer_process(hwnd) {
            if let Some(tb) = tb_opt {
                unsafe {
                    crate::warn_on_err!(ShowWindow(tb, SW_HIDE).ok());
                }
            }
        }
        return;
    }

    if event == EVENT_SYSTEM_MOVESIZEEND {
        // Explorer finished moving/resizing — reposition and show toolbar.
        if !in_our_process && hwnd_in_explorer_process(hwnd) {
            if let Some(tb) = tb_opt {
                // Use the CabinetWClass for origin, not the HWND from the
                // event (which might be a child window).
                let explorer = if class == "CabinetWClass" {
                    hwnd
                } else if let Some(state) = unsafe { crate::toolbar::toolbar_state(tb) } {
                    state.active_explorer.unwrap_or(hwnd)
                } else {
                    hwnd
                };
                show_above(tb, explorer);
            }
        }
        return;
    }

    // EVENT_SYSTEM_FOREGROUND
    // Keep toolbar visible if the foreground window is:
    //   - An Explorer window (re-raise above it; create toolbar on first event)
    //   - Explorer's own process popups (tooltips, tree-view pop-outs, etc.)
    //   - OUR process (rename edit, folder picker, popup menu — all transient)
    // Hide only when a window in a DIFFERENT unrelated process takes foreground.
    let in_explorer = hwnd_in_explorer_process(hwnd);
    if is_explorer {
        if let Some(toolbar_hwnd) = get_global_toolbar_hwnd() {
            // SAFETY: Win32 dispatches WinEvent callbacks on the thread that
            // installed SetWinEventHook — our message-pump thread. Same
            // single-threaded invariant `toolbar_state` relies on.
            if let Some(state) = unsafe { crate::toolbar::toolbar_state(toolbar_hwnd) } {
                state.active_explorer = Some(hwnd);
            }
        }
        // First time we see an Explorer foreground, create the toolbar.
        // If not ready, retry logic is deferred to Task 8.
        if tb_opt.is_none()
            && let Some(info) = crate::explorer::check_explorer_ready(hwnd)
        {
            let hinst = crate::lifecycle::exe_hinstance();
            let _ = crate::lifecycle::create_toolbar(info.cabinet_hwnd, &info.default_pos, hinst);
        }
        if let Some(tb) = get_global_toolbar_hwnd() {
            show_above(tb, hwnd);
        }
    } else if in_explorer {
        // Explorer-process window (XAML islands, ForegroundStaging, etc.)
        // that isn't CabinetWClass. Re-show the toolbar only if Explorer
        // is genuinely foreground — Win11 fires these events during
        // transition animations even when switching AWAY from Explorer.
        let actual_fg = unsafe { GetForegroundWindow() };
        if actual_fg == hwnd
            || crate::explorer::get_class_name(actual_fg) == "CabinetWClass"
            || hwnd_in_explorer_process(actual_fg)
        {
            if let Some(tb) = tb_opt {
                show_above(tb, hwnd);
            }
        }
    } else if in_our_process {
        // Our own popup menu / rename edit / folder picker. Keep visible.
    } else if let Some(tb) = tb_opt {
        unsafe {
            crate::warn_on_err!(ShowWindow(tb, SW_HIDE).ok());
        }
    }
}

pub(crate) fn show_above(toolbar: HWND, explorer: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST;

    // Reposition toolbar relative to the Explorer window using saved offset.
    if let Some((off_x, off_y)) = crate::position::load_saved_offset() {
        let (ox, oy) = crate::position::explorer_visible_origin(explorer);
        let (tx, ty) = crate::position::apply_offset(off_x, off_y, ox, oy);
        // Get current toolbar size for clamping.
        let mut tr = windows::Win32::Foundation::RECT::default();
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(toolbar, &mut tr);
        }
        let tw = tr.right - tr.left;
        let th = tr.bottom - tr.top;
        let (cx, cy) =
            crate::position::clamp_to_work_area_for(tx, ty, tw, th, Some(explorer));
        unsafe {
            crate::warn_on_err!(SetWindowPos(
                toolbar,
                Some(HWND_TOPMOST),
                cx,
                cy,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE,
            ));
            crate::warn_on_err!(ShowWindow(toolbar, SW_SHOWNA).ok());
        }
    } else {
        // No saved offset — just show in place with topmost.
        unsafe {
            crate::warn_on_err!(ShowWindow(toolbar, SW_SHOWNA).ok());
            crate::warn_on_err!(SetWindowPos(
                toolbar,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            ));
        }
    }
}

/// Hide the toolbar if the foreground window is in a different process
/// (i.e., not Explorer or any of its helper windows).
fn update_toolbar_visibility(toolbar: HWND) {
    let fg = unsafe { GetForegroundWindow() };
    if !hwnd_in_our_process(fg) {
        unsafe {
            crate::warn_on_err!(ShowWindow(toolbar, SW_HIDE).ok());
        }
    }
}

/// Install the foreground WinEvent hook. Callers must invoke exactly once
/// (from `run_hook`). Returns the hook handle so the caller can
/// `UnhookWinEvent` it at process exit.
pub fn install_foreground_hook() -> HWINEVENTHOOK {
    // SAFETY: SetWinEventHook registers our extern "system" callback and
    // returns a handle we own; single call from run_hook is the sole user.
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_MINIMIZEEND, // range covers FOREGROUND, MINIMIZESTART, MINIMIZEEND
            None,
            Some(foreground_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    log::info!("Installed foreground event hook");
    hook
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_pid_is_ours() {
        assert_eq!(
            classify_foreground(42, Some("C:\\Windows\\explorer.exe"), 42),
            Foreground::Ours
        );
    }

    #[test]
    fn explorer_basename_is_explorer() {
        assert_eq!(
            classify_foreground(7, Some("C:\\Windows\\explorer.exe"), 1),
            Foreground::Explorer
        );
    }

    #[test]
    fn explorer_basename_case_insensitive() {
        assert_eq!(
            classify_foreground(7, Some("C:\\Windows\\Explorer.EXE"), 1),
            Foreground::Explorer
        );
    }

    #[test]
    fn other_executable_is_other() {
        assert_eq!(
            classify_foreground(7, Some("C:\\Program Files\\Code\\code.exe"), 1),
            Foreground::Other
        );
    }

    #[test]
    fn missing_exe_is_other() {
        assert_eq!(classify_foreground(7, None, 1), Foreground::Other);
    }

    #[test]
    fn forward_slash_path_works() {
        assert_eq!(
            classify_foreground(7, Some("C:/Windows/explorer.exe"), 1),
            Foreground::Explorer
        );
    }
}
