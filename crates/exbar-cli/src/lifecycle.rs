//! Toolbar window lifecycle: registration, creation, refresh,
//! drop-target wiring, opacity. The "what to do once we have an
//! Explorer to attach to" module — distinct from `visibility.rs`
//! (which decides *when* to attach).

use std::sync::Once;

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    DEFAULT_GUI_FONT, GetDC, GetStockObject, InvalidateRect, ReleaseDC, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, LWA_ALPHA, RegisterClassExW, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOZORDER, SetLayeredWindowAttributes, SetWindowPos, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
};

use crate::config::Config;
use crate::theme;
use crate::toolbar::{ToolbarState, toolbar_state, wide_null};

// ── Constants ────────────────────────────────────────────────────────────────

static CLASS_REGISTERED: Once = Once::new();
pub(crate) const CLASS_NAME: &str = "ExbarToolbar";

// ── Module handle ─────────────────────────────────────────────────────────────

/// Returns the HINSTANCE for the running executable.
///
/// Used when registering the window class and creating windows — must be
/// the same instance that owns the wndproc, i.e. `exbar.exe` itself (not
/// any injected DLL).
pub fn exe_hinstance() -> HINSTANCE {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    let hmod = unsafe { GetModuleHandleW(windows_core::PCWSTR::null()) }
        .unwrap_or(HMODULE(std::ptr::null_mut()));
    HINSTANCE(hmod.0)
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
        rect: windows::Win32::Foundation::RECT,
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
            rect: crate::paint::rect_to_win32(b.rect),
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

    match crate::dragdrop::register_drop_target(
        hwnd,
        Box::new(resolver),
        std::sync::Arc::clone(&state.file_operator),
    ) {
        Ok(()) => {
            state.drop_registered = true;
            log::info!("Registered OLE drop target on toolbar");
        }
        Err(e) => {
            log::error!("RegisterDragDrop failed: {e}");
        }
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Create the top-level popup toolbar window above `owner`.
///
/// `owner` is the triggering Explorer (`CabinetWClass`) HWND — used for
/// monitor / DPI detection. Returns the new toolbar HWND on success.
/// See ADR-0002 for why this is a top-level popup, not a child window.
pub fn create_toolbar(owner: HWND, screen_pos: &RECT, hinstance: HINSTANCE) -> Option<HWND> {
    CLASS_REGISTERED.call_once(|| {
        let class_wide: Vec<u16> = wide_null(CLASS_NAME);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(crate::wndproc::toolbar_wndproc_safe),
            cbClsExtra: 0,
            cbWndExtra: std::mem::size_of::<*mut ToolbarState>() as i32,
            hInstance: hinstance,
            lpszClassName: windows_core::PCWSTR(class_wide.as_ptr()),
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

    let mut state = Box::new(ToolbarState::new(dpi, config));
    // Seed active_target with the triggering cabinet HWND. WM_CREATE runs
    // synchronously inside CreateWindowExW, so we can't set this after creation;
    // seeding the Box before into_raw guarantees WM_CREATE observes it.
    state.active_target = Some(crate::target::ActiveTarget::explorer(owner));
    // SAFETY: Box::into_raw transfers ownership to the CreateWindowExW lpCreateParams
    // slot, which Win32 delivers to WM_CREATE as cs.lpCreateParams. If window
    // creation fails, the Err branch below reclaims the box via Box::from_raw.
    let state_ptr = Box::into_raw(state);

    let class_wide: Vec<u16> = wide_null(CLASS_NAME);

    // Determine initial window position: saved offset > default pos
    let (origin_x, origin_y) = crate::position::explorer_visible_origin(owner);
    let (mut x, mut y) =
        match crate::position::load_saved_offset(crate::target::TargetKind::Explorer) {
            Some((ox, oy)) => crate::position::apply_offset(ox, oy, origin_x, origin_y),
            None => (screen_pos.left, screen_pos.top),
        };

    // Rough placeholder size for clamping; resized in WM_CREATE.
    // Clamp using the monitor that contains the triggering Explorer window.
    let placeholder_w = 400;
    let placeholder_h = 30;
    let clamped =
        crate::position::clamp_to_work_area_for(x, y, placeholder_w, placeholder_h, Some(owner));
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
            windows_core::PCWSTR(class_wide.as_ptr()),
            windows_core::PCWSTR::null(),
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
            crate::visibility::set_global_toolbar(hwnd);
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

/// Reload `~/.exbar.json` from disk for `hwnd`'s `ToolbarState` and recompute the layout.
pub fn refresh_toolbar(hwnd: HWND) {
    let Some(state) = (unsafe { toolbar_state(hwnd) }) else {
        return;
    };
    state.config = state.config_store.load();
    state.layout = state
        .config
        .as_ref()
        .map_or(crate::config::Orientation::Horizontal, |c| c.layout);

    // Re-apply opacity in case config changed.
    apply_opacity(hwnd, state);

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
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOMOVE,
        ));
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

/// Apply `apply_opacity` and `register_drop_targets` to the toolbar window.
///
/// Called from `WM_CREATE` via `crate::toolbar::toolbar_wndproc`.
pub(crate) fn setup_on_create(hwnd: HWND, state: &mut ToolbarState) {
    apply_opacity(hwnd, state);
    register_drop_targets(hwnd, state);
}
