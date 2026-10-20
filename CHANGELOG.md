# Changelog

All notable changes to Cooldown. Dates are UTC.

## 0.1.0 — first public release

### What Cooldown does

- Shows how much of your AI limits you have used and exactly when they reset, on a Windows PC and an optional paired Android phone.
- **Sources:** Claude Code (5-hour and weekly, through its status line hook), Codex (5-hour and weekly), Antigravity CLI, formerly Gemini CLI (weekly quota, through its status line hook), GitHub Copilot (AI credits spent this month), and prepaid balances for OpenRouter, DeepSeek, xAI and Kimi.
- **Choose what you see:** Settings > Agents and sources has a switch per source. A source that is switched off is not read at all.
- **Notifications,** on by default and each switchable: when a limit is back, when one runs out, and when one is running low (75, 80, 90 or 95%). Plus a bell you can set by hand on any limit. They ring on the PC and on a paired phone.
- **Overview** answers "can I work now?" per provider ("Ready", "Limit reached, back in 2h 10m", "Weekly at 91%"), with balances in their own row.
- Every number shows where it came from (official, estimated, scraped) and how old it is. A failing source keeps its last good value and says so.
- **Phone pairing** by QR code over your own network, end-to-end encrypted (Noise), with a six-digit code you confirm on the PC. No cloud, no account.

### Privacy

- No account, no server, no telemetry. API keys stay in the Windows Credential Manager on the PC where you typed them.
- Cooldown never reads subscription logins, tokens, cookies or private endpoints. Agents with no documented way to read usage are not supported; the README lists them and why.

### Known limitations

- The Windows installer and the app are **not code-signed**: Windows SmartScreen warns, and PCs with Smart App Control switched on block them. See docs/install-windows.md.
- The Android app is installed by hand (not on Google Play) and has been tested on the Android emulator, not on a range of real phones.
- Claude Code and Antigravity CLI only report their limits while they are running; Cooldown shows the last reading and its age in between.
- GitHub does not report a Copilot plan's allowance, so the percentage needs the allowance you enter.
