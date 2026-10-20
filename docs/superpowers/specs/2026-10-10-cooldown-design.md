# Cooldown — design spec (v1)

Status: working draft, decisions made 2026-10-10. The project was renamed from the working name Resetbell to Cooldown on 2026-10-19.

> **This is the original design, not a description of the shipped app.** Where it disagrees with the README, the guides in `docs/` or `docs/pairing-protocol.md`, those win. Planned here but **not built**: the local browser dashboard on `127.0.0.1` (sections 3, 6 and 8) and the widget/dashboard catalog. Changed since: the Android widget lets the user pick any windows (not up to three) and scrolls, the phone syncs in the background about every 15 minutes, and the xAI team id is optional.

## 1. What it is

An open-source, local-first app that shows how much of your AI subscription and API limits you have used and exactly when they reset, then tells you the moment you can work again. It runs on your own machines. There is no hosted service, no account and no maintainer-run server.

v1 targets **Windows desktop + Android**. The code is cross-platform, so macOS and Linux builds come nearly free, but they are not promised or polished in v1.

## 2. Goals and non-goals

Goals
- Show current session (5-hour) and weekly limits, percent used, exact reset time, for Claude Code and Codex users, plus balance and spend for API-key providers.
- A "Notify me when my limit resets" button that fires a reliable alert on the PC and on a paired Android phone, with no server involved.
- Every figure is labelled with where it came from (official / estimated / scraped / manual) and how old it is.
- Keys and tokens never leave the device they were entered on. Only sanitized snapshots sync.
- One shared UI codebase for the Windows window and the Android app. (The original plan also had a local browser dashboard. It is **not implemented**; see section 6.)

Non-goals for v1
- iOS, hosted web app, accounts, cloud sync, proxying API traffic.
- A widget/dashboard catalog (designed for, not built).
- Reading subscription OAuth tokens, scraping claude.ai cookies, or any use of the undocumented `/api/oauth/usage` endpoint.
- Per-model Claude windows (the official statusline does not expose them; shown as "not available", never guessed).

## 3. Architecture

```
 Claude Code ─┐
 Codex ───────┼─► Source adapters ─► Normalizer ─► Local store (SQLite)
 API keys ────┘   (Rust, Windows)        │                 │
                                          ▼                 ├─► Dashboard UI (app window; a 127.0.0.1 browser tab was planned, not built)
                                     Snapshot JSON          ├─► Notifier (tray timer + Windows toast)
                                     (versioned)            └─► Sync server ──(QR-paired, Noise-encrypted)──► Android app
                                                                                                                ├─ Dashboard (same UI)
                                                                                                                ├─ Exact alarm: "limit is back"
                                                                                                                └─ Home-screen widget
```

Stack: Tauri 2 (Rust backend) + React + TypeScript + Tailwind v4 + shadcn/ui. Android uses Tauri 2 mobile with the same UI and small native Kotlin pieces (alarm, widget, QR scan).

Monorepo layout (npm workspaces):
- `packages/snapshot-schema` — snapshot types, JSON Schema, fixtures, validators. The contract between everything.
- `packages/ui` — shared React dashboard.
- `apps/desktop` — Tauri 2 app (Rust adapters, store, notifier, local + sync servers).
- `apps/android` — Android shell and native pieces (added after the spike).

## 4. The Snapshot (the seam between everything)

