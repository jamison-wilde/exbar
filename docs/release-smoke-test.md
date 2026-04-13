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
- [ ] Click refresh button (⟳) → no error (config reloads silently if unchanged)
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
