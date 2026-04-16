//! Configuration: schema, persistence, and mutation API for `~/.exbar.json`.
//!
//! The on-disk JSON shape is owned by [`Config`] and its nested types
//! ([`FolderEntry`], [`Orientation`], [`LogLevel`]). Mutation helpers
//! enforce small invariants in one place — e.g. [`Config::rename_folder`]
//! refuses to overwrite a folder with empty / whitespace-only text.
//!
//! The [`ConfigStore`] trait abstracts the file-IO boundary. Production
//! wires [`JsonFileStore`] (which reads/writes `~/.exbar.json`); tests
//! inject a `MockConfigStore` that holds a `Config` in a `Mutex`. See
//! `docs/adrs/ADR-0004-trait-seams-via-box-dyn.md` for why this seam
//! exists.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Toolbar orientation — horizontal lays buttons left-to-right; vertical stacks top-to-bottom.
#[derive(Debug, Default, Deserialize, Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Orientation {
    #[default]
    Horizontal,
    Vertical,
}

/// Filter for the file logger (see `log.rs`). Values serialize as
/// lowercase ("error", "warn", "info", "debug", "trace").
#[derive(Debug, Default, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

fn default_opacity() -> f32 {
    0.8
}
fn default_new_tab_timeout() -> u32 {
    1000
}
fn default_reposition_delay() -> u32 {
    250
}
fn default_enable_file_dialogs() -> bool {
    true
}

fn deserialize_clamped_timeout<'de, D>(d: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = u32::deserialize(d)?;
    Ok(v.min(5000))
}

/// Top-level configuration loaded from `~/.exbar.json`. Mutations go through methods so
/// JSON-round-trip invariants stay in one place.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    pub folders: Vec<FolderEntry>,
    #[serde(default)]
    pub layout: Orientation,
    #[serde(default = "default_opacity")]
    pub background_opacity: f32,
    #[serde(
        rename = "newTabTimeoutMsZeroDisables",
        default = "default_new_tab_timeout",
        deserialize_with = "deserialize_clamped_timeout"
    )]
    pub new_tab_timeout_ms_zero_disables: u32,
    #[serde(default)]
    pub log_level: LogLevel,
    /// Delay in ms before showing the toolbar at its new position after
    /// Explorer maximize/restore/move. Toolbar hides during this delay
    /// so it doesn't visually jump mid-animation. 0 = no delay.
    #[serde(rename = "repositionDelayMs", default = "default_reposition_delay")]
    pub reposition_delay_ms: u32,
    #[serde(rename = "enableFileDialogs", default = "default_enable_file_dialogs")]
    pub enable_file_dialogs: bool,
}

/// One folder shortcut. Persists to JSON as `{"name": "...", "path": "..."}` plus an optional cached icon.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct FolderEntry {
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

impl Config {
    // Intentionally not implementing `std::str::FromStr` — that trait returns
    // `Result<Self, E>`, but here we treat any parse failure as "use the
    // default"/None at the callsite. Keep the `Option`-returning bespoke API.
    /// Parse `json` as a `Config`. Returns `None` on any deserialize error.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(json: &str) -> Option<Config> {
        serde_json::from_str(json).ok()
    }

    /// Load and parse a config from an arbitrary file path. Returns `None` if the file is missing or malformed.
    pub fn load_from_path(path: &str) -> Option<Config> {
        let contents = fs::read_to_string(path).ok()?;
        Self::from_str(&contents)
    }

    /// Load config from the default path (`~/.exbar.json`). Returns `None` if missing or malformed.
    pub fn load() -> Option<Config> {
        let path = default_config_path();
        Self::load_from_path(&path)
    }

    /// Append a new folder shortcut with the given display `name` and filesystem `path`.
    pub fn add_folder(&mut self, name: String, path: String) {
        self.folders.push(FolderEntry {
            name,
            path,
            icon: None,
        });
    }

    /// Remove the folder at `index`. No-op if `index` is out of bounds.
    pub fn remove_folder(&mut self, index: usize) {
        if index < self.folders.len() {
            self.folders.remove(index);
        }
    }

    /// Move the folder at `from` to position `to` in the folders list.
    /// `to` is a pre-removal insertion index in `0..=folders.len()`.
    /// No-op if `from >= len`, or if the resulting position equals `from`.
    pub fn move_folder(&mut self, from: usize, to: usize) {
        if from >= self.folders.len() {
            return;
        }
        // Adjust the insertion index for removal-shift.
        let effective_to = if to > from { to - 1 } else { to };
        if effective_to == from {
            return;
        }
        if effective_to > self.folders.len() {
            return;
        }
        let entry = self.folders.remove(from);
        self.folders
            .insert(effective_to.min(self.folders.len()), entry);
    }

    /// Rename the folder at `index` to `new_name`. Whitespace-only names are trimmed and treated as no-ops.
    pub fn rename_folder(&mut self, index: usize, new_name: String) {
        if index >= self.folders.len() {
            return;
        }
        let trimmed = new_name.trim();
        if trimmed.is_empty() {
            return;
        }
        self.folders[index].name = trimmed.to_owned();
    }

    /// Serialize the config to pretty JSON and write it to `path`.
    pub fn save_to_path(&self, path: &str) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        fs::write(path, json)
    }

    /// Serialize the config and write it to the default path (`~/.exbar.json`).
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to_path(&default_config_path())
    }
}

