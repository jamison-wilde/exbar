# Backlog

Standing work items and warnings. Plan pre-flight greps this file for every
touched file (see the global CLAUDE.md "Decomposition (size) discipline").

## Features

- **Recent: toggle between the two most recent folders** (requested 2026-10-03).
  Recents should always include the very latest folder, and one click should
  flip between the two most recent — the `cd -` / Alt+Tab pattern.
  Suggested UI: left-click the 🕘 Recent button navigates the active target to
  the most recent folder that is *not* the current one (so repeated clicks
  ping-pong between the last two); hover/long-press keeps opening the full
  list. Open questions: should "most recent" commit on navigation immediately
  (no dwell) so the toggle target is always current; what a click does when
  the list has fewer than two entries; Ctrl+click → new tab/window as for
  folder buttons.

## Size warnings

- `crates/exbar-cli/src/toolbar.rs` split 2026-10-03 (submenu adapter →
  `submenu_adapter.rs`); production now 925 lines.
- `crates/exbar-cli/src/wndproc.rs::toolbar_wndproc` — 1,049 lines, frozen.
  One `WM_USER_DIALOG_MRU_CHANGED` arm added under a dated ruling
  (2026-10-03). Decomposition (move `WM_*` arms into feature handlers) is
  owed; every new arm needs a ruling.
- `crates/exbar-cli/src/submenu_wnd.rs::submenu_wndproc` — 262 lines, frozen;
  one `WM_RBUTTONUP` arm added under a dated ruling (2026-10-03, Remove from
  Recents). Decomposition owed.
