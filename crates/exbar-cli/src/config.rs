//! Configuration: schema, persistence, and mutation API for `~/.exbar/config.json`.
//!
//! The on-disk JSON shape is owned by [`Config`] and its nested types
//! ([`FolderEntry`], [`Orientation`], [`LogLevel`]). Mutation helpers
//! enforce small invariants in one place — e.g. [`Config::rename_folder`]
//! refuses to overwrite a folder with empty / whitespace-only text.
//!
//! The [`ConfigStore`] trait abstracts the file-IO boundary. Production
//! wires [`JsonFileStore`] (which reads/writes `~/.exbar/config.json`); tests
//! inject a `MockConfigStore` that holds a `Config` in a `Mutex`. See
//! `docs/adrs/ADR-0004-trait-seams-via-box-dyn.md` for why this seam
//! exists.

use serde::{Deserialize, Serialize};
use std::fs;

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
fn default_show_icons() -> bool {
    true
}

fn deserialize_clamped_timeout<'de, D>(d: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = u32::deserialize(d)?;
    Ok(v.min(5000))
}

fn default_spring_open_delay_ms() -> u32 {
    500
}
fn default_long_hover_open_ms() -> u32 {
    1200
}
fn default_hover_buffer_px() -> u32 {
    30
}
fn default_non_chain_item_opacity() -> f32 {
    0.5
}

fn deserialize_spring_open_delay<'de, D>(d: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = u32::deserialize(d)?;
    Ok(v.clamp(100, 2000))
}

fn deserialize_long_hover_open<'de, D>(d: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(u32::deserialize(d)?.clamp(500, 5000))
}

fn deserialize_hover_buffer<'de, D>(d: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = u32::deserialize(d)?;
    Ok(v.min(64))
}

fn deserialize_non_chain_opacity<'de, D>(d: D) -> Result<f32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = f32::deserialize(d)?;
    Ok(v.clamp(0.2, 1.0))
}

fn default_recent_max_count() -> u32 {
    5
}
fn default_recent_dwell_seconds() -> u32 {
    10
}

fn deserialize_recent_max_count<'de, D>(d: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(u32::deserialize(d)?.clamp(1, 20))
}

fn deserialize_recent_dwell<'de, D>(d: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(u32::deserialize(d)?.clamp(1, 300))
}

pub(crate) fn default_foreground_watchdog_ms() -> u32 {
    2000
}

/// Clamp the watchdog interval. `0` passes through (disables the watchdog);
/// any positive value is clamped into a sane 500 ms..=60 s band.
fn deserialize_watchdog_ms<'de, D>(d: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = u32::deserialize(d)?;
    Ok(if v == 0 { 0 } else { v.clamp(500, 60_000) })
}

/// Recent Folders tracking config. All fields optional with sensible defaults.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RecentConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(
        rename = "maxCount",
        default = "default_recent_max_count",
        deserialize_with = "deserialize_recent_max_count"
    )]
    pub max_count: u32,
    #[serde(rename = "includePinned", default)]
    pub include_pinned: bool,
    #[serde(
        rename = "dwellSecondsToTrack",
        default = "default_recent_dwell_seconds",
        deserialize_with = "deserialize_recent_dwell"
    )]
    pub dwell_seconds_to_track: u32,
    #[serde(rename = "excludedPaths", default)]
    pub excluded_paths: Vec<String>,
}

impl Default for RecentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_count: default_recent_max_count(),
            include_pinned: false,
            dwell_seconds_to_track: default_recent_dwell_seconds(),
            excluded_paths: Vec::new(),
        }
    }
}

/// Submenu/spring-open UX knobs. All fields have defaults and clamps.
#[derive(Debug, Deserialize, Serialize, Clone, Copy)]
pub struct SubmenuConfig {
    #[serde(
        rename = "springOpenDelayMs",
        default = "default_spring_open_delay_ms",
        deserialize_with = "deserialize_spring_open_delay"
    )]
    pub spring_open_delay_ms: u32,
    #[serde(
        rename = "hoverBufferPx",
        default = "default_hover_buffer_px",
        deserialize_with = "deserialize_hover_buffer"
    )]
    pub hover_buffer_px: u32,
    /// Reserved for future per-item dimming. Parsed and clamped but not applied
    /// to the layered-window alpha — all open popups use `config.background_opacity`
    /// as their uniform layered alpha. Kept in schema for forward-compat.
    #[serde(
        rename = "nonChainItemOpacity",
        default = "default_non_chain_item_opacity",
        deserialize_with = "deserialize_non_chain_opacity"
    )]
    pub non_chain_item_opacity: f32,
    /// How long (ms) the cursor must rest on a folder button before hover-open
    /// fires. Deliberately longer than `springOpenDelayMs` (which controls
    /// spring-open inside an already-open submenu chain) to reduce accidental
    /// triggers and narrow the right-click race window. Clamped 500..=5000.
    #[serde(
        rename = "longHoverOpenMs",
        default = "default_long_hover_open_ms",
        deserialize_with = "deserialize_long_hover_open"
    )]
    pub long_hover_open_ms: u32,
}

