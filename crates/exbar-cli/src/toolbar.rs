//! Floating draggable toolbar window for Explorer folder shortcuts.

use std::panic::AssertUnwindSafe;
use std::sync::{Mutex, Once};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, CreateSolidBrush, DEFAULT_GUI_FONT, DT_CENTER, DT_SINGLELINE,
    DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC, GetStockObject,
    GetTextExtentPoint32W, HDC, InvalidateRect, PAINTSTRUCT, ReleaseDC, ScreenToClient,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::SystemServices::MK_CONTROL;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::Controls::{WC_EDITW, WM_MOUSELEAVE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DLGC_WANTALLKEYS, DefWindowProcW,
    DestroyWindow, GWLP_USERDATA, GetClientRect, GetForegroundWindow, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowTextW, HTCAPTION, LWA_ALPHA, PostMessageW, RegisterClassExW,
    SPI_GETWORKAREA, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SendMessageW, SetLayeredWindowAttributes,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, SystemParametersInfoW, WM_CAPTURECHANGED,
    WM_CREATE, WM_DESTROY, WM_GETDLGCODE, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOVE, WM_NCHITTEST, WM_PAINT, WM_RBUTTONUP, WNDCLASSEXW, WS_BORDER, WS_CHILD,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
};
use windows_core::PCWSTR;

use std::sync::Arc;

use crate::clipboard::{Clipboard, Win32Clipboard};
use crate::config::{Config, ConfigStore, FolderEntry, JsonFileStore, Orientation};
use crate::dragdrop::{FileOperator, Win32FileOp};
use crate::hit_test;
use crate::layout::{self, ButtonLayout, LayoutInput};
use crate::picker::{FolderPicker, Win32Picker};
use crate::pointer;
use crate::shell_windows::{ShellBrowser, Win32Shell};
use crate::theme;

// ── Safe wrappers for repetitive patterns ───────────────────────────────────

/// Retrieve the `ToolbarState` stored in the window's user data.
///
/// # Safety
/// - `hwnd` must be a toolbar window (same HWND that had
///   `SetWindowLongPtrW(GWLP_USERDATA, state)` called during its `WM_CREATE`).
/// - Caller must be on the toolbar's message-pump thread — Win32's
///   single-threaded message dispatch is the synchronization boundary.
/// - The returned reference borrows the state for the caller's scope; no
///   other code path may hold a mutable reference in parallel (guaranteed
///   by single-threaded message dispatch).
unsafe fn toolbar_state<'a>(hwnd: HWND) -> Option<&'a mut ToolbarState> {
    // SAFETY: GetWindowLongPtrW returns the value set by SetWindowLongPtrW;
    // we stored a Box::into_raw pointer in WM_CREATE.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ToolbarState;
    if ptr.is_null() {
        return None;
    }
    // SAFETY: ptr is non-null; state is owned by the window; caller is on
    // the message-pump thread (contract above).
    Some(unsafe { &mut *ptr })
}

/// Extract `(x, y)` from a WM_* LPARAM whose layout is
/// `(y << 16) | (x & 0xFFFF)` with signed 16-bit components.
fn lparam_point(lparam: LPARAM) -> (i32, i32) {
    let x = (lparam.0 & 0xFFFF) as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
    (x, y)
}

/// Encode `s` as a null-terminated UTF-16 vector suitable for
/// `PCWSTR(v.as_ptr())`. The vec must outlive the PCWSTR usage.
fn wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ── Constants ────────────────────────────────────────────────────────────────

static CLASS_REGISTERED: Once = Once::new();
const CLASS_NAME: &str = "ExbarToolbar";
const WM_USER_RELOAD: u32 = 0x0401;
const WM_DPICHANGED: u32 = 0x02E0;

// Layout constants (logical pixels, scale by DPI)
const BTN_PAD_H: i32 = 10;
/// Logical pixel width/height of the drag handle grip area.
const GRIP_SIZE: i32 = 12;

const REORDER_THRESHOLD: i32 = 5;

const MENU_ID_EDIT_CONFIG: u32 = 101;
const MENU_ID_RELOAD_CONFIG: u32 = 102;

const MENU_ID_OPEN: u32 = 201;
const MENU_ID_OPEN_NEW_TAB: u32 = 202;
const MENU_ID_COPY_PATH: u32 = 203;
const MENU_ID_RENAME: u32 = 204;
const MENU_ID_REMOVE: u32 = 205;

// ── Global state ──────────────────────────────────────────────────────────────

/// The single global toolbar HWND (None if not yet created or destroyed).
static GLOBAL_TOOLBAR: Mutex<Option<isize>> = Mutex::new(None);

fn set_global_toolbar(hwnd: HWND) {
    *GLOBAL_TOOLBAR.lock().unwrap() = Some(hwnd.0 as isize);
}

fn clear_global_toolbar() {
    *GLOBAL_TOOLBAR.lock().unwrap() = None;
}

fn get_global_toolbar_hwnd() -> Option<HWND> {
    GLOBAL_TOOLBAR.lock().unwrap().map(|h| HWND(h as *mut _))
}

// ── Foreground window tracking ───────────────────────────────────────────────


const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const EVENT_SYSTEM_MINIMIZESTART: u32 = 0x0016;
const EVENT_SYSTEM_MINIMIZEEND: u32 = 0x0017;
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;

/// True if `hwnd` belongs to our own (exbar.exe) process — e.g., our toolbar,
/// our popup menu, our rename edit, our folder picker dialog.
fn hwnd_in_our_process(hwnd: HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let mut pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid != 0 && pid == std::process::id()
}

/// True if `hwnd` belongs to any process whose executable filename is `explorer.exe`.
/// Used by the foreground hook to keep the toolbar visible over Explorer's own
/// popups (tooltips, tree-view pop-outs, Quick Access breadcrumb flyouts, etc.).
fn hwnd_in_explorer_process(hwnd: HWND) -> bool {
    hwnd_process_name_is(hwnd, "explorer.exe")
}

fn hwnd_process_name_is(hwnd: HWND, want: &str) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::ProcessStatus::GetModuleFileNameExW;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

    let mut pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid == 0 {
        return false;
    }
    let h = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => h,
        Err(_) => return false,
    };
    let mut buf = [0u16; 260];
    let len = unsafe { GetModuleFileNameExW(Some(h), None, &mut buf) } as usize;
    unsafe {
        let _ = CloseHandle(h);
    }
    if len == 0 {
        return false;
    }
    let path = String::from_utf16_lossy(&buf[..len]);
    path.rsplit('\\')
        .next()
        .map(|name| name.eq_ignore_ascii_case(want))
        .unwrap_or(false)
}

