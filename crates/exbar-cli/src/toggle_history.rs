//! Instant two-folder history behind the Recent button's `cd -` toggle.
//!
//! Separate from the dwell-gated Recents list: it records every folder the
//! user lands in (including self-initiated and pinned navigations), is global
//! across Explorer windows/tabs and file dialogs, lives in memory only, and
//! ignores `excludedPaths`.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::HWND;

use crate::path_norm::normalize;

fn same(a: &Path, b: &Path) -> bool {
    normalize(&a.to_string_lossy()) == normalize(&b.to_string_lossy())
}

/// The last two distinct folders the user was in, newest first.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ToggleHistory {
    newest: Option<PathBuf>,
    previous: Option<PathBuf>,
}

impl ToggleHistory {
    /// Record a folder the user is now in. No-op if it equals `newest`
    /// (normalized compare via [`normalize`], same rule as `recent_list::push`);
    /// otherwise newest becomes previous. If it equals `previous` the two swap.
    pub fn record(&mut self, path: &Path) {
        if self.newest.as_deref().is_some_and(|n| same(n, path)) {
            return;
        }
        self.previous = self.newest.take();
        self.newest = Some(path.to_path_buf());
    }

    /// Where a toggle click should go: the newest entry that is not `current`.
    /// `current == None` (e.g. a file dialog, whose folder can't be read) gives
    /// `newest`. `None` when there is nothing distinct to go to.
    pub fn target(&self, current: Option<&Path>) -> Option<PathBuf> {
        [&self.newest, &self.previous]
            .into_iter()
            .flatten()
            .find(|p| current.is_none_or(|c| !same(p, c)))
            .cloned()
    }
}

impl crate::toolbar::ToolbarState {
    /// Record `path` in the toggle history when Recents is enabled.
    pub(crate) fn record_toggle_history(&mut self, path: &Path) {
        if self.config.as_ref().is_some_and(|c| c.recent.enabled) {
            self.toggle_history.record(path);
        }
    }

    /// Remember the folder exbar just navigated the active file dialog to.
    pub(crate) fn note_dialog_nav(&mut self, path: &Path) {
        if let Some(t) = self.active_target
            && t.kind == crate::target::TargetKind::FileDialog
        {
            self.dialog_last_nav = Some((t.hwnd.0 as isize, path.to_path_buf()));
        }
    }

    /// Last folder exbar sent the *active* dialog to, if it is the same dialog.
    fn dialog_current(&self) -> Option<PathBuf> {
        let t = self.active_target?;
        if t.kind != crate::target::TargetKind::FileDialog {
            return None;
        }
        match &self.dialog_last_nav {
            Some((h, p)) if *h == t.hwnd.0 as isize => Some(p.clone()),
            _ => None,
        }
    }

    /// Handle a click on the Recent button: reads the active Explorer tab's
    /// folder (COM) and delegates to [`Self::toggle_click_with_current`].
    pub(crate) fn on_recent_button_click(&mut self, toolbar: HWND, ctrl: bool) {
        if !self.config.as_ref().is_some_and(|c| c.recent.enabled) {
            return;
        }
        let current = self.current_active_tab_path();
        self.toggle_click_with_current(toolbar, current, ctrl);
    }

    /// Testable core of the toggle: go to the history target that is not
    /// `current`, navigating like a folder-button click.
    pub(crate) fn toggle_click_with_current(
        &mut self,
        toolbar: HWND,
        current: Option<PathBuf>,
        ctrl: bool,
    ) {
        if !self.config.as_ref().is_some_and(|c| c.recent.enabled) {
            return;
        }
        // A dialog's folder can't be read; use where exbar last sent it.
        let current = if self
            .active_target
            .is_some_and(|t| t.kind == crate::target::TargetKind::FileDialog)
        {
            self.dialog_current()
        } else {
            current
        };
        let Some(path) = self.toggle_history.target(current.as_deref()) else {
            log::debug!("recent toggle: no distinct folder to go to");
            return;
        };
        self.navigate_folder(toolbar, &path, ctrl);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }
    fn hist(paths: &[&str]) -> ToggleHistory {
        let mut h = ToggleHistory::default();
        for x in paths {
            h.record(Path::new(x));
        }
        h
    }

    #[test]
    fn empty_has_no_target() {
        assert_eq!(ToggleHistory::default().target(None), None);
    }

    #[test]
    fn single_entry_targets_itself_only_when_current_unknown() {
        let h = hist(&["C:\\A"]);
        assert_eq!(h.target(None), Some(p("C:\\A")));
        assert_eq!(h.target(Some(Path::new("C:\\A"))), None);
    }

