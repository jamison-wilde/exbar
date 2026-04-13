# CLAUDE.md

Orientation for AI coding tools working in this repo.

## Project

**Exbar** — a Rust DLL + CLI that injects a floating folder-shortcut toolbar into Windows 11 File Explorer. See `README.md` for user-facing details.

## Layout

```
exbar/
├── Cargo.toml                          # workspace
├── crates/
│   ├── exbar-dll/                      # cdylib injected into explorer.exe
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs                  # DllMain, ExbarCBTHook export
│   │   │   ├── hook.rs                 # CBT hook callback, Explorer discovery, global state
│   │   │   ├── explorer.rs             # check_explorer_ready, class-name window walking
│   │   │   ├── toolbar.rs              # Owner-drawn floating popup window (the main UI)
│   │   │   ├── navigate.rs             # IShellBrowser::BrowseObject navigation
│   │   │   ├── dragdrop.rs             # IDropTarget — move/copy via IFileOperation
│   │   │   ├── config.rs               # JSON config (~/.exbar.json)
│   │   │   ├── theme.rs                # DPI scale, dark-mode detection, layout constants
│   │   │   └── log.rs                  # %TEMP%\exbar.log writer
│   │   ├── build.rs                    # winres version metadata
│   │   └── tests/                      # integration tests using #[path = "../src/..."]
│   └── exbar-cli/                      # bin — install/uninstall/hook/status
│       ├── build.rs                    # winres version metadata
│       ├── src/main.rs
│       └── wix/
│           └── main.wxs               # WiX v4 installer definition
├── scripts/
│   └── build-msi.sh                   # invokes `wix build` to produce the MSI
├── docs/
│   └── superpowers/
│       ├── specs/                      # design docs
│       └── plans/                      # implementation plans
└── legacy_source/                      # QtTabBar C# source (reference only, not built)
```

## Commands

All commands assume `cargo` is on PATH (`export PATH="$HOME/.cargo/bin:$PATH"` in git-bash).

- **Build:** `cargo build` (dev) or `cargo build --release`
- **Run unit tests:** `cargo test -p exbar-dll`
- **Build only the DLL:** `cargo build --release -p exbar-dll` (faster iteration)
- **Run the CLI:** `./target/release/exbar.exe <install|uninstall|status|hook>`
- **Build MSI:** `./scripts/build-msi.sh` (requires WiX v7 installed — see "MSI installer" section)
- **CLI subcommands**: `hook` (production, started by Run key), `status` (diagnostics). `install` and `uninstall` are dev-only fallbacks; end users use the MSI.

## Architecture

### Loading mechanism

1. The MSI installer writes `HKCU\...\Run\Exbar = "exbar.exe hook"` and launches the hook as a post-install action
2. `exbar.exe hook` calls `SetWindowsHookExW(WH_CBT, ..., 0)` — a global hook
3. Windows injects `exbar_dll.dll` into every process on the system as Explorer activations fire
4. `DllMain` in `lib.rs` early-returns unless the current process is `explorer.exe` (stability guard — see "Stability guard" below)
5. In `explorer.exe`, `DllMain` also pins the DLL via `GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_PIN, ...)` so the DLL stays loaded even after the hook process exits. Without this, killing `exbar.exe` would unload the DLL while its wndproc and WinEvent callback are still referenced — causing `explorer.exe` to crash.
6. When a `CabinetWClass` window activates, the CBT hook calls `try_inject` which creates the toolbar (once, globally)

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

The global `SetWindowsHookEx` injects `exbar_dll.dll` into **every process on the system**. If the DLL does anything other than immediately return in non-Explorer processes, those processes can destabilize — save-as dialogs, anything using shell components, etc.

The guard is in `lib.rs::DllMain`:
- On `DLL_PROCESS_ATTACH`, check `is_explorer_process()` (compares `current_exe` filename). Return `TRUE` immediately if not explorer.exe. Leave `INITIALIZED = false`.
- `ExbarCBTHook` also checks `INITIALIZED` and passes through immediately when false.

**Never remove or weaken this guard.** If you need the DLL to do something new, do it behind this check.

## Gotchas

- **DLL file locks**: once the hook is running, the DLL is loaded in many processes and can't be overwritten. When iterating, either `taskkill /f /im exbar.exe` + rename-old-DLL + copy-new, or use a different output name.
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
- **Hook process must not show a console**: `exbar.exe hook` calls `FreeConsole()` at the start to detach from any inherited console. The MSI's post-install custom action otherwise opens a visible terminal window. Don't add `println!` calls in `run_hook()` after `FreeConsole` — they'll silently no-op.

## Logging

All DLL logs go to `%TEMP%\exbar.log` with format `HH:MM:SS.mmm [LEVEL] pid=N message`. Use this as the first diagnostic tool when something isn't working as expected.

```bash
type C:\Users\slain\AppData\Local\Temp\exbar.log
```

## Build & deploy loop (live-iteration)

When iterating on the DLL while the hook is running:

```bash
# 1. Build
cargo build --release -p exbar-dll

# 2. Stop hook so we can replace the DLL
taskkill /f /im exbar.exe

# 3. Rename + copy (overwrite may fail due to process-wide DLL locks)
mv %LOCALAPPDATA%/Exbar/exbar_dll.dll %LOCALAPPDATA%/Exbar/exbar_dll.old
cp target/release/exbar_dll.dll %LOCALAPPDATA%/Exbar/exbar_dll.dll

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

1. Decide whether it lives in the DLL (runtime behavior) or the CLI (install/management)
2. For DLL changes, write/extend integration tests in `crates/exbar-dll/tests/` using the `#[path = "../src/..."]` pattern to test pure logic without Windows APIs
3. Respect the stability guard — no work in `DllMain` / the hook callback before the Explorer check
4. All UI pixel values must pass through `theme::scale(px, dpi)` — no hardcoded pixels
5. All theme colors must branch on `theme::is_dark_mode()` — don't assume dark
6. Catch panics at FFI boundaries with `std::panic::catch_unwind` (see `toolbar_wndproc_safe`)
