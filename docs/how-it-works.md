# How Cooldown works

This page explains, in plain words, where Cooldown gets its numbers, what it stores, and what it sends over the network. The short version:

- Cooldown runs **only on your own devices**. There is no Cooldown server and no account.
- It never reads your Claude or Codex **login**.
- Your **API keys** stay on the PC where you typed them.
- The only thing that travels between your PC and your phone is a small, cleaned-up list of readings, and it is **encrypted** and **never leaves your own network**.

If you want the technical details, they are in [pairing-protocol.md](pairing-protocol.md) and in the README of each folder under `crates/`.

## The pieces

```
 Claude Code ──(its statusline)──► Cooldown hook ─► small file
                                                       │
 Codex (local program) ───────────────────────────────►│
                                                       ▼
 OpenRouter / DeepSeek / xAI ◄──(your key)── Cooldown on your PC ──► notifications + tray
                                                       │
                                  (encrypted, same network only)
                                                       ▼
                                             Cooldown on your phone ──► alarm + widget
```

- **Cooldown on the PC** is a tray app. It reads each source on a timer, keeps the readings in a small database on your PC, rings when a limit resets, and shares the readings with a paired phone.
- **The Claude Code hook** is a tiny helper program that Claude Code runs. It hands your limits to Cooldown.
- **Cooldown on the phone** shows the readings the PC sends, and sets an Android alarm for each bell. It catches up with the PC when you open it and, with the app closed, about every 15 minutes in the background (on the same network only). Its home-screen widget shows the windows you choose.

## Where each number comes from

| Source | What you get | Where it comes from | How often |
|---|---|---|---|
| **Claude Code** | 5-hour limit and weekly limit: percent used and reset time | Claude Code's own *statusline* feature, through the Cooldown hook. **Official.** | The file is checked about every 30 seconds. It only changes while Claude Code is running. |
| **Codex** | 5-hour (session) limit and weekly limit | Cooldown starts the local `codex` program and asks it for your rate limits (`codex app-server`). If that fails, it reads the usage records that Codex already keeps on your disk (`~/.codex/sessions`). **Scraped**, because neither way is documented by OpenAI. | About every 2 minutes. |
| **OpenRouter** | Credit used, and what is left if your key has a limit | OpenRouter's key-info web address, with your API key. **Official.** | About every 5 minutes |
| **DeepSeek** | Top-up balance in USD (or in CNY if that is all your account has) | DeepSeek's balance web address, with your API key. **Official.** | About every 5 minutes |
| **xAI** | Prepaid balance | xAI's Management API, with a *management key*. The team is found from the key; only a key that covers a whole organization also needs your team ID. **Official.** | About every 10 minutes |

Things to know:

- **No per-model Claude numbers.** Claude Code does not give out limits per model (Opus, Sonnet). Cooldown shows "not available" instead of guessing.
- **Claude numbers are only fresh while Claude Code runs.** When it is closed, Cooldown shows the last reading and how old it is, for example "last observed 2 hours ago". Claude Code also drops a limit from its report after the limit resets. When Cooldown sees that a limit it knew about has gone after its reset time, it understands that the reset happened.
- **Claude only reports limits to Pro and Max subscribers**, and only after its first answer in a session. API-key users get no numbers.
- **Codex can change under us.** Because Codex's limit information is undocumented, a Codex update can break it. Cooldown then shows the reading as **Degraded** or **Unavailable** instead of failing silently. Before changing or cancelling a bell because of a Codex reading, it wants two matching readings, since a single odd reading can happen.
- **Balances are not limits.** A balance does not reset, so there is no bell for it.

### Source and status labels

Every number carries labels, so you can judge it:

| Source label | Meaning |
|---|---|
| **Official** | Reported by the provider or by its own tool. |
| **Estimated** | Worked out from other data, so it can be off. |
| **Scraped** | Read from something undocumented that can change without notice. |
| **Manual** | Typed in by you. It only changes when you edit it. No built-in source uses this yet. |

