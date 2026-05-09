//! Pure reachability cache + network-root classification.
//!
//! See `docs/superpowers/specs/2026-05-09-network-folder-reachability-design.md`
//! for the full design. This module owns no I/O and no Win32 — production
//! probes come through the `ReachabilityProbe` trait in the
//! `reachability_probe` module (added in Task 3).
//!
//! The cache is keyed by **network root** (e.g. `Z:` or `\\server\share`),
//! not full paths, so multiple folder entries on the same share share a
//! single probe result.

/// Per-root reachability state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reachability {
    /// No probe has completed yet. UI treats as Reachable (allow interaction).
    Unknown,
    /// Worker thread is probing this root.
    Probing,
    /// Last probe succeeded.
    Reachable,
    /// Last probe failed or timed out.
    Unreachable,
}

/// Returns `Some(root)` for network paths (mapped drives + UNC),
/// `None` for local paths, shell aliases, or unrecognized forms.
///
/// Mapped drives use the `X:` prefix as the root key.
/// UNC paths use `\\server\share` as the root key (first three backslashes
/// + 4th literal slash boundary).
pub fn classify_root(path: &str) -> Option<String> {
    if path.starts_with("\\\\") {
        return classify_unc(path);
    }
    classify_drive_letter(path)
}

fn classify_drive_letter(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let drive = (bytes[0] as char).to_ascii_uppercase();
        let key = format!("{drive}:");
        if is_remote_drive(&key) {
            return Some(key);
        }
    }
    None
}

fn classify_unc(path: &str) -> Option<String> {
    // Strip leading "\\", split on remaining '\\'.
    let trimmed = &path[2..];
    let mut parts = trimmed.splitn(3, '\\');
    let server = parts.next().filter(|s| !s.is_empty())?;
    let share = parts.next().filter(|s| !s.is_empty())?;
    Some(format!("\\\\{server}\\{share}"))
}

/// Returns true iff `drive` (e.g. `"Z:"`) is `DRIVE_REMOTE` per the OS.
/// Cheap — does not touch the network. Returns false for any failure
/// (caller treats unknown as local-non-network).
fn is_remote_drive(drive: &str) -> bool {
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;
    // From `Win32::System::WindowsProgramming::DRIVE_REMOTE` — inlined to avoid
    // pulling in another feature flag for a single stable u32 constant.
    const DRIVE_REMOTE: u32 = 4;
    let mut wide: Vec<u16> = drive.encode_utf16().collect();
    wide.push(b'\\' as u16);
    wide.push(0);
    let kind = unsafe { GetDriveTypeW(windows_core::PCWSTR(wide.as_ptr())) };
    kind == DRIVE_REMOTE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_unc_extracts_server_share() {
        assert_eq!(
            classify_root("\\\\srv\\share\\foo\\bar"),
            Some("\\\\srv\\share".to_string())
        );
        assert_eq!(
            classify_root("\\\\srv\\share"),
            Some("\\\\srv\\share".to_string())
        );
    }

    #[test]
    fn classify_unc_with_trailing_slash() {
        assert_eq!(
            classify_root("\\\\srv\\share\\"),
            Some("\\\\srv\\share".to_string())
        );
    }

    #[test]
    fn classify_unc_partial_returns_none() {
        // Only server, no share segment.
        assert_eq!(classify_root("\\\\srv"), None);
        assert_eq!(classify_root("\\\\srv\\"), None);
    }

    #[test]
    fn classify_local_drive_returns_none() {
        // C: is virtually always DRIVE_FIXED, so this should be None.
        assert_eq!(classify_root("C:\\Users\\me"), None);
    }

    #[test]
    fn classify_shell_alias_returns_none() {
        assert_eq!(classify_root("shell:downloads"), None);
        assert_eq!(classify_root(""), None);
    }
}
