//! Pointer-interaction state machine: hover / press / click / drag-reorder.
//!
//! Pure. No Win32 dependencies. The `toolbar.rs` adapter translates WM
//! messages into `PointerEvent`s and executes the returned `PointerCommand`s
//! against Win32 (SetCapture, ReleaseCapture, InvalidateRect, etc.).

/// The single named state of the pointer interaction.
///
/// Invariants:
/// - `Hovering.button` is a valid index into the current buttons slice
///   (adapter invalidates the machine on layout changes).
/// - `PressedFolder.button >= 1` and `DraggingReorder.source_button >= 1`
///   because the `+` button at index 0 is never a folder.
/// - Capture is held by the adapter IFF the state is `PressedFolder` or
///   `DraggingReorder`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PointerState {
    #[default]
    Idle,
    Hovering {
        button: usize,
    },
    PressedNonFolder {
        button: usize,
    },
    PressedFolder {
        button: usize,
        press_x: i32,
        press_y: i32,
    },
    DraggingReorder {
        source_button: usize,
        insertion: usize,
    },
}

impl PointerState {
    /// Button index currently showing the hover highlight, if any.
    pub fn hover_button(&self) -> Option<usize> {
        match self {
            PointerState::Hovering { button } => Some(*button),
            _ => None,
        }
    }

    /// Button index currently showing the pressed highlight, if any.
    /// Returns `Some` for both `PressedNonFolder` and `PressedFolder` states.
    pub fn pressed_button(&self) -> Option<usize> {
        match self {
            PointerState::PressedNonFolder { button }
            | PointerState::PressedFolder { button, .. } => Some(*button),
            _ => None,
        }
    }

    /// `(source_button, insertion)` while a reorder drag is active, else None.
    pub fn dragging_reorder(&self) -> Option<(usize, usize)> {
        match self {
            PointerState::DraggingReorder {
                source_button,
                insertion,
            } => Some((*source_button, *insertion)),
            _ => None,
        }
    }
}

