# Cooldown

See how much of your AI limits you've used, exactly when they reset, and get told the moment you can work again.

Free, open source and local-first. It runs on your own machines. No account, no hosted service, no ads, no tracking, and your keys never leave the device you typed them on.

## What it shows

| Source | What you get | Where the numbers come from |
|---|---|---|
| Claude Code | 5-hour session and weekly limits, reset times | Claude Code's own statusline hook (official) |
| Codex | Session and weekly limits, reset times | `codex app-server`, falling back to Codex's local session logs |
| Antigravity CLI (formerly Gemini CLI) | Weekly quota, reset time | Antigravity CLI's own status line hook, which passes its `quota` object (official) |
| GitHub Copilot | AI credits spent this month; a percentage once you enter your plan's allowance | GitHub's billing API, with a fine-grained token that can only read your plan |
| OpenRouter, DeepSeek, xAI, Kimi | Prepaid balance (USD; DeepSeek also in CNY) | Each provider's API, with your own key |

In **Settings > Agents and sources** you choose which of these appear on the dashboard. A source that is switched off is not read at all.

Every number shows its source (official, estimated or scraped) and how old it is. Claude Code only reports limits while it is running, so a reading can be "last observed 2 hours ago"; Cooldown says so instead of guessing.

Press **Notify me when it resets** on any limit and Cooldown rings the moment it comes back, on your PC and on your phone. By default it also tells you when a limit is running low (90%, adjustable), when one runs out, and when a used-up limit is back; each can be switched off in Settings.

### Agents that are not supported, and why

Cooldown only reads a number through a route the agent's maker documents for it: a status line or hook, its own command, or its API with your key. It never reads a login, a cookie or a private endpoint. These agents have no such route today, so they are left out:

| Agent | Why not |
|---|---|
| Cursor CLI | Its status line gets only the session's cost; plan usage is shown only inside the interactive `/usage` screen. |
| Kiro CLI | Usage is shown by the interactive `/usage` command, and headless mode does not run slash commands. |
| Factory Droid | `/cost`, `/stats` and `/limits` are interactive only. |
| Qwen Code | Its status line has token counts but no quota. |
| OpenCode Go, Z.ai GLM Coding Plan | Usage is only on their web consoles (or undocumented endpoints). |
| Amp | `amp usage` prints the credit balance as text; support is planned once its output can be checked against a real account. |

