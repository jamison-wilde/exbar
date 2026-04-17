//! Decides *when* the toolbar should be shown or hidden, based on
//! cross-process foreground-window changes. Hosts the
//! `WINEVENT_OUTOFCONTEXT` hook (`foreground_event_proc`) and the
//! shared `GLOBAL_TOOLBAR` static the hook uses to find the toolbar
//! HWND. Lifecycle (`create_toolbar`) lives in `lifecycle.rs`.

use std::sync::Mutex;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SetWindowPos, ShowWindow,
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

// ── File-dialog probe ─────────────────────────────────────────────────────────

/// Can tell whether a given HWND is a Shell-hosted file dialog, i.e. whether
/// it has a `SHELLDLL_DefView` descendant. Separated behind a trait so the
/// pure classifier can be tested without touching Win32.
pub trait DefViewProbe {
    fn has_defview(&self, hwnd: HWND) -> bool;
}

/// Production probe: walks the window tree via `EnumChildWindows`, checking
/// each child's class name for `SHELLDLL_DefView`.
pub struct Win32DefViewProbe;

impl DefViewProbe for Win32DefViewProbe {
    fn has_defview(&self, hwnd: HWND) -> bool {
        use windows::Win32::Foundation::LPARAM;
        use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetClassNameW};
        use windows_core::BOOL;

        struct Ctx {
            found: bool,
        }
        unsafe extern "system" fn cb(child: HWND, lparam: LPARAM) -> BOOL {
            let ctx = unsafe { &mut *(lparam.0 as *mut Ctx) };
            let mut buf = [0u16; 64];
            let n = unsafe { GetClassNameW(child, &mut buf) } as usize;
            if n > 0 && String::from_utf16_lossy(&buf[..n]) == "SHELLDLL_DefView" {
                ctx.found = true;
                BOOL(0) // stop enumeration
            } else {
                BOOL(1)
            }
        }
        let mut ctx = Ctx { found: false };
        unsafe {
            let _ = EnumChildWindows(Some(hwnd), Some(cb), LPARAM(&mut ctx as *mut _ as isize));
        }
        ctx.found
    }
}

/// Role a foreground HWND can play for the toolbar.
#[derive(Debug, PartialEq, Eq)]
pub enum HwndRole {
    /// A Shell-hosted file dialog (Save As / Open). Toolbar attaches to this.
    FileDialog,
    /// Not a role the toolbar cares about specifically.
    Unknown,
}

/// Pure: decide whether an HWND represents a Shell-hosted file dialog.
///
/// Gated on `dialog_enabled` so the `Config.enable_file_dialogs = false`
/// escape hatch produces `Unknown`.
///
/// Recognises a file dialog by: class name `#32770` AND a `SHELLDLL_DefView`
/// descendant. Class name is passed in rather than queried here to keep this
/// function fully pure and testable.
pub fn classify_hwnd(
    hwnd: HWND,
    class_name: &str,
    dialog_enabled: bool,
    probe: &impl DefViewProbe,
) -> HwndRole {
    if !dialog_enabled {
        return HwndRole::Unknown;
    }
    if class_name == "#32770" && probe.has_defview(hwnd) {
        return HwndRole::FileDialog;
    }
    HwndRole::Unknown
}

// ── Foreground window tracking ───────────────────────────────────────────────

