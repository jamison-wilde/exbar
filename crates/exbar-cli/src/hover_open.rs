//! Hover-to-open controller for toolbar folder buttons. Pure; no Win32.
//!
//! - A folder opens after the pointer *rests* on it for `rest_ms`; movement
//!   beyond `jitter_px` restarts the wait (Win32 `WM_MOUSEHOVER` semantics).
//! - The Recent button opens on contact (its list is in memory).
//! - Menu-bar switching: while a chain is open, hovering another folder
//!   button opens that button's chain at once.
//! - A chain that closed under the cursor (Esc, click, dismiss) stays closed
//!   until the cursor leaves that button (`Suppressed`), so hover never
//!   re-pops something the user just dismissed.
//!
//! The adapter (`ToolbarState::execute_hover_event`) runs the commands.

use crate::toolbar::ToolbarState;

/// Rest-detection tolerance in logical px (Windows' default `SM_CXMOUSEHOVER`).
pub const HOVER_JITTER_LOGICAL_PX: i32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HoverState {
    #[default]
    Idle,
    Resting {
        button: usize,
        anchor: (i32, i32),
    },
    Open {
        button: usize,
    },
    Suppressed {
        button: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoverEvent {
    MoveOnButton {
        button: usize,
        is_recent: bool,
        x: i32,
        y: i32,
        chain_open: bool,
    },
    MoveOffButtons,
    Leave,
    RestTimerFired {
        chain_open: bool,
    },
    RightButtonDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoverCommand {
    ArmRest(u32),
    KillRest,
    Open(usize),
}

#[derive(Debug, Clone, Copy)]
pub struct HoverCtx {
    pub rest_ms: u32,
    pub jitter_px: i32,
}

pub fn transition(state: &mut HoverState, ev: HoverEvent, ctx: &HoverCtx) -> Vec<HoverCommand> {
    use HoverCommand::*;
    match ev {
        HoverEvent::MoveOnButton {
            button,
            is_recent,
            x,
            y,
            chain_open,
        } => match *state {
            HoverState::Open { button: open } if chain_open => {
                if open == button {
                    vec![]
                } else {
                    *state = HoverState::Open { button };
                    vec![Open(button)]
                }
            }
            // Our chain closed under the cursor: stay closed on this button.
            HoverState::Open { button: open } if open == button => {
                *state = HoverState::Suppressed { button };
                vec![]
            }
            HoverState::Suppressed { button: b } if b == button => vec![],
            // A chain someone else opened (long-press, drag-hover): adopt it.
            _ if chain_open => {
                *state = HoverState::Open { button };
                vec![KillRest]
            }
            HoverState::Resting { button: b, anchor }
                if b == button
                    && (x - anchor.0).abs() <= ctx.jitter_px
                    && (y - anchor.1).abs() <= ctx.jitter_px =>
            {
                vec![]
            }
            _ => arrive(state, button, is_recent, (x, y), ctx),
        },
        HoverEvent::MoveOffButtons | HoverEvent::Leave | HoverEvent::RightButtonDown => {
            match *state {
                HoverState::Resting { .. } => {
                    *state = HoverState::Idle;
                    vec![KillRest]
                }
                HoverState::Suppressed { .. } if !matches!(ev, HoverEvent::RightButtonDown) => {
                    *state = HoverState::Idle;
                    vec![]
                }
                _ => vec![],
            }
        }
        HoverEvent::RestTimerFired { chain_open } => match *state {
            HoverState::Resting { button, .. } => {
                *state = HoverState::Open { button };
                if chain_open {
                    vec![]
                } else {
                    vec![Open(button)]
                }
            }
            _ => vec![],
        },
    }
}

/// Fresh arrival on `button`: Recent (or a zero delay) opens now, anything
/// else waits for the pointer to rest.
fn arrive(
    state: &mut HoverState,
    button: usize,
    is_recent: bool,
    at: (i32, i32),
    ctx: &HoverCtx,
) -> Vec<HoverCommand> {
    if is_recent || ctx.rest_ms == 0 {
        *state = HoverState::Open { button };
        vec![HoverCommand::KillRest, HoverCommand::Open(button)]
    } else {
        *state = HoverState::Resting { button, anchor: at };
        vec![HoverCommand::ArmRest(ctx.rest_ms)]
    }
}

// ── ToolbarState adapter ─────────────────────────────────────────────────────

impl ToolbarState {
    /// Whether toolbar button `button` (0 = '+') is the Recent pseudo-button.
    pub(crate) fn button_is_recent(&self, button: usize) -> bool {
        button >= 1
            && self
                .config
                .as_ref()
                .and_then(|c| c.folders.get(button - 1))
                .is_some_and(|f| matches!(f.kind, crate::config::FolderKind::Recent))
    }

    /// Feed a hover event through the controller and run its commands.
    pub(crate) fn execute_hover_event(
        &mut self,
        toolbar: windows::Win32::Foundation::HWND,
        ev: HoverEvent,
    ) {
        use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};
        let ctx = HoverCtx {
            rest_ms: self.submenu_cfg.long_hover_open_ms,
            jitter_px: crate::theme::scale(HOVER_JITTER_LOGICAL_PX, self.dpi),
        };
        for cmd in transition(&mut self.hover, ev, &ctx) {
            match cmd {
                HoverCommand::ArmRest(ms) => unsafe {
                    let _ = SetTimer(Some(toolbar), crate::toolbar::TIMER_HOVER_OPEN, ms, None);
                },
                HoverCommand::KillRest => unsafe {
                    let _ = KillTimer(Some(toolbar), crate::toolbar::TIMER_HOVER_OPEN);
                },
                HoverCommand::Open(button) => self.open_root_for_button(toolbar, button),
            }
        }
    }

    /// Record the trigger context and open `button`'s root submenu.
    fn open_root_for_button(&mut self, toolbar: windows::Win32::Foundation::HWND, button: usize) {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
        // Translate button index → folder index (button 0 is '+').
        let folder_button = button.saturating_sub(1);
        let Some(folder) = self
            .config
            .as_ref()
            .and_then(|c| c.folders.get(folder_button))
        else {
            return;
        };
        let is_recent = matches!(folder.kind, crate::config::FolderKind::Recent);
        let raw_path = folder.path.clone();

        // Record trigger context (same as FireLongPress arm in execute_pointer_command).
        let btn_rect = self.button_screen_rect(toolbar, folder_button);
        self.last_button_screen_rect = btn_rect;
        self.last_button_center_y_on_open = (btn_rect.top + btn_rect.bottom) / 2;
        let mut cursor = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cursor);
        }
        self.last_cursor_x_on_open = cursor.x;
        self.last_cursor_y_on_open = cursor.y;

        log::info!(
            "hover_open: fire folder_button={} at cursor=({},{})",
            folder_button,
            cursor.x,
            cursor.y
        );

        self.execute_submenu_event(
            toolbar,
            crate::submenu::SubmenuEvent::OpenRoot {
                path: std::path::PathBuf::from(raw_path),
                button_center_y: self.last_button_center_y_on_open,
                is_recent,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::HoverCommand::*;
    use super::*;

    const CTX: HoverCtx = HoverCtx {
        rest_ms: 400,
        jitter_px: 4,
    };

    fn on(button: usize, x: i32, chain_open: bool) -> HoverEvent {
        HoverEvent::MoveOnButton {
            button,
            is_recent: false,
            x,
            y: 10,
            chain_open,
        }
    }

    fn recent(button: usize, chain_open: bool) -> HoverEvent {
        HoverEvent::MoveOnButton {
            button,
            is_recent: true,
            x: 0,
            y: 10,
            chain_open,
        }
    }

    #[test]
    fn arrival_arms_rest_timer() {
        let mut s = HoverState::Idle;
        assert_eq!(
            transition(&mut s, on(2, 100, false), &CTX),
            vec![ArmRest(400)]
        );
        assert_eq!(
            s,
            HoverState::Resting {
                button: 2,
                anchor: (100, 10)
            }
        );
    }

    #[test]
    fn jitter_within_tolerance_does_not_rearm() {
        let mut s = HoverState::Idle;
        transition(&mut s, on(2, 100, false), &CTX);
        assert_eq!(transition(&mut s, on(2, 104, false), &CTX), vec![]);
    }

    #[test]
    fn movement_beyond_tolerance_rearms_and_reanchors() {
        let mut s = HoverState::Idle;
        transition(&mut s, on(2, 100, false), &CTX);
        assert_eq!(
            transition(&mut s, on(2, 105, false), &CTX),
            vec![ArmRest(400)]
        );
        assert_eq!(
            s,
            HoverState::Resting {
                button: 2,
                anchor: (105, 10)
            }
        );
    }

    #[test]
    fn rest_timer_opens_resting_button() {
        let mut s = HoverState::Idle;
        transition(&mut s, on(2, 100, false), &CTX);
        assert_eq!(
            transition(
                &mut s,
                HoverEvent::RestTimerFired { chain_open: false },
                &CTX
            ),
            vec![Open(2)]
        );
        assert_eq!(s, HoverState::Open { button: 2 });
    }

    #[test]
    fn stale_rest_timer_is_ignored() {
        let mut s = HoverState::Idle;
        assert_eq!(
            transition(
                &mut s,
                HoverEvent::RestTimerFired { chain_open: false },
                &CTX
            ),
            vec![]
        );
    }

    #[test]
    fn recent_opens_on_contact() {
        let mut s = HoverState::Idle;
        assert_eq!(
            transition(&mut s, recent(1, false), &CTX),
            vec![KillRest, Open(1)]
        );
        assert_eq!(s, HoverState::Open { button: 1 });
    }

    #[test]
    fn zero_rest_opens_every_button_on_contact() {
        let ctx = HoverCtx {
            rest_ms: 0,
            jitter_px: 4,
        };
        let mut s = HoverState::Idle;
        assert_eq!(
            transition(&mut s, on(3, 0, false), &ctx),
            vec![KillRest, Open(3)]
        );
    }

    #[test]
    fn open_chain_switches_instantly_to_another_button() {
        let mut s = HoverState::Open { button: 2 };
        assert_eq!(transition(&mut s, on(3, 0, true), &CTX), vec![Open(3)]);
        assert_eq!(s, HoverState::Open { button: 3 });
    }

    #[test]
    fn open_chain_same_button_is_noop() {
        let mut s = HoverState::Open { button: 2 };
        assert_eq!(transition(&mut s, on(2, 50, true), &CTX), vec![]);
    }

    /// The reported bug: after A's chain closed, arriving on B never armed,
    /// so every other folder failed to open.
    #[test]
    fn every_other_folder_regression_next_button_arms_after_chain_closed() {
        let mut s = HoverState::Idle;
        transition(&mut s, on(2, 0, false), &CTX);
        transition(
            &mut s,
            HoverEvent::RestTimerFired { chain_open: false },
            &CTX,
        ); // A open
        // A's chain dismissed while the cursor travelled to B.
        assert_eq!(
            transition(&mut s, on(3, 40, false), &CTX),
            vec![ArmRest(400)]
        );
        assert_eq!(
            transition(
                &mut s,
                HoverEvent::RestTimerFired { chain_open: false },
                &CTX
            ),
            vec![Open(3)]
        );
        // ...and the one after that (C) as well.
        assert_eq!(
            transition(&mut s, on(4, 80, false), &CTX),
            vec![ArmRest(400)]
        );
    }

    #[test]
    fn dismissed_chain_does_not_reopen_on_same_button() {
        let mut s = HoverState::Open { button: 2 };
        // Esc / click closed the chain; cursor still on 2.
        assert_eq!(transition(&mut s, on(2, 60, false), &CTX), vec![]);
        assert_eq!(s, HoverState::Suppressed { button: 2 });
        assert_eq!(transition(&mut s, on(2, 90, false), &CTX), vec![]);
    }

    #[test]
    fn suppression_ends_when_cursor_leaves_the_button() {
        let mut s = HoverState::Suppressed { button: 2 };
        transition(&mut s, HoverEvent::MoveOffButtons, &CTX);
        assert_eq!(
            transition(&mut s, on(2, 0, false), &CTX),
            vec![ArmRest(400)]
        );
    }

    #[test]
    fn suppression_ends_on_another_button() {
        let mut s = HoverState::Suppressed { button: 2 };
        assert_eq!(
            transition(&mut s, on(3, 0, false), &CTX),
            vec![ArmRest(400)]
        );
    }

    #[test]
    fn suppressed_open_does_not_retry_loop() {
        // Open was suppressed downstream (unreachable share): chain never opened.
        let mut s = HoverState::Idle;
        transition(&mut s, on(2, 0, false), &CTX);
        transition(
            &mut s,
            HoverEvent::RestTimerFired { chain_open: false },
            &CTX,
        );
        assert_eq!(transition(&mut s, on(2, 30, false), &CTX), vec![]);
        assert_eq!(s, HoverState::Suppressed { button: 2 });
    }

    #[test]
    fn externally_opened_chain_is_adopted_not_reopened() {
        // Long-press opened a chain while we were resting.
        let mut s = HoverState::Idle;
        transition(&mut s, on(2, 0, false), &CTX);
        assert_eq!(transition(&mut s, on(2, 1, true), &CTX), vec![KillRest]);
        assert_eq!(s, HoverState::Open { button: 2 });
        assert_eq!(
            transition(
                &mut s,
                HoverEvent::RestTimerFired { chain_open: true },
                &CTX
            ),
            vec![]
        );
    }

    #[test]
    fn rest_timer_with_chain_already_open_adopts() {
        let mut s = HoverState::Resting {
            button: 2,
            anchor: (0, 0),
        };
        assert_eq!(
            transition(
                &mut s,
                HoverEvent::RestTimerFired { chain_open: true },
                &CTX
            ),
            vec![]
        );
        assert_eq!(s, HoverState::Open { button: 2 });
    }

    #[test]
    fn moving_off_buttons_cancels_rest() {
        let mut s = HoverState::Resting {
            button: 2,
            anchor: (0, 0),
        };
        assert_eq!(
            transition(&mut s, HoverEvent::MoveOffButtons, &CTX),
            vec![KillRest]
        );
        assert_eq!(s, HoverState::Idle);
    }

    #[test]
    fn leave_cancels_rest_but_keeps_open_chain() {
        let mut s = HoverState::Resting {
            button: 2,
            anchor: (0, 0),
        };
        assert_eq!(transition(&mut s, HoverEvent::Leave, &CTX), vec![KillRest]);
        let mut s = HoverState::Open { button: 2 };
        assert_eq!(transition(&mut s, HoverEvent::Leave, &CTX), vec![]);
        assert_eq!(s, HoverState::Open { button: 2 });
    }

    #[test]
    fn right_button_cancels_rest() {
        let mut s = HoverState::Resting {
            button: 2,
            anchor: (0, 0),
        };
        assert_eq!(
            transition(&mut s, HoverEvent::RightButtonDown, &CTX),
            vec![KillRest]
        );
        assert_eq!(s, HoverState::Idle);
    }

    #[test]
    fn recent_switches_instantly_from_an_open_chain() {
        let mut s = HoverState::Open { button: 3 };
        assert_eq!(transition(&mut s, recent(1, true), &CTX), vec![Open(1)]);
    }
}
