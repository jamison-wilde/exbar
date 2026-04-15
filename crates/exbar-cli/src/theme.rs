use std::sync::OnceLock;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::core::w;

/// Scale a logical pixel value by the given DPI.
/// 96 DPI = 100% scaling.
pub fn scale(logical_px: i32, dpi: u32) -> i32 {
    (logical_px as u32 * dpi / 96) as i32
}

static DARK_MODE_CACHE: OnceLock<bool> = OnceLock::new();

/// Query the system dark mode setting (cached for the life of the process).
/// Returns true if apps are set to dark mode.
pub fn is_dark_mode() -> bool {
    *DARK_MODE_CACHE.get_or_init(|| {
        let mut data: u32 = 1; // default to light (1 = light)
        let mut size = std::mem::size_of::<u32>() as u32;
        let result = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
                w!("AppsUseLightTheme"),
                RRF_RT_REG_DWORD,
                None,
                Some(&mut data as *mut u32 as *mut std::ffi::c_void),
                Some(&mut size),
            )
        };
        // data == 0 means dark mode; data == 1 means light mode
        let dark = result.is_ok() && data == 0;
        #[cfg(not(test))]
        log::info!(
            "theme: AppsUseLightTheme registry read ok={} raw_value={} dark_mode={}",
            result.is_ok(),
            data,
            dark
        );
        dark
    })
}

/// Get the DPI for a specific window. Returns 96 if the call fails.
pub fn get_dpi(hwnd: HWND) -> u32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 96 } else { dpi }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_at_100_percent() {
        assert_eq!(scale(24, 96), 24);
    }

    #[test]
    fn scale_at_150_percent() {
        assert_eq!(scale(24, 144), 36);
    }

    #[test]
    fn scale_at_200_percent() {
        assert_eq!(scale(24, 192), 48);
    }

    #[test]
    fn scale_at_125_percent() {
        // 24 * 120 / 96 = 30
        assert_eq!(scale(24, 120), 30);
    }

    #[test]
    fn detect_dark_mode_returns_bool() {
        // Just verify it doesn't crash — actual value depends on system setting
        let _is_dark = is_dark_mode();
    }
}
