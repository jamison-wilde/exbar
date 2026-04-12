# CLAUDE.md

Orientation for AI coding tools working in this repo.

## Project

**Tabplorer** — a Rust DLL + CLI that injects a floating folder-shortcut toolbar into Windows 11 File Explorer. See `README.md` for user-facing details.

## Layout

```
tabplorer/
├── Cargo.toml                          # workspace
├── crates/
│   ├── tabplorer-dll/                  # cdylib injected into explorer.exe
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs                  # DllMain, COM exports (DllGetClassObject, DllCanUnloadNow), TabplorerCBTHook export
│   │   │   ├── bho.rs                  # IObjectWithSite — registered but unused on Win11
│   │   │   ├── hook.rs                 # CBT hook callback, Explorer discovery, global state
│   │   │   ├── explorer.rs             # check_explorer_ready, class-name window walking
│   │   │   ├── toolbar.rs              # Owner-drawn floating popup window (the main UI)
│   │   │   ├── navigate.rs             # IShellBrowser::BrowseObject navigation
│   │   │   ├── dragdrop.rs             # IDropTarget — move/copy via IFileOperation
│   │   │   ├── config.rs               # JSON config (~/.tabplorer.json)
│   │   │   ├── theme.rs                # DPI scale, dark-mode detection, layout constants
│   │   │   └── log.rs                  # %TEMP%\tabplorer.log writer
│   │   └── tests/                      # integration tests using #[path = "../src/..."]
│   └── tabplorer-cli/                  # bin — install/uninstall/hook/status
│       └── src/main.rs
├── docs/
│   └── superpowers/
│       ├── specs/                      # design docs
│       └── plans/                      # implementation plans
└── legacy_source/                      # QtTabBar C# source (reference only, not built)
```

## Commands

All commands assume `cargo` is on PATH (`export PATH="$HOME/.cargo/bin:$PATH"` in git-bash).

- **Build:** `cargo build` (dev) or `cargo build --release`
- **Run unit tests:** `cargo test -p tabplorer-dll`
- **Build only the DLL:** `cargo build --release -p tabplorer-dll` (faster iteration)
- **Run the CLI:** `./target/release/tabplorer.exe <install|uninstall|status|hook>`

## Architecture

### Loading mechanism

Win11 Explorer does **not** load COM BHOs, so the BHO registration in `bho.rs` is a no-op in practice. The actual injection path is:

1. `tabplorer.exe hook` is registered in `HKCU\...\Run\Tabplorer` during install
2. The hook process calls `SetWindowsHookExW(WH_CBT, ..., 0)` — a global hook
3. Windows injects `tabplorer_dll.dll` into every process on the system
4. `DllMain` in `lib.rs` early-returns unless the current process is `explorer.exe` (this is critical for stability — see "Stability guard" below)
5. Inside `explorer.exe`, when a `CabinetWClass` window activates, the CBT hook calls `try_inject` which creates the toolbar (once, globally)

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

- Per-click, we look up the most-recently-activated Explorer via `ACTIVE_EXPLORER` (static Mutex in `toolbar.rs`)
- `hook::get_shell_browser_for(hwnd)` enumerates `IShellWindows` to get a fresh `IShellBrowser` for that window (never stored — always obtained fresh to avoid stale COM references)
- `navigate::navigate_to` calls `SHParseDisplayName` → `IShellBrowser::BrowseObject(pidl, SBSP_SAMEBROWSER)`

### Drag and drop

- `FolderDropTarget` in `dragdrop.rs` implements `IDropTarget`
- Registered on the whole toolbar window; at drop time it uses the cursor position (converted to client coords) to determine which button the drop is over
- Shell aliases (`shell:downloads`) are resolved to real paths via `SHParseDisplayName` + `SHGetPathFromIDListW` before comparing drive letters for the move/copy heuristic
- Executes the drop via `IFileOperation` with `FOF_ALLOWUNDO | FOF_NOCONFIRMMKDIR`

## Stability guard — critical