fn exe_hinstance() -> windows::Win32::Foundation::HINSTANCE {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    let hmod = unsafe { GetModuleHandleW(windows_core::PCWSTR::null()) }
        .unwrap_or(windows::Win32::Foundation::HMODULE(std::ptr::null_mut()));
    windows::Win32::Foundation::HINSTANCE(hmod.0)
}

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

    // EVENT_SYSTEM_FOREGROUND
    // Keep toolbar visible if the foreground window is:
    //   - An Explorer window (re-raise above it; create toolbar on first event)
    //   - Explorer's own process popups (tooltips, tree-view pop-outs, etc.)
    //   - OUR process (rename edit, folder picker, popup menu — all transient)
    // Hide only when a window in a DIFFERENT unrelated process takes foreground.
    if is_explorer {
        if let Some(toolbar_hwnd) = get_global_toolbar_hwnd() {
            // SAFETY: Win32 dispatches WinEvent callbacks on the thread that
            // installed SetWinEventHook — our message-pump thread. Same
            // single-threaded invariant `toolbar_state` relies on.
            if let Some(state) = unsafe { toolbar_state(toolbar_hwnd) } {
                state.active_explorer = Some(hwnd);
            }
        }
        // First time we see an Explorer foreground, create the toolbar.
        // If not ready, retry logic is deferred to Task 8.
        if tb_opt.is_none()
            && let Some(info) = crate::explorer::check_explorer_ready(hwnd)
        {
            let hinst = exe_hinstance();
            let _ = create_toolbar(info.cabinet_hwnd, &info.default_pos, hinst);
        }
        if let Some(tb) = get_global_toolbar_hwnd() {
            show_above(tb, hwnd);
        }
    } else if hwnd_in_explorer_process(hwnd) || in_our_process {
        // Transient popup — either Explorer's own tooltips/tree-views or our
        // own popup menu / rename edit / folder picker. Keep visible.
    } else if let Some(tb) = tb_opt {
        unsafe {
            crate::warn_on_err!(ShowWindow(tb, SW_HIDE).ok());
        }
    }
}

fn show_above(toolbar: HWND, _explorer: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST;
    unsafe {
        crate::warn_on_err!(ShowWindow(toolbar, SW_SHOWNA).ok());
        // Use HWND_TOPMOST so the toolbar stays above Explorer reliably.
        // When a non-Explorer app is foreground, the toolbar is hidden entirely,
        // so topmost won't intrude on other applications.
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

// ── Position persistence ──────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedPos {
    x: i32,
    y: i32,
}

fn pos_file_path() -> std::path::PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| "C:\\Users\\Default".into());
    let mut p = std::path::PathBuf::from(home);
    p.push(".exbar-pos.json");
    p
}

fn load_saved_pos() -> Option<(i32, i32)> {
    let bytes = std::fs::read(pos_file_path()).ok()?;
    let saved: SavedPos = serde_json::from_slice(&bytes).ok()?;
    Some((saved.x, saved.y))
}

fn save_pos(x: i32, y: i32) {
    let saved = SavedPos { x, y };
    if let Ok(json) = serde_json::to_string(&saved) {
        let _ = std::fs::write(pos_file_path(), json);
    }
}

// ── Screen bounds clamping ────────────────────────────────────────────────────

/// Return the work area of the monitor containing `ref_hwnd`, or the primary
/// monitor work area if that fails.
fn work_area_for(ref_hwnd: Option<HWND>) -> RECT {
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };

    if let Some(hwnd) = ref_hwnd {
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        if !monitor.is_invalid() {
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if unsafe { GetMonitorInfoW(monitor, &mut mi) }.as_bool() {
                return mi.rcWork;
            }
        }
    }
    // Fallback: primary monitor work area
    let mut wa = RECT::default();
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut wa as *mut RECT as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    wa
}

fn clamp_to_work_area(x: i32, y: i32, w: i32, h: i32, ref_hwnd: Option<HWND>) -> (i32, i32) {
    let wa = work_area_for(ref_hwnd);
    let cx = x.max(wa.left).min((wa.right - w).max(wa.left));
    let cy = y.max(wa.top).min((wa.bottom - h).max(wa.top));
    (cx, cy)
}

// ── Data structures ──────────────────────────────────────────────────────────

struct ToolbarState {
    buttons: Vec<ButtonLayout>,
    dpi: u32,
    config: Option<Config>,
    layout: Orientation,
    drop_registered: bool,
    /// Logical pixel size of the grip (already includes DPI scale factor).
    grip_size: i32,
    pointer: pointer::PointerState,
    mouse_tracking_started: bool,
    self_release_pending: bool,
    // SP3 trait seams
    shell_browser: Box<dyn ShellBrowser>,
    folder_picker: Box<dyn FolderPicker>,
    file_operator: Arc<dyn FileOperator>,
    clipboard: Box<dyn Clipboard>,
    config_store: Box<dyn ConfigStore>,
    // SP4 consolidation — populated in Tasks 2-3:
    active_explorer: Option<HWND>,
    rename_state: Option<RenameState>,
}

impl ToolbarState {
    fn new(dpi: u32, config: Option<Config>) -> Self {
        Self::with_deps(
            dpi,
            config,
            Box::new(Win32Shell::new()),
            Box::new(Win32Picker::new()),
            Arc::new(Win32FileOp::new()),
            Box::new(Win32Clipboard::new()),
            Box::new(JsonFileStore::new()),
        )
    }

    pub(crate) fn with_deps(
        dpi: u32,
        config: Option<Config>,
        shell_browser: Box<dyn ShellBrowser>,
        folder_picker: Box<dyn FolderPicker>,
        file_operator: Arc<dyn FileOperator>,
        clipboard: Box<dyn Clipboard>,
        config_store: Box<dyn ConfigStore>,
    ) -> Self {
        let layout = config
            .as_ref()
            .map_or(Orientation::Horizontal, |c| c.layout);
        ToolbarState {
            buttons: Vec::new(),
            dpi,
            config,
            layout,
            drop_registered: false,
            grip_size: theme::scale(GRIP_SIZE, dpi),
            pointer: pointer::PointerState::default(),
            mouse_tracking_started: false,
            self_release_pending: false,
            shell_browser,
            folder_picker,
            file_operator,
            clipboard,
            config_store,
            active_explorer: None,
            rename_state: None,
        }
    }
}

// ── SP2b pointer adapter methods ─────────────────────────────────────────────

impl ToolbarState {
    /// Drive the pointer state machine with a single event, then execute the
    /// resulting commands against Win32.
    ///
    /// Safety note on `mem::take`: between `take` and the reassignment, `self.pointer`
    /// transiently reads `Idle`. Reentrancy into this wndproc during the gap would
    /// observe the wrong state. Today the only command that can pump Win32 state
    /// synchronously is `CancelInlineRename`, which calls `DestroyWindow` on the
    /// subclassed EDIT control — `WM_DESTROY` is dispatched to the EDIT's wndproc
    /// (not ours), so `toolbar_wndproc` is not re-entered. Any future command that
    /// might trigger a toolbar-directed WM must preserve this invariant.
    fn apply_pointer_event(&mut self, hwnd: HWND, event: pointer::PointerEvent) {
        let (new_state, commands) = pointer::transition(std::mem::take(&mut self.pointer), event);
        self.pointer = new_state;
        for cmd in commands {
            self.execute_pointer_command(hwnd, cmd);
        }
    }