impl Default for SubmenuConfig {
    fn default() -> Self {
        Self {
            spring_open_delay_ms: default_spring_open_delay_ms(),
            hover_buffer_px: default_hover_buffer_px(),
            non_chain_item_opacity: default_non_chain_item_opacity(),
            long_hover_open_ms: default_long_hover_open_ms(),
        }
    }
}

/// Top-level configuration loaded from `~/.exbar/config.json`. Mutations go through methods so
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
    #[serde(default)]
    pub submenu: SubmenuConfig,
    #[serde(default)]
    pub recent: RecentConfig,
    /// When `false`, toolbar folder buttons render without the leading
    /// `📁`/`🕘` emoji (denser layout). Defaults `true` (icons shown).
    #[serde(rename = "showIcons", default = "default_show_icons")]
    pub show_icons: bool,
    /// Interval (ms) for the foreground watchdog that hides a toolbar left
    /// visible over a foreign app after a spurious Explorer foreground event.
    /// `0` disables the watchdog. Clamped to 500..=60000 otherwise.
    /// Applied at toolbar creation; changing this value takes effect only after
    /// the hook restarts (Reload config does not re-arm the timer).
    #[serde(
        rename = "foregroundWatchdogMs",
        default = "default_foreground_watchdog_ms",
        deserialize_with = "deserialize_watchdog_ms"
    )]
    pub foreground_watchdog_ms: u32,
    /// When `true`, the watchdog also re-shows the toolbar if it is hidden
    /// while the active Explorer/dialog target is foreground. Default `false`
    /// (hide-only).
    #[serde(rename = "watchdogReshow", default)]
    pub watchdog_reshow: bool,
}

/// Discriminator for toolbar button kinds. Omitted in JSON = `Folder` (backward compat).
/// Unknown values deserialize to `Folder` (forward compat via `serde(other)`).
///
/// Note: `#[serde(other)]` must appear on the **last** variant; `Folder` is placed last
/// so it acts as the catch-all for unknown future kinds.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum FolderKind {
    Recent,
    #[default]
    #[serde(other)]
    Folder,
}

/// One folder shortcut. Persists to JSON as `{"name": "...", "path": "..."}` plus an optional cached icon.
/// `path` is empty for `kind = Recent` pseudo-entries (no fixed path).
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct FolderEntry {
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default)]
    pub kind: FolderKind,
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

    /// Load config from the default path (`~/.exbar/config.json`). Returns `None` if missing or malformed.
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
            kind: FolderKind::Folder,
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
    /// Creates the parent directory if missing — needed because the default
    /// path lives under `~/.exbar/`, which won't exist on a fresh install.
    pub fn save_to_path(&self, path: &str) -> std::io::Result<()> {
        if let Some(parent) = std::path::Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        fs::write(path, json)
    }

    /// Serialize the config and write it to the default path
    /// (`~/.exbar/config.json`).
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to_path(&default_config_path())
    }
}

