# @cooldown/snapshot-schema

The contract between every Cooldown component: the `Snapshot` / `SyncPayload` / `AlertIntent`
types (spec section 4, frozen in `src/types.ts`), strict validators, pure helpers, a generated JSON
Schema, and the adapter fixtures that the TypeScript and Rust tests share.

```ts
import { parseSnapshot, effectiveStatus, formatAge, alertIdFor } from '@cooldown/snapshot-schema';

const result = parseSnapshot(json);          // never throws
if (result.ok) {
  effectiveStatus(result.value, new Date()); // 'ok' | 'stale' | 'degraded' | 'unavailable'
  formatAge(result.value.observedAt, new Date()); // '2 min ago'
}
```

## What is in here

| Path | What |
|---|---|
| `src/types.ts` | Frozen TypeScript types. Changing a shape means bumping `schemaVersion`. |
| `src/schema.ts` | zod 4 validators that mirror the types exactly (compile-time checked). `parseSnapshot`, `parseSyncPayload` return `{ ok, value }` or `{ ok: false, errors }`. |
| `src/helpers.ts` | `effectiveStatus`, `alertIdFor`, `snapshotIdFor`, `windowFromDurationMinutes`, `secondsUntilReset`, `formatAge`. All pure; time is always the `now: Date` argument. |
| `src/adapters.ts` | Adapter table (provider, source kind, windows) and the fixture context schema. |
| `schema/*.json` | Generated JSON Schema (draft 2020-12). Rust tests validate against these. |
| `fixtures/` | Raw provider payloads and the exact snapshots a correct adapter must produce. See `fixtures/README.md`. |
| `test/` | vitest suite, including a test-only reference implementation of the adapter rules. |

## Strictness

Unknown keys are rejected; `percentUsed` is 0..100; timestamps are ISO-8601 UTC (`...Z`, no
offsets); `id` must equal `${provider}:${account}:${window}`; `account` is a label of 1 to 64
characters with no `@`, no `:`, no control characters and no well-known token prefix; `provider`
is a lowercase slug. A `SyncPayload` may not repeat a snapshot id or an alert id.

The id equality, token-prefix and uniqueness rules cannot be written in JSON Schema, so the
generated files are slightly looser than the zod validators. Use the validators wherever the rules matter.

## Scripts

```
npm test -w @cooldown/snapshot-schema                # vitest
npm run typecheck -w @cooldown/snapshot-schema       # tsc --noEmit
npm run build:schema -w @cooldown/snapshot-schema    # regenerate schema/*.json (commit the result)
```

The suite fails if the committed `schema/*.json` are out of date.