    fn execute_pointer_command(&mut self, hwnd: HWND, cmd: pointer::PointerCommand) {
        use pointer::PointerCommand::*;
        match cmd {
            Redraw => unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            },
            StartMouseTracking => {
                if !self.mouse_tracking_started {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    crate::warn_on_err!(unsafe { TrackMouseEvent(&mut tme) });
                    self.mouse_tracking_started = true;
                }
            }
            CaptureMouse => unsafe {
                let _ = SetCapture(hwnd);
            },
            ReleaseMouse => {
                // Only set the pending flag if we actually hold capture —
                // else ReleaseCapture won't fire WM_CAPTURECHANGED and the
                // flag would strand. Calling ReleaseCapture unconditionally
                // on the `else` branch is safe: per MSDN, ReleaseCapture is
                // a no-op when the calling thread doesn't own capture (no
                // WM_CAPTURECHANGED is dispatched).
                let we_have_capture = unsafe { GetCapture() } == hwnd;
                if we_have_capture {
                    self.self_release_pending = true;
                }
                unsafe {
                    crate::warn_on_err!(ReleaseCapture());
                }
            }
            CancelInlineRename => cancel_inline_rename(self),
            FireAddClick => {
                if let Some(path) = self.folder_picker.pick_folder() {
                    append_folder_and_reload(self, &path);
                }
            }
            FireFolderClick {
                folder_button,
                ctrl,
            } => {
                // folder_button is in folder-index space; buttons[0] is the + button.
                let btn_slot = folder_button + 1;
                if btn_slot < self.buttons.len() {
                    let path = std::path::PathBuf::from(&self.buttons[btn_slot].folder.path);
                    if ctrl {
                        let timeout = self
                            .config
                            .as_ref()
                            .map(|c| c.new_tab_timeout_ms_zero_disables)
                            .unwrap_or(500);
                        if let Some(explorer) = self.active_explorer {
                            self.shell_browser.open_in_new_tab(explorer, &path, timeout);
                        } else {
                            log::debug!("FireFolderClick(ctrl): no active explorer");
                        }
                    } else if let Some(explorer) = self.active_explorer {
                        crate::warn_on_err!(self.shell_browser.navigate(explorer, &path));
                    } else {
                        log::debug!("FireFolderClick: no active explorer");
                    }
                }
            }
            CommitReorder {
                from_folder,
                to_folder,
            } => {
                commit_reorder(self, hwnd, from_folder, to_folder);
            }
        }
    }
}

// ── Layout computation ───────────────────────────────────────────────────────

/// Measure the rendered-pixel width of each folder's label ("📁 Name" — the
/// same format used in paint) using the currently-selected font in `hdc`.
///
/// Caller must `SelectObject(hdc, font)` first. Returns a Vec the same
/// length as `folders`.
fn measure_folder_text_widths(hdc: HDC, folders: &[FolderEntry]) -> Vec<i32> {
    use windows::Win32::Foundation::SIZE;

    folders
        .iter()
        .map(|f| {
            // Match the label format used in paint: "📁 Name".
            let label = format!("\u{1F4C1} {}", f.name);
            let wide: Vec<u16> = label.encode_utf16().collect();
            let mut size = SIZE::default();
            let ok = unsafe { GetTextExtentPoint32W(hdc, &wide, &mut size) };
            if ok.as_bool() {
                size.cx
            } else {
                // Fallback: approximate — same as prior code.
                (label.chars().count() as i32) * 8
            }
        })
        .collect()
}