const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const EVENT_SYSTEM_MINIMIZESTART: u32 = 0x0016;
const EVENT_SYSTEM_MINIMIZEEND: u32 = 0x0017;
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const EVENT_SYSTEM_MOVESIZESTART: u32 = 0x000A;
const EVENT_SYSTEM_MOVESIZEEND: u32 = 0x000B;
const EVENT_OBJECT_LOCATIONCHANGE: u32 = 0x800B;
const OBJID_WINDOW: i32 = 0;
const CHILDID_SELF: i32 = 0;

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
            reposition_and_show(tb, hwnd);
        }
        return;
    }

    if event == EVENT_SYSTEM_MOVESIZESTART {
        // Explorer is being moved/resized — hide toolbar and set flag.
        // Only react for the active Explorer to avoid hiding when a
        // different Explorer window is being moved.
        if let Some(tb) = tb_opt
            && let Some(state) = unsafe { crate::toolbar::toolbar_state(tb) }
            && state.active_target.map(|t| t.hwnd) == Some(hwnd)
        {
            state.explorer_moving = true;
            unsafe {
                crate::warn_on_err!(ShowWindow(tb, SW_HIDE).ok());
            }
        }
        return;
    }

    if event == EVENT_SYSTEM_MOVESIZEEND {
        // Explorer finished moving/resizing — clear flag, reposition and show.
        if let Some(tb) = tb_opt
            && let Some(state) = unsafe { crate::toolbar::toolbar_state(tb) }
            && state.active_target.map(|t| t.hwnd) == Some(hwnd)
        {
            state.explorer_moving = false;
            reposition_and_show(tb, hwnd);
        }
        return;
    }

    if event == EVENT_OBJECT_LOCATIONCHANGE
        && _id_object == OBJID_WINDOW
        && _id_child == CHILDID_SELF
    {
        // Explorer window moved/resized (maximize, restore, snap, drag finish).
        // Only react for the active CabinetWClass, and not during a drag
        // (MOVESIZEEND handles that). Defer via PostMessage to avoid
        // repositioning from an async callback before geometry settles.
        if let Some(tb) = tb_opt
            && let Some(state) = unsafe { crate::toolbar::toolbar_state(tb) }
            && state.active_target.map(|t| t.hwnd) == Some(hwnd)
            && !state.explorer_moving
        {
            let delay = state.config.as_ref().map_or(250, |c| c.reposition_delay_ms);
            log::debug!(
                "LOCATIONCHANGE: explorer={hwnd:?}, hiding + scheduling reposition ({delay}ms)"
            );
            unsafe {
                // Hide immediately so the toolbar doesn't sit in the wrong
                // spot during the maximize/restore animation.
                crate::warn_on_err!(ShowWindow(tb, SW_HIDE).ok());
                // Schedule reposition after the animation settles.
                // SetTimer with the same ID replaces any pending timer,
                // so rapid LOCATIONCHANGE events naturally debounce.
                let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(
                    Some(tb),
                    crate::toolbar::TIMER_REPOSITION,
                    delay,
                    None,
                );
            }
        }
        return;
    }

    // Ignore all other event types — only process EVENT_SYSTEM_FOREGROUND below.
    if event != EVENT_SYSTEM_FOREGROUND {
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
                state.active_target = Some(crate::target::ActiveTarget::explorer(hwnd));
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
            reposition_and_show(tb, hwnd);
        }
    } else if in_explorer {
        // Explorer-process window that isn't CabinetWClass. Only show the
        // toolbar if it's related to the active Explorer file browser —
        // check that its root ancestor is the active CabinetWClass.
        // This filters out alt-tab/win-tab (XamlExplorerHostIslandWindow
        // owned by the task switcher, not by a CabinetWClass) while still
        // allowing Explorer's own XAML islands and popups through.
        if let Some(tb) = tb_opt
            && let Some(state) = unsafe { crate::toolbar::toolbar_state(tb) }
            && let Some(active) = state.active_target.map(|t| t.hwnd)
        {
            let root = unsafe {
                windows::Win32::UI::WindowsAndMessaging::GetAncestor(
                    hwnd,
                    windows::Win32::UI::WindowsAndMessaging::GA_ROOT,
                )
            };
            if root == active {
                log::debug!(
                    "foreground: explorer-process class={class:?} root={root:?} matches active, showing"
                );
                // Also verify Explorer is genuinely foreground — Win11 fires
                // XAML events during transition animations away from Explorer.
                let actual_fg = unsafe { GetForegroundWindow() };
                if actual_fg == hwnd
                    || crate::explorer::get_class_name(actual_fg) == "CabinetWClass"
                    || hwnd_in_explorer_process(actual_fg)
                {
                    show_above(tb, hwnd);
                }
            } else {
                log::debug!(
                    "foreground: explorer-process class={class:?} root={root:?} != active={active:?}, ignoring (task switcher?)"
                );
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

/// Show the toolbar and set it topmost. If the active Explorer window has
/// moved since we last positioned (e.g. maximize/restore), reposition.
/// Uses `state.active_explorer` (the CabinetWClass HWND) for the origin
/// check — NOT the event HWND, which may be an XAML island child.
pub(crate) fn show_above(toolbar: HWND, _explorer: HWND) {
    if let Some(state) = unsafe { crate::toolbar::toolbar_state(toolbar) }
        && let Some(active) = state.active_target.map(|t| t.hwnd)
    {
        let current_origin = crate::position::explorer_visible_origin(active);
        log::debug!(
            "show_above: active={active:?} origin={current_origin:?} cached={:?}",
            state.last_explorer_origin
        );
        if state.last_explorer_origin != Some(current_origin) {
            log::debug!("show_above: explorer moved, repositioning");
            reposition_and_show(toolbar, active);
            return;
        }
    }

    use windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST;
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

/// Reposition the toolbar relative to `explorer` using the saved offset,
/// then show it topmost. Used on Explorer move/resize finish, maximize/restore,
/// and Explorer window switch — NOT on routine foreground events.
pub(crate) fn reposition_and_show(toolbar: HWND, explorer: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST;

    let origin = crate::position::explorer_visible_origin(explorer);
    log::debug!("reposition_and_show: explorer={explorer:?} origin={origin:?}");

    if let Some((off_x, off_y)) =
        crate::position::load_saved_offset(crate::target::TargetKind::Explorer)
    {
        let (tx, ty) = crate::position::apply_offset(off_x, off_y, origin.0, origin.1);
        log::debug!("reposition_and_show: offset=({off_x},{off_y}) target=({tx},{ty})");
        let mut tr = windows::Win32::Foundation::RECT::default();
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(toolbar, &mut tr);
        }
        let tw = tr.right - tr.left;
        let th = tr.bottom - tr.top;
        let (cx, cy) = crate::position::clamp_to_work_area_for(tx, ty, tw, th, Some(explorer));
        log::debug!("reposition_and_show: clamped=({cx},{cy}) size=({tw},{th})");
        unsafe {
            // Move first (no z-order change), then show, then raise topmost.
            // Split into separate calls because during maximize/restore
            // animations, a single SetWindowPos with move+topmost can lose
            // the z-order fight with Explorer.
            crate::warn_on_err!(SetWindowPos(
                toolbar,
                None,
                cx,
                cy,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER,
            ));
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
    } else {
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

    // Cache the origin so show_above can detect future moves.
    if let Some(state) = unsafe { crate::toolbar::toolbar_state(toolbar) } {
        state.last_explorer_origin = Some(origin);
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

/// Install WinEvent hooks. Callers must invoke exactly once (from `run_hook`).
/// Returns both hook handles so the caller can `UnhookWinEvent` them at exit.
///
/// Two hooks:
/// 1. System events (0x0003–0x0017): FOREGROUND, MOVESIZESTART/END, MINIMIZESTART/END
/// 2. LOCATIONCHANGE (0x800B): detects Explorer maximize/restore/snap
pub fn install_foreground_hook() -> (HWINEVENTHOOK, HWINEVENTHOOK) {
    // SAFETY: SetWinEventHook registers our extern "system" callback and
    // returns a handle we own; single call from run_hook is the sole user.
    let system_hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_MINIMIZEEND, // range covers FOREGROUND..MINIMIZEEND
            None,
            Some(foreground_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    let location_hook = unsafe {
        SetWinEventHook(
            EVENT_OBJECT_LOCATIONCHANGE,
            EVENT_OBJECT_LOCATIONCHANGE,
            None,
            Some(foreground_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    log::info!("Installed foreground + location-change event hooks");
    (system_hook, location_hook)
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

    struct MockDefView(bool);
    impl super::DefViewProbe for MockDefView {
        fn has_defview(&self, _hwnd: windows::Win32::Foundation::HWND) -> bool {
            self.0
        }
    }

    #[test]
    fn mock_defview_true() {
        let m = MockDefView(true);
        assert!(m.has_defview(HWND(42 as *mut _)));
    }

    #[test]
    fn mock_defview_false() {
        let m = MockDefView(false);
        assert!(!m.has_defview(HWND(42 as *mut _)));
    }

    #[test]
    fn classify_hwnd_dialog_class_with_defview_is_file_dialog() {
        let probe = MockDefView(true);
        assert_eq!(
            classify_hwnd(
                HWND(42 as *mut _),
                "#32770",
                /* dialog_enabled */ true,
                &probe
            ),
            HwndRole::FileDialog,
        );
    }

    #[test]
    fn classify_hwnd_dialog_class_without_defview_is_unknown() {
        let probe = MockDefView(false);
        assert_eq!(
            classify_hwnd(HWND(42 as *mut _), "#32770", true, &probe),
            HwndRole::Unknown,
        );
    }

    #[test]
    fn classify_hwnd_non_dialog_class_is_unknown_even_if_defview_exists() {
        let probe = MockDefView(true);
        assert_eq!(
            classify_hwnd(HWND(42 as *mut _), "CabinetWClass", true, &probe),
            HwndRole::Unknown,
        );
    }

    #[test]
    fn classify_hwnd_dialog_disabled_suppresses_file_dialog() {
        let probe = MockDefView(true);
        assert_eq!(
            classify_hwnd(
                HWND(42 as *mut _),
                "#32770",
                /* dialog_enabled */ false,
                &probe
            ),
            HwndRole::Unknown,
        );
    }
}
