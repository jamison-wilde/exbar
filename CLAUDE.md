# CLAUDE.md

Orientation for AI coding tools working in this repo.

## Project

**Exbar** — a Rust CLI that shows a floating folder-shortcut toolbar in Windows 11 File Explorer, driven by an out-of-process WinEvent hook. See `README.md` for user-facing details.

## Layout

```
exbar/
├── Cargo.toml                          # workspace
├── crates/
│   └── exbar-cli/                      # lib + bin (one binary)
│       ├── Cargo.toml                  # [lints.rust]/[clippy]/[rustdoc] gates
│       ├── build.rs                    # winres version metadata
│       ├── src/
│       │   ├── lib.rs                  # Crate-level rustdoc + pub mod declarations
│       │   ├── bin/
│       │   │   └── exbar.rs            # CLI entry + run_hook() with WinEvent + message pump
│       │   ├── toolbar.rs              # ToolbarState struct + adapter impls (execute_pointer_command, execute_rename_event)
│       │   ├── wndproc.rs              # Win32 WM_* dispatcher (SP8)
│       │   ├── visibility.rs           # foreground_event_proc, install_foreground_hook, classify_foreground (SP8)
│       │   ├── lifecycle.rs            # create_toolbar, refresh_toolbar, register_drop_targets (SP8)
│       │   ├── paint.rs                # GDI render path: paint, compute_layout, in_grip (SP8)
│       │   ├── actions.rs              # Folder action handlers + pure *_to_state cores (SP8)
│       │   ├── rename_edit.rs          # Inline-rename Win32 EDIT control mgmt (SP8)
│       │   ├── position.rs             # Position persistence + pure clamp_to_work_area (SP8)
│       │   ├── pointer.rs              # Pure pointer-interaction state machine (SP2b)
│       │   ├── rename.rs               # Pure inline-rename state machine (SP6)
│       │   ├── layout.rs               # Pure button-layout computation (SP2a)
│       │   ├── hit_test.rs             # Pure point-in-button hit testing (SP2a)
│       │   ├── drop_effect.rs          # Pure drag-drop effect determination (SP2a)
│       │   ├── dragdrop.rs             # IDropTarget + FileOperator trait (SP3)
│       │   ├── shell_windows.rs        # IShellWindows enum + ShellBrowser trait (SP3)
│       │   ├── dialog_nav.rs           # DialogNavigator trait + KeybdDialogNavigator (Ctrl+L keyboard injection)
│       │   ├── target.rs               # TargetKind + ActiveTarget (Explorer vs FileDialog)
│       │   ├── explorer.rs             # check_explorer_ready, class-name walking
│       │   ├── picker.rs               # FolderPicker trait (IFileOpenDialog) (SP3)
│       │   ├── clipboard.rs            # Clipboard trait (CF_UNICODETEXT) (SP3)
│       │   ├── contextmenu.rs          # TrackPopupMenu wrapper
│       │   ├── config.rs               # Config + ConfigStore trait (SP3) — ~/.exbar/config.json
│       │   ├── paths.rs                # Filesystem layout (~/.exbar/) + one-shot legacy migration
│       │   ├── theme.rs                # DPI scale, dark-mode detection
│       │   ├── error.rs                # ExbarError + ExbarResult (SP5)
│       │   ├── log.rs                  # FileLogger (log crate) → %TEMP%\exbar.log (SP5)
│       │   ├── path_norm.rs            # Pure path normalization + prefix-exclusion match
│       │   ├── submenu.rs              # Pure chain-of-popups state machine + direction/display-list
│       │   ├── subfolder_enum.rs       # SubfolderSource trait + Win32 impl (read_dir + ▸ probe)
│       │   ├── submenu_wnd.rs          # Submenu popup HWND lifecycle (WS_EX_LAYERED + IDropTarget)
│       │   ├── submenu_adapter.rs      # Submenu popup adapter on ToolbarState: open/place/close/highlight, Recents right-click
│       │   ├── hover_open.rs           # Pure rest-based hover-open controller + adapter
│       │   ├── recent_list.rs          # Pure LRU ops for recent folders (push/dedup/trim/exclude)
│       │   ├── recent_tracker.rs       # Pure dwell+action state machine for Recent Folders
│       │   ├── toggle_history.rs       # Pure two-folder history for the 🕘 click toggle + ToolbarState adapter
│       │   ├── recent_store.rs         # RecentStore trait + JsonRecentStore → ~/.exbar/recents.json
│       │   ├── dialog_mru.rs           # Shell dialog-MRU parse + gate, DialogMruSource trait, watcher thread (file-dialog Save/Open → Recents)
│       │   ├── clock.rs                # Clock trait + SystemClock + MockClock (time-source seam)
│       │   ├── reachability.rs         # Pure ReachabilityCache + classify_root for network paths
│       │   ├── reachability_probe.rs   # ReachabilityProbe trait + Win32Probe (GetFileAttributesW + 3s timeout)
│       │   ├── bootstrap.rs           # Bounded retry for deferred toolbar creation (cold Explorer)
│       │   ├── fg_debounce.rs         # Pure foreground-storm debounce + settled-target classifier
│       │   └── bin/uia_spike.rs        # Diagnostic: dump UIA tree of a live file dialog (kept for future selector changes)
│       ├── tests/                      # integration tests
│       └── wix/
│           └── main.wxs                # WiX v4 installer definition
├── scripts/
│   ├── build-msi.sh                    # invokes `wix build`
│   └── doc-check.sh                    # RUSTDOCFLAGS="-D warnings" cargo doc gate (SP7)
└── .github/
    └── workflows/
        └── ci.yml                      # GitHub Actions: lint, test, doc-check, build-msi
```

## Commands

All commands assume `cargo` is on PATH (`export PATH="$HOME/.cargo/bin:$PATH"` in git-bash).