/// Convert a `layout::Rect` to a Win32 `RECT` for use with GDI APIs.
fn rect_to_win32(r: layout::Rect) -> RECT {
    RECT {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

/// Adapter: measures text widths via the given `hdc`, calls
/// `layout::compute_layout`, writes the resulting buttons into `state.buttons`,
/// and returns `(total_width, total_height)`.
fn compute_layout(hdc: HDC, state: &mut ToolbarState) -> (i32, i32) {
    let folders: Vec<FolderEntry> = state
        .config
        .as_ref()
        .map(|c| c.folders.clone())
        .unwrap_or_default();
    let widths = measure_folder_text_widths(hdc, &folders);

    let input = LayoutInput {
        dpi: state.dpi,
        orientation: state.layout,
        folders: &folders,
        folder_text_widths_physical_px: &widths,
        grip_size_logical_px: GRIP_SIZE,
    };

    let computed = layout::compute_layout(&input);
    state.buttons = computed.buttons;
    (computed.total_width, computed.total_height)
}

/// Returns true if (x, y) is in the grip area.
fn in_grip(state: &ToolbarState, x: i32, y: i32) -> bool {
    match state.layout {
        Orientation::Horizontal => x < state.grip_size,
        Orientation::Vertical => y < state.grip_size,
    }
}

// ── Painting ─────────────────────────────────────────────────────────────────

/// Render the toolbar into its window's DC. Called from WM_PAINT.
///
/// # Safety
///
/// Must be called from the WM_PAINT handler on the toolbar window's
/// message-pump thread. `hwnd` must be a valid toolbar HWND. The
/// function calls `BeginPaint`/`EndPaint` internally; callers must
/// not call those themselves.
unsafe fn paint(hwnd: HWND, state: &ToolbarState) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    if hdc.is_invalid() {
        return;
    }

    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }

    let is_dark = theme::is_dark_mode();

    // Background
    let bg_color = if is_dark {
        COLORREF(0x002D2D2D)
    } else {
        COLORREF(0x00F0F0F0)
    };
    let bg_brush = unsafe { CreateSolidBrush(bg_color) };
    unsafe {
        FillRect(hdc, &client, bg_brush);
    }
    unsafe {
        let _ = DeleteObject(bg_brush.into());
    }

    // Grip area — draw dots
    let grip = state.grip_size;
    let grip_color = if is_dark {
        COLORREF(0x00888888)
    } else {
        COLORREF(0x00999999)
    };
    let grip_brush = unsafe { CreateSolidBrush(grip_color) };
    let dot_size = theme::scale(2, state.dpi);
    let dot_gap = theme::scale(4, state.dpi);

    match state.layout {
        Orientation::Horizontal => {
            // Three vertical dots centered in the grip column
            let cx = grip / 2;
            let total_h = dot_size * 3 + dot_gap * 2;
            let start_y = (client.bottom - client.top - total_h) / 2;
            for i in 0..3i32 {
                let dy = start_y + i * (dot_size + dot_gap);
                let dot = RECT {
                    left: cx - dot_size / 2,
                    top: dy,
                    right: cx + dot_size / 2 + 1,
                    bottom: dy + dot_size,
                };
                unsafe {
                    FillRect(hdc, &dot, grip_brush);
                }
            }
        }
        Orientation::Vertical => {
            // Three horizontal dots centered in the grip row
            let cy = grip / 2;
            let total_w = dot_size * 3 + dot_gap * 2;
            let start_x = (client.right - client.left - total_w) / 2;
            for i in 0..3i32 {
                let dx = start_x + i * (dot_size + dot_gap);
                let dot = RECT {
                    left: dx,
                    top: cy - dot_size / 2,
                    right: dx + dot_size,
                    bottom: cy + dot_size / 2 + 1,
                };
                unsafe {
                    FillRect(hdc, &dot, grip_brush);
                }
            }
        }
    }
    unsafe {
        let _ = DeleteObject(grip_brush.into());
    }

    // Border
    let border_color = if is_dark {
        COLORREF(0x00555555)
    } else {
        COLORREF(0x00CCCCCC)
    };
    let border_brush = unsafe { CreateSolidBrush(border_color) };
    let top_border = RECT {
        left: client.left,
        top: client.top,
        right: client.right,
        bottom: client.top + 1,
    };
    unsafe {
        FillRect(hdc, &top_border, border_brush);
    }
    let bottom_border = RECT {
        left: client.left,
        top: client.bottom - 1,
        right: client.right,
        bottom: client.bottom,
    };
    unsafe {
        FillRect(hdc, &bottom_border, border_brush);
    }
    let left_border = RECT {
        left: client.left,
        top: client.top,
        right: client.left + 1,
        bottom: client.bottom,
    };
    unsafe {
        FillRect(hdc, &left_border, border_brush);
    }
    let right_border = RECT {
        left: client.right - 1,
        top: client.top,
        right: client.right,
        bottom: client.bottom,
    };
    unsafe {
        FillRect(hdc, &right_border, border_brush);
    }
    unsafe {
        let _ = DeleteObject(border_brush.into());
    }

    unsafe {
        SetBkMode(hdc, TRANSPARENT);
    }

    let text_cr = if is_dark {
        COLORREF(0x00FFFFFF)
    } else {
        COLORREF(0x00202020)
    };
    unsafe {
        SetTextColor(hdc, text_cr);
    }

    let default_font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
    let old_font = unsafe { SelectObject(hdc, default_font) };

    let hover_button = state.pointer.hover_button();
    let pressed_button = state.pointer.pressed_button();
    let drag_source = state.pointer.dragging_reorder().map(|(src, _ins)| src);

    for (i, btn) in state.buttons.iter().enumerate() {
        let is_hover = hover_button == Some(i);
        let is_pressed = pressed_button == Some(i);
        let is_dragging_source = drag_source == Some(i);

        if is_dragging_source {
            // Don't draw hover/pressed highlight for the dragged button.
        } else if is_pressed {
            let hl = if is_dark {
                COLORREF(0x00505050)
            } else {
                COLORREF(0x00D0D0D0)
            };
            let hbr = unsafe { CreateSolidBrush(hl) };
            unsafe {
                FillRect(hdc, &rect_to_win32(btn.rect), hbr);
            }
            unsafe {
                let _ = DeleteObject(hbr.into());
            }
        } else if is_hover {
            let hl = if is_dark {
                COLORREF(0x003D3D3D)
            } else {
                COLORREF(0x00E0E0E0)
            };
            let hbr = unsafe { CreateSolidBrush(hl) };
            unsafe {
                FillRect(hdc, &rect_to_win32(btn.rect), hbr);
            }
            unsafe {
                let _ = DeleteObject(hbr.into());
            }
        }

        let label = if btn.is_add {
            "+".to_string()
        } else {
            format!("\u{1F4C1} {}", btn.folder.name)
        };

        // Dim text for the button being dragged.
        let text_cr_this = if is_dragging_source {
            if is_dark {
                COLORREF(0x00808080)
            } else {
                COLORREF(0x00A0A0A0)
            }
        } else {
            text_cr
        };
        unsafe {
            SetTextColor(hdc, text_cr_this);
        }

        let mut label_wide: Vec<u16> = label.encode_utf16().collect();
        let mut draw_rect = rect_to_win32(btn.rect);
        let flags = if btn.is_add {
            DT_SINGLELINE | DT_VCENTER | DT_CENTER
        } else {
            DT_SINGLELINE | DT_VCENTER
        };
        if !btn.is_add {
            draw_rect.left += theme::scale(BTN_PAD_H, state.dpi);
        }
        unsafe {
            DrawTextW(hdc, &mut label_wide, &mut draw_rect, flags);
        }
    }

    if !old_font.is_invalid() {
        unsafe {
            SelectObject(hdc, old_font);
        }
    }

    // Reorder insertion caret (horizontal layout only).
    if let Some((_src, insertion)) = state.pointer.dragging_reorder()
        && state.layout == Orientation::Horizontal
    {
        let folder_buttons: Vec<&ButtonLayout> =
            state.buttons.iter().filter(|b| !b.is_add).collect();
        if !folder_buttons.is_empty() {
            // X coordinate of the caret.
            let caret_x = if insertion >= folder_buttons.len() {
                folder_buttons.last().unwrap().rect.right + 1
            } else {
                folder_buttons[insertion].rect.left - 1
            };
            let caret_w = theme::scale(2, state.dpi);
            let caret_color = if is_dark {
                COLORREF(0x00A0A0FF)
            } else {
                COLORREF(0x004040C0)
            };
            let caret_brush = unsafe { CreateSolidBrush(caret_color) };
            let caret_rect = RECT {
                left: caret_x,
                top: client.top + 2,
                right: caret_x + caret_w,
                bottom: client.bottom - 2,
            };
            unsafe {
                FillRect(hdc, &caret_rect, caret_brush);
            }
            unsafe {
                let _ = DeleteObject(caret_brush.into());
            }
        }
    }

    unsafe {
        let _ = EndPaint(hwnd, &ps);
    }
}

