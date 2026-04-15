//! Unified error type for exbar runtime.

use std::path::PathBuf;
use thiserror::Error;

/// All errors the runtime can produce. Win32 is the dominant source;
/// config/IO/JSON round out the rest.
#[derive(Debug, Error)]
pub enum ExbarError {
    /// Underlying Win32 call failed. Wraps HRESULT + message.
    #[error("Win32 error: {0}")]
    Win32(#[from] windows::core::Error),

    /// Filesystem I/O failed. Carries path context for diagnostics.
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// JSON serialization or parsing failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Config file path couldn't be resolved or was malformed.
    #[error("config error: {0}")]
    Config(String),
}

pub type ExbarResult<T> = Result<T, ExbarError>;

impl ExbarError {
    /// Convenience constructor for the `Io` variant, which carries path
    /// context beyond what a bare `std::io::Error` conveys.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        ExbarError::Io {
            path: path.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_windows_core_error_works() {
        let win_err = windows::core::Error::from(windows::core::HRESULT(0x80004005u32 as i32));
        let ex: ExbarError = win_err.into();
        assert!(matches!(ex, ExbarError::Win32(_)));
    }

    #[test]
    fn from_serde_json_error_works() {
        let json_err = serde_json::from_str::<serde_json::Value>("{ invalid").unwrap_err();
        let ex: ExbarError = json_err.into();
        assert!(matches!(ex, ExbarError::Json(_)));
    }

    #[test]
    fn io_constructor_carries_path() {
        let src = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let ex = ExbarError::io("C:\\nope.json", src);
        let msg = format!("{ex}");
        assert!(msg.contains("C:\\nope.json"), "expected path in: {msg}");
        assert!(msg.contains("missing"), "expected source in: {msg}");
    }

    #[test]
    fn config_variant_display_has_prefix() {
        let ex = ExbarError::Config("bad field".into());
        let msg = format!("{ex}");
        assert!(msg.starts_with("config error:"), "got: {msg}");
    }
}
