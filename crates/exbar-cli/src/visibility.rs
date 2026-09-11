//! Decides *when* the toolbar should be shown or hidden, based on
//! cross-process foreground-window changes. Hosts the
//! `WINEVENT_OUTOFCONTEXT` hook (`foreground_event_proc`) and the
//! shared `GLOBAL_TOOLBAR` static the hook uses to find the toolbar
//! HWND. Lifecycle (`create_toolbar`) lives in `lifecycle.rs`.

use std::sync::Mutex;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, IsIconic, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SetWindowPos, ShowWindow,
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
    /// Returns true if `hwnd` descends into a recognisable file-dialog shape.
    ///
    /// Two markers are accepted:
    /// - `SHELLDLL_DefView` — modern Common Item Dialog (`IFileDialog`).
    /// - `ComboBoxEx32` — legacy `GetOpenFileName` / `GetSaveFileName`
    ///   Common Dialog's "Look in:" folder combo.
    ///
    /// Either marker is sufficient; both imply the dialog accepts path
    /// input via Ctrl+L (modern) or typed-into-filename-and-Enter (legacy).
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
            if n > 0 {
                let name = String::from_utf16_lossy(&buf[..n]);
                if name == "SHELLDLL_DefView" || name == "ComboBoxEx32" {
                    ctx.found = true;
                    return BOOL(0); // stop enumeration
                }
            }
            BOOL(1)
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

/// Pure: decide whether an HWND represents a file dialog.
///
/// Gated on `dialog_enabled` so the `Config.enable_file_dialogs = false`
/// escape hatch produces `Unknown`.
///
/// Recognises a file dialog by: class name `#32770` AND a known file-dialog
/// descendant (`SHELLDLL_DefView` for modern `IFileDialog`, or `ComboBoxEx32`
/// for the legacy `GetOpenFileName`/`GetSaveFileName` Common Dialog).
/// Class name is passed in rather than queried here to keep this function
/// fully pure and testable.
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
const EVENT_SYSTEM_MENUPOPUPSTART: u32 = 0x0006;
const EVENT_SYSTEM_MENUPOPUPEND: u32 = 0x0007;
const EVENT_SYSTEM_MINIMIZESTART: u32 = 0x0016;
const EVENT_SYSTEM_MINIMIZEEND: u32 = 0x0017;
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const EVENT_SYSTEM_MOVESIZESTART: u32 = 0x000A;
const EVENT_SYSTEM_MOVESIZEEND: u32 = 0x000B;
const EVENT_OBJECT_LOCATIONCHANGE: u32 = 0x800B;
const EVENT_OBJECT_SHOW: u32 = 0x8002;
const EVENT_OBJECT_HIDE: u32 = 0x8003;
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

