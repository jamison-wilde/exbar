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
pub fn transition(
    _state: PointerState,
    _event: PointerEvent,
) -> (PointerState, Vec<PointerCommand>) {
    unimplemented!("Tasks 2-4 implement transition")
}
