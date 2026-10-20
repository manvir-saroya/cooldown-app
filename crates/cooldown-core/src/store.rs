//! Local SQLite store: the latest snapshot per limit, a bounded history, and reset-alert intents.
//!
//! The store never loses the last good value. When a `degraded` or `unavailable` snapshot arrives
//! without figures, the stored figures (value, reset time and original `observedAt`) are kept and
//! only the status and note change.
//!
//! Timestamps in columns are Unix epoch milliseconds. The latest snapshot is stored as its JSON, so
//! the stored shape is always the contract's shape and survives column changes.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use time::OffsetDateTime;

use crate::clock::{Clock, SystemClock};
use crate::model::{AlertIntent, Snapshot, Status, ValidationError};

/// Default history retention: 90 days.
pub const DEFAULT_HISTORY_RETENTION: Duration = Duration::from_secs(90 * 24 * 60 * 60);

/// How often `upsert` re-runs retention pruning.
const PRUNE_EVERY: time::Duration = time::Duration::hours(1);

/// Store errors.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite failed.
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A stored snapshot is not valid JSON for the contract.
    #[error("stored snapshot is unreadable: {0}")]
    Json(#[from] serde_json::Error),
    /// The snapshot or alert failed validation and was not stored.
    #[error("invalid value: {0}")]
    Invalid(#[from] ValidationError),
    /// A stored timestamp is out of range.
    #[error("stored timestamp is out of range")]
    Timestamp,
    /// The database was written by a newer version of the app.
    #[error("database schema version {found} is newer than this build supports ({supported})")]
    SchemaTooNew {
        /// Version found in the file.
        found: u32,
        /// Highest version this build knows.
        supported: u32,
    },
    /// Creating the database directory failed.
    #[error("cannot create the database directory: {0}")]
    Io(#[from] std::io::Error),
}

/// Result alias for the store.
pub type StoreResult<T> = Result<T, StoreError>;

/// Store settings.
#[derive(Debug, Clone)]
pub struct StoreOptions {
    /// History older than this is pruned.
    pub history_retention: Duration,
}

impl Default for StoreOptions {
    fn default() -> Self {
        StoreOptions {
            history_retention: DEFAULT_HISTORY_RETENTION,
        }
    }
}

/// One point of a limit's history.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryPoint {
    /// When the figure was observed.
    pub observed_at: OffsetDateTime,
    /// Percent used.
    pub percent_used: Option<f64>,
    /// Amount used.
    pub used: Option<f64>,
    /// Amount remaining.
    pub remaining: Option<f64>,
}

/// What `upsert` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertKind {
    /// First snapshot for this id.
    Inserted,
    /// Replaced the stored snapshot.
    Replaced,
    /// A failure arrived without figures; the last good figures were kept.
    KeptLastGood,
    /// An older reading than the stored one; nothing changed.
    IgnoredOlder,
}

/// Result of `upsert`.
#[derive(Debug, Clone, PartialEq)]
pub struct UpsertOutcome {
    /// What happened.
    pub kind: UpsertKind,
    /// The snapshot now stored for the id.
    pub stored: Snapshot,
}

/// Migrations, applied in order. Index + 1 is the schema version. Never edit one that has shipped;
/// append a new one.
const MIGRATIONS: &[&str] = &[
    // 1: initial schema
    "CREATE TABLE latest (
         id TEXT PRIMARY KEY NOT NULL,
         observed_at INTEGER NOT NULL,
         updated_at INTEGER NOT NULL,
         snapshot TEXT NOT NULL
     ) WITHOUT ROWID;
     CREATE TABLE history (
         id TEXT NOT NULL,
         observed_at INTEGER NOT NULL,
         percent_used REAL,
         used REAL,
         remaining REAL,
         PRIMARY KEY (id, observed_at)
     ) WITHOUT ROWID;
     CREATE INDEX history_observed_at ON history (observed_at);
     CREATE TABLE alerts (
         id TEXT PRIMARY KEY NOT NULL,
         fire_at INTEGER NOT NULL,
         enabled INTEGER NOT NULL CHECK (enabled IN (0, 1))
     ) WITHOUT ROWID;",
];