// ── Window procedure ─────────────────────────────────────────────────────────

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
            let (w, h) = compute_layout(hdc, state);
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
            let (final_x, final_y) =
                clamp_to_work_area(current_rect.left, current_rect.top, w, h, Some(hwnd));

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

            // Apply layered window transparency
            apply_opacity(hwnd, state);

            // Register drop target
            register_drop_targets(hwnd, state);

            // The foreground WinEvent fires reliably; GetForegroundWindow() does not.
            // Use state.active_explorer (set by foreground_event_proc before toolbar creation).
            let explorer_hwnd =
                state.active_explorer.unwrap_or_else(|| unsafe { GetForegroundWindow() });
            let class = crate::explorer::get_class_name(explorer_hwnd);
            if class == "CabinetWClass" {
                log::info!("toolbar create: showing above explorer={explorer_hwnd:?}");
                show_above(hwnd, explorer_hwnd);
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
            clear_global_toolbar();
            let _ = crate::dragdrop::unregister_drop_target(hwnd);
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ToolbarState;
            if !ptr.is_null() {
                // Cancel any active inline rename before freeing state.
                // SAFETY: ptr is non-null and state is still live at this point;
                // we zero the USERDATA slot and drop state below.
                cancel_inline_rename(unsafe { &mut *ptr });
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
                if in_grip(state, pt.x, pt.y) {
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
                    paint(hwnd, state);
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
            save_pos(x, y);
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
                            MENU_ID_EDIT_CONFIG => open_config_in_editor(),
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
                            MENU_ID_OPEN => {
                                if let Some(explorer) = state.active_explorer {
                                    crate::warn_on_err!(
                                        state.shell_browser.navigate(explorer, &path)
                                    );
                                }
                            }
                            MENU_ID_OPEN_NEW_TAB => {
                                let timeout = state
                                    .config
                                    .as_ref()
                                    .map(|c| c.new_tab_timeout_ms_zero_disables)
                                    .unwrap_or(500);
                                if let Some(explorer) = state.active_explorer {
                                    state
                                        .shell_browser
                                        .open_in_new_tab(explorer, &path, timeout);
                                }
                            }
                            MENU_ID_COPY_PATH => {
                                let folder_button = idx - 1; // + button at index 0
                                copy_folder_path_to_clipboard(state, folder_button);
                            }
                            MENU_ID_RENAME => {
                                let rect = rect_to_win32(state.buttons[idx].rect);
                                let name = state.buttons[idx].folder.name.clone();
                                let folder_index = idx - 1; // + button at index 0
                                start_inline_rename(hwnd, rect, folder_index, &name);
                            }
                            MENU_ID_REMOVE => {
                                remove_folder_at(state, hwnd, idx);
                            }
                            _ => {}
                        }
                    }
                }
            }
            LRESULT(0)
        }

        x if x == WM_USER_RELOAD => {
            refresh_toolbar(hwnd);
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
                let (w, h) = compute_layout(hdc, state);
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

unsafe extern "system" fn toolbar_wndproc_safe(
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

// ── Opacity ───────────────────────────────────────────────────────────────────

fn apply_opacity(hwnd: HWND, state: &ToolbarState) {
    let opacity = state.config.as_ref().map_or(0.8, |c| c.background_opacity);
    let alpha = (opacity.clamp(0.0, 1.0) * 255.0) as u8;
    unsafe {
        crate::warn_on_err!(SetLayeredWindowAttributes(
            hwnd,
            COLORREF(0),
            alpha,
            LWA_ALPHA
        ));
    }
}

// ── Drop target registration ─────────────────────────────────────────────────

fn register_drop_targets(hwnd: HWND, state: &mut ToolbarState) {
    if state.drop_registered {
        return;
    }

    // Capture everything needed for the closure (must be Send+Sync, no borrows on state).
    #[derive(Clone)]
    struct Info {
        rect: RECT,
        action: ActionSource,
    }
    #[derive(Clone)]
    enum ActionSource {
        Folder(String),
        Add,
    }

    let button_info: Vec<Info> = state
        .buttons
        .iter()
        .map(|b| Info {
            rect: rect_to_win32(b.rect),
            action: if b.is_add {
                ActionSource::Add
            } else {
                ActionSource::Folder(b.folder.path.clone())
            },
        })
        .collect();

    let resolver = move |cx: i32, cy: i32| -> Option<crate::dragdrop::DropAction> {
        let hit = button_info.iter().find(|i| {
            cx >= i.rect.left && cx < i.rect.right && cy >= i.rect.top && cy < i.rect.bottom
        })?;
        Some(match &hit.action {
            ActionSource::Folder(p) => crate::dragdrop::DropAction::MoveCopyTo {
                target: std::path::PathBuf::from(p),
            },
            ActionSource::Add => crate::dragdrop::DropAction::AddFolder,
        })
    };

    if crate::dragdrop::register_drop_target(
        hwnd,
        Box::new(resolver),
        std::sync::Arc::clone(&state.file_operator),
    )
    .is_ok()
    {
        state.drop_registered = true;
        log::info!("Registered OLE drop target on toolbar");
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

pub fn create_toolbar(
    owner: HWND,
    screen_pos: &RECT,
    hinstance: windows::Win32::Foundation::HINSTANCE,
) -> Option<HWND> {
    CLASS_REGISTERED.call_once(|| {
        let class_wide: Vec<u16> = wide_null(CLASS_NAME);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(toolbar_wndproc_safe),
            cbClsExtra: 0,
            cbWndExtra: std::mem::size_of::<*mut ToolbarState>() as i32,
            hInstance: hinstance,
            lpszClassName: PCWSTR(class_wide.as_ptr()),
            ..Default::default()
        };
        unsafe { RegisterClassExW(&wc) };
    });

    let dpi = theme::get_dpi(owner);
    // Bootstrap: no ToolbarState exists yet, so the `config_store` trait seam
    // is unavailable. This is the only direct Config::load() call in the
    // runtime — ToolbarState::new() hands the loaded Config off and all
    // subsequent mutations route through state.config_store.
    let config = Config::load();
    let is_dark = theme::is_dark_mode();
    log::info!("create_toolbar: dark_mode={is_dark}");

    let state = Box::new(ToolbarState::new(dpi, config));
    // SAFETY: Box::into_raw transfers ownership to the CreateWindowExW lpCreateParams
    // slot, which Win32 delivers to WM_CREATE as cs.lpCreateParams. If window
    // creation fails, the Err branch below reclaims the box via Box::from_raw.
    let state_ptr = Box::into_raw(state);

    let class_wide: Vec<u16> = wide_null(CLASS_NAME);

    // Determine initial window position: saved pos > default pos
    let (mut x, mut y) = load_saved_pos().unwrap_or((screen_pos.left, screen_pos.top));

    // Rough placeholder size for clamping; resized in WM_CREATE.
    // Clamp using the monitor that contains the triggering Explorer window.
    let placeholder_w = 400;
    let placeholder_h = 30;
    let clamped = clamp_to_work_area(x, y, placeholder_w, placeholder_h, Some(owner));
    x = clamped.0;
    y = clamped.1;

    log::info!("create_toolbar: screen x={x} y={y}");

    // Create as a TOP-LEVEL popup (no owner) so it survives individual
    // Explorer window closures. The `owner` HWND is used for monitor
    // detection only, not as the parent/owner.
    let hwnd_result = unsafe {
        // WS_EX_NOACTIVATE: the toolbar is a companion window — clicking it
        // must NOT steal foreground focus from Explorer, or folder clicks
        // end up routed to a newly-activated toolbar and navigation fails.
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            PCWSTR(class_wide.as_ptr()),
            PCWSTR::null(),
            WS_POPUP | WS_VISIBLE,
            x,
            y,
            placeholder_w,
            placeholder_h,
            None, // no owner — independent top-level window
            None,
            Some(hinstance),
            Some(state_ptr as *const _ as *const std::ffi::c_void),
        )
    };

    // Prevent "unused" warning when the owner only informs monitor choice.
    let _ = owner;

    match hwnd_result {
        Ok(hwnd) => {
            set_global_toolbar(hwnd);
            Some(hwnd)
        }
        Err(_) => {
            // SAFETY: CreateWindowExW never called WM_CREATE (window failed to
            // create), so the pointer was not handed off to the window; we
            // reclaim it here to avoid a leak.
            drop(unsafe { Box::from_raw(state_ptr) });
            None
        }
    }
}

pub fn refresh_toolbar(hwnd: HWND) {
    let Some(state) = (unsafe { toolbar_state(hwnd) }) else {
        return;
    };
    state.config = state.config_store.load();
    state.layout = state
        .config
        .as_ref()
        .map_or(Orientation::Horizontal, |c| c.layout);

    // Re-apply opacity in case config changed.
    apply_opacity(hwnd, state);

    let hdc = unsafe { GetDC(Some(hwnd)) };
    let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
    let old_font = unsafe { SelectObject(hdc, font) };
    let (w, h) = compute_layout(hdc, state);
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
            SWP_NOZORDER | SWP_NOACTIVATE | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE,
        ));
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

/// Append a folder to `~/.exbar.json` using its basename as the label, then reload.
/// No-op on empty / invalid paths.
fn append_folder_and_reload(state: &mut ToolbarState, path: &std::path::Path) {
    let name = match path.file_name().and_then(|s| s.to_str()) {
        Some(n) if !n.is_empty() => n.to_owned(),
        _ => return,
    };
    let path_str = match path.to_str() {
        Some(s) => s.to_owned(),
        None => return,
    };

    // Load → mutate → save. If load fails (no file yet), start from a minimal config.
    let mut cfg = state.config_store.load().unwrap_or_else(|| {
        crate::config::Config::from_str(r#"{"folders":[]}"#).expect("default config parses")
    });
    cfg.add_folder(name, path_str);
    if let Err(e) = state.config_store.save(&cfg) {
        log::error!("append_folder_and_reload: save failed: {e}");
        return;
    }
    state.config = Some(cfg);

    if let Some(hwnd) = get_global_toolbar_hwnd() {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_USER_RELOAD, WPARAM(0), LPARAM(0));
        }
    }
}

