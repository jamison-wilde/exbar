//! Navigation into a foreground file dialog via keyboard injection.
//!
//! We focus the dialog's breadcrumb path bar by injecting Ctrl+L, then type
//! the target path as Unicode characters, then submit with Enter. This works
//! because every Shell-hosted file dialog honours Ctrl+L as "focus address
//! bar in edit mode" — the same shortcut used in File Explorer itself.
//!
//! An earlier UIA-based approach was tried and rejected; see
//! `docs/superpowers/spikes/2026-04-16-uia-spike-results.md`.

use std::path::Path;

use windows::Win32::Foundation::HWND;

use crate::error::ExbarResult;

/// Drives navigation of a foreground file dialog to a target path.
pub trait DialogNavigator {
    /// Navigate `dialog_hwnd` to `path`. Returns `Err` if the injection
    /// sequence couldn't be submitted.
    fn navigate(&self, dialog_hwnd: HWND, path: &Path) -> ExbarResult<()>;
}

/// Production impl — keyboard injection via `SendInput`.
///
/// Focuses the dialog's address bar with Ctrl+L, types the path as Unicode
/// code units, then submits with Enter.
pub struct KeybdDialogNavigator;

impl KeybdDialogNavigator {
    /// Create a new `KeybdDialogNavigator`.
    pub fn new() -> Self {
        Self
    }
}

impl Default for KeybdDialogNavigator {
    fn default() -> Self {
        Self::new()
    }
}

impl DialogNavigator for KeybdDialogNavigator {
    fn navigate(&self, dialog_hwnd: HWND, path: &Path) -> ExbarResult<()> {
        use std::time::Duration;
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
            KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY,
        };
        use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;

        use crate::error::ExbarError;

        // VK_L (0x4C) and VK_RETURN (0x0D) are not exported as named constants
        // in windows = 0.61; use raw VIRTUAL_KEY values instead.
        const VK_CONTROL: u16 = 0x11;
        const VK_L: u16 = 0x4C;
        const VK_RETURN: u16 = 0x0D;

        const FOCUS_SETTLE_MS: u64 = 50;
        const AFTER_CTRL_L_MS: u64 = 40;

        fn vk_input(vk: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(vk),
                        dwFlags: flags,
                        ..Default::default()
                    },
                },
            }
        }

        fn unicode_input(unit: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0),
                        wScan: unit,
                        dwFlags: KEYEVENTF_UNICODE | flags,
                        ..Default::default()
                    },
                },
            }
        }

        fn send(inputs: &[INPUT]) -> ExbarResult<()> {
            let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
            if sent as usize != inputs.len() {
                return Err(ExbarError::Config(format!(
                    "SendInput sent={sent}, expected {}",
                    inputs.len()
                )));
            }
            Ok(())
        }

        let path_str = path.to_string_lossy().to_string();

        unsafe {
            let _ = SetForegroundWindow(dialog_hwnd);
        }
        std::thread::sleep(Duration::from_millis(FOCUS_SETTLE_MS));

        // Ctrl+L: focus breadcrumb in edit mode.
        let ctrl_l = [
            vk_input(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
            vk_input(VK_L, KEYBD_EVENT_FLAGS(0)),
            vk_input(VK_L, KEYEVENTF_KEYUP),
            vk_input(VK_CONTROL, KEYEVENTF_KEYUP),
        ];
        send(&ctrl_l)?;
        std::thread::sleep(Duration::from_millis(AFTER_CTRL_L_MS));

        // Type the path as Unicode code units.
        if !path_str.is_empty() {
            let mut typing: Vec<INPUT> = Vec::with_capacity(path_str.len() * 2);
            for unit in path_str.encode_utf16() {
                typing.push(unicode_input(unit, KEYBD_EVENT_FLAGS(0)));
                typing.push(unicode_input(unit, KEYEVENTF_KEYUP));
            }
            send(&typing)?;
        }

        // Enter: commit navigation.
        let enter = [
            vk_input(VK_RETURN, KEYBD_EVENT_FLAGS(0)),
            vk_input(VK_RETURN, KEYEVENTF_KEYUP),
        ];
        send(&enter)?;

        log::info!("dialog_nav: navigated hwnd={:?} to {:?}", dialog_hwnd, path);
        Ok(())
    }
}

#[cfg(test)]
pub mod test_mocks {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    /// Test double that records every call.
    ///
    /// `calls` holds one `(hwnd_as_isize, path)` entry per `navigate`
    /// invocation. `force_err` makes the next (and all subsequent) calls
    /// return an `Err`.
    #[derive(Default)]
    pub struct MockDialogNavigator {
        /// Each recorded call, as `(hwnd_as_isize, path)`. HWND.0 is `*mut c_void`
        /// which isn't comparable; casting to `isize` makes assertions easy.
        pub calls: RefCell<Vec<(isize, PathBuf)>>,
        /// When `Some`, every `navigate` call returns this as an error message.
        pub force_err: RefCell<Option<String>>,
    }

    impl DialogNavigator for MockDialogNavigator {
        fn navigate(&self, hwnd: HWND, path: &Path) -> ExbarResult<()> {
            self.calls
                .borrow_mut()
                .push((hwnd.0 as isize, path.to_path_buf()));
            if let Some(msg) = self.force_err.borrow().clone() {
                return Err(crate::error::ExbarError::Config(msg));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_mocks::MockDialogNavigator;
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn mock_records_each_call() {
        let m = MockDialogNavigator::default();
        let h1 = HWND(42 as *mut _);
        let h2 = HWND(99 as *mut _);
        m.navigate(h1, Path::new("C:\\Downloads")).unwrap();
        m.navigate(h2, Path::new("C:\\Projects")).unwrap();
        let calls = m.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, 42);
        assert_eq!(calls[0].1, PathBuf::from("C:\\Downloads"));
        assert_eq!(calls[1].0, 99);
        assert_eq!(calls[1].1, PathBuf::from("C:\\Projects"));
    }

    #[test]
    fn mock_propagates_forced_error() {
        let m = MockDialogNavigator::default();
        *m.force_err.borrow_mut() = Some("simulated".into());
        assert!(m
            .navigate(HWND(42 as *mut _), Path::new("C:\\"))
            .is_err());
    }
}