| Status | Meaning |
|---|---|
| **Current** | Read recently, nothing wrong. |
| **Stale** | Not refreshed lately, or the window it describes has already ended. The last good value is kept. |
| **Degraded** | The source is struggling or Cooldown fell back to a weaker source. The value may be off. |
| **Unavailable** | The source cannot be read right now. The last good value is kept. |

Every number also shows **when it was last observed**. When a source fails, Cooldown **keeps the last good value** and marks it, instead of showing a blank or a made-up number.

## The Claude Code hook, in detail

Claude Code lets you set a *statusline*: a command it runs every time it redraws the line of information at the bottom of its screen. It sends that command a block of text that includes your current limits. Cooldown's hook is such a command.

When you press **Install hook** (in **Settings**), Cooldown:

1. Makes a backup copy of your Claude settings file, `~/.claude/settings.json`.
2. Remembers the statusline you had before, if any.
3. Points the `statusLine` setting at the hook program that comes with Cooldown.

Every time Claude Code runs the hook, it:

1. Reads what Claude Code sends.
2. Saves **only** this to a small file on your PC: the time, the two limits (percent used and reset time), Claude Code's version number, and, for organisations that use a gateway, a spend limit. It stores no file paths, no session IDs, no account names, no model names and no costs.
3. Runs your old statusline, with the same input, and shows its output. If you had none, it shows a short line such as `5h 42% · 7d 18%`. So your status line looks the same as before.

The hook makes **no network calls**. It does not read your Claude login or any credential file. **Uninstall hook** puts your old setting back.

## What bells do

A *bell* is an alert for one limit. When you choose **Notify me when it resets**:

- Cooldown remembers the time the limit resets.
- A timer on the PC checks every 15 seconds. When the time comes, you get a Windows notification such as **Claude 5-hour limit is back**.
- Reset times can move, for example when a limit is reset early by hand. So Cooldown re-checks the time shortly before it rings and adjusts.
- A paired phone also sets its own Android alarm for the same time. That is why the phone still rings if the PC is asleep.

A bell only works on the PC while Cooldown is running. It lives in the tray, so closing the window does not stop it.

## What is stored, and where

