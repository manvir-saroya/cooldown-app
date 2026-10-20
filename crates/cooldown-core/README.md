# cooldown-core

The part of Cooldown that is not UI and not operating-system glue. No network access, no credentials, no platform-specific code: it builds and tests the same on macOS and Windows.

| Module | What it is |
|---|---|
| `model` | The snapshot contract from `packages/snapshot-schema` in Rust: `Snapshot`, `AlertIntent`, `SyncPayload`, with the same JSON (camelCase, unknown keys rejected, RFC 3339 UTC timestamps) and `validate()`. Helpers: `snapshot_id`, `alert_id`, `window_from_duration_minutes` (windows are classified by duration, never by slot), `effective_status`, `seconds_until_reset`. |
| `adapter` | `trait Adapter` (`poll` never errors), `AdapterFailure::into_snapshot` (failures become `degraded`/`unavailable`/`stale` snapshots that carry the last good value), and a `Registry` that polls each adapter on its own interval with exponential backoff and jitter. Dropping the handle cancels everything, including a poll in flight. |
| `store` | SQLite (`rusqlite`, bundled). Latest snapshot per id, history with 90-day pruning, alert intents, versioned migrations. A failure never overwrites the last good figures. |
| `alerts` | Pure reset-alert reconciliation: `reconcile`, `Reconciler`, `should_fire`. |
| `clock` | `Clock` trait, `SystemClock`, and `FakeClock` for deterministic tests. |

## How the pieces fit

```text
Registry --PollEvent--> Store::upsert --> Store::latest() --+
                                                            v
              Store::alerts() ----> Reconciler::reconcile(alerts, snapshots, now)
                                                            |
                    Schedule / Reschedule / Cancel / Fire --+--> tray timer, toast, Android alarm
                                                            |
                                  apply_actions + Store::put_alert / remove_alert
```

## Reset alerts in short

One alert per limit window, id `reset:<provider>:<account>:<window>`; the desired fire time is the latest known `resetsAt`; alerts are replaced, never stacked.

- Shifts under 60 seconds are ignored.
- A change (reschedule, cancel, fire early) needs two distinct, agreeing, fresh `ok` reads (Codex reads can glitch). The same read delivered twice counts once. Firing on time is never gated on confirmation.
- Disabled alerts never fire. An alert overdue by more than 2 hours is dropped, not fired.
- A window that vanishes (or loses its reset time) after the reset time means the reset happened: fire once.
- `should_fire` is the single-read gate to run just before showing the notification. A veto is only a pause; the alert stays armed.
- A window with no alert at all that is exhausted (99 percent or more) and has a future reset gets one automatically (`ReconcileConfig::auto_arm_at_percent`, `None` turns it off). A user's explicit request is just an alert the caller stores itself.

`Reconciler` keeps the last reads so confirmation works across polls. The free function `reconcile` is the same logic without memory and holds unconfirmed changes unless you pass it several reads of a window. After `Fire`, remove the alert (`apply_actions` does the bookkeeping).

## Tests

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p cooldown-core
cargo clippy -p cooldown-core --all-targets -- -D warnings
cargo fmt -p cooldown-core -- --check
```

`tests/fixtures.rs` reads `packages/snapshot-schema/fixtures/adapters/**/*.expected.json`, checks each snapshot deserializes, validates and serializes back to the same JSON, and checks the `previous` snapshots in the `*.context.json` files. If the directory is missing or empty it passes with a note.

## License

MIT
