//! Subfolder enumeration for submenus: lists sorted subdirectories of a
//! directory, with a shallow "has children" probe for the ▸ indicator.
//!
//! Budget semantics: a global 50 ms wall-clock budget across the probe pass
//! (not per-item). On exhaustion, remaining items unconditionally set
//! `has_children = true`.
//!
//! UNC paths (`\\server\share\...`) skip the probe entirely — the cost of
//! round-tripping to a slow network share is not worth the polish.

use crate::error::ExbarResult;
use std::path::{Path, PathBuf};

/// One entry in a submenu listing (populated by `SubfolderSource::list`).
#[derive(Debug, Clone, PartialEq)]
pub struct SubfolderEntry {
    pub name: String,
    pub path: PathBuf,
    pub has_children: bool,
}

/// Trait seam for subfolder enumeration. Production: `Win32SubfolderSource`
/// (reads the filesystem). Tests: `MockSubfolderSource`.
pub trait SubfolderSource: Send + Sync {
    /// Enumerate subdirectories of `parent`, sorted case-insensitively by
    /// name, capped at `max_items` entries. On cap overflow, the last
    /// element is an "ellipsis sentinel" with `name = "…(more)"`,
    /// `path = parent`, `has_children = false`.
    ///
    /// `has_children` is populated under the 50 ms global probe budget
    /// described in the module docs, or unconditionally `true` for UNC paths.
    fn list(&self, parent: &Path, max_items: usize) -> ExbarResult<Vec<SubfolderEntry>>;
}

use std::time::{Duration, Instant};

/// Production `SubfolderSource` that reads the real filesystem via
/// `std::fs::read_dir` (which internally uses `FindFirstFileW` on Windows).
///
/// Hidden directories (name starting with `.`) are excluded.
/// System / reparse points (junctions, symlinks) are included.
#[derive(Default)]
pub struct Win32SubfolderSource;

const HAS_CHILDREN_BUDGET_MS: u64 = 50;

impl SubfolderSource for Win32SubfolderSource {
    fn list(&self, parent: &Path, max_items: usize) -> ExbarResult<Vec<SubfolderEntry>> {
        let is_unc = crate::path_norm::is_unc_path(parent.to_string_lossy().as_ref());
        let rd = std::fs::read_dir(parent)
            .map_err(|e| crate::error::ExbarError::io(&*parent.to_string_lossy(), e))?;

        let mut dirs: Vec<(String, PathBuf)> = Vec::new();
        for entry in rd.flatten() {
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if !ft.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            dirs.push((name, entry.path()));
        }
        dirs.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));

        // Apply cap + ellipsis sentinel.
        let overflow = dirs.len() > max_items;
        let keep = if overflow {
            max_items.saturating_sub(1)
        } else {
            dirs.len()
        };
        dirs.truncate(keep);

        // Probe pass with 50 ms global budget; UNC skips and returns true.
        let deadline = Instant::now() + Duration::from_millis(HAS_CHILDREN_BUDGET_MS);
        let mut out: Vec<SubfolderEntry> =
            Vec::with_capacity(dirs.len() + if overflow { 1 } else { 0 });
        for (name, path) in dirs {
            let has_children =
                if is_unc || Instant::now() >= deadline { true } else { probe_has_child_dir(&path) };
            out.push(SubfolderEntry {
                name,
                path,
                has_children,
            });
        }
        if overflow {
            out.push(SubfolderEntry {
                name: "…(more)".to_string(),
                path: parent.to_path_buf(),
                has_children: false,
            });
        }
        Ok(out)
    }
}

fn probe_has_child_dir(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in rd.flatten() {
        if let Ok(ft) = entry.file_type()
            && ft.is_dir()
        {
            let n = entry.file_name().to_string_lossy().to_string();
            if !n.starts_with('.') {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
pub(crate) mod test_mocks {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory `SubfolderSource`. Test seed: `set_children(parent, vec![...])`.
    #[derive(Default)]
    pub struct MockSubfolderSource {
        pub children: Mutex<HashMap<PathBuf, Vec<SubfolderEntry>>>,
        pub list_calls: Mutex<Vec<PathBuf>>,
        pub err_on_next: Mutex<bool>,
    }

    impl MockSubfolderSource {
        #[allow(dead_code)]
        pub fn set_children(&self, parent: &Path, entries: Vec<SubfolderEntry>) {
            self.children
                .lock()
                .unwrap()
                .insert(parent.to_path_buf(), entries);
        }
    }

    impl SubfolderSource for MockSubfolderSource {
        fn list(&self, parent: &Path, max_items: usize) -> ExbarResult<Vec<SubfolderEntry>> {
            self.list_calls.lock().unwrap().push(parent.to_path_buf());
            if *self.err_on_next.lock().unwrap() {
                *self.err_on_next.lock().unwrap() = false;
                return Err(crate::error::ExbarError::Config(
                    "mock subfolder err".into(),
                ));
            }
            let map = self.children.lock().unwrap();
            let items = map.get(parent).cloned().unwrap_or_default();
            if items.len() > max_items {
                let mut capped = items[..max_items.saturating_sub(1)].to_vec();
                capped.push(SubfolderEntry {
                    name: "…(more)".to_string(),
                    path: parent.to_path_buf(),
                    has_children: false,
                });
                Ok(capped)
            } else {
                Ok(items)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn lists_local_subfolders_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("Zulu")).unwrap();
        fs::create_dir(tmp.path().join("alpha")).unwrap();
        fs::create_dir(tmp.path().join("Bravo")).unwrap();
        fs::write(tmp.path().join("ignored.txt"), "").unwrap();

        let src = Win32SubfolderSource;
        let items = src.list(tmp.path(), 200).unwrap();

        let names: Vec<String> = items.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, vec!["alpha", "Bravo", "Zulu"]);
    }

    #[test]
    fn hidden_dirs_excluded() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("visible")).unwrap();
        fs::create_dir(tmp.path().join(".hidden")).unwrap();

        let src = Win32SubfolderSource;
        let items = src.list(tmp.path(), 200).unwrap();
        let names: Vec<&str> = items.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["visible"]);
    }

    #[test]
    fn cap_overflow_appends_ellipsis_sentinel() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fs::create_dir(tmp.path().join(format!("dir{i}"))).unwrap();
        }
        let src = Win32SubfolderSource;
        let items = src.list(tmp.path(), 3).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[2].name, "…(more)");
        assert!(!items[2].has_children);
    }

    #[test]
    fn has_children_detects_nested_dir() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("parent")).unwrap();
        fs::create_dir(tmp.path().join("parent").join("child")).unwrap();
        fs::create_dir(tmp.path().join("leaf")).unwrap();

        let src = Win32SubfolderSource;
        let items = src.list(tmp.path(), 200).unwrap();
        let parent = items.iter().find(|e| e.name == "parent").unwrap();
        let leaf = items.iter().find(|e| e.name == "leaf").unwrap();
        assert!(parent.has_children);
        assert!(!leaf.has_children);
    }
}
