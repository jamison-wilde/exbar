//! Pure dwell + action hybrid state machine for Recent Folders tracking.
//!
//! Spec §5.3. Adapter feeds events (navigation, foreground-lost,
//! self-initiated click, dwell tick, action in folder). Transitions produce
//! `CommitRecent(path)` or `ClearDwell` commands the adapter applies to the
//! LRU list.
//!
//! Pure — no clock, no I/O. Adapter owns both.

use std::path::PathBuf;

/// Current tracking state.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TrackerState {
    pub current_path: Option<PathBuf>,
    pub dwell_seconds: u32,
    pub skip_next_navigation: bool,
}

#[derive(Debug, Clone)]
pub enum TrackerEvent {
    NavigationTo(PathBuf),
    ForegroundLost,
    SelfInitiated,
    DwellTick,
    ActionInFolder(PathBuf),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TrackerCommand {
    CommitRecent(PathBuf),
    ClearDwell,
}

/// Inputs that affect tracking decisions — caller passes a snapshot.
pub struct TrackerContext<'a> {
    pub dwell_threshold_seconds: u32,
    pub excluded_paths: &'a [String],
}

fn is_drive_root_path(path: &std::path::Path) -> bool {
    crate::path_norm::is_drive_root(&path.to_string_lossy())
}

pub fn transition(
    state: &mut TrackerState,
    event: TrackerEvent,
    ctx: &TrackerContext,
) -> Vec<TrackerCommand> {
    match event {
        TrackerEvent::NavigationTo(path) => {
            if state.skip_next_navigation {
                state.skip_next_navigation = false;
                state.current_path = None;
                state.dwell_seconds = 0;
                return vec![TrackerCommand::ClearDwell];
            }
            let path_str = path.to_string_lossy().to_string();
            if is_drive_root_path(&path)
                || crate::path_norm::is_excluded(&path_str, ctx.excluded_paths)
            {
                state.current_path = None;
                state.dwell_seconds = 0;
                return vec![TrackerCommand::ClearDwell];
            }
            state.current_path = Some(path);
            state.dwell_seconds = 0;
            vec![]
        }
        TrackerEvent::ForegroundLost => {
            state.current_path = None;
            state.dwell_seconds = 0;
            vec![TrackerCommand::ClearDwell]
        }
        TrackerEvent::SelfInitiated => {
            state.skip_next_navigation = true;
            vec![]
        }
        TrackerEvent::DwellTick => {
            let Some(path) = state.current_path.clone() else {
                return vec![];
            };
            state.dwell_seconds += 1;
            if state.dwell_seconds == ctx.dwell_threshold_seconds {
                return vec![TrackerCommand::CommitRecent(path)];
            }
            vec![]
        }
        TrackerEvent::ActionInFolder(path) => {
            let path_str = path.to_string_lossy().to_string();
            if crate::path_norm::is_excluded(&path_str, ctx.excluded_paths) {
                return vec![];
            }
            vec![TrackerCommand::CommitRecent(path)]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }
    fn ctx(excluded: &[String]) -> TrackerContext<'_> {
        TrackerContext {
            dwell_threshold_seconds: 10,
            excluded_paths: excluded,
        }
    }

    #[test]
    fn dwell_commits_at_threshold_not_before() {
        let mut s = TrackerState::default();
        let excluded: Vec<String> = vec![];
        transition(
            &mut s,
            TrackerEvent::NavigationTo(p("C:\\A")),
            &ctx(&excluded),
        );
        for _ in 0..9 {
            let cmds = transition(&mut s, TrackerEvent::DwellTick, &ctx(&excluded));
            assert!(cmds.is_empty());
        }
        let cmds = transition(&mut s, TrackerEvent::DwellTick, &ctx(&excluded));
        assert_eq!(cmds, vec![TrackerCommand::CommitRecent(p("C:\\A"))]);
    }

    #[test]
    fn action_commits_immediately() {
        let mut s = TrackerState::default();
        let excluded: Vec<String> = vec![];
        let cmds = transition(
            &mut s,
            TrackerEvent::ActionInFolder(p("C:\\X")),
            &ctx(&excluded),
        );
        assert_eq!(cmds, vec![TrackerCommand::CommitRecent(p("C:\\X"))]);
    }

    #[test]
    fn self_initiated_skips_next_navigation() {
        let mut s = TrackerState::default();
        let excluded: Vec<String> = vec![];
        transition(&mut s, TrackerEvent::SelfInitiated, &ctx(&excluded));
        transition(
            &mut s,
            TrackerEvent::NavigationTo(p("C:\\A")),
            &ctx(&excluded),
        );
        assert!(s.current_path.is_none());
        assert!(!s.skip_next_navigation);
    }

    #[test]
    fn drive_root_navigation_cleared() {
        let mut s = TrackerState::default();
        let excluded: Vec<String> = vec![];
        transition(
            &mut s,
            TrackerEvent::NavigationTo(p("C:\\")),
            &ctx(&excluded),
        );
        assert!(s.current_path.is_none());
    }

    #[test]
    fn excluded_navigation_cleared() {
        let mut s = TrackerState::default();
        let excluded: Vec<String> = vec!["C:\\private".to_string()];
        transition(
            &mut s,
            TrackerEvent::NavigationTo(p("C:\\private\\docs")),
            &ctx(&excluded),
        );
        assert!(s.current_path.is_none());
    }

    #[test]
    fn foreground_lost_resets_dwell() {
        let mut s = TrackerState::default();
        let excluded: Vec<String> = vec![];
        transition(
            &mut s,
            TrackerEvent::NavigationTo(p("C:\\A")),
            &ctx(&excluded),
        );
        transition(&mut s, TrackerEvent::DwellTick, &ctx(&excluded));
        transition(&mut s, TrackerEvent::ForegroundLost, &ctx(&excluded));
        assert!(s.current_path.is_none());
        assert_eq!(s.dwell_seconds, 0);
    }

    #[test]
    fn rapid_self_initiated_is_idempotent() {
        let mut s = TrackerState::default();
        let excluded: Vec<String> = vec![];
        transition(&mut s, TrackerEvent::SelfInitiated, &ctx(&excluded));
        transition(&mut s, TrackerEvent::SelfInitiated, &ctx(&excluded));
        assert!(s.skip_next_navigation);
        transition(
            &mut s,
            TrackerEvent::NavigationTo(p("C:\\A")),
            &ctx(&excluded),
        );
        assert!(!s.skip_next_navigation);
    }

    #[test]
    fn action_on_excluded_folder_no_commit() {
        let mut s = TrackerState::default();
        let excluded: Vec<String> = vec!["C:\\priv".to_string()];
        let cmds = transition(
            &mut s,
            TrackerEvent::ActionInFolder(p("C:\\priv\\docs")),
            &ctx(&excluded),
        );
        assert!(cmds.is_empty());
    }
}