/// Returns the default config file path (`~/.exbar.json` on Windows). Does not verify the file exists.
pub fn default_config_path() -> String {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| String::from("C:\\Users\\Default"));
    let mut path = PathBuf::from(home);
    path.push(".exbar.json");
    path.to_string_lossy().into_owned()
}

/// Returns `true` if `path` looks like a shell alias such as `shell:downloads` or `shell:home`.
pub fn is_shell_alias(path: &str) -> bool {
    path.starts_with("shell:")
}

// ── SP3: ConfigStore trait ──────────────────────────────────────────────────

use crate::error::{ExbarError, ExbarResult};

/// Pluggable persistence for [`Config`]. Production uses [`JsonFileStore`]; tests inject mocks. See ADR-0004.
pub trait ConfigStore: Send + Sync {
    fn load(&self) -> Option<Config>;
    fn save(&self, config: &Config) -> ExbarResult<()>;
}

/// Production `ConfigStore` that reads/writes `~/.exbar.json`.
#[derive(Default)]
pub struct JsonFileStore;

impl JsonFileStore {
    pub fn new() -> Self {
        Self
    }
}

impl ConfigStore for JsonFileStore {
    fn load(&self) -> Option<Config> {
        Config::load_from_path(&default_config_path())
    }

    fn save(&self, config: &Config) -> ExbarResult<()> {
        let path = default_config_path();
        config
            .save_to_path(&path)
            .map_err(|e| ExbarError::io(&path, e))
    }
}

#[cfg(test)]
pub(crate) mod test_mocks {
    use super::{Config, ConfigStore};
    use crate::error::{ExbarError, ExbarResult};
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct MockConfigStore {
        pub load_value: Mutex<Option<Config>>,
        pub load_calls: Mutex<usize>,
        pub save_calls: Mutex<Vec<Config>>,
        pub save_should_err: Mutex<bool>,
    }
    impl ConfigStore for MockConfigStore {
        fn load(&self) -> Option<Config> {
            *self.load_calls.lock().unwrap() += 1;
            self.load_value.lock().unwrap().clone()
        }
        fn save(&self, config: &Config) -> ExbarResult<()> {
            self.save_calls.lock().unwrap().push(config.clone());
            if *self.save_should_err.lock().unwrap() {
                return Err(ExbarError::Config("mock save error".into()));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn parse_valid_config() {
        let json = r#"{
            "folders": [
                {"name": "Downloads", "path": "C:\\Users\\test\\Downloads"},
                {"name": "Projects", "path": "C:\\Users\\test\\Projects", "icon": "C:\\icons\\proj.ico"}
            ]
        }"#;
        let cfg = Config::from_str(json).unwrap();
        assert_eq!(cfg.folders.len(), 2);
        assert_eq!(cfg.folders[0].name, "Downloads");
        assert_eq!(cfg.folders[0].path, "C:\\Users\\test\\Downloads");
        assert!(cfg.folders[0].icon.is_none());
        assert_eq!(cfg.folders[1].icon.as_deref(), Some("C:\\icons\\proj.ico"));
    }

    #[test]
    fn parse_empty_folders() {
        let json = r#"{"folders": []}"#;
        let cfg = Config::from_str(json).unwrap();
        assert!(cfg.folders.is_empty());
    }

    #[test]
    fn parse_missing_file_returns_none() {
        let result = Config::load_from_path("C:\\nonexistent\\path\\.exbar.json");
        assert!(result.is_none());
    }

    #[test]
    fn parse_malformed_json_returns_none() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "not json at all {{{{").unwrap();
        let result = Config::load_from_path(f.path().to_str().unwrap());
        assert!(result.is_none());
    }

