# Windows laptop test

The first run of Cooldown on real Windows. Go through the steps in order and note what happens at each one. When something looks wrong, a screenshot plus the log file is enough to debug it.

The normal, user-facing instructions are in [install-windows.md](install-windows.md). This page is the tester's checklist for the same app.

**Log file:** `%LOCALAPPDATA%\app.cooldown.desktop\logs\cooldown.log`. Paste that path into the File Explorer address bar. Each start adds a line like `Cooldown 0.1.0 starting, data in ...`.

**App data:** `%LOCALAPPDATA%\app.cooldown.desktop\`, the same folder as the log (the database, `settings.json`, `trust.json`). Nothing of Cooldown's should appear in `%APPDATA%\app.cooldown.desktop`; if it does, that is a bug.

## 1. Install
- [ ] Download the installer from the **[Releases page](../../../releases)**: `Cooldown_<version>_x64-setup.exe` and `SHA256SUMS.txt`.
  - *Fallback for a build that is not released yet:* open **Actions** > the latest green **Windows** run, download the `cooldown-windows-installer` artifact, unzip it and use the setup file inside. (This artifact has no checksum file, so skip the next item.)
- [ ] Check the file in PowerShell. The two hashes match (capital or small letters do not matter):
  ```powershell
  cd $HOME\Downloads
  Get-FileHash .\Cooldown_<version>_x64-setup.exe -Algorithm SHA256
  Select-String -Path .\SHA256SUMS.txt -Pattern "x64-setup"
  ```
- [ ] Run the installer. SmartScreen warns (the build is unsigned): **More info**, then **Run anyway**. Setup finishes without asking for an administrator password.
- [ ] Cooldown opens, and a bell icon shows in the tray (it may be hidden under the `^` arrow).
- [ ] The log file exists and its first lines say `Cooldown <version> starting`. The version matches the installer's file name.

## 2. Window and tray
- [ ] Right-click the tray icon. The menu shows **Open Cooldown**, **Check limits now**, **Start at login** (ticked) and **Quit Cooldown**.
- [ ] Closing the window keeps the app in the tray. Clicking the tray icon, or **Open Cooldown**, brings the window back.
- [ ] Starting Cooldown a second time from the Start menu brings the open window forward instead of starting a second copy.
- [ ] **Quit Cooldown** in the tray menu really ends it (it is gone from Task Manager).

## 3. Claude Code
- [ ] **Settings** > section **Claude Code statusline hook** > **Install hook**, then confirm. The status changes to **Installed**.
- [ ] Use Claude Code for a minute. The overview shows the 5-hour and weekly limits with reset times, and **Last reading** under the hook shows a time.
- [ ] If you had your own statusline before, it still shows in Claude Code.
- [ ] A file `settings.json.cooldown-backup-<date>` now sits next to `%USERPROFILE%\.claude\settings.json`.
- [ ] Copy the `statusLine` entry from `%USERPROFILE%\.claude\settings.json` into your notes (it shows which shell the hook runs through).

## 4. Codex (only if `codex` works in a terminal)
- [ ] Codex limits show on the overview within a few minutes.
- [ ] No black console window flashes while Cooldown polls.
- [ ] With Codex itself closed, Task Manager > Details shows no `node.exe` that Cooldown left behind (wait a minute after a poll).

## 5. API keys
- [ ] **Settings** > **Adapters**: save an OpenRouter or DeepSeek key with **Save key**. The balance shows.
- [ ] Quit and restart Cooldown: the key is still saved (no need to paste it again). Windows **Credential Manager** > **Windows Credentials** has an entry that mentions `cooldown`.
- [ ] **Remove key**: the balance disappears from the overview.

## 6. Bells (notifications)
- [ ] On a limit, press **Notify me when it resets**. It appears under **Alerts**. When the reset comes, a Windows notification appears, such as **Claude 5-hour limit is back**.
- [ ] If none appears, check that Settings > System > Notifications allows Cooldown, and look in the log for `could not show the notification`.

## 7. Phone listening (no phone needed)
- [ ] Open **Pair** in the menu (the page is called **Pair a device**). A QR code and a countdown appear. Windows Firewall asks whether Cooldown may communicate: tick **Private networks** only, then allow.
- [ ] On a network Windows calls *Public*, pairing refuses with "This PC is not on a network Windows treats as private…".

## 8. Phone pairing (only if you have the Android app)
Steps are in [install-android.md](install-android.md#pair-with-your-pc).
- [ ] The phone scans the QR code. Both screens show the same six digits.
- [ ] **It matches, pair** on the PC. The phone says it is paired, and **Settings** > **Paired devices** lists it.
- [ ] The overview on the phone shows the same limits as the PC.

## 9. Launch at login
- [ ] Restart the laptop. Cooldown starts on its own and stays hidden in the tray.

## 10. Update and uninstall
- [ ] *(Optional, when a second version exists)* Quit Cooldown, run the newer installer. Settings, keys and the hook are still there.
- [ ] **Settings** > **Uninstall hook** and confirm. `statusLine` in `%USERPROFILE%\.claude\settings.json` is back to what it was before step 3, and Claude Code shows your old statusline.
- [ ] Quit Cooldown, then **Windows Settings** > **Apps** > **Installed apps** > **Cooldown** > **Uninstall**. Note whether the uninstaller offers to delete the app data, and whether `%LOCALAPPDATA%\app.cooldown.desktop` and `%LOCALAPPDATA%\Cooldown` are gone afterwards.

## What to send back
For each step: OK, or what happened instead. For failures: a screenshot and `cooldown.log`. Open an issue (**Bug report**) or send it to the maintainers.
