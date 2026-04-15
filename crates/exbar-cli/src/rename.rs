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
    Started {
        folder_index: usize,
        edit_hwnd: isize,
    },
    CommitRequested {
        text: String,
    },
    Cancelled,
}

/// What the adapter must do as a result of a transition.
///
/// `ApplyRename` triggers a `Config::rename_folder` + `ConfigStore::save`.
/// Empty/whitespace `new_name` is the controller's responsibility to pass
/// through — `Config::rename_folder` enforces the trim-empty rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameAction {
    ApplyRename {
        folder_index: usize,
        new_name: String,
    },
    DestroyEdit {
        edit_hwnd: isize,
    },
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
#[must_use]
pub fn transition(
    state: Option<RenameState>,
    event: RenameEvent,
) -> (Option<RenameState>, Vec<RenameAction>) {
    match (state, event) {
        (
            None,
            RenameEvent::Started {
                folder_index,
                edit_hwnd,
            },
        ) => (
            Some(RenameState {
                folder_index,
                edit_hwnd,
            }),
            Vec::new(),
        ),
        (
            Some(prior),
            RenameEvent::Started {
                folder_index,
                edit_hwnd,
            },
        ) => (
            Some(RenameState {
                folder_index,
                edit_hwnd,
            }),
            vec![RenameAction::DestroyEdit {
                edit_hwnd: prior.edit_hwnd,
            }],
        ),
        (Some(active), RenameEvent::CommitRequested { text }) => (
            None,
            vec![
                RenameAction::ApplyRename {
                    folder_index: active.folder_index,
                    new_name: text,
                },
                RenameAction::DestroyEdit {
                    edit_hwnd: active.edit_hwnd,
                },
                RenameAction::ReloadToolbar,
            ],
        ),
        (Some(active), RenameEvent::Cancelled) => (
            None,
            vec![RenameAction::DestroyEdit {
                edit_hwnd: active.edit_hwnd,
            }],
        ),
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
        RenameState {
            folder_index,
            edit_hwnd,
        }
    }

    #[test]
    fn started_from_idle_sets_state_no_actions() {
        let (next, actions) = transition(
            None,
            RenameEvent::Started {
                folder_index: 3,
                edit_hwnd: 0xABC,
            },
        );
        assert_eq!(next, Some(rs(3, 0xABC)));
        assert!(actions.is_empty());
    }

    #[test]
    fn started_from_active_destroys_old_edit_and_replaces_state() {
        let (next, actions) = transition(
            Some(rs(1, 0x111)),
            RenameEvent::Started {
                folder_index: 2,
                edit_hwnd: 0x222,
            },
        );
        assert_eq!(next, Some(rs(2, 0x222)));
        assert_eq!(
            actions,
            vec![RenameAction::DestroyEdit { edit_hwnd: 0x111 }]
        );
    }

    #[test]
    fn commit_with_text_emits_apply_destroy_reload() {
        let (next, actions) = transition(
            Some(rs(7, 0xDEAD)),
            RenameEvent::CommitRequested {
                text: "Renamed".into(),
            },
        );
        assert_eq!(next, None);
        assert_eq!(
            actions,
            vec![
                RenameAction::ApplyRename {
                    folder_index: 7,
                    new_name: "Renamed".into()
                },
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
            RenameEvent::CommitRequested {
                text: String::new(),
            },
        );
        assert_eq!(next, None);
        assert_eq!(
            actions,
            vec![
                RenameAction::ApplyRename {
                    folder_index: 0,
                    new_name: String::new()
                },
                RenameAction::DestroyEdit { edit_hwnd: 0xBEEF },
                RenameAction::ReloadToolbar,
            ]
        );
    }

    #[test]
    fn cancel_emits_destroy_only() {
        let (next, actions) = transition(Some(rs(4, 0xCAFE)), RenameEvent::Cancelled);
        assert_eq!(next, None);
        assert_eq!(
            actions,
            vec![RenameAction::DestroyEdit { edit_hwnd: 0xCAFE }]
        );
    }

    #[test]
    fn commit_when_idle_is_noop() {
        let (next, actions) = transition(
            None,
            RenameEvent::CommitRequested {
                text: "ignored".into(),
            },
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

    fn arb_state() -> impl Strategy<Value = Option<RenameState>> {
        prop::option::of(
            (0usize..10, 1i64..1_000_000).prop_map(|(folder_index, edit_hwnd)| RenameState {
                folder_index,
                edit_hwnd: edit_hwnd as isize,
            }),
        )
    }

    fn arb_event() -> impl Strategy<Value = RenameEvent> {
        prop_oneof![
            (0usize..10, 1i64..1_000_000).prop_map(|(folder_index, edit_hwnd)| {
                RenameEvent::Started {
                    folder_index,
                    edit_hwnd: edit_hwnd as isize,
                }
            }),
            ".{0,16}".prop_map(|text| RenameEvent::CommitRequested { text }),
            Just(RenameEvent::Cancelled),
        ]
    }

    proptest! {
        /// Same input → same output, every time. (Mirrors pointer.rs's
        /// `transition_is_deterministic`.)
        #[test]
        fn transition_is_deterministic(
            state in arb_state(),
            event in arb_event(),
        ) {
            let (s1, c1) = transition(state.clone(), event.clone());
            let (s2, c2) = transition(state, event);
            prop_assert_eq!(s1, s2);
            prop_assert_eq!(c1, c2);
        }

        /// On `Started`, the new state's fields equal the event's fields —
        /// independent of whether a prior rename was active. This catches
        /// any future bug where double-start fails to fully replace the
        /// prior context.
        #[test]
        fn started_replaces_state_with_event_fields(
            prior in arb_state(),
            new_index in 0usize..10,
            new_hwnd in 1i64..1_000_000,
        ) {
            let event = RenameEvent::Started {
                folder_index: new_index,
                edit_hwnd: new_hwnd as isize,
            };
            let (next, _actions) = transition(prior, event);
            let active = next.expect("Started must yield Some");
            prop_assert_eq!(active.folder_index, new_index);
            prop_assert_eq!(active.edit_hwnd, new_hwnd as isize);
        }

        /// `ApplyRename` is only ever emitted as the first of three actions
        /// in a fixed order: ApplyRename → DestroyEdit → ReloadToolbar.
        /// This catches reordering bugs that would corrupt the commit
        /// sequence (e.g., destroying the edit before reading saved state).
        #[test]
        fn apply_rename_appears_only_in_canonical_commit_sequence(
            state in arb_state(),
            event in arb_event(),
        ) {
            let (_next, actions) = transition(state, event);
            for (i, action) in actions.iter().enumerate() {
                if matches!(action, RenameAction::ApplyRename { .. }) {
                    prop_assert_eq!(i, 0, "ApplyRename must be first action");
                    prop_assert_eq!(actions.len(), 3, "commit sequence has 3 actions");
                    prop_assert!(
                        matches!(actions[1], RenameAction::DestroyEdit { .. }),
                        "second action must be DestroyEdit, got {:?}",
                        actions[1],
                    );
                    prop_assert_eq!(&actions[2], &RenameAction::ReloadToolbar);
                }
            }
        }
    }
}