- **Build:** `cargo build` (dev) or `cargo build --release`
- **Run unit tests:** `cargo test` (or `cargo test -p exbar-cli`) — 290+ tests across ~36 modules
- **Build only the CLI:** `cargo build --release -p exbar-cli` (faster iteration)
- **Run the CLI:** `./target/release/exbar.exe <install|uninstall|status|hook>`
- **Build MSI:** `./scripts/build-msi.sh` (requires WiX v7 installed — see "MSI installer" section)
- **Doc gate:** `./scripts/doc-check.sh` — runs `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`; catches broken intra-doc links
- **CLI subcommands**: `hook` (production, started by Run key), `status` (diagnostics). `install` and `uninstall` are dev-only fallbacks; end users use the MSI.

## Architecture

### Loading mechanism

1. The MSI installer writes `HKCU\...\Run\Exbar = "exbar.exe hook"` and launches the hook as a post-install action.
2. `exbar.exe hook` calls `SetWinEventHook(EVENT_SYSTEM_FOREGROUND, ..., WINEVENT_OUTOFCONTEXT)` — a global foreground event hook that does NOT inject any DLL into other processes.
3. Callbacks fire on our own message-pump thread (the thread that called `SetWinEventHook` and runs `GetMessage`). When a `CabinetWClass` window becomes foreground for the first time, we create the toolbar (in our own process). Subsequent events drive show/hide.
4. Navigation, drag-drop, folder picker, and context menus all run cross-process via COM: `IShellWindows::Item()` → `IShellBrowser` proxy for `BrowseObject`, `IFileOperation` for move/copy, `IFileOpenDialog` for the folder picker.
5. The toolbar HWND is a top-level `WS_POPUP | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE` window owned by our thread. Its message pump never dies unless `exbar.exe` exits — no more orphan HWND when Explorer windows close.

### Toolbar window

- Top-level `WS_POPUP | WS_EX_TOOLWINDOW | WS_EX_LAYERED` window — **no owner**, so it survives individual Explorer window closures
- `HWND_TOPMOST` when visible — avoids z-order issues with Explorer's WinUI 3 XAML content
- `SetWinEventHook(EVENT_SYSTEM_FOREGROUND, ...)` monitors foreground changes
  - Shows toolbar when a `CabinetWClass` becomes foreground
  - Keeps toolbar visible when another `explorer.exe` window whose root ancestor is the active CabinetWClass becomes foreground (tooltips, tree view, Quick Access popups, XAML islands)
  - Ignores explorer-process windows whose root is NOT the active CabinetWClass (alt-tab/win-tab task switcher overlay)
  - Hides toolbar when a window in a different process becomes foreground
- `WM_NCHITTEST` returns `HTCAPTION` for the grip area (dots on left/top edge) to make only the grip draggable; buttons get `HTCLIENT` for normal mouse handling
- Auto-sized in `WM_CREATE` based on `compute_layout`; position is clamped to the work area of the monitor containing the triggering Explorer window
- **Relative positioning**: toolbar position is stored as an offset from the active Explorer window's origin (`~/.exbar/position.json`). `EVENT_OBJECT_LOCATIONCHANGE` (filtered to `OBJID_WINDOW`/`CHILDID_SELF` on `active_explorer`) detects Explorer maximize/restore/snap. The toolbar hides during the transition, then repositions after a configurable delay (`repositionDelayMs`, default 250ms) via `SetTimer`/`WM_TIMER`. `MOVESIZESTART`/`END` handle interactive drag (hide during, reposition after). `show_above` compares `last_explorer_origin` to detect changes from non-foreground events.
- On startup, `run_hook` checks if Explorer is already foreground and creates the toolbar immediately (fixes post-MSI-install appearance)

### Navigation

- Per-click, we look up the most-recently-activated Explorer via `state.active_explorer` (field on `ToolbarState`; set by `foreground_event_proc`)
- `shell_windows::get_shell_browser_for(hwnd)` enumerates `IShellWindows` to get a fresh `IShellBrowser` for that window (never stored — always obtained fresh to avoid stale COM references)
- `Win32Shell::navigate` (the production impl of the `ShellBrowser` trait) calls `SHParseDisplayName` → `IShellBrowser::BrowseObject(pidl, SBSP_SAMEBROWSER)`
- Click dispatch: `WM_LBUTTONUP` → `PointerEvent::Release` → `pointer::transition` returns a `PointerCommand::FireFolderClick` → `execute_pointer_command` calls `state.shell_browser.navigate(...)` or `state.shell_browser.open_in_new_tab(...)`. Tests use `MockShellBrowser`.

### Drag and drop

- `FolderDropTarget` in `dragdrop.rs` implements `IDropTarget`
- Registered on the whole toolbar window; at drop time it uses the cursor position (converted to client coords) to determine which button the drop is over
- Shell aliases (`shell:downloads`) are resolved to real paths via `SHParseDisplayName` + `SHGetPathFromIDListW` before comparing drive letters for the move/copy heuristic
- Executes the drop via `IFileOperation` with `FOF_ALLOWUNDO | FOF_NOCONFIRMMKDIR`
- Dispatches via `DropAction` enum: `MoveCopyTo(target)` for folder buttons, `AddFolder` for the `+` button (appends dropped directory to `~/.exbar/config.json`)

### Pure controllers + Win32 adapters (SP2b, SP6)

Pointer and rename interactions are split into pure state-machine modules and thin Win32 adapter methods on `ToolbarState`.

