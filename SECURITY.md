# Security policy

Cooldown handles things people care about: the way two of their devices talk to each other, the API keys they paste in, and a change to their Claude Code settings. We take reports about these seriously.

## Reporting a vulnerability

**Please report security problems privately. Do not open a public issue, discussion or pull request for them.**

1. Go to the [Security tab of the Cooldown repository](../../security).
2. Choose **Report a vulnerability**. (Direct link: [new private report](../../security/advisories/new).)
3. Describe the problem. Only the maintainers can read what you send.

Please include:

- what you found, and what an attacker could do with it,
- the Cooldown version (the number in the installer file name, or in the log file on the line that starts `Cooldown ... starting`) and your Windows or Android version,
- the steps to repeat it, or a small proof of concept,
- anything you think would fix it.

Please do not include real API keys or other secrets in a report. If your proof needs a key, use a fake one.

### What to expect

Cooldown is a volunteer project, so we cannot promise a fixed timeline. We will try to:

- reply within about a week,
- tell you whether we agree it is a vulnerability,
- fix it, and tell you when the fix is released,
- credit you in the release notes if you want that.

Please give us a reasonable chance to fix a problem before you tell anyone else about it.

### Please do not

- test against devices or networks that are not yours,
- read, change or delete other people's data,
- run attacks that could take a network down.

## Supported versions

Only the **latest release** gets security fixes. Cooldown is young and changes quickly, so please update before you report a problem.

| Version | Supported |
|---|---|
| Latest release on the [Releases page](../../releases) | Yes |
| Anything older, and unreleased builds from the `main` branch | No |

## What is in scope

These parts of Cooldown matter most to us:

| Area | Examples |
|---|---|
| **Pairing and sync protocol** | Getting a phone paired without the user's confirmation. Reading or changing the data between the PC and the phone. Replaying or downgrading a connection. Making the PC listen on a network it should not. Crashing or flooding the sync listener. See [docs/pairing-protocol.md](docs/pairing-protocol.md) for the design and the threat model. |
| **Key storage** | An API key, the PC's pairing identity key or the phone's key ending up in a file, a log, a backup, a notification, the screen or the network. The phone key surviving in an Android backup. |
| **What is sent to a phone** | Anything other than sanitized readings (percentages, balances, reset times, labels) crossing the network. |
| **The Claude Code hook install** | The installer or uninstaller damaging, leaking or wrongly rewriting `~/.claude/settings.json`. The hook running something it should not. |
| **Reading other programs** | Cooldown reading a credential file, token or cookie of Claude, Codex or a browser, or calling a private usage endpoint. This is against our rules even if nothing bad happens. |
| **The Android app** | Exported components or intents that another app can abuse. Leaking the pairing code. |
| **Releases** | A published installer or APK that does not match its `SHA256SUMS.txt`, or a way to get malicious code into the release process. |

## What is out of scope

- **A program that already runs as you on your PC, or a rooted phone.** It can already read your credential store. Cooldown does not claim to defend against that.
- **Someone with physical access to your unlocked PC or phone.**
- **Someone on your Wi-Fi who can only stop the connection** (for example by dropping packets). Cooldown accepts that a network attacker can cause a denial of service.
- **The SmartScreen warning on the Windows installer.** The installer is not code-signed. This is known and explained in [docs/install-windows.md](docs/install-windows.md).
- **Problems in other products**: Claude Code, Codex, Windows, Android, or the providers' own services. Please report those to their makers.
- **Reports from automated scanners with no real impact**, and best-practice suggestions without a way to exploit them. They are still welcome as normal issues.

## Known limits

- The pairing and sync protocol, and the `snow` Noise library it uses, **have not been independently audited**. The design is documented in [docs/pairing-protocol.md](docs/pairing-protocol.md). Reviews from people who know cryptography are very welcome. You can send those as normal issues if they do not describe a way to attack a real user.
- Windows builds are unsigned.

## Safe use

- Download Cooldown only from the [Releases page](../../releases) of this repository, and check the file with `SHA256SUMS.txt`. See [docs/install-windows.md](docs/install-windows.md).
- Pair a phone only on a home or work network you trust. Do not pair on a public or shared network.
- Only confirm a pairing on the PC if the six digits match the ones on your phone.
