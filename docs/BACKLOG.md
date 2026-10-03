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

- **Vertical toolbars: place level-1 popups at the toolbar's right/left edge**
  (found 2026-10-03). Side-aware placement only handles horizontal bars;
  vertical bars still open the popup over the triggering button's row.

## Size warnings

- `crates/exbar-cli/src/toolbar.rs` split 2026-10-03 (submenu adapter →
  `submenu_adapter.rs`); production now 925 lines (cap 1,500).
- `crates/exbar-cli/src/wndproc.rs::toolbar_wndproc` — 965 lines (shrunk from
  1,056 on 2026-10-03 when the hover logic moved to `hover_open.rs`; the
  `WM_USER_SUBMENU_RCLICK` arm rode on that removal), frozen.
  One `WM_USER_DIALOG_MRU_CHANGED` arm added under a dated ruling
  (2026-10-03). Decomposition (move `WM_*` arms into feature handlers) is
  owed; every new arm needs a ruling.
- `crates/exbar-cli/src/submenu_wnd.rs::submenu_wndproc` — 262 lines, frozen;
  one `WM_RBUTTONUP` arm added under a dated ruling (2026-10-03, Remove from
  Recents). Decomposition owed.
- `crates/exbar-cli/src/toolbar.rs::execute_pointer_command` — 166 lines,
  pre-existing over-cap, frozen. Decomposition owed.
- `crates/exbar-cli/src/paint.rs::paint` — 278 lines, pre-existing over-cap,
  frozen. Decomposition owed.
