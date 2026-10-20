//! Source adapters and the registry that polls them.
//!
//! An [`Adapter`] reads one source (a statusline inbox file, an HTTP API, ...) and returns
//! [`Snapshot`]s. `poll` never fails: problems become `degraded` or `unavailable` snapshots, built
//! with [`AdapterFailure`], that keep the last good value so the UI never blanks.
//!
//! The [`Registry`] runs each adapter on its own interval. Failed polls back off exponentially with
//! jitter. Dropping the [`RegistryHandle`] (or calling `cancel`/`shutdown`) stops every worker,
//! including one that is in the middle of a poll.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use time::OffsetDateTime;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::clock::Clock;
use crate::model::{
    snapshot_id, Snapshot, SnapshotSource, SourceKind, Status, Unit, WindowKind, SCHEMA_VERSION,
};

/// Smallest interval a worker will wait between polls, whatever the adapter asks for.
pub const MIN_POLL_INTERVAL: Duration = Duration::from_secs(1);

const MAX_NOTE_CHARS: usize = 500;

/// Everything an adapter needs besides its own configuration.
#[derive(Debug, Clone)]
pub struct PollContext {
    /// The time of this poll, from the registry's clock.
    pub now: OffsetDateTime,
    /// The snapshots this adapter returned last time (empty on the first poll). Use them to carry
    /// the last good value forward on failure.
    pub previous: Vec<Snapshot>,
    /// How many polls in a row have failed (0 after a success).
    pub consecutive_failures: u32,
}

impl PollContext {
    /// A context for a first poll.
    pub fn new(now: OffsetDateTime) -> Self {
        PollContext {
            now,
            previous: Vec::new(),
            consecutive_failures: 0,
        }
    }

    /// The previous snapshot with this id, if any.
    pub fn previous_for(&self, id: &str) -> Option<&Snapshot> {
        self.previous.iter().find(|s| s.id == id)
    }
}

/// A source of snapshots.
#[async_trait]
pub trait Adapter: Send + Sync {
    /// Stable adapter id, also the `source.adapter` value (for example `claude-statusline`).
    fn id(&self) -> &'static str;

    /// Adapter version, also the `source.adapterVersion` value.
    fn version(&self) -> &'static str;

    /// How often to poll when things are healthy.
    fn poll_interval(&self) -> Duration;

    /// Whether failed polls slow the schedule down (exponential backoff). True for anything that
    /// talks to a server. A cheap local read, such as a file another program writes, returns false:
    /// it keeps its normal pace, so data that appears later is picked up at once.
    fn backs_off(&self) -> bool {
        true
    }

    /// Read the source. NEVER returns an error: failures become `degraded` or `unavailable`
    /// snapshots (see [`AdapterFailure`]).
    async fn poll(&self, ctx: &PollContext) -> Vec<Snapshot>;
}

/// Identity of one limit window, enough to build a snapshot for it when the source gave nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotTemplate {
    /// Provider slug.
    pub provider: String,
    /// Account label.
    pub account: String,
    /// Which limit.
    pub window: WindowKind,
    /// What the numbers count.
    pub unit: Unit,
    /// Provenance to stamp on snapshots built from this template.
    pub source: SnapshotSource,
}

impl SnapshotTemplate {
    /// A template for a window of an adapter.
    pub fn new(
        provider: impl Into<String>,
        account: impl Into<String>,
        window: WindowKind,
        unit: Unit,
        adapter: &dyn Adapter,
        kind: SourceKind,
    ) -> Self {
        SnapshotTemplate {
            provider: provider.into(),
            account: account.into(),
            window,
            unit,
            source: SnapshotSource {
                kind,
                adapter: adapter.id().to_owned(),
                adapter_version: adapter.version().to_owned(),
            },
        }
    }

    /// Recover the identity of an existing snapshot.
    pub fn from_snapshot(snapshot: &Snapshot) -> Self {
        SnapshotTemplate {
            provider: snapshot.provider.clone(),
            account: snapshot.account.clone(),
            window: snapshot.window,
            unit: snapshot.unit,
            source: snapshot.source.clone(),
        }
    }

    /// `{provider}:{account}:{window}`.
    pub fn id(&self) -> String {
        snapshot_id(&self.provider, &self.account, self.window)
    }
}

/// Why a poll produced no fresh figures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterFailure {
    /// `Degraded` (something is wrong but figures may still exist) or `Unavailable` (nothing).
    pub status: Status,
    /// Human-readable reason. Must not contain secrets; it is shown in the UI and synced.
    pub reason: String,
}

impl AdapterFailure {
    /// A degraded-source failure.
    pub fn degraded(reason: impl Into<String>) -> Self {
        AdapterFailure {
            status: Status::Degraded,
            reason: reason.into(),
        }
    }

