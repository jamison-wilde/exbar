//! Debounce for bursty foreground-change events.
//!
//! Explorer under stress — a recursive delete of thousands of files is the
//! reproducer — emits storms of `EVENT_SYSTEM_FOREGROUND`, cycling through
//! transient windows (`ForegroundStaging`, `Static`, a `CabinetWClass` that
//! is not really in front, sometimes a null foreground) faster than a user
//! could ever switch windows. Handling each one on arrival made the toolbar
//! flash in lockstep with Explorer: show on a spurious Explorer event, hide
//! on the next watchdog tick, show again.
//!
//! The rule here: a single activation still applies immediately, so normal
//! use stays snappy and gains no latency. But once two visibility-affecting
//! events land within the debounce window, the toolbar hides and stays
//! hidden until the storm stops. Each further event re-arms the settle
//! timer. When it finally fires, [`settle_outcome`] looks at what is
//! *actually* in front — once — and picks a single final state.
//!
//! Both halves are pure and unit-tested; the Win32 adapter lives in
//! `visibility.rs` and the settle timer on the toolbar window.

// ── Event-rate debounce ──────────────────────────────────────────────────────

/// What to do with a freshly-arrived foreground event.
#[derive(Debug, PartialEq, Eq)]
pub enum EventDecision {
    /// Traffic is calm — handle this event normally, right now.
    ApplyNow,
    /// Traffic is bursty — hide the toolbar and (re)arm the settle timer.
    SuppressAndSettle { settle_ms: u32 },
}

/// Where the settle re-check should leave the toolbar.
#[derive(Debug, PartialEq, Eq)]
pub enum SettleOutcome {
    /// A usable Explorer/dialog target is genuinely in front — show on it.
    Show,
    /// Nothing usable in front — stay hidden.
    Hide,
}

/// Event-rate tracker. One per toolbar.
#[derive(Debug, Default)]
pub struct DebounceState {
    last_event_ms: Option<u64>,
    settling: bool,
}

impl DebounceState {
    /// Record a visibility-affecting foreground event and decide how to treat it.
    ///
    /// `debounce_ms` is both the "too quick" threshold and the quiet period
    /// the settle timer waits for; `0` disables debouncing entirely.
    pub fn on_event(&mut self, now_ms: u64, debounce_ms: u32) -> EventDecision {
        let previous = self.last_event_ms.replace(now_ms);

        if debounce_ms == 0 {
            return EventDecision::ApplyNow;
        }

        // Already riding out a storm: keep hiding and push the settle out.
        if self.settling {
            return EventDecision::SuppressAndSettle {
                settle_ms: debounce_ms,
            };
        }

        // saturating_sub keeps a backwards clock step from reading as a huge
        // gap, which would let a storm through.
        let gap = previous.map_or(u64::MAX, |prev| now_ms.saturating_sub(prev));
        if gap < u64::from(debounce_ms) {
            self.settling = true;
            return EventDecision::SuppressAndSettle {
                settle_ms: debounce_ms,
            };
        }

        EventDecision::ApplyNow
    }

    /// Called when the settle timer fires — the storm is over.
    pub fn on_settle(&mut self) {
        self.settling = false;
    }

    /// True while a storm is being ridden out (toolbar forced hidden).
    pub fn is_settling(&self) -> bool {
        self.settling
    }
}

// ── Settled-foreground classification ────────────────────────────────────────

/// Pure: window classes the toolbar must never ride on top of.
///
/// The desktop (`Progman`/`WorkerW`) is a deliberate exclusion — it lives in
/// explorer.exe but is not a file browser. The rest are the transient shells
/// Windows cycles through mid-switch, plus the empty string, which is what
/// `GetClassName` yields for the null HWND `GetForegroundWindow` returns
/// while no window owns the foreground.
pub fn is_usable_target_class(fg_class: &str) -> bool {
    !matches!(
        fg_class,
        "" | "Progman" | "WorkerW" | "ForegroundStaging" | "Static"
    )
}

