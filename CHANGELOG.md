# Changelog

All notable changes to Exbar are documented here. Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Click 🕘 to flip between your last two folders** (`cd -` style). The Recent button now navigates to the previous folder; clicking again returns, and Ctrl+click opens it in a new tab (new window in a file dialog). It uses its own instant, global, in-memory two-folder history that includes toolbar, pinned and file-dialog navigations; nothing is persisted. Requires Recent Folders to be enabled.
- **Recent Folders learn from Save As / Open dialogs.** Completing a Save or Open in a file dialog exbar is attached to adds that folder to Recents immediately; Cancel adds nothing. Dialogs expose no `IShellBrowser`, so the signal is the shell's own `ComDlg32\LastVisitedPidlMRU`, which Windows writes only on OK. A watcher thread waits on the key, coalesces each Save's write burst, and commits only entries whose app matches the dialog exbar is attached to (by exe name), so exbar's own `+` picker is ignored. Respects `recent.enabled` and `recent.excludedPaths`.
- **Remove a single Recent folder.** Right-click an entry in the 🕘 Recent popup → **Remove from Recents**. The list refreshes in place; removing the last entry shows the empty placeholder.

### Changed

- **Folders open on contact by default.** Popups no longer cover the toolbar, so the old guard delay is unnecessary: `longHoverOpenMs` now defaults to `0` (open on contact; was a 400 ms rest). Set it above `0` to restore a rest-based delay (the pointer must rest that long; movement re-arms it). Menu-bar switching is unchanged: once a submenu is open, sliding to another folder switches instantly.
- **Submenus open beside the toolbar, never over it (horizontal toolbars; vertical toolbars still overlap).** The first popup level opens on whichever side of the toolbar has room (below preferred), flush against it and capped to that side, so the bar stays visible whether it sits mid-window or in a bottom status bar. The in-place name row that covered neighbouring buttons is now a 📂 header at the toolbar-facing end of the popup.

### Fixed

- **Every other folder failed to hover-open.** Sliding from an open submenu to the next folder recorded the new button but never armed its timer once the old submenu closed, so alternate folders never opened. Hover logic moved out of the window procedure into a tested state machine (`hover_open.rs`).
- **No hover-open right after a click.** A stationary click during the hover wait no longer pops the submenu, and a submenu dismissed with Esc or a click stays closed until the pointer leaves that button.

## [1.3.1] - 2026-09-10

### Fixed

- **Duplicate "Exbar" entries in Settings → Apps.** Every reinstall added another row — 37 had accumulated on one dev machine. WiX generates a fresh ProductCode per build, and the default `MajorUpgrade` only detects related products at a *strictly lower* version, so a same-version reinstall matched nothing and installed side-by-side. `AllowSameVersionUpgrades` puts the current version inside the detected range, so a reinstall replaces its predecessors; because the UpgradeCode is fixed, the first install of this build also reclaims every stale product already present. Separately, the hand-rolled `UninstallEntry` component duplicated the ARP row Windows Installer writes itself and, being keyed by a fixed name rather than ProductCode, outlived the product it pointed at — it is gone, its two unique fields are now ARP properties, and the installer removes the stale key on install.
- **Toolbar missing on the first Explorer window after a restart.** Creation is gated on Explorer's `DesktopChildSiteBridge` XAML child, which does not exist yet when `EVENT_SYSTEM_FOREGROUND` fires for a cold Explorer; the single attempt was silently abandoned, so the toolbar waited for a later event that caught Explorer warm — which is why minimize/restore appeared to fix it. The probe now retries on a bounded schedule (250 ms, 40 attempts) from both creation call sites, and logs the not-ready path that previously left no trace at all.
- **Toolbar flashing along with Explorer.** Explorer under stress (deleting a large tree recursively, for instance) emits bursts of foreground events cycling through transient windows, and the toolbar tracked every one — showing on a spurious activation, hidden by the watchdog, shown again. Bursts are now debounced: the toolbar's visibility freezes for the duration and a single re-check settles the final state once traffic stops, verifying the top window is genuinely a usable Explorer or dialog rather than the desktop or a transient shell. A lone window switch is never delayed. Tunable via `foregroundDebounceMs` (default 300, `0` disables, applied on `Reload config`).