/// Create the version table if needed and apply every migration newer than the stored version, each
/// in its own transaction. Returns the resulting version.
fn migrate(conn: &mut Connection, migrations: &[&str], now_ms: i64) -> StoreResult<u32> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL,
             applied_at INTEGER NOT NULL
         );",
    )?;
    let current: u32 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    let supported = migrations.len() as u32;
    if current > supported {
        return Err(StoreError::SchemaTooNew {
            found: current,
            supported,
        });
    }
    for (index, sql) in migrations.iter().enumerate().skip(current as usize) {
        let version = index as u32 + 1;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![version, now_ms],
        )?;
        tx.commit()?;
    }
    Ok(supported)
}

fn to_ms(time: OffsetDateTime) -> i64 {
    (time.unix_timestamp_nanos() / 1_000_000) as i64
}

fn from_ms(ms: i64) -> StoreResult<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .map_err(|_| StoreError::Timestamp)
}

struct Inner {
    conn: Connection,
    last_prune: Option<OffsetDateTime>,
}

/// The local store. Cheap to share: methods take `&self` and serialize through one connection.
pub struct Store {
    inner: Mutex<Inner>,
    clock: Arc<dyn Clock>,
    options: StoreOptions,
}

impl Store {
    /// Open (creating it and its directory if needed) the database at `path`.
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Store> {
        Store::open_with(path, StoreOptions::default(), Arc::new(SystemClock))
    }

    /// Open a private in-memory database (tests, previews).
    pub fn open_in_memory() -> StoreResult<Store> {
        Store::open_in_memory_with(StoreOptions::default(), Arc::new(SystemClock))
    }