/// Pure: decide the toolbar's final state from the settled foreground window.
///
/// `fg_is_ours` — foreground belongs to exbar (our menu, rename edit, picker).
/// `fg_class` — class name of the real foreground window.
/// `fg_root_is_active` — `GetAncestor(fg, GA_ROOT)` equals the active target,
/// covering Explorer's XAML islands and a file dialog's child popups.
pub fn settle_outcome(fg_class: &str, fg_is_ours: bool, fg_root_is_active: bool) -> SettleOutcome {
    // Our own transient windows never mean the user left Explorer.
    if fg_is_ours {
        return SettleOutcome::Show;
    }
    if !is_usable_target_class(fg_class) {
        return SettleOutcome::Hide;
    }
    if fg_class == "CabinetWClass" || fg_root_is_active {
        SettleOutcome::Show
    } else {
        SettleOutcome::Hide
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const DEBOUNCE: u32 = 300;

    #[test]
    fn first_event_applies_immediately() {
        let mut s = DebounceState::default();
        assert_eq!(s.on_event(1_000, DEBOUNCE), EventDecision::ApplyNow);
    }

    #[test]
    fn well_spaced_events_all_apply_immediately() {
        // A user alt-tabbing between apps must feel no added latency.
        let mut s = DebounceState::default();
        assert_eq!(s.on_event(1_000, DEBOUNCE), EventDecision::ApplyNow);
        assert_eq!(s.on_event(5_000, DEBOUNCE), EventDecision::ApplyNow);
        assert_eq!(s.on_event(9_000, DEBOUNCE), EventDecision::ApplyNow);
        assert!(!s.is_settling());
    }

    #[test]
    fn a_second_quick_event_starts_suppressing() {
        let mut s = DebounceState::default();
        s.on_event(1_000, DEBOUNCE);
        assert_eq!(
            s.on_event(1_050, DEBOUNCE),
            EventDecision::SuppressAndSettle {
                settle_ms: DEBOUNCE
            }
        );
        assert!(s.is_settling());
    }

    #[test]
    fn an_event_exactly_at_the_threshold_is_not_a_storm() {
        let mut s = DebounceState::default();
        s.on_event(1_000, DEBOUNCE);
        assert_eq!(
            s.on_event(1_000 + u64::from(DEBOUNCE), DEBOUNCE),
            EventDecision::ApplyNow
        );
    }

    #[test]
    fn a_storm_keeps_suppressing_even_when_events_slow_down() {
        // Once settling, every event re-arms until the timer actually fires;
        // otherwise a storm that eases briefly would flash the toolbar.
        let mut s = DebounceState::default();
        s.on_event(1_000, DEBOUNCE);
        s.on_event(1_050, DEBOUNCE);
        assert_eq!(
            s.on_event(9_000, DEBOUNCE),
            EventDecision::SuppressAndSettle {
                settle_ms: DEBOUNCE
            }
        );
    }

    #[test]
    fn settling_ends_on_the_timer_and_normal_handling_resumes() {
        let mut s = DebounceState::default();
        s.on_event(1_000, DEBOUNCE);
        s.on_event(1_050, DEBOUNCE);
        s.on_settle();
        assert!(!s.is_settling());
        assert_eq!(s.on_event(9_000, DEBOUNCE), EventDecision::ApplyNow);
    }

    #[test]
    fn zero_debounce_disables_suppression_entirely() {
        let mut s = DebounceState::default();
        assert_eq!(s.on_event(1_000, 0), EventDecision::ApplyNow);
        assert_eq!(s.on_event(1_001, 0), EventDecision::ApplyNow);
        assert!(!s.is_settling());
    }

    #[test]
    fn a_backwards_clock_step_still_reads_as_a_storm() {
        // Wall-clock adjustments must not be an escape hatch for the burst.
        let mut s = DebounceState::default();
        s.on_event(5_000, DEBOUNCE);
        assert_eq!(
            s.on_event(4_000, DEBOUNCE),
            EventDecision::SuppressAndSettle {
                settle_ms: DEBOUNCE
            }
        );
    }

    #[test]
    fn settles_showing_on_a_real_explorer_window() {
        assert_eq!(
            settle_outcome("CabinetWClass", false, false),
            SettleOutcome::Show
        );
    }

    #[test]
    fn settles_showing_on_an_explorer_xaml_island_of_the_active_target() {
        assert_eq!(
            settle_outcome("Microsoft.UI.Content.IslandWindow", false, true),
            SettleOutcome::Show
        );
    }

    #[test]
    fn settles_showing_when_our_own_window_has_focus() {
        assert_eq!(
            settle_outcome("ExbarToolbar", true, false),
            SettleOutcome::Show
        );
    }

    #[test]
    fn settles_hidden_on_the_desktop() {
        // Progman/WorkerW are explorer.exe but are not file browsers.
        assert_eq!(settle_outcome("Progman", false, false), SettleOutcome::Hide);
        assert_eq!(settle_outcome("WorkerW", false, false), SettleOutcome::Hide);
    }

    #[test]
    fn settles_hidden_on_the_transient_windows_a_storm_cycles_through() {
        assert_eq!(
            settle_outcome("ForegroundStaging", false, false),
            SettleOutcome::Hide
        );
        assert_eq!(settle_outcome("Static", false, false), SettleOutcome::Hide);
    }

    #[test]
    fn settles_hidden_when_nothing_owns_the_foreground() {
        // GetForegroundWindow returns null mid-storm; its class reads empty.
        assert_eq!(settle_outcome("", false, false), SettleOutcome::Hide);
    }

    #[test]
    fn settles_hidden_on_an_unrelated_app() {
        assert_eq!(
            settle_outcome("Chrome_WidgetWin_1", false, false),
            SettleOutcome::Hide
        );
    }

    #[test]
    fn a_desktop_class_is_not_rescued_by_a_matching_root() {
        // Defence in depth: the desktop must stay excluded even if the
        // ancestor walk somehow points back at the active target.
        assert_eq!(settle_outcome("Progman", false, true), SettleOutcome::Hide);
    }
}
