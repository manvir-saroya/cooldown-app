# Adapter fixtures

Raw provider payloads and the exact `Snapshot[]` a correct adapter must produce from them. They are
the shared test contract: the TypeScript suite in this package and the Rust adapter tests in
`apps/desktop` read the same files (design spec section 9).

Everything here is invented. There are no real tokens, emails, account ids or team ids; paths are
`/home/example/...`, ids are `00000000-...`.

## Layout

```
fixtures/adapters/<adapter>/<case>.input.json      raw provider payload
fixtures/adapters/<adapter>/<case>.context.json    the poll context
fixtures/adapters/<adapter>/<case>.expected.json   Snapshot[] the adapter must emit
```

Adapters: `claude-statusline`, `codex-app-server`, `codex-rollout-log`, `openrouter`, `deepseek`, `xai`.

### `input.json`

The provider payload exactly as the adapter receives it, decoded as JSON.

| Adapter | Input |
|---|---|
| `claude-statusline` | The JSON Claude Code writes to the statusline command's stdin ([docs](https://code.claude.com/docs/en/statusline)). `rate_limits.five_hour` / `seven_day` carry `used_percentage` (0-100) and `resets_at` (Unix seconds). |
| `codex-app-server` | The JSON-RPC response line to `account/rateLimits/read`: `{"id":2,"result":{...}}` or `{"id":2,"error":{"code":...,"message":...}}`. camelCase fields: `usedPercent`, `windowDurationMins`, `resetsAt` (Unix seconds), plus `rateLimitsByLimitId`. Shape from `codex-rs/app-server-protocol/schema/json/v2/GetAccountRateLimitsResponse.json`. |
| `codex-rollout-log` | One line of a `~/.codex/sessions/**/*.jsonl` rollout file: an `event_msg` whose `payload.type` is `token_count`, with `payload.rate_limits.{primary,secondary}.{used_percent,window_minutes,resets_at}`. Stored as a single-line JSON object. |
| `openrouter` | Response body of `GET /api/v1/key` (`{"data":{...}}` envelope). |
| `deepseek` | Response body of `GET /user/balance`. |
| `xai` | Response body of `GET /v1/billing/teams/{team}/prepaid/balance` (Management API). |

Error bodies for the HTTP adapters are illustrative: the adapter keys off `context.httpStatus`, not
the body text.

### `context.json`

```jsonc
{
  "now": "2026-10-10T12:00:00Z",   // the fixed clock for the poll (ISO-8601 UTC)
  "account": "default",            // local account label the adapter was configured with
  "adapterVersion": "0.1.0",       // goes into source.adapterVersion
  "observedAt": "...",             // optional: when the source produced the payload, if not `now`
  "previous": [ /* Snapshot[] */ ],// optional: the adapter's last emitted snapshots
  "httpStatus": 200                // optional: HTTP status, API-key adapters only
}
```

`previous` is the adapter's own last output (same ids). It is how an adapter keeps "the last good
value" and recognises a window that reset. TypeScript type and validator: `FixtureContext` /
`FixtureContextSchema` in `src/adapters.ts`.

### `expected.json`

A JSON array of `Snapshot` objects (`src/types.ts`), one per window the adapter always emits, in
the order of the table below. Each element validates against `schema/snapshot.schema.json`.

## How tests consume them

A fixture run is a pure function: `adapter(input, context) -> Snapshot[]`. No clock, no network,
no disk. For each `<case>`:

1. Parse `input`, `context`; call the adapter's mapping function with `context.now` as its clock.
2. Compare the result to `expected` with deep equality, **including `note`**. The array order and
   every key matter; floats may be compared with a 1e-9 tolerance; absent optional keys must be
   absent (not `null`). Note wording is normative (catalogue below); if a note changes, change the
   fixtures in the same commit.
3. Validate `expected` (and the adapter output) against `schema/snapshot.schema.json`.

Rust sketch:

```rust
let input: serde_json::Value = read("both-windows.input.json");
let ctx: Context = read("both-windows.context.json");
let expected: Vec<Snapshot> = read("both-windows.expected.json");
assert_eq!(claude_statusline::map(&input, &ctx), expected);
```

The TypeScript suite (`test/fixtures.test.ts`) validates every `expected.json` with zod, checks the
status/note invariants below, and runs each case through `test/reference-adapters.ts`, a test-only
executable version of these rules. If the README, the reference and the fixtures disagree, the suite fails.

## Semantic rules

### Windows each adapter always emits

Every poll yields exactly one snapshot per window, even when there is no data (that one is
`unavailable`, or carried forward).