/// Pure: decide whether the watchdog should HIDE a currently-visible toolbar,
/// given the foreground window's classification.
///
/// `fg_is_ours`        — foreground window is in exbar's own process.
/// `fg_class`          — class name of the foreground window.
/// `fg_root_is_active` — `GetAncestor(fg, GA_ROOT)` equals the active target HWND
///                       (covers Explorer XAML islands/tooltips and the file
///                       dialog plus its child popups).
///
/// Keep (return `false`) for our process, any `CabinetWClass`, or a window
/// rooted in the active target. Hide (return `true`) otherwise.
pub fn watchdog_should_hide(fg_is_ours: bool, fg_class: &str, fg_root_is_active: bool) -> bool {
    if fg_is_ours {
        return false;
    }
    if fg_class == "CabinetWClass" {
        return false;
    }
    if fg_root_is_active {
        return false;
    }
    true
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
pub(crate) fn hwnd_in_our_process(hwnd: HWND) -> bool {
    let pid = pid_for_hwnd(hwnd);
    let our_pid = std::process::id();
    classify_foreground(pid, exe_path_for_pid(pid).as_deref(), our_pid) == Foreground::Ours
}

/// True if `hwnd` belongs to any process whose executable filename is `explorer.exe`.
/// Used by the foreground hook to keep the toolbar visible over Explorer's own
/// popups (tooltips, tree-view pop-outs, Quick Access breadcrumb flyouts, etc.).
pub(crate) fn hwnd_in_explorer_process(hwnd: HWND) -> bool {
    let pid = pid_for_hwnd(hwnd);
    let our_pid = std::process::id();
    classify_foreground(pid, exe_path_for_pid(pid).as_deref(), our_pid) == Foreground::Explorer
}

// ── Topmost helpers ───────────────────────────────────────────────────────────

/// Set or clear the toolbar's `HWND_TOPMOST` flag.
///
/// Called when a shell context menu opens (drop to `HWND_NOTOPMOST` so the
/// menu renders above the toolbar) and when it closes / Explorer retakes
/// foreground (restore `HWND_TOPMOST` so Explorer's own XAML content stays below).
fn set_toolbar_topmost(toolbar: HWND, topmost: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{HWND_NOTOPMOST, HWND_TOPMOST};
    let target = if topmost {
        HWND_TOPMOST
    } else {
        HWND_NOTOPMOST
    };
    unsafe {
        crate::warn_on_err!(SetWindowPos(
            toolbar,
            Some(target),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        ));
    }
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

    // Classic Win32 popup menus (including shell context menus on Win10 and
    // legacy Win11 style). Drop topmost so the menu renders above the toolbar,
    // then restore on MENUPOPUPEND. These events are in the system-hook range
    // (0x0003..=0x0017) so no extra hook registration is needed.
    if event == EVENT_SYSTEM_MENUPOPUPSTART {
        if let Some(tb) = tb_opt
            && let Some(state) = unsafe { crate::toolbar::toolbar_state(tb) }
        {
            if state.popup_open_count == 0 {
                log::debug!("MENUPOPUPSTART hwnd={hwnd:?} class={class:?} — toolbar → non-topmost");
                set_toolbar_topmost(tb, false);
            }
            state.popup_open_count = state.popup_open_count.saturating_add(1);
        }
        return;
    }
    if event == EVENT_SYSTEM_MENUPOPUPEND {
        if let Some(tb) = tb_opt
            && let Some(state) = unsafe { crate::toolbar::toolbar_state(tb) }
        {
            state.popup_open_count = state.popup_open_count.saturating_sub(1);
            if state.popup_open_count == 0 {
                log::debug!("MENUPOPUPEND — count=0, toolbar → topmost");
                set_toolbar_topmost(tb, true);
            }
        }
        return;
    }

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
            // If Explorer is minimized, its GetWindowRect origin is (-32000, -32000).
            // Scheduling a reposition now would race with the hide triggered by
            // MINIMIZESTART and re-show the toolbar at the work-area corner.
            if unsafe { IsIconic(hwnd).as_bool() } {
                log::debug!(
                    "LOCATIONCHANGE: explorer={hwnd:?} is iconic — skipping timer schedule"
                );
                return;
            }
            let delay = state.config.as_ref().map_or(250, |c| c.reposition_delay_ms);
            log::debug!(
                "LOCATIONCHANGE: explorer={hwnd:?}, hiding + scheduling reposition ({delay}ms)"
            );
            state.reposition_pending = true;
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

    handle_foreground_change(hwnd, &class, is_explorer, in_our_process, tb_opt);
}

/// Handle one `EVENT_SYSTEM_FOREGROUND`.
///
/// Keep the toolbar visible if the foreground window is:
///   - An Explorer window (re-raise above it; create the toolbar on first event)
///   - Explorer's own process popups (tooltips, tree-view pop-outs, etc.)
///   - OUR process (rename edit, folder picker, popup menu — all transient)
///
/// Hide only when a window in a DIFFERENT unrelated process takes foreground.
///
/// Bursty traffic short-circuits all of that: see [`storm_suppressed`].
fn handle_foreground_change(
    hwnd: HWND,
    class: &str,
    is_explorer: bool,
    in_our_process: bool,
    tb_opt: Option<HWND>,
) {
    // A storm of events means Explorer is thrashing, not that the user is
    // switching windows. Hide and let the settle timer decide once it stops.
    if storm_suppressed(hwnd, is_explorer, in_our_process, tb_opt) {
        return;
    }

    let in_explorer = hwnd_in_explorer_process(hwnd);

    if is_explorer {
        handle_explorer_foreground(hwnd, tb_opt);
    } else if in_explorer {
        handle_explorer_process_foreground(hwnd, class, tb_opt);
    } else if in_our_process {
        // Our own popup menu / rename edit / folder picker. Keep visible.
    } else {
        handle_foreign_foreground(hwnd, class, tb_opt);
    }
}

/// Run the event through the debounce. Returns `true` when the event was
/// swallowed because foreground traffic is bursty — in which case
/// `TIMER_FG_SETTLE` has been (re)armed and the toolbar's *current*
/// visibility is frozen until it fires.
///
/// Freezing rather than force-hiding is deliberate, and was measured. An
/// earlier version hid the toolbar for the duration of every storm; two
/// hours of dogfooding produced three sequences like this one, where a
/// transient window blipped the foreground while the toolbar sat correctly
/// over Explorer:
///
/// ```text
/// 22:32:30.131 watchdog: visible=true should_hide=false
/// 22:32:30.223 foreground storm: suppressed
/// 22:32:30.224 ShowWindow(tb, SW_HIDE)
/// 22:32:30.535 fg settle: CabinetWClass -> Show
/// ```
///
/// That is a 314 ms blank inserted by the anti-flicker path itself. Holding
/// the existing state is better in every case: a storm that ends where it
/// started is now invisible to the user, one that ends elsewhere just hides
/// up to `settle_ms` later than it would have, and during genuine Explorer
/// churn the toolbar sits still while Explorer flashes — which was the
/// point.
///
/// The active target is still recorded for real Explorer windows even while
/// suppressing, so the settle re-check knows where to put the toolbar if
/// Explorer turns out to be what is genuinely in front.
fn storm_suppressed(
    hwnd: HWND,
    is_explorer: bool,
    in_our_process: bool,
    tb_opt: Option<HWND>,
) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::SetTimer;

    if in_our_process {
        // Our own menu / rename edit / picker taking focus is never Explorer
        // churn. Debouncing these would hide the toolbar out from under a
        // user who is actively interacting with it.
        return false;
    }

    let Some(tb) = tb_opt else {
        // No toolbar yet — nothing can flash, and creation must not be
        // debounced away.
        return false;
    };
    // SAFETY: WinEvent callbacks arrive on the thread that installed the
    // hook — our message-pump thread, the same single-threaded invariant
    // `toolbar_state` relies on.
    let Some(state) = (unsafe { crate::toolbar::toolbar_state(tb) }) else {
        return false;
    };

    // Never yank the toolbar away mid-interaction. Mirrors the transient
    // states the watchdog already refuses to act on.
    if state.popup_open_count > 0 || state.submenu_chain.is_open() || state.rename_state.is_some() {
        return false;
    }

    let debounce_ms = state
        .config
        .as_ref()
        .map_or_else(crate::config::default_foreground_debounce_ms, |c| {
            c.foreground_debounce_ms
        });
    let now = state.clock.now_unix_ms();

    match state.fg_debounce.on_event(now, debounce_ms) {
        crate::fg_debounce::EventDecision::ApplyNow => false,
        crate::fg_debounce::EventDecision::SuppressAndSettle { settle_ms } => {
            if is_explorer {
                state.active_target = Some(crate::target::ActiveTarget::explorer(hwnd));
            }
            log::debug!("foreground storm: hwnd={hwnd:?} suppressed, settling in {settle_ms}ms");
            unsafe {
                // Visibility is deliberately left untouched here — see above.
                // Same ID replaces any pending settle, so a continuing storm
                // keeps pushing the re-check out until the traffic stops.
                let _ = SetTimer(Some(tb), crate::toolbar::TIMER_FG_SETTLE, settle_ms, None);
            }
            true
        }
    }
}

