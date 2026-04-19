//! Pure LRU list operations for recent folders. Dedup on push (normalized
//! path compare), trim to max, exclusion-filter on push.
//!
//! Consumers apply this to an in-memory `Vec<RecentEntry>`; persistence
//! lives in `recent_store.rs`.

use std::path::{Path, PathBuf};

/// One entry in the recent list. Order within the vec = LRU (index 0 = most recent).
#[derive(Debug, Clone, PartialEq)]
pub struct RecentEntry {
    pub path: PathBuf,
    pub last_accessed_unix_ms: u64,
}

/// Push `path` to the front (most-recent) slot. If the path is already present
/// (by normalized compare), it's moved to front instead of duplicated. If the
/// path is in `excluded`, the list is unchanged.
///
/// After push, trim to `max_count` entries (drop oldest).
pub fn push(
    entries: &mut Vec<RecentEntry>,
    path: &Path,
    now_unix_ms: u64,
    max_count: usize,
    excluded: &[String],
) {
    let path_str = path.to_string_lossy().to_string();
    if crate::path_norm::is_excluded(&path_str, excluded) {
        return;
    }
    let norm_new = crate::path_norm::normalize(&path_str);
    entries.retain(|e| crate::path_norm::normalize(&e.path.to_string_lossy()) != norm_new);
    entries.insert(
        0,
        RecentEntry {
            path: path.to_path_buf(),
            last_accessed_unix_ms: now_unix_ms,
        },
    );
    if entries.len() > max_count {
        entries.truncate(max_count);
    }
}

/// Filter out entries that should NOT be shown: those matching any `pinned`
/// path when `include_pinned == false`. Returns a NEW vec; does not mutate.
pub fn for_display(
    entries: &[RecentEntry],
    pinned: &[String],
    include_pinned: bool,
) -> Vec<RecentEntry> {
    if include_pinned {
        return entries.to_vec();
    }
    entries
        .iter()
        .filter(|e| {
            let norm = crate::path_norm::normalize(&e.path.to_string_lossy());
            !pinned
                .iter()
                .any(|p| crate::path_norm::normalize(p) == norm)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn push_inserts_at_front() {
        let mut list = Vec::new();
        push(&mut list, &p("C:\\A"), 100, 5, &[]);
        push(&mut list, &p("C:\\B"), 200, 5, &[]);
        assert_eq!(list[0].path, p("C:\\B"));
        assert_eq!(list[1].path, p("C:\\A"));
    }

    #[test]
    fn push_dedups_moves_to_front() {
        let mut list = Vec::new();
        push(&mut list, &p("C:\\A"), 100, 5, &[]);
        push(&mut list, &p("C:\\B"), 200, 5, &[]);
        push(&mut list, &p("C:\\A"), 300, 5, &[]);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].path, p("C:\\A"));
        assert_eq!(list[0].last_accessed_unix_ms, 300);
    }

    #[test]
    fn push_case_insensitive_dedup() {
        let mut list = Vec::new();
        push(&mut list, &p("C:\\A"), 100, 5, &[]);
        push(&mut list, &p("c:\\a"), 200, 5, &[]);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].last_accessed_unix_ms, 200);
    }

    #[test]
    fn push_trims_to_max() {
        let mut list = Vec::new();
        for i in 0..10 {
            push(&mut list, &p(&format!("C:\\{i}")), i as u64, 3, &[]);
        }
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].path, p("C:\\9"));
    }

    #[test]
    fn push_respects_exclusion() {
        let mut list = Vec::new();
        push(
            &mut list,
            &p("C:\\private\\docs"),
            100,
            5,
            &["C:\\private".to_string()],
        );
        assert!(list.is_empty());
    }

    #[test]
    fn for_display_filters_pinned_when_include_pinned_false() {
        let list = vec![
            RecentEntry {
                path: p("C:\\A"),
                last_accessed_unix_ms: 100,
            },
            RecentEntry {
                path: p("C:\\B"),
                last_accessed_unix_ms: 200,
            },
        ];
        let pinned = vec!["c:\\a".to_string()];
        let out = for_display(&list, &pinned, false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, p("C:\\B"));
    }

    #[test]
    fn for_display_includes_pinned_when_include_pinned_true() {
        let list = vec![RecentEntry {
            path: p("C:\\A"),
            last_accessed_unix_ms: 100,
        }];
        let pinned = vec!["C:\\A".to_string()];
        let out = for_display(&list, &pinned, true);
        assert_eq!(out.len(), 1);
    }
}