- `pointer.rs` — `PointerState`, `PointerEvent`, `PointerCommand`, `transition(state, event) → (state, Vec<command>)`. No Win32.
- `rename.rs` — `RenameState`, `RenameEvent`, `RenameAction`, `transition(...)`. No Win32.
- `submenu.rs` — `SubmenuChain`, `SubmenuEvent`, `SubmenuCommand`, `transition(...)`. No Win32.
- `recent_tracker.rs` — `TrackerState`, `TrackerEvent`, `TrackerCommand`, `transition(...)`. No Win32.
- `toggle_history.rs` — `ToggleHistory::{record, target}` (last two distinct folders). No Win32; the `ToolbarState` adapter block lives at the end of the same file.
- `fg_debounce.rs` — `DebounceState::on_event`, `settle_outcome`. No Win32.
- `hover_open.rs` — `HoverState`, `HoverEvent`, `HoverCommand`, `transition(...)`. No Win32 in the core; adapter `execute_hover_event` in the same file.
- `bootstrap.rs` — `next_action`, `should_abandon` (pure policy) + a thread-timer adapter.
- `recent_list.rs` — `push`, `for_display` — LRU mutations, no time/IO; adapter supplies `now_unix_ms` via `Clock` trait.
- `path_norm.rs` — Windows path normalization + prefix-exclusion match (no IO).
- `toolbar.rs::execute_pointer_command` / `execute_rename_event` / `execute_tracker_event` and `submenu_adapter.rs::execute_submenu_event` — the adapters. Translate `WM_*` messages or foreground events to events, call `transition`, dispatch returned commands against Win32 + trait seams.

Future interaction subsystems (context-menu controller, drag-reorder commit, etc.) should follow the same split.

### Trait seams (SP3)

All cross-process Win32 surfaces are abstracted behind traits on `ToolbarState` for mock-driven testability.

| Trait | Production impl | Used for |
|---|---|---|
| `shell_windows::ShellBrowser` | `Win32Shell` | Explorer navigation (`BrowseObject`, `open_in_new_tab`, `open_in_new_window`) |
| `picker::FolderPicker` | `Win32Picker` | `IFileOpenDialog` folder picker (accepts optional start folder) |
| `dragdrop::FileOperator` | `Win32FileOp` | `IFileOperation` move/copy |
| `clipboard::Clipboard` | `Win32Clipboard` | `OleClipboard` text writes |
| `config::ConfigStore` | `JsonFileStore` | `~/.exbar/config.json` load/save |
| `dialog_nav::DialogNavigator` | `KeybdDialogNavigator` | Ctrl+L keyboard injection into file dialogs |
| `visibility::DefViewProbe` | `Win32DefViewProbe` | Detects `SHELLDLL_DefView` descendants to recognise file dialogs |
| `subfolder_enum::SubfolderSource` | `Win32SubfolderSource` | Directory enumeration + ▸ has-children probe for submenus |
| `recent_store::RecentStore` | `JsonRecentStore` | `~/.exbar/recents.json` load/save/delete for Recent Folders |
| `dialog_mru::DialogMruSource` | `Win32DialogMru` | Newest `ComDlg32\LastVisitedPidlMRU` entry (file-dialog Save/Open) for Recent Folders |
| `clock::Clock` | `SystemClock` | Time source for dwell timestamps + debounced writes |
| `reachability_probe::ReachabilityProbe` | `Win32Probe` | Network reachability probe with 3 s wall-clock budget |

Tests inject `MockShellBrowser`, `MockFolderPicker`, `MockFileOp`, `MockClipboard`, `MockConfigStore`, `MockDialogNavigator`, `MockDefView`, `MockSubfolderSource`, `MockRecentStore`, `MockDialogMru`, `MockClock`, `MockProbe` — each mock lives in its trait's `test_mocks` sub-module; shared builders live in `test_helpers.rs` (SP8). `ToolbarState.dialog_mru` is not a `with_deps` parameter; it defaults to `Win32DialogMru` and tests replace it by field assignment — any test reaching `on_dialog_mru_changed` must assign a mock.

### Error handling (SP5)

`crate::error::ExbarError` is the unified error type (`Win32`, `Io`, `Json`, `Config`). `ExbarResult<T> = Result<T, ExbarError>`. `warn_on_err!` macro logs-and-continues on `Result`s where panic-on-error isn't appropriate (most Win32 one-shot calls).

Logging goes through the `log` crate — `log::info!` / `warn!` / `error!` / `debug!`. `FileLogger` in `log.rs` is the sink; verbosity comes from `Config.log_level`.

### State ownership (SP4)

All runtime state lives on `ToolbarState`, owned by the wndproc via `GWLP_USERDATA`. The one surviving static is `GLOBAL_TOOLBAR: Mutex<Option<isize>>` — a thread-safe bootstrap entry for `WINEVENT_OUTOFCONTEXT` callbacks (which have no `&self`) to find the toolbar HWND; from there, `unsafe { toolbar_state(hwnd) }` recovers the pointer. The safety of that helper relies on the single-threaded message-pump invariant.

### File dialog support

Beyond Explorer windows, the toolbar also activates over the Windows Common Item Dialog — every app's Save As / Open dialog on Win10/11 (Fusion, VS Code, Chrome, Office, etc.).

- **Detection**: `visibility::classify_hwnd` recognises a foreground window whose class is `#32770` AND contains a `SHELLDLL_DefView` descendant. `Win32DefViewProbe` does the child-window walk via `EnumChildWindows`. Gated by `Config.enable_file_dialogs` (default `true`).
- **Target abstraction**: `ToolbarState.active_target: Option<ActiveTarget>` pairs a foreground HWND with a `TargetKind` (`Explorer` | `FileDialog`). Positioning, `LOCATIONCHANGE` filtering, `MOVESIZESTART/END` guards, and the reposition timer are all HWND-keyed and work unchanged across both kinds.
- **Navigation**: `dialog_nav::KeybdDialogNavigator::navigate(hwnd, path)` does `SetForegroundWindow(hwnd)` → `SendInput(Ctrl+L)` (focuses the breadcrumb path bar in edit mode) → `SendInput` Unicode-typing the path → `SendInput(Enter)`. No UIA; `Ctrl+L` is the OS-level shortcut baked into every Shell-hosted dialog. Same `SendInput` rationale as `shell_windows::open_in_new_tab` — `PostMessageW` is unreliable across focus boundaries.
- **Position persistence**: `~/.exbar/position.json` stores per-kind offsets under `{"explorer": {offset_x, offset_y}, "file_dialog": {offset_x, offset_y}}`. Old flat `{offset_x, offset_y}` auto-migrates — value is promoted to `explorer` and copied to `file_dialog` so the user's tuned position applies on first dialog interaction.
- **Degraded actions in FileDialog mode**: `FireFolderClick(ctrl=true)`, right-click **Open**, and right-click **Open in new tab** all call `ShellBrowser::open_in_new_window(path)` (ShellExecuteW of `explorer.exe "path"`) — dialogs have no tabs, so opening a fresh Explorer window is the most useful degradation. Drag-drop, Copy path, Rename, Remove, Add-folder, drag-reorder, and `+` config actions all work unchanged.

