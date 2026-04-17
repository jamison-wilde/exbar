# Exbar

A floating, configurable folder-shortcut toolbar for Windows 11 File Explorer. 

I was a big fan of [GPSoft's Directory Opus](https://www.gpsoft.com.au/) in the early 2000's and then mostly have just used QTTabBar since for tabs and folder bars, but it is now bloated, unsupported ('[original](http://qttabbar.wikidot.com/)' version), and currently broken (including the newer [indiff](https://github.com/indiff/qttabbar) version, or needing deep workarounds) on Windows 11. I previously used it mostly for tabs and folder bars because 'Quick Access' is a terrible UX. So this is my Rust-built version now that Windows 11's File Explorer has tab support.  

## Features
* Works with tabs, changing the active tab when clicking a folder in exbar. Ctrl-click to open in new tab.
* Works in Save As / Open file dialogs too — click a folder to retarget the dialog instead of Explorer. Drag a file out of the dialog onto a toolbar folder to move or copy it there.
* Drag-n-drop support for moving and copying files with native Windows semantics around ctrl/shift drop.
* Drag-n-drop support for adding folders to Exbar.
* Drag re-sort the order of the folders in Exbar.
* Right click exbar folder for various options, like
* Right click '+' for editing config.
* Remembers relative position and adjusts after drag, resize and maximize events.

## Install

1. Download `exbar-1.0.0-x64.msi` from the [latest release](https://github.com/jamison-wilde/exbar/releases/latest).
2. Double-click the MSI.
3. Windows SmartScreen will warn you that the publisher is unrecognized (the installer is not yet signed). Click **More info** → **Run anyway**.
4. Step through the installer dialogs — defaults are correct.
5. When the installer finishes, open any Explorer window. The toolbar appears within a second.

The installer is per-user (no admin required) and:
- Installs to `%LOCALAPPDATA%\Exbar\`
- Adds **Exbar** to your Start menu so you can re-launch it any time
- Configures the toolbar to auto-start when you sign in

## Configure

Edit `~\.exbar.json` (in your user home folder):

```json
{
  "folders": [
    {"name": "Downloads", "path": "shell:downloads"},
    {"name": "Documents", "path": "shell:personal"},
    {"name": "Projects",  "path": "C:\\Users\\you\\projects"},
    {"name": "Work",      "path": "D:\\work"}
  ],
  "layout": "horizontal",
  "background_opacity": 0.8
}
```

**Fields:**
- `folders[].name` — button label (required)
- `folders[].path` — absolute path or `shell:` alias like `shell:downloads`, `shell:desktop`, `shell:personal` (required)
- `layout` — `"horizontal"` (default) or `"vertical"`
- `background_opacity` — 0.0 (transparent) to 1.0 (opaque). Default: 0.8
- `enableFileDialogs` — `true` (default) to light up the toolbar over Save As / Open dialogs. Set to `false` for Explorer-only behavior.

If the file doesn't exist, the installer created a stub for you with Downloads, Documents, and Desktop. Click the refresh button (⟳) on the toolbar after editing.

## Use

- **Click a folder button** — the active Explorer window navigates to that folder
- **Drag a file onto a folder button** — moves (same drive) or copies (different drive)
  - Hold `Ctrl` to force copy
  - Hold `Shift` to force move
- **Drag the grip** (dots on the left edge when horizontal, top edge when vertical) — move the toolbar
- **Refresh button (⟳)** — reload the config

Position is remembered across sign-outs. The toolbar auto-hides when you switch to non-Explorer apps.

## Requirements

- Windows 11 (x86_64)

## Troubleshooting

## For developers

<details>
<summary>Build from source</summary>

```bash
# Build the binaries
cargo build --release

# Build the MSI installer
./scripts/build-msi.sh
```

Prerequisites:
- [Rust toolchain](https://rustup.rs/) — requires the `x86_64-pc-windows-msvc` target (installed by default on Windows)
- [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/) with the *Desktop development with C++* workload
- [WiX Toolset v7](https://wixtoolset.org/) — install via `dotnet tool install --global wix`, then `wix eula accept wix7 && wix extension add --global WixToolset.Util.wixext`

See `CLAUDE.md` for architecture notes and the live-iteration build loop.

</details>

## Status

Current release: v1.0.0. Known caveats:
- Installer is unsigned (SmartScreen warning)
- Icon support not yet implemented (folder emoji + label only)
- Only tested on Win11, x86_64, single-user installs

## License

MIT — see [LICENSE](LICENSE).