| Adapter | `provider` | `source.kind` | Windows (in order) |
|---|---|---|---|
| `claude-statusline` | `claude` | `official` | `session_5h`, `weekly` |
| `codex-app-server` | `codex` | `scraped` | `session_5h`, `weekly` |
| `codex-rollout-log` | `codex` | `scraped` | `session_5h`, `weekly` |
| `openrouter` | `openrouter` | `official` | `balance` |
| `deepseek` | `deepseek` | `official` | `balance` |
| `xai` | `xai` | `official` | `balance` |

`source.kind` is `official` for the Claude statusline and the three API-key endpoints, and
`scraped` for both Codex paths (undocumented / log scraping).

### Identity and time

- `schemaVersion: 1`, `account = context.account`, `id = provider:account:window`,
  `source = { kind, adapter: <directory name>, adapterVersion: context.adapterVersion }`.
- `observedAt` is `context.now`, except: `context.observedAt` when present (`claude-statusline`, stale inbox files), the
  event `timestamp` for `codex-rollout-log`, and the carried-over `observedAt` for degraded/stale
  carry-forwards (the age of the value is the age of the last real reading).
- All timestamps are ISO-8601 UTC, whole seconds, `YYYY-MM-DDTHH:MM:SSZ` (truncate milliseconds).
- Unix times in payloads are seconds. `codex-*` tolerates milliseconds: a `resetsAt` with absolute
  value >= 10,000,000,000 is divided by 1000. Non-positive reset times are ignored.

### Value mapping

- `claude-statusline`: `five_hour` -> `session_5h`, `seven_day` -> `weekly`; `used_percentage` ->
  `percentUsed` (clamped to 0..100, not rounded), `resets_at` -> `resetsAt`. Only numbers count:
  `null`, strings or missing fields make the percentage unusable (a window with a percentage but
  no usable reset time is still `ok`, without `resetsAt`). `rate_limits.spend_limit` (Claude apps
  gateway) is ignored in v1.
- `codex-*`: the bucket is `rateLimitsByLimitId.codex` (falling back to `rateLimits`); in rollout
  lines `rate_limits` is used only when `limit_id` is absent, null or `codex`. Windows are assigned
  by **duration**, never by slot: `windowDurationMins` / `window_minutes` through
  `windowFromDurationMinutes`. Only `session_5h` and `weekly` are emitted; windows with an unknown
  or unrecognised duration are ignored; the first window of a kind wins. Other limit buckets
  (for example `codex_bengalfox` / Spark) and `credits` are ignored in v1.
- `openrouter`: unit `usd`. With a `limit`: `used = limit - limit_remaining`, `limit`,
  `remaining = limit_remaining` (may be negative), `percentUsed = min(100, used / limit * 100)`
  (omitted if `limit` is 0). Without a `limit` (uncapped key): `used = usage` only, with a note.
  `limit_reset` and the daily/weekly/monthly spend fields are ignored in v1.
- `deepseek`: unit `usd`, `remaining = total_balance` of the single `USD` entry in `balance_infos`.
  Two `USD` entries is schema drift. CNY-only is `unavailable` (the contract has no CNY unit).
  `is_available: false` keeps `status: ok` and adds a note.
- `xai`: unit `usd`, `remaining = -total.val / 100` (`total.val` is a string of USD cents in an
  inverted ledger: purchases negative, spend positive). A negative result is kept with a note.
- Derived numbers are rounded: money to 6 decimals, derived percentages to 4. Percentages the
  provider reports are passed through unrounded.

### Status

| Situation | Result |
|---|---|
| Fresh data, `resetsAt` absent or after `now` | `ok` |
| Data present but `resetsAt <= now` (old rollout event, stale inbox) | `stale`, figures kept, note `Reset time has already passed; waiting for a fresh reading` |
| **Claude window missing and its previous `resetsAt <= now`** (Claude Code drops a window once it resets) | **`ok`, `percentUsed: 0`, no `resetsAt`, `observedAt = now`, note `window reset`.** A missing window after a known reset means the reset happened. A previous `window reset` snapshot that is still missing is repeated the same way. |
| Window or whole poll unusable, `previous` holds a value whose `resetsAt` is absent or in the future | `degraded`, previous snapshot carried over unchanged (including its `observedAt`), note `<reason>; showing the last known value` |
| Same, but the previous `resetsAt <= now` | `stale`, previous snapshot carried over, note `<reason>; the last known value is past its reset time` |
| Window or whole poll unusable and nothing to carry | `unavailable`, no values, note = reason |

Adapters never apply an age limit: "observed too long ago" is derived by consumers with
`effectiveStatus(snapshot, now, { maxAgeMs })`. Adapters only encode what is certain at poll time.
The test suite checks that `effectiveStatus(expected, now, { maxAgeMs: Infinity })` equals the
stored status for every fixture.