| What | Where | Notes |
|---|---|---|
| **API keys** (OpenRouter, DeepSeek, xAI) | The PC's **Windows Credential Manager** | Never in a settings file, never synced to a phone, never in the log. Sent only to the provider they belong to. |
| **The PC's pairing key** | Windows Credential Manager | Identifies this PC to your phones. |
| **Readings and history** | A small SQLite database on the PC, in `%LOCALAPPDATA%\app.cooldown.desktop` | History is pruned to 90 days. |
| **Settings, and the list of paired phones** | Files in the same folder (`settings.json`, `trust.json`) | Contain nothing secret. The phone list holds public information about each phone. They are in the *Local* app data folder on purpose, so a managed Windows setup that syncs your roaming profile does not copy them to other PCs. |
| **The Claude hook's file** | `claude-statusline.json` in a folder on the PC | The latest limits, as described above. |
| **Warnings and errors** | A log file on the PC | See [Where your data and logs live](install-windows.md#where-your-data-and-logs-live). Kept local. |
| **On the phone: the pairing key** | In the app's private storage, locked with the Android Keystore | Left out of Android backups on purpose. |
| **On the phone: the latest readings** | In the app's private storage | The latest sync only. The phone keeps no history and no API keys. The widget reads a copy of these readings from the same private storage. Unpairing removes them. |

## What crosses the network

### From the PC to the providers

- Only for OpenRouter, DeepSeek and xAI, and only if you saved a key. The PC sends a request to that provider's own web address with your key, over HTTPS. The key is sent to that provider and nowhere else. Redirects to a different site are refused. Answers larger than 2 MB are refused.
- Cooldown itself sends nothing to Anthropic or OpenAI. Note that the `codex` program, which Cooldown asks for your Codex limits, uses **its own login and its own connection** to do its job. That is Codex acting as it normally does, not Cooldown.

### Between the PC and the phone

- **Direct, over your own network.** The phone connects to the PC. No server of ours is in between, and there is no way to connect over the internet.
- **Encrypted.** Everything is wrapped in [Noise](https://noiseprotocol.org), a well-known encryption design. The first pairing uses `Noise_IKpsk1_25519_ChaChaPoly_BLAKE2s` and later reconnects use `Noise_IK_25519_ChaChaPoly_BLAKE2s`. The code comes from the open-source `snow` library. It has not been independently audited.
- **Only cleaned-up readings.** The PC builds the message from an allowlist: for each limit, its provider, a label like `default`, the kind of window, percent or balance, reset time, when it was observed, the source label, the status and a short note, plus your bells' times. Anything that looks like a key or token is replaced before it is sent. An API key can not be in the message, because the code that builds it never sees keys.
- **The phone asks, the PC answers.** The phone always starts the connection. The phone does not listen for anyone. It connects when you open the app, and a background job (Android's WorkManager) connects about every 15 minutes while a PC is paired and the phone has a network. Each run takes the newest readings, sets the alarms again, refreshes the widget and disconnects; nothing stays running between runs.
- **Finding the PC again.** If your router gives the PC a new address, the phone looks for it with a tiny multicast message (32 bytes, on your local network only). It contains no name, no ID and no key. It only says "a Cooldown device is at this address and port", and it carries a tag made with a secret that only your paired devices know. The phone still has to prove who it is with the encrypted handshake before anything is shared.
- **Private networks only.** On Windows, Cooldown listens only on networks Windows has marked **Private** (or on a domain), never on **Public** ones such as cafes. It does not listen at all until you open **Pair** or have a paired phone.

### Pairing, in plain words

1. You open **Pair** on the PC. The PC makes a one-time secret and shows it as a QR code. The code also holds the PC's public key and its address on your network. It works for about two minutes and only once.
2. The phone scans it. Because the phone now has the PC's public key and the secret, only the real PC can talk to it, and only this phone can start a pairing.
3. Both devices show **six digits**. You compare them and confirm on the PC. This catches the rare case where someone photographed your QR code and raced you.
4. The PC remembers the phone's public key. Until you revoke it, that phone can reconnect.
5. Revoking a phone (**Settings** > **Paired devices** > **Revoke**) deletes its key on the PC and tells the phone, if it is connected.

A PC can pair with up to 8 phones.

### What Cooldown does not send

No analytics, no crash reports, no usage statistics, no update check, and nothing to any server run by the maintainers. Cooldown has no telemetry. This is a rule of the project (see [CONTRIBUTING.md](../CONTRIBUTING.md#project-principles)), and because the code is open, you can check it.

## What Cooldown never touches

- Your Claude or Codex **login**: no OAuth tokens, no `~/.claude/.credentials.json`, no `~/.codex/auth.json`.
- Your **browser cookies**.
- Anyone's **private usage endpoints** (for example `/api/oauth/usage`).
- Windows Credential Manager entries that are not Cooldown's own.
- Files outside the few listed on this page. The Codex fallback opens only Codex's own `rollout-*.jsonl` session logs, only reads the end of each, and never follows shortcuts to other places.

## Honest limits

- The pairing and sync code has **not had an independent security review** yet. See [SECURITY.md](../SECURITY.md).
- The Windows installer is **not code-signed**. Windows will warn you.
- The Android app has only been tested on an emulator so far, not on many real phones. In particular, how often each maker's battery manager lets the 15-minute background sync run is unproven.
- Claude numbers depend on Claude Code running. Codex numbers depend on an undocumented interface.
- The phone must be on the same network as the PC. See [Install on Android](install-android.md#what-it-cannot-do-yet).

## Credits

Cooldown reuses ideas and code from other open-source projects. Each one is credited, with its license, in [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).
