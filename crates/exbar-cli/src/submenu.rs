//! Pure chain-of-popups state machine for spring-open submenus.
//!
//! The adapter (`submenu_wnd.rs`) translates mouse / timer / drag events into
//! events, calls the transition function, and dispatches returned commands
//! against Win32. No Win32 types appear here.
//!
//! Chain model: at most [`MAX_CHAIN_DEPTH`] levels of nested popups. Level 0 is the toolbar
//! button itself (not a popup); levels 1–[`MAX_CHAIN_DEPTH`] are popups. Each level records
//! its folder path, `ancestor_mode` flag, and horizontal flow direction
//! (which is locked at the chain level once the first right-overflow happens).

use crate::subfolder_enum::SubfolderEntry;
use std::path::PathBuf;

pub const MAX_CHAIN_DEPTH: usize = 7;

/// Horizontal flow for levels 2+. Locked per chain once set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowDir {
    Right,
    Left,
}

/// Vertical orientation of a popup relative to its triggering point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertOrient {
    Upward,
    Downward,
}

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct WorkArea {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl WorkArea {
    pub fn width(self) -> i32 {
        self.right - self.left
    }
    pub fn height(self) -> i32 {
        self.bottom - self.top
    }
    pub fn vertical_midline(self) -> i32 {
        self.top + self.height() / 2
    }
}

/// Decides the vertical orientation for a LEVEL-1 submenu based on the
/// trigger button's center Y relative to the work-area midline + the
/// "direction-for-most-options" rule (spec §3.7).
///
/// Inputs:
/// - `btn_center_y` — triggering toolbar button's center Y (screen coords).
/// - `item_count` — number of items the submenu wants to show.
/// - `item_px` — per-item height + margins, DPI-scaled.
/// - `cursor_y` — cursor Y at open time (screen coords).
/// - `work` — monitor work area.
///
/// Returns the vertical orientation. The caller uses this + parent-reshow
/// policy (§3.6) to place the popup.
pub fn resolve_level1_orientation(
    btn_center_y: i32,
    item_count: i32,
    item_px: i32,
    cursor_y: i32,
    work: WorkArea,
) -> VertOrient {
    let needed = item_count * item_px;
    let space_up = cursor_y - work.top;
    let space_down = work.bottom - cursor_y;

    let fits_up = space_up >= needed;
    let fits_down = space_down >= needed;

    match (fits_up, fits_down) {
        (true, false) => VertOrient::Upward,
        (false, true) => VertOrient::Downward,
        (true, true) => {
            // Both fit → fall back to button-half rule (spec §3.6).
            if btn_center_y >= work.vertical_midline() {
                VertOrient::Upward
            } else {
                VertOrient::Downward
            }
        }
        (false, false) => {
            // Neither fits → direction-for-most-options.
            if space_up >= space_down {
                VertOrient::Upward
            } else {
                VertOrient::Downward
            }
        }
    }
}

/// Resolve the horizontal flow direction for a chain, given the proposed X
/// of the first nested (level-2) popup and its width. If it overflows right,
/// returns `Left`; otherwise `Right`. Once returned, the adapter locks this
/// direction for the remainder of the chain.
pub fn resolve_flow_direction(proposed_right_edge_x: i32, work: WorkArea) -> FlowDir {
    if proposed_right_edge_x > work.right {
        FlowDir::Left
    } else {
        FlowDir::Right
    }
}

/// One level in the open chain. Level 1 is root (opened from a toolbar
/// button); level 2+ are nested. Indexing starts at 1.
#[derive(Debug, Clone)]
pub struct ChainLevel {
    pub level: u8,                       // 1..=MAX_CHAIN_DEPTH
    pub path: PathBuf,                   // folder whose contents are shown
    pub ancestor_mode: bool,             // true iff "..",-only descent so far
    pub highlighted_item: Option<usize>, // index into the rendered item list
    /// True only at level 1 when opened from a `FolderKind::Recent` button.
    /// Levels 2+ are always false — they browse real subfolders from there on.
    pub is_recent: bool,
}

/// Whole open chain + per-chain locked flow direction.
#[derive(Debug, Default, Clone)]
pub struct SubmenuChain {
    pub levels: Vec<ChainLevel>,
    pub flow: Option<FlowDir>,
    pub dismiss_pending_ticks: u8, // countdown after cursor leaves all buffers
}

impl SubmenuChain {
    pub fn is_open(&self) -> bool {
        !self.levels.is_empty()
    }
    pub fn depth(&self) -> usize {
        self.levels.len()
    }
    pub fn deepest(&self) -> Option<&ChainLevel> {
        self.levels.last()
    }
}

#[derive(Debug, Clone)]
pub enum SubmenuEvent {
    /// User long-pressed / drag-hovered a toolbar folder button. Adapter
    /// provides the button's folder path.
    OpenRoot {
        path: PathBuf,
        button_center_y: i32,
        is_recent: bool,
    },
    /// Cursor moved onto a concrete subfolder item inside an open popup.
    /// Any deeper levels are discarded; a new deeper level is opened.
    HoverChildItem {
        level: u8,
        index: usize,
        child_path: PathBuf,
        is_dotdot: bool,
    },
    /// Cursor moved into a popup's translucent padding buffer OR a non-item
    /// spot inside a painted popup. Cancels any pending dismiss countdown;
    /// does not mutate highlights (the last highlighted item stays set by
    /// whichever level it belongs to). Chain-wide — no level parameter.
    HoverBufferAt,
    /// Cursor left all open popups (and their buffers). Adapter starts a
    /// short dismiss countdown.
    CursorExit,
    /// Safety timer tick (~30 ms) — re-checks cursor state; the adapter
    /// decrements `dismiss_pending_ticks` on each tick and dispatches
    /// `CloseAll` when it reaches 0.
    SafetyTick,
    /// Cursor re-entered any open popup (its paint or buffer). Cancel dismiss.
    CursorReenter,
    /// User committed by clicking / dropping on an item. Close the chain.
    Commit,
    /// User pressed Esc or clicked outside all popups.
    Dismiss,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SubmenuCommand {
    OpenLevel {
        level: u8,
        path: PathBuf,
        ancestor_mode: bool,
        /// Forwarded from `SubmenuEvent::OpenRoot`; false for all levels 2+.
        is_recent: bool,
    },
    CloseDeeperThan {
        level: u8,
    },
    CloseAll,
    SetHighlight {
        level: u8,
        index: Option<usize>,
    },
}

/// Pure transition. Mutates `chain`, returns a vec of commands.
pub fn transition(chain: &mut SubmenuChain, ev: SubmenuEvent) -> Vec<SubmenuCommand> {
    use SubmenuCommand::*;
    match ev {
        SubmenuEvent::OpenRoot {
            path, is_recent, ..
        } => {
            // Discard any prior chain, start fresh.
            chain.levels.clear();
            chain.flow = None;
            chain.dismiss_pending_ticks = 0;
            chain.levels.push(ChainLevel {
                level: 1,
                path: path.clone(),
                ancestor_mode: true,
                highlighted_item: None,
                is_recent,
            });
            vec![
                CloseAll,
                OpenLevel {
                    level: 1,
                    path,
                    ancestor_mode: true,
                    is_recent,
                },
            ]
        }
        SubmenuEvent::HoverChildItem {
            level,
            index,
            child_path,
            is_dotdot,
        } => {
            // Find the level this hover is at.
            let Some(idx) = chain.levels.iter().position(|l| l.level == level) else {
                return vec![];
            };
            let mut cmds: Vec<SubmenuCommand> = Vec::new();

            // If a deeper level is already open with the same child_path, this is a
            // no-op on the chain — just update highlight if it changed.
            if chain.levels.len() > idx + 1 && chain.levels[idx + 1].path == child_path {
                if chain.levels[idx].highlighted_item != Some(index) {
                    chain.levels[idx].highlighted_item = Some(index);
                    cmds.push(SetHighlight {
                        level,
                        index: Some(index),
                    });
                }
                return cmds;
            }

            // Different deeper path (or no deeper level): truncate and open fresh.
            if chain.levels.len() > idx + 1 {
                chain.levels.truncate(idx + 1);
                cmds.push(CloseDeeperThan { level });
            }
            if chain.levels[idx].highlighted_item != Some(index) {
                chain.levels[idx].highlighted_item = Some(index);
                cmds.push(SetHighlight {
                    level,
                    index: Some(index),
                });
            }

            if chain.depth() >= MAX_CHAIN_DEPTH {
                return cmds;
            }
            let parent_mode = chain.levels[idx].ancestor_mode;
            let parent_is_recent = chain.levels[idx].is_recent;
            let new_level = level + 1;
            let ancestor_mode = if parent_is_recent {
                // Recent's children are fresh browsing roots: each item is a real
                // filesystem folder, and ".." into its parent is meaningful.
                true
            } else {
                parent_mode && is_dotdot
            };
            chain.levels.push(ChainLevel {
                level: new_level,
                path: child_path.clone(),
                ancestor_mode,
                highlighted_item: None,
                is_recent: false, // levels 2+ always browse real folders
            });
            cmds.push(OpenLevel {
                level: new_level,
                path: child_path,
                ancestor_mode,
                is_recent: false,
            });
            cmds
        }
        SubmenuEvent::HoverBufferAt => {
            chain.dismiss_pending_ticks = 0;
            vec![]
        }
        SubmenuEvent::CursorExit => {
            chain.dismiss_pending_ticks = 5; // 5 * 30ms ≈ 150ms
            vec![]
        }
        SubmenuEvent::CursorReenter => {
            chain.dismiss_pending_ticks = 0;
            vec![]
        }
        SubmenuEvent::SafetyTick => {
            if chain.dismiss_pending_ticks == 0 {
                return vec![];
            }
            chain.dismiss_pending_ticks -= 1;
            if chain.dismiss_pending_ticks == 0 {
                chain.levels.clear();
                chain.flow = None;
                return vec![CloseAll];
            }
            vec![]
        }
        SubmenuEvent::Commit | SubmenuEvent::Dismiss => {
            chain.levels.clear();
            chain.flow = None;
            chain.dismiss_pending_ticks = 0;
            vec![CloseAll]
        }
    }
}

/// One entry in a popup's rendered list. Either a real subfolder, a ".."
/// ancestor hop, the parent-reshow item (level 1 only), or the "…(more)"
/// ellipsis sentinel.
#[derive(Debug, Clone, PartialEq)]
pub enum DisplayItem {
    Dotdot {
        parent_path: PathBuf,
        parent_name: String,
    },
    ParentReshow {
        path: PathBuf,
        name: String,
    },
    Subfolder {
        entry: SubfolderEntry,
    },
    Ellipsis,
    Empty {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReshowPosition {
    First,
    Last,
    None, // levels 2+ don't get a reshow
}

/// Build the display list for a popup.
///
/// - `level`: 1..=5
/// - `folder_path`: absolute path of the folder whose contents we're showing
/// - `ancestor_mode`: should ".." be included (if not at drive root)
/// - `entries`: output of `SubfolderSource::list`
/// - `reshow`: where to place the parent-reshow item (First/Last/None)
/// - `parent_name`: display name for the ".." row (e.g. "Users")
pub fn build_display_list(
    level: u8,
    folder_path: &std::path::Path,
    folder_display_name: &str,
    ancestor_mode: bool,
    entries: &[SubfolderEntry],
    reshow: ReshowPosition,
) -> Vec<DisplayItem> {
    debug_assert!(
        level == 1 || reshow == ReshowPosition::None,
        "ReshowPosition::{{First|Last}} is only valid at level 1; level={level} reshow={reshow:?}"
    );

    let path_s = folder_path.to_string_lossy().to_string();
    let mut out: Vec<DisplayItem> = Vec::new();

    // Empty-state (no subfolders AND no ".." applicable AND no reshow).
    let want_dotdot = ancestor_mode && !crate::path_norm::is_drive_root(&path_s);
    if entries.is_empty() && !want_dotdot && reshow == ReshowPosition::None {
        out.push(DisplayItem::Empty {
            message: "(empty)".to_string(),
        });
        return out;
    }

    // Reshown parent at "First" goes at the very top (popup opened downward).
    if reshow == ReshowPosition::First && level == 1 {
        out.push(DisplayItem::ParentReshow {
            path: folder_path.to_path_buf(),
            name: folder_display_name.to_string(),
        });
    }

    // Two-guard form kept intentionally: want_dotdot is a named predicate
    // (ancestor_mode && !drive_root); a collapse loses that structure.
    #[allow(clippy::collapsible_if)]
    if want_dotdot {
        if let Some(parent) = crate::path_norm::parent_dir(&path_s) {
            let parent_name = std::path::Path::new(&parent)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| parent.clone());
            out.push(DisplayItem::Dotdot {
                parent_path: std::path::PathBuf::from(&parent),
                parent_name,
            });
        }
    }

    for e in entries {
        if e.name == crate::subfolder_enum::ELLIPSIS_SENTINEL {
            out.push(DisplayItem::Ellipsis);
        } else {
            out.push(DisplayItem::Subfolder { entry: e.clone() });
        }
    }

    if reshow == ReshowPosition::Last && level == 1 {
        out.push(DisplayItem::ParentReshow {
            path: folder_path.to_path_buf(),
            name: folder_display_name.to_string(),
        });
    }
    out
}

/// Build the display list for the Recent button's level-1 submenu.
///
/// Differences from [`build_display_list`]:
/// - No `".."` (Recent is a pseudo-entry with no real parent path).
/// - No parent-reshow row.
/// - Items come from the `RecentList` directly (pre-filtered by
///   `include_pinned` at the caller).
/// - Empty state: single disabled `"(no recent folders yet)"` item.
pub fn build_recent_display_list(entries: &[crate::recent_list::RecentEntry]) -> Vec<DisplayItem> {
    if entries.is_empty() {
        return vec![DisplayItem::Empty {
            message: "(no recent folders yet)".to_string(),
        }];
    }
    entries
        .iter()
        .map(|e| {
            let name = e
                .path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| e.path.to_string_lossy().to_string());
            DisplayItem::Subfolder {
                entry: crate::subfolder_enum::SubfolderEntry {
                    name,
                    path: e.path.clone(),
                    has_children: true, // users can spring into subfolder browsing
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests_display {
    use super::*;
    use crate::subfolder_enum::SubfolderEntry;

    fn sf(name: &str) -> SubfolderEntry {
        SubfolderEntry {
            name: name.to_string(),
            path: PathBuf::from(name),
            has_children: false,
        }
    }

    #[test]
    fn level1_ancestor_mode_includes_dotdot_and_reshow_first() {
        let items = build_display_list(
            1,
            std::path::Path::new("C:\\Users\\Alice"),
            "Alice",
            true,
            &[sf("Docs"), sf("Pictures")],
            ReshowPosition::First,
        );
        assert!(matches!(&items[0], DisplayItem::ParentReshow { .. }));
        assert!(matches!(&items[1], DisplayItem::Dotdot { .. }));
        assert!(matches!(&items[2], DisplayItem::Subfolder { .. }));
    }

    #[test]
    fn level1_ancestor_mode_includes_dotdot_and_reshow_last() {
        let items = build_display_list(
            1,
            std::path::Path::new("C:\\Users\\Alice"),
            "Alice",
            true,
            &[sf("Docs")],
            ReshowPosition::Last,
        );
        assert!(matches!(&items[0], DisplayItem::Dotdot { .. }));
        assert!(matches!(&items[1], DisplayItem::Subfolder { .. }));
        assert!(matches!(&items[2], DisplayItem::ParentReshow { .. }));
    }

    #[test]
    fn drive_root_omits_dotdot_even_in_ancestor_mode() {
        let items = build_display_list(
            1,
            std::path::Path::new("C:\\"),
            "C:",
            true,
            &[sf("Windows")],
            ReshowPosition::First,
        );
        assert!(
            !items
                .iter()
                .any(|i| matches!(i, DisplayItem::Dotdot { .. }))
        );
    }

    #[test]
    fn non_ancestor_level_omits_dotdot() {
        let items = build_display_list(
            2,
            std::path::Path::new("C:\\Users\\Alice"),
            "Alice",
            false,
            &[sf("Docs")],
            ReshowPosition::None,
        );
        assert!(
            !items
                .iter()
                .any(|i| matches!(i, DisplayItem::Dotdot { .. }))
        );
        assert!(
            !items
                .iter()
                .any(|i| matches!(i, DisplayItem::ParentReshow { .. }))
        );
    }

    #[test]
    fn empty_list_gets_empty_item_at_level2() {
        let items = build_display_list(
            2,
            std::path::Path::new("C:\\Users\\Alice\\Docs"),
            "Docs",
            false,
            &[],
            ReshowPosition::None,
        );
        assert_eq!(items.len(), 1);
        assert!(matches!(&items[0], DisplayItem::Empty { .. }));
    }

    #[test]
    fn level1_empty_entries_with_reshow_first_produces_only_reshow() {
        // Non-ancestor level-1 with nothing to list but a reshow at First:
        // empty-state guard must suppress the "(empty)" sentinel, and the
        // reshow item itself must be the only rendered entry.
        let items = build_display_list(
            1,
            std::path::Path::new("C:\\Users\\Alice\\Docs"),
            "Docs",
            false,
            &[],
            ReshowPosition::First,
        );
        assert_eq!(items.len(), 1);
        assert!(matches!(&items[0], DisplayItem::ParentReshow { .. }));
    }
}

#[cfg(test)]
mod tests_chain {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn open_root_starts_chain_with_ancestor_mode() {
        let mut chain = SubmenuChain::default();
        let cmds = transition(
            &mut chain,
            SubmenuEvent::OpenRoot {
                path: p("C:\\A"),
                button_center_y: 500,
                is_recent: false,
            },
        );
        assert!(chain.is_open());
        assert_eq!(chain.depth(), 1);
        assert!(chain.levels[0].ancestor_mode);
        assert!(matches!(&cmds[0], SubmenuCommand::CloseAll));
        assert!(matches!(
            &cmds[1],
            SubmenuCommand::OpenLevel {
                level: 1,
                ancestor_mode: true,
                ..
            }
        ));
    }

    #[test]
    fn hover_child_opens_level2_descent_clears_ancestor_mode() {
        let mut chain = SubmenuChain::default();
        transition(
            &mut chain,
            SubmenuEvent::OpenRoot {
                path: p("C:\\A"),
                button_center_y: 0,
                is_recent: false,
            },
        );
        let cmds = transition(
            &mut chain,
            SubmenuEvent::HoverChildItem {
                level: 1,
                index: 2,
                child_path: p("C:\\A\\B"),
                is_dotdot: false,
            },
        );
        assert_eq!(chain.depth(), 2);
        assert!(!chain.levels[1].ancestor_mode);
        assert!(cmds.iter().any(|c| matches!(
            c,
            SubmenuCommand::OpenLevel {
                level: 2,
                ancestor_mode: false,
                ..
            }
        )));
    }

    #[test]
    fn hover_dotdot_from_ancestor_keeps_ancestor_mode() {
        let mut chain = SubmenuChain::default();
        transition(
            &mut chain,
            SubmenuEvent::OpenRoot {
                path: p("C:\\A\\B"),
                button_center_y: 0,
                is_recent: false,
            },
        );
        transition(
            &mut chain,
            SubmenuEvent::HoverChildItem {
                level: 1,
                index: 0,
                child_path: p("C:\\A"),
                is_dotdot: true,
            },
        );
        assert!(chain.levels[1].ancestor_mode);
    }

    #[test]
    fn hover_dotdot_from_descendant_does_not_restore_ancestor_mode() {
        let mut chain = SubmenuChain::default();
        transition(
            &mut chain,
            SubmenuEvent::OpenRoot {
                path: p("C:\\A"),
                button_center_y: 0,
                is_recent: false,
            },
        );
        transition(
            &mut chain,
            SubmenuEvent::HoverChildItem {
                level: 1,
                index: 2,
                child_path: p("C:\\A\\B"),
                is_dotdot: false,
            },
        );
        // Now at level 2, descent mode. Even if adapter claims is_dotdot=true
        // (shouldn't happen — no ".." item is shown in descent mode — but be
        // defensive), ancestor_mode must remain false.
        transition(
            &mut chain,
            SubmenuEvent::HoverChildItem {
                level: 2,
                index: 0,
                child_path: p("C:\\A"),
                is_dotdot: true,
            },
        );
        assert!(!chain.levels[2].ancestor_mode);
    }

    #[test]
    fn hover_at_shallower_level_truncates_deeper() {
        let mut chain = SubmenuChain::default();
        transition(
            &mut chain,
            SubmenuEvent::OpenRoot {
                path: p("C:\\A"),
                button_center_y: 0,
                is_recent: false,
            },
        );
        transition(
            &mut chain,
            SubmenuEvent::HoverChildItem {
                level: 1,
                index: 0,
                child_path: p("C:\\A\\B"),
                is_dotdot: false,
            },
        );
        transition(
            &mut chain,
            SubmenuEvent::HoverChildItem {
                level: 2,
                index: 0,
                child_path: p("C:\\A\\B\\C"),
                is_dotdot: false,
            },
        );
        assert_eq!(chain.depth(), 3);
        let cmds = transition(
            &mut chain,
            SubmenuEvent::HoverChildItem {
                level: 1,
                index: 2,
                child_path: p("C:\\A\\X"),
                is_dotdot: false,
            },
        );
        assert_eq!(chain.depth(), 2);
        assert!(
            cmds.iter()
                .any(|c| matches!(c, SubmenuCommand::CloseDeeperThan { level: 1 }))
        );
    }

    #[test]
    fn max_depth_caps_at_const() {
        let mut chain = SubmenuChain::default();
        transition(
            &mut chain,
            SubmenuEvent::OpenRoot {
                path: p("C:\\"),
                button_center_y: 0,
                is_recent: false,
            },
        );
        for l in 1..=(MAX_CHAIN_DEPTH as u8 + 5) {
            transition(
                &mut chain,
                SubmenuEvent::HoverChildItem {
                    level: l,
                    index: 0,
                    child_path: p(&format!("C:\\{l}")),
                    is_dotdot: false,
                },
            );
        }
        assert_eq!(chain.depth(), MAX_CHAIN_DEPTH);
    }

    #[test]
    fn cursor_exit_arms_dismiss_and_tick_closes() {
        let mut chain = SubmenuChain::default();
        transition(
            &mut chain,
            SubmenuEvent::OpenRoot {
                path: p("C:\\A"),
                button_center_y: 0,
                is_recent: false,
            },
        );
        transition(&mut chain, SubmenuEvent::CursorExit);
        for _ in 0..4 {
            let cmds = transition(&mut chain, SubmenuEvent::SafetyTick);
            assert!(cmds.is_empty());
        }
        let cmds = transition(&mut chain, SubmenuEvent::SafetyTick);
        assert_eq!(cmds, vec![SubmenuCommand::CloseAll]);
        assert!(!chain.is_open());
    }

    #[test]
    fn cursor_reenter_cancels_dismiss() {
        let mut chain = SubmenuChain::default();
        transition(
            &mut chain,
            SubmenuEvent::OpenRoot {
                path: p("C:\\A"),
                button_center_y: 0,
                is_recent: false,
            },
        );
        transition(&mut chain, SubmenuEvent::CursorExit);
        transition(&mut chain, SubmenuEvent::CursorReenter);
        for _ in 0..10 {
            let cmds = transition(&mut chain, SubmenuEvent::SafetyTick);
            assert!(cmds.is_empty());
        }
        assert!(chain.is_open());
    }

    #[test]
    fn commit_closes_all() {
        let mut chain = SubmenuChain::default();
        transition(
            &mut chain,
            SubmenuEvent::OpenRoot {
                path: p("C:\\A"),
                button_center_y: 0,
                is_recent: false,
            },
        );
        let cmds = transition(&mut chain, SubmenuEvent::Commit);
        assert_eq!(cmds, vec![SubmenuCommand::CloseAll]);
        assert!(!chain.is_open());
    }

    #[test]
    fn dismiss_closes_all_same_as_commit() {
        let mut chain = SubmenuChain::default();
        transition(
            &mut chain,
            SubmenuEvent::OpenRoot {
                path: p("C:\\A"),
                button_center_y: 0,
                is_recent: false,
            },
        );
        transition(
            &mut chain,
            SubmenuEvent::HoverChildItem {
                level: 1,
                index: 0,
                child_path: p("C:\\A\\B"),
                is_dotdot: false,
            },
        );
        let cmds = transition(&mut chain, SubmenuEvent::Dismiss);
        assert_eq!(cmds, vec![SubmenuCommand::CloseAll]);
        assert!(!chain.is_open());
        assert!(chain.flow.is_none());
    }

    #[test]
    fn hover_at_unknown_level_is_noop() {
        let mut chain = SubmenuChain::default();
        transition(
            &mut chain,
            SubmenuEvent::OpenRoot {
                path: p("C:\\A"),
                button_center_y: 0,
                is_recent: false,
            },
        );
        // Level 7 does not exist — expect safe no-op returning empty vec.
        let cmds = transition(
            &mut chain,
            SubmenuEvent::HoverChildItem {
                level: 7,
                index: 0,
                child_path: p("C:\\A\\Z"),
                is_dotdot: false,
            },
        );
        assert!(cmds.is_empty());
        assert_eq!(chain.depth(), 1);
    }

    #[test]
    fn buffer_hover_cancels_dismiss_countdown() {
        let mut chain = SubmenuChain::default();
        transition(
            &mut chain,
            SubmenuEvent::OpenRoot {
                path: p("C:\\A"),
                button_center_y: 0,
                is_recent: false,
            },
        );
        transition(&mut chain, SubmenuEvent::CursorExit);
        assert_eq!(chain.dismiss_pending_ticks, 5);
        transition(&mut chain, SubmenuEvent::HoverBufferAt);
        assert_eq!(chain.dismiss_pending_ticks, 0);
    }

    #[test]
    fn hover_same_child_twice_does_not_reopen_deeper() {
        let mut chain = SubmenuChain::default();
        transition(
            &mut chain,
            SubmenuEvent::OpenRoot {
                path: p("C:\\A"),
                button_center_y: 0,
                is_recent: false,
            },
        );
        // First hover opens level 2.
        let cmds1 = transition(
            &mut chain,
            SubmenuEvent::HoverChildItem {
                level: 1,
                index: 0,
                child_path: p("C:\\A\\B"),
                is_dotdot: false,
            },
        );
        assert!(
            cmds1
                .iter()
                .any(|c| matches!(c, SubmenuCommand::OpenLevel { level: 2, .. }))
        );
        // Second hover at same level/index/path — already open with same path, so no-op.
        let cmds2 = transition(
            &mut chain,
            SubmenuEvent::HoverChildItem {
                level: 1,
                index: 0,
                child_path: p("C:\\A\\B"),
                is_dotdot: false,
            },
        );
        assert!(
            cmds2
                .iter()
                .all(|c| !matches!(c, SubmenuCommand::OpenLevel { .. }))
        );
        assert!(
            cmds2
                .iter()
                .all(|c| !matches!(c, SubmenuCommand::CloseDeeperThan { .. }))
        );
    }

    #[test]
    fn hover_from_recent_root_enables_ancestor_mode_on_child() {
        let mut chain = SubmenuChain::default();
        transition(
            &mut chain,
            SubmenuEvent::OpenRoot {
                path: std::path::PathBuf::new(),
                button_center_y: 0,
                is_recent: true,
            },
        );
        transition(
            &mut chain,
            SubmenuEvent::HoverChildItem {
                level: 1,
                index: 0,
                child_path: std::path::PathBuf::from("C:\\Users\\wix"),
                is_dotdot: false,
            },
        );
        assert_eq!(chain.depth(), 2);
        assert!(
            chain.levels[1].ancestor_mode,
            "child of recent root should be in ancestor mode"
        );
    }

    #[test]
    fn hover_from_non_recent_root_clears_ancestor_mode_on_concrete_descent() {
        // Regression: regular folder path still works.
        let mut chain = SubmenuChain::default();
        transition(
            &mut chain,
            SubmenuEvent::OpenRoot {
                path: std::path::PathBuf::from("C:\\A"),
                button_center_y: 0,
                is_recent: false,
            },
        );
        transition(
            &mut chain,
            SubmenuEvent::HoverChildItem {
                level: 1,
                index: 0,
                child_path: std::path::PathBuf::from("C:\\A\\B"),
                is_dotdot: false,
            },
        );
        assert!(!chain.levels[1].ancestor_mode);
    }

    #[test]
    fn hover_different_child_path_closes_and_reopens() {
        let mut chain = SubmenuChain::default();
        transition(
            &mut chain,
            SubmenuEvent::OpenRoot {
                path: p("C:\\A"),
                button_center_y: 0,
                is_recent: false,
            },
        );
        transition(
            &mut chain,
            SubmenuEvent::HoverChildItem {
                level: 1,
                index: 0,
                child_path: p("C:\\A\\B"),
                is_dotdot: false,
            },
        );
        // Hover a different sibling — should close level 2 and reopen with new path.
        let cmds = transition(
            &mut chain,
            SubmenuEvent::HoverChildItem {
                level: 1,
                index: 2,
                child_path: p("C:\\A\\C"),
                is_dotdot: false,
            },
        );
        assert!(
            cmds.iter()
                .any(|c| matches!(c, SubmenuCommand::CloseDeeperThan { level: 1 }))
        );
        let opened = cmds.iter().find_map(|c| match c {
            SubmenuCommand::OpenLevel { level: 2, path, .. } => Some(path),
            _ => None,
        });
        assert_eq!(
            opened.map(|p| p.to_string_lossy().to_string()),
            Some("C:\\A\\C".to_string())
        );
    }
}

#[cfg(test)]
mod tests_direction {
    use super::*;

    fn wa() -> WorkArea {
        WorkArea {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    }

    #[test]
    fn level1_up_when_below_midline_both_fit() {
        let o = resolve_level1_orientation(800, 5, 30, 700, wa());
        assert_eq!(o, VertOrient::Upward);
    }

    #[test]
    fn level1_down_when_above_midline_both_fit() {
        let o = resolve_level1_orientation(200, 5, 30, 200, wa());
        assert_eq!(o, VertOrient::Downward);
    }

    #[test]
    fn level1_prefers_direction_that_fits() {
        // Only 100 px above cursor, 980 below; 5*30=150 needed.
        let o = resolve_level1_orientation(200, 5, 30, 100, wa());
        assert_eq!(o, VertOrient::Downward);
    }

    #[test]
    fn level1_most_space_when_neither_fits() {
        // 40 items * 30 px = 1200; cursor at y=300 → up=300, down=780; pick down.
        let o = resolve_level1_orientation(200, 40, 30, 300, wa());
        assert_eq!(o, VertOrient::Downward);
    }

    #[test]
    fn flow_right_when_no_overflow() {
        assert_eq!(resolve_flow_direction(1800, wa()), FlowDir::Right);
    }

    #[test]
    fn flow_left_on_right_overflow() {
        assert_eq!(resolve_flow_direction(1921, wa()), FlowDir::Left);
    }

    #[test]
    fn flow_direction_once_left_always_left() {
        let work = WorkArea {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        // First overflow flips to Left.
        let first = resolve_flow_direction(2000, work);
        assert_eq!(first, FlowDir::Left);
        // Subsequent non-overflow x would normally say Right, but the ratchet is
        // enforced at the caller (open_popup_level match arm) — resolve_flow_direction
        // itself is a pure function and always answers based on the input.
        let second = resolve_flow_direction(1800, work);
        assert_eq!(second, FlowDir::Right);
        // This test documents that the one-way lock is the CALLER's responsibility,
        // not this function's. The caller preserves FlowDir::Left once set.
    }
}

#[cfg(test)]
mod tests_recent_display {
    use super::*;
    use crate::recent_list::RecentEntry;

    #[test]
    fn empty_list_produces_placeholder() {
        let out = build_recent_display_list(&[]);
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], DisplayItem::Empty { .. }));
    }

    #[test]
    fn each_entry_becomes_subfolder_with_has_children_true() {
        let entries = vec![
            RecentEntry {
                path: PathBuf::from("C:\\A\\B"),
                last_accessed_unix_ms: 100,
            },
            RecentEntry {
                path: PathBuf::from("C:\\X"),
                last_accessed_unix_ms: 200,
            },
        ];
        let out = build_recent_display_list(&entries);
        assert_eq!(out.len(), 2);
        match &out[0] {
            DisplayItem::Subfolder { entry } => {
                assert_eq!(entry.name, "B");
                assert!(entry.has_children);
            }
            _ => panic!("expected Subfolder"),
        }
    }

    #[test]
    fn no_dotdot_or_parent_reshow_items() {
        let entries = vec![RecentEntry {
            path: PathBuf::from("C:\\Users\\Alice\\Projects"),
            last_accessed_unix_ms: 100,
        }];
        let out = build_recent_display_list(&entries);
        assert!(!out.iter().any(|i| matches!(
            i,
            DisplayItem::Dotdot { .. } | DisplayItem::ParentReshow { .. }
        )));
    }

    #[test]
    fn drive_root_entry_uses_full_path_as_name() {
        // When file_name() returns None (e.g. "C:\\"), fall back to the full path string.
        let entries = vec![RecentEntry {
            path: PathBuf::from("C:\\"),
            last_accessed_unix_ms: 100,
        }];
        let out = build_recent_display_list(&entries);
        match &out[0] {
            DisplayItem::Subfolder { entry } => {
                assert!(!entry.name.is_empty());
            }
            _ => panic!("expected Subfolder"),
        }
    }
}
