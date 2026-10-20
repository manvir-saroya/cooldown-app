**Cooldown {{VERSION}}** shows how much of your AI usage limits you have used (Claude Code, Codex, Antigravity CLI, GitHub Copilot) and your API balances (OpenRouter, DeepSeek, xAI, Kimi), says exactly when they reset, and rings on your PC and your phone the moment you can work again.

It runs on your own devices: no account, no hosted service, no server of ours. The phone pairs with the PC by QR code over your own network.

> **Early release.** The Windows installer and the Android app are not signed by a publisher your system already knows, so each warns you once. The steps below show what to click.

## Downloads

| File | For |
|---|---|
| `{{WINDOWS_ASSET}}` | Windows 10 and 11, 64-bit |
| `{{ANDROID_ASSET}}` | Android 7.0 or newer, 64-bit ARM (nearly every phone sold since 2017) |
| `SHA256SUMS.txt` | Checksums of the two files above |

## Install on Windows

1. Download `{{WINDOWS_ASSET}}` and double-click it. It installs for your user only, so it asks for no administrator rights.
2. Windows SmartScreen shows **"Windows protected your PC"** because the installer is not signed with a code-signing certificate yet. Click **More info**, then **Run anyway**. On a PC with **Smart App Control** switched on, Windows blocks unsigned apps with no way to run them; see docs/install-windows.md in the repository.
3. Cooldown starts in the system tray (a bell icon; it may sit under the `^` arrow). In **Settings**, press **Install hook** to read Claude Code's limits, choose which agents to show under **Agents and sources**, and paste an API key or token for the ones that need one. Notifications for "running low", "ran out" and "limit is back" are on by default.

If something goes wrong, the log is at `%LOCALAPPDATA%\app.cooldown.desktop\logs\cooldown.log`.

## Install on Android (sideload)

1. On your phone, download `{{ANDROID_ASSET}}` from this page (or download it on a PC and copy it over).
2. Open the file. Android asks whether this app (your browser or Files app) may install apps: allow it. You can switch that off again afterwards under Settings > Apps > Special app access > Install unknown apps.
3. Google Play Protect may say it does not recognise the app. Choose **More details**, then **Install anyway**.
4. Open Cooldown. On the PC choose **Pair a device** and scan the QR code. Allow notifications, the camera (for the scan) and **Alarms & reminders** when asked, so the bell can ring on time. The phone and the PC must be on the same network.

{{#signed}}
The APK is signed with the project's release key. Its certificate SHA-256 fingerprint is `{{CERT_SHA256}}`. Later releases are signed with the same key, so they install over this one and keep your pairing.
{{/signed}}
{{^signed}}
**This APK is signed with a throwaway debug key** that was created for this one build. It installs and runs, but Android will refuse to update it with a later release: uninstall it first, then install the newer one and pair again.
{{/signed}}

## Verify your download

`SHA256SUMS.txt` has the SHA-256 checksum of each file:

```
{{CHECKSUMS}}
```

On Windows, in PowerShell: `Get-FileHash .\{{WINDOWS_ASSET}} -Algorithm SHA256`. On macOS or Linux, in the folder with the files: `sha256sum -c SHA256SUMS.txt`. A checksum shows the file arrived intact; it does not say who built it.

Both files were built by [this GitHub Actions run]({{RUN_URL}}) from commit `{{COMMIT}}`.