/// A real `CabinetWClass` took foreground.
fn handle_explorer_foreground(hwnd: HWND, tb_opt: Option<HWND>) {
    if let Some(toolbar_hwnd) = get_global_toolbar_hwnd() {
        // SAFETY: see `storm_suppressed` — callback runs on the pump thread.
        if let Some(state) = unsafe { crate::toolbar::toolbar_state(toolbar_hwnd) } {
            state.active_target = Some(crate::target::ActiveTarget::explorer(hwnd));
        }
    }
    // First time we see an Explorer foreground, create the toolbar.
    // A cold Explorer has not built its XAML bridge yet when this event
    // arrives, so the readiness probe fails; hand off to the bootstrap
    // retry rather than abandoning creation until some later event
    // happens to catch Explorer warm.
    if tb_opt.is_none() {
        match crate::explorer::check_explorer_ready(hwnd) {
            Some(info) => {
                let hinst = crate::lifecycle::exe_hinstance();
                let _ =
                    crate::lifecycle::create_toolbar(info.cabinet_hwnd, &info.default_pos, hinst);
            }
            None => crate::bootstrap::schedule_retry(hwnd),
        }
    }
    if let Some(tb) = get_global_toolbar_hwnd() {
        // Direction-1 guard: Win11 fires spurious EVENT_SYSTEM_FOREGROUND for
        // Explorer windows during transition animations while a foreign app is
        // the real foreground. Only show if Explorer is genuinely foreground —
        // symmetric with the actual_fg guard in the in_explorer branch below.
        let actual_fg = unsafe { GetForegroundWindow() };
        if actual_fg == hwnd
            || crate::explorer::get_class_name(actual_fg) == "CabinetWClass"
            || hwnd_in_explorer_process(actual_fg)
            || hwnd_in_our_process(actual_fg)
        {
            reposition_and_show(tb, hwnd);
        } else {
            log::debug!(
                "is_explorer foreground but actual_fg={actual_fg:?} is foreign — skipping show"
            );
        }
    }
}

