//! Shared logging — writes to %TEMP%\tabplorer.log

use std::io::Write as _;

fn log_path() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push("tabplorer.log");
    p
}

pub fn log(level: &str, msg: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        let pid = std::process::id();
        let ts = timestamp();
        let _ = writeln!(f, "{ts} [{level}] pid={pid} {msg}");
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

pub fn info(msg: &str) {
    log("INFO ", msg);
}

pub fn error(msg: &str) {
    log("ERROR", msg);
}
