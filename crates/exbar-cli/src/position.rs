//! Toolbar window position: persistence (~/.exbar/position.json) and
//! work-area clamping. The pure `clamp_to_work_area` is the only
//! testable surface; the rest is Win32 `SystemParametersInfoW` /
//! filesystem I/O.
//!
//! The JSON file uses a per-kind schema (v1):
//! ```json
//! { "explorer": {"offset_x": 10, "offset_y": 20},
//!   "file_dialog": {"offset_x": 30, "offset_y": 40} }
//! ```
//! Older flat files (`{"offset_x": 5, "offset_y": 7}`) are promoted on load:
//! the flat value becomes both `explorer` and `file_dialog`.

use crate::target::TargetKind;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};

/// A single (offset_x, offset_y) pair stored per [`TargetKind`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedPos {
    pub offset_x: i32,
    pub offset_y: i32,
}

// ── JSON schema helpers ───────────────────────────────────────────────────────

/// On-disk v1 shape: both kinds present.
#[derive(serde::Serialize)]
struct PositionOut {
    explorer: SavedPos,
    file_dialog: SavedPos,
}

/// Untagged union: accepts the legacy flat object OR the v1 per-kind object.
///
/// `Flat` is tried first: it requires both `offset_x` and `offset_y`, so the
/// per-kind shape (`{"explorer": {...}}`) fails here and falls through to
/// `PerKind`. The flat shape (`{"offset_x": N, "offset_y": N}`) won't parse
/// as `PerKind` either direction, but we don't need it to — `Flat` catches it.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum PositionJson {
    Flat(SavedPos),
    PerKind {
        #[serde(default)]
        explorer: Option<SavedPos>,
        #[serde(default)]
        file_dialog: Option<SavedPos>,
    },
}

// ── PositionStore ─────────────────────────────────────────────────────────────

/// In-memory store of per-[`TargetKind`] offsets.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct PositionStore {
    explorer: SavedPos,
    file_dialog: SavedPos,
}

impl PositionStore {
    /// Return the saved offset for `kind`.
    pub(crate) fn offset(&self, kind: TargetKind) -> SavedPos {
        match kind {
            TargetKind::Explorer => self.explorer,
            TargetKind::FileDialog => self.file_dialog,
        }
    }

    /// Overwrite the saved offset for `kind`.
    pub(crate) fn set_offset(&mut self, kind: TargetKind, pos: SavedPos) {
        match kind {
            TargetKind::Explorer => self.explorer = pos,
            TargetKind::FileDialog => self.file_dialog = pos,
        }
    }

    /// Parse from a JSON string, accepting both v1 (per-kind) and legacy (flat).
    pub(crate) fn from_json_str(s: &str) -> Result<Self, serde_json::Error> {
        match serde_json::from_str::<PositionJson>(s)? {
            PositionJson::PerKind {
                explorer,
                file_dialog,
            } => {
                let ex = explorer.unwrap_or_default();
                Ok(Self {
                    explorer: ex,
                    file_dialog: file_dialog.unwrap_or(ex),
                })
            }
            PositionJson::Flat(p) => Ok(Self {
                explorer: p,
                file_dialog: p,
            }),
        }
    }

    /// Serialise to a pretty JSON string in the v1 per-kind schema.
    pub(crate) fn to_json_string(self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&PositionOut {
            explorer: self.explorer,
            file_dialog: self.file_dialog,
        })
    }
}

// ── Disk helpers ──────────────────────────────────────────────────────────────

pub(crate) fn pos_file_path() -> std::path::PathBuf {
    crate::paths::position_path()
}

/// Load the saved offset for `kind` from disk. Returns `None` on missing file
/// or parse error (callers fall back to a default position).
pub(crate) fn load_saved_offset(kind: TargetKind) -> Option<(i32, i32)> {
    let bytes = std::fs::read(pos_file_path()).ok()?;
    let s = std::str::from_utf8(&bytes).ok()?;
    let store = PositionStore::from_json_str(s).ok()?;
    let pos = store.offset(kind);
    Some((pos.offset_x, pos.offset_y))
}

/// Persist `offset_x/offset_y` for `kind`, preserving the other kind's value.
pub(crate) fn save_offset(kind: TargetKind, offset_x: i32, offset_y: i32) {
    // Load existing store so we don't clobber the other kind's offset.
    let mut store = std::fs::read(pos_file_path())
        .ok()
        .and_then(|bytes| std::str::from_utf8(&bytes).ok().map(|s| s.to_owned()))
        .and_then(|s| PositionStore::from_json_str(&s).ok())
        .unwrap_or_default();
    store.set_offset(kind, SavedPos { offset_x, offset_y });
    if let Ok(json) = store.to_json_string() {
        let path = pos_file_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, json);
    }
}

