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
    let result = config::Config::load_from_path("C:\\nonexistent\\path\\.tabplorer.json");
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
    assert!(path.ends_with(".tabplorer.json"));
    assert!(path.starts_with("C:\\Users\\") || path.starts_with("/"));
}

#[test]
fn shell_alias_detected() {
    assert!(config::is_shell_alias("shell:downloads"));
    assert!(!config::is_shell_alias("C:\\Users\\test"));
}