```ts
type SourceKind = 'official' | 'estimated' | 'scraped' | 'manual';
type WindowKind = 'session_5h' | 'weekly' | 'monthly' | 'daily' | 'balance';
type Status = 'ok' | 'stale' | 'degraded' | 'unavailable';

interface Snapshot {
  schemaVersion: 1;
  id: string;              // `${provider}:${account}:${window}` — stable, used for alert IDs
  provider: string;        // 'claude' | 'codex' | 'openrouter' | 'deepseek' | 'xai' | ...
  account: string;         // local label chosen by the user; never a token or email
  window: WindowKind;
  unit: 'percent' | 'usd' | 'cny' | 'tokens' | 'requests'; // 'cny' added 2026-10-19 for DeepSeek
  percentUsed?: number;    // 0..100
  used?: number; limit?: number; remaining?: number;
  resetsAt?: string;       // ISO-8601 UTC
  observedAt: string;      // ISO-8601 UTC — when the adapter last saw it
  source: { kind: SourceKind; adapter: string; adapterVersion: string };
  status: Status;
  note?: string;           // human-readable reason when not ok
}

interface SyncPayload {
  schemaVersion: 1;
  deviceId: string;
  generatedAt: string;
  snapshots: Snapshot[];
  alerts: { id: string; fireAt: string; enabled: boolean }[]; // id = `reset:${provider}:${account}:${window}`
}
```

Rules: a snapshot whose `resetsAt` is in the past is treated as `stale` by every consumer. Adapters never throw into the UI; failure produces `degraded`/`unavailable` snapshots that keep the last good value.

## 5. Source adapters (v1)

| Adapter | Method | Rating | Notes |
|---|---|---|---|
| Claude Code | Opt-in statusline hook writes `rate_limits.five_hour` / `seven_day` to an inbox file | official | Only fresh while Claude Code runs. Installer backs up and chains the user's existing statusline; one-click uninstall. |
| Codex | `codex app-server` rate-limits call, falling back to session logs | scraped/estimated | Undocumented/experimental; adapter is versioned and may degrade. |
| OpenRouter | `GET /api/v1/key` | official | Plain key. |
| DeepSeek | `GET /user/balance` | official | Plain key (auth type to confirm in testing). |
| xAI | Management API prepaid balance | official | Needs a management key. The team id is found from the key; only an organization-scoped key needs it configured. |
| Later | OpenAI/Anthropic org admin keys, Copilot, Gemini CLI, Cursor | — | Out of v1 scope. |

API keys live in the OS credential store (Windows Credential Manager via the `keyring` crate), never in a config file, never synced. Adapters are `trait Adapter { fn poll() -> Vec<Snapshot> }` with per-adapter polling intervals and backoff.

## 6. Sync and pairing (decision: direct, no third party)

