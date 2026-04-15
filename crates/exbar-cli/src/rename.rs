//! Inline-rename interaction state machine.
//!
//! Pure. No Win32 dependencies. The `toolbar.rs` adapter translates
//! EDIT-control subclass messages (Enter / Esc / KillFocus) into
//! `RenameEvent`s and executes the returned `RenameAction`s against
//! Win32 (`destroy_rename_edit`, `PostMessageW`) and the `ConfigStore`
//! trait seam.

/// What's currently being renamed. Absent when no rename is in flight.
///
/// `edit_hwnd` is opaque to the controller — it is the EDIT control's
/// `HWND.0`, which the adapter passes back to Win32 for cleanup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameState {
    pub folder_index: usize,
    pub edit_hwnd: isize,
}

/// Events the controller observes. Emitted by the Win32 subclass-proc
/// adapter. The `text` on `CommitRequested` is pre-read from the EDIT by
/// the adapter so the controller stays pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameEvent {
    Started { folder_index: usize, edit_hwnd: isize },
    CommitRequested { text: String },
    Cancelled,
}

/// What the adapter must do as a result of a transition.
///
/// `ApplyRename` triggers a `Config::rename_folder` + `ConfigStore::save`.
/// Empty/whitespace `new_name` is the controller's responsibility to pass
/// through — `Config::rename_folder` enforces the trim-empty rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameAction {
    ApplyRename { folder_index: usize, new_name: String },
    DestroyEdit { edit_hwnd: isize },
    ReloadToolbar,
}

/// Pure transition function. Returns the next state and the actions the
/// adapter must execute, in order.
///
/// Transition table (see spec §"Transition rules"):
///
/// | Current state         | Event                | New state | Actions                                       |
/// |-----------------------|----------------------|-----------|-----------------------------------------------|
/// | `None`                | `Started{i,e}`       | `Some`    | _(none)_                                      |
/// | `Some({_, e_old})`    | `Started{i2,e2}`     | `Some`    | `DestroyEdit{e_old}` (double-start safety net)|
/// | `Some({i, e})`        | `CommitRequested{t}` | `None`    | `ApplyRename{i,t}`, `DestroyEdit{e}`, `ReloadToolbar` |
/// | `Some({_, e})`        | `Cancelled`          | `None`    | `DestroyEdit{e}`                              |
/// | `None`                | `CommitRequested` / `Cancelled` | `None` | _(none — stale event after teardown)_ |
pub fn transition(
    state: Option<RenameState>,
    event: RenameEvent,
) -> (Option<RenameState>, Vec<RenameAction>) {
    match (state, event) {
        (None, RenameEvent::Started { folder_index, edit_hwnd }) => {
            (Some(RenameState { folder_index, edit_hwnd }), Vec::new())
        }
        (Some(prior), RenameEvent::Started { folder_index, edit_hwnd }) => (
            Some(RenameState { folder_index, edit_hwnd }),
            vec![RenameAction::DestroyEdit { edit_hwnd: prior.edit_hwnd }],
        ),
        (Some(active), RenameEvent::CommitRequested { text }) => (
            None,
            vec![
                RenameAction::ApplyRename { folder_index: active.folder_index, new_name: text },
                RenameAction::DestroyEdit { edit_hwnd: active.edit_hwnd },
                RenameAction::ReloadToolbar,
            ],
        ),
        (Some(active), RenameEvent::Cancelled) => {
            (None, vec![RenameAction::DestroyEdit { edit_hwnd: active.edit_hwnd }])
        }
        (None, RenameEvent::CommitRequested { .. }) | (None, RenameEvent::Cancelled) => {
            (None, Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn rs(folder_index: usize, edit_hwnd: isize) -> RenameState {
        RenameState { folder_index, edit_hwnd }
    }

    #[test]
    fn started_from_idle_sets_state_no_actions() {
        let (next, actions) = transition(
            None,
            RenameEvent::Started { folder_index: 3, edit_hwnd: 0xABC },
        );
        assert_eq!(next, Some(rs(3, 0xABC)));
        assert!(actions.is_empty());
    }

    #[test]
    fn started_from_active_destroys_old_edit_and_replaces_state() {
        let (next, actions) = transition(
            Some(rs(1, 0x111)),
            RenameEvent::Started { folder_index: 2, edit_hwnd: 0x222 },
        );
        assert_eq!(next, Some(rs(2, 0x222)));
        assert_eq!(actions, vec![RenameAction::DestroyEdit { edit_hwnd: 0x111 }]);
    }

    #[test]
    fn commit_with_text_emits_apply_destroy_reload() {
        let (next, actions) = transition(
            Some(rs(7, 0xDEAD)),
            RenameEvent::CommitRequested { text: "Renamed".into() },
        );
        assert_eq!(next, None);
        assert_eq!(
            actions,
            vec![
                RenameAction::ApplyRename { folder_index: 7, new_name: "Renamed".into() },
                RenameAction::DestroyEdit { edit_hwnd: 0xDEAD },
                RenameAction::ReloadToolbar,
            ]
        );
    }

    #[test]
    fn commit_with_empty_text_still_emits_apply() {
        // Controller stays pure — the trim-empty rule lives in Config::rename_folder,
        // which is exercised end-to-end by the adapter test
        // `rename_apply_with_empty_text_keeps_old_name`.
        let (next, actions) = transition(
            Some(rs(0, 0xBEEF)),
            RenameEvent::CommitRequested { text: String::new() },
        );
        assert_eq!(next, None);
        assert_eq!(
            actions,
            vec![
                RenameAction::ApplyRename { folder_index: 0, new_name: String::new() },
                RenameAction::DestroyEdit { edit_hwnd: 0xBEEF },
                RenameAction::ReloadToolbar,
            ]
        );
    }

    #[test]
    fn cancel_emits_destroy_only() {
        let (next, actions) = transition(
            Some(rs(4, 0xCAFE)),
            RenameEvent::Cancelled,
        );
        assert_eq!(next, None);
        assert_eq!(actions, vec![RenameAction::DestroyEdit { edit_hwnd: 0xCAFE }]);
    }

    #[test]
    fn commit_when_idle_is_noop() {
        let (next, actions) = transition(
            None,
            RenameEvent::CommitRequested { text: "ignored".into() },
        );
        assert_eq!(next, None);
        assert!(actions.is_empty());
    }

    #[test]
    fn cancel_when_idle_is_noop() {
        let (next, actions) = transition(None, RenameEvent::Cancelled);
        assert_eq!(next, None);
        assert!(actions.is_empty());
    }

    proptest! {
        /// `transition` returns `None` state IFF the event is `CommitRequested`
        /// or `Cancelled`. (Started always yields `Some`.)
        #[test]
        fn next_state_none_iff_event_is_commit_or_cancel(
            prior_index in 0usize..10,
            prior_hwnd in 1i64..1_000_000,
            event_kind in 0u8..3,
            text in ".{0,16}",
            new_index in 0usize..10,
            new_hwnd in 1i64..1_000_000,
        ) {
            let prior = if prior_hwnd % 2 == 0 {
                Some(rs(prior_index, prior_hwnd as isize))
            } else {
                None
            };
            let event = match event_kind {
                0 => RenameEvent::Started { folder_index: new_index, edit_hwnd: new_hwnd as isize },
                1 => RenameEvent::CommitRequested { text },
                _ => RenameEvent::Cancelled,
            };
            let event_is_started = matches!(event, RenameEvent::Started { .. });

            let (next, _actions) = transition(prior, event);

            prop_assert_eq!(next.is_some(), event_is_started);
        }
    }
}
