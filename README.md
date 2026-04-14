# Exbar

A floating, configurable folder-shortcut toolbar for Windows 11 File Explorer. Pin your most-used folders to a draggable bar that hovers above Explorer. Click to navigate. Drag files onto a folder button to move or copy them with native Windows semantics (move on same drive, copy across drives; hold Ctrl or Shift to override).

## Install

1. Download `exbar-0.1.0-x64.msi` from the [latest release](https://github.com/YOUR_GITHUB_USERNAME/exbar/releases/latest).
   <!-- TODO: replace YOUR_GITHUB_USERNAME with the actual GitHub username when the repo is published -->
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

If the file doesn't exist, the installer created a stub for you with Downloads, Documents, and Desktop. Click the refresh button (⟳) on the toolbar after editing.

## Use

- **Click a folder button** — the active Explorer window navigates to that folder
- **Drag a file onto a folder button** — moves (same drive) or copies (different drive)
  - Hold `Ctrl` to force copy
  - Hold `Shift` to force move
- **Drag the grip** (dots on the left edge when horizontal, top edge when vertical) — move the toolbar
- **Refresh button (⟳)** — reload the config

Position is remembered across sign-outs. The toolbar auto-hides when you switch to non-Explorer apps.

## Restart the toolbar

If the toolbar disappears (e.g. you killed `exbar.exe` via Task Manager or it crashed), launch it from the Start menu:

**Start menu** → type **Exbar** → **Enter**

Or sign out and back in.

## Uninstall

**Settings** → **Apps** → **Installed apps** → search **Exbar** → **Uninstall**

Your config file (`~\.exbar.json`) is preserved.

## Requirements

- Windows 11 (x86_64)
- That's it.

## Troubleshooting

**The toolbar isn't appearing.**
Check the log file at `%TEMP%\exbar.log`. If there are no recent entries about `ExbarCBTHook` or `try_inject`, the hook process isn't running. Launch it from Start menu → Exbar.

**An app crashed shortly after I installed.**
The DLL is designed to no-op in non-Explorer processes, but report it on GitHub Issues with the log file (`%TEMP%\exbar.log`) attached.

**SmartScreen warning on install.**
Expected — the MSI is not yet code-signed. Click "More info" → "Run anyway".

**Toolbar covers other apps.**
It should hide when non-Explorer apps are in the foreground. If it isn't hiding, check `%TEMP%\exbar.log` for WinEvent activity and file an issue.

## For developers

<details>
<summary>Build from source</summary>

```bash
# Build the binaries
cargo build --release

# Build the MSI installer
./scripts/build-msi.sh
```

Output: `target/wix/exbar-0.1.0-x64.msi`

Prerequisites:
- [Rust toolchain](https://rustup.rs/) — requires the `x86_64-pc-windows-msvc` target (installed by default on Windows)
- [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/) with the *Desktop development with C++* workload
- [WiX Toolset v7](https://wixtoolset.org/) — install via `dotnet tool install --global wix`, then `wix eula accept wix7 && wix extension add --global WixToolset.Util.wixext`

See `CLAUDE.md` for architecture notes and the live-iteration build loop.

</details>

## Status

Early release (v0.1.0). Known caveats:
- Installer is unsigned (SmartScreen warning)
- Icon support not yet implemented (folder emoji + label only)
- Only tested on Win11, x86_64, single-user installs

## License

See repository for details.