/// An explorer.exe window that is not a `CabinetWClass` took foreground.
fn handle_explorer_process_foreground(hwnd: HWND, class: &str, tb_opt: Option<HWND>) {
    // Desktop (Progman / WorkerW) lives in explorer.exe but is NOT a
    // file-browser window. Hide the toolbar when the desktop takes
    // foreground so exbar doesn't ride on top of the wallpaper.
    if class == "Progman" || class == "WorkerW" {
        log::debug!("foreground: desktop class={class:?} hwnd={hwnd:?} — hiding toolbar");
        if let Some(tb) = tb_opt {
            update_toolbar_visibility(tb);
        }
        return;
    }

    // Only show the toolbar if this window is related to the active Explorer
    // file browser — check that its root ancestor is the active CabinetWClass.
    // This filters out alt-tab/win-tab (XamlExplorerHostIslandWindow owned by
    // the task switcher, not by a CabinetWClass) while still allowing
    // Explorer's own XAML islands and popups through.
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
}

/// A window in some other process took foreground. It is either a
/// Shell-hosted file dialog (treat like Explorer) or a reason to hide.
fn handle_foreign_foreground(hwnd: HWND, class: &str, tb_opt: Option<HWND>) {
    let dialog_enabled = tb_opt
        .and_then(|tb| unsafe { crate::toolbar::toolbar_state(tb) })
        .and_then(|s| s.config.as_ref())
        .map(|c| c.enable_file_dialogs)
        .unwrap_or(true);

    match classify_hwnd(hwnd, class, dialog_enabled, &Win32DefViewProbe) {
        HwndRole::FileDialog => {
            // Toolbar may not exist yet (first dialog ever).
            if tb_opt.is_none() {
                let mut rect = windows::Win32::Foundation::RECT::default();
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect);
                }
                let default_pos = windows::Win32::Foundation::RECT {
                    left: rect.left + 40,
                    top: rect.top + 120,
                    right: rect.left + 440,
                    bottom: rect.top + 160,
                };
                let hinst = crate::lifecycle::exe_hinstance();
                let _ = crate::lifecycle::create_toolbar(hwnd, &default_pos, hinst);
            }
            if let Some(tb) = get_global_toolbar_hwnd() {
                if let Some(state) = unsafe { crate::toolbar::toolbar_state(tb) } {
                    state.active_target = Some(crate::target::ActiveTarget::file_dialog(hwnd));
                    // Force a reposition; last_explorer_origin was for explorer.
                    state.last_explorer_origin = None;
                }
                reposition_and_show(tb, hwnd);
            }
        }
        HwndRole::Unknown => {
            // Different unrelated process — hide. Also dismiss any open
            // submenu chain so popups don't linger after focus leaves us.
            if let Some(tb) = tb_opt {
                if let Some(state) = unsafe { crate::toolbar::toolbar_state(tb) }
                    && state.submenu_chain.is_open()
                {
                    state.execute_submenu_event(tb, crate::submenu::SubmenuEvent::Dismiss);
                }
                unsafe {
                    crate::warn_on_err!(ShowWindow(tb, SW_HIDE).ok());
                }
            }
        }
    }
}

