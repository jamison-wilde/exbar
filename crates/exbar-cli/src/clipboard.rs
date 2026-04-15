//! Clipboard abstraction — SP3 trait seam.
//!
//! `Win32Clipboard` implements the `Clipboard` trait using the standard
//! CF_UNICODETEXT + HGLOBAL pattern that was previously inlined in toolbar.rs.

use crate::error::{ExbarError, ExbarResult};

/// Sets text on the system clipboard.
///
/// The `Win32Clipboard` implementation uses CF_UNICODETEXT.
pub trait Clipboard: Send + Sync {
    fn set_text(&self, text: &str) -> ExbarResult<()>;
}

/// Production implementation backed by the Win32 clipboard API.
#[derive(Default)]
pub struct Win32Clipboard;

impl Win32Clipboard {
    pub fn new() -> Self {
        Self
    }
}

impl Clipboard for Win32Clipboard {
    fn set_text(&self, text: &str) -> ExbarResult<()> {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::DataExchange::{
            CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
        };
        use windows::Win32::System::Memory::{
            GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
        };
        use windows::Win32::System::Ole::CF_UNICODETEXT;

        // Encode as null-terminated UTF-16.
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let byte_size = wide.len() * std::mem::size_of::<u16>();

        // Use a closure so we can return early and still guarantee
        // CloseClipboard is called on every exit path after a successful Open.
        let result: ExbarResult<()> = unsafe {
            OpenClipboard(None)?;

            let outcome: ExbarResult<()> = (|| {
                EmptyClipboard()?;

                let hmem = GlobalAlloc(GMEM_MOVEABLE, byte_size)?;
                if hmem.is_invalid() {
                    return Err(ExbarError::Win32(windows::core::Error::from_win32()));
                }

                let dest = GlobalLock(hmem);
                if dest.is_null() {
                    // GlobalLock failed; hmem ownership stays with us — free it.
                    // We do not have an ExbarError variant for this, so use from_win32.
                    return Err(ExbarError::Win32(windows::core::Error::from_win32()));
                }

                // SAFETY: `dest` is a GlobalLock'd buffer of exactly `byte_size` bytes
                // freshly allocated above. `wide.as_ptr()` points to a live Vec of the
                // same byte count. The two regions cannot overlap (one is heap-allocated
                // by the OS via GlobalAlloc, the other is a Rust Vec on the Rust heap).
                std::ptr::copy_nonoverlapping(
                    wide.as_ptr() as *const u8,
                    dest as *mut u8,
                    byte_size,
                );
                let _ = GlobalUnlock(hmem);

                // SetClipboardData takes ownership of the HGLOBAL on success; do not
                // free hmem afterwards in that case.
                SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(hmem.0)))?;

                Ok(())
            })();

            // Balance OpenClipboard regardless of success or failure.
            let _ = CloseClipboard();

            outcome
        };

        result
    }
}