### Spring-open submenus (v1.2.0)

Every folder button can spawn a hierarchical subfolder popup tree on long-press (`springOpenDelayMs`, default 500 ms) or hover (`longHoverOpenMs`, default 0 = on contact; > 0 requires the pointer to rest).

- **Hover-open controller** — `hover_open.rs`: pure `HoverState` (`Idle` / `Resting{button, anchor}` / `Open` / `Away` / `Suppressed`), `HoverEvent` (`MoveOnButton{…, chain_open}` / `MoveOffButtons` / `Leave` / `RestTimerFired` / `ButtonDown`), `HoverCommand` (`ArmRest` / `KillRest` / `Open`). With `longHoverOpenMs` > 0 a folder opens after the pointer *rests* that long (movement beyond `theme::scale(4)` px re-arms; default `0` = open on contact (rest detection applies only when > 0)). The Recent button always opens on contact. While a chain is open, hovering another folder button switches immediately (`OpenRoot` closes the old chain first). A chain dismissed under the cursor, or a press on a button, leaves it `Suppressed` until the cursor leaves that button; `Leave` into a popup moves `Open` → `Away`, so returning after the chain closed elsewhere re-arms. Every move carries `chain_open`, which is how the controller notices chains it didn't open or that closed — the old inline logic lost the next button whenever a chain closed under it (every other folder never opened). Adapter `execute_hover_event` lives in the same file.

- **Pure state machine** — `submenu.rs` owns `SubmenuChain` (vec of `ChainLevel`), `SubmenuEvent` (OpenRoot / HoverChildItem / HoverBufferAt / CursorExit / SafetyTick / CursorReenter / Commit / Dismiss), and `SubmenuCommand` (OpenLevel / CloseDeeperThan / CloseAll / SetHighlight). `transition(state, event) → Vec<command>`. No Win32.
- **Win32 adapter** — `submenu_wnd.rs` creates per-level `WS_POPUP | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | HWND_TOPMOST` windows, each with its own `IDropTarget`. `SubmenuPopup` state stored via `GWLP_USERDATA` (`Box::into_raw` / `Box::from_raw` on destroy). Paint via `paint::paint_submenu_popup` with dark-mode-aware palette, chain-opacity rendering, and partial-rect invalidation on highlight changes.
- **Level-1 placement** — pure `submenu::place_level1`: the popup opens on the side of the toolbar band that fits (Below preferred; else Above; else the larger side, height capped, overflow scrolls) and sits flush against it — never over the toolbar, wherever it is docked. Band = toolbar window rect for horizontal layout, the triggering button rect for vertical. The folder's `DisplayItem::Header` row (📂 full name, 1 px separator, click = open the folder) sits at the toolbar-facing end. The adapter is `submenu_adapter.rs` (`open_popup_level` → `load_level_contents`, `level1_band`, `leveln_origin`, `install_popup`).
- **Depth cap** `MAX_CHAIN_DEPTH = 7`. Levels 2+ flow right unless overflow triggers a one-way flip to left (reset per-subtree on truncation). `..` ancestor navigation with mode tracking (disappears once descent begins).
- **Dismiss** — 30 ms safety timer polls cursor position; transitions inside→outside arm a 5-tick (~150 ms) dismiss countdown. The toolbar window counts as "inside" for the countdown (so the cursor can return to the bar to switch buttons), but a click on it still dismisses. Click outside (`GetAsyncKeyState(VK_LBUTTON/VK_RBUTTON)` polled in the same tick) dismisses immediately. Esc via `GetAsyncKeyState(VK_ESCAPE)`. Foreground change to foreign window dismisses via existing `HwndRole::Unknown` branch.
- **Scroll** — popup height clamps to work area. `SubmenuLayout` reports `visible_count`, `total_count`, asymmetric `buffer_top_px` / `buffer_bottom_px` (toolbar-facing buffer collapses to 0 at level 1 for toolbar-edge forgiveness). `WM_MOUSEWHEEL` accumulates sub-WHEEL_DELTA values for touchpads. The header row is pinned (Top for Below popups, Bottom for Above); arrow rows (`▲`/`▼`) appear only on overflow, dim and inert when disabled, and hovering an enabled arrow auto-advances via `TIMER_SUBMENU_AUTOSCROLL` (150 ms cadence). Buffers are pure forgiveness. All display-index to row mapping goes through `SubmenuLayout::hit` / `rect_for_display_index` / `max_scroll_offset`.
- **Drop-through** — `SubmenuDropTarget::Drop` resolves the hit display-item to its path, extracts paths from the `CF_HDROP` `IDataObject`, and invokes `FileOperator::move_or_copy_paths` synchronously before posting `WM_USER_SUBMENU_CLICK` to dismiss.

### Recent Folders (v1.2.0)

Opt-in tracking of folders where the user spends time or acts.