/// Get the window-rect origin of an Explorer window via `GetWindowRect`.
/// Uses raw window rect (including the invisible DWM border) rather than
/// `DWMWA_EXTENDED_FRAME_BOUNDS` because the border offset is inconsistent
/// between maximized (border hidden, rect starts at -8,-8) and restored
/// (border visible, extended bounds differ from rect by ~8px). Using
/// `GetWindowRect` consistently means the invisible border cancels out
/// when computing and applying offsets.
pub(crate) fn explorer_visible_origin(hwnd: HWND) -> (i32, i32) {
    let mut wr = RECT::default();
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut wr);
    }
    (wr.left, wr.top)
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

/// Compute the toolbar's offset relative to an Explorer window origin.
pub fn compute_offset(toolbar_x: i32, toolbar_y: i32, origin_x: i32, origin_y: i32) -> (i32, i32) {
    (toolbar_x - origin_x, toolbar_y - origin_y)
}

/// Apply a saved offset to an Explorer window origin to get screen coords.
pub fn apply_offset(offset_x: i32, offset_y: i32, origin_x: i32, origin_y: i32) -> (i32, i32) {
    (origin_x + offset_x, origin_y + offset_y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
        RECT {
            left: l,
            top: t,
            right: r,
            bottom: b,
        }
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

    #[test]
    fn compute_offset_basic() {
        assert_eq!(compute_offset(500, 200, 100, 100), (400, 100));
    }

    #[test]
    fn apply_offset_basic() {
        assert_eq!(apply_offset(400, 100, 100, 100), (500, 200));
    }

    #[test]
    fn offset_round_trip() {
        let (ox, oy) = compute_offset(500, 200, 100, 100);
        assert_eq!(apply_offset(ox, oy, 100, 100), (500, 200));
    }

    #[test]
    fn offset_negative_origin_multi_monitor() {
        let (ox, oy) = compute_offset(-1500, 200, -1920, 0);
        assert_eq!((ox, oy), (420, 200));
        assert_eq!(apply_offset(ox, oy, -1920, 0), (-1500, 200));
    }

    // ── PositionStore tests ───────────────────────────────────────────────

    #[test]
    fn store_loads_new_schema_returns_per_kind() {
        let json = r#"{
            "explorer":    {"offset_x": 10, "offset_y": 20},
            "file_dialog": {"offset_x": 30, "offset_y": 40}
        }"#;
        let store = PositionStore::from_json_str(json).unwrap();
        assert_eq!(
            store.offset(TargetKind::Explorer),
            SavedPos {
                offset_x: 10,
                offset_y: 20
            }
        );
        assert_eq!(
            store.offset(TargetKind::FileDialog),
            SavedPos {
                offset_x: 30,
                offset_y: 40
            }
        );
    }

    #[test]
    fn store_loads_old_flat_schema_as_explorer_and_copies_to_dialog() {
        let json = r#"{"offset_x": 5, "offset_y": 7}"#;
        let store = PositionStore::from_json_str(json).unwrap();
        assert_eq!(
            store.offset(TargetKind::Explorer),
            SavedPos {
                offset_x: 5,
                offset_y: 7
            }
        );
        assert_eq!(
            store.offset(TargetKind::FileDialog),
            SavedPos {
                offset_x: 5,
                offset_y: 7
            }
        );
    }

    #[test]
    fn store_missing_dialog_field_defaults_to_explorer_value() {
        let json = r#"{"explorer":{"offset_x":1,"offset_y":2}}"#;
        let store = PositionStore::from_json_str(json).unwrap();
        assert_eq!(
            store.offset(TargetKind::FileDialog),
            SavedPos {
                offset_x: 1,
                offset_y: 2
            }
        );
    }

    #[test]
    fn store_save_roundtrip_preserves_both_kinds() {
        let mut store = PositionStore::default();
        store.set_offset(
            TargetKind::Explorer,
            SavedPos {
                offset_x: 11,
                offset_y: 22,
            },
        );
        store.set_offset(
            TargetKind::FileDialog,
            SavedPos {
                offset_x: 33,
                offset_y: 44,
            },
        );
        let json = store.to_json_string().unwrap();
        let reload = PositionStore::from_json_str(&json).unwrap();
        assert_eq!(
            reload.offset(TargetKind::Explorer),
            SavedPos {
                offset_x: 11,
                offset_y: 22
            }
        );
        assert_eq!(
            reload.offset(TargetKind::FileDialog),
            SavedPos {
                offset_x: 33,
                offset_y: 44
            }
        );
    }
}