- The Windows app runs a small server. First run triggers the Windows Firewall prompt; the server binds to private-network interfaces only.
- Pairing: the PC shows a QR code containing its LAN address(es), port, its static X25519 public key and a one-time pairing secret. The phone scans it and completes a Noise handshake. **The full protocol and threat model are in `docs/pairing-protocol.md` and supersede this section where they differ**: first pairing uses `Noise_IKpsk1_25519_ChaChaPoly_BLAKE2s` (the QR already gives the phone the PC's key, so the PC never reveals its key to an unauthenticated connection), reconnects use `Noise_IK` with a monotonic timestamp, pairing has a 120 s single-use window with a 6-digit confirmation code, sync is length-prefixed TCP, and discovery uses a private beacon port. Crypto is one shared Rust crate used by both Tauri apps.
- After pairing, the phone reconnects automatically whenever it can reach the PC and keeps its last snapshot and scheduled alerts otherwise. Tailscale should work as just another address (untested).
- Only `SyncPayload` crosses the wire. No keys, tokens or cookies, ever.
- Paired devices are listed in Settings and can be revoked (their static key is deleted).
- **Not implemented (future):** a local browser dashboard as a separate listener on `127.0.0.1` only, with a per-launch token and strict `Host` header checks (DNS-rebinding defence). The shipped app has no HTTP listener at all: it listens only on the Noise sync port (TCP) and the discovery beacon (UDP) for paired phones, and only while pairing is open or a phone is paired.
- **This design needs a security review (Opus) before the repo is published.**

## 7. Reset alerts

- One alert per limit window with a stable ID `reset:<provider>:<account>:<window>`; reconcile by replacing, never by stacking.
- Desired state = the latest known `resetsAt`. Re-read the value shortly before firing, because resets can move (manual resets, saved resets). Debounce shifts under about 60 seconds. Cancel if the reset moves away or usage resumes.
- Windows: resident tray app with launch-at-login and its own timer (the Tauri notification plugin cannot schedule on desktop), toast on fire.
- Android: native Kotlin exact alarm. Needs the exact-alarm permission on Android 14+, which the app requests with a settings deep link; falls back to an inexact alarm and says so in the UI. Boot receiver restores alarms.
- Optional later: ntfy with the `Delay` header, and a calendar `.ics` fallback.
- Threshold alerts (for example 80% used) need live data, so they run only while a collector is polling.

## 8. Dashboard and widgets

- Mobile-first (390px) and desktop (1440px) from one codebase; light and dark themes; `prefers-reduced-motion` respected.
- v1 screens: Overview (provider cards with meter, countdown, source label, age), Provider detail (history chart), Alerts, Settings (adapters, keys, paired devices, Claude Code hook install/uninstall), Pairing.
- Android home-screen widget (v1): one generic native Glance widget reading the synced `snapshots.json`. As built, it shows every window by default and scrolls; the user can pick any subset (at least one) in the phone's Settings or on the configuration screen shown when the widget is added. (The original plan was "up to 3 limit IDs".) Shows an "as of" timestamp because widget refresh is budgeted. The catalog (declarative JSON manifest, built-in renderers, optional sandboxed-iframe community widgets, GitHub-hosted registry) is designed for but not built in v1.

## 9. Errors and testing

- Failure never blanks the UI: last good value is kept and marked `stale`/`degraded` with a reason.
- Fixtures: raw provider payloads and the exact snapshots they must produce, shared by the TypeScript validators and the Rust adapter tests.
- Rust unit tests for adapters, normalizer, alert reconciliation and the Noise handshake; vitest for schema and UI logic; Playwright screenshots at 390px and 1440px as the UI gate; a manual checklist on a real Android phone for alarms, widget and pairing.

## 10. Android spike (before building the Android app)

Question: can Tauri 2 mobile on a real Android phone (a) schedule an exact alarm that fires with the app closed, (b) host a Glance widget reading app-written JSON, (c) open a LAN connection and complete a Noise handshake with the desktop?
Pass: all three work. Fail on any: build the phone app as native Kotlin/Compose against the same Snapshot contract (fallback approach 2); the desktop work is unaffected.

## 11. Phasing

1. Foundation: this spec, snapshot-schema, fixtures, UI on sample data (no Rust needed).
2. Windows app: Rust adapters, store, notifier, local server (needs Rust toolchain).
3. Pairing and sync, then security review.
4. Android spike, then Android app with alarms and widget.
5. Packaging, signing decisions, docs, public release.
6. Later: more providers, widget catalog, optional relay, iOS.

## 12. Risks and open items

- Anthropic's terms restrict handling subscription credentials; v1 uses only the official statusline and API keys.
- Statusline data is only fresh while Claude Code runs; the UI says so.
- Tauri on Android is younger; the spike and the Kotlin fallback contain the risk.
- Unverified from research: ntfy browser header behaviour, DeepSeek auth type, Android alarm reliability on aggressive battery managers, Homebrew's policy on unsigned casks.
- Distribution: Windows builds are unsigned unless a certificate is bought (SmartScreen warning); Android via GitHub APK and F-Droid; no Apple account needed for v1.
- Toolchain needed on the build machine: Rust (rustup), and for Android a JDK 17+, Android SDK and NDK. Windows installers are built in GitHub Actions because this development machine is a Mac.

## 13. Reuse plan (decided 2026-10-10 after reading the actual code and LICENSE files)

All candidates below are MIT or Apache-2.0, so Cooldown stays MIT. Every ported file keeps its copyright notice in `THIRD_PARTY_NOTICES`; Apache-2.0 material keeps its license text and NOTICE. Nothing is GPL/AGPL. No project is a drop-in base, so we port small, well-tested pieces.

| Layer | Base / source | License | How we reuse it |
|---|---|---|---|
| Claude statusline hook | morsechimwai/claude-status-line (`ccstatus`) | MIT | Port `src/input.rs` (tolerant parse) and the `resolve_window` logic into a tiny `cooldown-statusline` Rust binary. Fix: atomic write-temp-then-rename, `dirs::cache_dir`, chain to the user's saved statusline. |
| Codex adapter | jamesbrink/burnrate (`providers/codex.rs`) | MIT | Port the tokio client for `codex app-server --listen stdio://` (`account/rateLimits/read`). Add Windows resolution (`codex.cmd`/`.ps1`/`.exe`/`where.exe`), `CREATE_NO_WINDOW`, assign windows by duration not slot, rollout-log `rate_limits` tail parser as fallback. Fixtures from openai/codex JSON schema (Apache-2.0 + NOTICE). |
| API-key adapters | akitaonrails/ai-usagebar (OpenRouter, DeepSeek, xAI) | MIT | Port the three `fetch.rs`/`types.rs` with their httpmock tests, the 2 MiB body cap, same-origin redirect policy and env-secret scrubbing. DeepSeek auth is Bearer (verified). |
| Adapter trait / registry | deviffyy/OpenQuota | MIT | Model on `providers/mod.rs` and `registry.rs`; use the `keyring` crate for secrets. |
| Android widget | T0mSIlver/starbridge `android/` (Glance) | MIT | Adapt `widget/Widgets.kt` (~400 lines), `QuotasWidgetSetup.kt`, `widget_quotas.xml`; replace its Hilt store with a read of `snapshots.json`. Drop its relay, push, FCM and `protocol/*`. |
| Android alarm | tauri-apps/plugins-workspace notification plugin | Apache-2.0 OR MIT | Lift exact-alarm scheduling, boot-restore receiver and storage; add `SCHEDULE_EXACT_ALARM`, `canScheduleExactAlarms()`, the request deep link, and `FLAG_IMMUTABLE`. |
| Noise crypto | mcginty/snow | Apache-2.0 OR MIT | Dependency (not formally audited, so the security review is mandatory). |
| LAN discovery | localsend/localsend `packages/core` | Apache-2.0 | Adopt the multicast announce pattern and certificate-fingerprint identity so the phone can re-find the PC when its IP changes. |
| Pairing UX/state machine | KDE Connect, Syncthing | GPL / MPL | Design reference only; no code copied. |
| UI | Watermelon registry (Capitalio shell, Credit Usage Card, Budget Card) + shadcn Progress, Sonner, Recharts, qrcode.react | MIT / registry | Install via `npx shadcn@latest add`; hand-build only the mobile bottom nav. |

Explicitly not reused: anything that reads Claude/Codex OAuth tokens, `.credentials.json`/`auth.json`, the keychain or browser cookies, or calls `/api/oauth/usage` / `wham/usage` (CodeZeno, tokscale, OpenQuota auth, burnrate Claude provider, ai-usagebar anthropic/openai, claude-pulse-android, utaysi, TokenGrass, CodexBar web/cookie paths). Also skipped: tokscale (scope), ccusage (no limit data; later "estimated" source only), s00d widgets plugin (single author), KDE Connect and teahouse (GPL/AGPL).

Extra design rules learned from the code review:
- Claude Code drops a window once its `resets_at` passes, so a missing window after a known reset means "reset happened", not "unknown".
- Codex quota reads can glitch; alert reconciliation requires two consistent reads before firing or cancelling.
- Android 17 gates all LAN traffic behind the runtime permission `ACCESS_LOCAL_NETWORK`; pairing must request it, with a settings fallback.
- The Tauri-generated Android project pins Kotlin 1.9.25; the Android spike must first prove Glance builds against it (otherwise bump Kotlin or use the native fallback built from Starbridge's `android/`).
