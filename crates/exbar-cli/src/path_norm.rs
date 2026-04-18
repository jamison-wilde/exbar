//! Pure path normalization and prefix-exclusion matching.
//!
//! Normalization rules (Windows-targeted):
//! 1. Case-fold ASCII (drive letters + typical NTFS names).
//! 2. Strip trailing `\` unless the path is a drive root (`C:\` keeps it).
//! 3. Callers that need shell-alias expansion must resolve to a real path
//!    first (via `SHParseDisplayName` + `SHGetPathFromIDListW`) — this module
//!    operates on already-resolved absolute paths.
//!
//! Exclusion match is a **path-prefix** test with a `\` boundary — avoids
//! `C:\priv` matching `C:\private-stuff`.

/// Normalize a Windows path for comparison: ASCII case-fold + trailing-slash strip.
/// Drive roots retain their trailing slash (`C:\` stays `c:\`).
pub fn normalize(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    if is_drive_root(&lower) {
        return lower;
    }
    lower.trim_end_matches('\\').to_string()
}

/// `true` iff `path` (normalized or not) is a drive root like `C:\`.
pub fn is_drive_root(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() == 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\'
}

/// `true` iff `path` is a UNC path (`\\server\share...`).
pub fn is_unc_path(path: &str) -> bool {
    path.starts_with("\\\\")
}

/// `true` iff `path` is covered by any entry in `excluded` under a path-prefix
/// match with a `\\` boundary. Comparison is case-insensitive.
pub fn is_excluded(path: &str, excluded: &[String]) -> bool {
    let n_path = normalize(path);
    for e in excluded {
        let n_excl = normalize(e);
        if n_path == n_excl {
            return true;
        }
        // Prefix with boundary: "c:\\private" should match "c:\\private\\x"
        // but not "c:\\private-stuff".
        if n_path.starts_with(&n_excl)
            && (n_excl.ends_with('\\') || n_path.as_bytes().get(n_excl.len()) == Some(&b'\\'))
        {
            return true;
        }
    }
    false
}

/// Return the parent directory of `path`, or `None` for drive roots and UNC
/// share roots. Output is not normalized — uses the same casing as input for
/// the parent segment, with drive root as `X:\`.
pub fn parent_dir(path: &str) -> Option<String> {
    if is_drive_root(path) {
        return None;
    }
    // UNC share root: \\server\share has no submenu-parent
    if is_unc_path(path) {
        let segments: Vec<&str> = path.trim_start_matches('\\').split('\\').collect();
        if segments.len() <= 2 {
            return None;
        }
    }
    let trimmed = path.trim_end_matches('\\');
    let idx = trimmed.rfind('\\')?;
    let parent = &trimmed[..idx];
    // Drive X followed by colon and nothing else ⇒ rebuild with trailing slash
    if parent.len() == 2 && parent.as_bytes()[1] == b':' {
        return Some(format!("{parent}\\"));
    }
    Some(parent.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_case_folds_and_strips_trailing_slash() {
        assert_eq!(normalize("C:\\Users\\Alice\\"), "c:\\users\\alice");
        assert_eq!(normalize("C:\\USERS\\ALICE"), "c:\\users\\alice");
    }

    #[test]
    fn normalize_preserves_drive_root() {
        assert_eq!(normalize("C:\\"), "c:\\");
        assert_eq!(normalize("D:\\"), "d:\\");
    }

    #[test]
    fn is_drive_root_matches() {
        assert!(is_drive_root("C:\\"));
        assert!(is_drive_root("d:\\"));
        assert!(!is_drive_root("C:\\Users"));
        assert!(!is_drive_root("C:"));
    }

    #[test]
    fn is_unc_path_matches() {
        assert!(is_unc_path("\\\\server\\share"));
        assert!(is_unc_path("\\\\server\\share\\dir"));
        assert!(!is_unc_path("C:\\Users"));
    }

    #[test]
    fn is_excluded_prefix_match_with_boundary() {
        let excl = vec!["C:\\private".to_string()];
        assert!(is_excluded("C:\\private", &excl));
        assert!(is_excluded("C:\\private\\docs", &excl));
        assert!(is_excluded("c:\\PRIVATE\\docs", &excl));
        // Boundary test: partial name match should NOT count.
        assert!(!is_excluded("C:\\private-stuff", &excl));
    }

    #[test]
    fn is_excluded_empty_list_returns_false() {
        assert!(!is_excluded("C:\\anything", &[]));
    }

    #[test]
    fn parent_dir_returns_parent() {
        assert_eq!(
            parent_dir("C:\\Users\\Alice"),
            Some("C:\\Users".to_string())
        );
        assert_eq!(parent_dir("C:\\Users"), Some("C:\\".to_string()));
        assert_eq!(parent_dir("C:\\"), None);
        assert_eq!(parent_dir("\\\\server\\share"), None);
    }

    #[test]
    fn is_excluded_drive_root_matches_children() {
        let excl = vec!["C:\\".to_string()];
        assert!(is_excluded("C:\\Users\\Alice", &excl));
        assert!(is_excluded("c:\\users", &excl));
        assert!(is_excluded("C:\\", &excl));
        // A different drive root does not match.
        assert!(!is_excluded("D:\\Users", &excl));
    }

    #[test]
    fn is_excluded_exclusion_with_trailing_slash_is_boundary_safe() {
        // User typed "C:\\private\\" (with trailing slash). Normalize strips it,
        // so behavior should match the no-slash case.
        let excl = vec!["C:\\private\\".to_string()];
        assert!(is_excluded("C:\\private\\docs", &excl));
        assert!(!is_excluded("C:\\private-stuff", &excl));
    }
}
