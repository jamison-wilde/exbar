use std::io::Write;
use tempfile::NamedTempFile;

#[path = "../src/config.rs"]
mod config;

#[test]
fn parse_valid_config() {
    let json = r#"{
        "folders": [
            {"name": "Downloads", "path": "C:\\Users\\test\\Downloads"},
            {"name": "Projects", "path": "C:\\Users\\test\\Projects", "icon": "C:\\icons\\proj.ico"}
        ]
    }"#;
    let cfg = config::Config::from_str(json).unwrap();
    assert_eq!(cfg.folders.len(), 2);
    assert_eq!(cfg.folders[0].name, "Downloads");
    assert_eq!(cfg.folders[0].path, "C:\\Users\\test\\Downloads");
    assert!(cfg.folders[0].icon.is_none());
    assert_eq!(cfg.folders[1].icon.as_deref(), Some("C:\\icons\\proj.ico"));
}

#[test]
fn parse_empty_folders() {
    let json = r#"{"folders": []}"#;
    let cfg = config::Config::from_str(json).unwrap();
    assert!(cfg.folders.is_empty());
}

#[test]
fn parse_missing_file_returns_none() {
    let result = config::Config::load_from_path("C:\\nonexistent\\path\\.exbar.json");
    assert!(result.is_none());
}

#[test]
fn parse_malformed_json_returns_none() {
    let mut f = NamedTempFile::new().unwrap();
    write!(f, "not json at all {{{{").unwrap();
    let result = config::Config::load_from_path(f.path().to_str().unwrap());
    assert!(result.is_none());
}

#[test]
fn config_path_resolves_home() {
    let path = config::default_config_path();
    assert!(path.ends_with(".exbar.json"));
    assert!(path.starts_with("C:\\Users\\") || path.starts_with("/"));
}

#[test]
fn shell_alias_detected() {
    assert!(config::is_shell_alias("shell:downloads"));
    assert!(!config::is_shell_alias("C:\\Users\\test"));
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
    let cfg = config::Config::from_str(json).unwrap();
    let serialized = serde_json::to_string(&cfg).unwrap();
    let cfg2 = config::Config::from_str(&serialized).unwrap();
    assert_eq!(cfg.folders.len(), cfg2.folders.len());
    assert_eq!(cfg.folders[0].name, cfg2.folders[0].name);
    assert_eq!(cfg.folders[1].icon, cfg2.folders[1].icon);
    assert_eq!(cfg.new_tab_timeout_ms_zero_disables, cfg2.new_tab_timeout_ms_zero_disables);
    assert_eq!(cfg.new_tab_timeout_ms_zero_disables, 200);
}

#[test]
fn new_tab_timeout_defaults_to_500_when_missing() {
    let json = r#"{"folders": []}"#;
    let cfg = config::Config::from_str(json).unwrap();
    assert_eq!(cfg.new_tab_timeout_ms_zero_disables, 500);
}

#[test]
fn new_tab_timeout_clamps_to_range() {
    let json = r#"{"folders": [], "newTabTimeoutMsZeroDisables": 99999}"#;
    let cfg = config::Config::from_str(json).unwrap();
    assert_eq!(cfg.new_tab_timeout_ms_zero_disables, 5000);
}
