//! Filesystem layout for Exbar's persisted state.
//!
//! All state lives under `~/.exbar/`:
//! - `config.json` — folder list and runtime options.
//! - `position.json` — per-target-kind toolbar offset.
//!
//! Pre-1.2 versions stored these as `~/.exbar.json` and `~/.exbar-pos.json`
//! at the home root. [`migrate_legacy_files`] moves them into the new
//! directory on first run after upgrading.

use std::path::PathBuf;

/// `~/.exbar/` — the directory that holds all of Exbar's persisted state.
pub fn exbar_dir() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| String::from("C:\\Users\\Default"));
    let mut p = PathBuf::from(home);
    p.push(".exbar");
    p
}

/// `~/.exbar/config.json` — folder definitions + runtime options.
pub fn config_path() -> PathBuf {
    let mut p = exbar_dir();
    p.push("config.json");
    p
}

/// `~/.exbar/position.json` — per-target-kind toolbar offset.
pub fn position_path() -> PathBuf {
    let mut p = exbar_dir();
    p.push("position.json");
    p
}

/// Pre-1.2 location of the config file (`~/.exbar.json`). Kept only so
/// [`migrate_legacy_files`] can find it.
fn legacy_config_path() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| String::from("C:\\Users\\Default"));
    let mut p = PathBuf::from(home);
    p.push(".exbar.json");
    p
}

/// Pre-1.2 location of the position file (`~/.exbar-pos.json`). Kept only
/// so [`migrate_legacy_files`] can find it.
fn legacy_position_path() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| String::from("C:\\Users\\Default"));
    let mut p = PathBuf::from(home);
    p.push(".exbar-pos.json");
    p
}

/// One-shot, best-effort migration from the pre-1.2 layout. For each legacy
/// file: if it exists and its new-layout counterpart does not, create
/// `~/.exbar/` if needed and move the file over. Failures are logged but
/// do not abort startup — the worst case is the user re-creates their
/// config.
pub fn migrate_legacy_files() {
    let dir = exbar_dir();
    for (legacy, current) in [
        (legacy_config_path(), config_path()),
        (legacy_position_path(), position_path()),
    ] {
        if !legacy.exists() || current.exists() {
            continue;
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::warn!("paths: create_dir_all({dir:?}) failed during migration: {e}");
            continue;
        }
        match std::fs::rename(&legacy, &current) {
            Ok(()) => {
                log::info!("paths: migrated {legacy:?} -> {current:?}");
            }
            Err(e) => {
                log::warn!("paths: rename({legacy:?} -> {current:?}) failed: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exbar_dir_ends_with_dot_exbar() {
        let p = exbar_dir();
        assert!(p.ends_with(".exbar"), "expected .exbar suffix: {p:?}");
    }

    #[test]
    fn config_path_under_exbar_dir() {
        let p = config_path();
        assert_eq!(p.file_name().unwrap().to_str(), Some("config.json"));
        assert!(p.parent().unwrap().ends_with(".exbar"));
    }

    #[test]
    fn position_path_under_exbar_dir() {
        let p = position_path();
        assert_eq!(p.file_name().unwrap().to_str(), Some("position.json"));
        assert!(p.parent().unwrap().ends_with(".exbar"));
    }
}
