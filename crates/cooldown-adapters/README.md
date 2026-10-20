# cooldown-adapters

The source adapters of Cooldown: the code that turns "where does this limit live" into
[`Snapshot`](../cooldown-core/README.md)s. Each adapter implements `cooldown_core::Adapter`, and
`poll` never fails: a problem becomes a `degraded`, `stale` or `unavailable` snapshot that keeps the
last good value, so the UI never blanks.

| Adapter | id | Source | `source.kind` | Windows |
|---|---|---|---|---|
| `ClaudeInboxAdapter` | `claude-statusline` | the inbox file written by `cooldown-statusline` (Claude Code's official statusline `rate_limits`) | official | `session_5h`, `weekly` |
| `CodexAppServerAdapter` | `codex-app-server` | `codex app-server --listen stdio://` (JSON-RPC, undocumented) | scraped | `session_5h`, `weekly` |
| `CodexRolloutAdapter` | `codex-rollout-log` | tail of the newest `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` | scraped | `session_5h`, `weekly` |
| `OpenRouterAdapter` | `openrouter` | `GET https://openrouter.ai/api/v1/key` | official | `balance` (usd) |
| `DeepSeekAdapter` | `deepseek` | `GET https://api.deepseek.com/user/balance` (Bearer) | official | `balance` (usd) |
| `XaiAdapter` | `xai` | `management-api.x.ai`: key validation, then the team's prepaid balance | official | `balance` (usd) |

## The mapping rules are pure and shared

Every adapter module has `map_input(&serde_json::Value, &MapContext) -> Vec<Snapshot>`: no clock, no
disk, no network. `MapContext` is `{ now, account, adapterVersion, observedAt?, previous[], httpStatus? }`,
which is exactly a fixture's `<case>.context.json`.

`tests/fixtures.rs` runs **every** case in `packages/snapshot-schema/fixtures/adapters`
(36 cases: 9 Claude, 7 Codex app-server, 4 Codex rollout, 6 OpenRouter, 5 DeepSeek, 5 xAI) through those functions and
requires exactly the snapshots in `expected.json` (`36 of 36 fixtures pass`). The TypeScript suite reads
the same files, so the Rust adapters and the shared contract cannot drift. Where Rust and the
TypeScript reference disagree, the fixtures win.

The rules, in short (full text: `packages/snapshot-schema/fixtures/README.md`):

- **Claude**: `five_hour` is `session_5h`, `seven_day` is `weekly`. A reset time in the past is `stale`.
  Claude Code drops a window once it resets, so a window that is **missing after its known reset time
  means the reset happened** (`ok`, 0 percent, note `window reset`). `observedAt` is when the hook wrote
  the file, so the UI shows the true age. Missing file: `unavailable` with the note "Run Claude Code with
  the Cooldown hook installed". Unreadable or malformed file: `degraded`, last value kept.
- **Codex** (both paths): windows are assigned by **duration**, never by slot (`windowDurationMins` /
  `window_minutes` 300 is `session_5h`, 10080 is `weekly`; the weekly window can arrive in `primary`).
  Other durations and other limit buckets (Spark) are ignored. Reset times in milliseconds are tolerated.
- **OpenRouter**: with a key limit: `used`, `limit`, `remaining` (can go negative), `percentUsed` capped at 100.
  Uncapped key: `used` only, with a note.
- **DeepSeek**: the single `USD` balance, or the single `CNY` balance (unit `cny`) when there is no USD one; two entries of the chosen currency is schema drift.
- **xAI**: `remaining = -total.val / 100` (inverted cents ledger); a negative result is kept with a note.
- Non-2xx HTTP is `request failed (HTTP <status>)`; a body that does not match is `response did not match the expected shape`.
  Notes never contain provider response text (the one exception, the 200-character Codex JSON-RPC error message, is scrubbed).

## Configuration

Each adapter has a small, serde-friendly config struct (`camelCase`, every field has a default, nothing
secret). `*_secs` fields are seconds.

```rust
use std::sync::Arc;
use cooldown_adapters::*;

let claude = ClaudeInboxAdapter::new(ClaudeInboxConfig::default())?;
// default inbox: dirs::data_local_dir()/cooldown/inbox/claude-statusline.json
// (%LOCALAPPDATA%\cooldown\inbox\... on Windows; COOLDOWN_INBOX_DIR overrides the directory, as in the hook)

let codex = CodexAppServerAdapter::new(CodexAppServerConfig::default())?;
let codex_fallback = CodexRolloutAdapter::new(CodexRolloutConfig::default())?;

let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore::new());
let openrouter = OpenRouterAdapter::new(OpenRouterConfig::default(), secrets.clone())?;
let deepseek = DeepSeekAdapter::new(DeepSeekConfig::default(), secrets.clone())?;
let xai = XaiAdapter::new(XaiConfig { team_id: None, ..Default::default() }, secrets)?;
```

| Config | Field | Default | Meaning |
|---|---|---|---|
| all | `account` | `default` | local label, part of every snapshot id (no `@`, no `:`, never a token) |
| all | `pollIntervalSecs` | Claude 30, rollout 60, app-server 120, OpenRouter/DeepSeek 300, xAI 600 | |
| Claude | `inboxPath` | see above | |
| Codex app-server | `binary` | search for `codex` | a path or command name |
| | `codexHome` | none | passed to the child as `CODEX_HOME` (never read by Cooldown) |
| | `timeoutSecs` | 20 | whole handshake and call |
| Codex rollout | `sessionsDir` | `$CODEX_HOME/sessions` or `~/.codex/sessions` | |
| | `tailBytes`, `maxFiles` | 512 KiB, 5 | read cap per log, logs tried per poll |
| API adapters | `baseUrl` | the production host | `https://` only; plain `http://` only to loopback (tests) |
| | `requestTimeoutSecs` | 15 | per request, connect to last byte |
| | `useSystemProxy` | true | |
| xAI | `teamId` | from the key | needed for organization-scoped management keys |

Constructors return `ConfigError` for a bad account label, a non-HTTPS base URL, a bad team id or a
configured path that is a credential file. Nothing fails later at poll time.

## How secrets work

API keys (OpenRouter key, DeepSeek key, xAI **management** key) live only in a `SecretStore`:

- `KeyringSecretStore`: the OS credential store through the `keyring` crate (Windows Credential
  Manager, macOS Keychain, Secret Service), service name `cooldown`. Only Cooldown's own entries are touched.
- `InMemorySecretStore`: a map, for tests.

The secret is stored under `secret_name(adapter_id, account)`, for example `openrouter:default`;
`adapter.secret_name()` tells the Settings screen which name to save a key under. The key is read from the
store **on every poll** (so a new key takes effect immediately), held in a `SecretString` (`Debug` prints
`[redacted]`, no `Display`, no `Serialize`), checked to be a plain visible-ASCII token so it can never
break out of a header, sent only as `Authorization: Bearer ...`, and dropped. It is never written to
configuration, never logged (the crate has no logging), never placed in a snapshot note and never synced.

## Safety rules

- **No credential is read, ever**: not `~/.claude/.credentials.json`, not `~/.codex/auth.json`, no
  browser cookies, no OS keychain entry that is not Cooldown's own, and no `/api/oauth/usage` or
  `wham/usage` call. The Claude adapter reads only the file Cooldown's own hook wrote (and refuses a
  configured path named like a credential file). The Codex rollout adapter opens only regular files named
  `rollout-*.jsonl` inside numeric `YYYY/MM/DD` directories, never follows symbolic links, and reads at
  most `tailBytes` from the end of a file. The Codex app-server uses Codex's own login over a pipe.