## [1.3.0] - 2026-05-09

### Added

- **Network folder reachability.** Mapped drives (`Z:\…`) and UNC paths (`\\server\share\…`) work as toolbar folders, with disconnected shares handled gracefully instead of hanging the UI on the SMB timeout (~30 s). A persistent worker thread probes each distinct network root once at startup (via `GetFileAttributesW` with a 3 s wall-clock budget) and on every config reload; results cache for the session in an `Arc<RwLock<ReachabilityCache>>`. Unreachable buttons render greyed (mid-grey text, no hover highlight); click is a no-op, drop is rejected with `DROPEFFECT_NONE`, and spring-open submenus are suppressed. Greyed buttons get a `Retry connection` right-click entry plus disabled Open / Open in new tab. Probe also fires immediately after a drag-onto-`+` of a network folder so the button doesn't sit Unknown. Local paths and shell aliases never touch the cache.
- **Drive roots and UNC share roots as folder buttons.** Dragging `C:\` or `Z:\` onto the `+`, or picking a drive root in the picker, now adds the folder with the trimmed path as its button label (e.g. `"C:"`, `"\\server\share"`) instead of silently failing. User can right-click → Rename to set a friendlier name.
- **Show / Hide icons toggle.** New `showIcons` config field (default `true`); toggle via the `+` button's right-click menu (`Show icons` / `Hide icons`). When `false`, the `📁` / `🕘` emoji prefix is dropped for a narrower toolbar.
- **Foreground watchdog.** Periodic safety-net timer (`foregroundWatchdogMs`, default 2 s, clamp 500..=60000, `0` disables) hides the toolbar when a spurious Explorer foreground event left it visible over an unrelated app. Optional `watchdogReshow` (default `false`) also re-shows the toolbar if the active target is foreground but the toolbar is hidden.

### Fixed

- **Spring-open submenu click in file dialogs.** Clicking a submenu item while a file dialog was foreground previously opened the path in a new Explorer window. Now the dialog navigates to the path (consistent with toolbar-button click behavior in dialog mode).
- **Dot-prefixed folders in spring-open subfolders.** Subfolders whose names start with `.` (e.g. `.git`, `.vscode`) were filtered from the top-level toolbar (intentional) but were also being filtered from nested subfolder enumerations (unintentional). They now appear in spring-open submenus.

## [1.2.0] - 2026-04-19

### Added

- **Recent Folders** (opt-in, right click on '+') — tracks folders where you spend time or take action. Enable via right-click on the `+` button → `Enable Recent Folders`. A dedicated 🕘 Recent button appears on the toolbar whose submenu lists recently-used paths (default 5, max 20, configurable). Dwell + action hybrid detection: a folder commits after `dwellSecondsToTrack` seconds (default 10 s) of being the active tab in the foreground Explorer window, OR immediately when you drop files into it via exbar. Configurable exclusion paths (prefix match with `\` boundary). `recents.json` stored under `~/.exbar/`; deleted atomically when Recent is disabled (privacy-preserving). New config block `recent` with `enabled`, `maxCount`, `includePinned`, `dwellSecondsToTrack`, `excludedPaths`. Right-clicking the 🕘 Recent button shows only `Remove`, which disables Recent and deletes `recents.json`. The Recent submenu sits entirely above or below the toolbar (never covering the 🕘 button), and hovering a recent folder enables `..` upward navigation from there.
- **Spring-open submenus** for every folder button. Long-press (500 ms, configurable) OR long-hover (1200 ms, configurable) opens a vertical popup listing that folder's subdirectories, nestable up to 7 levels deep. Drop files on any item to move/copy into that folder. `..` navigation in ancestor mode for quick up-traversal. Translucent buffer zone around each popup provides cursor forgiveness; the toolbar-facing buffer collapses to 0 at the screen edge so items sit flush against the button. Flow direction (left/right for nested popups) resolves per-subtree: hovering back up a shallower level re-evaluates fresh for the new subtree. Cursor-tracking safety timer dismisses the chain ~150 ms after the cursor leaves all popups; clicking outside OR pressing Esc dismisses immediately. Click / Ctrl-click a submenu item to navigate / open in new tab; drops invoke move/copy via `IFileOperation`. File-dialog mode degrades to `open_in_new_window`. New `submenu` block in `~/.exbar/config.json` (`springOpenDelayMs`, `longHoverOpenMs`, `hoverBufferPx`, `nonChainItemOpacity`).
- **Scroll support for large submenus.** Popups clamp their height to the monitor work area. Mouse wheel scrolls within the popup (with touchpad sub-WHEEL_DELTA accumulation); ▲ / ▼ glyphs at the top / bottom edges indicate more items available. Hovering in the scroll-trigger band at top or bottom auto-scrolls the list at ~150 ms / item. Hit-test, drop-target, and paint all correctly handle the scrolled offset.

### Changed

- **Persisted state moved to `~/.exbar/`.** Previously two files at the home root: `~/.exbar.json` (config) and `~/.exbar-pos.json` (position). Now one folder: `~/.exbar/config.json` and `~/.exbar/position.json`. The hook auto-migrates the legacy files on first run after upgrade — no manual intervention needed. Recent Folders tracking (when enabled) adds `~/.exbar/recents.json`.
- **`FolderEntry.kind`** added to config schema (optional; `"Folder"` default, also fall-back for unknown values). The Recent pseudo-button uses `"kind": "Recent"`.
- **The `+` button's folder picker** now opens at the active Explorer tab's current folder instead of always `%SystemDrive%\`.
- **Drop-target resolution** reads live toolbar state on each drop event, so newly-added folder buttons become valid drop targets immediately (previously required toolbar recreation to register).

### Fixed

- Cursor no longer disappears over the toolbar after committing an inline rename. Root cause: the toolbar window class didn't set `hCursor`, so when the rename `EDIT` child released the cursor, `DefWindowProc` had no class cursor to fall back on. Now uses `IDC_ARROW` as the class cursor.
- **Desktop foreground hides the toolbar.** Clicking the desktop (`Progman` / `WorkerW` in explorer.exe) previously left the toolbar visible on top of the wallpaper. Now explicitly hides.
- **Reposition skips when Explorer is minimized.** `IsIconic` check short-circuits `reposition_and_show` (with a defence-in-depth origin-sentinel check). Previously the 250 ms LOCATIONCHANGE timer would re-position the toolbar to the minimized-window sentinel origin and show it at the work-area top-left.
- **Hover-open no longer races right-click context menus.** `TIMER_HOVER_OPEN` is cancelled at `WM_RBUTTONDOWN` so right-click always pre-empts pending hover-open. Hover-open also uses a distinct `longHoverOpenMs` (default 1200 ms) so accidental brief hovers don't spawn a submenu.
- **Highlighted item under cursor tracks correctly across scroll.** `set_popup_highlight` maps display-items index → visible-window index before partial invalidation; out-of-view indices fall back to full popup invalidation. `refresh_highlight_from_cursor` recomputes after each scroll change.
- **Cursor on the triggering toolbar button counts as "inside" the submenu chain.** Prevents immediate dismissal when the Recent submenu opens above or below the button (no overlap).

### Removed

- Legacy flat state paths (`~/.exbar.json`, `~/.exbar-pos.json`) are no longer read after migration. The hook migrates on first start post-upgrade.

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

[1.3.1]: https://github.com/jamison-wilde/exbar/compare/v1.3.0...v1.3.1
[1.3.0]: https://github.com/jamison-wilde/exbar/compare/v1.2.0...v1.3.0
[1.2.0]: https://github.com/jamison-wilde/exbar/compare/v1.1.0...v1.2.0

[1.1.0]: https://github.com/jamison-wilde/exbar/releases/tag/v1.1.0
[1.0.0]: https://github.com/jamison-wilde/exbar/releases/tag/v1.0.0
