//! Pure drop-effect determination for the toolbar's `IDropTarget`.
//!
//! The OLE drag-drop protocol exposes keyboard modifiers and a dropeffect
//! value as bitflag-typed Win32 data. This module trades them in and out at
//! the adapter boundary (`dragdrop.rs`) so the decision logic can be tested
//! without touching COM.

use std::path::PathBuf;

/// Modifier keys that affect drop semantics. Replaces
/// `windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyState {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// Result of the effect computation. Replaces
/// `windows::Win32::System::Ole::DROPEFFECT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    None,
    Copy,
    Move,
    Link,
}

/// What action the drop will take if it lands on the current button.
#[derive(Debug, Clone, PartialEq)]
pub enum DropAction {
    /// Drop on a folder button — move or copy the payload there.
    MoveCopyTo { target: PathBuf },
    /// Drop on the `+` button — append the dragged single-directory to
    /// `~/.exbar.json`.
    AddFolder,
}

/// Cached payload metadata for a drag session.
#[derive(Debug, Clone, PartialEq)]
pub struct DragSession {
    /// True if the drag payload is exactly one directory.
    pub is_single_directory: bool,
    /// Source drive letter (uppercase) if the payload is a filesystem path
    /// on a lettered drive; `None` for shell aliases or unresolved sources.
    pub source_drive: Option<char>,
}

/// Compute the effect for a given action/session/keystate tuple.
pub fn effect_for(
    action: Option<&DropAction>,
    session: Option<&DragSession>,
    keystate: KeyState,
) -> Effect {
    match action {
        Some(DropAction::MoveCopyTo { .. }) => {
            let src = session.and_then(|s| s.source_drive);
            let target_drive = target_drive_from_action(action);
            determine_effect(keystate, src, target_drive)
        }
        Some(DropAction::AddFolder) => {
            if session.is_some_and(|s| s.is_single_directory) {
                Effect::Copy
            } else {
                Effect::None
            }
        }
        None => Effect::None,
    }
}

/// Same-drive defaults to `Move`; cross-drive defaults to `Copy`.
/// Ctrl forces `Copy`; Shift forces `Move`.
///
/// Target drive is pre-resolved by the caller — shell alias resolution
/// happens in the adapter, not here.
pub fn determine_effect(
    keystate: KeyState,
    source_drive: Option<char>,
    target_drive: Option<char>,
) -> Effect {
    if keystate.ctrl {
        return Effect::Copy;
    }
    if keystate.shift {
        return Effect::Move;
    }
    match (source_drive, target_drive) {
        (Some(s), Some(t)) if s == t => Effect::Move,
        // Target-drive unknown → default to Move (same-drive is the common case).
        (_, None) => Effect::Move,
        _ => Effect::Copy,
    }
}

fn target_drive_from_action(action: Option<&DropAction>) -> Option<char> {
    match action {
        Some(DropAction::MoveCopyTo { target }) => {
            let s = target.to_string_lossy();
            let mut chars = s.chars();
            let first = chars.next()?;
            if first.is_ascii_alphabetic() && chars.next() == Some(':') {
                Some(first.to_ascii_uppercase())
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn mk_target(path: &str) -> DropAction {
        DropAction::MoveCopyTo {
            target: PathBuf::from(path),
        }
    }

    fn session(drive: Option<char>, single_dir: bool) -> DragSession {
        DragSession {
            source_drive: drive,
            is_single_directory: single_dir,
        }
    }

    #[test]
    fn no_action_gives_none() {
        assert_eq!(effect_for(None, None, KeyState::default()), Effect::None);
        assert_eq!(
            effect_for(None, Some(&session(Some('C'), true)), KeyState::default()),
            Effect::None,
        );
    }

    #[test]
    fn add_folder_with_single_directory_copies() {
        let action = DropAction::AddFolder;
        let s = session(None, true);
        assert_eq!(
            effect_for(Some(&action), Some(&s), KeyState::default()),
            Effect::Copy
        );
    }

    #[test]
    fn add_folder_without_single_directory_none() {
        let action = DropAction::AddFolder;
        let s = session(Some('C'), false);
        assert_eq!(
            effect_for(Some(&action), Some(&s), KeyState::default()),
            Effect::None
        );
        assert_eq!(
            effect_for(Some(&action), None, KeyState::default()),
            Effect::None
        );
    }

    #[test]
    fn same_drive_defaults_to_move() {
        let action = mk_target("C:\\Users\\me\\Documents");
        let s = session(Some('C'), false);
        assert_eq!(
            effect_for(Some(&action), Some(&s), KeyState::default()),
            Effect::Move
        );
    }

    #[test]
    fn cross_drive_defaults_to_copy() {
        let action = mk_target("D:\\Backups");
        let s = session(Some('C'), false);
        assert_eq!(
            effect_for(Some(&action), Some(&s), KeyState::default()),
            Effect::Copy
        );
    }

    #[test]
    fn ctrl_forces_copy_even_on_same_drive() {
        let action = mk_target("C:\\Users\\me\\Documents");
        let s = session(Some('C'), false);
        let k = KeyState {
            ctrl: true,
            ..KeyState::default()
        };
        assert_eq!(effect_for(Some(&action), Some(&s), k), Effect::Copy);
    }

    #[test]
    fn shift_forces_move_even_cross_drive() {
        let action = mk_target("D:\\Backups");
        let s = session(Some('C'), false);
        let k = KeyState {
            shift: true,
            ..KeyState::default()
        };
        assert_eq!(effect_for(Some(&action), Some(&s), k), Effect::Move);
    }

    #[test]
    fn shell_alias_source_defaults_to_copy_cross_drive() {
        // source_drive = None simulates shell: alias unresolved → drive letter unknown.
        let action = mk_target("C:\\Users\\me\\Documents");
        let s = session(None, false);
        // determine_effect: (None, Some('C')) → not same drive, not None-target → Copy
        assert_eq!(
            effect_for(Some(&action), Some(&s), KeyState::default()),
            Effect::Copy
        );
    }

    #[test]
    fn unresolvable_target_defaults_to_move() {
        // Target path without a drive letter (e.g. a shell alias the adapter couldn't resolve).
        let action = mk_target("shell:Downloads");
        let s = session(Some('C'), false);
        assert_eq!(
            effect_for(Some(&action), Some(&s), KeyState::default()),
            Effect::Move
        );
    }

    #[test]
    fn ctrl_wins_over_shift() {
        // Implementation precedence: ctrl is checked first.
        let action = mk_target("C:\\Users\\me");
        let s = session(Some('C'), false);
        let k = KeyState {
            ctrl: true,
            shift: true,
            ..KeyState::default()
        };
        assert_eq!(effect_for(Some(&action), Some(&s), k), Effect::Copy);
    }

    #[test]
    fn determine_effect_both_unknown_gives_move() {
        assert_eq!(
            determine_effect(KeyState::default(), None, None),
            Effect::Move
        );
    }
}