    /// A no-data failure.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        AdapterFailure {
            status: Status::Unavailable,
            reason: reason.into(),
        }
    }

    /// Turn the failure into a snapshot for one window.
    ///
    /// With a `previous` snapshot that has figures, the figures, reset time and original
    /// `observedAt` are carried forward and the status becomes `degraded`, or `stale` when the
    /// carried reset time has passed, so the UI keeps showing the last good value. Without one the
    /// snapshot has no figures, `observedAt` is `now` and the status is the failure's own.
    pub fn into_snapshot(
        self,
        template: &SnapshotTemplate,
        previous: Option<&Snapshot>,
        now: OffsetDateTime,
    ) -> Snapshot {
        let id = template.id();
        let carried = previous.filter(|p| p.id == id && p.has_value());
        let mut snapshot = Snapshot {
            schema_version: SCHEMA_VERSION,
            id,
            provider: template.provider.clone(),
            account: template.account.clone(),
            window: template.window,
            unit: template.unit,
            percent_used: None,
            used: None,
            limit: None,
            remaining: None,
            resets_at: None,
            observed_at: now,
            source: template.source.clone(),
            status: self.status,
            note: None,
        };
        match carried {
            Some(last) => {
                snapshot.percent_used = last.percent_used;
                snapshot.used = last.used;
                snapshot.limit = last.limit;
                snapshot.remaining = last.remaining;
                snapshot.resets_at = last.resets_at;
                snapshot.observed_at = last.observed_at;
                let reset_passed = last.resets_at.is_some_and(|r| r <= now);
                let tail = if reset_passed {
                    snapshot.status = Status::Stale;
                    "the last known value is past its reset time"
                } else {
                    snapshot.status = Status::Degraded;
                    "showing the last known value"
                };
                snapshot.note = Some(truncate_note(&format!("{}; {tail}", self.reason)));
            }
            None => snapshot.note = Some(truncate_note(&self.reason)),
        }
        snapshot
    }

    /// [`AdapterFailure::into_snapshot`] for every window an adapter emits, carrying forward from
    /// `ctx.previous`.
    pub fn into_snapshots(
        self,
        templates: &[SnapshotTemplate],
        ctx: &PollContext,
    ) -> Vec<Snapshot> {
        templates
            .iter()
            .map(|template| {
                let previous = ctx.previous_for(&template.id());
                self.clone().into_snapshot(template, previous, ctx.now)
            })
            .collect()
    }
}