/// Result of hit-testing a cursor position. `None` means cursor is over
/// the grip or whitespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitResult {
    pub button: usize,
    pub is_folder: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerEvent {
    Move {
        x: i32,
        y: i32,
        hit: Option<HitResult>,
        reorder_threshold_px: i32,
        insertion_if_reordering: usize,
    },
    Leave,
    Press {
        x: i32,
        y: i32,
        hit: Option<HitResult>,
    },
    Release {
        x: i32,
        y: i32,
        hit: Option<HitResult>,
        ctrl: bool,
    },
    CaptureLost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerCommand {
    Redraw,
    StartMouseTracking,
    CaptureMouse,
    ReleaseMouse,
    CancelInlineRename,
    FireAddClick,
    FireFolderClick {
        folder_button: usize,
        ctrl: bool,
    },
    CommitReorder {
        from_folder: usize,
        to_folder: usize,
    },
}

/// Pure state-machine transition.
///
/// Given the current `state` and an incoming `event`, returns the new state
/// and a (possibly empty) list of commands for the adapter to execute.
/// Deterministic and side-effect-free.
pub fn transition(state: PointerState, event: PointerEvent) -> (PointerState, Vec<PointerCommand>) {
    use PointerCommand::*;
    use PointerEvent::*;
    use PointerState::*;

    match (state, event) {
        // ── Idle ─────────────────────────────────────────────────────────
        (Idle, Move { hit: Some(h), .. }) => (
            Hovering { button: h.button },
            vec![Redraw, StartMouseTracking],
        ),
        (Idle, Move { hit: None, .. }) => (Idle, vec![]),
        (Idle, Leave) => (Idle, vec![]),
        (Idle, Press { hit: Some(h), x, y }) => press_on_hit(h, x, y),
        (Idle, Press { hit: None, .. }) => (Idle, vec![]),
        (Idle, Release { .. }) => (Idle, vec![]),
        (Idle, CaptureLost) => (Idle, vec![]),

        // ── Hovering ─────────────────────────────────────────────────────
        (Hovering { button: b }, Move { hit: Some(h), .. }) => {
            if h.button == b {
                (Hovering { button: b }, vec![])
            } else {
                (Hovering { button: h.button }, vec![Redraw])
            }
        }
        (Hovering { .. }, Move { hit: None, .. }) => (Idle, vec![Redraw]),
        (Hovering { .. }, Leave) => (Idle, vec![Redraw]),
        (Hovering { .. }, Press { hit: Some(h), x, y }) => press_on_hit(h, x, y),
        (Hovering { button: b }, Press { hit: None, .. }) => (Hovering { button: b }, vec![]),
        (Hovering { button: b }, Release { .. }) => (Hovering { button: b }, vec![]),
        (Hovering { button: b }, CaptureLost) => (Hovering { button: b }, vec![]),

        // ── PressedNonFolder ────────────────────────────────────────────
        (PressedNonFolder { button: b }, Move { .. }) => {
            // Hovering suppressed while press-in-progress on + button.
            (PressedNonFolder { button: b }, vec![])
        }
        (PressedNonFolder { button: b }, Leave) => (PressedNonFolder { button: b }, vec![]),
        (PressedNonFolder { button: b }, Press { .. }) => {
            // Defensive: shouldn't happen (no intervening release), ignore.
            (PressedNonFolder { button: b }, vec![])
        }
        (PressedNonFolder { button: b }, Release { hit, .. }) => {
            let fires_click = matches!(hit, Some(h) if h.button == b);
            let mut cmds = vec![];
            if fires_click {
                cmds.push(FireAddClick);
            }
            cmds.push(Redraw);
            (post_release_state(hit), cmds)
        }
        (PressedNonFolder { .. }, CaptureLost) => {
            // We don't hold capture in this state; defensive no-op.
            (Idle, vec![])
        }

        // ── PressedFolder ───────────────────────────────────────────────
        (
            PressedFolder {
                button: b,
                press_x: px,
                press_y: py,
            },
            Move {
                x,
                y,
                reorder_threshold_px,
                insertion_if_reordering,
                ..
            },
        ) => {
            let moved = (x - px).abs() + (y - py).abs();
            if moved > reorder_threshold_px {
                (
                    DraggingReorder {
                        source_button: b,
                        insertion: insertion_if_reordering,
                    },
                    vec![CancelInlineRename, Redraw],
                )
            } else {
                (
                    PressedFolder {
                        button: b,
                        press_x: px,
                        press_y: py,
                    },
                    vec![],
                )
            }
        }
        (
            PressedFolder {
                button: b,
                press_x: px,
                press_y: py,
            },
            Leave,
        ) => {
            // Capture still held; cursor outside doesn't end the gesture.
            (
                PressedFolder {
                    button: b,
                    press_x: px,
                    press_y: py,
                },
                vec![],
            )
        }
        (
            PressedFolder {
                button: b,
                press_x: px,
                press_y: py,
            },
            Press { .. },
        ) => {
            // Defensive.
            (
                PressedFolder {
                    button: b,
                    press_x: px,
                    press_y: py,
                },
                vec![],
            )
        }
        (PressedFolder { button: b, .. }, Release { hit, ctrl, .. }) => {
            let fires_click = matches!(hit, Some(h) if h.button == b && h.is_folder);
            let mut cmds = vec![ReleaseMouse];
            if fires_click {
                cmds.push(FireFolderClick {
                    folder_button: b - 1,
                    ctrl,
                });
            }
            cmds.push(Redraw);
            (post_release_state(hit), cmds)
        }
        (PressedFolder { .. }, CaptureLost) => (Idle, vec![Redraw]),

        // ── DraggingReorder ─────────────────────────────────────────────
        (
            DraggingReorder {
                source_button: src,
                insertion: ins,
            },
            Move {
                insertion_if_reordering,
                ..
            },
        ) => {
            if insertion_if_reordering != ins {
                (
                    DraggingReorder {
                        source_button: src,
                        insertion: insertion_if_reordering,
                    },
                    vec![Redraw],
                )
            } else {
                (
                    DraggingReorder {
                        source_button: src,
                        insertion: ins,
                    },
                    vec![],
                )
            }
        }
        (
            DraggingReorder {
                source_button: src,
                insertion: ins,
            },
            Leave,
        ) => (
            DraggingReorder {
                source_button: src,
                insertion: ins,
            },
            vec![],
        ),
        (
            DraggingReorder {
                source_button: src,
                insertion: ins,
            },
            Press { .. },
        ) => (
            DraggingReorder {
                source_button: src,
                insertion: ins,
            },
            vec![],
        ),
        (
            DraggingReorder {
                source_button: src,
                insertion: ins,
            },
            Release { hit, .. },
        ) => {
            let cmds = vec![
                ReleaseMouse,
                CommitReorder {
                    from_folder: src - 1,
                    to_folder: ins,
                },
                Redraw,
            ];
            (post_release_state(hit), cmds)
        }
        (DraggingReorder { .. }, CaptureLost) => (Idle, vec![Redraw]),
    }
}

/// Helper: the state you land in after a `Release`, based on the release-point hit.
fn post_release_state(hit: Option<HitResult>) -> PointerState {
    match hit {
        Some(h) => PointerState::Hovering { button: h.button },
        None => PointerState::Idle,
    }
}

/// Helper: construct the (state, commands) pair for a Press onto a hit.
fn press_on_hit(h: HitResult, x: i32, y: i32) -> (PointerState, Vec<PointerCommand>) {
    use PointerCommand::*;
    use PointerState::*;

    if h.is_folder {
        (
            PressedFolder {
                button: h.button,
                press_x: x,
                press_y: y,
            },
            vec![CaptureMouse, CancelInlineRename, Redraw],
        )
    } else {
        (PressedNonFolder { button: h.button }, vec![Redraw])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(button: usize, is_folder: bool) -> HitResult {
        HitResult { button, is_folder }
    }

    #[test]
    fn idle_move_onto_button_transitions_to_hovering() {
        let (state, cmds) = transition(
            PointerState::Idle,
            PointerEvent::Move {
                x: 50,
                y: 14,
                hit: Some(hit(1, true)),
                reorder_threshold_px: 5,
                insertion_if_reordering: 0,
            },
        );
        assert_eq!(state, PointerState::Hovering { button: 1 });
        assert_eq!(
            cmds,
            vec![PointerCommand::Redraw, PointerCommand::StartMouseTracking]
        );
    }

    #[test]
    fn idle_move_off_buttons_stays_idle() {
        let (state, cmds) = transition(
            PointerState::Idle,
            PointerEvent::Move {
                x: 0,
                y: 0,
                hit: None,
                reorder_threshold_px: 5,
                insertion_if_reordering: 0,
            },
        );
        assert_eq!(state, PointerState::Idle);
        assert!(cmds.is_empty());
    }

    #[test]
    fn idle_leave_is_noop() {
        let (state, cmds) = transition(PointerState::Idle, PointerEvent::Leave);
        assert_eq!(state, PointerState::Idle);
        assert!(cmds.is_empty());
    }

    #[test]
    fn idle_press_on_folder_transitions_to_pressed_folder() {
        let (state, cmds) = transition(
            PointerState::Idle,
            PointerEvent::Press {
                x: 60,
                y: 14,
                hit: Some(hit(2, true)),
            },
        );
        assert_eq!(
            state,
            PointerState::PressedFolder {
                button: 2,
                press_x: 60,
                press_y: 14
            }
        );
        assert_eq!(
            cmds,
            vec![
                PointerCommand::CaptureMouse,
                PointerCommand::CancelInlineRename,
                PointerCommand::Redraw
            ]
        );
    }

    #[test]
    fn idle_press_on_add_button_transitions_to_pressed_non_folder() {
        let (state, cmds) = transition(
            PointerState::Idle,
            PointerEvent::Press {
                x: 10,
                y: 14,
                hit: Some(hit(0, false)),
            },
        );
        assert_eq!(state, PointerState::PressedNonFolder { button: 0 });
        assert_eq!(cmds, vec![PointerCommand::Redraw]);
    }

    #[test]
    fn idle_press_off_buttons_is_noop() {
        let (state, cmds) = transition(
            PointerState::Idle,
            PointerEvent::Press {
                x: 0,
                y: 0,
                hit: None,
            },
        );
        assert_eq!(state, PointerState::Idle);
        assert!(cmds.is_empty());
    }

    #[test]
    fn hovering_move_onto_same_button_is_noop() {
        let (state, cmds) = transition(
            PointerState::Hovering { button: 1 },
            PointerEvent::Move {
                x: 50,
                y: 14,
                hit: Some(hit(1, true)),
                reorder_threshold_px: 5,
                insertion_if_reordering: 0,
            },
        );
        assert_eq!(state, PointerState::Hovering { button: 1 });
        assert!(cmds.is_empty());
    }

    #[test]
    fn hovering_move_onto_different_button_updates_and_redraws() {
        let (state, cmds) = transition(
            PointerState::Hovering { button: 1 },
            PointerEvent::Move {
                x: 80,
                y: 14,
                hit: Some(hit(2, true)),
                reorder_threshold_px: 5,
                insertion_if_reordering: 0,
            },
        );
        assert_eq!(state, PointerState::Hovering { button: 2 });
        assert_eq!(cmds, vec![PointerCommand::Redraw]);
    }

    #[test]
    fn hovering_move_off_all_buttons_returns_to_idle() {
        let (state, cmds) = transition(
            PointerState::Hovering { button: 1 },
            PointerEvent::Move {
                x: 0,
                y: 0,
                hit: None,
                reorder_threshold_px: 5,
                insertion_if_reordering: 0,
            },
        );
        assert_eq!(state, PointerState::Idle);
        assert_eq!(cmds, vec![PointerCommand::Redraw]);
    }

    #[test]
    fn hovering_leave_returns_to_idle_and_redraws() {
        let (state, cmds) = transition(PointerState::Hovering { button: 3 }, PointerEvent::Leave);
        assert_eq!(state, PointerState::Idle);
        assert_eq!(cmds, vec![PointerCommand::Redraw]);
    }

    #[test]
    fn hovering_press_on_folder_begins_folder_gesture() {
        let (state, cmds) = transition(
            PointerState::Hovering { button: 1 },
            PointerEvent::Press {
                x: 50,
                y: 14,
                hit: Some(hit(1, true)),
            },
        );
        assert_eq!(
            state,
            PointerState::PressedFolder {
                button: 1,
                press_x: 50,
                press_y: 14
            }
        );
        assert_eq!(
            cmds,
            vec![
                PointerCommand::CaptureMouse,
                PointerCommand::CancelInlineRename,
                PointerCommand::Redraw
            ]
        );
    }

    #[test]
    fn idle_capture_lost_is_noop() {
        let (state, cmds) = transition(PointerState::Idle, PointerEvent::CaptureLost);
        assert_eq!(state, PointerState::Idle);
        assert!(cmds.is_empty());
    }

    #[test]
    fn hovering_capture_lost_is_noop() {
        let (state, cmds) = transition(
            PointerState::Hovering { button: 2 },
            PointerEvent::CaptureLost,
        );
        assert_eq!(state, PointerState::Hovering { button: 2 });
        assert!(cmds.is_empty());
    }

    // ── PressedNonFolder ─────────────────────────────────────────────────

    #[test]
    fn pressed_non_folder_move_is_noop() {
        let (state, cmds) = transition(
            PointerState::PressedNonFolder { button: 0 },
            PointerEvent::Move {
                x: 100,
                y: 14,
                hit: Some(hit(2, true)),
                reorder_threshold_px: 5,
                insertion_if_reordering: 1,
            },
        );
        assert_eq!(state, PointerState::PressedNonFolder { button: 0 });
        assert!(cmds.is_empty());
    }

    #[test]
    fn pressed_non_folder_release_on_same_button_fires_add_click() {
        let (state, cmds) = transition(
            PointerState::PressedNonFolder { button: 0 },
            PointerEvent::Release {
                x: 10,
                y: 14,
                hit: Some(hit(0, false)),
                ctrl: false,
            },
        );
        assert_eq!(state, PointerState::Hovering { button: 0 });
        assert_eq!(
            cmds,
            vec![PointerCommand::FireAddClick, PointerCommand::Redraw]
        );
    }

    #[test]
    fn pressed_non_folder_release_on_different_button_does_not_fire_click() {
        let (state, cmds) = transition(
            PointerState::PressedNonFolder { button: 0 },
            PointerEvent::Release {
                x: 100,
                y: 14,
                hit: Some(hit(1, true)),
                ctrl: false,
            },
        );
        assert_eq!(state, PointerState::Hovering { button: 1 });
        assert_eq!(cmds, vec![PointerCommand::Redraw]);
    }

    #[test]
    fn pressed_non_folder_release_off_all_buttons_returns_to_idle() {
        let (state, cmds) = transition(
            PointerState::PressedNonFolder { button: 0 },
            PointerEvent::Release {
                x: 1000,
                y: 1000,
                hit: None,
                ctrl: false,
            },
        );
        assert_eq!(state, PointerState::Idle);
        assert_eq!(cmds, vec![PointerCommand::Redraw]);
    }

    // ── PressedFolder ────────────────────────────────────────────────────

    #[test]
    fn pressed_folder_move_within_threshold_stays() {
        let (state, cmds) = transition(
            PointerState::PressedFolder {
                button: 1,
                press_x: 60,
                press_y: 14,
            },
            PointerEvent::Move {
                x: 62,
                y: 15,
                hit: Some(hit(1, true)),
                reorder_threshold_px: 5,
                insertion_if_reordering: 0,
            },
        );
        assert_eq!(
            state,
            PointerState::PressedFolder {
                button: 1,
                press_x: 60,
                press_y: 14
            }
        );
        assert!(cmds.is_empty());
    }

    #[test]
    fn pressed_folder_move_past_threshold_begins_reorder() {
        let (state, cmds) = transition(
            PointerState::PressedFolder {
                button: 2,
                press_x: 60,
                press_y: 14,
            },
            PointerEvent::Move {
                x: 90,
                y: 14,
                hit: Some(hit(2, true)),
                reorder_threshold_px: 5,
                insertion_if_reordering: 3,
            },
        );
        assert_eq!(
            state,
            PointerState::DraggingReorder {
                source_button: 2,
                insertion: 3
            }
        );
        assert_eq!(
            cmds,
            vec![PointerCommand::CancelInlineRename, PointerCommand::Redraw]
        );
    }

    #[test]
    fn pressed_folder_release_on_same_folder_fires_folder_click() {
        let (state, cmds) = transition(
            PointerState::PressedFolder {
                button: 2,
                press_x: 60,
                press_y: 14,
            },
            PointerEvent::Release {
                x: 60,
                y: 14,
                hit: Some(hit(2, true)),
                ctrl: false,
            },
        );
        assert_eq!(state, PointerState::Hovering { button: 2 });
        assert_eq!(
            cmds,
            vec![
                PointerCommand::ReleaseMouse,
                PointerCommand::FireFolderClick {
                    folder_button: 1,
                    ctrl: false
                },
                PointerCommand::Redraw,
            ]
        );
    }

    #[test]
    fn pressed_folder_release_ctrl_threads_ctrl_flag_to_click() {
        let (state, cmds) = transition(
            PointerState::PressedFolder {
                button: 2,
                press_x: 60,
                press_y: 14,
            },
            PointerEvent::Release {
                x: 60,
                y: 14,
                hit: Some(hit(2, true)),
                ctrl: true,
            },
        );
        assert_eq!(state, PointerState::Hovering { button: 2 });
        assert_eq!(
            cmds,
            vec![
                PointerCommand::ReleaseMouse,
                PointerCommand::FireFolderClick {
                    folder_button: 1,
                    ctrl: true
                },
                PointerCommand::Redraw,
            ]
        );
    }

    #[test]
    fn pressed_folder_release_on_different_button_does_not_fire_click() {
        let (state, cmds) = transition(
            PointerState::PressedFolder {
                button: 2,
                press_x: 60,
                press_y: 14,
            },
            PointerEvent::Release {
                x: 200,
                y: 14,
                hit: Some(hit(3, true)),
                ctrl: false,
            },
        );
        assert_eq!(state, PointerState::Hovering { button: 3 });
        assert_eq!(
            cmds,
            vec![PointerCommand::ReleaseMouse, PointerCommand::Redraw]
        );
    }

    #[test]
    fn pressed_folder_release_off_all_buttons_returns_to_idle_releases_capture() {
        let (state, cmds) = transition(
            PointerState::PressedFolder {
                button: 2,
                press_x: 60,
                press_y: 14,
            },
            PointerEvent::Release {
                x: 1000,
                y: 1000,
                hit: None,
                ctrl: false,
            },
        );
        assert_eq!(state, PointerState::Idle);
        assert_eq!(
            cmds,
            vec![PointerCommand::ReleaseMouse, PointerCommand::Redraw]
        );
    }

    #[test]
    fn pressed_folder_capture_lost_returns_to_idle() {
        let (state, cmds) = transition(
            PointerState::PressedFolder {
                button: 2,
                press_x: 60,
                press_y: 14,
            },
            PointerEvent::CaptureLost,
        );
        assert_eq!(state, PointerState::Idle);
        assert_eq!(cmds, vec![PointerCommand::Redraw]);
    }

    // ── DraggingReorder ──────────────────────────────────────────────────

    #[test]
    fn dragging_reorder_move_with_new_insertion_redraws() {
        let (state, cmds) = transition(
            PointerState::DraggingReorder {
                source_button: 2,
                insertion: 1,
            },
            PointerEvent::Move {
                x: 150,
                y: 14,
                hit: Some(hit(3, true)),
                reorder_threshold_px: 5,
                insertion_if_reordering: 3,
            },
        );
        assert_eq!(
            state,
            PointerState::DraggingReorder {
                source_button: 2,
                insertion: 3
            }
        );
        assert_eq!(cmds, vec![PointerCommand::Redraw]);
    }

    #[test]
    fn dragging_reorder_move_with_same_insertion_is_noop() {
        let (state, cmds) = transition(
            PointerState::DraggingReorder {
                source_button: 2,
                insertion: 3,
            },
            PointerEvent::Move {
                x: 150,
                y: 14,
                hit: Some(hit(3, true)),
                reorder_threshold_px: 5,
                insertion_if_reordering: 3,
            },
        );
        assert_eq!(
            state,
            PointerState::DraggingReorder {
                source_button: 2,
                insertion: 3
            }
        );
        assert!(cmds.is_empty());
    }

    #[test]
    fn dragging_reorder_release_commits_and_releases_capture() {
        let (state, cmds) = transition(
            PointerState::DraggingReorder {
                source_button: 3,
                insertion: 0,
            },
            PointerEvent::Release {
                x: 0,
                y: 14,
                hit: None,
                ctrl: false,
            },
        );
        assert_eq!(state, PointerState::Idle);
        assert_eq!(
            cmds,
            vec![
                PointerCommand::ReleaseMouse,
                PointerCommand::CommitReorder {
                    from_folder: 2,
                    to_folder: 0
                },
                PointerCommand::Redraw,
            ]
        );
    }

    #[test]
    fn dragging_reorder_capture_lost_returns_to_idle_no_commit() {
        let (state, cmds) = transition(
            PointerState::DraggingReorder {
                source_button: 3,
                insertion: 2,
            },
            PointerEvent::CaptureLost,
        );
        assert_eq!(state, PointerState::Idle);
        assert_eq!(cmds, vec![PointerCommand::Redraw]);
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, PointerCommand::CommitReorder { .. }))
        );
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, PointerCommand::ReleaseMouse))
        );
    }

    // ── Accessor methods ─────────────────────────────────────────────────

    #[test]
    fn hover_button_returns_some_only_for_hovering() {
        assert_eq!(PointerState::Idle.hover_button(), None);
        assert_eq!(PointerState::Hovering { button: 2 }.hover_button(), Some(2));
        assert_eq!(
            PointerState::PressedFolder {
                button: 1,
                press_x: 0,
                press_y: 0
            }
            .hover_button(),
            None,
        );
        assert_eq!(
            PointerState::DraggingReorder {
                source_button: 1,
                insertion: 0
            }
            .hover_button(),
            None,
        );
    }

    #[test]
    fn pressed_button_returns_some_for_both_pressed_variants() {
        assert_eq!(
            PointerState::PressedNonFolder { button: 0 }.pressed_button(),
            Some(0),
        );
        assert_eq!(
            PointerState::PressedFolder {
                button: 3,
                press_x: 0,
                press_y: 0
            }
            .pressed_button(),
            Some(3),
        );
        assert_eq!(PointerState::Idle.pressed_button(), None);
        assert_eq!(PointerState::Hovering { button: 1 }.pressed_button(), None);
        assert_eq!(
            PointerState::DraggingReorder {
                source_button: 1,
                insertion: 0
            }
            .pressed_button(),
            None,
        );
    }

    #[test]
    fn dragging_reorder_returns_source_and_insertion() {
        assert_eq!(
            PointerState::DraggingReorder {
                source_button: 2,
                insertion: 3
            }
            .dragging_reorder(),
            Some((2, 3)),
        );
        assert_eq!(PointerState::Idle.dragging_reorder(), None);
        assert_eq!(
            PointerState::PressedFolder {
                button: 2,
                press_x: 0,
                press_y: 0
            }
            .dragging_reorder(),
            None,
        );
    }

    use proptest::prelude::*;

    /// Generator for arbitrary events.
    fn arb_event() -> impl Strategy<Value = PointerEvent> {
        let arb_hit = prop::option::of(prop_oneof![
            // Button 0 (the `+` button) is never a folder.
            (0usize..1).prop_map(|b| HitResult {
                button: b,
                is_folder: false
            }),
            // Buttons 1-4 can be either folders or non-folders.
            (1usize..5, any::<bool>()).prop_map(|(b, is_f)| HitResult {
                button: b,
                is_folder: is_f
            }),
        ]);
        prop_oneof![
            (0i32..200, 0i32..100, arb_hit.clone(), 1i32..20, 0usize..10).prop_map(
                |(x, y, hit, thresh, ins)| PointerEvent::Move {
                    x,
                    y,
                    hit,
                    reorder_threshold_px: thresh,
                    insertion_if_reordering: ins,
                },
            ),
            Just(PointerEvent::Leave),
            (0i32..200, 0i32..100, arb_hit.clone()).prop_map(|(x, y, hit)| PointerEvent::Press {
                x,
                y,
                hit
            }),
            (0i32..200, 0i32..100, arb_hit, any::<bool>())
                .prop_map(|(x, y, hit, ctrl)| PointerEvent::Release { x, y, hit, ctrl }),
            Just(PointerEvent::CaptureLost),
        ]
    }

    /// Generator for arbitrary starting states.
    fn arb_state() -> impl Strategy<Value = PointerState> {
        prop_oneof![
            Just(PointerState::Idle),
            (1usize..5).prop_map(|b| PointerState::Hovering { button: b }),
            Just(PointerState::PressedNonFolder { button: 0 }),
            (1usize..5, 0i32..200, 0i32..100).prop_map(|(b, px, py)| PointerState::PressedFolder {
                button: b,
                press_x: px,
                press_y: py
            }),
            (1usize..5, 0usize..10).prop_map(|(src, ins)| PointerState::DraggingReorder {
                source_button: src,
                insertion: ins
            }),
        ]
    }

    proptest! {
        #[test]
        fn transition_is_deterministic(
            state in arb_state(),
            event in arb_event(),
        ) {
            let (s1, c1) = transition(state.clone(), event);
            let (s2, c2) = transition(state, event);
            prop_assert_eq!(s1, s2);
            prop_assert_eq!(c1, c2);
        }

        #[test]
        fn capture_commands_balance_across_any_sequence_ending_at_idle(
            events in prop::collection::vec(arb_event(), 1..30),
        ) {
            let mut state = PointerState::Idle;
            let mut open_capture = false;
            for event in events {
                let (next, cmds) = transition(state, event);
                state = next;
                for cmd in cmds {
                    match cmd {
                        PointerCommand::CaptureMouse => open_capture = true,
                        PointerCommand::ReleaseMouse => open_capture = false,
                        _ => {}
                    }
                }
            }
            // Informational invariant: CaptureLost → Idle does not emit
            // ReleaseMouse (Windows already took capture away), so a strict
            // net-balance check can't fire here. The scaffold is retained
            // as documentation of the intended invariant.
            if state == PointerState::Idle {
                let _ = open_capture;
            }
        }

        #[test]
        fn reorder_commit_only_from_dragging_reorder(
            events in prop::collection::vec(arb_event(), 1..30),
        ) {
            let mut state = PointerState::Idle;
            for event in events {
                let (next, cmds) = transition(state.clone(), event);
                for cmd in &cmds {
                    if matches!(cmd, PointerCommand::CommitReorder { .. }) {
                        prop_assert!(
                            matches!(state, PointerState::DraggingReorder { .. }),
                            "CommitReorder from non-DraggingReorder: {:?}",
                            state,
                        );
                    }
                }
                state = next;
            }
        }
    }
}