- **Enable/disable** via `+` button's right-click menu. Enable appends a `FolderEntry{kind: "Recent"}` pseudo-entry to `folders[]`, hydrates `recent_list` from `~/.exbar/recents.json`, arms a 1 Hz dwell-tick timer. Disable deletes `recents.json` atomically, clears in-memory list, disarms tick, dismisses any open submenu chain.
- **Tracking** — pure `recent_tracker::TrackerState` + `transition(event)` returning `CommitRecent` / `ClearDwell` commands. Events: `NavigationTo`, `ForegroundLost`, `SelfInitiated`, `DwellTick`, `ActionInFolder`. Dwell fires at `dwellSecondsToTrack` (default 10s) of being the active tab in the foreground Explorer. Actions (drops onto folder buttons) commit immediately regardless of dwell. Toolbar-initiated navigations emit `SelfInitiated` to suppress self-tracking.
- **File-dialog Save/Open** — dialogs expose no `IShellBrowser`, so dwell can't see them. The shell writes `HKCU\…\ComDlg32\LastVisitedPidlMRU` on every dialog OK and never on Cancel; the `exbar-dialog-mru` thread watches it (`RegNotifyChangeKeyValue`, 150 ms coalesce) and posts `WM_USER_DIALOG_MRU_CHANGED`. `on_dialog_mru_changed` commits via `ActionInFolder` only when the active target is a `FileDialog` whose exe (captured at attach time by `visibility::attach_file_dialog` — the dialog is gone by the time the write lands) matches the entry's app name; GUID-named entries pass on target kind alone. Apps using `FOS_DONTADDTORECENT` and virtual folders are not captured.
- **Active-tab path resolution** — `IShellBrowser::QueryActiveShellView` → `IFolderView::GetFolder::<IPersistFolder2>` → `GetCurFolder()` PIDL → `SHGetPathFromIDListW`. Polled each `TIMER_DWELL_TICK`.
- **Persistence** — `RecentStore` trait; `JsonRecentStore` writes `recents.json` with a 2 s debounced `TIMER_RECENT_DEBOUNCE` on each `CommitRecent`. `flush_recent` called on toolbar `WM_DESTROY` so a clean shutdown doesn't lose pending commits.
- **LRU semantics** — `recent_list::push` dedupes case-insensitively (normalized via `path_norm::normalize`), trims to `maxCount`. `for_display` filters pinned folders at render time when `includePinned == false`. `excludedPaths` is a prefix match with `\` boundary.
- **UI** — Recent button renders `🕘 Recent` (fixed label); its root submenu uses `build_recent_display_list` (no header, no `..`, empty state shows `(no recent folders yet)`). Hovering a recent folder opens level 2 as a normal subfolder chain with `..` enabled from there down. Right-click the 🕘 button → `Remove` (same effect as Disable). Right-click an item in the Recents popup → `Remove from Recents` (`recent_list::remove` + debounced save; the popup posts `WM_USER_SUBMENU_RCLICK`, the safety timer pauses during the menu's modal loop, and the Recents root reopens to refresh).
- **Click toggle (`cd -`)** — a click on the 🕘 button navigates to the previous folder via `ToolbarState::navigate_folder` (the same path as folder-button clicks; Ctrl+click = new tab, new window in dialogs). History is `toggle_history::ToggleHistory`: instant (no dwell), global across Explorer windows/tabs and file dialogs, in-memory only, ignores `excludedPaths` and pinned status. Sources: every `TrackerEvent::NavigationTo` (recorded before the tracker transition so self-initiated navigations count) plus every exbar-performed navigation (`navigate_folder`, `navigate_or_new_window_or_tab`). `target(current)` returns the newest entry that is not `current` (normalized compare); in a file dialog `current` is unreadable so the newest entry is used. Disabled Recents = no recording, click is a no-op.

### Foreground watchdog

Periodic safety net for cases where a spurious Explorer foreground event leaves the toolbar visible over an unrelated foreground app. Pure-controller + Win32 adapter, like the other interaction subsystems.

- **Pure decision core** — `visibility::watchdog_should_hide(fg_is_ours, fg_class, fg_root_is_active) -> bool`. Returns `true` when none of: foreground is our process, class is `CabinetWClass`, or `GetAncestor(fg, GA_ROOT)` matches the active target. Fully unit-testable.
- **Win32 adapter** — `visibility::watchdog_tick(hwnd)` reads the actual foreground, classifies it, calls the core, and `ShowWindow(SW_HIDE)`s on `true`. When `Config.watchdog_reshow == true`, it also re-shows the toolbar if it was hidden while the active target is foreground.
- **Timer** — `TIMER_FOREGROUND_WATCHDOG = 8`, armed once at toolbar creation when `Config.foreground_watchdog_ms != 0`. Default 2 s, clamp 500..=60000. Changing the config value requires a hook restart; `WM_USER_RELOAD` does not re-arm.
- **Reposition-window skip** — the watchdog short-circuits while `state.reposition_pending` is set so it does not interfere with the dialog/Explorer reshow path. The timer handler clears `reposition_pending` unconditionally on fire to avoid sticky state.
- **Why this exists** — Win11 sporadically posts `EVENT_SYSTEM_FOREGROUND` for a CabinetWClass even when the user has already alt-tabbed away; the toolbar's normal foreground-driven hide path never runs because no further event arrives. The watchdog catches that mismatch within one tick.

### Foreground-storm debounce

Explorer under stress — a recursive delete of thousands of files is the reproducer — emits bursts of `EVENT_SYSTEM_FOREGROUND` cycling through transient windows (`ForegroundStaging`, `Static`, a `CabinetWClass` that isn't really in front, sometimes a null foreground). Handling each on arrival made the toolbar flash in lockstep with Explorer.

- **Pure core** — `fg_debounce.rs`: `DebounceState::on_event(now_ms, debounce_ms) -> EventDecision` (`ApplyNow` | `SuppressAndSettle`), plus `settle_outcome(fg_class, fg_is_ours, fg_root_is_active) -> SettleOutcome` and `is_usable_target_class`. No Win32.
- **Policy** — a *lone* activation always applies immediately, so ordinary window switching gains no latency. Two visibility-affecting events inside `foregroundDebounceMs` flip to suppressing: **freeze** the toolbar's current visibility and arm `TIMER_FG_SETTLE`. Every further event re-arms it, so the state holds for the whole storm.
- **Freeze, don't hide** — the first implementation force-hid the toolbar for the duration. Dogfooding measured three cases where a transient window blipped the foreground while the toolbar sat correctly over Explorer, producing a 314 ms blank followed by an immediate re-show: the anti-flicker path generating flicker. Holding the existing state dominates — a storm ending where it started is invisible, one ending elsewhere merely hides up to `settle_ms` late, and during real Explorer churn the toolbar sits still while Explorer flashes, which was the goal. Do not reintroduce the unconditional `SW_HIDE`.
- **Settle** — on timer fire, `visibility::settle_foreground` reads the real foreground *once*, re-targets `active_target` if the storm ended on a different Explorer window or a file dialog (the defview probe runs here, once per storm, never per event), then commits to a single show/hide. The desktop (`Progman`/`WorkerW`) and the transient churn classes are explicitly not usable targets.
- **Interaction guards** — the debounce is skipped entirely for our own process's foreground events and while a popup menu, submenu chain, or inline rename is active; the watchdog in turn skips while `fg_debounce.is_settling()`, so the two never fight.
- **Escape hatch** — `foregroundDebounceMs: 0` disables suppression completely and is read live, so `Reload config` toggles it without a hook restart.

### Network folder reachability

Mapped-drive (`Z:\…`) and UNC (`\\server\share\…`) folder buttons may point at shares that are unreachable. To avoid blocking the UI thread on the standard SMB timeout (~30 s), reachability is determined lazily on a worker thread and cached for the session.

- **Pure cache** — `reachability::ReachabilityCache` keyed by network root (`Z:` or `\\server\share`); `classify_root` separates network paths from local. Local paths and shell aliases never touch the cache. Mapped drives are detected via `GetDriveTypeW == DRIVE_REMOTE` (cheap, no network IO).
- **Worker thread** — one persistent thread spawned in `WM_CREATE`, consumes `mpsc::Receiver<String>` (root), invokes `ReachabilityProbe`, writes result to `Arc<RwLock<ReachabilityCache>>`, posts `WM_USER_REACHABILITY_UPDATED` to trigger a repaint. Worker exits when the channel sender drops on toolbar destroy.
- **Probe primitive** — `Win32Probe` spawns a fresh helper thread per request, calls `GetFileAttributesW` on the root with trailing `\`, joins via `mpsc::recv_timeout(3 s)`. Late helpers complete in background and discard their result.
- **Startup probe** — `request_probes_for_current_folders` walks `config.folders`, classifies each root, and for each distinct network root not yet in the cache fires a probe. Re-run on every `WM_USER_RELOAD` (drops cache entries for removed folders, fires probes for newly-added). Also called after `actions::append_folder_and_reload` so a drag-add of a network folder probes immediately.
- **UI integration** — paint, click, drop-target hover, drop fire, and submenu spring-open all consult the cache. `Unreachable` → text greyed (mid-grey on both themes), no hover highlight, click no-op, drop effect overridden to `DROPEFFECT_NONE` (cursor shows ⊘), submenu open suppressed (read_dir would hang).
- **Recovery** — `Unreachable` buttons get a "Retry connection" right-click context-menu entry and have Open / Open-in-new-tab greyed (`MF_GRAYED`). Retry calls `request_probe(root)` to re-run the probe.
- **Limitations** — mid-session disconnect of a previously-`Reachable` share will hang once on the next click before the user can retry; Recent submenu items are not reachability-greyed in v1; the brief Unknown window between toolbar create and probe completion (≤3 s) treats network roots as Reachable.

### Context menus and inline rename

- The `+` button (first slot) has three interactions:
  - **Left-click** → `picker.rs` opens `IFileOpenDialog` with `FOS_PICKFOLDERS`, starting at `%SystemDrive%\`; selected folder appended via `Config::add_folder` + `save()`
  - **Right-click** → `Edit config` (ShellExecute opens `~/.exbar/config.json` in default handler) / `Reload config` (posts `WM_USER_RELOAD`) / `Show icons` (flip-label, toggles `Config.show_icons` via `actions::toggle_icons_in_state` — hides the `📁`/`🕘` button-label emoji for a denser toolbar) / `Enable`/`Disable Recent Folders`
  - **Drop a single directory** → same path as click-picker result
- Folder buttons:
  - **Left-click** → navigate active Explorer via `IShellBrowser::BrowseObject`
  - **Ctrl+left-click** → `navigate::open_in_new_tab` — posts Ctrl+T to the active Explorer HWND, polls `IShellWindows` for up to `newTabTimeoutMsZeroDisables` ms looking for a newly-appeared HWND, navigates it; on timeout or `0` config, falls back to `ShellExecuteW("explorer.exe", "\"path\"")`
  - **Right-click** → `Open / Open in new tab / Copy path / --- / Rename / Remove`
  - **Rename** spawns a child `EDIT` control (`start_inline_rename` in `rename_edit.rs`) subclassed via `SetWindowSubclass` (ref_data = toolbar HWND) to intercept Enter (commit), Esc (cancel), `WM_KILLFOCUS` (commit). The subclass proc translates Win32 messages to `RenameEvent`s and calls `state.execute_rename_event()`, which drives the pure `rename::transition` function in `rename.rs`. Empty commit keeps the old name via `Config::rename_folder`'s trim-empty guard.
- The `contextmenu.rs` wrapper exposes `show_menu(owner, pt, items) -> u32` around `TrackPopupMenu` with `TPM_RETURNCMD`

## Gotchas

- **`windows` crate v0.61 quirks**:
  - `BOOL` is `windows_core::BOOL`, NOT `windows::Win32::Foundation::BOOL`
  - `GetSysColor` / `SYS_COLOR_INDEX` are in `Win32::Graphics::Gdi`, not `Win32::UI::WindowsAndMessaging`
  - `IObjectWithSite` is in `Win32::System::Ole`
  - Many APIs take `Option<HWND>` / `Option<HMENU>` / `Option<HINSTANCE>`
  - `#[implement]` trait method signatures use `Ref<'_, T>` for optional COM params, not `Option<&T>`
  - `DeleteObject` expects `HGDIOBJ`; convert with `.into()` from `HBRUSH` / `HPEN` / `HFONT`
