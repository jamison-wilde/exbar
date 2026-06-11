# Exbar

A configurable folder toolbar for Windows 11 File Explorer and Open/Save As dialogs. 


![Main Explorer Demo](docs/images/main_demo.gif)

I was a big fan of [GPSoft's Directory Opus](https://www.gpsoft.com.au/) in the early 2000's and then mostly have just used QTTabBar since for tabs and folder bars, but it is now bloated, unsupported ('[original](http://qttabbar.wikidot.com/)' version), and currently broken (including the newer [indiff](https://github.com/indiff/qttabbar) version, or needing deep workarounds) on Windows 11. I previously used it mostly for tabs and folder bars because 'Quick Access' is a terrible UX. This is my Rust-built version now that Windows 11's File Explorer has tab support.  

![Save/Open Dialog Demo](docs/images/dialog_demo.gif)

## Features
* Works with tabs, changing the active tab when clicking a folder in exbar. Ctrl-click to open in new tab.
* Works in normal Save As / Open file dialogs too — click a folder to retarget the dialog instead of Explorer. Drag a file out of the dialog onto a toolbar folder to move or copy it there.
* **Spring-open submenus**: long-press OR long-hover a folder to browse its subdirectories (up to 7 levels deep). Drop files anywhere in the submenu tree. `..` rows for quick up-traversal. Mouse wheel + hover-band autoscroll for long lists.
* **Recent Folders** (opt-in): a 🕘 button that tracks folders where you spend time or take action. Privacy-preserving — disable anytime and the data file is deleted. Exclusion-path config keeps sensitive folders out.
* **Network folder support**: mapped drives (`Z:\…`) and UNC paths (`\\server\share\…`) work as toolbar folders. Unreachable shares grey out instead of hanging the UI; right-click → `Retry connection` to re-probe.
* Drag-n-drop support for moving and copying files with native Windows semantics around ctrl/shift drop.
* Drag-n-drop support for adding folders to Exbar.
* Drag re-sort the order of the folders in Exbar.
* Right click exbar folder for various options, like copy path or rename the shortcut.
* Right click '+' for editing config and toggling Recent Folders.
* Remembers relative position and adjusts after drag, resize and maximize events. Vertical layout supported for those with odd tastes.
* Supports Windows theme (currently requires a restart of the exbar.ex if you change mid session)

Enable **Recent** by right clicking the '+'.

![recent_folders_demo](docs/images/recent_folders_demo.png)

Large subfolders supported with scrolling

![Large Subfolder Image](docs/images/large_subfolder_menu.png)

Let's go vertical!

![Vertical Orientation Image](docs/images/vertical_orientation.png)

## Install

Download and install `exbar-1.3.0-x64.msi` from the [latest release](https://github.com/jamison-wilde/exbar/releases/latest).

Windows SmartScreen will warn you that the publisher is unrecognized (the installer is not yet signed). Click **More info** → **Run anyway**.

The installer is per-user (no admin required) and:
- Installs to `%LOCALAPPDATA%\Exbar\`
- Adds **Exbar** to your Start menu so you can re-launch it any time
- Configures the toolbar to auto-start when you sign in

## Use

- **Click a folder button** — the active Explorer window's active tab navigates to that folder
- **Long-press or long-hover** a folder button — opens a subfolder submenu. Click / Ctrl-click items to navigate; drop files onto them to move/copy; mouse-wheel or hover the top/bottom arrows to scroll long lists. `..` rows navigate up. Esc or click outside dismisses.
- **Drag a file/folder onto an exbar folder button** — moves (same drive) or copies (different drive) just like it would with a Quick Access folder
  - Hold `Ctrl` to force copy, or `Shift` to force move
- **Drag the grip** (dots on the left edge when horizontal, top edge when vertical) — move the toolbar
- **Right click on the '+'** — to edit config, reload, or enable/disable Recent Folders

Position is remembered across sign-outs. The toolbar auto-hides when you switch to non-Explorer apps.

## Configure

Edit `~\.exbar\config.json` (in your user home folder):

```json
{
  "folders": [
    {"name": "Downloads", "path": "shell:downloads"},
    {"name": "Documents", "path": "shell:personal"},
    {"name": "Projects",  "path": "C:\\Users\\you\\projects"},
    {"name": "Work",      "path": "D:\\work"}
  ],
  "layout": "horizontal", // or "vertical"
  "background_opacity": 0.8,
  "log_level": "info", // exbar.log in %TEMP% usually in AppData\Local\Temp
  "repositionDelayMs": 250, // dial in the time the exbar reappears after a max/unmax
  "enableFileDialogs": true,
  "showIcons": true, // false hides the 📁/🕘 emoji on toolbar buttons (toggle via + right-click)
  "foregroundWatchdogMs": 2000, // periodic safety-net that hides a toolbar left over a foreign app; 0 disables
  "watchdogReshow": false,      // also let the watchdog re-show the toolbar when active target is foreground
  "submenu": {
    "springOpenDelayMs": 500,   // long-press threshold to open a subfolder submenu
    "longHoverOpenMs": 1200,    // cursor-rest threshold to open without pressing
    "hoverBufferPx": 30,        // forgiveness zone around each popup
    "nonChainItemOpacity": 0.5  // (reserved for future use)
  },
  "recent": {
    "enabled": false,            // toggle via right-click on the + button
    "maxCount": 5,               // 1..=20
    "includePinned": false,      // hide paths that are also in folders[]
    "dwellSecondsToTrack": 10,   // 1..=300
    "excludedPaths": ["C:\\private"]
  }
}
```

**Fields:**
- `folders[].name` — button label (required)
- `folders[].path` — absolute path or `shell:` alias like `shell:downloads`, `shell:desktop`, `shell:personal` (required)
- `folders[].kind` — `"Folder"` (default, omit) or `"Recent"` (the 🕘 pseudo-button, managed by the Enable Recent Folders toggle — you shouldn't edit this by hand)
- `layout` — `"horizontal"` (default) or `"vertical"`
- `background_opacity` — 0.0 (transparent) to 1.0 (opaque). Default: 0.8
- `enableFileDialogs` — `true` (default) to light up the toolbar over Save As / Open dialogs. Set to `false` for Explorer-only behavior.
- `showIcons` — `true` (default) shows the `📁`/`🕘` emoji prefix on folder buttons; `false` drops it for a narrower toolbar. Toggle via the `+` button's right-click menu (`Show icons` / `Hide icons`).
- `foregroundWatchdogMs` — interval (ms) for the foreground watchdog that hides a toolbar left visible over a foreign app after a spurious Explorer foreground event. Default 2000. Clamped to 500..=60000; `0` disables. Applied at toolbar creation — changing it takes effect on next hook restart.
- `watchdogReshow` — when `true`, the watchdog also re-shows the toolbar if it was hidden while the active Explorer/dialog target is foreground. Default `false` (hide-only).
- `submenu.*` — spring-open submenu tuning. Omit the block for defaults.
- `recent.*` — Recent Folders tracking (opt-in). Enable via the `+` right-click menu; `recent.excludedPaths` is a prefix match on folder paths (any descendant is also excluded). `recents.json` lives under `~/.exbar/` and is deleted when Recent is disabled.

If the file doesn't exist, the installer created a stub for you with Downloads, Documents, and Desktop. Click the refresh button (⟳) on the toolbar after editing.

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

## License

MIT — see [LICENSE](LICENSE).
