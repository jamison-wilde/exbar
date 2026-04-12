use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    Horizontal,
    Vertical,
}

impl Default for Layout {
    fn default() -> Self { Layout::Horizontal }
}

fn default_opacity() -> f32 { 0.8 }

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub folders: Vec<FolderEntry>,
    #[serde(default)]
    pub layout: Layout,
    #[serde(default = "default_opacity")]
    pub background_opacity: f32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FolderEntry {
    pub name: String,
    pub path: String,
    pub icon: Option<String>,
}

impl Config {
    pub fn from_str(json: &str) -> Option<Config> {
        serde_json::from_str(json).ok()
    }

    pub fn load_from_path(path: &str) -> Option<Config> {
        let contents = fs::read_to_string(path).ok()?;
        Self::from_str(&contents)
    }

    pub fn load() -> Option<Config> {
        let path = default_config_path();
        Self::load_from_path(&path)
    }
}

pub fn default_config_path() -> String {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| String::from("C:\\Users\\Default"));
    let mut path = PathBuf::from(home);
    path.push(".exbar.json");
    path.to_string_lossy().into_owned()
}

pub fn is_shell_alias(path: &str) -> bool {
    path.starts_with("shell:")
}