    #[test]
    fn two_entries_pick_the_other_one() {
        let h = hist(&["C:\\A", "C:\\B"]);
        assert_eq!(h.target(Some(Path::new("C:\\B"))), Some(p("C:\\A")));
        assert_eq!(h.target(Some(Path::new("C:\\A"))), Some(p("C:\\B")));
        assert_eq!(h.target(None), Some(p("C:\\B")));
    }

    #[test]
    fn duplicate_newest_is_noop() {
        let h = hist(&["C:\\A", "C:\\B", "C:\\B"]);
        assert_eq!(h, hist(&["C:\\A", "C:\\B"]));
    }

    #[test]
    fn dedupe_is_case_and_trailing_slash_insensitive() {
        let h = hist(&["C:\\Foo", "c:\\foo\\\\"]);
        assert_eq!(h.target(Some(Path::new("C:\\other"))), Some(p("C:\\Foo")));
        assert_eq!(h.target(Some(Path::new("C:\\FOO"))), None);
    }

    #[test]
    fn revisiting_previous_swaps() {
        let h = hist(&["C:\\A", "C:\\B", "C:\\A"]);
        assert_eq!(h.newest, Some(p("C:\\A")));
        assert_eq!(h.previous, Some(p("C:\\B")));
    }

    #[test]
    fn only_last_two_are_kept() {
        let h = hist(&["C:\\A", "C:\\B", "C:\\C"]);
        assert_eq!(h.newest, Some(p("C:\\C")));
        assert_eq!(h.previous, Some(p("C:\\B")));
    }

    #[test]
    fn unrelated_current_gets_newest() {
        let h = hist(&["C:\\A", "C:\\B"]);
        assert_eq!(h.target(Some(Path::new("C:\\Z"))), Some(p("C:\\B")));
    }

    // ── adapter tests ──────────────────────────────────────────────────────

    use crate::config::Config;
    use crate::target::ActiveTarget;
    use crate::test_helpers::{make_test_state, mk_deps};
    use windows::Win32::Foundation::HWND;

    fn recents_cfg(enabled: bool) -> Config {
        Config::from_str(&format!(
            r#"{{"folders":[{{"name":"A","path":"C:\\A"}},{{"name":"Recent","kind":"Recent"}}],"recent":{{"enabled":{enabled}}}}}"#
        ))
        .unwrap()
    }
    fn hwnd() -> HWND {
        HWND(std::ptr::dangling_mut())
    }

