use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    Horizontal,
    Vertical,
}

impl Default for Layout {
    fn default() -> Self {
        Layout::Horizontal
    }
}

fn default_opacity() -> f32 {
    0.8
}
fn default_new_tab_timeout() -> u32 {
    500
}

fn deserialize_clamped_timeout<'de, D>(d: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = u32::deserialize(d)?;
    Ok(v.min(5000))
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    pub folders: Vec<FolderEntry>,
    #[serde(default)]
    pub layout: Layout,
    #[serde(default = "default_opacity")]
    pub background_opacity: f32,
    #[serde(
        rename = "newTabTimeoutMsZeroDisables",
        default = "default_new_tab_timeout",
        deserialize_with = "deserialize_clamped_timeout"
    )]
    pub new_tab_timeout_ms_zero_disables: u32,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FolderEntry {
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
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

    #[cfg_attr(test, allow(dead_code))]
    pub fn load() -> Option<Config> {
        let path = default_config_path();
        Self::load_from_path(&path)
    }

    pub fn add_folder(&mut self, name: String, path: String) {
        self.folders.push(FolderEntry {
            name,
            path,
            icon: None,
        });
    }

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

    pub fn save_to_path(&self, path: &str) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        fs::write(path, json)
    }

    #[cfg_attr(test, allow(dead_code))]
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to_path(&default_config_path())
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

#[allow(dead_code)]
pub fn is_shell_alias(path: &str) -> bool {
    path.starts_with("shell:")
}
