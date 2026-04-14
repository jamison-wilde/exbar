use std::sync::OnceLock;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{GetSysColor, SYS_COLOR_INDEX};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::core::w;

/// Scale a logical pixel value by the given DPI.
/// 96 DPI = 100% scaling.
pub fn scale(logical_px: i32, dpi: u32) -> i32 {
    (logical_px as u32 * dpi / 96) as i32
}

static DARK_MODE_CACHE: OnceLock<bool> = OnceLock::new();

/// Query the system dark mode setting (cached per DLL load).
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
        crate::log::info(&format!(
            "theme: AppsUseLightTheme registry read ok={} raw_value={} dark_mode={}",
            result.is_ok(),
            data,
            dark
        ));
        dark
    })
}

/// Get the DPI for a specific window. Returns 96 if the call fails.
pub fn get_dpi(hwnd: HWND) -> u32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 96 } else { dpi }
}

/// Convert a COLORREF (0x00BBGGRR) to (r, g, b).
pub fn colorref_to_rgb(cr: u32) -> (u8, u8, u8) {
    let r = (cr & 0xFF) as u8;
    let g = ((cr >> 8) & 0xFF) as u8;
    let b = ((cr >> 16) & 0xFF) as u8;
    (r, g, b)
}

/// Get the system text color as (r, g, b).
pub fn text_color() -> (u8, u8, u8) {
    // COLOR_WINDOWTEXT = 8
    let cr = unsafe { GetSysColor(SYS_COLOR_INDEX(8)) };
    colorref_to_rgb(cr)
}

/// Get the system hotlight (hover) color as (r, g, b).
pub fn hotlight_color() -> (u8, u8, u8) {
    // COLOR_HOTLIGHT = 26
    let cr = unsafe { GetSysColor(SYS_COLOR_INDEX(26)) };
    colorref_to_rgb(cr)
}

/// Layout constants (in logical pixels — always pass through `scale()`)
pub const REFRESH_BUTTON_SIZE: i32 = 24;
pub const SEPARATOR_WIDTH: i32 = 1;
pub const SEPARATOR_MARGIN: i32 = 6;
pub const BUTTON_ICON_SIZE: i32 = 16;
pub const BUTTON_PADDING_H: i32 = 8;
pub const BUTTON_PADDING_V: i32 = 4;
pub const BUTTON_GAP: i32 = 2;
pub const ICON_TEXT_GAP: i32 = 4;