If one of these adds a documented way to read usage, it can be added. See [Adding a provider adapter](CONTRIBUTING.md#adding-a-provider-adapter).

## What it never touches

- Subscription logins: no OAuth tokens, no `~/.claude/.credentials.json`, no `~/.codex/auth.json`, no browser cookies, no private usage endpoints.
- Any server of ours. There isn't one. Your PC and phone talk to each other directly on your own network.

API keys go into the operating system's credential store (Windows Credential Manager) and stay on that PC. A paired phone only receives the sanitized readings (percentages, balances, reset times, labels), encrypted. See [How it works](docs/how-it-works.md).

## Download and install

Get the files from the **[Releases page](../../releases)**.

| Device | File | Guide |
|---|---|---|
| **Windows 10 or 11** (the PC app) | `Cooldown_<version>_x64-setup.exe` | [Install on Windows](docs/install-windows.md) |
| **Android 7.0 or newer** (the phone app, optional) | `Cooldown_<version>_android.apk` | [Install on Android](docs/install-android.md) |

Each release also has a `SHA256SUMS.txt` so you can check your download. The guides show how.

The Windows installer is **not code-signed** (a certificate costs money and this project is free), so Windows SmartScreen warns when you run it: choose **More info**, then **Run anyway**. On a PC with **Smart App Control** switched on, Windows blocks unsigned apps outright; see the [Windows guide](docs/install-windows.md#3-install). The Android file is installed by hand ("sideloaded"), because Cooldown is not on Google Play.

Mac and Linux builds work for development but are not supported releases. There is no iPhone version.

## First run

1. **Claude Code:** open **Settings** in Cooldown and select **Install hook**. This adds Cooldown's hook to `~/.claude/settings.json`; an existing `statusLine` entry is backed up and still runs. **Uninstall hook** puts everything back.
2. **Codex:** nothing to do if the `codex` command works in a terminal.
3. **Pick your agents:** in **Settings > Agents and sources**, switch on what you use. Antigravity CLI needs **Install hook** in its row; GitHub Copilot needs a fine-grained token with only *Plan: read-only*.
4. **API balances:** in the same list, paste a key for OpenRouter, DeepSeek or Kimi, or an xAI management key. The xAI team ID is optional: Cooldown finds the team from the key, and only a key that covers a whole organization needs it.
5. **Phone (optional):** on the PC choose **Pair**, then scan the QR code with the Cooldown Android app and compare the six digits. The first time, Windows Firewall asks whether Cooldown may listen; allow it on **Private networks** only.
6. Notifications for "running low", "ran out" and "limit is back" are on by default (**Settings > Notifications**). You can also set a bell by hand on any limit.

Full steps are in the [Windows guide](docs/install-windows.md) and the [Android guide](docs/install-android.md). If something goes wrong, see [Troubleshooting](docs/troubleshooting.md). The Windows log is at `%LOCALAPPDATA%\app.cooldown.desktop\logs\cooldown.log`.

## Documentation

| I want to... | Read |
|---|---|
| See what changed in each version | [CHANGELOG.md](CHANGELOG.md) |
| Install the Windows app | [docs/install-windows.md](docs/install-windows.md) |
| Install the Android app and pair it | [docs/install-android.md](docs/install-android.md) |
| Fix a problem, or read the FAQ | [docs/troubleshooting.md](docs/troubleshooting.md) |
| Understand where numbers come from and what is sent where | [docs/how-it-works.md](docs/how-it-works.md) |
| Build it myself | [docs/build-from-source.md](docs/build-from-source.md) |
| Help with the code | [CONTRIBUTING.md](CONTRIBUTING.md) |
| Report a security problem | [SECURITY.md](SECURITY.md) |
| See the pairing and sync protocol | [docs/pairing-protocol.md](docs/pairing-protocol.md) |
| See the design | [docs/superpowers/specs/2026-10-10-cooldown-design.md](docs/superpowers/specs/2026-10-10-cooldown-design.md) |

## Project status

Cooldown is **young**. Version 0.1.0 is the first public release.

- **Windows app:** the tests pass in CI on Windows, and CI installs the app on a real Windows runner, starts it hidden, checks its log and uninstalls it. It has not yet had much use on people's own PCs, so expect rough edges.
- **Android app:** the full flow (pairing, sync, alarm, widget) works on an Android emulator. It has not yet been tested on a range of real phones, so battery-saver behavior and local-network discovery are unproven.
- **Security:** the pairing and sync protocol has **not had an independent security audit**. Read [SECURITY.md](SECURITY.md) and [docs/pairing-protocol.md](docs/pairing-protocol.md).
- **Codex numbers** come from an undocumented interface and may break after a Codex update. Cooldown then marks them degraded instead of failing silently.
- **Android background sync** (about every 15 minutes with the app closed, on the same Wi-Fi as the PC) and the configurable, scrolling home-screen widget work on the emulator. How often real phones' battery managers let the sync run is unproven. Bells you already set ring either way.
- **Not built yet:** a signed Windows installer, and a local browser dashboard (the design mentions one; there is none).

Please report what you find: see [How to report a bug](docs/troubleshooting.md#how-to-report-a-bug).

## Build from source

You need Node 24, a stable Rust toolchain (rustup) and the [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/) for your system.

```sh
git clone <this repository URL>
cd cooldown
npm ci

npm test                                               # every TypeScript package
node apps/desktop/scripts/build-sidecar.mjs --debug    # builds the statusline hook the desktop crate needs
cargo test --workspace                                 # every Rust crate

npm run tauri dev -w @cooldown/desktop                 # run the desktop app
```

The Windows installer, the Android app and every prerequisite (JDK 17, Android SDK and NDK versions) are covered in [docs/build-from-source.md](docs/build-from-source.md).

## Contributing

Contributions are welcome: bug reports, documentation fixes, testing on your devices, and code. Please read [CONTRIBUTING.md](CONTRIBUTING.md). It has the project's hard rules (local-first, no hosted service, never read subscription logins, keys never leave the device, every number shows its source and age, no telemetry), the checks a pull request must pass, and how to add a new provider.

By taking part you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).

## Repository layout

```
crates/
  cooldown-core/        data types, adapter registry, SQLite store, alert logic
  cooldown-statusline/  the Claude Code statusline hook (a small binary)
  cooldown-adapters/    Claude, Codex, OpenRouter, DeepSeek and xAI sources
  cooldown-sync/        QR pairing and the encrypted PC-to-phone link
packages/
  snapshot-schema/      the data contract: TypeScript types, zod, JSON Schema, shared test fixtures
  ui/                   the React dashboard shared by both apps
apps/
  desktop/              Tauri 2 Windows app
  android/              Tauri 2 Android app
docs/                   guides, pairing protocol and threat model, design spec
```

The data contract in `packages/snapshot-schema` is shared by the Rust and TypeScript code; both sides run the same fixtures.

## Security

Found a vulnerability? Please report it privately; see [SECURITY.md](SECURITY.md). Do not open a public issue for it.

## License

Cooldown is MIT licensed; see [LICENSE](LICENSE).

## Acknowledgements

Cooldown reuses ideas and code from other open-source projects; each one is credited, with its license, in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

Cooldown was built with [Claude](https://www.anthropic.com/claude), Anthropic's AI model, through [Claude Code](https://claude.com/claude-code). Claude wrote much of the code, tests and docs alongside the maintainer.
