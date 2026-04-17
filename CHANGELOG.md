# Changelog

All notable changes to Exbar are documented here. Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project uses [Semantic Versioning](https://semver.org/).

## [1.1.0] - 2026-04-17

### Added
- **File dialog support.** Toolbar activates over Windows file dialogs (Save As / Open) in any app using the modern Common Item Dialog or the legacy `GetOpenFileName` API. Click a toolbar folder to retarget the dialog to that path; drag a file out of the dialog onto a toolbar folder to move or copy it there.
- `enableFileDialogs` config flag (default `true`) — set to `false` to keep Explorer-only behaviour.
- Per-target-kind position persistence. `~/.exbar-pos.json` now stores separate offsets for Explorer vs file dialogs under `{"explorer": ..., "file_dialog": ...}`. Old flat-schema files auto-migrate on first load.

### Fixed
- Alt-tab / Win-tab task switcher no longer triggers the toolbar to appear. The switcher is hosted in `explorer.exe`, so a process-name check isn't enough; we now compare `GetAncestor(hwnd, GA_ROOT)` against the tracked active CabinetWClass.
- Ctrl-click and right-click **Open in new tab** now reliably open a tab instead of a new window on Windows 11. Tabs in the same Explorer window share one HWND, so new-tab detection is now count-based (`IShellWindows::Count()`), and `Ctrl+T` is injected via `SendInput` so it survives the focus loss caused by context menus.

### Changed
- `active_explorer` field on `ToolbarState` generalised to `active_target: Option<ActiveTarget>`, carrying a `TargetKind` discriminator (`Explorer` vs `FileDialog`). Dispatch branches on kind; positioning remains target-agnostic.

## [1.0.0] - 2026-04-15

First public release.

### Added
- Floating folder-shortcut toolbar for Windows 11 File Explorer, driven by an out-of-process WinEvent hook.
- Tab-aware navigation: clicking a folder changes the active tab; `Ctrl`-click opens in a new tab.
- Drag-and-drop support:
  - Drop files onto a folder button to move (same drive) or copy (different drive), with `Ctrl`/`Shift` overrides.
  - Drop a folder onto the `+` button to add it to the toolbar.
  - Drag-reorder folder buttons in place.
- Right-click menus on folder buttons (Open / Open in new tab / Copy path / Rename / Remove) and on `+` (Edit config / Reload config).
- Relative-position tracking: toolbar follows its Explorer window across move, drag, maximize, restore, snap, and foreground-switch events. Offset persists in `~/.exbar-pos.json`.
- Per-user MSI installer that registers an HKCU Run key, a Start Menu shortcut, and an uninstall entry.
- Configurable `repositionDelayMs` to tune the animation-aware reposition debounce (default 250 ms).
- GitHub Actions CI: lint, test, doc-check, and MSI build on every push; automatic release creation on tag push.

[Unreleased]: https://github.com/jamison-wilde/exbar/compare/v1.1.0...HEAD
[1.1.0]: https://github.com/jamison-wilde/exbar/releases/tag/v1.1.0
[1.0.0]: https://github.com/jamison-wilde/exbar/releases/tag/v1.0.0