- **Win11 Explorer window hierarchy**: command bar is rendered by `Microsoft.UI.Content.DesktopChildSiteBridge` (WinUI 3 XAML). Cannot inject Win32 child windows into that hierarchy. We use a separate top-level popup instead. The old approach of overlaying the command bar area is abandoned — don't reintroduce it.
- **`WINEVENT_SKIPOWNPROCESS`**: do NOT set this flag on the foreground-window WinEvent hook. Most events we care about (Explorer activations) happen in explorer.exe itself.
- **`newTabTimeoutMsZeroDisables` semantics**: config field controls ctrl-click-new-tab behavior. `0` disables the new-tab attempt entirely (always opens a new Explorer window). Any positive value is both the poll ceiling AND the trigger to try the tab path. Clamped to `0..=5000` during deserialization.
- **Inline rename on layered window**: the `EDIT` control is a child of the `WS_EX_LAYERED` toolbar. If paint artifacts appear, replace the child-window approach with a small `CreateDialogIndirectParamW` modal keyed to the button's screen rect.
- **Inline rename ownership (post-SP6)**: `SetWindowSubclass`'s `ref_data` is the **toolbar HWND** (`usize`), not a leaked `Box`. The subclass proc reaches context (folder index, edit HWND) via `toolbar_state(toolbar).rename_state`. Commit/cancel flow through `state.execute_rename_event(RenameEvent::CommitRequested|Cancelled)` → `rename::transition` → adapter executes `ApplyRename` / `DestroyEdit` / `ReloadToolbar` actions. `destroy_rename_edit` calls `RemoveWindowSubclass` before `DestroyWindow` so the WM_DESTROY re-entry can't reach our subclass proc. Do NOT fall through to `DefSubclassProc` after the adapter clears `rename_state` — the HWND has been destroyed.
- **Toolbar UI thread blocks during `open_in_new_tab`**: the poll sleeps up to `newTabTimeoutMsZeroDisables` ms on the toolbar's wndproc thread. Accepted trade-off for v0.2.0 simplicity; revisit with a worker-thread variant if it feels bad.
- **Hook process must not show a console**: `exbar.exe hook` calls `FreeConsole()` at the start to detach from any inherited console. The MSI's post-install custom action otherwise opens a visible terminal window. Don't add `println!` calls in `run_hook()` after `FreeConsole` — they'll silently no-op.
- **Process-name detection for the foreground hook**: `hwnd_in_our_process` checks PID against `std::process::id()` (exbar.exe). `hwnd_in_explorer_process` does an executable-name check (`explorer.exe`) via `GetModuleFileNameExW`. The combination keeps the toolbar visible over Explorer's own popups (tooltips, tree-views, Quick Access flyouts) while still hiding when a different app takes foreground.
- **Alt-tab/win-tab overlay is explorer.exe**: the task switcher UI uses `XamlExplorerHostIslandWindow` in explorer.exe's process — same class as Explorer's own XAML content. Distinguish via `GetAncestor(hwnd, GA_ROOT)`: if the root is the active CabinetWClass, it's a real Explorer child (show toolbar); if not, it's the task switcher (ignore).
- **Win11 new-tab detection**: `IShellWindows` entries for tabs in the same window share the same HWND. Detect a new tab by `IShellWindows.Count()` increase, not by new-HWND appearance. The new tab is the last entry in enumeration. `open_in_new_tab` uses `SendInput` (hardware-level Ctrl+T injection) rather than `PostMessageW` because PostMessage doesn't reach Explorer after a right-click context menu steals focus.
- **`OleInitialize` required**: `RegisterDragDrop` requires `OleInitialize`, not just `CoInitializeEx(COINIT_APARTMENTTHREADED)`. Without it, drop target registration silently fails.
- **Foreground hook must be installed before the first toolbar exists**: `install_foreground_hook()` is called from `run_hook()` (not from toolbar `WM_CREATE`) because the hook is what creates the toolbar on the first `CabinetWClass` foreground event. Chicken-and-egg if reversed. However, `run_hook` also checks if Explorer is already foreground and creates the toolbar immediately (handles post-MSI-install case).
- **A cold Explorer isn't ready when its foreground event fires**: `explorer::check_explorer_ready` gates creation on the `Microsoft.UI.Content.DesktopChildSiteBridge` child, which does not exist yet on the first Explorer launch after login. Both creation call sites (`visibility::handle_explorer_foreground` and `run_hook`'s startup check) hand a failed probe to `bootstrap::schedule_retry` instead of giving up — 250 ms re-probes, 40-attempt budget, abandoned early only if a toolbar appears or the Explorer window dies. Note the split between `should_abandon` (terminal conditions) and `ready_to_create` (per-probe gate): the foreground momentarily not being Explorer must NOT end the attempt, because a cold Explorer launch flickers focus while it starts — observed live, mid-probe, on exactly the launch this exists to rescue. Focus only gates creation, so a blip costs one probe instead of the toolbar. The retry timer is a **thread** timer (`SetTimer` with a null HWND plus a `TIMERPROC`, dispatched by the existing `DispatchMessageW` pump) precisely because no toolbar window exists yet to own a normal timer. Without this the toolbar was missing until a later foreground event caught Explorer warm — which is why minimize/restore "fixed" it.
- **Why Ctrl+L for file-dialog navigation, not UIA**: the 2026-04-16 UIA spike found that the Common Item Dialog's breadcrumb is NOT a `ControlType.Edit` in the static UIA tree — it's a `Pane`/`Toolbar` composite with `Button` children that only becomes editable on click. The filename Edit (`AutomationId=1001`) does accept `SetValue` but clobbers user-typed filenames on Save As (hostile UX). Ctrl+L is the OS-level Shell shortcut that focuses the breadcrumb in edit mode directly — no selector required, no filename clobber. Full spike trace in `docs/superpowers/spikes/2026-04-16-uia-spike-results.md`. The `crates/exbar-cli/src/bin/uia_spike.rs` binary is retained for future diagnostic use.
- **Foreground set before keyboard injection**: `SendInput` targets whichever window has the foreground at the moment of injection. `KeybdDialogNavigator::navigate` calls `SetForegroundWindow(dialog_hwnd)` + 50 ms sleep before injecting Ctrl+L, the Unicode path, and Enter. Without the focus step, keystrokes can land in the wrong window during rapid clicks. Same pattern as the Ctrl+T injection in `shell_windows::open_in_new_tab`.
- **Position schema backward-compat**: `~/.exbar/position.json` migrated from flat `{offset_x, offset_y}` to `{"explorer": ..., "file_dialog": ...}` in v1.1. `PositionStore::from_json_str` accepts both via a serde `untagged` enum — old flat shape loads as the `explorer` offset and copies the same value to `file_dialog` (better UX than defaulting dialog offset to zero).
- **State directory layout (v1.2)**: persisted state lives under `~/.exbar/` (`config.json`, `position.json`). Pre-1.2 versions used `~/.exbar.json` and `~/.exbar-pos.json` at the home root. `paths::migrate_legacy_files()` runs at hook startup, idempotent and best-effort, and renames the old files into the new directory. Don't add new state files outside `~/.exbar/` — keep the layout one folder.
- **`hCursor` on the toolbar window class**: `WNDCLASSEXW.hCursor` MUST be set (we use `IDC_ARROW`). Without it, after the inline-rename `EDIT` child is destroyed the cursor "disappears" over the toolbar — `DefWindowProc::WM_SETCURSOR` falls back to the class cursor, which was null. Hover events still fire (highlights work), only the visible cursor is missing.