The global `SetWindowsHookEx` injects `tabplorer_dll.dll` into **every process on the system**. If the DLL does anything other than immediately return in non-Explorer processes, those processes can destabilize — save-as dialogs, anything using shell components, etc.

The guard is in `lib.rs::DllMain`:
- On `DLL_PROCESS_ATTACH`, check `is_explorer_process()` (compares `current_exe` filename). Return `TRUE` immediately if not explorer.exe. Leave `INITIALIZED = false`.
- `TabplorerCBTHook` also checks `INITIALIZED` and passes through immediately when false.

**Never remove or weaken this guard.** If you need the DLL to do something new, do it behind this check.

## Gotchas

- **DLL file locks**: once the hook is running, the DLL is loaded in many processes and can't be overwritten. When iterating, either `taskkill /f /im tabplorer.exe` + rename-old-DLL + copy-new, or use a different output name.
- **Killing explorer.exe during testing**: can destabilize apps that have Explorer DLL dependencies. Prefer leaving Explorer alone; use the hook restart flow.
- **`windows` crate v0.61 quirks**:
  - `BOOL` is `windows_core::BOOL`, NOT `windows::Win32::Foundation::BOOL`
  - `GetSysColor` / `SYS_COLOR_INDEX` are in `Win32::Graphics::Gdi`, not `Win32::UI::WindowsAndMessaging`
  - `IObjectWithSite` is in `Win32::System::Ole`
  - Many APIs take `Option<HWND>` / `Option<HMENU>` / `Option<HINSTANCE>`
  - `#[implement]` trait method signatures use `Ref<'_, T>` for optional COM params, not `Option<&T>`
  - `DeleteObject` expects `HGDIOBJ`; convert with `.into()` from `HBRUSH` / `HPEN` / `HFONT`
- **Win11 Explorer window hierarchy**: command bar is rendered by `Microsoft.UI.Content.DesktopChildSiteBridge` (WinUI 3 XAML). Cannot inject Win32 child windows into that hierarchy. We use a separate top-level popup instead. The old approach of overlaying the command bar area is abandoned — don't reintroduce it.
- **`WINEVENT_SKIPOWNPROCESS`**: do NOT set this flag on the foreground-window WinEvent hook. Most events we care about (Explorer activations) happen in explorer.exe itself.

## Logging

All DLL logs go to `%TEMP%\tabplorer.log` with format `HH:MM:SS.mmm [LEVEL] pid=N message`. Use this as the first diagnostic tool when something isn't working as expected.

```bash
type C:\Users\slain\AppData\Local\Temp\tabplorer.log
```

## Build & deploy loop (live-iteration)

When iterating on the DLL while the hook is running:

```bash
# 1. Build
cargo build --release -p tabplorer-dll

# 2. Stop hook so we can replace the DLL
taskkill /f /im tabplorer.exe

# 3. Rename + copy (overwrite may fail due to process-wide DLL locks)
mv %LOCALAPPDATA%/tabplorer/tabplorer_dll.dll %LOCALAPPDATA%/tabplorer/tabplorer_dll.old
cp target/release/tabplorer_dll.dll %LOCALAPPDATA%/tabplorer/tabplorer_dll.dll

# 4. Clear log for a clean diagnostic run
rm -f %TEMP%/tabplorer.log

# 5. Restart hook
./target/release/tabplorer.exe hook
```

## Adding a new feature

1. Decide whether it lives in the DLL (runtime behavior) or the CLI (install/management)
2. For DLL changes, write/extend integration tests in `crates/tabplorer-dll/tests/` using the `#[path = "../src/..."]` pattern to test pure logic without Windows APIs
3. Respect the stability guard — no work in `DllMain` / the hook callback before the Explorer check
4. All UI pixel values must pass through `theme::scale(px, dpi)` — no hardcoded pixels
5. All theme colors must branch on `theme::is_dark_mode()` — don't assume dark
6. Catch panics at FFI boundaries with `std::panic::catch_unwind` (see `toolbar_wndproc_safe`)
