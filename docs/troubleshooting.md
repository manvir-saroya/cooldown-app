# Troubleshooting and FAQ

Find your problem below. Each answer starts with the quickest fix.

**Contents**

- [First, two quick checks](#first-two-quick-checks)
- [I see no Claude numbers](#i-see-no-claude-numbers)
- [Codex is not found](#codex-is-not-found)
- [A balance says unavailable](#a-balance-says-unavailable)
- [I do not get notifications](#i-do-not-get-notifications)
- [The Android widget is out of date or empty](#the-android-widget-is-out-of-date-or-empty)
- [My phone cannot find my PC](#my-phone-cannot-find-my-pc)
- [Pairing fails or the digits do not match](#pairing-fails-or-the-digits-do-not-match)
- [The hook will not install, or my Claude status line looks wrong](#the-hook-will-not-install-or-my-claude-status-line-looks-wrong)
- [Where the log files are](#where-the-log-files-are)
- [How to report a bug](#how-to-report-a-bug)
- [FAQ](#faq)

## First, two quick checks

1. **Is Cooldown running?** Look for the bell icon in the system tray, next to the clock (it may be hidden behind the **^** arrow). Closing the window does not quit the app, but **Quit Cooldown** does. If it is not there, start **Cooldown** from the Start menu.
2. **Read the note.** Open **Settings** and look at the **Adapters** section. Each source has a status (**Current**, **Stale**, **Degraded** or **Unavailable**) and often a short sentence that says exactly what is wrong. Many problems explain themselves there. You can also open a limit on the **Overview** to see its **Source details**.

To ask Cooldown to look again right now, use the tray menu item **Check limits now**, or the refresh button at the top of the window.

## I see no Claude numbers

Claude numbers come from Claude Code's statusline, through the Cooldown hook. Check these in order:

1. **Is the hook installed?** Open **Settings**, section **Claude Code statusline hook**. It must say **Installed**. If it says **Not installed**, select **Install hook** and confirm.
2. **Have you used Claude Code since?** The numbers arrive only when Claude Code runs and has answered at least once in a session. Open Claude Code, send a message, then look again. The row **Last reading** shows when Cooldown last heard from it. "Nothing received yet" means nothing has arrived.
3. **Do you have a Claude subscription?** Claude Code reports limits only to **Pro and Max** subscribers. If you use Claude Code with an API key, there are no limits to show. The note will say: *"Claude Code has not reported this window (it only does for Pro/Max subscribers, after the first response of a session)."*
4. **Start a new Claude Code session.** If you installed the hook while Claude Code was already open, a fresh session makes sure it picks up the change. <!-- TODO: verify whether an already-running Claude Code session needs restarting after the hook is installed -->
5. **Is the numbers' age the problem?** If Claude Code is closed, Cooldown shows the last reading and its age, for example "last observed 3 hours ago", and marks it **Stale** once the reset time has passed. That is by design: Cooldown does not guess.
6. **Did you move or delete Cooldown?** The hook is a small program, `cooldown-statusline.exe`, that lives in Cooldown's install folder (`%LOCALAPPDATA%\Cooldown`, next to `cooldown-desktop.exe`). If the folder changed, select **Install hook** again.
7. **Is there a "chained" statusline problem?** The hook runs your old statusline after it saves the numbers. Your old statusline should look the same as before. If Claude Code shows nothing at all, select **Uninstall hook** and tell us in an issue what your old statusline command was.

Cooldown never reads your Claude login. There is no other way to get these numbers, so there is no "log in" button to try.

## Codex is not found

Cooldown asks the Codex program on your PC for your limits.

1. **Does `codex` work in a terminal?** Open **PowerShell** and type `codex --version`. If it says the command is not found, Codex is not installed, or it is not on your `PATH`. Install Codex, or fix your `PATH`, and then use **Check limits now** in the tray menu.
2. **Restart Cooldown** after installing Codex or changing your `PATH`. A program only sees the `PATH` it started with. Quit Cooldown from the tray and start it again.
3. **Are you signed in to Codex?** Run `codex` once in a terminal and sign in the way Codex asks. Cooldown never touches your Codex login; it only works if Codex itself can read your limits.
4. **Give it a few minutes.** The first Codex reading can take a short while.
5. **No `codex` program, but you have used Codex before?** Cooldown can fall back to the session records that Codex keeps in `.codex\sessions` in your user folder. These only give a reading after you have used Codex, and they are labelled **Scraped**.
6. **The note says "Codex CLI not found. Make sure the codex command works in a terminal, then use the refresh button".** Do step 1, then press the refresh button at the top of the window (or **Check limits now** in the tray menu). Cooldown looks for `codex` on your `PATH` and has no setting for a custom location, so if the command only works from one folder, add that folder to your `PATH` and restart Cooldown (step 2).

If the status is **Degraded**, a Codex update may have changed something Cooldown depends on, because Codex's limit information is not documented. [Open a bug report](../../../issues/new/choose) and include your Codex version (`codex --version`).

## A balance says unavailable

This is for OpenRouter, DeepSeek and xAI. Look at the sentence under the provider's row in **Settings** > **Adapters**:

| What it says | What it means and what to do |
|---|---|
| *No ... key saved. Add one in Settings* | You have not saved a key. Paste it and select **Save key**. |
| *The saved ... key is not valid. Replace it in Settings* | The saved text cannot be a key (for example it has spaces or line breaks). Paste it again. |
| *Could not read the ... key from the system credential store* | Windows would not give Cooldown its saved key. Save it again. If this repeats, check that Windows Credential Manager works for your user. |
| *... request failed (HTTP 401)* or *(HTTP 403)* | The provider refused the key. It may be wrong, expired or revoked. Make a new key at the provider and replace it. For xAI, remember that you need a **management key**, not a normal API key. If the key covers your whole organization, also fill in the right **Team ID**. |
| *This xAI management key is organization-scoped. Fill in the Team ID in Settings and save the key again* (or *xAI did not say which team this key belongs to...*) | xAI could not tell which team to read. In **Settings** > **Adapters**, fill in **Team ID** in the xAI row, paste the key again and select **Save key**. Saving a key replaces the saved team ID, so the ID has to be typed with the key. |
| *... request failed (HTTP 429)* | You asked too often. Wait. Cooldown slows down by itself after failures. |
| *... request failed (HTTP 5xx)* | The provider has a problem. Try again later. |
| *request failed (timed out)*, *(could not connect)* or *(network error)* | No internet, a blocking firewall or a proxy. Check that your PC can open the provider's website. |
| *request failed (response too large)* or *(redirect refused)* | The provider's answer looked wrong, so Cooldown refused it on purpose. Try again later, and [report it](#how-to-report-a-bug) if it keeps happening. |
| *... response did not match the expected shape* | The provider changed its answer. Please [report it](#how-to-report-a-bug) with the provider's name. |

Your last good balance stays visible, marked **Stale** or **Unavailable**, while this happens.

Tips:

- Use the eye icon next to the key field to check what you pasted.
- A balance has no reset time, so there is no bell for it.
- OpenRouter shows what is left only if your OpenRouter key has a spending limit. Without one it shows what you have used.
- After you select **Remove key**, the balance disappears from the **Overview** too.

## I do not get notifications

### On the PC

1. **Cooldown must be running.** Bells only ring while Cooldown is in the tray. Check the tray icon.
2. **Is the bell really set?** Open **Alerts**. Your bell must be listed with a ring time, and its switch must be on. A bell can only be set on a limit that has a reset time in the future. If the button says *"Waiting for a fresh reading before a bell can be set"*, Cooldown needs a newer reading first (for Claude: use Claude Code once).
3. **Allow notifications for Cooldown in Windows.** Open **Windows Settings** > **System** > **Notifications**, make sure notifications are on, and find **Cooldown** in the list below and turn it on.
4. **Check Do Not Disturb or Focus.** Windows hides notifications while Do Not Disturb (Focus assist) is on. They appear in the notification centre afterwards.
5. **Check the log.** Look for the line `could not show the notification` in the [log file](#where-the-log-files-are).

### On the phone

1. Open the app, then **Settings** > **Permissions**. **Notifications** and **Exact alarms** must say **Allowed**. If not, select **Grant permissions**.
2. In Android's settings, check that the notification channel **Limit resets** for Cooldown is not muted.
3. Check Do Not Disturb.
4. Check the [battery settings](install-android.md#make-sure-android-does-not-stop-the-alarms). Some phones stop apps' alarms.
5. Remember that the phone only knows about bells it has synced. It syncs when you open the app and, on the same Wi-Fi as the PC, in the background about every 15 minutes (Android may delay that). Open the app near the PC once so it can sync.

## The Android widget is out of date or empty

1. The widget shows **as of** and a time: that is when the phone last got numbers from the PC, not the time now. It is refreshed whenever the app syncs and about every 15 minutes in the background, but only while the PC is on and on the same Wi-Fi. See [Background sync](install-android.md#background-sync).
2. It says **No data yet. Pair with your PC.** until you have paired and the phone has synced once.
3. It shows only the windows you chose. Open the app, **Settings** > **Home-screen widget**, and check **Show every window** or the ticked list.
4. Battery savers can delay the background sync. See the [battery settings](install-android.md#make-sure-android-does-not-stop-the-alarms).

## My phone cannot find my PC

Work through this list from the top.

1. **Same network.** Both devices must be on the **same Wi-Fi network**. Turn off mobile data on the phone while testing. Turn off any VPN on both devices. Do not use a guest network: many routers stop devices on them from talking to each other (called *client isolation* or *AP isolation*).
2. **The PC is on, awake, and Cooldown is running** (tray icon).
3. **Windows sees the network as Private.** If Cooldown shows *"This PC is not on a network Windows treats as private"*, change it: **Windows Settings** > **Network & internet** > your Wi-Fi or Ethernet > set **Network profile type** to **Private network**. Cooldown never listens on **Public** networks. <!-- TODO: verify the exact Windows menu wording -->
4. **The firewall allows it.** The first time you open **Pair**, Windows Defender Firewall asks. You must allow **Private networks**. If you chose Cancel or allowed only Public, fix it:
   1. Open **Windows Security** > **Firewall & network protection** > **Allow an app through firewall**.
   2. Select **Change settings**.
   3. Find the entry for Cooldown (it may be listed as `Cooldown` or `cooldown-desktop`) and tick **Private**. If there is no entry, choose **Allow another app**, browse to `%LOCALAPPDATA%\Cooldown\cooldown-desktop.exe`, add it, and tick **Private**. <!-- TODO: verify how the app is named in the firewall list -->
   4. Select **OK**.
5. **Make a new code.** On the PC, open **Pair** and select **Get a new code**. A code lives about 2 minutes and works once.
6. **Android permissions.** On the phone: **Settings** > **Permissions** > **Local network** should be **Allowed** (Android 17 and newer ask for this). Camera must be allowed to scan.
7. **Multiple network adapters.** If your PC has several (Wi-Fi, VPN, virtual machines), the code carries up to four of its private addresses and the phone tries them. A VPN that takes over all traffic can still get in the way, so turn it off to test.
8. **Use Reconnect.** After pairing, if the phone says *"... is not connected"*, select **Reconnect**. If it still fails, check that the PC is awake and on the same network. The phone also looks for the PC by itself using a small local broadcast. Some routers block those broadcasts. Pairing again (with a fresh code) gives the phone the PC's current address.
9. **Port in use.** Cooldown prefers port 53320. If something else uses it, it picks another one by itself, and paired phones find it again.

If you have tried all of these, [open a bug report](#how-to-report-a-bug) and say what Wi-Fi setup you have (router model, guest network or not, VPN or not).

## Pairing fails or the digits do not match

- **The digits differ.** Choose **It does not match** on the PC. Someone else may be trying to pair. Make a new code and try again on a network you trust.
- **"The code expired before a phone connected."** Select **Get a new code** on the PC and scan the new one.
- **"No one confirmed the code within a minute."** You have one minute to compare the digits and select **It matches, pair**. Try again.
- **"Too many failed attempts, so that code no longer works."** Select **Get a new code**.
- **"The phone disconnected before pairing finished."** Stay on the pairing screen on both devices and try again.
- **The camera does not read the code.** Make the QR code bigger (full-screen the window), reduce glare, and hold the phone steady. Or skip the camera: on the PC select **Copy code** (under **Cannot scan? Copy the code**), send the text to the phone, and on the phone choose **Paste code instead**. If the app says *"That is not a Cooldown pairing code"*, you scanned or pasted something else. The pasted text must start with `CD1:` and be complete.
- **A phone shows on the PC under Paired devices but is gone from the phone** (or the other way round). Revoke or unpair it on both sides and pair again.

## The hook will not install, or my Claude status line looks wrong

- *"Cannot find your home folder..."*: Cooldown could not locate `.claude\settings.json`. Make sure you are signed in to Windows as the user that uses Claude Code.
- An error that mentions the settings file not being valid JSON: Cooldown refuses to touch a settings file it cannot read. Nothing was changed. Open `%USERPROFILE%\.claude\settings.json` in Notepad and fix the mistake (a missing comma or quote is the usual cause), then try again.
- *"The hook program is missing from the app folder. Reinstall Cooldown and try again."*: run the installer again.
- The status says **Not installed** after you updated or moved Cooldown: select **Install hook** to repair it.
- Claude Code shows an empty or broken status line after you removed Cooldown: follow "Already uninstalled without removing the hook?" in [Install on Windows](install-windows.md#uninstalling-cooldown).

## Where the log files are

| System | Log file |
|---|---|
| **Windows PC** | `%LOCALAPPDATA%\app.cooldown.desktop\logs\cooldown.log` (paste that into the File Explorer address bar) |
| **macOS** (development builds only) | `~/Library/Logs/app.cooldown.desktop/` |
| **Android** | There is no log file on the phone. Developers can read the log with `adb logcat -s CooldownSync CooldownAlarms CooldownWidget CooldownNative CooldownScan CooldownKeyWrap` while the phone is connected by USB. `CooldownSync` is the background sync (one line per run); the others are alarms, the widget, permissions, the QR scanner and the key wrapping. Nothing secret is logged. |

The Windows log holds warnings and errors, grows to about 1 MB and then starts a new file. Cooldown never writes your API keys to it. File paths in it can include your Windows user name, so read it before posting it. More places data lives are in [Install on Windows](install-windows.md#where-your-data-and-logs-live).

## How to report a bug

Good reports get fixed faster. It takes five minutes.

1. **Search first.** Look through the [existing issues](../../../issues). If yours is there, add a comment with your details.
2. **Open a new one.** Go to [New issue](../../../issues/new/choose) and choose **Bug report**. Fill in the form.
3. **Tell us:**
   - **Which app:** Windows or Android, and the **version**. The version is in the installer's file name (for example `Cooldown_0.1.0_x64-setup.exe`) and in the log, on a line that starts with `Cooldown 0.1.0 starting`.
   - **Your system:** Windows 10 or 11, or the Android version and phone model.
   - **What you did**, step by step.
   - **What you expected**, and **what happened instead**.
   - **Which source** is involved (Claude, Codex, OpenRouter, ...).
4. **Attach evidence:**
   - A screenshot of the problem. Cover anything personal.
   - The Windows log file `cooldown.log` (see above). Read it first and remove anything you do not want to share.
5. **Never post** API keys, tokens, the pairing code or QR code, or your Claude or Codex login files. If you pasted a key by accident, delete the comment and replace the key at the provider.

**Is it a security problem?** (For example: a way to pair a phone without confirmation, a key showing up somewhere it should not, or the hook damaging Claude's settings.) Do not post it in public. Follow [SECURITY.md](../SECURITY.md).

## FAQ

**Is Cooldown really free? Do I need an account?**
Yes, it is free and open source (MIT license). There is no account, no ads, no paid version and no server. It will stay that way.

**Does Cooldown read my Claude or Codex password or tokens?**
No. It never reads login files, tokens or cookies, and it never calls private usage addresses. Claude numbers come from Claude Code's official statusline. See [How it works](how-it-works.md).

**Why are my Claude numbers old?**
Claude Code only shares them while it is running. Cooldown shows the last reading with its age and does not guess.

**Why can't it show Claude Opus and Sonnet limits separately?**
Claude Code's statusline does not provide them. Cooldown shows "not available" rather than making something up.

**I only use Claude on the website (claude.ai), not Claude Code. Will it work?**
No. There is no official way to read those limits, so Cooldown does not try. You could still use it for Codex and API balances.

**Why does Windows warn me about the installer?**
It is not code-signed, because certificates cost money and Cooldown is a free, non-commercial project. Check the checksum as described in [Install on Windows](install-windows.md#2-check-the-file-recommended), then choose **More info** > **Run anyway**.

**Does it work on Mac, Linux or iPhone?**
The Windows and Android apps are the supported ones. The code also builds on macOS for development, but there are no Mac or Linux releases and no iPhone version.

**Does my PC have to stay on?**
For bells on the PC and for fresh numbers, yes: only the PC reads the sources. A paired phone keeps its own alarms, so they still ring if the PC goes to sleep.

**Can I use it away from home, on mobile data?**
Not today. The phone and the PC talk directly on the same local network, with nothing in between. There is no internet relay.

**Where are my API keys?**
In the Windows Credential Manager, on that PC. They are never copied to a phone or a file, and only sent to their own provider.

**Can I pair more than one phone, or one phone with two PCs?**
Yes. A PC can pair with up to 8 phones. On the phone, **Settings** > **Your PC** lists your PCs, and you can use **Pair a different PC**.

**Does Cooldown update itself or send any data about me?**
No. It has no updater and no telemetry. To update, download the new version from the [Releases page](../../../releases).

**How do I stop it starting with Windows?**
Right-click the tray icon and untick **Start at login**.

**How do I remove everything?**
Follow [Uninstalling Cooldown](install-windows.md#uninstalling-cooldown) (it includes removing the hook), and [Unpair, update or uninstall](install-android.md#unpair-update-or-uninstall) on the phone.

**Where can I ask something that is not a bug?**
Open an [issue](../../../issues/new/choose). Questions are welcome.