Unusable includes: absent, `null`, wrong type, non-2xx `httpStatus`, JSON-RPC `error`, schema
drift. Adapters never throw into the UI.

### Note catalogue (normative)

| Where | Note |
|---|---|
| any reset marker | `window reset` |
| any stale-by-reset | `Reset time has already passed; waiting for a fresh reading` |
| carry-forward | `<reason>; showing the last known value` / `<reason>; the last known value is past its reset time` |
| Claude, nothing usable | `Claude Code has not reported this window (it only does for Pro/Max subscribers, after the first response of a session)` |
| Claude, carry reason | `Window missing from the latest Claude Code update` |
| Codex app-server, window missing | `Codex did not report this window` |
| Codex app-server, JSON-RPC error | `Codex app-server error: <error.message, at most 200 characters>` |
| Codex app-server, no result | `Codex app-server returned no usable rate limits` |
| Codex rollout, nothing usable | `No Codex rate-limit data in the latest rollout event` |
| API adapters, non-2xx | `<OpenRouter\|DeepSeek\|xAI> request failed (HTTP <status>)` |
| API adapters, drift | `<OpenRouter\|DeepSeek\|xAI> response did not match the expected shape` |
| OpenRouter, no limit | `No spending limit set on this key` |
| DeepSeek, CNY only | `DeepSeek balance is in CNY; only USD balances are supported` |
| DeepSeek, `is_available: false` | `DeepSeek reports the balance as too low for API calls` |
| xAI, negative balance | `Prepaid balance is negative (more spent than purchased)` |

Notes are shown in the UI and synced to phones, so adapters must never copy raw response bodies,
keys, emails or ids into one. Quoted provider text is limited to the 200-character error message above.

## Cases

| Adapter | Case | Covers |
|---|---|---|
| claude-statusline | `both-windows` | Full docs payload, both windows, ignored `spend_limit` |
| | `only-five-hour` | Weekly absent, nothing to carry: `unavailable` |
| | `no-rate-limits` | No `rate_limits` at all (API-key user, or before the first response) |
| | `window-reset-missing` | `five_hour` dropped after its reset: `window reset`; weekly continues |
| | `window-missing-before-reset` | Window missing but not yet reset: `degraded` carry-forward |
| | `windows-reset-while-idle` | Claude Code closed for a long time: reset markers, repeated and new |
| | `stale-inbox` | `context.observedAt` older than `now`; `five_hour` already past its reset: `stale` |
| | `null-fields` | `null` window, `null` percentage and reset time |
| | `malformed-fields` | String percentage (unusable), 140.5% (clamped), `resets_at: -1` (dropped) |
| codex-app-server | `both-windows` | `rateLimitsByLimitId` with `codex` and a Spark bucket, credits (ignored) |
| | `weekly-in-primary` | Weekly window in the `primary` slot, `secondary: null` |
| | `swapped-slots` | `primary` weekly, `secondary` five-hour |
| | `resets-at-milliseconds` | Millisecond `resetsAt` tolerated |
| | `unrecognised-duration` | 600-minute window ignored |
| | `error-response` | JSON-RPC error, nothing to carry |
| | `error-with-previous` | Error with previous values: one `degraded`, one `stale` |
| codex-rollout-log | `both-windows` | Event timestamp becomes `observedAt` |
| | `null-rate-limits` | `token_count` with `rate_limits: null` |
| | `stale-event` | Old event whose five-hour window has since reset: `stale` |
| | `other-limit-id` | Event for a different limit (`codex_bengalfox`) is not the main limit |
| openrouter | `capped-key` | Limit 50, remaining 24.5 |
| | `uncapped-key` | `limit: null` |
| | `overspent` | Negative `limit_remaining`, percentage clamped to 100 |
| | `unauthorized` | HTTP 401 |
| | `unauthorized-with-previous` | HTTP 401 with a previous value: `degraded` |
| | `schema-drift` | 200 with missing fields |
| deepseek | `usd-and-cny` | Both currencies; USD wins |
| | `cny-only` | Documented example shape; unsupported currency |
| | `insufficient-balance` | `is_available: false` |
| | `unauthorized` | HTTP 401 |
| | `empty-body` | `{}` with 200 |
| xai | `prepaid-balance` | Purchase and spend ledger, `total.val` `-2500` is $25.00 |
| | `zero-balance` | `0` |
| | `overdrawn` | Positive `total.val` |
| | `malformed` | `total` without `val` |
| | `forbidden` | HTTP 403 |

## Adding a case

Create the three files, keep the payload realistic and fake, hand-write `expected.json` from the
rules above, then run `npm test -w @cooldown/snapshot-schema`. If the reference adapter disagrees,
either the fixture or the rule is wrong; fix the README, the reference and the fixture together.