/// Returns the default config file path (`~/.exbar/config.json` on Windows).
/// Does not verify the file exists.
pub fn default_config_path() -> String {
    crate::paths::config_path().to_string_lossy().into_owned()
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

/// Production `ConfigStore` that reads/writes `~/.exbar/config.json`.
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
    fn show_icons_defaults_true_when_absent() {
        let cfg = Config::from_str(r#"{"folders":[]}"#).expect("parses");
        assert!(cfg.show_icons);
    }

    #[test]
    fn show_icons_false_round_trips() {
        let cfg = Config::from_str(r#"{"folders":[],"showIcons":false}"#).expect("parses");
        assert!(!cfg.show_icons);
        let json = serde_json::to_string(&cfg).expect("serializes");
        let reparsed = Config::from_str(&json).expect("reparses");
        assert!(!reparsed.show_icons);
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
        assert!(
            path.ends_with("config.json"),
            "expected config.json suffix: {path}"
        );
        assert!(path.contains(".exbar"), "expected .exbar component: {path}");
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

    #[test]
    fn submenu_defaults_when_missing() {
        let cfg: Config = Config::from_str(r#"{"folders":[]}"#).unwrap();
        assert_eq!(cfg.submenu.spring_open_delay_ms, 500);
        assert_eq!(cfg.submenu.hover_buffer_px, 30);
        assert!((cfg.submenu.non_chain_item_opacity - 0.5).abs() < 1e-6);
    }

    #[test]
    fn submenu_delay_clamped_low() {
        let cfg: Config =
            Config::from_str(r#"{"folders":[],"submenu":{"springOpenDelayMs":50}}"#).unwrap();
        assert_eq!(cfg.submenu.spring_open_delay_ms, 100);
    }

    #[test]
    fn submenu_delay_clamped_high() {
        let cfg: Config =
            Config::from_str(r#"{"folders":[],"submenu":{"springOpenDelayMs":9999}}"#).unwrap();
        assert_eq!(cfg.submenu.spring_open_delay_ms, 2000);
    }

    #[test]
    fn submenu_opacity_clamped_low() {
        let cfg: Config =
            Config::from_str(r#"{"folders":[],"submenu":{"nonChainItemOpacity":0.05}}"#).unwrap();
        assert!((cfg.submenu.non_chain_item_opacity - 0.2).abs() < 1e-6);
    }

    #[test]
    fn submenu_opacity_clamped_high() {
        let cfg: Config =
            Config::from_str(r#"{"folders":[],"submenu":{"nonChainItemOpacity":2.0}}"#).unwrap();
        assert!((cfg.submenu.non_chain_item_opacity - 1.0).abs() < 1e-6);
    }

    #[test]
    fn submenu_buffer_clamped_high() {
        let cfg: Config =
            Config::from_str(r#"{"folders":[],"submenu":{"hoverBufferPx":999}}"#).unwrap();
        assert_eq!(cfg.submenu.hover_buffer_px, 64);
    }

    #[test]
    fn submenu_long_hover_default_when_missing() {
        let cfg: Config = Config::from_str(r#"{"folders":[]}"#).unwrap();
        assert_eq!(cfg.submenu.long_hover_open_ms, 1200);
    }

    #[test]
    fn submenu_long_hover_clamped_low() {
        let cfg: Config =
            Config::from_str(r#"{"folders":[],"submenu":{"longHoverOpenMs":100}}"#).unwrap();
        assert_eq!(cfg.submenu.long_hover_open_ms, 500);
    }

    #[test]
    fn submenu_long_hover_clamped_high() {
        let cfg: Config =
            Config::from_str(r#"{"folders":[],"submenu":{"longHoverOpenMs":99999}}"#).unwrap();
        assert_eq!(cfg.submenu.long_hover_open_ms, 5000);
    }

    #[test]
    fn submenu_round_trips() {
        let cfg: Config = Config::from_str(
            r#"{"folders":[],"submenu":{"springOpenDelayMs":700,"hoverBufferPx":15,"nonChainItemOpacity":0.7}}"#,
        )
        .unwrap();
        let serialized = serde_json::to_string(&cfg).unwrap();
        let cfg2 = Config::from_str(&serialized).unwrap();
        assert_eq!(
            cfg.submenu.spring_open_delay_ms,
            cfg2.submenu.spring_open_delay_ms
        );
        assert_eq!(cfg.submenu.hover_buffer_px, cfg2.submenu.hover_buffer_px);
        assert!(
            (cfg.submenu.non_chain_item_opacity - cfg2.submenu.non_chain_item_opacity).abs() < 1e-6
        );
    }

    #[test]
    fn folder_entry_kind_defaults_to_folder_when_missing() {
        let cfg: Config =
            Config::from_str(r#"{"folders":[{"name":"Downloads","path":"C:\\Downloads"}]}"#)
                .unwrap();
        assert_eq!(cfg.folders[0].kind, FolderKind::Folder);
    }

    #[test]
    fn folder_entry_kind_deserializes_recent() {
        let cfg: Config =
            Config::from_str(r#"{"folders":[{"name":"Recent","kind":"Recent"}]}"#).unwrap();
        assert_eq!(cfg.folders[0].kind, FolderKind::Recent);
    }

    #[test]
    fn folder_entry_unknown_kind_falls_back_to_folder() {
        // Forward-compat: unknown kinds should NOT fail deserialization.
        let cfg: Config =
            Config::from_str(r#"{"folders":[{"name":"X","path":"C:\\x","kind":"Mystery"}]}"#)
                .unwrap();
        assert_eq!(cfg.folders[0].kind, FolderKind::Folder);
    }

    #[test]
    fn folder_kind_round_trips() {
        let cfg: Config =
            Config::from_str(r#"{"folders":[{"name":"Recent","kind":"Recent"}]}"#).unwrap();
        let json = serde_json::to_string(&cfg).unwrap();
        let cfg2 = Config::from_str(&json).unwrap();
        assert_eq!(cfg2.folders[0].kind, FolderKind::Recent);
    }

    #[test]
    fn recent_config_defaults_when_missing() {
        let cfg: Config = Config::from_str(r#"{"folders":[]}"#).unwrap();
        assert!(!cfg.recent.enabled);
        assert_eq!(cfg.recent.max_count, 5);
        assert!(!cfg.recent.include_pinned);
        assert_eq!(cfg.recent.dwell_seconds_to_track, 10);
        assert!(cfg.recent.excluded_paths.is_empty());
    }

    #[test]
    fn recent_max_count_clamped_low() {
        let cfg: Config = Config::from_str(r#"{"folders":[],"recent":{"maxCount":0}}"#).unwrap();
        assert_eq!(cfg.recent.max_count, 1);
    }

    #[test]
    fn recent_max_count_clamped_high() {
        let cfg: Config = Config::from_str(r#"{"folders":[],"recent":{"maxCount":99}}"#).unwrap();
        assert_eq!(cfg.recent.max_count, 20);
    }

    #[test]
    fn recent_dwell_seconds_clamped() {
        let cfg: Config =
            Config::from_str(r#"{"folders":[],"recent":{"dwellSecondsToTrack":500}}"#).unwrap();
        assert_eq!(cfg.recent.dwell_seconds_to_track, 300);
    }

    #[test]
    fn recent_round_trips() {
        let cfg: Config = Config::from_str(
            r#"{"folders":[],"recent":{"enabled":true,"maxCount":10,"includePinned":true,"dwellSecondsToTrack":30,"excludedPaths":["C:\\private"]}}"#,
        ).unwrap();
        let json = serde_json::to_string(&cfg).unwrap();
        let cfg2 = Config::from_str(&json).unwrap();
        assert_eq!(cfg.recent.enabled, cfg2.recent.enabled);
        assert_eq!(cfg.recent.max_count, cfg2.recent.max_count);
        assert_eq!(cfg.recent.include_pinned, cfg2.recent.include_pinned);
        assert_eq!(
            cfg.recent.dwell_seconds_to_track,
            cfg2.recent.dwell_seconds_to_track
        );
        assert_eq!(cfg.recent.excluded_paths, cfg2.recent.excluded_paths);
    }

    #[test]
    fn watchdog_ms_defaults_to_2000() {
        let cfg = Config::from_str(r#"{"folders":[]}"#).unwrap();
        assert_eq!(cfg.foreground_watchdog_ms, 2000);
        assert!(!cfg.watchdog_reshow);
    }

    #[test]
    fn watchdog_ms_zero_passes_through_as_disabled() {
        let cfg = Config::from_str(r#"{"folders":[],"foregroundWatchdogMs":0}"#).unwrap();
        assert_eq!(cfg.foreground_watchdog_ms, 0);
    }

    #[test]
    fn watchdog_ms_clamped_low() {
        let cfg = Config::from_str(r#"{"folders":[],"foregroundWatchdogMs":50}"#).unwrap();
        assert_eq!(cfg.foreground_watchdog_ms, 500);
    }

    #[test]
    fn watchdog_ms_clamped_high() {
        let cfg = Config::from_str(r#"{"folders":[],"foregroundWatchdogMs":999999}"#).unwrap();
        assert_eq!(cfg.foreground_watchdog_ms, 60_000);
    }

    #[test]
    fn watchdog_reshow_parses_true() {
        let cfg = Config::from_str(r#"{"folders":[],"watchdogReshow":true}"#).unwrap();
        assert!(cfg.watchdog_reshow);
    }
}
