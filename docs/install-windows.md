# Install Cooldown on Windows

This guide takes you from download to a working app. It takes about 10 minutes.

You need a PC with **Windows 10 or Windows 11 (64-bit)**. You do not need an administrator password: Cooldown installs for your own user only.

**Contents:** [Download](#1-download) · [Check the file](#2-check-the-file-recommended) · [Install](#3-install) · [First run](#4-first-run) · [Where your data lives](#where-your-data-and-logs-live) · [Update](#updating-cooldown) · [Uninstall](#uninstalling-cooldown) · [Troubleshooting](#having-trouble)

## 1. Download

1. Open the [Releases page](../../../releases).
2. Under the newest release, open **Assets**.
3. Download these two files:
   - `Cooldown_<version>_x64-setup.exe` (the installer)
   - `SHA256SUMS.txt` (the checksums, used in the next step)

   `<version>` is the version number, for example `0.1.0`.

Only download Cooldown from this page. Cooldown has no other website and no app store listing.

> **Trying a build that is not released yet?** Builds from the `main` branch are on the repository's **Actions** tab. Open the latest green **Windows** run and download the `cooldown-windows-installer` artifact at the bottom. It is a `.zip`; unzip it and run the setup file inside. Use these only to help test. They have no checksum file.

## 2. Check the file (recommended)

A checksum is a short fingerprint of a file. If your downloaded file gives the same fingerprint as the one we published, the file was not damaged or changed on its way to you.

1. Open **PowerShell**. (Press the Windows key, type `PowerShell`, press Enter.)
2. Go to the folder with your downloads:

   ```powershell
   cd $HOME\Downloads
   ```

3. Print the fingerprint of the installer. Use your real file name:

   ```powershell
   Get-FileHash .\Cooldown_0.1.0_x64-setup.exe -Algorithm SHA256
   ```

4. Print the line for the installer from the checksum file:

   ```powershell
   Select-String -Path .\SHA256SUMS.txt -Pattern "x64-setup"
   ```

5. Compare the long string of letters and numbers. The two must be **identical**. Capital and small letters do not matter.

If they differ, **do not run the file**. Delete it, download it again, and if it still differs, [open an issue](../../../issues/new/choose).

The checksum only proves that your file is the one published on the Releases page. It does not replace the next step, where Windows warns you.

## 3. Install

1. Double-click `Cooldown_<version>_x64-setup.exe`.
2. **Windows will probably show a blue window titled "Windows protected your PC".** This is expected. The installer is **not code-signed**, because a signing certificate costs money and Cooldown is free and non-commercial. Windows warns about every unsigned installer from a small publisher.
   1. Select **More info**.
   2. Select **Run anyway**.
3. Follow the installer. It installs only for your user, into `%LOCALAPPDATA%\Cooldown`, and needs no administrator rights.
4. On the last page, leave **Run Cooldown** ticked and select **Finish**. Cooldown opens. <!-- TODO: verify the wording of the installer's last page -->

If your browser or antivirus blocks the download itself, keep the file only if the checksum from step 2 matched.

If Windows does not offer **Run anyway**: close the warning, right-click the file, choose **Properties**, tick **Unblock** at the bottom of the **General** tab, select **OK**, and try again.

**Smart App Control.** Some Windows 11 PCs have *Smart App Control* switched on (Windows Security > App & browser control > Smart App Control). When it is set to **On**, it blocks unsigned programs it does not know, with no **Run anyway** button, and that can include Cooldown's installer, the app, or its small hook program (`cooldown-statusline.exe`). There is no safe way around this for an unsigned app: Cooldown cannot be used on that PC unless Smart App Control is set to **Off**. Think carefully before you do that, because Windows does not let you switch it back on without resetting Windows. If you prefer to keep it on, use Cooldown on another PC.

Cooldown needs Microsoft's WebView2 component to draw its window. Windows 11 and up-to-date Windows 10 already have it. If yours does not, the installer downloads it, which needs an internet connection.

## 4. First run

When Cooldown opens, you see the **Overview** screen. The menu has four parts: **Overview**, **Alerts**, **Pair** and **Settings**.

A **bell icon** appears in the system tray, next to the clock. It may be hidden behind the **^** arrow.

**Closing the window does not quit Cooldown.** It keeps running in the tray, because that is how it can ring when a limit resets. To really quit, right-click the tray icon and choose **Quit Cooldown**.

The tray menu has:

- **Open Cooldown**
- **Check limits now**
- **Start at login** (a tick box)
- **Quit Cooldown**

Left-clicking the tray icon also opens the window.

**Start at login** is switched on automatically the first time you run an installed Cooldown, so the app starts hidden in the tray when you sign in to Windows. If you do not want that, untick it in the tray menu. It stays off.

Now connect the things you want to watch. Skip any you do not use.

### Claude Code

Cooldown reads your Claude Code limits through Claude Code's own *statusline*, which is an official feature. It never reads your Claude login.

1. Open **Settings**.
2. Find the section **Claude Code statusline hook**.
3. Select **Install hook**.
4. A box asks **Install the statusline hook?** Select **Install hook** again to confirm.
5. The status changes to **Installed**.

What this does:

- It makes a backup copy of `%USERPROFILE%\.claude\settings.json`, next to the original, named `settings.json.cooldown-backup-<date and time>`.
- It saves your current `statusLine` entry, if you have one, so it can be restored.
- It points `statusLine` at a small program, `cooldown-statusline.exe`, that comes with Cooldown and sits next to `cooldown-desktop.exe` (the app itself) in the install folder. The program saves your two limits to a local file, then runs your old statusline, so you still see exactly what you saw before.
- **Uninstall hook** (same place) puts everything back.

Then use Claude Code as normal. After its first answer in a session, the **Overview** shows your 5-hour and weekly limits with their reset times. The row **Last reading** under the hook shows when Cooldown last heard from Claude Code.

Important to know:

- Claude Code only sends these numbers **while it is running**. When it is closed, Cooldown shows the last reading and how old it is. It does not guess.
- Claude Code only reports limits to **Pro and Max subscribers**, not to API-key users.
- If the numbers do not appear, see [Troubleshooting: no Claude numbers](troubleshooting.md#i-see-no-claude-numbers).

### Codex

Nothing to set up. If the `codex` command works in a terminal on this PC, Cooldown finds it and reads your Codex limits through Codex itself. If it cannot, it falls back to Codex's local session logs. Codex numbers can take a few minutes to appear. Cooldown never reads your Codex login. See [Troubleshooting: Codex not found](troubleshooting.md#codex-is-not-found).

### Choose which agents to show

**Settings > Agents and sources** lists every agent and balance Cooldown can read, each with a **Show on dashboard** switch. A source you switch off is not read at all and disappears from the Overview (and from your phone). Claude Code, Codex and the API balances start switched on; Antigravity CLI starts off because it needs its hook first.

### Antigravity CLI (formerly Gemini CLI)

Cooldown reads Antigravity CLI's quota through Antigravity's own *status line*, the same way as for Claude Code. It never reads your Google login.

1. In **Settings > Agents and sources**, switch **Antigravity CLI statusline** on.
2. Select **Install hook** in its row and confirm. This adds a status line command to `%USERPROFILE%\.gemini\antigravity-cli\settings.json`; a status line you already had is kept and still runs. **Uninstall hook** puts it back.
3. Use Antigravity CLI as normal. Its weekly quota appears on the Overview once it has sent one.

### GitHub Copilot

Copilot plans bill in GitHub AI Credits. Cooldown reads what you spent this month from GitHub's billing API.

1. On github.com, open **Settings > Developer settings > Personal access tokens > Fine-grained tokens** and create a token with only **Account permissions > Plan: Read-only**. It can read your billing usage and nothing else.
2. In **Settings > Agents and sources**, paste it into **GitHub token** under **GitHub Copilot** and select **Save key**.
3. Optional: type your plan's monthly AI-credit allowance in dollars (for example 10 for Pro). GitHub does not report it, so without it Cooldown shows what you spent; with it, it shows a percentage and can warn you before it runs out.

### Notifications

**Settings > Notifications** has three switches, all on by default: **A limit is back** (rings when a limit you used up resets, without pressing its bell), **A limit ran out** (at 100%) and **Running low** (once per window, at 75, 80, 90 or 95%; 90 by default).

### API balances (OpenRouter, DeepSeek, xAI, Kimi)

1. Open **Settings**, section **Agents and sources**.
2. Find the provider's row.
3. For **OpenRouter**, **DeepSeek** or **Kimi** (a platform.kimi.ai key), paste your key into **API key** and select **Save key**.
4. For **xAI**, paste a *management key* into **Management key** and select **Save key**. (A normal xAI API key does not work for balances.) **Team ID** is optional: Cooldown finds your team from the key. Fill it in only if the note under the xAI row says it cannot tell which team to read, which happens with a key that covers your whole organization. Saving a key replaces the saved team ID, so type it again when you paste a new key.
5. A message says the key is saved on this device, and the balance appears on the **Overview**.

Your key is stored in the **Windows Credential Manager**, on this PC only. It is sent only to the provider it belongs to, and never to your phone or anyone else. To delete it, select **Remove key**.

You can use the eye icon next to the field to check what you pasted.

### Your phone (optional)

1. Install the Android app first: [Install Cooldown on Android](install-android.md).
2. On the PC, open **Pair** (its page is called **Pair a device**). A QR code appears.
3. The first time, **Windows Defender Firewall** asks whether Cooldown may communicate on your networks.
   - Tick **Private networks**.
   - Leave **Public networks** unticked.
   - Select **Allow access**.
4. On the phone, scan the QR code. Both screens then show the same six digits. If they match, select **It matches, pair** on the PC.
5. If the phone's camera cannot read the QR code, use **Copy code** under **Cannot scan? Copy the code** instead, and paste it on the phone with **Paste code instead**.

The full steps are in [Install Cooldown on Android](install-android.md#pair-with-your-pc).

Things to know:

- Cooldown only starts listening for phones when you open **Pair** or once a phone is already paired. A fresh install opens no network port.
- It only listens on networks Windows calls **Private** (or on a domain). If your home Wi-Fi is set to **Public**, pairing stops with the message *"This PC is not on a network Windows treats as private."* To fix it: **Windows Settings** > **Network & internet** > your Wi-Fi > your network > set **Network profile type** to **Private network**. <!-- TODO: verify the exact Windows menu wording on Windows 10 and 11 -->
- If you clicked **Cancel** on the firewall prompt, see [Troubleshooting: phone cannot find the PC](troubleshooting.md#my-phone-cannot-find-my-pc).

### Bells (alerts)

On any limit, select **Notify me when it resets**. When the limit comes back, Windows shows a notification such as **Claude 5-hour limit is back**: *"Your usage limit has reset. You can work again."* Your bells are listed under **Alerts**.

A bell only rings while Cooldown is running. Keep it in the tray. If a notification does not show, see [Troubleshooting: notifications](troubleshooting.md#i-do-not-get-notifications).

## Where your data and logs live

You can paste these paths into the File Explorer address bar. `%APPDATA%` and `%LOCALAPPDATA%` are standard Windows shortcuts for folders in your user profile.

| What | Where |
|---|---|
| **The program** (`cooldown-desktop.exe`, and the hook `cooldown-statusline.exe` beside it) | `%LOCALAPPDATA%\Cooldown\` |
| **Log file** | `%LOCALAPPDATA%\app.cooldown.desktop\logs\cooldown.log` |
| App data: readings history (`cooldown.sqlite3`), settings (`settings.json`), paired phones (`trust.json`) | `%LOCALAPPDATA%\app.cooldown.desktop\` (the same folder as the log). It stays on this PC: it is not in the roaming profile that some work or school setups copy to other PCs. |
| Latest Claude Code reading from the hook (`claude-statusline.json`) | `%LOCALAPPDATA%\cooldown\inbox\`, which is a subfolder of the install folder above (Windows ignores capital letters in folder names) |
| Your saved original statusline (`chain.json`) | `%APPDATA%\cooldown\` |
| Claude Code settings the hook edits, and its backups | `%USERPROFILE%\.claude\settings.json` |
| API keys and the PC's pairing identity key | Windows **Credential Manager**, under **Windows Credentials**, entries that mention `cooldown` |

The log file holds warnings and errors. Each start adds a line like `Cooldown 0.1.0 starting, data in ...`. When the file reaches about 1 MB, a fresh one is started and a couple of older ones are kept. Cooldown never writes API keys to it. File paths in it can include your Windows user name, so look it over before you post it publicly.

Nothing is sent anywhere. Cooldown has no telemetry.

## Updating Cooldown

Cooldown does not update itself. To update:

1. Download the new installer from the [Releases page](../../../releases) and [check it](#2-check-the-file-recommended).
2. Right-click the tray icon and choose **Quit Cooldown**.
3. Run the new installer. It installs over the old version.
4. Your settings, saved keys and paired phones stay. <!-- TODO: verify that an upgrade keeps the data folder, keys and pairings -->
5. Open **Settings**. The hook should still say **Installed**. If it says **Not installed** after an update, select **Install hook** again to repair it.

## Uninstalling Cooldown

Do these in order.

1. **Uninstall the Claude Code hook first.** Open Cooldown, go to **Settings**, section **Claude Code statusline hook**, and select **Uninstall hook**, then confirm. This puts your original statusline back. If you skip it and remove Cooldown, Claude Code is left pointing at a program that no longer exists, and its status line may stop working.
2. *(Optional)* Remove your API keys: **Settings** > **Adapters** > **Remove key** on each row.
3. *(Optional)* Unpair phones: **Settings** > **Paired devices** > **Revoke**.
4. Right-click the tray icon and choose **Quit Cooldown**.
5. Open **Windows Settings** > **Apps** > **Installed apps**, find **Cooldown**, and choose **Uninstall**. If the uninstaller offers to delete the app data, tick it. <!-- TODO: verify the uninstaller's wording and that it removes %LOCALAPPDATA%\app.cooldown.desktop -->

To remove every trace, afterwards delete these folders if they exist:

- `%LOCALAPPDATA%\app.cooldown.desktop`
- `%LOCALAPPDATA%\Cooldown` (the install folder; it also holds the hook's `inbox` subfolder, so it can be left behind after uninstalling)
- `%APPDATA%\cooldown`

And open **Credential Manager** > **Windows Credentials** and remove any entries that mention `cooldown`.

**Already uninstalled without removing the hook?** Open `%USERPROFILE%\.claude\settings.json` in Notepad. Find the `"statusLine"` entry that mentions `cooldown-statusline`. Replace it with your old one, or delete the whole entry. Your old one is saved in `%APPDATA%\cooldown\chain.json` (under `"original"`) and in the `settings.json.cooldown-backup-...` file next to the settings file.

## Having trouble?

Start with [docs/troubleshooting.md](troubleshooting.md). It covers missing Claude numbers, Codex not found, balances that will not load, missing notifications and a phone that cannot find the PC. If that does not solve it, it also explains how to report a bug.

If you want to test the app in a systematic way on a Windows PC, there is a checklist in [windows-test.md](windows-test.md).