    #[test]
    fn explorer_toggle_goes_to_previous_folder() {
        let deps = mk_deps();
        let mut st = make_test_state(&deps, Some(recents_cfg(true)));
        st.active_target = Some(ActiveTarget::explorer(HWND(7 as *mut _)));
        st.toggle_history.record(Path::new("C:\\A"));
        st.toggle_history.record(Path::new("C:\\B"));

        st.toggle_click_with_current(hwnd(), Some(PathBuf::from("C:\\B")), false);

        let calls = deps.navigate_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], (7, PathBuf::from("C:\\A")));
    }

    #[test]
    fn dialog_toggle_goes_to_newest() {
        let deps = mk_deps();
        let mut st = make_test_state(&deps, Some(recents_cfg(true)));
        st.active_target = Some(ActiveTarget::file_dialog(HWND(99 as *mut _)));
        st.toggle_history.record(Path::new("C:\\A"));
        st.toggle_history.record(Path::new("C:\\B"));

        st.toggle_click_with_current(hwnd(), None, false);

        let calls = deps.dialog_nav.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, 99);
        assert_eq!(calls[0].1, PathBuf::from("C:\\B"));
    }

    #[test]
    fn ctrl_toggle_in_explorer_opens_new_tab() {
        let deps = mk_deps();
        let mut st = make_test_state(&deps, Some(recents_cfg(true)));
        st.active_target = Some(ActiveTarget::explorer(HWND(7 as *mut _)));
        st.toggle_history.record(Path::new("C:\\A"));
        st.toggle_history.record(Path::new("C:\\B"));

        st.toggle_click_with_current(hwnd(), Some(PathBuf::from("C:\\B")), true);

        let tabs = deps.new_tab_calls.lock().unwrap();
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].1, PathBuf::from("C:\\A"));
        assert!(deps.navigate_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn ctrl_toggle_in_dialog_opens_new_window() {
        let deps = mk_deps();
        let mut st = make_test_state(&deps, Some(recents_cfg(true)));
        st.active_target = Some(ActiveTarget::file_dialog(HWND(99 as *mut _)));
        st.toggle_history.record(Path::new("C:\\A"));

        st.toggle_click_with_current(hwnd(), None, true);

        let wins = deps.new_window_calls.lock().unwrap();
        assert_eq!(*wins, vec![PathBuf::from("C:\\A")]);
    }

    #[test]
    fn toggle_with_empty_history_does_nothing() {
        let deps = mk_deps();
        let mut st = make_test_state(&deps, Some(recents_cfg(true)));
        st.active_target = Some(ActiveTarget::explorer(HWND(7 as *mut _)));

        st.toggle_click_with_current(hwnd(), Some(PathBuf::from("C:\\B")), false);

        assert!(deps.navigate_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn toggle_with_recents_disabled_does_nothing_and_records_nothing() {
        let deps = mk_deps();
        let mut st = make_test_state(&deps, Some(recents_cfg(false)));
        st.active_target = Some(ActiveTarget::explorer(HWND(7 as *mut _)));
        st.toggle_history = hist(&["C:\\A", "C:\\B"]);

        st.toggle_click_with_current(hwnd(), Some(PathBuf::from("C:\\B")), false);
        st.navigate_folder(hwnd(), Path::new("C:\\Z"), false);

        // Only the explicit navigate_folder call went through; the toggle did not.
        let calls = deps.navigate_calls.lock().unwrap();
        assert_eq!(*calls, vec![(7, PathBuf::from("C:\\Z"))]);
        assert_eq!(st.toggle_history, hist(&["C:\\A", "C:\\B"]));
    }

    #[test]
    fn navigation_event_records_even_after_self_initiated() {
        let deps = mk_deps();
        let mut st = make_test_state(&deps, Some(recents_cfg(true)));
        st.execute_tracker_event(hwnd(), crate::recent_tracker::TrackerEvent::SelfInitiated);
        st.execute_tracker_event(
            hwnd(),
            crate::recent_tracker::TrackerEvent::NavigationTo(PathBuf::from("C:\\N")),
        );
        assert_eq!(st.toggle_history.target(None), Some(PathBuf::from("C:\\N")));
    }

    #[test]
    fn navigate_folder_records_its_path() {
        let deps = mk_deps();
        let mut st = make_test_state(&deps, Some(recents_cfg(true)));
        st.active_target = Some(ActiveTarget::explorer(HWND(7 as *mut _)));
        st.navigate_folder(hwnd(), Path::new("C:\\Z"), false);
        assert_eq!(st.toggle_history.target(None), Some(PathBuf::from("C:\\Z")));
    }

    #[test]
    fn dialog_toggle_ping_pongs() {
        let deps = mk_deps();
        let mut st = make_test_state(&deps, Some(recents_cfg(true)));
        st.active_target = Some(ActiveTarget::file_dialog(HWND(99 as *mut _)));
        st.toggle_history = hist(&["C:\\A", "C:\\B"]);

        for _ in 0..3 {
            st.toggle_click_with_current(hwnd(), None, false);
        }

        let calls = deps.dialog_nav.calls.borrow();
        let paths: Vec<_> = calls.iter().map(|c| c.1.clone()).collect();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("C:\\B"),
                PathBuf::from("C:\\A"),
                PathBuf::from("C:\\B")
            ]
        );
    }

    #[test]
    fn dialog_last_nav_is_not_reused_for_another_dialog() {
        let deps = mk_deps();
        let mut st = make_test_state(&deps, Some(recents_cfg(true)));
        st.active_target = Some(ActiveTarget::file_dialog(HWND(99 as *mut _)));
        st.toggle_history = hist(&["C:\\A", "C:\\B"]);
        st.toggle_click_with_current(hwnd(), None, false); // dialog 99 -> B
        st.active_target = Some(ActiveTarget::file_dialog(HWND(100 as *mut _)));

        // Dialog 100's folder is unknown, so it gets the newest (B), not A.
        st.toggle_click_with_current(hwnd(), None, false);

        let calls = deps.dialog_nav.calls.borrow();
        assert_eq!(
            (calls[1].0, calls[1].1.clone()),
            (100, PathBuf::from("C:\\B"))
        );
    }

    #[test]
    fn navigate_folder_without_target_records_nothing() {
        let deps = mk_deps();
        let mut st = make_test_state(&deps, Some(recents_cfg(true)));
        st.navigate_folder(hwnd(), Path::new("C:\\Z"), false);
        assert_eq!(st.toggle_history, ToggleHistory::default());
    }
}
