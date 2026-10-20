# cooldown-statusline

The Claude Code half of Cooldown's data path. Claude Code can run a user-configured
`statusLine` command on every update and pipes it JSON on stdin that includes
`rate_limits.five_hour` and `rate_limits.seven_day` (`used_percentage` 0 to 100,
`resets_at` unix seconds). That is the official, terms-of-service-safe source of
subscription limits: no OAuth token, no cookie, no undocumented endpoint.

This crate builds one small binary, `cooldown-statusline`, plus a library with the same logic.

- **Hook mode** (no arguments): the command Claude Code runs. It writes the limits to an
  inbox file for the Cooldown app, then shows your previous status line (or a short default).
- **`install` / `uninstall` / `status`**: manage the `statusLine` entry in Claude's settings
  file safely.

Typical run time is about 3 ms.

## The inbox file (the contract with the app's adapter)

Path: `<inbox dir>/claude-statusline.json`

Inbox dir: `%LOCALAPPDATA%\cooldown\inbox` on Windows (`dirs::data_local_dir()/cooldown/inbox`
elsewhere). The environment variable `COOLDOWN_INBOX_DIR` replaces the whole directory.

```json
{
  "version": 1,
  "observedAt": "2026-10-10T12:34:56Z",
  "fiveHour": { "usedPercentage": 23.5, "resetsAt": "2026-10-10T16:00:00Z" },
  "sevenDay": { "usedPercentage": 41.2, "resetsAt": "2026-10-14T16:00:00Z" },
  "spendLimit": null,
  "claudeCodeVersion": "2.1.90"
}
```

| Field | Type | Meaning |
|---|---|---|
| `version` | integer | Schema version. Always `1` today. Reject higher values you do not understand. |
| `observedAt` | RFC 3339 UTC string, whole seconds | When the hook received this data. |
| `fiveHour`, `sevenDay` | object or `null` | The two subscription windows. `null` when Claude Code did not report that window. |
| `fiveHour.usedPercentage` | number or `null` | Clamped to 0..100. |
| `fiveHour.resetsAt` | RFC 3339 UTC string or `null` | When the window resets. |
| `spendLimit` | object or `null` | Gateway-only (Claude apps gateway) spend limit; `null` for normal subscribers. |
| `spendLimit.usedPercentage` | number or `null` | 0 or more. **Can exceed 100** once the limit is passed. |
| `spendLimit.resetsAt` | RFC 3339 UTC string or `null` | When the spend period resets. |
| `spendLimit.usedUsd`, `spendLimit.limitUsd` | number or `null` | Estimated spend and limit in USD. Often absent. |
| `spendLimit.period` | `"daily"`, `"weekly"`, `"monthly"` or `null` | The limit's period. |
| `claudeCodeVersion` | string or `null` | Claude Code's `version`, only if it looks like a version string. |

Every key is always present; "absent" is always an explicit `null`. Within a window either
field can be `null` on its own, and a window is `null` outright when both would be.

Nothing else is stored: no paths, session ids, account names, model names or cost fields.

### How the adapter should read it

- **Freshness.** The data is only as fresh as the last Claude Code status update. Show
  `observedAt` as the age. It is a statement about when Claude Code last spoke, not about
  when the limit last changed.
- **The file is not rewritten when Claude Code sends no `rate_limits` object at all.** That
  happens at session start before the first API response, and for API-key users. Rewriting
  then would erase the last good reading, so the previous file stays and `observedAt` keeps
  its old value.
- **A past `resetsAt` means the window has reset.** Claude Code drops a window once its
  `resets_at` passes, so a `null` window in a file that carries `rate_limits` after a known
  reset also means "reset happened", not "unknown". Treat a window whose `resetsAt` is in
  the past as stale (spec section 4).
- **Replaced atomically.** The hook writes `.claude-statusline.json.<pid>.<n>.<ns>.tmp` in the
  same directory and renames it over the file, so a reader never sees a partial file. Watch
  the directory (rename events), ignore the `.tmp` files, and do not hold the file open
  exclusively. Orphaned temp files from cancelled runs are swept after 60 seconds.
- **Unknown fields.** Ignore any you do not know; `version` is bumped only for breaking changes.

### Parsing tolerance

The hook never fails on odd input: unknown fields are ignored; `null`, missing or wrong-typed
values become `null`; numeric strings (`"42"`) are accepted; non-finite or out-of-range numbers
and unrepresentable timestamps become `null`; malformed, empty or oversized input (capped at
1 MiB, 1 second timeout) is treated as "no data". A UTF-8 BOM and trailing bytes are ignored.

## Hook mode and chaining

Order of work on every status update:

1. Read stdin (1 MiB cap, 1 s timeout; a terminal is treated as empty input) and parse it.
2. If the input has a `rate_limits` object, write the inbox file atomically. Errors and panics
   here are swallowed.