/// Fired by `TIMER_FG_SETTLE` once a foreground-event storm has gone quiet.
///
/// Looks at what is *actually* in front exactly once and commits to a single
/// show/hide, instead of having tracked every transient window on the way.
pub(crate) fn settle_foreground(toolbar: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor, KillTimer};

    unsafe {
        let _ = KillTimer(Some(toolbar), crate::toolbar::TIMER_FG_SETTLE);
    }

    let Some(state) = (unsafe { crate::toolbar::toolbar_state(toolbar) }) else {
        return;
    };
    state.fg_debounce.on_settle();

    let fg = unsafe { GetForegroundWindow() };
    let fg_class = crate::explorer::get_class_name(fg);
    let fg_is_ours = hwnd_in_our_process(fg);

    // Re-target before deciding: a storm can end on a different Explorer
    // window, or on a file dialog, than the one we last attached to. The
    // dialog probe walks child windows, which is why it belongs here — once
    // per storm — and not on the suppressed per-event path.
    if fg_class == "CabinetWClass" {
        state.active_target = Some(crate::target::ActiveTarget::explorer(fg));
    } else if !fg_is_ours {
        let dialog_enabled = state
            .config
            .as_ref()
            .map(|c| c.enable_file_dialogs)
            .unwrap_or(true);
        if classify_hwnd(fg, &fg_class, dialog_enabled, &Win32DefViewProbe) == HwndRole::FileDialog
        {
            state.active_target = Some(crate::target::ActiveTarget::file_dialog(fg));
            // Offsets are per-kind; force a fresh measure against the dialog.
            state.last_explorer_origin = None;
        }
    }

    let active = state.active_target.map(|t| t.hwnd);
    // A re-target above makes this true by construction for the new target.
    let fg_root_is_active = active.is_some_and(|a| unsafe { GetAncestor(fg, GA_ROOT) } == a);

    let outcome = crate::fg_debounce::settle_outcome(&fg_class, fg_is_ours, fg_root_is_active);
    log::debug!(
        "fg settle: fg={fg:?} class={fg_class:?} ours={fg_is_ours} root_active={fg_root_is_active} -> {outcome:?}"
    );

    match outcome {
        crate::fg_debounce::SettleOutcome::Show => {
            // Prefer the recorded target: `fg` may be an XAML island child,
            // which is not something to position against.
            if let Some(target) = active {
                reposition_and_show(toolbar, target);
            }
        }
        crate::fg_debounce::SettleOutcome::Hide => unsafe {
            crate::warn_on_err!(ShowWindow(toolbar, SW_HIDE).ok());
        },
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

    // Guard: skip if Explorer is minimized. GetWindowRect on a minimized window
    // returns origin (-32000, -32000) — the Windows iconic sentinel. Repositioning
    // to that origin would clamp to the work-area corner and show the toolbar
    // there. It also corrupts any subsequent offset re-measurement.
    if unsafe { IsIconic(explorer).as_bool() } {
        log::debug!("reposition_and_show: explorer={explorer:?} is iconic — skipping show");
        return;
    }

    let origin = crate::position::explorer_visible_origin(explorer);
    log::debug!("reposition_and_show: explorer={explorer:?} origin={origin:?}");

    // Defence-in-depth: reject the iconic sentinel even if IsIconic missed it
    // (e.g., a race between the animation start and our callback).
    if origin.0 < -30_000 || origin.1 < -30_000 {
        log::debug!(
            "reposition_and_show: explorer={explorer:?} origin=({},{}) looks minimized — skipping",
            origin.0,
            origin.1
        );
        return;
    }

    let kind = unsafe { crate::toolbar::toolbar_state(toolbar) }
        .and_then(|s| s.active_target.map(|t| t.kind))
        .unwrap_or(crate::target::TargetKind::Explorer);

    if let Some((off_x, off_y)) = crate::position::load_saved_offset(kind) {
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

/// Periodic safety net (driven by `TIMER_FOREGROUND_WATCHDOG`). Corrects a
/// toolbar that was left visible over a foreign app by a spurious Explorer
/// foreground event with no subsequent corrective event.
///
/// Hide-only by default; if `Config.watchdog_reshow` is set it also re-shows
/// the toolbar when it is hidden but the active target is genuinely foreground.
pub(crate) fn watchdog_tick(toolbar: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor, IsWindowVisible};

    // Read all needed fields from state, then drop the borrow before calling
    // any function (like show_above) that re-enters toolbar_state internally.
    let (skip, should_hide, visible, active, reshow) = {
        let Some(state) = (unsafe { crate::toolbar::toolbar_state(toolbar) }) else {
            return;
        };

        // Skip during transient/modal states — a popup menu, open submenu chain,
        // active inline rename, or an in-progress Explorer move all legitimately
        // change which window is foreground.
        if state.popup_open_count > 0
            || state.submenu_chain.is_open()
            || state.rename_state.is_some()
            || state.explorer_moving
            || state.reposition_pending
            // Riding out a foreground storm: the toolbar is deliberately
            // hidden and TIMER_FG_SETTLE owns the next state change. A
            // watchdog re-show here would reintroduce the flashing.
            || state.fg_debounce.is_settling()
        {
            (true, false, false, None, false)
        } else {
            let fg = unsafe { GetForegroundWindow() };
            let fg_is_ours = hwnd_in_our_process(fg);
            let fg_class = crate::explorer::get_class_name(fg);
            let active = state.active_target.map(|t| t.hwnd);
            let fg_root_is_active = active.is_some_and(|a| {
                let root = unsafe { GetAncestor(fg, GA_ROOT) };
                root == a
            });

            let should_hide = watchdog_should_hide(fg_is_ours, &fg_class, fg_root_is_active);
            let visible = unsafe { IsWindowVisible(toolbar).as_bool() };
            let reshow = state.config.as_ref().is_some_and(|c| c.watchdog_reshow);

            log::debug!(
                "watchdog: fg={fg:?} class={fg_class:?} ours={fg_is_ours} root_active={fg_root_is_active} visible={visible} should_hide={should_hide}"
            );

            (false, should_hide, visible, active, reshow)
        }
    }; // state borrow ends here

    if skip {
        return;
    }

    if visible && should_hide {
        log::debug!("watchdog: hiding toolbar");
        unsafe {
            crate::warn_on_err!(ShowWindow(toolbar, SW_HIDE).ok());
        }
        return;
    }

    // Opt-in re-show: toolbar hidden but the active target is genuinely foreground.
    if !visible
        && !should_hide
        && reshow
        && let Some(active_hwnd) = active
        && !unsafe { IsIconic(active_hwnd).as_bool() }
    {
        log::debug!("watchdog: re-showing toolbar over active={active_hwnd:?}");
        show_above(toolbar, active_hwnd);
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

/// Narrow context-menu detection. Win11 shell context menus (right-click
/// on a file/folder/empty area in Explorer) fire EVENT_OBJECT_SHOW /
/// EVENT_OBJECT_HIDE with window class
/// "Microsoft.UI.Content.PopupWindowSiteBridge". While at least one is
/// visible, drop the toolbar from HWND_TOPMOST so the menu renders above.
///
/// # Safety
/// Registered as a WinEvent callback — Win32 guarantees the signature.
unsafe extern "system" fn object_show_hide_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object != OBJID_WINDOW {
        return;
    }
    // Filter to explorer.exe first to keep the event volume sane.
    if !hwnd_in_explorer_process(hwnd) {
        return;
    }
    let class = crate::explorer::get_class_name(hwnd);
    if class != "Microsoft.UI.Content.PopupWindowSiteBridge" {
        return;
    }
    let Some(toolbar) = get_global_toolbar_hwnd() else {
        return;
    };
    let Some(state) = (unsafe { crate::toolbar::toolbar_state(toolbar) }) else {
        return;
    };

    match event {
        EVENT_OBJECT_SHOW => {
            if state.popup_open_count == 0 {
                log::debug!("popup SHOW hwnd={hwnd:?} class={class:?} — toolbar → non-topmost");
                set_toolbar_topmost(toolbar, false);
            }
            state.popup_open_count = state.popup_open_count.saturating_add(1);
        }
        EVENT_OBJECT_HIDE => {
            state.popup_open_count = state.popup_open_count.saturating_sub(1);
            if state.popup_open_count == 0 {
                log::debug!("popup HIDE hwnd={hwnd:?} — count=0, toolbar → topmost");
                set_toolbar_topmost(toolbar, true);
            }
        }
        _ => {}
    }
}

/// Install WinEvent hooks. Callers must invoke exactly once (from `run_hook`).
/// Returns hook handles so the caller can `UnhookWinEvent` them at exit.
///
/// Three hooks:
/// 1. System events (0x0003–0x0017): FOREGROUND, MOVESIZESTART/END, MINIMIZESTART/END,
///    MENUPOPUPSTART/END (classic Win32 popup menus)
/// 2. LOCATIONCHANGE (0x800B): detects Explorer maximize/restore/snap
/// 3. OBJECT_SHOW/HIDE (0x8002–0x8003): Win11 shell context menus
///    (`Microsoft.UI.Content.PopupWindowSiteBridge`) — fires when popups appear/dismiss
pub fn install_foreground_hook() -> (HWINEVENTHOOK, HWINEVENTHOOK, HWINEVENTHOOK) {
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
    // Win11 shell context menus fire OBJECT_SHOW/HIDE (not FOREGROUND) for
    // their PopupWindowSiteBridge windows. Filter to explorer.exe in the callback.
    let show_hide_hook = unsafe {
        SetWinEventHook(
            EVENT_OBJECT_SHOW,
            EVENT_OBJECT_HIDE,
            None,
            Some(object_show_hide_proc),
            0, // idProcess = 0: all processes (filter in callback)
            0, // idThread
            WINEVENT_OUTOFCONTEXT,
        )
    };
    log::info!("Installed foreground + location-change + show/hide event hooks");
    (system_hook, location_hook, show_hide_hook)
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

    #[test]
    fn watchdog_keeps_when_foreground_is_ours() {
        assert!(!watchdog_should_hide(true, "RandomClass", false));
    }

    #[test]
    fn watchdog_keeps_when_foreground_is_cabinet() {
        assert!(!watchdog_should_hide(false, "CabinetWClass", false));
    }

    #[test]
    fn watchdog_keeps_when_foreground_root_is_active_target() {
        // e.g. an Explorer XAML island or a dialog child popup.
        assert!(!watchdog_should_hide(
            false,
            "Microsoft.UI.Content.IslandWindow",
            true
        ));
    }

    #[test]
    fn watchdog_hides_foreign_app() {
        assert!(watchdog_should_hide(false, "Chrome_WidgetWin_1", false));
    }

    #[test]
    fn watchdog_hides_desktop() {
        // Progman is neither ours, nor cabinet, nor rooted in the active target.
        assert!(watchdog_should_hide(false, "Progman", false));
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