    /// [`Store::open`] with explicit options and clock.
    pub fn open_with(
        path: impl AsRef<Path>,
        options: StoreOptions,
        clock: Arc<dyn Clock>,
    ) -> StoreResult<Store> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        // WAL keeps readers (the UI) from blocking the writer (the poller). It silently stays
        // off on filesystems that cannot do it, which is fine.
        let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        Store::from_connection(conn, options, clock)
    }

    /// [`Store::open_in_memory`] with explicit options and clock.
    pub fn open_in_memory_with(options: StoreOptions, clock: Arc<dyn Clock>) -> StoreResult<Store> {
        Store::from_connection(Connection::open_in_memory()?, options, clock)
    }

    fn from_connection(
        mut conn: Connection,
        options: StoreOptions,
        clock: Arc<dyn Clock>,
    ) -> StoreResult<Store> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
        migrate(&mut conn, MIGRATIONS, to_ms(clock.now()))?;
        let store = Store {
            inner: Mutex::new(Inner {
                conn,
                last_prune: None,
            }),
            clock,
            options,
        };
        store.prune_history()?;
        Ok(store)
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The schema version of the open database.
    pub fn schema_version(&self) -> StoreResult<u32> {
        Ok(self.lock().conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?)
    }

    /// Store a snapshot.
    ///
    /// - Invalid snapshots are rejected (nothing is stored).
    /// - `ok` and `stale` snapshots replace the stored one and add a history point. A reading
    ///   observed before the stored one (out-of-order delivery) is ignored unless the stored one is
    ///   `degraded` or `unavailable`.
    /// - A `degraded` or `unavailable` snapshot WITHOUT figures keeps the stored figures, reset time
    ///   and `observedAt`; the status becomes `degraded` (or `stale` when the kept reset time has
    ///   passed) and the note is the failure's. With nothing stored yet it is stored as is. A
    ///   failure that carries its own figures is stored as is, without a history point.
    pub fn upsert(&self, snapshot: &Snapshot) -> StoreResult<UpsertOutcome> {
        snapshot.validate()?;
        let now = self.clock.now();
        let mut inner = self.lock();
        let tx = inner.conn.transaction()?;

        let existing = read_latest(&tx, &snapshot.id)?;
        let (stored, kind) = merge(existing.as_ref(), snapshot, now);

        if kind != UpsertKind::IgnoredOlder {
            tx.execute(
                "INSERT INTO latest (id, observed_at, updated_at, snapshot)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET
                     observed_at = excluded.observed_at,
                     updated_at = excluded.updated_at,
                     snapshot = excluded.snapshot",
                params![
                    stored.id,
                    to_ms(stored.observed_at),
                    to_ms(now),
                    serde_json::to_string(&stored)?
                ],
            )?;
            let fresh_reading = matches!(kind, UpsertKind::Inserted | UpsertKind::Replaced)
                && matches!(snapshot.status, Status::Ok | Status::Stale)
                && snapshot.has_value();
            let retention = time::Duration::try_from(self.options.history_retention)
                .unwrap_or(time::Duration::MAX);
            if fresh_reading && now - snapshot.observed_at <= retention {
                tx.execute(
                    "INSERT OR IGNORE INTO history (id, observed_at, percent_used, used, remaining)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        snapshot.id,
                        to_ms(snapshot.observed_at),
                        snapshot.percent_used,
                        snapshot.used,
                        snapshot.remaining
                    ],
                )?;
            }
        }
        tx.commit()?;

        let due = inner
            .last_prune
            .is_none_or(|last| now - last >= PRUNE_EVERY);
        if due {
            prune(&mut inner, now, self.options.history_retention)?;
        }
        Ok(UpsertOutcome { kind, stored })
    }

    /// Store several snapshots. Stops at the first error; earlier ones stay stored.
    pub fn upsert_all(&self, snapshots: &[Snapshot]) -> StoreResult<Vec<UpsertOutcome>> {
        snapshots.iter().map(|s| self.upsert(s)).collect()
    }

    /// The latest snapshot for every limit, ordered by id.
    pub fn latest(&self) -> StoreResult<Vec<Snapshot>> {
        let inner = self.lock();
        let mut stmt = inner
            .conn
            .prepare("SELECT snapshot FROM latest ORDER BY id")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row?)?);
        }
        Ok(out)
    }

    /// The latest snapshot for one id.
    pub fn get(&self, id: &str) -> StoreResult<Option<Snapshot>> {
        read_latest(&self.lock().conn, id)
    }

    /// History points for `id` observed at or after `since`, oldest first.
    pub fn history(&self, id: &str, since: OffsetDateTime) -> StoreResult<Vec<HistoryPoint>> {
        let inner = self.lock();
        let mut stmt = inner.conn.prepare(
            "SELECT observed_at, percent_used, used, remaining FROM history
             WHERE id = ?1 AND observed_at >= ?2 ORDER BY observed_at",
        )?;
        let rows = stmt.query_map(params![id, to_ms(since)], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<f64>>(1)?,
                row.get::<_, Option<f64>>(2)?,
                row.get::<_, Option<f64>>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (ms, percent_used, used, remaining) = row?;
            out.push(HistoryPoint {
                observed_at: from_ms(ms)?,
                percent_used,
                used,
                remaining,
            });
        }
        Ok(out)
    }

    /// Forget one account: its latest snapshots, history and alerts (every id that starts with
    /// `{provider}:{account}:` or `reset:{provider}:{account}:`). Returns how many latest
    /// snapshots were removed.
    pub fn remove_account(&self, provider: &str, account: &str) -> StoreResult<usize> {
        let prefix = format!("{provider}:{account}:");
        let alert_prefix = format!("reset:{prefix}");
        let mut inner = self.lock();
        let tx = inner.conn.transaction()?;
        // substr instead of LIKE so '_' and '%' in a name match only themselves.
        let removed = tx.execute(
            "DELETE FROM latest WHERE substr(id, 1, length(?1)) = ?1",
            params![prefix],
        )?;
        tx.execute(
            "DELETE FROM history WHERE substr(id, 1, length(?1)) = ?1",
            params![prefix],
        )?;
        tx.execute(
            "DELETE FROM alerts WHERE substr(id, 1, length(?1)) = ?1",
            params![alert_prefix],
        )?;
        tx.commit()?;
        Ok(removed)
    }

    /// Delete history older than the retention. Returns how many points were removed. `upsert` does
    /// this at most hourly; call it directly to force it.
    pub fn prune_history(&self) -> StoreResult<usize> {
        let now = self.clock.now();
        let mut inner = self.lock();
        prune(&mut inner, now, self.options.history_retention)
    }

    /// Create or replace the alert with this id (alerts are replaced, never stacked).
    pub fn put_alert(&self, alert: &AlertIntent) -> StoreResult<()> {
        alert.validate()?;
        self.lock().conn.execute(
            "INSERT INTO alerts (id, fire_at, enabled) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET fire_at = excluded.fire_at, enabled = excluded.enabled",
            params![alert.id, to_ms(alert.fire_at), alert.enabled],
        )?;
        Ok(())
    }

    /// Delete an alert. Returns whether it existed.
    pub fn remove_alert(&self, id: &str) -> StoreResult<bool> {
        let removed = self
            .lock()
            .conn
            .execute("DELETE FROM alerts WHERE id = ?1", params![id])?;
        Ok(removed > 0)
    }

    /// Every alert, ordered by id.
    pub fn alerts(&self) -> StoreResult<Vec<AlertIntent>> {
        let inner = self.lock();
        let mut stmt = inner
            .conn
            .prepare("SELECT id, fire_at, enabled FROM alerts ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, fire_at, enabled) = row?;
            out.push(AlertIntent {
                id,
                fire_at: from_ms(fire_at)?,
                enabled,
            });
        }
        Ok(out)
    }
}