/// Cut a note to the schema limit (500 UTF-16 code units), ending in an ellipsis when cut.
pub fn truncate_note(note: &str) -> String {
    if note.encode_utf16().count() <= MAX_NOTE_CHARS {
        return note.to_owned();
    }
    let mut out = String::new();
    let mut units = 0;
    for c in note.chars() {
        units += c.len_utf16();
        if units > MAX_NOTE_CHARS - 1 {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

/// True when a poll's output means the source is failing: at least one snapshot and none of them
/// usable. A mix (one window fine, another degraded) is a partial success. Stale snapshots (an
/// inbox nobody has written to lately) are not failures, so polling stays at its normal pace.
pub fn is_failed_poll(snapshots: &[Snapshot]) -> bool {
    !snapshots.is_empty() && snapshots.iter().all(|s| s.status.is_failure())
}

/// Delay before the next poll.
///
/// - No failures: `interval`.
/// - `n` consecutive failures: `interval * 2^n`, capped at `max(max_backoff, interval)`.
/// - Then multiplied by `1 + jitter_fraction * (2 * unit - 1)`, where `unit` is a uniform draw in
///   `[0, 1)`; so the result is within `jitter_fraction` either side of the base. `unit = 0.5`
///   gives no jitter.
///
/// The result is never below [`MIN_POLL_INTERVAL`].
pub fn next_delay(
    interval: Duration,
    consecutive_failures: u32,
    max_backoff: Duration,
    jitter_fraction: f64,
    unit: f64,
) -> Duration {
    let interval = interval.max(MIN_POLL_INTERVAL);
    let base = if consecutive_failures == 0 {
        interval
    } else {
        let cap = max_backoff.max(interval);
        let factor = 1u32 << consecutive_failures.min(20);
        interval.saturating_mul(factor).min(cap)
    };
    let fraction = if jitter_fraction.is_finite() {
        jitter_fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let unit = if unit.is_finite() {
        unit.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let scale = 1.0 + fraction * (2.0 * unit - 1.0);
    base.mul_f64(scale).max(MIN_POLL_INTERVAL)
}

/// A source of uniform random draws in `[0, 1)`, used for jitter. Injectable for tests.
pub type JitterFn = Arc<dyn Fn() -> f64 + Send + Sync>;

/// A small thread-safe pseudo-random source (splitmix64) seeded from the system clock. Jitter does
/// not need cryptographic randomness.
pub fn default_jitter() -> JitterFn {
    let seed = OffsetDateTime::now_utc().unix_timestamp_nanos() as u64 ^ 0x9E37_79B9_7F4A_7C15;
    let state = Arc::new(AtomicU64::new(seed));
    Arc::new(move || {
        let x = state.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
        let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    })
}

/// Registry tuning.
#[derive(Clone)]
pub struct RegistryConfig {
    /// Longest wait after repeated failures (never less than the adapter's own interval).
    pub max_backoff: Duration,
    /// Jitter as a fraction of the delay, 0.0 to 1.0 (0.1 means plus or minus 10%).
    pub jitter_fraction: f64,
    /// Capacity of the event channel. Workers wait when it is full.
    pub channel_capacity: usize,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        RegistryConfig {
            max_backoff: Duration::from_secs(30 * 60),
            jitter_fraction: 0.1,
            channel_capacity: 64,
        }
    }
}

/// What a worker reports after each poll.
#[derive(Debug, Clone)]
pub struct PollEvent {
    /// Which registered adapter (index in registration order).
    pub slot: usize,
    /// The adapter's id.
    pub adapter: &'static str,
    /// The adapter's version.
    pub adapter_version: &'static str,
    /// When the poll started, by the registry's clock.
    pub polled_at: OffsetDateTime,
    /// The snapshots to store.
    pub snapshots: Vec<Snapshot>,
    /// Whether the poll counted as a failure (see [`is_failed_poll`]) or the adapter panicked.
    pub failed: bool,
    /// Consecutive failures including this one (0 after a success).
    pub consecutive_failures: u32,
    /// How long the worker will wait before the next poll.
    pub next_poll_in: Duration,
}

/// Runs adapters on their own schedules.
pub struct Registry {
    clock: Arc<dyn Clock>,
    config: RegistryConfig,
    jitter: JitterFn,
    adapters: Vec<Arc<dyn Adapter>>,
}

impl Registry {
    /// An empty registry on `clock`.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Registry {
            clock,
            config: RegistryConfig::default(),
            jitter: default_jitter(),
            adapters: Vec::new(),
        }
    }

    /// Replace the tuning.
    pub fn with_config(mut self, config: RegistryConfig) -> Self {
        self.config = config;
        self
    }

    /// Replace the jitter source (tests pass a constant).
    pub fn with_jitter(mut self, jitter: JitterFn) -> Self {
        self.jitter = jitter;
        self
    }

    /// Add an adapter. Several instances of the same adapter (one per account) are fine.
    pub fn register(mut self, adapter: Arc<dyn Adapter>) -> Self {
        self.adapters.push(adapter);
        self
    }

    /// Number of registered adapters.
    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    /// True when nothing is registered.
    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }

    /// Spawn one worker per adapter on the current tokio runtime. Every adapter is polled at once,
    /// then on its own interval. Events arrive on the returned receiver; the workers stop when the
    /// handle is dropped, cancelled or shut down, or when the receiver is dropped.
    pub fn start(self) -> (RegistryHandle, mpsc::Receiver<PollEvent>) {
        let (events, receiver) = mpsc::channel(self.config.channel_capacity.max(1));
        let (cancel, cancelled) = watch::channel(false);
        let tasks = self
            .adapters
            .iter()
            .enumerate()
            .map(|(slot, adapter)| {
                tokio::spawn(run_worker(Worker {
                    slot,
                    adapter: adapter.clone(),
                    clock: self.clock.clone(),
                    config: self.config.clone(),
                    jitter: self.jitter.clone(),
                    events: events.clone(),
                    cancelled: cancelled.clone(),
                }))
            })
            .collect();
        (RegistryHandle { cancel, tasks }, receiver)
    }
}

/// Controls a running registry. Dropping it cancels every worker.
pub struct RegistryHandle {
    cancel: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl RegistryHandle {
    /// Ask every worker to stop. A poll in progress is aborted.
    pub fn cancel(&self) {
        let _ = self.cancel.send(true);
    }

    /// Cancel and wait until every worker has finished.
    pub async fn shutdown(self) {
        self.cancel();
        for task in self.tasks {
            let _ = task.await;
        }
    }
}

struct Worker {
    slot: usize,
    adapter: Arc<dyn Adapter>,
    clock: Arc<dyn Clock>,
    config: RegistryConfig,
    jitter: JitterFn,
    events: mpsc::Sender<PollEvent>,
    cancelled: watch::Receiver<bool>,
}

/// Resolves when cancellation is requested or the handle is gone.
async fn wait_cancelled(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|cancelled| *cancelled).await;
}

async fn run_worker(mut worker: Worker) {
    let mut previous: Vec<Snapshot> = Vec::new();
    let mut failures: u32 = 0;
    loop {
        let polled_at = worker.clock.now();
        let ctx = PollContext {
            now: polled_at,
            previous: previous.clone(),
            consecutive_failures: failures,
        };

        // Poll in its own task: a panicking adapter cannot kill the worker, and cancellation can
        // abort a poll that is stuck.
        let mut poll = {
            let adapter = worker.adapter.clone();
            let ctx = ctx.clone();
            tokio::spawn(async move { adapter.poll(&ctx).await })
        };
        let result = tokio::select! {
            () = wait_cancelled(&mut worker.cancelled) => {
                poll.abort();
                return;
            }
            result = &mut poll => result,
        };

        let (snapshots, failed) = match result {
            Ok(snapshots) => {
                let failed = is_failed_poll(&snapshots);
                (snapshots, failed)
            }
            Err(_) => {
                // The adapter panicked. Keep showing what we had, marked degraded.
                let carried = previous
                    .iter()
                    .map(|p| {
                        AdapterFailure::degraded("adapter crashed while polling").into_snapshot(
                            &SnapshotTemplate::from_snapshot(p),
                            Some(p),
                            polled_at,
                        )
                    })
                    .collect();
                (carried, true)
            }
        };

        failures = if failed {
            failures.saturating_add(1)
        } else {
            0
        };
        if !snapshots.is_empty() {
            previous = snapshots.clone();
        }

        let unit = (worker.jitter)();
        let delay = next_delay(
            worker.adapter.poll_interval(),
            if worker.adapter.backs_off() {
                failures
            } else {
                0
            },
            worker.config.max_backoff,
            worker.config.jitter_fraction,
            unit,
        );

        let event = PollEvent {
            slot: worker.slot,
            adapter: worker.adapter.id(),
            adapter_version: worker.adapter.version(),
            polled_at,
            snapshots,
            failed,
            consecutive_failures: failures,
            next_poll_in: delay,
        };
        let sent = tokio::select! {
            () = wait_cancelled(&mut worker.cancelled) => return,
            sent = worker.events.send(event) => sent,
        };
        if sent.is_err() {
            return; // nobody is listening any more
        }

        tokio::select! {
            () = wait_cancelled(&mut worker.cancelled) => return,
            () = worker.clock.sleep(delay) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicU32};
    use std::sync::Mutex;
    use time::macros::datetime;

    const START: OffsetDateTime = datetime!(2026-10-10 12:00:00 UTC);

    fn source() -> SnapshotSource {
        SnapshotSource {
            kind: SourceKind::Official,
            adapter: "test".into(),
            adapter_version: "1.0.0".into(),
        }
    }

    fn template() -> SnapshotTemplate {
        SnapshotTemplate {
            provider: "claude".into(),
            account: "default".into(),
            window: WindowKind::Session5h,
            unit: Unit::Percent,
            source: source(),
        }
    }

    fn good(observed_at: OffsetDateTime) -> Snapshot {
        let mut s = Snapshot::new(
            "claude",
            "default",
            WindowKind::Session5h,
            Unit::Percent,
            source(),
            observed_at,
        );
        s.percent_used = Some(40.0);
        s.resets_at = Some(START + time::Duration::hours(2));
        s
    }

    #[test]
    fn failure_without_previous_has_no_figures() {
        let now = START;
        let s = AdapterFailure::unavailable("HTTP 401").into_snapshot(&template(), None, now);
        assert_eq!(s.status, Status::Unavailable);
        assert_eq!(s.observed_at, now);
        assert!(!s.has_value());
        assert_eq!(s.note.as_deref(), Some("HTTP 401"));
        assert_eq!(s.validate(), Ok(()));
        assert_eq!(s.id, "claude:default:session_5h");

        let d = AdapterFailure::degraded("odd").into_snapshot(&template(), None, now);
        assert_eq!(d.status, Status::Degraded);
    }

    #[test]
    fn failure_with_previous_carries_the_last_good_value() {
        let previous = good(START - time::Duration::minutes(5));
        let s = AdapterFailure::unavailable("HTTP 401").into_snapshot(
            &template(),
            Some(&previous),
            START,
        );
        assert_eq!(s.status, Status::Degraded);
        assert_eq!(s.percent_used, Some(40.0));
        assert_eq!(s.resets_at, previous.resets_at);
        assert_eq!(
            s.observed_at, previous.observed_at,
            "keeps the age of the value"
        );
        assert_eq!(
            s.note.as_deref(),
            Some("HTTP 401; showing the last known value")
        );
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn carried_value_past_its_reset_is_stale() {
        let previous = good(START - time::Duration::hours(3));
        let now = START + time::Duration::hours(3); // past resets_at
        let s = AdapterFailure::degraded("down").into_snapshot(&template(), Some(&previous), now);
        assert_eq!(s.status, Status::Stale);
        assert_eq!(
            s.note.as_deref(),
            Some("down; the last known value is past its reset time")
        );
    }

    #[test]
    fn previous_without_figures_or_for_another_window_is_not_carried() {
        let empty = AdapterFailure::unavailable("x").into_snapshot(&template(), None, START);
        let s = AdapterFailure::unavailable("y").into_snapshot(&template(), Some(&empty), START);
        assert!(!s.has_value());
        assert_eq!(s.note.as_deref(), Some("y"));

        let mut other = good(START);
        other.window = WindowKind::Weekly;
        other.id = snapshot_id(&other.provider, &other.account, other.window);
        let s = AdapterFailure::unavailable("z").into_snapshot(&template(), Some(&other), START);
        assert!(!s.has_value());
    }

    #[test]
    fn repeated_failures_do_not_pile_up_notes() {
        let previous = good(START - time::Duration::minutes(5));
        let first =
            AdapterFailure::degraded("down").into_snapshot(&template(), Some(&previous), START);
        let second = AdapterFailure::degraded("down").into_snapshot(
            &template(),
            Some(&first),
            START + time::Duration::minutes(1),
        );
        assert_eq!(first.note, second.note);
        assert_eq!(second.observed_at, previous.observed_at);
    }

    #[test]
    fn long_reasons_are_truncated_to_the_note_limit() {
        let s =
            AdapterFailure::unavailable("é".repeat(900)).into_snapshot(&template(), None, START);
        assert_eq!(s.validate(), Ok(()));
        assert!(s.note.unwrap().ends_with('…'));
    }

    #[test]
    fn into_snapshots_covers_every_window() {
        let weekly = SnapshotTemplate {
            window: WindowKind::Weekly,
            ..template()
        };
        let mut ctx = PollContext::new(START);
        ctx.previous = vec![good(START - time::Duration::minutes(1))];
        let out = AdapterFailure::unavailable("nope").into_snapshots(&[template(), weekly], &ctx);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].status, Status::Degraded);
        assert_eq!(out[1].status, Status::Unavailable);
        assert_eq!(out[1].id, "claude:default:weekly");
    }

    #[test]
    fn failed_poll_classification() {
        let ok = good(START);
        let bad = AdapterFailure::unavailable("x").into_snapshot(&template(), None, START);
        let mut stale = good(START);
        stale.status = Status::Stale;
        assert!(!is_failed_poll(&[]), "an empty poll is not a failure");
        assert!(!is_failed_poll(std::slice::from_ref(&ok)));
        assert!(is_failed_poll(std::slice::from_ref(&bad)));
        assert!(is_failed_poll(&[bad.clone(), bad.clone()]));
        assert!(!is_failed_poll(&[ok, bad.clone()]), "partial success");
        assert!(
            !is_failed_poll(std::slice::from_ref(&stale)),
            "stale is not a failure"
        );
        assert!(!is_failed_poll(&[stale, bad]));
    }

    #[test]
    fn next_delay_table() {
        let min = Duration::from_secs(60);
        let cap = Duration::from_secs(30 * 60);
        let cases = [
            // (failures, expected seconds) with no jitter
            (0u32, 60u64),
            (1, 120),
            (2, 240),
            (3, 480),
            (4, 960),
            (5, 1800),
            (6, 1800),
            (40, 1800),
        ];
        for (failures, secs) in cases {
            assert_eq!(
                next_delay(min, failures, cap, 0.0, 0.5),
                Duration::from_secs(secs),
                "{failures} failures"
            );
        }
    }

    #[test]
    fn next_delay_jitter_bounds() {
        let interval = Duration::from_secs(100);
        let cap = Duration::from_secs(3600);
        assert_eq!(
            next_delay(interval, 0, cap, 0.1, 0.0),
            Duration::from_secs(90)
        );
        assert_eq!(
            next_delay(interval, 0, cap, 0.1, 0.5),
            Duration::from_secs(100)
        );
        let high = next_delay(interval, 0, cap, 0.1, 0.999_999);
        assert!(high > Duration::from_secs(109) && high <= Duration::from_secs(110));
        // backoff is jittered too
        assert_eq!(
            next_delay(interval, 1, cap, 0.1, 0.0),
            Duration::from_secs(180)
        );
        // garbage inputs stay sane
        assert_eq!(
            next_delay(interval, 0, cap, f64::NAN, f64::NAN),
            Duration::from_secs(100)
        );
        assert_eq!(
            next_delay(interval, 0, cap, 5.0, 0.0),
            MIN_POLL_INTERVAL,
            "fraction clamps to 1.0, which would be zero delay, so the floor applies"
        );
    }

    #[test]
    fn next_delay_never_goes_below_the_floor_or_the_adapters_own_interval() {
        assert_eq!(
            next_delay(Duration::ZERO, 0, Duration::from_secs(10), 0.0, 0.5),
            MIN_POLL_INTERVAL
        );
        // cap smaller than the interval: never poll faster than the interval
        assert_eq!(
            next_delay(
                Duration::from_secs(600),
                3,
                Duration::from_secs(60),
                0.0,
                0.5
            ),
            Duration::from_secs(600)
        );
    }

    #[test]
    fn default_jitter_is_in_range_and_varies() {
        let jitter = default_jitter();
        let draws: Vec<f64> = (0..200).map(|_| jitter()).collect();
        assert!(draws.iter().all(|d| (0.0..1.0).contains(d)));
        let first = draws[0];
        assert!(draws.iter().any(|d| (d - first).abs() > 1e-9));
    }

    // --- Registry, driven by a fake clock ---

    struct Scripted {
        id: &'static str,
        interval: Duration,
        script: Mutex<VecDeque<Vec<Snapshot>>>,
        fallback: Vec<Snapshot>,
        polls: AtomicU32,
        contexts: Mutex<Vec<PollContext>>,
    }

    impl Scripted {
        fn new(
            id: &'static str,
            interval: Duration,
            script: Vec<Vec<Snapshot>>,
            fallback: Vec<Snapshot>,
        ) -> Arc<Self> {
            Arc::new(Scripted {
                id,
                interval,
                script: Mutex::new(script.into()),
                fallback,
                polls: AtomicU32::new(0),
                contexts: Mutex::new(Vec::new()),
            })
        }

        fn polls(&self) -> u32 {
            self.polls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Adapter for Scripted {
        fn id(&self) -> &'static str {
            self.id
        }
        fn version(&self) -> &'static str {
            "1.0.0"
        }
        fn poll_interval(&self) -> Duration {
            self.interval
        }
        async fn poll(&self, ctx: &PollContext) -> Vec<Snapshot> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            self.contexts.lock().unwrap().push(ctx.clone());
            self.script
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| self.fallback.clone())
        }
    }

    fn failing() -> Vec<Snapshot> {
        vec![AdapterFailure::unavailable("down").into_snapshot(&template(), None, START)]
    }

    fn no_jitter() -> JitterFn {
        Arc::new(|| 0.5)
    }

    fn registry(clock: &Arc<FakeClock>) -> Registry {
        Registry::new(clock.clone()).with_jitter(no_jitter())
    }

    fn drain(rx: &mut mpsc::Receiver<PollEvent>) -> Vec<PollEvent> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            out.push(event);
        }
        out
    }

    #[tokio::test]
    async fn polls_immediately_then_on_its_own_interval() {
        let clock = FakeClock::new(START);
        let adapter = Scripted::new("fast", Duration::from_secs(60), vec![], vec![good(START)]);
        let (handle, mut rx) = registry(&clock).register(adapter.clone()).start();

        FakeClock::settle().await;
        assert_eq!(adapter.polls(), 1, "first poll happens at once");
        let events = drain(&mut rx);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].adapter, "fast");
        assert_eq!(events[0].polled_at, START);
        assert!(!events[0].failed);
        assert_eq!(events[0].next_poll_in, Duration::from_secs(60));

        clock.advance(Duration::from_secs(59)).await;
        assert_eq!(adapter.polls(), 1, "not before the interval");
        clock.advance(Duration::from_secs(1)).await;
        assert_eq!(adapter.polls(), 2);
        clock.advance(Duration::from_secs(60 * 3)).await;
        assert_eq!(adapter.polls(), 5);
        assert_eq!(drain(&mut rx).len(), 4);
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn adapters_run_on_independent_intervals() {
        let clock = FakeClock::new(START);
        let fast = Scripted::new("fast", Duration::from_secs(60), vec![], vec![good(START)]);
        let slow = Scripted::new("slow", Duration::from_secs(600), vec![], vec![good(START)]);
        let (handle, mut rx) = registry(&clock)
            .register(fast.clone())
            .register(slow.clone())
            .start();
        FakeClock::settle().await;
        clock.advance(Duration::from_secs(600)).await;
        assert_eq!(fast.polls(), 11);
        assert_eq!(slow.polls(), 2);
        let events = drain(&mut rx);
        assert_eq!(events.iter().filter(|e| e.slot == 0).count(), 11);
        assert_eq!(events.iter().filter(|e| e.slot == 1).count(), 2);
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn failures_back_off_exponentially_and_success_resets() {
        let clock = FakeClock::new(START);
        // fail, fail, fail, then succeed forever
        let adapter = Scripted::new(
            "flaky",
            Duration::from_secs(60),
            vec![failing(), failing(), failing()],
            vec![good(START)],
        );
        let (handle, mut rx) = registry(&clock).register(adapter.clone()).start();
        FakeClock::settle().await;
        let first = drain(&mut rx);
        assert!(first[0].failed);
        assert_eq!(first[0].consecutive_failures, 1);
        assert_eq!(first[0].next_poll_in, Duration::from_secs(120));

        clock.advance(Duration::from_secs(119)).await;
        assert_eq!(adapter.polls(), 1);
        clock.advance(Duration::from_secs(1)).await;
        assert_eq!(adapter.polls(), 2);
        let second = drain(&mut rx);
        assert_eq!(second[0].consecutive_failures, 2);
        assert_eq!(second[0].next_poll_in, Duration::from_secs(240));

        clock.advance(Duration::from_secs(240)).await;
        assert_eq!(adapter.polls(), 3);
        let third = drain(&mut rx);
        assert_eq!(third[0].next_poll_in, Duration::from_secs(480));

        clock.advance(Duration::from_secs(480)).await;
        assert_eq!(adapter.polls(), 4);
        let fourth = drain(&mut rx);
        assert!(!fourth[0].failed);
        assert_eq!(fourth[0].consecutive_failures, 0);
        assert_eq!(
            fourth[0].next_poll_in,
            Duration::from_secs(60),
            "back to normal"
        );
        handle.shutdown().await;
    }

    /// A [`Scripted`] adapter that does not back off (like a local inbox file).
    struct LocalFile(Arc<Scripted>);

    #[async_trait]
    impl Adapter for LocalFile {
        fn id(&self) -> &'static str {
            self.0.id()
        }
        fn version(&self) -> &'static str {
            self.0.version()
        }
        fn poll_interval(&self) -> Duration {
            self.0.poll_interval()
        }
        fn backs_off(&self) -> bool {
            false
        }
        async fn poll(&self, ctx: &PollContext) -> Vec<Snapshot> {
            self.0.poll(ctx).await
        }
    }

    #[tokio::test]
    async fn an_adapter_that_does_not_back_off_keeps_its_pace_while_failing() {
        let clock = FakeClock::new(START);
        let inner = Scripted::new(
            "inbox",
            Duration::from_secs(30),
            vec![failing(), failing(), failing()],
            vec![good(START)],
        );
        let (handle, mut rx) = registry(&clock)
            .register(Arc::new(LocalFile(inner.clone())))
            .start();
        FakeClock::settle().await;
        let first = drain(&mut rx);
        assert!(first[0].failed);
        assert_eq!(
            first[0].consecutive_failures, 1,
            "still counted as a failure"
        );
        assert_eq!(
            first[0].next_poll_in,
            Duration::from_secs(30),
            "but not slowed down"
        );
        clock.advance(Duration::from_secs(90)).await;
        assert_eq!(
            inner.polls(),
            4,
            "the data that appeared is read within one interval"
        );
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn jitter_is_applied_to_the_delay() {
        let clock = FakeClock::new(START);
        let adapter = Scripted::new("j", Duration::from_secs(100), vec![], vec![good(START)]);
        let (handle, mut rx) = Registry::new(clock.clone())
            .with_jitter(Arc::new(|| 0.0))
            .with_config(RegistryConfig {
                jitter_fraction: 0.1,
                ..RegistryConfig::default()
            })
            .register(adapter.clone())
            .start();
        FakeClock::settle().await;
        assert_eq!(drain(&mut rx)[0].next_poll_in, Duration::from_secs(90));
        clock.advance(Duration::from_secs(89)).await;
        assert_eq!(adapter.polls(), 1);
        clock.advance(Duration::from_secs(1)).await;
        assert_eq!(adapter.polls(), 2);
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn context_carries_previous_snapshots_and_failure_count() {
        let clock = FakeClock::new(START);
        let adapter = Scripted::new(
            "ctx",
            Duration::from_secs(60),
            vec![vec![good(START)], failing()],
            failing(),
        );
        let (handle, _rx) = registry(&clock).register(adapter.clone()).start();
        FakeClock::settle().await;
        clock.advance(Duration::from_secs(60)).await;
        clock.advance(Duration::from_secs(120)).await;
        let contexts = adapter.contexts.lock().unwrap().clone();
        assert_eq!(contexts[0].previous.len(), 0);
        assert_eq!(contexts[0].consecutive_failures, 0);
        assert_eq!(contexts[1].previous.len(), 1);
        assert_eq!(contexts[1].previous[0].status, Status::Ok);
        assert_eq!(contexts[2].consecutive_failures, 1);
        assert_eq!(contexts[2].previous[0].status, Status::Unavailable);
        assert_eq!(contexts[2].now, START + time::Duration::seconds(180));
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_stops_polling() {
        let clock = FakeClock::new(START);
        let adapter = Scripted::new("s", Duration::from_secs(60), vec![], vec![good(START)]);
        let (handle, _rx) = registry(&clock).register(adapter.clone()).start();
        FakeClock::settle().await;
        handle.shutdown().await;
        let before = adapter.polls();
        clock.advance(Duration::from_secs(600)).await;
        assert_eq!(adapter.polls(), before);
        assert_eq!(clock.sleepers(), 0);
    }

    #[tokio::test]
    async fn dropping_the_handle_cancels_workers() {
        let clock = FakeClock::new(START);
        let adapter = Scripted::new("d", Duration::from_secs(60), vec![], vec![good(START)]);
        let (handle, _rx) = registry(&clock).register(adapter.clone()).start();
        FakeClock::settle().await;
        drop(handle);
        FakeClock::settle().await;
        let before = adapter.polls();
        clock.advance(Duration::from_secs(600)).await;
        assert_eq!(adapter.polls(), before);
    }

    #[tokio::test]
    async fn dropping_the_receiver_stops_the_worker() {
        let clock = FakeClock::new(START);
        let adapter = Scripted::new("r", Duration::from_secs(60), vec![], vec![good(START)]);
        let (handle, rx) = registry(&clock).register(adapter.clone()).start();
        FakeClock::settle().await;
        drop(rx);
        clock.advance(Duration::from_secs(60)).await;
        let after = adapter.polls();
        clock.advance(Duration::from_secs(600)).await;
        assert_eq!(
            adapter.polls(),
            after,
            "worker exited after the failed send"
        );
        handle.shutdown().await;
    }

    struct Hanging {
        started: AtomicBool,
        dropped: Arc<AtomicBool>,
    }

    struct SetOnDrop(Arc<AtomicBool>);
    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl Adapter for Hanging {
        fn id(&self) -> &'static str {
            "hanging"
        }
        fn version(&self) -> &'static str {
            "1"
        }
        fn poll_interval(&self) -> Duration {
            Duration::from_secs(60)
        }
        async fn poll(&self, _ctx: &PollContext) -> Vec<Snapshot> {
            let _guard = SetOnDrop(self.dropped.clone());
            self.started.store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
            Vec::new()
        }
    }

    #[tokio::test]
    async fn cancellation_aborts_a_poll_in_flight() {
        let clock = FakeClock::new(START);
        let dropped = Arc::new(AtomicBool::new(false));
        let adapter = Arc::new(Hanging {
            started: AtomicBool::new(false),
            dropped: dropped.clone(),
        });
        let (handle, _rx) = registry(&clock).register(adapter.clone()).start();
        FakeClock::settle().await;
        assert!(adapter.started.load(Ordering::SeqCst));
        assert!(!dropped.load(Ordering::SeqCst));
        handle.shutdown().await;
        FakeClock::settle().await;
        assert!(dropped.load(Ordering::SeqCst), "the stuck poll was aborted");
    }

    struct Panicky {
        calls: AtomicU32,
    }

    #[async_trait]
    impl Adapter for Panicky {
        fn id(&self) -> &'static str {
            "panicky"
        }
        fn version(&self) -> &'static str {
            "1"
        }
        fn poll_interval(&self) -> Duration {
            Duration::from_secs(60)
        }
        async fn poll(&self, _ctx: &PollContext) -> Vec<Snapshot> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![good(START)]
            } else {
                panic!("boom");
            }
        }
    }

    #[tokio::test]
    async fn a_panicking_adapter_degrades_instead_of_killing_the_worker() {
        let clock = FakeClock::new(START);
        let adapter = Arc::new(Panicky {
            calls: AtomicU32::new(0),
        });
        let (handle, mut rx) = registry(&clock).register(adapter.clone()).start();
        FakeClock::settle().await;
        assert!(!drain(&mut rx)[0].failed);
        clock.advance(Duration::from_secs(60)).await;
        let events = drain(&mut rx);
        assert_eq!(events.len(), 1);
        assert!(events[0].failed);
        assert_eq!(events[0].snapshots.len(), 1);
        assert_eq!(events[0].snapshots[0].status, Status::Degraded);
        assert_eq!(events[0].snapshots[0].percent_used, Some(40.0));
        // and it keeps going, with backoff
        clock.advance(Duration::from_secs(120)).await;
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 3);
        handle.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn works_with_the_system_clock_under_tokio_paused_time() {
        let adapter = Scripted::new("sys", Duration::from_secs(60), vec![], vec![good(START)]);
        let (handle, mut rx) = Registry::new(Arc::new(crate::clock::SystemClock))
            .with_jitter(no_jitter())
            .register(adapter.clone())
            .start();
        let first = rx.recv().await.unwrap();
        assert_eq!(first.adapter, "sys");
        let second = rx.recv().await.unwrap();
        assert_eq!(second.consecutive_failures, 0);
        assert_eq!(adapter.polls(), 2);
        handle.shutdown().await;
    }

    #[test]
    fn public_types_are_thread_safe() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Registry>();
        assert_send_sync::<RegistryHandle>();
        assert_send_sync::<PollEvent>();
        assert_send_sync::<AdapterFailure>();
    }

    #[test]
    fn registry_bookkeeping() {
        let clock = FakeClock::new(START);
        let reg = Registry::new(clock);
        assert!(reg.is_empty());
        let reg = reg.register(Scripted::new("a", Duration::from_secs(1), vec![], vec![]));
        assert_eq!(reg.len(), 1);
    }
}