3. If `install` saved a previous statusline, run it with the **same stdin bytes**, print its
   stdout unchanged and **exit with its exit status**. Otherwise print a default line such as
   `5h 42% · 7d 18%` (`--` for a window that is not reported) and exit 0.

The previous command runs through `sh -c` on macOS and Linux, and on Windows through Git Bash
(found via `CLAUDE_CODE_GIT_BASH_PATH`, `git.exe` on `PATH`, or the standard install folders)
or, without Git Bash, `powershell -NoProfile -NonInteractive -Command`: the same rule Claude Code
uses. Its stderr is inherited. A chained command that cannot be started falls back to the default
line and exit 0, so the status line is never worse than before. The chained child gets
`COOLDOWN_STATUSLINE_DEPTH=1`; a nested run never chains again, and a saved command that is
Cooldown itself is never chained to.

## Install flow

```text
cooldown-statusline install                     # dry run: prints exactly what would change
cooldown-statusline install --yes               # applies it
cooldown-statusline status [--json]             # installed | not installed | modified
cooldown-statusline uninstall                   # dry run
cooldown-statusline uninstall --yes             # restores the previous statusLine
```

Options: `--settings <path>` (default `~/.claude/settings.json`), `--exe <path>` (the binary to
register, default this binary; its file name must be `cooldown-statusline`), `--shell
bash|powershell` (command syntax; default bash, or powershell on Windows without Git Bash).
Exit codes: 0 success, 1 failed or refused, 2 bad usage.

`install --yes` does, in order:

1. Refuses if the settings file is not valid JSON (or not a JSON object). Nothing is touched.
2. Copies the file byte for byte to `settings.json.cooldown-backup-<UTC timestamp>` (never
   overwrites an existing backup).
3. Saves your current `statusLine` object to the chain config,
   `%APPDATA%\cooldown\chain.json` (`dirs::config_dir()/cooldown/chain.json`; the environment
   variable `COOLDOWN_CONFIG_DIR` replaces the `cooldown` directory).
4. Sets `statusLine` to `{"type": "command", "command": "<this binary>"}`, keeping any other
   fields of your old entry (`padding`, `refreshInterval`, ...), and writes the file atomically.
   Every other key keeps its value and its position; if you had a `statusLine` it stays where it
   was.

On Windows the command uses forward slashes (Git Bash eats backslashes) and has the `\\?\`
prefix stripped. A path with spaces is single-quoted for Git Bash; for PowerShell it becomes
`& '<path>'`.

Running `install` again changes nothing. If the binary moved, it updates the command and keeps
the saved chain. If someone replaced the entry with their own command since, that command
becomes the new saved original.

`uninstall --yes` puts the saved `statusLine` back exactly (or removes the key if there was
none), then deletes the chain config. If the current `statusLine` is not Cooldown's, it
refuses and leaves the file alone, telling you what it found and what was saved.

### chain.json

```json
{
  "version": 1,
  "settingsPath": "/home/me/.claude/settings.json",
  "installedAt": "2026-10-10T12:00:00Z",
  "installedStatusLine": { "type": "command", "command": "/path/cooldown-statusline", "padding": 2 },
  "original": { "type": "command", "command": "~/.claude/statusline.sh", "padding": 2 }
}
```

`original` is `null` if you had no statusline before. The chain config belongs to one settings
file; install and uninstall refuse to use it for a different one.

## Safety guarantees

- The only files read or written are the settings file you point it at (default
  `~/.claude/settings.json`), its backups, `chain.json`, and the inbox. It never reads
  `~/.claude` credentials, `~/.codex`, the OS keychain or any token. It makes no network calls.
- `install` and `uninstall` are dry runs unless `--yes` is given.
- The settings file is backed up before every change and re-checked just before the write; if
  it changed in between (Claude Code or an editor wrote it), nothing is written.
- Writes are write-temp-then-rename in the same directory; a symlinked settings file stays a
  symlink and keeps its permissions.
- The hook cannot break your status line: every failure still prints a line, and the exit code is
  0 unless your own chained command returned something else.
- The settings file is re-serialised as pretty JSON (2-space indent) with key order preserved.
  Comments are not supported because Claude's settings are plain JSON.
- Tests use only temp directories and the path overrides; the end-to-end tests also point
  `HOME`, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA` and `XDG_*` at the sandbox.

## Development

```text
cargo test -p cooldown-statusline
cargo clippy -p cooldown-statusline --all-targets -- -D warnings
cargo fmt -p cooldown-statusline -- --check
cargo build -p cooldown-statusline --release
```

The Windows path, quoting and shell-selection logic are pure functions tested on every OS. The
`cmd /c` chaining tests only compile on Windows.

Derived in part from the MIT-licensed `ccstatus` project; see `THIRD_PARTY_NOTICES.md`.
