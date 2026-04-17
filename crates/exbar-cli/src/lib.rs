//! # exbar — floating folder-shortcut toolbar for Windows 11 File Explorer.
//!
//! Exbar adds a small, draggable toolbar of folder shortcuts that
//! appears alongside any active File Explorer window. It is **not** a
//! shell extension and **does not** inject any DLL into `explorer.exe`.
//!
//! ## Runtime architecture
//!
//! A single `exbar.exe` process runs continuously after login (started
//! via the `HKCU\…\Run\Exbar` key written by the MSI). Its message-pump
//! thread installs a `SetWinEventHook(EVENT_SYSTEM_FOREGROUND, …,
//! WINEVENT_OUTOFCONTEXT)` callback. When a Win11 File Explorer window
//! (`CabinetWClass`) becomes foreground, the callback creates the
//! toolbar (a top-level `WS_POPUP`) over it. Folder clicks navigate
//! the active Explorer via cross-process COM (`IShellBrowser`).
//!
//! ## Module map
//!
//! **Pure logic** (no Win32, fully unit-testable):
//! - [`layout`] — toolbar button layout from folder names + DPI.
//! - [`hit_test`] — point-in-button hit testing.
//! - [`drop_effect`] — pure drag-drop effect determination.
//! - [`mod@pointer`] — pointer-interaction state machine (hover / press / drag-reorder).
//! - [`rename`] — inline-rename state machine.
//! - [`config`] — `~/.exbar.json` schema + mutation API.
//! - [`error`] — unified [`error::ExbarError`] + [`error::ExbarResult`].
//!
//! **Trait seams** (Win32 surfaces behind `Box<dyn Trait>` for testability):
//! - [`shell_windows::ShellBrowser`] — Explorer navigation.
//! - [`picker::FolderPicker`] — `IFileOpenDialog` folder picker.
//! - [`dragdrop::FileOperator`] — `IFileOperation` move/copy.
//! - [`clipboard::Clipboard`] — `OleClipboard` text writes.
//! - [`config::ConfigStore`] — JSON file load/save.
//! - [`dialog_nav::DialogNavigator`] — keyboard-injection navigation for file dialogs.
//!
//! **Win32 wrappers** (thin adapters around system APIs):
//! - [`contextmenu`] — `TrackPopupMenu` wrapper.
//! - [`explorer`] — Explorer-window detection and class-name walking.
//! - [`theme`] — DPI scaling and dark-mode detection.
//! - [`log`] — `log` crate sink writing to `%TEMP%\exbar.log`.
//!
//! **State + UI orchestration**:
//! - [`toolbar`] — central `ToolbarState`, the wndproc, paint, and the
//!   Win32 adapters that drive [`pointer::transition`] and
//!   [`rename::transition`].
//!
//! ## Architecture diagram
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │ exbar.exe (single process, single STA message-pump)     │
//! │                                                         │
//! │   bin/exbar.rs ── run_hook ── SetWinEventHook ──┐       │
//! │                                                  ▼       │
//! │   toolbar.rs ── wndproc ──┬─► pointer::transition (pure)│
//! │                           ├─► rename::transition  (pure)│
//! │                           ├─► layout::compute_layout    │
//! │                           └─► drop_effect::effect_for   │
//! │                                                         │
//! │   trait seams (impl in *_windows / picker / dragdrop):  │
//! │     ShellBrowser  FolderPicker  FileOperator            │
//! │     Clipboard     ConfigStore                           │
//! └──────────────┬──────────────────────────────────────────┘
//!                │ COM (cross-process)
//!                ▼
//!        explorer.exe (IShellWindows / IShellBrowser)
//! ```
//!
//! ## Where to read next
//!
//! - `docs/adrs/README.md` — the six architectural decision records
//!   explaining why the major design choices are *not* the obvious thing.
//! - `CLAUDE.md` (repo root) — operational gotchas, build/deploy loop,
//!   `windows = 0.61` quirks.
//! - [`mod@pointer`] and [`rename`] — canonical examples of the
//!   pure-controller + Win32-adapter pattern.
//!
//! ## Build and install
//!
//! End users install via the MSI built by `scripts/build-msi.sh` —
//! see `README.md` at the repo root for the full install flow. For
//! development iteration, `cargo build --release -p exbar-cli`
//! produces the binary directly; the build/deploy loop is documented
//! in `CLAUDE.md`.

pub mod actions;
pub mod clipboard;
pub mod config;
pub mod contextmenu;
pub mod dialog_nav;
pub mod dragdrop;
pub mod drop_effect;
pub mod error;
pub mod explorer;
pub mod hit_test;
pub mod layout;
pub mod lifecycle;
pub mod log;
pub mod paint;
pub mod paths;
pub mod picker;
pub mod pointer;
pub mod position;
pub mod rename;
pub mod rename_edit;
pub mod shell_windows;
pub mod target;
pub mod theme;
pub mod toolbar;
pub mod visibility;
pub mod wndproc;

#[cfg(test)]
pub(crate) mod test_helpers;
