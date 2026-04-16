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
│       │   ├── explorer.rs             # check_explorer_ready, class-name walking
│       │   ├── picker.rs               # FolderPicker trait (IFileOpenDialog) (SP3)
│       │   ├── clipboard.rs            # Clipboard trait (CF_UNICODETEXT) (SP3)
│       │   ├── contextmenu.rs          # TrackPopupMenu wrapper
│       │   ├── config.rs               # Config + ConfigStore trait (SP3) — ~/.exbar.json
│       │   ├── theme.rs                # DPI scale, dark-mode detection
│       │   ├── error.rs                # ExbarError + ExbarResult (SP5)
│       │   └── log.rs                  # FileLogger (log crate) → %TEMP%\exbar.log (SP5)
│       ├── tests/                      # integration tests
│       └── wix/
│           └── main.wxs                # WiX v4 installer definition
├── scripts/
│   ├── build-msi.sh                    # invokes `wix build`
│   └── doc-check.sh                    # RUSTDOCFLAGS="-D warnings" cargo doc gate (SP7)
├── docs/
│   ├── adrs/                           # Architecture Decision Records (SP7)
│   └── superpowers/
│       ├── specs/                      # design docs
│       └── plans/                      # implementation plans
└── legacy_source/                      # QtTabBar C# source (reference only)
```

## Commands

All commands assume `cargo` is on PATH (`export PATH="$HOME/.cargo/bin:$PATH"` in git-bash).

- **Build:** `cargo build` (dev) or `cargo build --release`
- **Run unit tests:** `cargo test` (or `cargo test -p exbar-cli`) — 162 tests across 24 modules
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
  - Keeps toolbar visible when another window in `explorer.exe` becomes foreground (tooltips, tree view, Quick Access popups)
  - Hides toolbar when a window in a different process becomes foreground
- `WM_NCHITTEST` returns `HTCAPTION` for the grip area (dots on left/top edge) to make only the grip draggable; buttons get `HTCLIENT` for normal mouse handling
- Auto-sized in `WM_CREATE` based on `compute_layout`; position is clamped to the work area of the monitor containing the triggering Explorer window

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
- Dispatches via `DropAction` enum: `MoveCopyTo(target)` for folder buttons, `AddFolder` for the `+` button (appends dropped directory to `~/.exbar.json`)

### Pure controllers + Win32 adapters (SP2b, SP6)

Pointer and rename interactions are split into pure state-machine modules and thin Win32 adapter methods on `ToolbarState`. See `docs/adrs/ADR-0003-pure-controller-adapter-pattern.md`.

- `pointer.rs` — `PointerState`, `PointerEvent`, `PointerCommand`, `transition(state, event) → (state, Vec<command>)`. No Win32.
- `rename.rs` — `RenameState`, `RenameEvent`, `RenameAction`, `transition(...)`. No Win32.
- `toolbar.rs::execute_pointer_command` / `execute_rename_event` — the adapters. Translate `WM_*` messages to events, call `transition`, dispatch returned commands against Win32 + trait seams.

Future interaction subsystems (context-menu controller, drag-reorder commit, etc.) should follow the same split.

### Trait seams (SP3)

All cross-process Win32 surfaces are abstracted behind traits on `ToolbarState` for mock-driven testability. See `docs/adrs/ADR-0004-trait-seams-via-box-dyn.md`.

| Trait | Production impl | Used for |
|---|---|---|
| `shell_windows::ShellBrowser` | `Win32Shell` | Explorer navigation (`BrowseObject`, `open_in_new_tab`) |
| `picker::FolderPicker` | `Win32Picker` | `IFileOpenDialog` folder picker |
| `dragdrop::FileOperator` | `Win32FileOp` | `IFileOperation` move/copy |
| `clipboard::Clipboard` | `Win32Clipboard` | `OleClipboard` text writes |
| `config::ConfigStore` | `JsonFileStore` | `~/.exbar.json` load/save |

Tests inject `MockShellBrowser`, `MockFolderPicker`, `MockFileOp`, `MockClipboard`, `MockConfigStore` — each mock lives in its trait's `test_mocks` sub-module; shared builders live in `test_helpers.rs` (SP8).

### Error handling (SP5)

`crate::error::ExbarError` is the unified error type (`Win32`, `Io`, `Json`, `Config`). `ExbarResult<T> = Result<T, ExbarError>`. `warn_on_err!` macro logs-and-continues on `Result`s where panic-on-error isn't appropriate (most Win32 one-shot calls).

Logging goes through the `log` crate — `log::info!` / `warn!` / `error!` / `debug!`. `FileLogger` in `log.rs` is the sink; verbosity comes from `Config.log_level`.

### State ownership (SP4)

All runtime state lives on `ToolbarState`, owned by the wndproc via `GWLP_USERDATA`. See `docs/adrs/ADR-0005-toolbar-state-over-statics.md`. The one surviving static is `GLOBAL_TOOLBAR: Mutex<Option<isize>>` — a thread-safe bootstrap entry for `WINEVENT_OUTOFCONTEXT` callbacks (which have no `&self`) to find the toolbar HWND; from there, `unsafe { toolbar_state(hwnd) }` recovers the pointer. The safety of that helper relies on the single-threaded message-pump invariant.

### Context menus and inline rename

- The `+` button (first slot) has three interactions:
  - **Left-click** → `picker.rs` opens `IFileOpenDialog` with `FOS_PICKFOLDERS`, starting at `%SystemDrive%\`; selected folder appended via `Config::add_folder` + `save()`
  - **Right-click** → `Edit config` (ShellExecute opens `~/.exbar.json` in default handler) / `Reload config` (posts `WM_USER_RELOAD`)
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
- **Foreground hook must be installed before the first toolbar exists**: `install_foreground_hook()` is called from `run_hook()` (not from toolbar `WM_CREATE`) because the hook is what creates the toolbar on the first `CabinetWClass` foreground event. Chicken-and-egg if reversed.

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
3. For new state-machine logic, follow the SP2b/SP6 pattern: pure `transition()` module + thin `execute_*` adapter on `ToolbarState`. See ADR-0003.
4. For new cross-process Win32 surfaces, follow the SP3 pattern: trait + `Win32*` impl + mock. See ADR-0004.
5. All UI pixel values must pass through `theme::scale(px, dpi)` — no hardcoded pixels
6. All theme colors must branch on `theme::is_dark_mode()` — don't assume dark
7. Catch panics at FFI boundaries with `std::panic::catch_unwind` (see `wndproc::toolbar_wndproc_safe`)
8. Before pushing, run `cargo fmt && cargo clippy --all-targets && cargo test && ./scripts/doc-check.sh`. All four must pass.
9. Architectural decisions worth preserving as future-reader context go in `docs/adrs/ADR-NNNN-*.md` using the Nygard template.