- **Codex process hygiene**: the child is started with `kill_on_drop`, is always killed and reaped (also
  on timeout and when the poll is cancelled), gets `CREATE_NO_WINDOW` on Windows, and has
  `OPENAI_API_KEY`, `CODEX_API_KEY`, `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN` and
  `CLAUDE_CODE_OAUTH_TOKEN` removed so an inherited variable cannot switch it to API-key auth.
  Output is capped at 2 MiB and noise lines are bounded.
- **HTTP**: rustls only; bodies capped at 2 MiB (checked by `Content-Length` and while streaming);
  redirects only to the same scheme, host and port (so the key can never be forwarded elsewhere or
  downgraded to HTTP); a timeout on every request; errors are classified (`timed out`, `could not
  connect`, `response too large`, ...) and their text never includes the URL, the key or anything the
  server said. `scrub` additionally removes known secrets and anything credential-shaped from text that
  does travel.
- **Windows launcher resolution** (`codex_launcher`, a pure function with an injected `PATH`/`PATHEXT`
  list): `codex.cmd`, `codex.ps1`, `codex.exe` in every `PATH` directory, then `where.exe codex`. A `.ps1`
  runs through `powershell.exe -NoProfile -ExecutionPolicy Bypass -File`; a `.cmd` shim is started
  directly (Rust runs batch files through `cmd.exe` with safe quoting). The extensionless `codex` npm
  also installs is a shell script and is ignored on Windows.

## Tests

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p cooldown-adapters
cargo clippy -p cooldown-adapters --all-targets -- -D warnings
cargo fmt -p cooldown-adapters -- --check
```

No test touches the network, the real Codex or Claude, a credential file or the OS keychain. They use
temporary directories, `httpmock` on a loopback port, an in-memory secret store and a fake `codex`
(an `sh` script on Unix; on Windows a `.cmd` shim in `tests/codex_app_server.rs` that only compiles
there).

## Not yet verified

Nothing has been built or run on Windows. The Windows-specific pieces (`CREATE_NO_WINDOW`, the `where.exe`
fallback, the `.cmd`/`.ps1` launchers, the Windows `.cmd` fake in the test) are small, behind `cfg`, and the
decisions they make are pure functions tested on every OS, but the process spawning itself needs a run
on a real Windows machine. Killing a `.cmd` shim kills `cmd.exe` and may leave the `node` process it started;
a Windows Job Object would fix that and is not implemented. The `KeyringSecretStore` has never talked to a
real credential store in tests (by design); only its error mapping is tested.

## License

MIT. Ported ideas and their licenses are listed in the repository's `THIRD_PARTY_NOTICES.md`.