## Logging

All logs go to `%TEMP%\exbar.log` with format `HH:MM:SS.mmm [LEVEL] pid=N message`. Use this as the first diagnostic tool when something isn't working as expected.

```bash
type C:\Users\slain\AppData\Local\Temp\exbar.log
```

## Build & deploy loop (live-iteration)

When iterating on exbar while the hook is running:

```bash
# 1. Build
cargo build --release -p exbar-cli

# 2. Stop hook (no DLL lock = no rename dance)
taskkill /f /im exbar.exe

# 3. Replace binary
cp target/release/exbar.exe %LOCALAPPDATA%/Exbar/exbar.exe

# 4. Clear log for a clean diagnostic run
rm -f %TEMP%/exbar.log

# 5. Restart hook
./target/release/exbar.exe hook
```

## MSI installer (WiX)

The installer is defined in `crates/exbar-cli/wix/main.wxs` (WiX v4 schema). Built via `./scripts/build-msi.sh` which invokes `wix build` directly — `cargo-wix` v0.3 generates WiX v3 templates and can't drive WiX v7.

The MSI:
- **Per-user install** (`Scope="perUser"`) to `%LOCALAPPDATA%\Exbar\` — no UAC prompt
- **Run key** at `HKCU\...\Run\Exbar` so the hook auto-starts at login
- **Uninstall entry** under `HKCU\...\Uninstall\Exbar` so it appears in Settings → Apps
- **Start Menu shortcut** so users can re-launch after killing the hook
- **Post-install custom action** launches `exbar.exe hook` immediately (deferred, impersonated, async-no-wait)
- **`util:CloseApplication`** shuts down running `exbar.exe` before file replacement on upgrade/uninstall

The WiX Util extension (`WixToolset.Util.wixext`) is required for `util:CloseApplication`.

**UpgradeCode** `E47632D3-B73C-4EE3-B987-D2E04332BCDB` is fixed in `main.wxs` and must never change across versions. Changing it makes new versions install side-by-side instead of replacing the old one.

WiX v7 install (one-time per machine):
```
dotnet tool install --global wix
wix eula accept wix7
wix extension add --global WixToolset.Util.wixext
```

## Adding a new feature

1. All runtime behavior lives in `exbar-cli`; the WiX installer is purely for packaging
2. Prefer inline `#[cfg(test)] mod tests` over `tests/` — the post-SP1.5 lib/bin split makes inline tests the natural choice. Pure modules (`pointer`, `rename`, `layout`, `hit_test`, `drop_effect`, `config`, `error`, `position`, `actions`, `visibility`) are fully unit-testable; Win32-touching code is tested via the trait seams + mocks (each mock lives in its trait file's `test_mocks` sub-module; shared builders live in `test_helpers.rs`). Only truly Win32-API-heavy code (paint, wndproc dispatch) stays manual-smoke.
3. For new state-machine logic, follow the pure-controller + adapter pattern: pure `transition()` module + thin `execute_*` adapter on `ToolbarState` (see `pointer.rs`/`rename.rs` for examples).
4. For new cross-process Win32 surfaces, follow the trait-seam pattern: trait + `Win32*` impl + mock (see `shell_windows.rs`/`dragdrop.rs` for examples).
5. All UI pixel values must pass through `theme::scale(px, dpi)` — no hardcoded pixels
6. All theme colors must branch on `theme::is_dark_mode()` — don't assume dark
7. Catch panics at FFI boundaries with `std::panic::catch_unwind` (see `wndproc::toolbar_wndproc_safe`)
8. Before pushing, run `cargo fmt && cargo clippy --all-targets && cargo test && ./scripts/doc-check.sh`. All four must pass.
9. Document architectural decisions as comments in the relevant module or in this file's Architecture section.
