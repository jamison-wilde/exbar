//! Persistence for Recent Folders tracking.
//!
//! `JsonRecentStore` reads/writes `~/.exbar/recents.json`. Load is tolerant:
//! missing file or parse errors return an empty list rather than erroring.
//! Save atomically replaces the file. Delete is idempotent (ignores NotFound).

use crate::error::{ExbarError, ExbarResult};
use crate::recent_list::RecentEntry;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct RecentData {
    #[serde(default)]
    pub entries: Vec<StoredEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StoredEntry {
    pub path: String,
    #[serde(rename = "lastAccessedUnixMs")]
    pub last_accessed_unix_ms: u64,
}

impl From<&RecentEntry> for StoredEntry {
    fn from(e: &RecentEntry) -> Self {
        Self {
            path: e.path.to_string_lossy().into_owned(),
            last_accessed_unix_ms: e.last_accessed_unix_ms,
        }
    }
}

impl From<&StoredEntry> for RecentEntry {
    fn from(s: &StoredEntry) -> Self {
        Self {
            path: std::path::PathBuf::from(&s.path),
            last_accessed_unix_ms: s.last_accessed_unix_ms,
        }
    }
}

pub trait RecentStore: Send + Sync {
    fn load(&self) -> Vec<RecentEntry>;
    fn save(&self, entries: &[RecentEntry]) -> ExbarResult<()>;
    fn delete(&self) -> ExbarResult<()>;
}

#[derive(Default)]
pub struct JsonRecentStore;

impl JsonRecentStore {
    pub fn new() -> Self {
        Self
    }
}

impl RecentStore for JsonRecentStore {
    fn load(&self) -> Vec<RecentEntry> {
        let path = crate::paths::recents_path();
        let Ok(s) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        let data: RecentData = match serde_json::from_str(&s) {
            Ok(d) => d,
            Err(e) => {
                log::warn!("recents.json parse failed: {e}");
                return Vec::new();
            }
        };
        data.entries.iter().map(RecentEntry::from).collect()
    }

    fn save(&self, entries: &[RecentEntry]) -> ExbarResult<()> {
        let path = crate::paths::recents_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ExbarError::io(&*parent.to_string_lossy(), e))?;
        }
        let data = RecentData {
            entries: entries.iter().map(StoredEntry::from).collect(),
        };
        let json =
            serde_json::to_string_pretty(&data).map_err(|e| ExbarError::Config(e.to_string()))?;
        std::fs::write(&path, json).map_err(|e| ExbarError::io(&*path.to_string_lossy(), e))
    }

    fn delete(&self) -> ExbarResult<()> {
        let path = crate::paths::recents_path();
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(ExbarError::io(&*path.to_string_lossy(), e)),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_mocks {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct MockRecentStore {
        pub stored: Mutex<Vec<RecentEntry>>,
        pub save_calls: Mutex<usize>,
        pub delete_calls: Mutex<usize>,
        pub save_should_err: Mutex<bool>,
    }

    impl RecentStore for MockRecentStore {
        fn load(&self) -> Vec<RecentEntry> {
            self.stored.lock().unwrap().clone()
        }

        fn save(&self, entries: &[RecentEntry]) -> ExbarResult<()> {
            *self.save_calls.lock().unwrap() += 1;
            if *self.save_should_err.lock().unwrap() {
                return Err(ExbarError::Config("mock save err".into()));
            }
            *self.stored.lock().unwrap() = entries.to_vec();
            Ok(())
        }

        fn delete(&self) -> ExbarResult<()> {
            *self.delete_calls.lock().unwrap() += 1;
            self.stored.lock().unwrap().clear();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_entry_round_trips_via_serde() {
        let data = RecentData {
            entries: vec![StoredEntry {
                path: "C:\\A".to_string(),
                last_accessed_unix_ms: 12345,
            }],
        };
        let json = serde_json::to_string(&data).unwrap();
        let decoded: RecentData = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.entries[0].path, "C:\\A");
        assert_eq!(decoded.entries[0].last_accessed_unix_ms, 12345);
    }

    #[test]
    fn mock_store_tracks_save_and_delete_calls() {
        let s = test_mocks::MockRecentStore::default();
        s.save(&[]).unwrap();
        s.save(&[]).unwrap();
        s.delete().unwrap();
        assert_eq!(*s.save_calls.lock().unwrap(), 2);
        assert_eq!(*s.delete_calls.lock().unwrap(), 1);
    }

    #[test]
    fn mock_store_save_should_err_propagates() {
        let s = test_mocks::MockRecentStore::default();
        *s.save_should_err.lock().unwrap() = true;
        assert!(s.save(&[]).is_err());
    }
}