/// Drop-target entry point: looks up the global toolbar state and delegates to
/// `append_folder_and_reload`. Called from `dragdrop.rs` which has no direct access to state.
pub(crate) fn append_folder_and_reload_global(path: &std::path::Path) {
    let hwnd = match get_global_toolbar_hwnd() {
        Some(h) => h,
        None => return,
    };
    if let Some(state) = unsafe { toolbar_state(hwnd) } {
        append_folder_and_reload(state, path);
    }
}

fn open_config_in_editor() {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let path = crate::config::default_config_path();
    let path_wide: Vec<u16> = wide_null(&path);
    let verb_wide: Vec<u16> = wide_null("open");

    unsafe {
        let _ = ShellExecuteW(
            None,
            PCWSTR(verb_wide.as_ptr()),
            PCWSTR(path_wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn copy_folder_path_to_clipboard(state: &mut ToolbarState, folder_button: usize) {
    let btn_slot = folder_button + 1;
    if btn_slot < state.buttons.len() {
        let path = state.buttons[btn_slot].folder.path.clone();
        crate::warn_on_err!(state.clipboard.set_text(&path));
    }
}

fn commit_reorder(state: &mut ToolbarState, hwnd: HWND, from: usize, to: usize) {
    let mut cfg = match state.config_store.load() {
        Some(c) => c,
        None => {
            log::error!("commit_reorder: config load failed");
            return;
        }
    };
    cfg.move_folder(from, to);
    if let Err(e) = state.config_store.save(&cfg) {
        log::error!("commit_reorder: save failed: {e}");
        return;
    }
    state.config = Some(cfg);
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_USER_RELOAD, WPARAM(0), LPARAM(0));
    }
}

fn remove_folder_at(state: &mut ToolbarState, hwnd: HWND, index: usize) {
    let mut cfg = match state.config_store.load() {
        Some(c) => c,
        None => return,
    };
    // The toolbar's button index includes the + button at position 0; adjust.
    if index == 0 {
        return;
    } // safety: + button never reaches here (is_add branch)
    let folder_index = index - 1;
    cfg.remove_folder(folder_index);
    if let Err(e) = state.config_store.save(&cfg) {
        log::error!("remove_folder_at: save failed: {e}");
        return;
    }
    state.config = Some(cfg);
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_USER_RELOAD, WPARAM(0), LPARAM(0));
    }
}

// ── Inline rename ───────────────────────────────────────────────────────────

struct RenameState {
    edit_hwnd: isize,
    /// Raw `Box<RenameSubclassData>` pointer handed to `SetWindowSubclass`.
    /// Stored so `cancel_inline_rename` can reclaim the Box on parent teardown.
    /// `folder_index` and `toolbar_hwnd` live in the Box; the subclass proc
    /// reads them from `ref_data`.
    subclass_data: usize,
}

fn start_inline_rename(toolbar: HWND, button_rect: RECT, folder_index: usize, initial_name: &str) {
    // Cancel any existing rename first.
    // SAFETY: toolbar is the toolbar HWND; we are on the message-pump thread.
    let Some(state) = (unsafe { toolbar_state(toolbar) }) else { return; };
    cancel_inline_rename(state);

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

    // Select all text
    const EM_SETSEL: u32 = 0x00B1;
    unsafe {
        SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
        let _ = SetFocus(Some(edit));
    }

    // Subclass for Enter/Esc/KillFocus.
    // SAFETY: Box::into_raw leaks the allocation into SetWindowSubclass's ref_data.
    // Ownership is reclaimed by Box::from_raw in commit_rename / cancel_rename
    // (called from the subclass proc) or in cancel_inline_rename (WM_DESTROY path).
    let data: *mut RenameSubclassData = Box::into_raw(Box::new(RenameSubclassData {
        toolbar_hwnd: toolbar.0 as isize,
        folder_index,
    }));
    unsafe {
        use windows::Win32::UI::Shell::SetWindowSubclass;
        crate::warn_on_err!(
            SetWindowSubclass(edit, Some(rename_subclass_proc), 1, data as usize).ok()
        );
    }

    // SAFETY: toolbar_state called at the top of this function; we are still on the
    // message-pump thread and no other code mutated GWLP_USERDATA since that call.
    if let Some(state) = unsafe { toolbar_state(toolbar) } {
        state.rename_state = Some(RenameState {
            edit_hwnd: edit.0 as isize,
            subclass_data: data as usize,
        });
    }
}

struct RenameSubclassData {
    toolbar_hwnd: isize,
    folder_index: usize,
}

