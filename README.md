# Tabplorer

A floating, configurable folder-shortcut toolbar for Windows 11 File Explorer. Written in Rust.

Think: a customizable tab bar of your most-used folders that hovers above Explorer. Click to navigate. Drag files onto a folder button to move or copy them with native Windows semantics (move on same drive, copy across drives, hold Ctrl/Shift to override).

## Features

- **Floating toolbar** — a draggable popup window that appears above Explorer and hides when you switch to other apps
- **Configurable folders** — JSON config at `~/.tabplorer.json`; add absolute paths or `shell:` aliases (`shell:downloads`, `shell:desktop`, `shell:personal`, etc.)
- **Horizontal or vertical layout** — set `"layout": "vertical"` in the config
- **Adjustable transparency** — `background_opacity` in config (default 0.8)
- **Click to navigate** — routes the most-recently-active Explorer window to the selected folder
- **Drag-and-drop** — drop files on a folder button to move (same drive) or copy (different drive); Ctrl forces copy, Shift forces move
- **Native look** — matches system dark/light theme; DPI-aware
- **Remembers position** — saves window position to `~/.tabplorer-pos.json`
- **Auto-show/hide** — appears only when Explorer is in the foreground
- **Refresh button** — re-reads the JSON config on demand

## Prerequisites

- **Windows 11** (x86_64). Win10 is untested and likely broken because the toolbar injection depends on Win11 Explorer's window hierarchy (`Microsoft.UI.Content.DesktopChildSiteBridge`).
- **Rust toolchain** — install via [rustup](https://rustup.rs/). Requires the `x86_64-pc-windows-msvc` target (installed by default on Windows).
- **Visual Studio Build Tools** with the *Desktop development with C++* workload (for the MSVC linker and Windows SDK). Download from [Microsoft](https://visualstudio.microsoft.com/downloads/).

## Build

```bash
cargo build --release
```

Produces two artifacts in `target/release/`:
- `tabplorer_dll.dll` — the injected DLL
- `tabplorer.exe` — the CLI installer and hook process host

## Install

From the repo root after building:

```bash
./target/release/tabplorer.exe install
```

This will:
1. Copy the DLL to `%LOCALAPPDATA%\tabplorer\tabplorer_dll.dll`
2. Register a Run key at `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Tabplorer` so the hook process auto-starts on login
3. Create a stub config at `~/.tabplorer.json` if none exists
4. Restart Explorer and spawn the hook process

The COM BHO registration is a leftover from an earlier approach and currently a no-op on Win11 (Win11 Explorer doesn't load BHOs). Actual injection uses a global `SetWindowsHookEx` CBT hook.

## Configure

Edit `~/.tabplorer.json`:

```json
{
  "folders": [
    {"name": "Downloads", "path": "shell:downloads"},
    {"name": "Documents", "path": "shell:personal"},
    {"name": "Projects",  "path": "C:\\Users\\you\\projects"},
    {"name": "Work",      "path": "D:\\work", "icon": "C:\\icons\\work.ico"}
  ],
  "layout": "horizontal",
  "background_opacity": 0.8
}
```

**Fields:**
- `folders[].name` (required) — button label
- `folders[].path` (required) — absolute path or `shell:` alias
- `folders[].icon` (optional) — currently unused; reserved for future use
- `layout` — `"horizontal"` (default) or `"vertical"`
- `background_opacity` — float 0.0–1.0, default 0.8

Click the refresh button (⟳) on the toolbar to re-read the config after editing.

## Usage

Once installed, the toolbar appears automatically when any Explorer window has focus.

- **Click a folder button** — navigate the active Explorer window to that folder
- **Click refresh (⟳)** — reload the config
- **Drag the grip** (dots on the left edge horizontal, or top edge vertical) — move the toolbar
- **Drag a file onto a folder button** — move (same drive) or copy (different drive)
  - Hold `Ctrl` to force copy
  - Hold `Shift` to force move

## Uninstall

```bash
./target/release/tabplorer.exe uninstall
```

Leaves your config file (`~/.tabplorer.json`) in place. Pass `--clean` to also delete `%LOCALAPPDATA%\tabplorer\`.

## Troubleshooting

**The toolbar isn't appearing.**
Check the log file at `%TEMP%\tabplorer.log`. If there are no lines mentioning `TabplorerCBTHook`, the hook process isn't running — try starting it manually: `./target/release/tabplorer.exe hook`.

**Updating the DLL after rebuild.**
Once the hook is running, Windows has loaded the DLL into many processes and you can't overwrite it directly. Kill the hook (`taskkill /f /im tabplorer.exe`), rename the old DLL, then copy the new one:
```bash
mv %LOCALAPPDATA%/tabplorer/tabplorer_dll.dll %LOCALAPPDATA%/tabplorer/tabplorer_dll.old
cp target/release/tabplorer_dll.dll %LOCALAPPDATA%/tabplorer/tabplorer_dll.dll
```

**Toolbar covers other apps.**
It should hide when non-Explorer apps are foreground. If it's not hiding, check `%TEMP%\tabplorer.log` for WinEvent activity and file an issue.

**Stability issues in other apps.**
The DLL is designed to no-op in non-Explorer processes. If another app crashes after install, check the log and file an issue.

## Status

Early prototype. Known caveats:
- BHO registration path is registered but unused (Win11 Explorer doesn't invoke BHOs); the CBT hook is the actual loading mechanism
- Double-click navigation in Explorer can briefly hide the toolbar in some cases
- Icon support is not yet implemented (only the text label is shown, with a generic folder emoji)
- Only tested on Win11, x86_64, single-user installs under HKCU

## License

See repository for details.
