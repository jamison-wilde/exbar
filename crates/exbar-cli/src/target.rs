//! Active-target types used by visibility, positioning, and navigation dispatch.
//!
//! An [`ActiveTarget`] pairs a foreground HWND with its [`TargetKind`], letting code
//! that cares (dispatch, config lookup) branch on kind while code that doesn't
//! (positioning, LOCATIONCHANGE filtering) just reads the HWND.

use windows::Win32::Foundation::HWND;

/// What kind of window the toolbar is currently tracking as its navigation target.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TargetKind {
    /// A Windows File Explorer window (`CabinetWClass`).
    Explorer,
    /// A Shell-hosted file dialog (Save As / Open).
    FileDialog,
}

/// A foreground HWND paired with its [`TargetKind`].
#[derive(Copy, Clone, Debug)]
pub struct ActiveTarget {
    pub hwnd: HWND,
    pub kind: TargetKind,
}

impl ActiveTarget {
    pub fn explorer(hwnd: HWND) -> Self {
        Self {
            hwnd,
            kind: TargetKind::Explorer,
        }
    }
    pub fn file_dialog(hwnd: HWND) -> Self {
        Self {
            hwnd,
            kind: TargetKind::FileDialog,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::NonNull;

    fn fake_hwnd(n: usize) -> HWND {
        // HWND wraps *mut c_void in windows = 0.61. NonNull::dangling gives a
        // non-null bogus pointer just for equality checks in these pure tests.
        let _ = n;
        HWND(NonNull::<core::ffi::c_void>::dangling().as_ptr())
    }

    #[test]
    fn explorer_constructor_sets_kind() {
        let t = ActiveTarget::explorer(fake_hwnd(42));
        assert_eq!(t.kind, TargetKind::Explorer);
    }

    #[test]
    fn file_dialog_constructor_sets_kind() {
        let t = ActiveTarget::file_dialog(fake_hwnd(99));
        assert_eq!(t.kind, TargetKind::FileDialog);
    }
}