    #[test]
    fn config_path_resolves_home() {
        let path = default_config_path();
        assert!(path.ends_with(".exbar.json"));
        assert!(path.starts_with("C:\\Users\\") || path.starts_with("/"));
    }

    #[test]
    fn shell_alias_detected() {
        assert!(is_shell_alias("shell:downloads"));
        assert!(!is_shell_alias("C:\\Users\\test"));
    }

    #[test]
    fn serialize_round_trip() {
        let json = r#"{
            "folders": [
                {"name": "A", "path": "C:\\a"},
                {"name": "B", "path": "shell:Downloads", "icon": "icon.ico"}
            ],
            "layout": "vertical",
            "background_opacity": 0.5,
            "newTabTimeoutMsZeroDisables": 200
        }"#;
        let cfg = Config::from_str(json).unwrap();
        let serialized = serde_json::to_string(&cfg).unwrap();
        let cfg2 = Config::from_str(&serialized).unwrap();
        assert_eq!(cfg.folders.len(), cfg2.folders.len());
        assert_eq!(cfg.folders[0].name, cfg2.folders[0].name);
        assert_eq!(cfg.folders[1].icon, cfg2.folders[1].icon);
        assert_eq!(
            cfg.new_tab_timeout_ms_zero_disables,
            cfg2.new_tab_timeout_ms_zero_disables
        );
        assert_eq!(cfg.new_tab_timeout_ms_zero_disables, 200);
    }

    #[test]
    fn new_tab_timeout_defaults_to_1000_when_missing() {
        let json = r#"{"folders": []}"#;
        let cfg = Config::from_str(json).unwrap();
        assert_eq!(cfg.new_tab_timeout_ms_zero_disables, 1000);
    }

    #[test]
    fn new_tab_timeout_clamps_to_range() {
        let json = r#"{"folders": [], "newTabTimeoutMsZeroDisables": 99999}"#;
        let cfg = Config::from_str(json).unwrap();
        assert_eq!(cfg.new_tab_timeout_ms_zero_disables, 5000);
    }

    #[test]
    fn add_folder_appends_to_end() {
        let mut cfg = Config::from_str(r#"{"folders":[{"name":"A","path":"C:\\a"}]}"#).unwrap();
        cfg.add_folder("B".into(), "C:\\b".into());
        assert_eq!(cfg.folders.len(), 2);
        assert_eq!(cfg.folders[1].name, "B");
        assert_eq!(cfg.folders[1].path, "C:\\b");
        assert!(cfg.folders[1].icon.is_none());
    }

    #[test]
    fn remove_folder_deletes_by_index() {
        let mut cfg = Config::from_str(
            r#"{"folders":[{"name":"A","path":"C:\\a"},{"name":"B","path":"C:\\b"}]}"#,
        )
        .unwrap();
        cfg.remove_folder(0);
        assert_eq!(cfg.folders.len(), 1);
        assert_eq!(cfg.folders[0].name, "B");
    }

    #[test]
    fn remove_folder_out_of_bounds_is_noop() {
        let mut cfg = Config::from_str(r#"{"folders":[{"name":"A","path":"C:\\a"}]}"#).unwrap();
        cfg.remove_folder(42);
        assert_eq!(cfg.folders.len(), 1);
    }

    #[test]
    fn rename_folder_updates_name() {
        let mut cfg = Config::from_str(r#"{"folders":[{"name":"A","path":"C:\\a"}]}"#).unwrap();
        cfg.rename_folder(0, "Renamed".into());
        assert_eq!(cfg.folders[0].name, "Renamed");
        assert_eq!(cfg.folders[0].path, "C:\\a");
    }

    #[test]
    fn rename_folder_empty_is_noop() {
        let mut cfg = Config::from_str(r#"{"folders":[{"name":"A","path":"C:\\a"}]}"#).unwrap();
        cfg.rename_folder(0, "   ".into());
        assert_eq!(cfg.folders[0].name, "A");
    }

    #[test]
    fn rename_folder_out_of_bounds_is_noop() {
        let mut cfg = Config::from_str(r#"{"folders":[{"name":"A","path":"C:\\a"}]}"#).unwrap();
        cfg.rename_folder(7, "X".into());
        assert_eq!(cfg.folders[0].name, "A");
    }

    #[test]
    fn save_to_path_round_trips() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let mut cfg = Config::from_str(r#"{"folders":[{"name":"A","path":"C:\\a"}]}"#).unwrap();
        cfg.add_folder("B".into(), "C:\\b".into());
        cfg.save_to_path(f.path().to_str().unwrap()).unwrap();
        let cfg2 = Config::load_from_path(f.path().to_str().unwrap()).unwrap();
        assert_eq!(cfg2.folders.len(), 2);
        assert_eq!(cfg2.folders[1].name, "B");
        let _ = &mut f; // keep tempfile alive
    }

    #[test]
    fn move_folder_forward() {
        let mut cfg = Config::from_str(
            r#"{"folders":[{"name":"A","path":"C:\\a"},{"name":"B","path":"C:\\b"},{"name":"C","path":"C:\\c"}]}"#
        ).unwrap();
        cfg.move_folder(0, 3);
        assert_eq!(
            cfg.folders
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            vec!["B", "C", "A"]
        );
    }

    #[test]
    fn move_folder_backward() {
        let mut cfg = Config::from_str(
            r#"{"folders":[{"name":"A","path":"C:\\a"},{"name":"B","path":"C:\\b"},{"name":"C","path":"C:\\c"}]}"#
        ).unwrap();
        cfg.move_folder(2, 0);
        assert_eq!(
            cfg.folders
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            vec!["C", "A", "B"]
        );
    }

    #[test]
    fn move_folder_same_position_is_noop() {
        let mut cfg = Config::from_str(
            r#"{"folders":[{"name":"A","path":"C:\\a"},{"name":"B","path":"C:\\b"}]}"#,
        )
        .unwrap();
        cfg.move_folder(0, 0);
        cfg.move_folder(1, 1);
        cfg.move_folder(1, 2); // insertion index equals source+1 → no-op too
        assert_eq!(
            cfg.folders
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "B"]
        );
    }

    #[test]
    fn move_folder_out_of_bounds_is_noop() {
        let mut cfg = Config::from_str(r#"{"folders":[{"name":"A","path":"C:\\a"}]}"#).unwrap();
        cfg.move_folder(5, 0);
        cfg.move_folder(0, 99);
        assert_eq!(cfg.folders[0].name, "A");
    }

    #[test]
    fn log_level_defaults_to_info_when_missing() {
        let json = r#"{"folders": []}"#;
        let cfg = Config::from_str(json).unwrap();
        assert_eq!(cfg.log_level, LogLevel::Info);
    }

    #[test]
    fn log_level_deserializes_debug() {
        let json = r#"{"folders": [], "log_level": "debug"}"#;
        let cfg = Config::from_str(json).unwrap();
        assert_eq!(cfg.log_level, LogLevel::Debug);
    }

    #[test]
    fn log_level_deserializes_all_variants() {
        for (s, expected) in [
            ("error", LogLevel::Error),
            ("warn", LogLevel::Warn),
            ("info", LogLevel::Info),
            ("debug", LogLevel::Debug),
            ("trace", LogLevel::Trace),
        ] {
            let json = format!(r#"{{"folders": [], "log_level": "{s}"}}"#);
            let cfg = Config::from_str(&json).unwrap();
            assert_eq!(cfg.log_level, expected, "failed for {s}");
        }
    }

    #[test]
    fn log_level_round_trips_through_serde() {
        let cfg = Config::from_str(r#"{"folders": [], "log_level": "trace"}"#).unwrap();
        let serialized = serde_json::to_string(&cfg).unwrap();
        let cfg2 = Config::from_str(&serialized).unwrap();
        assert_eq!(cfg2.log_level, LogLevel::Trace);
    }

    #[test]
    fn enable_file_dialogs_defaults_to_true() {
        let cfg: Config = serde_json::from_str(r#"{"folders":[]}"#).unwrap();
        assert!(cfg.enable_file_dialogs);
    }

    #[test]
    fn enable_file_dialogs_respects_explicit_false() {
        let cfg: Config =
            serde_json::from_str(r#"{"folders":[],"enableFileDialogs":false}"#).unwrap();
        assert!(!cfg.enable_file_dialogs);
    }
}
