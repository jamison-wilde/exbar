//! File-backed logger driven by the `log` crate.
//!
//! Users configure verbosity via `Config::log_level` (defaults to Info).
//! Messages below the configured filter are not formatted, keeping
//! hot-path cost minimal when verbose logging is off.

use crate::config::LogLevel;
use std::io::Write as _;
use std::path::PathBuf;

fn log_path() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push("exbar.log");
    p
}

struct FileLogger;

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path())
        {
            let pid = std::process::id();
            let ts = timestamp();
            let level = format_level(record.level());
            // Intentionally ignored: logging-inside-logger must not recurse.
            let _ = writeln!(f, "{ts} [{level}] pid={pid} {}", record.args());
        }
    }

    fn flush(&self) {}
}

static FILE_LOGGER: FileLogger = FileLogger;

/// Install the file logger and set the max level from config.
///
/// Idempotent: a second call is a silent no-op (log::set_logger errors
/// are ignored because it fails only if a logger is already set).
pub fn init(level: LogLevel) {
    let filter = match level {
        LogLevel::Error => log::LevelFilter::Error,
        LogLevel::Warn => log::LevelFilter::Warn,
        LogLevel::Info => log::LevelFilter::Info,
        LogLevel::Debug => log::LevelFilter::Debug,
        LogLevel::Trace => log::LevelFilter::Trace,
    };
    let _ = log::set_logger(&FILE_LOGGER);
    log::set_max_level(filter);
}

fn format_level(level: log::Level) -> &'static str {
    // 5-char padded to align with the existing "INFO "/"ERROR" format.
    match level {
        log::Level::Error => "ERROR",
        log::Level::Warn => "WARN ",
        log::Level::Info => "INFO ",
        log::Level::Debug => "DEBUG",
        log::Level::Trace => "TRACE",
    }
}

fn timestamp() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = now.as_secs();
    let millis = now.subsec_millis();
    // Simple UTC timestamp — good enough for log correlation
    let secs_in_day = total_secs % 86400;
    let h = secs_in_day / 3600;
    let m = (secs_in_day % 3600) / 60;
    let s = secs_in_day % 60;
    format!("{h:02}:{m:02}:{s:02}.{millis:03}")
}

/// Log a warning (at debug level) if the expression is Err; discard
/// the value either way. `stringify!($e)` captures the call text for
/// log context.
///
/// The expression must evaluate to a `Result<_, impl Display>`. For Win32
/// BOOL-returning calls, use `.ok()` to convert to `windows::core::Result<()>`:
///
/// ```ignore
/// // Windows BOOL-returning call:
/// warn_on_err!(unsafe { ShowWindow(hwnd, SW_HIDE).ok() });
///
/// // Already returns Result:
/// warn_on_err!(unsafe { CoCreateInstance::<_, IFileOperation>(&FileOperation, None, CLSCTX_ALL) });
/// ```
#[macro_export]
macro_rules! warn_on_err {
    ($e:expr) => {
        match ($e) {
            Ok(v) => {
                let _ = v;
            }
            Err(err) => {
                log::debug!("{}: {}", stringify!($e), err);
            }
        }
    };
}