fn read_latest(conn: &Connection, id: &str) -> StoreResult<Option<Snapshot>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT snapshot FROM latest WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(json.map(|j| serde_json::from_str(&j)).transpose()?)
}

fn prune(inner: &mut Inner, now: OffsetDateTime, retention: Duration) -> StoreResult<usize> {
    inner.last_prune = Some(now);
    let Ok(retention) = time::Duration::try_from(retention) else {
        return Ok(0); // effectively unlimited
    };
    let cutoff = to_ms(now - retention);
    Ok(inner.conn.execute(
        "DELETE FROM history WHERE observed_at < ?1",
        params![cutoff],
    )?)
}

/// Decide what to store when `incoming` arrives and `existing` is already there. Pure.
fn merge(
    existing: Option<&Snapshot>,
    incoming: &Snapshot,
    now: OffsetDateTime,
) -> (Snapshot, UpsertKind) {
    let replaced = if existing.is_some() {
        UpsertKind::Replaced
    } else {
        UpsertKind::Inserted
    };
    match incoming.status {
        Status::Ok | Status::Stale => {
            if let Some(stored) = existing {
                let older = incoming.observed_at < stored.observed_at;
                if older && !stored.status.is_failure() {
                    return (stored.clone(), UpsertKind::IgnoredOlder);
                }
            }
            (incoming.clone(), replaced)
        }
        Status::Degraded | Status::Unavailable => match existing {
            Some(stored) if stored.has_value() && !incoming.has_value() => {
                let mut kept = stored.clone();
                let reset_passed = kept.resets_at.is_some_and(|r| r <= now);
                kept.status = if reset_passed {
                    Status::Stale
                } else {
                    Status::Degraded
                };
                kept.note = Some(incoming.note.clone().unwrap_or_else(|| {
                    "Source unavailable; showing the last known value".to_owned()
                }));
                (kept, UpsertKind::KeptLastGood)
            }
            _ => (incoming.clone(), replaced),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::model::{snapshot_id, SnapshotSource, SourceKind, Unit, WindowKind};
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-10-10 12:00:00 UTC);

    fn source(adapter: &str) -> SnapshotSource {
        SnapshotSource {
            kind: SourceKind::Official,
            adapter: adapter.into(),
            adapter_version: "0.1.0".into(),
        }
    }

    fn reading(window: WindowKind, percent: f64, observed_at: OffsetDateTime) -> Snapshot {
        let mut s = Snapshot::new(
            "claude",
            "default",
            window,
            Unit::Percent,
            source("claude-statusline"),
            observed_at,
        );
        s.percent_used = Some(percent);
        s.resets_at = Some(NOW + time::Duration::hours(2));
        s
    }

    fn session(percent: f64, observed_at: OffsetDateTime) -> Snapshot {
        reading(WindowKind::Session5h, percent, observed_at)
    }

    fn failure(status: Status, note: &str, observed_at: OffsetDateTime) -> Snapshot {
        let mut s = Snapshot::new(
            "claude",
            "default",
            WindowKind::Session5h,
            Unit::Percent,
            source("claude-statusline"),
            observed_at,
        );
        s.status = status;
        s.note = Some(note.into());
        s
    }

    fn store_at(now: OffsetDateTime) -> (Store, Arc<FakeClock>) {
        let clock = FakeClock::new(now);
        (
            Store::open_in_memory_with(StoreOptions::default(), clock.clone()).unwrap(),
            clock,
        )
    }

    #[test]
    fn insert_then_read_back() {
        let (store, _) = store_at(NOW);
        assert!(store.latest().unwrap().is_empty());
        let s = session(40.0, NOW);
        let outcome = store.upsert(&s).unwrap();
        assert_eq!(outcome.kind, UpsertKind::Inserted);
        assert_eq!(store.latest().unwrap(), vec![s.clone()]);
        assert_eq!(store.get(&s.id).unwrap(), Some(s));
        assert_eq!(store.get("nope").unwrap(), None);
    }

    #[test]
    fn ok_replaces_and_latest_is_ordered_by_id() {
        let (store, _) = store_at(NOW);
        store
            .upsert(&reading(WindowKind::Weekly, 10.0, NOW))
            .unwrap();
        store.upsert(&session(40.0, NOW)).unwrap();
        let newer = session(55.0, NOW + time::Duration::minutes(1));
        let outcome = store.upsert(&newer).unwrap();
        assert_eq!(outcome.kind, UpsertKind::Replaced);
        let latest = store.latest().unwrap();
        assert_eq!(latest.len(), 2);
        assert_eq!(latest[0].id, "claude:default:session_5h");
        assert_eq!(latest[0].percent_used, Some(55.0));
        assert_eq!(latest[1].id, "claude:default:weekly");
    }

    #[test]
    fn invalid_snapshots_are_rejected_and_not_stored() {
        let (store, _) = store_at(NOW);
        let mut bad = session(40.0, NOW);
        bad.percent_used = Some(150.0);
        assert!(matches!(store.upsert(&bad), Err(StoreError::Invalid(_))));
        let mut email = session(40.0, NOW);
        email.account = "me@example.com".into();
        email.id = "claude:me@example.com:session_5h".into();
        assert!(matches!(store.upsert(&email), Err(StoreError::Invalid(_))));
        assert!(store.latest().unwrap().is_empty());
    }

    #[test]
    fn unavailable_keeps_the_last_good_value() {
        let (store, _) = store_at(NOW);
        let good = session(40.0, NOW - time::Duration::minutes(5));
        store.upsert(&good).unwrap();
        let outcome = store
            .upsert(&failure(Status::Unavailable, "HTTP 401", NOW))
            .unwrap();
        assert_eq!(outcome.kind, UpsertKind::KeptLastGood);
        let stored = &store.latest().unwrap()[0];
        assert_eq!(stored.status, Status::Degraded);
        assert_eq!(stored.percent_used, Some(40.0));
        assert_eq!(stored.resets_at, good.resets_at);
        assert_eq!(
            stored.observed_at, good.observed_at,
            "age of the value is preserved"
        );
        assert_eq!(stored.note.as_deref(), Some("HTTP 401"));
        assert_eq!(stored.source, good.source);
        assert_eq!(stored.validate(), Ok(()));
    }

    #[test]
    fn degraded_without_a_note_gets_a_default_note() {
        let (store, _) = store_at(NOW);
        store.upsert(&session(40.0, NOW)).unwrap();
        let mut f = failure(Status::Degraded, "x", NOW);
        f.note = None;
        store.upsert(&f).unwrap();
        assert!(store.latest().unwrap()[0]
            .note
            .as_deref()
            .unwrap()
            .contains("last known"));
    }

    #[test]
    fn kept_value_past_its_reset_is_stale() {
        let (store, clock) = store_at(NOW);
        store.upsert(&session(40.0, NOW)).unwrap(); // resets_at = NOW + 2h
        clock.jump_to(NOW + time::Duration::hours(3));
        store
            .upsert(&failure(Status::Degraded, "down", clock.now()))
            .unwrap();
        assert_eq!(store.latest().unwrap()[0].status, Status::Stale);
    }

    #[test]
    fn repeated_failures_keep_the_original_value_and_age() {
        let (store, _) = store_at(NOW);
        let good = session(40.0, NOW - time::Duration::minutes(10));
        store.upsert(&good).unwrap();
        for minute in 1..=5 {
            store
                .upsert(&failure(
                    Status::Unavailable,
                    &format!("try {minute}"),
                    NOW + time::Duration::minutes(minute),
                ))
                .unwrap();
        }
        let stored = &store.latest().unwrap()[0];
        assert_eq!(stored.observed_at, good.observed_at);
        assert_eq!(stored.percent_used, Some(40.0));
        assert_eq!(stored.note.as_deref(), Some("try 5"));
    }

    #[test]
    fn recovery_after_failure_replaces_the_kept_value() {
        let (store, _) = store_at(NOW);
        store
            .upsert(&session(40.0, NOW - time::Duration::minutes(10)))
            .unwrap();
        store
            .upsert(&failure(Status::Unavailable, "down", NOW))
            .unwrap();
        let back = session(45.0, NOW + time::Duration::minutes(1));
        store.upsert(&back).unwrap();
        assert_eq!(store.latest().unwrap(), vec![back]);
    }

    #[test]
    fn failure_with_nothing_stored_is_stored_as_is() {
        let (store, _) = store_at(NOW);
        let f = failure(Status::Unavailable, "no data yet", NOW);
        let outcome = store.upsert(&f).unwrap();
        assert_eq!(outcome.kind, UpsertKind::Inserted);
        assert_eq!(store.latest().unwrap(), vec![f]);
        assert!(store
            .history(
                &store.latest().unwrap()[0].id,
                NOW - time::Duration::days(1)
            )
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_failure_that_carries_figures_is_stored_as_is_without_history() {
        let (store, _) = store_at(NOW);
        let good = session(40.0, NOW - time::Duration::minutes(5));
        store.upsert(&good).unwrap();
        let mut carried = good.clone();
        carried.status = Status::Degraded;
        carried.note = Some("down; showing the last known value".into());
        let outcome = store.upsert(&carried).unwrap();
        assert_eq!(outcome.kind, UpsertKind::Replaced);
        assert_eq!(store.latest().unwrap(), vec![carried.clone()]);
        let history = store
            .history(&good.id, NOW - time::Duration::days(1))
            .unwrap();
        assert_eq!(history.len(), 1, "only the real reading");
    }

    #[test]
    fn out_of_order_readings_are_ignored_but_not_over_failures() {
        let (store, _) = store_at(NOW);
        let newer = session(60.0, NOW);
        store.upsert(&newer).unwrap();
        let older = session(30.0, NOW - time::Duration::minutes(10));
        let outcome = store.upsert(&older).unwrap();
        assert_eq!(outcome.kind, UpsertKind::IgnoredOlder);
        assert_eq!(store.latest().unwrap(), vec![newer.clone()]);
        // same observedAt replaces (idempotent redelivery)
        assert_eq!(store.upsert(&newer).unwrap().kind, UpsertKind::Replaced);

        // after a failure, an older reading from a working source is accepted
        store
            .upsert(&failure(
                Status::Unavailable,
                "down",
                NOW + time::Duration::minutes(1),
            ))
            .unwrap();
        let outcome = store.upsert(&older).unwrap();
        assert_eq!(outcome.kind, UpsertKind::Replaced);
        assert_eq!(store.latest().unwrap()[0].percent_used, Some(30.0));
    }

    #[test]
    fn history_records_fresh_readings_in_order() {
        let (store, _) = store_at(NOW);
        let id = session(0.0, NOW).id;
        for (minute, percent) in [(0, 10.0), (1, 20.0), (2, 30.0)] {
            store
                .upsert(&session(percent, NOW + time::Duration::minutes(minute)))
                .unwrap();
        }
        // redelivery of the same observation does not duplicate
        store
            .upsert(&session(30.0, NOW + time::Duration::minutes(2)))
            .unwrap();
        let all = store.history(&id, NOW - time::Duration::hours(1)).unwrap();
        assert_eq!(
            all.iter()
                .map(|p| p.percent_used.unwrap())
                .collect::<Vec<_>>(),
            vec![10.0, 20.0, 30.0]
        );
        assert_eq!(all[0].observed_at, NOW);
        let since = store
            .history(&id, NOW + time::Duration::minutes(1))
            .unwrap();
        assert_eq!(since.len(), 2, "since is inclusive");
        assert!(store.history("other", NOW).unwrap().is_empty());
    }

    #[test]
    fn history_stores_used_and_remaining() {
        let (store, _) = store_at(NOW);
        let mut s = Snapshot::new(
            "openrouter",
            "default",
            WindowKind::Balance,
            Unit::Usd,
            source("openrouter"),
            NOW,
        );
        s.used = Some(25.5);
        s.limit = Some(50.0);
        s.remaining = Some(24.5);
        store.upsert(&s).unwrap();
        let points = store.history(&s.id, NOW).unwrap();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].percent_used, None);
        assert_eq!(points[0].used, Some(25.5));
        assert_eq!(points[0].remaining, Some(24.5));
    }

    #[test]
    fn history_is_pruned_past_the_retention() {
        let (store, clock) = store_at(NOW);
        let id = session(0.0, NOW).id;
        store.upsert(&session(10.0, NOW)).unwrap();
        clock.jump_to(NOW + time::Duration::days(60));
        store.upsert(&session(20.0, clock.now())).unwrap();
        assert_eq!(
            store
                .history(&id, NOW - time::Duration::days(1))
                .unwrap()
                .len(),
            2
        );
        clock.jump_to(NOW + time::Duration::days(91));
        // 91 days after the first reading: the first goes, the 60-day-old one stays
        let removed = store.prune_history().unwrap();
        assert_eq!(removed, 1);
        let left = store.history(&id, NOW - time::Duration::days(1)).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].percent_used, Some(20.0));
    }

    #[test]
    fn upsert_prunes_at_most_hourly() {
        let (store, clock) = store_at(NOW);
        let id = session(0.0, NOW).id;
        store.upsert(&session(10.0, NOW)).unwrap(); // prune ran at open, last_prune = NOW (open)
        clock.jump_to(NOW + time::Duration::days(95));
        // first upsert after the jump prunes (more than an hour since last prune)
        store.upsert(&session(20.0, clock.now())).unwrap();
        let left = store.history(&id, NOW - time::Duration::days(1)).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].percent_used, Some(20.0));
    }

    #[test]
    fn custom_retention_and_stale_readings_outside_it_are_not_recorded() {
        let clock = FakeClock::new(NOW);
        let store = Store::open_in_memory_with(
            StoreOptions {
                history_retention: Duration::from_secs(3600),
            },
            clock,
        )
        .unwrap();
        let mut ancient = session(10.0, NOW - time::Duration::hours(5));
        ancient.status = Status::Stale;
        store.upsert(&ancient).unwrap();
        assert_eq!(store.latest().unwrap().len(), 1, "still shown as latest");
        assert!(store
            .history(&ancient.id, NOW - time::Duration::days(1))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn alerts_are_replaced_never_stacked() {
        let (store, _) = store_at(NOW);
        assert!(store.alerts().unwrap().is_empty());
        let a = AlertIntent::new(
            "reset:claude:default:session_5h",
            NOW + time::Duration::hours(1),
        );
        let b = AlertIntent::new("reset:claude:default:weekly", NOW + time::Duration::days(2));
        store.put_alert(&b).unwrap();
        store.put_alert(&a).unwrap();
        assert_eq!(
            store.alerts().unwrap(),
            vec![a.clone(), b.clone()],
            "ordered by id"
        );

        let moved = AlertIntent {
            fire_at: NOW + time::Duration::hours(2),
            enabled: false,
            ..a.clone()
        };
        store.put_alert(&moved).unwrap();
        let alerts = store.alerts().unwrap();
        assert_eq!(alerts.len(), 2);
        assert_eq!(alerts[0], moved);

        assert!(store.remove_alert(&a.id).unwrap());
        assert!(!store.remove_alert(&a.id).unwrap());
        assert_eq!(store.alerts().unwrap(), vec![b]);
    }

    #[test]
    fn remove_account_forgets_only_that_account() {
        let (store, _) = store_at(NOW);
        let gone = session(40.0, NOW);
        let mut kept = session(10.0, NOW);
        kept.account = "default2".into();
        kept.id = snapshot_id("claude", "default2", WindowKind::Session5h);
        store.upsert_all(&[gone.clone(), kept.clone()]).unwrap();
        store
            .put_alert(&AlertIntent::new(
                "reset:claude:default:session_5h",
                NOW + time::Duration::hours(1),
            ))
            .unwrap();
        let kept_alert = AlertIntent::new(
            "reset:claude:default2:session_5h",
            NOW + time::Duration::hours(1),
        );
        store.put_alert(&kept_alert).unwrap();

        assert_eq!(store.remove_account("claude", "default").unwrap(), 1);
        assert_eq!(store.latest().unwrap(), vec![kept.clone()]);
        assert!(store
            .history(&gone.id, NOW - time::Duration::days(1))
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .history(&kept.id, NOW - time::Duration::days(1))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(store.alerts().unwrap(), vec![kept_alert]);
        assert_eq!(store.remove_account("claude", "default").unwrap(), 0);
    }

    #[test]
    fn invalid_alert_ids_are_rejected() {
        let (store, _) = store_at(NOW);
        let bad = AlertIntent::new("not-an-alert", NOW);
        assert!(matches!(store.put_alert(&bad), Err(StoreError::Invalid(_))));
        assert!(store.alerts().unwrap().is_empty());
    }

    #[test]
    fn data_survives_reopening_a_file_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("cooldown.db");
        let s = session(40.0, NOW);
        let alert = AlertIntent::new(
            "reset:claude:default:session_5h",
            NOW + time::Duration::hours(2),
        );
        {
            let clock = FakeClock::new(NOW);
            let store = Store::open_with(&path, StoreOptions::default(), clock).unwrap();
            store.upsert(&s).unwrap();
            store.put_alert(&alert).unwrap();
        }
        let clock = FakeClock::new(NOW);
        let store = Store::open_with(&path, StoreOptions::default(), clock).unwrap();
        assert_eq!(store.latest().unwrap(), vec![s.clone()]);
        assert_eq!(store.alerts().unwrap(), vec![alert]);
        assert_eq!(store.history(&s.id, NOW).unwrap().len(), 1);
        assert_eq!(store.schema_version().unwrap(), 1);
    }

    #[test]
    fn the_store_can_be_shared_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Store>();
        let store = Arc::new(Store::open_in_memory().unwrap());
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let store = store.clone();
                std::thread::spawn(move || {
                    let mut s = Snapshot::new(
                        "claude",
                        format!("acct{i}"),
                        WindowKind::Weekly,
                        Unit::Percent,
                        source("claude-statusline"),
                        OffsetDateTime::now_utc(),
                    );
                    s.percent_used = Some(f64::from(i));
                    store.upsert(&s).unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(store.latest().unwrap().len(), 4);
    }

    #[test]
    fn open_default_clock_works() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), MIGRATIONS.len() as u32);
    }

    #[test]
    fn migrations_apply_in_order_once_and_resume() {
        let mut conn = Connection::open_in_memory().unwrap();
        let v1 = "CREATE TABLE a (x INTEGER);";
        let v2 = "ALTER TABLE a ADD COLUMN y INTEGER; INSERT INTO a (x, y) VALUES (1, 2);";
        assert_eq!(migrate(&mut conn, &[v1], 0).unwrap(), 1);
        // upgrading only runs the new step
        assert_eq!(migrate(&mut conn, &[v1, v2], 5).unwrap(), 2);
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM a", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        // idempotent
        assert_eq!(migrate(&mut conn, &[v1, v2], 9).unwrap(), 2);
        let applied: Vec<(u32, i64)> = {
            let mut stmt = conn
                .prepare("SELECT version, applied_at FROM schema_migrations ORDER BY version")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(applied, vec![(1, 0), (2, 5)]);
    }

    #[test]
    fn a_failing_migration_rolls_back_and_leaves_the_version_alone() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn, &["CREATE TABLE a (x INTEGER);"], 0).unwrap();
        let broken = "CREATE TABLE b (x INTEGER); THIS IS NOT SQL;";
        assert!(migrate(&mut conn, &["CREATE TABLE a (x INTEGER);", broken], 1).is_err());
        let b_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'b'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(b_exists, 0, "partial migration rolled back");
        let version: u32 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn a_newer_database_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY NOT NULL, applied_at INTEGER NOT NULL);
                 INSERT INTO schema_migrations VALUES (99, 0);",
            )
            .unwrap();
        }
        match Store::open(&path) {
            Err(StoreError::SchemaTooNew {
                found: 99,
                supported: 1,
            }) => {}
            Err(other) => panic!("wrong error: {other}"),
            Ok(_) => panic!("opened a database from the future"),
        }
    }

    #[test]
    fn timestamps_round_trip_through_milliseconds() {
        for t in [
            NOW,
            datetime!(1999-12-31 23:59:59 UTC),
            datetime!(2026-10-10 12:00:00.123 UTC),
        ] {
            assert_eq!(from_ms(to_ms(t)).unwrap(), t);
        }
        assert!(from_ms(i64::MAX).is_err());
    }

    #[test]
    fn merge_table() {
        let good = session(40.0, NOW - time::Duration::minutes(5));
        let unavailable = failure(Status::Unavailable, "down", NOW);
        // (existing, incoming, expected kind)
        let (m, k) = merge(None, &good, NOW);
        assert_eq!((m, k), (good.clone(), UpsertKind::Inserted));
        let (_, k) = merge(Some(&good), &unavailable, NOW);
        assert_eq!(k, UpsertKind::KeptLastGood);
        let (m, k) = merge(None, &unavailable, NOW);
        assert_eq!((m, k), (unavailable.clone(), UpsertKind::Inserted));
        let mut stale_in = good.clone();
        stale_in.status = Status::Stale;
        stale_in.observed_at = NOW;
        let (m, k) = merge(Some(&good), &stale_in, NOW);
        assert_eq!((m.status, k), (Status::Stale, UpsertKind::Replaced));
    }
}
