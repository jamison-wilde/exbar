# Release Smoke Test Checklist

Every release MUST pass this checklist before the tag is pushed and the MSI is published to GitHub Releases.

## Environment

- A fresh Windows 11 (x86_64) user profile. A clean VM is ideal; a new local user account also works.
- **Never run this on the developer profile** — old install state pollutes the test.
- Have a test file on the C: drive AND a test file on a different drive (e.g., D:) for drag-drop move/copy checks.

## Pre-flight

- [ ] Copy `target/wix/exbar-<version>-x64.msi` to the test environment
- [ ] Confirm no previous Exbar install exists: Settings → Apps → search "Exbar" should show nothing
- [ ] Confirm no `exbar.exe` process is running: `tasklist | findstr exbar`

## Install

- [ ] Double-click the MSI
- [ ] Windows SmartScreen warning appears. Click **More info** → **Run anyway**
- [ ] Step through the installer dialogs. Complete install
- [ ] **No console window appears** during or after install
- [ ] Verify `%LOCALAPPDATA%\Exbar\exbar.exe` and `%LOCALAPPDATA%\Exbar\exbar_dll.dll` exist
- [ ] `reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v Exbar` shows the Run key value
- [ ] `reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Exbar"` shows DisplayName, DisplayVersion, etc.
- [ ] `tasklist | findstr exbar` shows `exbar.exe` running
- [ ] Start menu has an "Exbar" entry (search for "Exbar")

## Toolbar functionality

- [ ] Open Explorer (Win+E). Toolbar appears within ~1 second
- [ ] Click a configured folder button → Explorer navigates to that folder
- [ ] Toolbar shows `+` button (not the old ↻ glyph)
- [ ] Drag a file on `C:` to a folder button whose target is also on `C:` → file moves
- [ ] Drag a file on `D:` to a folder button whose target is on `C:` → file copies
- [ ] Drag a file while holding `Ctrl` → forces copy
- [ ] Drag a file while holding `Shift` → forces move
- [ ] Click a non-Explorer app (e.g., Notepad) → toolbar hides
- [ ] Click Explorer again → toolbar reappears, on top
- [ ] Minimize Explorer → toolbar hides
- [ ] Restore Explorer → toolbar reappears
- [ ] Drag toolbar by grip to a new position
- [ ] Verify toolbar position persists across log-out / log-in

## Stability

- [ ] `taskkill /f /im exbar.exe` — Explorer does NOT crash; toolbar stays visible
- [ ] Open a save dialog in another app (e.g., Notepad → File → Save As) — the dialog opens normally without crash
- [ ] Click Start menu → "Exbar" → Enter — toolbar still visible (re-launching doesn't break things)

## v0.2.0 UI additions

- [ ] Click `+` → folder picker opens at `C:\`; pick any folder → button appears at end
- [ ] Click `+` → Cancel → no-op, nothing added
- [ ] Drag a folder from Explorer onto `+` → button appears at end with the folder's basename
- [ ] Drag a file (not folder) onto `+` → cursor shows "no", nothing added
- [ ] Right-click `+` → menu shows **Edit config** and **Reload config**
- [ ] **Edit config** opens `~/.exbar.json` in Notepad (or default `.json` handler)
- [ ] Edit the JSON manually, save, then right-click `+` → **Reload config** — toolbar updates
- [ ] Right-click a folder button → menu shows **Open / Open in new tab / Copy path / --- / Rename / Remove**
- [ ] **Open** navigates current Explorer to the folder (same as left-click)
- [ ] **Copy path** places the literal path on the clipboard (paste into Notepad to verify)
- [ ] **Remove** deletes the entry immediately, no confirmation
- [ ] **Rename** shows an inline edit field over the button; Enter commits and updates config
- [ ] Rename: press Esc → cancels, name unchanged
- [ ] Rename: click elsewhere → commits (focus loss)
- [ ] Rename: clear the name (empty string) + Enter → old name preserved
- [ ] Ctrl+left-click a folder button → opens in a new Explorer tab in the current window
- [ ] Set `newTabTimeoutMsZeroDisables: 0` in config + Reload → ctrl-click opens a new Explorer window instead
- [ ] Test a path with spaces (e.g., `C:\Program Files`) — both tab and fallback-window open the correct folder

## Uninstall

- [ ] Settings → Apps → Installed apps → search **Exbar** → click **Uninstall**
- [ ] After uninstall: toolbar gone, no `exbar.exe` running, `%LOCALAPPDATA%\Exbar\` removed
- [ ] `reg query` for the Run key shows "not found"
- [ ] `reg query` for the Uninstall key shows "not found"
- [ ] Start menu no longer has an Exbar entry
- [ ] `~\.exbar.json` STILL exists (user data preserved)

## Failure handling

If any step fails:
1. Note the exact failing step and observed behavior
2. Capture `%TEMP%\exbar.log` if relevant
3. Fix the underlying issue
4. Re-run from "Pre-flight" on a fresh environment

Do not tag a release with any failing items.

## A2 (out-of-process) additions

Exbar v1.0.0 moved from DLL injection to an out-of-process architecture. Verify:

- [ ] Kill `exbar.exe` via Task Manager → toolbar disappears immediately. Explorer does NOT crash, flicker, or show any visual disruption.
- [ ] Restart `exbar.exe hook` → toolbar reappears above the active Explorer within ~1 second.
- [ ] Close the Explorer window that was foreground when exbar started → toolbar stays functional over other Explorer windows. Does not orphan or go silent.
- [ ] Open three Explorer windows, close in various orders → toolbar follows foreground correctly regardless of close order.
- [ ] Uninstall via Settings → Apps completes within a few seconds. Explorer does NOT close or restart. No "file in use" dialog.
- [ ] Upgrade install (v0.2.0 MSI → v1.0.0 MSI): `%LOCALAPPDATA%\Exbar\exbar.exe` is replaced, `exbar_dll.dll` is removed or scheduled for deletion. Explorer is not touched.
- [ ] Dev loop: `cargo build --release -p exbar-cli && cp target/release/exbar.exe %LOCALAPPDATA%\Exbar\` works with no file-lock errors (there's no longer a DLL pinned in every process).
- [ ] Drag file from Explorer → drop on a folder button → move/copy works (cross-process OLE drag-drop).
- [ ] Drag folder from Explorer → drop on `+` button → appended to config.
- [ ] All v0.2.0 smoke-test items still pass (click-to-navigate, ctrl-click new tab, right-click menus, inline rename, drag-reorder, folder picker).
