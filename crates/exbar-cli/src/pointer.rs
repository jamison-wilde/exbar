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
    Hovering { button: usize },
    PressedNonFolder { button: usize },
    PressedFolder { button: usize, press_x: i32, press_y: i32 },
    DraggingReorder { source_button: usize, insertion: usize },
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
    FireFolderClick { folder_button: usize, ctrl: bool },
    CommitReorder { from_folder: usize, to_folder: usize },
}

/// Pure state-machine transition.
///
/// Given the current `state` and an incoming `event`, returns the new state
/// and a (possibly empty) list of commands for the adapter to execute.
/// Deterministic and side-effect-free.
pub fn transition(
    state: PointerState,
    event: PointerEvent,
) -> (PointerState, Vec<PointerCommand>) {
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
        (Hovering { button: b }, Press { hit: None, .. }) => {
            (Hovering { button: b }, vec![])
        }
        (Hovering { button: b }, Release { .. }) => (Hovering { button: b }, vec![]),
        (Hovering { button: b }, CaptureLost) => (Hovering { button: b }, vec![]),

        // ── PressedNonFolder / PressedFolder / DraggingReorder ──────────
        // (Implemented in Task 3.)
        (s, _) => (s, vec![]),
    }
}

/// Helper: construct the (state, commands) pair for a Press onto a hit.
fn press_on_hit(
    h: HitResult,
    x: i32,
    y: i32,
) -> (PointerState, Vec<PointerCommand>) {
    use PointerCommand::*;
    use PointerState::*;

    if h.is_folder {
        (
            PressedFolder { button: h.button, press_x: x, press_y: y },
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
            PointerEvent::Press { x: 60, y: 14, hit: Some(hit(2, true)) },
        );
        assert_eq!(
            state,
            PointerState::PressedFolder { button: 2, press_x: 60, press_y: 14 }
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
            PointerEvent::Press { x: 10, y: 14, hit: Some(hit(0, false)) },
        );
        assert_eq!(state, PointerState::PressedNonFolder { button: 0 });
        assert_eq!(cmds, vec![PointerCommand::Redraw]);
    }

    #[test]
    fn idle_press_off_buttons_is_noop() {
        let (state, cmds) = transition(
            PointerState::Idle,
            PointerEvent::Press { x: 0, y: 0, hit: None },
        );
        assert_eq!(state, PointerState::Idle);
        assert!(cmds.is_empty());
    }

    #[test]
    fn hovering_move_onto_same_button_is_noop() {
        let (state, cmds) = transition(
            PointerState::Hovering { button: 1 },
            PointerEvent::Move {
                x: 50, y: 14,
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
                x: 80, y: 14,
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
                x: 0, y: 0,
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
        let (state, cmds) =
            transition(PointerState::Hovering { button: 3 }, PointerEvent::Leave);
        assert_eq!(state, PointerState::Idle);
        assert_eq!(cmds, vec![PointerCommand::Redraw]);
    }

    #[test]
    fn hovering_press_on_folder_begins_folder_gesture() {
        let (state, cmds) = transition(
            PointerState::Hovering { button: 1 },
            PointerEvent::Press { x: 50, y: 14, hit: Some(hit(1, true)) },
        );
        assert_eq!(
            state,
            PointerState::PressedFolder { button: 1, press_x: 50, press_y: 14 }
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
        let (state, cmds) =
            transition(PointerState::Hovering { button: 2 }, PointerEvent::CaptureLost);
        assert_eq!(state, PointerState::Hovering { button: 2 });
        assert!(cmds.is_empty());
    }
}
