//! Toolbar window position: persistence (~/.exbar.pos.json) and
//! work-area clamping. The pure `clamp_to_work_area` is the only
//! testable surface; the rest is Win32 `SystemParametersInfoW` /
//! filesystem I/O.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedPos {
    x: i32,
    y: i32,
}

pub(crate) fn pos_file_path() -> std::path::PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| "C:\\Users\\Default".into());
    let mut p = std::path::PathBuf::from(home);
    p.push(".exbar-pos.json");
    p
}

pub(crate) fn load_saved_pos() -> Option<(i32, i32)> {
    let bytes = std::fs::read(pos_file_path()).ok()?;
    let saved: SavedPos = serde_json::from_slice(&bytes).ok()?;
    Some((saved.x, saved.y))
}

pub(crate) fn save_pos(x: i32, y: i32) {
    let saved = SavedPos { x, y };
    if let Ok(json) = serde_json::to_string(&saved) {
        let _ = std::fs::write(pos_file_path(), json);
    }
}

/// Return the work area of the monitor containing `ref_hwnd`, or the primary
/// monitor work area if that fails.
pub(crate) fn work_area_for(ref_hwnd: Option<HWND>) -> RECT {
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

/// Clamp `(x, y, w, h)` so the rectangle stays inside `work`. If the
/// rectangle is wider/taller than the work area, pin it to
/// `(work.left, work.top)`.
pub fn clamp_to_work_area(x: i32, y: i32, w: i32, h: i32, work: RECT) -> (i32, i32) {
    let work_w = work.right - work.left;
    let work_h = work.bottom - work.top;
    let cx = if w > work_w {
        work.left
    } else {
        x.clamp(work.left, work.right - w)
    };
    let cy = if h > work_h {
        work.top
    } else {
        y.clamp(work.top, work.bottom - h)
    };
    (cx, cy)
}

/// Convenience: look up the work area for `ref_hwnd` and clamp.
pub fn clamp_to_work_area_for(
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    ref_hwnd: Option<HWND>,
) -> (i32, i32) {
    clamp_to_work_area(x, y, w, h, work_area_for(ref_hwnd))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
        RECT { left: l, top: t, right: r, bottom: b }
    }

    #[test]
    fn in_bounds_passthrough() {
        let work = rect(0, 0, 1920, 1080);
        assert_eq!(clamp_to_work_area(100, 100, 400, 40, work), (100, 100));
    }

    #[test]
    fn off_right_clamps_to_right_edge() {
        let work = rect(0, 0, 1920, 1080);
        assert_eq!(clamp_to_work_area(2000, 100, 400, 40, work), (1520, 100));
    }

    #[test]
    fn off_bottom_clamps_to_bottom_edge() {
        let work = rect(0, 0, 1920, 1080);
        assert_eq!(clamp_to_work_area(100, 1100, 400, 40, work), (100, 1040));
    }

    #[test]
    fn negative_clamps_to_top_left() {
        let work = rect(0, 0, 1920, 1080);
        assert_eq!(clamp_to_work_area(-50, -50, 400, 40, work), (0, 0));
    }

    #[test]
    fn oversized_pins_to_top_left() {
        let work = rect(0, 0, 1920, 1080);
        assert_eq!(clamp_to_work_area(500, 500, 4000, 40, work), (0, 500));
        assert_eq!(clamp_to_work_area(500, 500, 400, 4000, work), (500, 0));
    }

    #[test]
    fn nonzero_origin_work_area() {
        // Multi-monitor: secondary monitor right of primary.
        let work = rect(1920, 0, 3840, 1080);
        assert_eq!(clamp_to_work_area(2000, 100, 400, 40, work), (2000, 100));
        assert_eq!(clamp_to_work_area(1800, 100, 400, 40, work), (1920, 100));
        assert_eq!(clamp_to_work_area(4000, 100, 400, 40, work), (3440, 100));
    }
}