unsafe extern "system" fn rename_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    ref_data: usize,
) -> LRESULT {
    use windows::Win32::UI::Shell::DefSubclassProc;

    const VK_RETURN: usize = 0x0D;
    const VK_ESCAPE: usize = 0x1B;

    match msg {
        WM_GETDLGCODE => {
            return LRESULT(DLGC_WANTALLKEYS as isize);
        }
        WM_KEYDOWN => {
            let vk = wparam.0;
            if vk == VK_RETURN {
                commit_rename(hwnd, ref_data);
                return LRESULT(0);
            }
            if vk == VK_ESCAPE {
                cancel_rename(hwnd, ref_data);
                return LRESULT(0);
            }
        }
        WM_KILLFOCUS => {
            commit_rename(hwnd, ref_data);
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

fn read_edit_text(edit: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(edit) } as usize;
    let mut buf = vec![0u16; len + 1];
    let got = unsafe { GetWindowTextW(edit, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..got])
}

fn commit_rename(edit: HWND, ref_data: usize) {
    // SAFETY: ref_data is the pointer produced by Box::into_raw in start_inline_rename;
    // the subclass proc is invoked on the toolbar message-pump thread, and this is
    // the single reclaim point for the Enter / WM_KILLFOCUS paths.
    let data = unsafe { Box::from_raw(ref_data as *mut RenameSubclassData) };
    let toolbar = HWND(data.toolbar_hwnd as *mut _);
    let text = read_edit_text(edit);

    if let Some(state) = unsafe { toolbar_state(toolbar) }
        && let Some(mut cfg) = state.config_store.load()
    {
        cfg.rename_folder(data.folder_index, text);
        if let Err(e) = state.config_store.save(&cfg) {
            log::error!("commit_rename: save failed: {e}");
        } else {
            state.config = Some(cfg);
        }
    }

    destroy_rename_edit(edit);
    // Clear rename_state from ToolbarState.
    // SAFETY: commit_rename is called from the EDIT subclass wndproc (same message-pump
    // thread as the toolbar); toolbar_state's single-thread invariant is satisfied.
    if let Some(state) = unsafe { toolbar_state(toolbar) } {
        state.rename_state = None;
    }
    unsafe {
        let _ = PostMessageW(Some(toolbar), WM_USER_RELOAD, WPARAM(0), LPARAM(0));
    }
}

fn cancel_rename(edit: HWND, ref_data: usize) {
    // SAFETY: Same provenance as commit_rename; this is the Esc path reclaim point.
    let data = unsafe { Box::from_raw(ref_data as *mut RenameSubclassData) };
    let toolbar = HWND(data.toolbar_hwnd as *mut _);
    let _ = data;
    destroy_rename_edit(edit);
    // Clear rename_state from ToolbarState.
    // SAFETY: cancel_rename is called from the EDIT subclass wndproc on the toolbar's
    // message-pump thread; toolbar_state's single-thread invariant is satisfied.
    if let Some(state) = unsafe { toolbar_state(toolbar) } {
        state.rename_state = None;
    }
}

fn destroy_rename_edit(edit: HWND) {
    use windows::Win32::UI::Shell::RemoveWindowSubclass;
    unsafe {
        let _ = RemoveWindowSubclass(edit, Some(rename_subclass_proc), 1);
        let _ = DestroyWindow(edit);
    }
}

fn cancel_inline_rename(state: &mut ToolbarState) {
    if let Some(s) = state.rename_state.take() {
        let edit = HWND(s.edit_hwnd as *mut _);
        destroy_rename_edit(edit);
        // Reclaim the Box leaked into SetWindowSubclass; RemoveWindowSubclass
        // inside destroy_rename_edit ran before this, so no callback can race.
        if s.subclass_data != 0 {
            // SAFETY: subclass_data holds the Box::into_raw pointer from start_inline_rename.
            // RemoveWindowSubclass (inside destroy_rename_edit above) has already run,
            // so the subclass proc cannot fire again and race this reclaim.
            unsafe {
                drop(Box::from_raw(s.subclass_data as *mut RenameSubclassData));
            }
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::Clipboard;
    use crate::config::{Config, ConfigStore, FolderEntry};
    use crate::dragdrop::FileOperator;
    use crate::error::ExbarResult;
    use crate::layout::{ButtonLayout, Rect};
    use crate::picker::FolderPicker;
    use crate::pointer;
    use crate::shell_windows::ShellBrowser;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use windows::Win32::Foundation::HWND;

    // ── Mocks ────────────────────────────────────────────────────────────

    struct MockShellBrowser {
        navigate_calls: Arc<Mutex<Vec<(isize, PathBuf)>>>,
        new_tab_calls: Arc<Mutex<Vec<(isize, PathBuf, u32)>>>,
    }

    impl ShellBrowser for MockShellBrowser {
        fn navigate(&self, explorer: HWND, path: &Path) -> ExbarResult<()> {
            self.navigate_calls
                .lock()
                .unwrap()
                .push((explorer.0 as isize, path.to_path_buf()));
            Ok(())
        }
        fn open_in_new_tab(&self, explorer: HWND, path: &Path, timeout_ms: u32) {
            self.new_tab_calls.lock().unwrap().push((
                explorer.0 as isize,
                path.to_path_buf(),
                timeout_ms,
            ));
        }
    }

    #[derive(Default)]
    struct MockFolderPicker {
        next_result: Mutex<Option<PathBuf>>,
        calls: Mutex<u32>,
    }
    impl FolderPicker for MockFolderPicker {
        fn pick_folder(&self) -> Option<PathBuf> {
            *self.calls.lock().unwrap() += 1;
            self.next_result.lock().unwrap().clone()
        }
    }

    #[derive(Default)]
    struct MockFileOp {
        move_calls: Mutex<Vec<(Vec<PathBuf>, PathBuf)>>,
        copy_calls: Mutex<Vec<(Vec<PathBuf>, PathBuf)>>,
    }
    impl FileOperator for MockFileOp {
        fn move_items(&self, sources: &[PathBuf], target: &Path) -> ExbarResult<()> {
            self.move_calls
                .lock()
                .unwrap()
                .push((sources.to_vec(), target.to_path_buf()));
            Ok(())
        }
        fn copy_items(&self, sources: &[PathBuf], target: &Path) -> ExbarResult<()> {
            self.copy_calls
                .lock()
                .unwrap()
                .push((sources.to_vec(), target.to_path_buf()));
            Ok(())
        }
    }

    #[derive(Default)]
    struct MockClipboard {
        set_text_calls: Mutex<Vec<String>>,
    }
    impl Clipboard for MockClipboard {
        fn set_text(&self, text: &str) -> ExbarResult<()> {
            self.set_text_calls.lock().unwrap().push(text.to_string());
            Ok(())
        }
    }

    #[derive(Default)]
    struct MockConfigStore {
        load_value: Mutex<Option<Config>>,
        save_calls: Mutex<Vec<Config>>,
    }
    impl ConfigStore for MockConfigStore {
        fn load(&self) -> Option<Config> {
            self.load_value.lock().unwrap().clone()
        }
        fn save(&self, config: &Config) -> ExbarResult<()> {
            self.save_calls.lock().unwrap().push(config.clone());
            Ok(())
        }
    }

    // Newtypes to bridge Arc<Concrete> → Box<dyn Trait>.
    struct PickerArc(Arc<MockFolderPicker>);
    impl FolderPicker for PickerArc {
        fn pick_folder(&self) -> Option<PathBuf> {
            self.0.pick_folder()
        }
    }
    struct ClipArc(Arc<MockClipboard>);
    impl Clipboard for ClipArc {
        fn set_text(&self, t: &str) -> ExbarResult<()> {
            self.0.set_text(t)
        }
    }
    struct CfgArc(Arc<MockConfigStore>);
    impl ConfigStore for CfgArc {
        fn load(&self) -> Option<Config> {
            self.0.load()
        }
        fn save(&self, c: &Config) -> ExbarResult<()> {
            self.0.save(c)
        }
    }

    struct TestDeps {
        navigate_calls: Arc<Mutex<Vec<(isize, PathBuf)>>>,
        new_tab_calls: Arc<Mutex<Vec<(isize, PathBuf, u32)>>>,
        picker: Arc<MockFolderPicker>,
        file_op: Arc<MockFileOp>,
        clipboard: Arc<MockClipboard>,
        cfg_store: Arc<MockConfigStore>,
    }

    fn mk_deps() -> TestDeps {
        TestDeps {
            navigate_calls: Arc::default(),
            new_tab_calls: Arc::default(),
            picker: Arc::new(MockFolderPicker::default()),
            file_op: Arc::new(MockFileOp::default()),
            clipboard: Arc::new(MockClipboard::default()),
            cfg_store: Arc::new(MockConfigStore::default()),
        }
    }

    fn make_test_state(deps: &TestDeps, config: Option<Config>) -> ToolbarState {
        let shell = MockShellBrowser {
            navigate_calls: Arc::clone(&deps.navigate_calls),
            new_tab_calls: Arc::clone(&deps.new_tab_calls),
        };
        ToolbarState::with_deps(
            96,
            config,
            Box::new(shell),
            Box::new(PickerArc(deps.picker.clone())),
            deps.file_op.clone() as Arc<dyn FileOperator>,
            Box::new(ClipArc(deps.clipboard.clone())),
            Box::new(CfgArc(deps.cfg_store.clone())),
        )
    }

    fn mk_add_button() -> ButtonLayout {
        ButtonLayout {
            rect: Rect {
                left: 0,
                top: 0,
                right: 40,
                bottom: 28,
            },
            folder: FolderEntry {
                name: "+".into(),
                path: String::new(),
                icon: None,
            },
            is_add: true,
        }
    }

    fn mk_folder_button(name: &str, path: &str, left: i32) -> ButtonLayout {
        ButtonLayout {
            rect: Rect {
                left,
                top: 0,
                right: left + 90,
                bottom: 28,
            },
            folder: FolderEntry {
                name: name.into(),
                path: path.into(),
                icon: None,
            },
            is_add: false,
        }
    }

    fn mk_config_with_folders(entries: &[(&str, &str)]) -> Config {
        // Paths may contain backslashes; JSON-encode them so the parser
        // doesn't see unescaped control characters.
        let folders_json: Vec<String> = entries
            .iter()
            .map(|(n, p)| {
                let escaped_path = p.replace('\\', "\\\\");
                format!(r#"{{"name":"{n}","path":"{escaped_path}"}}"#)
            })
            .collect();
        let json = format!(r#"{{"folders":[{}]}}"#, folders_json.join(","));
        Config::from_str(&json).unwrap()
    }

    // ── Tests ────────────────────────────────────────────────────────────

    #[test]
    fn fire_folder_click_without_ctrl_calls_navigate_with_folder_path() {
        let deps = mk_deps();
        let cfg = mk_config_with_folders(&[("Downloads", "C:\\Downloads")]);
        let mut state = make_test_state(&deps, Some(cfg));
        state.active_explorer = Some(HWND(42 as *mut _));
        state.buttons = vec![
            mk_add_button(),
            mk_folder_button("Downloads", "C:\\Downloads", 42),
        ];

        state.execute_pointer_command(
            HWND(std::ptr::dangling_mut()),
            pointer::PointerCommand::FireFolderClick {
                folder_button: 0,
                ctrl: false,
            },
        );

        let calls = deps.navigate_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, PathBuf::from("C:\\Downloads"));
        assert_eq!(deps.new_tab_calls.lock().unwrap().len(), 0);
    }

    #[test]
    fn fire_folder_click_with_ctrl_calls_open_in_new_tab_with_configured_timeout() {
        let deps = mk_deps();
        let cfg = Config::from_str(
            r#"{"folders":[{"name":"D","path":"C:\\D"}],"newTabTimeoutMsZeroDisables":750}"#,
        )
        .unwrap();
        let mut state = make_test_state(&deps, Some(cfg));
        state.active_explorer = Some(HWND(42 as *mut _));
        state.buttons = vec![mk_add_button(), mk_folder_button("D", "C:\\D", 42)];

        state.execute_pointer_command(
            HWND(std::ptr::dangling_mut()),
            pointer::PointerCommand::FireFolderClick {
                folder_button: 0,
                ctrl: true,
            },
        );

        let calls = deps.new_tab_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].2, 750);
        assert_eq!(deps.navigate_calls.lock().unwrap().len(), 0);
    }

    #[test]
    fn fire_folder_click_when_no_active_explorer_is_noop() {
        let deps = mk_deps();
        let cfg = mk_config_with_folders(&[("D", "C:\\D")]);
        let mut state = make_test_state(&deps, Some(cfg));
        state.buttons = vec![mk_add_button(), mk_folder_button("D", "C:\\D", 42)];
        // active_explorer intentionally left None.

        state.execute_pointer_command(
            HWND(std::ptr::dangling_mut()),
            pointer::PointerCommand::FireFolderClick { folder_button: 0, ctrl: false },
        );

        assert!(deps.navigate_calls.lock().unwrap().is_empty(),
            "navigate should not be called when no active explorer");
        assert!(deps.new_tab_calls.lock().unwrap().is_empty(),
            "open_in_new_tab should not be called when no active explorer");
    }

    #[test]
    fn fire_add_click_when_picker_returns_some_appends_and_saves() {
        let deps = mk_deps();
        *deps.picker.next_result.lock().unwrap() = Some(PathBuf::from("C:\\NewFolder"));
        *deps.cfg_store.load_value.lock().unwrap() = Some(mk_config_with_folders(&[]));

        let mut state = make_test_state(&deps, None);
        state.execute_pointer_command(
            HWND(std::ptr::dangling_mut()),
            pointer::PointerCommand::FireAddClick,
        );

        assert_eq!(*deps.picker.calls.lock().unwrap(), 1);
        let saves = deps.cfg_store.save_calls.lock().unwrap();
        assert_eq!(saves.len(), 1);
        assert_eq!(saves[0].folders.len(), 1);
        assert_eq!(saves[0].folders[0].path, "C:\\NewFolder");
    }

    #[test]
    fn fire_add_click_when_picker_returns_none_is_noop() {
        let deps = mk_deps();
        *deps.picker.next_result.lock().unwrap() = None;
        let mut state = make_test_state(&deps, None);
        state.execute_pointer_command(
            HWND(std::ptr::dangling_mut()),
            pointer::PointerCommand::FireAddClick,
        );

        assert_eq!(*deps.picker.calls.lock().unwrap(), 1);
        assert_eq!(deps.cfg_store.save_calls.lock().unwrap().len(), 0);
    }

    #[test]
    fn commit_reorder_loads_modifies_saves_via_config_store() {
        let deps = mk_deps();
        *deps.cfg_store.load_value.lock().unwrap() = Some(mk_config_with_folders(&[
            ("A", "C:\\a"),
            ("B", "C:\\b"),
            ("C", "C:\\c"),
        ]));

        let mut state = make_test_state(&deps, None);
        state.execute_pointer_command(
            HWND(std::ptr::dangling_mut()),
            pointer::PointerCommand::CommitReorder {
                from_folder: 0,
                to_folder: 3,
            },
        );

        let saves = deps.cfg_store.save_calls.lock().unwrap();
        assert_eq!(saves.len(), 1);
        let names: Vec<&str> = saves[0].folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["B", "C", "A"]);
    }

    #[test]
    fn copy_folder_path_calls_clipboard_set_text_with_folder_path() {
        let deps = mk_deps();
        let mut state = make_test_state(&deps, None);
        state.buttons = vec![
            mk_add_button(),
            mk_folder_button("Target", "C:\\Target", 42),
        ];

        copy_folder_path_to_clipboard(&mut state, 0);

        let calls = deps.clipboard.set_text_calls.lock().unwrap();
        assert_eq!(*calls, vec!["C:\\Target".to_string()]);
    }
}
