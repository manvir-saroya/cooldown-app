//! Reset alerts: pure, platform-neutral reconciliation.
//!
//! This is the logic behind "Notify me when my limit resets". It has no clock, no I/O and no OS
//! calls: you give it the alerts you currently hold, the latest snapshots and `now`, and it says
//! what to do ([`AlertAction`]). The desktop tray timer and the Android alarm both just apply the
//! actions.
//!
//! # Model
//!
//! An [`AlertIntent`] is one alert for one limit window, with the stable id
//! `reset:<provider>:<account>:<window>`. Alerts are replaced, never stacked: there is at most one
//! per id, and when the same id appears twice in the input the last one wins. `fireAt` is the reset
//! time the alert is armed for; the desired fire time is always the latest known `resetsAt`.
//!
//! # Rules
//!
//! - **Schedule.** A window with no alert at all (not even a disabled one) that is exhausted
//!   (`percentUsed >= auto_arm_at_percent`) and has a future `resetsAt` gets an alert armed for it.
//!   A user's explicit request is just an alert the caller stored itself.
//! - **Reschedule.** If fresh reads say the reset moved, the alert follows (replace, never stack).
//!   Shifts under 60 seconds are ignored (debounce).
//! - **Cancel.** The reset moved away (the window lost its reset time) or usage resumed (the reset
//!   moved later and `percentUsed` is back below `resume_below_percent`: the limit was lifted
//!   early), the user disabled the alert, or the alert is more than `max_late` overdue.
//! - **Fire.** At `fireAt`, unless fresh reads contradict it. A window that vanished, or lost its
//!   reset time, after the reset time means the reset happened. A fresh read that shows the reset
//!   already happened (an earlier reset time in the past, or a new window with low usage) fires
//!   right away.
//! - **Two reads.** Codex quota reads can glitch. Any decision that *changes* the plan from a read
//!   (reschedule, cancel, fire early) needs `confirm_reads` (default 2) consecutive, distinct,
//!   agreeing reads. The same read delivered twice counts once. Reads that are `stale`, `degraded`,
//!   `unavailable` or older than `max_read_age` are not evidence; with no evidence the alert simply
//!   fires on time. Firing on time is never gated on confirmation.
//! - **Never fire a disabled alert.** And never fire twice: [`Reconciler`] remembers what it fired.
//!
//! # Using it
//!
//! [`Reconciler`] holds the recent reads, so confirmation works across polls; use it in the app.
//! The free [`reconcile`] is the same logic without memory: it can confirm a change only if you
//! pass several reads of the same window in `snapshots`, otherwise changes are held.
//! [`should_fire`] is the single-read gate to run just before showing a notification.
//!
//! After `Fire`, remove the alert (it has served its purpose); [`apply_actions`] does the
//! bookkeeping on a list of intents.

use std::collections::{HashMap, HashSet};

use time::{Duration, OffsetDateTime};

use crate::model::{AlertIntent, Snapshot, Status, WindowKind};

/// Why an alert was cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    /// The user switched it off. The intent stays (it records the choice); disarm any timer.
    Disabled,
    /// The window no longer has the reset time the alert was armed for.
    ResetMovedAway,
    /// The reset moved later and usage dropped: the limit was lifted early.
    UsageResumed,
    /// The alert is long overdue (the machine was off); firing now would only mislead.
    Expired,
}

/// Why an alert fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireReason {
    /// The reset time arrived.
    Due,
    /// A fresh read shows the reset has already happened.
    ResetObserved,
    /// The window vanished (or lost its reset time) after the reset time.
    WindowGone,
}

/// One thing to do about an alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlertAction {
    /// Arm a new alert.
    Schedule {
        /// Alert id.
        id: String,
        /// When to fire.
        fire_at: OffsetDateTime,
    },
    /// Move an armed alert (replaces the timer, never adds one).
    Reschedule {
        /// Alert id.
        id: String,
        /// Old fire time.
        from: OffsetDateTime,
        /// New fire time.
        to: OffsetDateTime,
    },
    /// Disarm an alert. Idempotent.
    Cancel {
        /// Alert id.
        id: String,
        /// Why.
        reason: CancelReason,
    },
    /// Show the notification now, once.
    Fire {
        /// Alert id.
        id: String,
        /// The time the alert was armed for.
        fire_at: OffsetDateTime,
        /// Why.
        reason: FireReason,
    },
}

impl AlertAction {
    /// The alert this action is about.
    pub fn id(&self) -> &str {
        match self {
            AlertAction::Schedule { id, .. }
            | AlertAction::Reschedule { id, .. }
            | AlertAction::Cancel { id, .. }
            | AlertAction::Fire { id, .. } => id,
        }
    }
}

/// Tuning for reconciliation. The defaults are the product's.
#[derive(Debug, Clone)]
pub struct ReconcileConfig {
    /// Reset-time shifts smaller than this are ignored. Default 60 seconds.
    pub debounce: Duration,
    /// Consecutive distinct agreeing reads needed before a read changes the plan. Default 2.
    pub confirm_reads: usize,
    /// Reads older than this are not evidence. Default 15 minutes.
    pub max_read_age: Duration,
    /// How early [`should_fire`] accepts a timer that wakes slightly before `fireAt`. Default 30 s.
    pub fire_lead: Duration,
    /// An alert overdue by more than this is dropped instead of fired. Default 2 hours.
    pub max_late: Duration,
    /// A moved-later reset with `percentUsed` below this means usage resumed. Default 90.
    pub resume_below_percent: f64,
    /// Windows at or above this percent get an alert automatically. `None` turns auto-arming off.
    /// Default `None`: alerts are armed by the user's "Notify me when it resets" button; auto-arming
    /// is an opt-in setting (for example `Some(99.0)`).
    pub auto_arm_at_percent: Option<f64>,
}

impl Default for ReconcileConfig {
    fn default() -> Self {
        ReconcileConfig {
            debounce: Duration::seconds(60),
            confirm_reads: 2,
            max_read_age: Duration::minutes(15),
            fire_lead: Duration::seconds(30),
            max_late: Duration::hours(2),
            resume_below_percent: 90.0,
            auto_arm_at_percent: None,
        }
    }
}

/// What one read says about an armed alert.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Change {
    /// The reset moved to this future time.
    MovedTo(OffsetDateTime),
    /// The reset already happened.
    ResetObserved,
    /// The reset went away or the limit was lifted.
    Gone(CancelReason),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Stance {
    /// The read agrees with the alert.
    Agrees,
    /// The alert is due and the window lost its reset time: the reset happened.
    Vanished,
    /// The read disagrees.
    Differs(Change),
}

fn same_change(a: &Change, b: &Change, debounce: Duration) -> bool {
    match (a, b) {
        (Change::MovedTo(x), Change::MovedTo(y)) => (*x - *y).abs() < debounce,
        (Change::ResetObserved, Change::ResetObserved) => true,
        (Change::Gone(x), Change::Gone(y)) => x == y,
        _ => false,
    }
}

/// Classify one fresh read against an alert armed for `fire_at`. `due` is whether the alert's time
/// has come.
fn stance(
    cfg: &ReconcileConfig,
    fire_at: OffsetDateTime,
    due: bool,
    read: &Snapshot,
    now: OffsetDateTime,
) -> Stance {
    // A read stamped in the future (clock skew) is treated as taken just now.
    let observed = read.observed_at.min(now);
    let usage_low = read
        .percent_used
        .is_some_and(|p| p < cfg.resume_below_percent);
    match read.resets_at {
        Some(reset) => {
            if (reset - fire_at).abs() < cfg.debounce {
                Stance::Agrees
            } else if reset <= now {
                Stance::Differs(Change::ResetObserved)
            } else if reset < fire_at {
                Stance::Differs(Change::MovedTo(reset))
            } else if observed < fire_at - cfg.debounce {
                // Taken before the alert was due, and the reset moved later.
                if usage_low {
                    Stance::Differs(Change::Gone(CancelReason::UsageResumed))
                } else {
                    Stance::Differs(Change::MovedTo(reset))
                }
            } else if usage_low || read.percent_used.is_none() {
                // Taken at or after the due time: a new window started.
                Stance::Differs(Change::ResetObserved)
            } else {
                // Still exhausted, with the reset pushed out.
                Stance::Differs(Change::MovedTo(reset))
            }
        }
        None if due => Stance::Vanished,
        None => Stance::Differs(Change::Gone(if usage_low {
            CancelReason::UsageResumed
        } else {
            CancelReason::ResetMovedAway
        })),
    }
}

fn age(now: OffsetDateTime, observed_at: OffsetDateTime) -> Duration {
    (now - observed_at).max(Duration::ZERO)
}

/// Reads of one window: only `ok` ones, oldest first, one per `observedAt` (the last delivered
/// wins).
fn usable_reads<'a>(reads: &[&'a Snapshot]) -> Vec<&'a Snapshot> {
    let mut usable: Vec<&Snapshot> = reads
        .iter()
        .copied()
        .filter(|s| s.status == Status::Ok)
        .collect();
    usable.sort_by_key(|s| s.observed_at); // stable: delivery order is kept for equal stamps
    let mut out: Vec<&Snapshot> = Vec::with_capacity(usable.len());
    for read in usable {
        match out.last() {
            Some(last) if last.observed_at == read.observed_at => {
                out.pop();
                out.push(read);
            }
            _ => out.push(read),
        }
    }
    out
}

/// Whether the newest `confirm_reads` reads all report the same change as the latest one.
fn confirmed(
    cfg: &ReconcileConfig,
    fire_at: OffsetDateTime,
    due: bool,
    reads: &[&Snapshot],
    change: &Change,
    now: OffsetDateTime,
) -> bool {
    let needed = cfg.confirm_reads.max(1);
    if reads.len() < needed {
        return false;
    }
    reads[reads.len() - needed..].iter().all(|read| {
        matches!(
            stance(cfg, fire_at, due, read, now),
            Stance::Differs(other) if same_change(&other, change, cfg.debounce)
        )
    })
}

/// Decide what to do about one existing alert. At most one action.
fn decide(
    cfg: &ReconcileConfig,
    intent: &AlertIntent,
    reads: &[&Snapshot],
    now: OffsetDateTime,
) -> Option<AlertAction> {
    let id = intent.id.clone();
    let fire_at = intent.fire_at;

    if !intent.enabled {
        // Nothing may fire. If a timer could still be armed from before the switch-off, say so.
        return (fire_at > now).then_some(AlertAction::Cancel {
            id,
            reason: CancelReason::Disabled,
        });
    }
    if now - fire_at > cfg.max_late {
        return Some(AlertAction::Cancel {
            id,
            reason: CancelReason::Expired,
        });
    }

    let due = now >= fire_at;
    let usable = usable_reads(reads);
    let evidence = usable
        .last()
        .copied()
        .filter(|read| age(now, read.observed_at) <= cfg.max_read_age);

    let Some(latest) = evidence else {
        // Nothing fresh to contradict the plan: keep time.
        return due.then_some(AlertAction::Fire {
            id,
            fire_at,
            reason: if reads.is_empty() {
                FireReason::WindowGone
            } else {
                FireReason::Due
            },
        });
    };

    match stance(cfg, fire_at, due, latest, now) {
        Stance::Agrees => due.then_some(AlertAction::Fire {
            id,
            fire_at,
            reason: FireReason::Due,
        }),
        Stance::Vanished => Some(AlertAction::Fire {
            id,
            fire_at,
            reason: FireReason::WindowGone,
        }),
        Stance::Differs(change) => {
            let confirmed = confirmed(cfg, fire_at, due, &usable, &change, now);
            match change {
                // A reset that already happened: due means we were about to fire anyway.
                Change::ResetObserved => (due || confirmed).then_some(AlertAction::Fire {
                    id,
                    fire_at,
                    reason: FireReason::ResetObserved,
                }),
                Change::MovedTo(to) => confirmed.then_some(AlertAction::Reschedule {
                    id,
                    from: fire_at,
                    to,
                }),
                Change::Gone(reason) => confirmed.then_some(AlertAction::Cancel { id, reason }),
            }
        }
    }
}

/// Should a new alert be armed for this window? Returns the reset time to arm for.
fn auto_arm(
    cfg: &ReconcileConfig,
    reads: &[&Snapshot],
    now: OffsetDateTime,
) -> Option<OffsetDateTime> {
    let threshold = cfg.auto_arm_at_percent?;
    let latest = *usable_reads(reads).last()?;
    if latest.window == WindowKind::Balance || age(now, latest.observed_at) > cfg.max_read_age {
        return None;
    }
    let reset = latest.resets_at.filter(|r| *r > now)?;
    let percent = latest.percent_used?;
    (percent >= threshold).then_some(reset)
}

/// Reconcile with explicit configuration. See the module docs for the rules.
///
/// `previous` is the alerts you hold; `snapshots` the latest reads (several reads of one window may
/// be included, which is how a stateless caller supplies the extra reads confirmation needs).
/// Actions come out in the order of `previous`, then new schedules in the order of `snapshots`.
pub fn reconcile_with(
    cfg: &ReconcileConfig,
    previous: &[AlertIntent],
    snapshots: &[Snapshot],
    now: OffsetDateTime,
) -> Vec<AlertAction> {
    // One alert per id: the last one in the input wins, at the position of the first.
    let mut order: Vec<&str> = Vec::new();
    let mut intents: HashMap<&str, &AlertIntent> = HashMap::new();
    for intent in previous {
        if intents.insert(intent.id.as_str(), intent).is_none() {
            order.push(intent.id.as_str());
        }
    }

    let mut reads: HashMap<String, Vec<&Snapshot>> = HashMap::new();
    let mut read_order: Vec<String> = Vec::new();
    for snapshot in snapshots {
        let id = snapshot.alert_id();
        let entry = reads.entry(id.clone()).or_default();
        if entry.is_empty() {
            read_order.push(id);
        }
        entry.push(snapshot);
    }

    let mut actions = Vec::new();
    for id in order {
        let intent = intents[id];
        let window_reads = reads.get(id).map(Vec::as_slice).unwrap_or(&[]);
        actions.extend(decide(cfg, intent, window_reads, now));
    }
    for id in read_order {
        if intents.contains_key(id.as_str()) {
            continue;
        }
        if let Some(fire_at) = auto_arm(cfg, &reads[&id], now) {
            actions.push(AlertAction::Schedule { id, fire_at });
        }
    }
    actions
}

/// [`reconcile_with`] using the default configuration and no memory between calls. With one read
/// per window, a changed reset time is held (unconfirmed) rather than acted on; use [`Reconciler`]
/// to carry reads across polls.
pub fn reconcile(
    previous: &[AlertIntent],
    snapshots: &[Snapshot],
    now: OffsetDateTime,
) -> Vec<AlertAction> {
    reconcile_with(&ReconcileConfig::default(), previous, snapshots, now)
}

/// The single-read gate to run just before showing a notification: re-verify against the latest
/// snapshot using the default configuration.
pub fn should_fire(alert: &AlertIntent, latest: Option<&Snapshot>, now: OffsetDateTime) -> bool {
    should_fire_with(&ReconcileConfig::default(), alert, latest, now)
}

/// [`should_fire`] with explicit configuration.
///
/// True when the alert is enabled, due (a timer may wake up to `fire_lead` early), not overdue past
/// `max_late`, and the latest snapshot does not contradict it. A fresh `ok` read that says the
/// reset moved away, or that usage resumed, vetoes the fire; a read that says the reset already
/// happened, a vanished window, a missing or stale snapshot do not. A veto is only a pause: the
/// alert stays armed and [`Reconciler`] decides, with confirmation, on later reads.
pub fn should_fire_with(
    cfg: &ReconcileConfig,
    alert: &AlertIntent,
    latest: Option<&Snapshot>,
    now: OffsetDateTime,
) -> bool {
    if !alert.enabled {
        return false;
    }
    let fire_at = alert.fire_at;
    if now < fire_at - cfg.fire_lead || now - fire_at > cfg.max_late {
        return false;
    }
    let evidence = latest.filter(|read| {
        read.status == Status::Ok
            && read.alert_id() == alert.id
            && age(now, read.observed_at) <= cfg.max_read_age
    });
    match evidence {
        None => true,
        Some(read) => !matches!(
            stance(cfg, fire_at, true, read, now),
            Stance::Differs(Change::MovedTo(_) | Change::Gone(_))
        ),
    }
}

/// Reconciliation with memory: remembers the last few `ok` reads of each window so that confirmation
/// works across polls, and remembers what it fired so nothing fires twice.
#[derive(Debug, Clone, Default)]
pub struct Reconciler {
    config: ReconcileConfig,
    reads: HashMap<String, Vec<Snapshot>>,
    fired: HashSet<(String, i64)>,
}

impl Reconciler {
    /// A reconciler with this configuration.
    pub fn new(config: ReconcileConfig) -> Self {
        Reconciler {
            config,
            reads: HashMap::new(),
            fired: HashSet::new(),
        }
    }

    /// The configuration in use.
    pub fn config(&self) -> &ReconcileConfig {
        &self.config
    }

    /// Change the configuration (a setting changed). The read memory and the fired set are kept,
    /// so nothing that already rang rings again.
    pub fn set_config(&mut self, config: ReconcileConfig) {
        self.config = config;
    }

    /// Feed the latest snapshots (all of them, every poll; repeats are harmless) and get the
    /// actions to apply.
    pub fn reconcile(
        &mut self,
        previous: &[AlertIntent],
        snapshots: &[Snapshot],
        now: OffsetDateTime,
    ) -> Vec<AlertAction> {
        let keep = self.config.confirm_reads.max(1);
        for snapshot in snapshots.iter().filter(|s| s.status == Status::Ok) {
            let memory = self.reads.entry(snapshot.alert_id()).or_default();
            match memory
                .iter_mut()
                .find(|old| old.observed_at == snapshot.observed_at)
            {
                Some(old) => *old = snapshot.clone(),
                None => memory.push(snapshot.clone()),
            }
            memory.sort_by_key(|s| s.observed_at);
            if memory.len() > keep {
                memory.drain(..memory.len() - keep);
            }
        }

        // Remembered reads first, then this call's snapshots, so on equal stamps the new one wins.
        let mut all: Vec<Snapshot> = self.reads.values().flatten().cloned().collect();
        all.sort_by_key(|s| (s.alert_id(), s.observed_at));
        all.extend(snapshots.iter().cloned());

        let mut actions = reconcile_with(&self.config, previous, &all, now);
        actions.retain(|action| match action {
            AlertAction::Fire { id, fire_at, .. } => {
                self.fired.insert((id.clone(), fire_at.unix_timestamp()))
            }
            _ => true,
        });
        let horizon = (now - Duration::days(1)).unix_timestamp();
        self.fired.retain(|(_, fire_at)| *fire_at >= horizon);
        actions
    }
}

/// Apply actions to a list of intents, the bookkeeping every caller needs:
///
/// - `Schedule` adds an enabled intent (replacing any with that id);
/// - `Reschedule` moves `fireAt`;
/// - `Fire` removes the intent (it has served its purpose);
/// - `Cancel` removes it, except `Disabled`, which leaves the user's switch where it is.
pub fn apply_actions(intents: &mut Vec<AlertIntent>, actions: &[AlertAction]) {
    for action in actions {
        match action {
            AlertAction::Schedule { id, fire_at } => {
                intents.retain(|i| i.id != *id);
                intents.push(AlertIntent {
                    id: id.clone(),
                    fire_at: *fire_at,
                    enabled: true,
                });
            }
            AlertAction::Reschedule { id, to, .. } => {
                for intent in intents.iter_mut().filter(|i| i.id == *id) {
                    intent.fire_at = *to;
                }
            }
            AlertAction::Fire { id, .. } => intents.retain(|i| i.id != *id),
            AlertAction::Cancel { id, reason } => {
                if *reason != CancelReason::Disabled {
                    intents.retain(|i| i.id != *id);
                }
            }
        }
    }
}

/// The title of the "your limit is back" notification: "Claude 5-hour limit is back". The phone
/// and the PC word it the same way, so the one text lives here. An unknown provider keeps its own id.
pub fn reset_title(provider: &str, window: WindowKind) -> String {
    format!("{} is back", limit_label(provider, window))
}

/// How a notification names a limit: "Claude 5-hour limit". An unknown provider keeps its own id.
pub fn limit_label(provider: &str, window: WindowKind) -> String {
    let provider = match provider {
        "claude" => "Claude",
        "codex" => "Codex",
        "openrouter" => "OpenRouter",
        "deepseek" => "DeepSeek",
        "xai" => "xAI",
        other => other,
    };
    let window = match window {
        WindowKind::Session5h => "5-hour limit",
        WindowKind::Weekly => "weekly limit",
        WindowKind::Monthly => "monthly limit",
        WindowKind::Daily => "daily limit",
        WindowKind::Balance => "balance",
    };
    format!("{provider} {window}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SnapshotSource, SourceKind, Unit};
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-10-10 12:00:00 UTC);
    const ID: &str = "reset:claude:default:session_5h";

    fn mins(n: i64) -> Duration {
        Duration::minutes(n)
    }

    fn secs(n: i64) -> Duration {
        Duration::seconds(n)
    }

    fn read(
        window: WindowKind,
        percent: Option<f64>,
        resets_at: Option<OffsetDateTime>,
        observed_at: OffsetDateTime,
    ) -> Snapshot {
        let mut s = Snapshot::new(
            "claude",
            "default",
            window,
            Unit::Percent,
            SnapshotSource {
                kind: SourceKind::Official,
                adapter: "claude-statusline".into(),
                adapter_version: "0.1.0".into(),
            },
            observed_at,
        );
        s.percent_used = percent;
        s.resets_at = resets_at;
        s
    }

    fn session(
        percent: Option<f64>,
        resets_at: Option<OffsetDateTime>,
        observed_at: OffsetDateTime,
    ) -> Snapshot {
        read(WindowKind::Session5h, percent, resets_at, observed_at)
    }

    fn with_status(mut s: Snapshot, status: Status) -> Snapshot {
        s.status = status;
        s
    }

    fn alert(fire_at: OffsetDateTime) -> AlertIntent {
        AlertIntent::new(ID, fire_at)
    }

    fn disabled(fire_at: OffsetDateTime) -> AlertIntent {
        AlertIntent {
            enabled: false,
            ..alert(fire_at)
        }
    }

    fn fire(fire_at: OffsetDateTime, reason: FireReason) -> AlertAction {
        AlertAction::Fire {
            id: ID.into(),
            fire_at,
            reason,
        }
    }

    fn cancel(reason: CancelReason) -> AlertAction {
        AlertAction::Cancel {
            id: ID.into(),
            reason,
        }
    }

    fn reschedule(from: OffsetDateTime, to: OffsetDateTime) -> AlertAction {
        AlertAction::Reschedule {
            id: ID.into(),
            from,
            to,
        }
    }

    /// Auto-arming is opt-in in the product (the default is `None`); these tests cover it, so they
    /// opt in with the threshold the feature was designed around.
    fn auto_cfg() -> ReconcileConfig {
        ReconcileConfig {
            auto_arm_at_percent: Some(99.0),
            ..ReconcileConfig::default()
        }
    }

    fn run(alerts: &[AlertIntent], reads: &[Snapshot], now: OffsetDateTime) -> Vec<AlertAction> {
        reconcile_with(&auto_cfg(), alerts, reads, now)
    }

    /// The alert is armed for one hour from NOW unless a case says otherwise.
    fn f() -> OffsetDateTime {
        NOW + mins(60)
    }

    /// An exhausted read taken one minute ago whose reset time is `reset`.
    fn exhausted(reset: OffsetDateTime) -> Snapshot {
        session(Some(100.0), Some(reset), NOW - mins(1))
    }

    // ---- firing on time ----

    #[test]
    fn fires_when_due_and_the_read_agrees() {
        let fire_at = NOW - secs(5);
        let actions = run(
            &[alert(fire_at)],
            &[session(Some(100.0), Some(fire_at), NOW - mins(1))],
            NOW,
        );
        assert_eq!(actions, vec![fire(fire_at, FireReason::Due)]);
    }

    #[test]
    fn due_boundary_is_inclusive_and_early_is_quiet() {
        let r = exhausted(f());
        assert_eq!(
            run(&[alert(f())], std::slice::from_ref(&r), f()),
            vec![fire(f(), FireReason::Due)]
        );
        assert!(run(
            &[alert(f())],
            std::slice::from_ref(&r),
            f() - Duration::nanoseconds(1)
        )
        .is_empty());
        assert!(run(&[alert(f())], &[r], NOW).is_empty());
    }

    #[test]
    fn small_reset_shifts_are_debounced() {
        for shift in [secs(-59), secs(-1), secs(0), secs(1), secs(59)] {
            let reads = [
                session(Some(100.0), Some(f() + shift), NOW - mins(2)),
                session(Some(100.0), Some(f() + shift), NOW - mins(1)),
            ];
            assert!(run(&[alert(f())], &reads, NOW).is_empty(), "shift {shift}");
        }
    }

    #[test]
    fn a_shift_of_exactly_the_debounce_counts() {
        let reads = [
            session(Some(100.0), Some(f() + secs(60)), NOW - mins(2)),
            session(Some(100.0), Some(f() + secs(60)), NOW - mins(1)),
        ];
        assert_eq!(
            run(&[alert(f())], &reads, NOW),
            vec![reschedule(f(), f() + secs(60))]
        );
    }

    // ---- reset time moves: two consistent reads ----

    #[test]
    fn a_changed_reset_needs_two_consistent_reads() {
        let later = f() + mins(30);
        let one = [session(Some(100.0), Some(later), NOW - mins(1))];
        assert!(run(&[alert(f())], &one, NOW).is_empty(), "one read is held");

        let two = [
            session(Some(100.0), Some(later), NOW - mins(2)),
            session(Some(100.0), Some(later), NOW - mins(1)),
        ];
        assert_eq!(run(&[alert(f())], &two, NOW), vec![reschedule(f(), later)]);
    }

    #[test]
    fn two_reads_within_the_debounce_agree_and_the_latest_wins() {
        let reads = [
            session(Some(100.0), Some(f() + mins(30)), NOW - mins(2)),
            session(Some(100.0), Some(f() + mins(30) + secs(30)), NOW - mins(1)),
        ];
        assert_eq!(
            run(&[alert(f())], &reads, NOW),
            vec![reschedule(f(), f() + mins(30) + secs(30))]
        );
    }

    #[test]
    fn two_reads_that_disagree_with_each_other_change_nothing() {
        let reads = [
            session(Some(100.0), Some(f() + mins(30)), NOW - mins(2)),
            session(Some(100.0), Some(f() + mins(90)), NOW - mins(1)),
        ];
        assert!(run(&[alert(f())], &reads, NOW).is_empty());
    }

    #[test]
    fn a_glitch_between_two_good_reads_is_ignored() {
        let reads = [
            session(Some(100.0), Some(f()), NOW - mins(3)),
            session(Some(100.0), Some(f() + mins(300)), NOW - mins(2)), // glitch
            session(Some(100.0), Some(f()), NOW - mins(1)),
        ];
        assert!(run(&[alert(f())], &reads, NOW).is_empty());
    }

    #[test]
    fn duplicate_reads_count_once() {
        let later = f() + mins(30);
        let dup = session(Some(100.0), Some(later), NOW - mins(1));
        assert!(run(&[alert(f())], &[dup.clone(), dup], NOW).is_empty());
    }

    #[test]
    fn reads_given_out_of_order_are_sorted_by_observation_time() {
        let later = f() + mins(30);
        let reads = [
            session(Some(100.0), Some(later), NOW - mins(1)),
            session(Some(100.0), Some(f()), NOW - mins(2)), // older, agrees
        ];
        // newest-first input: the latest read moved, the one before agreed: unconfirmed
        assert!(run(&[alert(f())], &reads, NOW).is_empty());
    }

    #[test]
    fn the_reset_moving_earlier_into_the_future_reschedules() {
        let earlier = f() - mins(20);
        let reads = [
            session(Some(100.0), Some(earlier), NOW - mins(2)),
            session(Some(100.0), Some(earlier), NOW - mins(1)),
        ];
        assert_eq!(
            run(&[alert(f())], &reads, NOW),
            vec![reschedule(f(), earlier)]
        );
    }

    #[test]
    fn a_reset_that_already_passed_fires_after_confirmation_even_if_not_yet_due() {
        let past = NOW - mins(5);
        let one = [session(Some(100.0), Some(past), NOW - mins(1))];
        assert!(run(&[alert(f())], &one, NOW).is_empty());
        let two = [
            session(Some(100.0), Some(past), NOW - mins(2)),
            session(Some(100.0), Some(past), NOW - mins(1)),
        ];
        assert_eq!(
            run(&[alert(f())], &two, NOW),
            vec![fire(f(), FireReason::ResetObserved)]
        );
    }

    #[test]
    fn due_alerts_fire_on_one_read_that_shows_the_reset_happened() {
        let fire_at = NOW - mins(1);
        // new window started: later reset, usage low
        let rolled = session(Some(2.0), Some(NOW + mins(300)), NOW - secs(10));
        assert_eq!(
            run(&[alert(fire_at)], &[rolled], NOW),
            vec![fire(fire_at, FireReason::ResetObserved)]
        );
        // the old reset time is in the past by a margin
        let earlier = session(Some(100.0), Some(fire_at - mins(10)), NOW - secs(10));
        assert_eq!(
            run(&[alert(fire_at)], &[earlier], NOW),
            vec![fire(fire_at, FireReason::ResetObserved)]
        );
    }

    #[test]
    fn a_new_window_with_unknown_usage_counts_as_reset_once_due() {
        let fire_at = NOW - mins(1);
        let rolled = session(None, Some(NOW + mins(300)), NOW - secs(10));
        assert_eq!(
            run(&[alert(fire_at)], &[rolled], NOW),
            vec![fire(fire_at, FireReason::ResetObserved)]
        );
    }

    #[test]
    fn still_exhausted_with_the_reset_pushed_out_holds_then_reschedules() {
        let fire_at = NOW - mins(1);
        let pushed = NOW + mins(90);
        let one = [session(Some(100.0), Some(pushed), NOW - secs(10))];
        assert!(
            run(&[alert(fire_at)], &one, NOW).is_empty(),
            "held, not fired"
        );
        let two = [
            session(Some(100.0), Some(pushed), NOW - mins(1)),
            session(Some(100.0), Some(pushed), NOW - secs(10)),
        ];
        assert_eq!(
            run(&[alert(fire_at)], &two, NOW),
            vec![reschedule(fire_at, pushed)]
        );
    }

    // ---- cancel: reset moves away, usage resumed ----

    #[test]
    fn losing_the_reset_time_before_it_is_due_cancels_after_confirmation() {
        let one = [session(Some(100.0), None, NOW - mins(1))];
        assert!(run(&[alert(f())], &one, NOW).is_empty());
        let two = [
            session(Some(100.0), None, NOW - mins(2)),
            session(Some(100.0), None, NOW - mins(1)),
        ];
        assert_eq!(
            run(&[alert(f())], &two, NOW),
            vec![cancel(CancelReason::ResetMovedAway)]
        );
        let unknown_usage = [
            session(None, None, NOW - mins(2)),
            session(None, None, NOW - mins(1)),
        ];
        assert_eq!(
            run(&[alert(f())], &unknown_usage, NOW),
            vec![cancel(CancelReason::ResetMovedAway)]
        );
    }

    #[test]
    fn usage_resumed_cancels_after_confirmation() {
        // window rolled early: usage back near zero and the reset is a whole new window away
        let rolled = f() + mins(300);
        let reads = [
            session(Some(3.0), Some(rolled), NOW - mins(2)),
            session(Some(4.0), Some(rolled), NOW - mins(1)),
        ];
        assert_eq!(
            run(&[alert(f())], &reads, NOW),
            vec![cancel(CancelReason::UsageResumed)]
        );
        // a single such read is held
        assert!(run(&[alert(f())], &reads[1..], NOW).is_empty());
        // usage reset with no reset time at all
        let gone = [
            session(Some(0.0), None, NOW - mins(2)),
            session(Some(0.0), None, NOW - mins(1)),
        ];
        assert_eq!(
            run(&[alert(f())], &gone, NOW),
            vec![cancel(CancelReason::UsageResumed)]
        );
    }

    #[test]
    fn the_resume_threshold_is_exclusive() {
        let later = f() + mins(300);
        let at = |p: f64| {
            [
                session(Some(p), Some(later), NOW - mins(2)),
                session(Some(p), Some(later), NOW - mins(1)),
            ]
        };
        assert_eq!(
            run(&[alert(f())], &at(89.9), NOW),
            vec![cancel(CancelReason::UsageResumed)]
        );
        assert_eq!(
            run(&[alert(f())], &at(90.0), NOW),
            vec![reschedule(f(), later)]
        );
    }

    // ---- window vanished ----

    #[test]
    fn a_vanished_window_fires_once_the_reset_time_has_passed() {
        let fire_at = NOW - mins(1);
        assert_eq!(
            run(&[alert(fire_at)], &[], NOW),
            vec![fire(fire_at, FireReason::WindowGone)]
        );
        // an unrelated window being present does not matter
        let other = read(WindowKind::Weekly, Some(10.0), Some(NOW + mins(900)), NOW);
        assert_eq!(
            run(&[alert(fire_at)], &[other], NOW),
            vec![fire(fire_at, FireReason::WindowGone)]
        );
    }

    #[test]
    fn a_vanished_window_before_the_reset_time_changes_nothing() {
        assert!(run(&[alert(f())], &[], NOW).is_empty());
    }

    #[test]
    fn an_ok_read_without_a_reset_time_after_the_due_time_means_reset_happened() {
        // This is what the Claude adapter emits once the window is dropped: percent 0, no reset.
        let fire_at = NOW - mins(1);
        let dropped = session(Some(0.0), None, NOW - secs(5));
        assert_eq!(
            run(&[alert(fire_at)], &[dropped], NOW),
            vec![fire(fire_at, FireReason::WindowGone)]
        );
    }

    // ---- reads that are not evidence ----

    #[test]
    fn stale_degraded_and_unavailable_reads_are_ignored() {
        let contradicting = session(Some(100.0), Some(f() + mins(300)), NOW - mins(1));
        for status in [Status::Stale, Status::Degraded, Status::Unavailable] {
            let reads = [
                with_status(contradicting.clone(), status),
                with_status(contradicting.clone(), status),
            ];
            // not due: no reschedule from a bad read
            assert!(run(&[alert(f())], &reads, NOW).is_empty(), "{status:?}");
            // due: fires on time anyway
            let fire_at = NOW - secs(5);
            assert_eq!(
                run(&[alert(fire_at)], &reads, NOW),
                vec![fire(fire_at, FireReason::Due)],
                "{status:?}"
            );
        }
    }

    #[test]
    fn reads_older_than_the_max_age_are_ignored() {
        let later = f() + mins(300);
        let old = |m: i64| session(Some(100.0), Some(later), NOW - mins(m));
        // 16 minutes old and older: not evidence
        assert!(run(&[alert(f())], &[old(18), old(16)], NOW).is_empty());
        // exactly at the limit still counts
        assert_eq!(
            run(&[alert(f())], &[old(20), old(15)], NOW),
            vec![reschedule(f(), later)]
        );
        // an old contradicting read does not stop a due alert
        let fire_at = NOW - secs(5);
        assert_eq!(
            run(&[alert(fire_at)], &[old(18), old(16)], NOW),
            vec![fire(fire_at, FireReason::Due)]
        );
    }

    #[test]
    fn a_stale_read_after_the_reset_does_not_block_firing() {
        // The adapter says "stale: past its reset time": that is exactly the due case.
        let fire_at = NOW - mins(1);
        let stale = with_status(
            session(Some(100.0), Some(fire_at), NOW - mins(10)),
            Status::Stale,
        );
        assert_eq!(
            run(&[alert(fire_at)], &[stale], NOW),
            vec![fire(fire_at, FireReason::Due)]
        );
    }

    // ---- clock skew ----

    #[test]
    fn reads_stamped_in_the_future_are_fresh_and_treated_as_just_taken() {
        let later = f() + mins(30);
        let reads = [
            session(Some(100.0), Some(later), NOW + mins(10)),
            session(Some(100.0), Some(later), NOW + mins(20)),
        ];
        assert_eq!(
            run(&[alert(f())], &reads, NOW),
            vec![reschedule(f(), later)]
        );

        // a future stamp must not make a pre-due read look like it was taken after the due time:
        // usage is low and the reset is far, so this is "usage resumed", not "reset observed"
        let rolled = f() + mins(300);
        let skewed = [
            session(Some(1.0), Some(rolled), f() + mins(5)),
            session(Some(1.0), Some(rolled), f() + mins(6)),
        ];
        assert_eq!(
            run(&[alert(f())], &skewed, NOW),
            vec![cancel(CancelReason::UsageResumed)]
        );
    }

    #[test]
    fn a_reset_time_in_the_past_when_scheduling_is_never_armed() {
        let r = session(Some(100.0), Some(NOW - secs(1)), NOW - mins(1));
        assert!(run(&[], &[r], NOW).is_empty());
        let exactly_now = session(Some(100.0), Some(NOW), NOW - mins(1));
        assert!(run(&[], &[exactly_now], NOW).is_empty());
    }

    // ---- disabled and expired ----

    #[test]
    fn a_disabled_alert_never_fires() {
        let fire_at = NOW - secs(5);
        let agreeing = session(Some(100.0), Some(fire_at), NOW - mins(1));
        assert!(run(&[disabled(fire_at)], std::slice::from_ref(&agreeing), NOW).is_empty());
        assert!(run(&[disabled(fire_at)], &[], NOW).is_empty());
        let rolled = session(Some(0.0), Some(NOW + mins(300)), NOW - mins(1));
        assert!(run(&[disabled(fire_at)], &[rolled], NOW).is_empty());
    }

    #[test]
    fn a_disabled_alert_in_the_future_is_cancelled_so_no_timer_survives() {
        assert_eq!(
            run(&[disabled(f())], &[exhausted(f())], NOW),
            vec![cancel(CancelReason::Disabled)]
        );
    }

    #[test]
    fn a_disabled_alert_suppresses_auto_arming() {
        assert_eq!(
            run(&[disabled(f())], &[exhausted(f())], NOW).len(),
            1,
            "only the cancel, no new schedule"
        );
    }

    #[test]
    fn an_overdue_alert_is_dropped_not_fired() {
        let fire_at = NOW - mins(121);
        let r = session(Some(100.0), Some(fire_at), NOW - mins(1));
        assert_eq!(
            run(&[alert(fire_at)], std::slice::from_ref(&r), NOW),
            vec![cancel(CancelReason::Expired)]
        );
        // exactly at the limit still fires
        let fire_at = NOW - mins(120);
        assert_eq!(
            run(&[alert(fire_at)], &[], NOW),
            vec![fire(fire_at, FireReason::WindowGone)]
        );
        let late = NOW - mins(90);
        assert_eq!(
            run(&[alert(late)], &[], NOW),
            vec![fire(late, FireReason::WindowGone)]
        );
    }

    // ---- one alert per window, replace never stack ----

    #[test]
    fn duplicate_intents_collapse_to_one_and_the_last_wins() {
        let old = alert(NOW - mins(30)); // would be due
        let newer = alert(f());
        let actions = run(&[old, newer], &[exhausted(f())], NOW);
        assert!(
            actions.is_empty(),
            "the later intent (not due) governs: {actions:?}"
        );

        let actions = run(
            &[alert(f()), alert(NOW - secs(5))],
            &[session(Some(100.0), Some(NOW - secs(5)), NOW - mins(1))],
            NOW,
        );
        assert_eq!(
            actions,
            vec![fire(NOW - secs(5), FireReason::Due)],
            "one fire, not two"
        );
    }

    #[test]
    fn duplicate_snapshots_use_the_latest_observation() {
        let fire_at = NOW - secs(5);
        let good = session(Some(100.0), Some(fire_at), NOW - mins(1));
        let glitch = session(Some(100.0), Some(NOW + mins(500)), NOW - mins(3));
        assert_eq!(
            run(&[alert(fire_at)], &[good, glitch], NOW),
            vec![fire(fire_at, FireReason::Due)]
        );
    }

    #[test]
    fn windows_are_independent() {
        let weekly_id = "reset:claude:default:weekly";
        let weekly_fire = NOW - secs(5);
        let alerts = [alert(f()), AlertIntent::new(weekly_id, weekly_fire)];
        let reads = [
            exhausted(f()),
            read(
                WindowKind::Weekly,
                Some(100.0),
                Some(weekly_fire),
                NOW - mins(1),
            ),
        ];
        assert_eq!(
            run(&alerts, &reads, NOW),
            vec![AlertAction::Fire {
                id: weekly_id.into(),
                fire_at: weekly_fire,
                reason: FireReason::Due
            }]
        );
    }

    #[test]
    fn different_accounts_do_not_share_alerts() {
        let mut other = session(Some(10.0), Some(NOW + mins(500)), NOW - mins(1));
        other.account = "work".into();
        other.id = "claude:work:session_5h".into();
        let fire_at = NOW - secs(5);
        // the "default" alert is due, and only the "work" window is present: window vanished
        assert_eq!(
            run(&[alert(fire_at)], &[other], NOW),
            vec![fire(fire_at, FireReason::WindowGone)]
        );
    }

    // ---- scheduling ----

    #[test]
    fn the_default_config_never_arms_an_alert_on_its_own() {
        // Alerts come from the user's "Notify me when it resets" button; auto-arming is opt-in.
        assert_eq!(ReconcileConfig::default().auto_arm_at_percent, None);
        let reset = NOW + mins(90);
        assert_eq!(reconcile(&[], &[exhausted(reset)], NOW), vec![]);
    }

    #[test]
    fn exhausted_windows_without_an_alert_get_one() {
        let reset = NOW + mins(90);
        let actions = run(&[], &[exhausted(reset)], NOW);
        assert_eq!(
            actions,
            vec![AlertAction::Schedule {
                id: ID.into(),
                fire_at: reset
            }]
        );
    }

    #[test]
    fn scheduling_table() {
        let reset = NOW + mins(90);
        let fresh = NOW - mins(1);
        type Case = (&'static str, Snapshot, bool);
        let cases: Vec<Case> = vec![
            ("exhausted", session(Some(100.0), Some(reset), fresh), true),
            (
                "at threshold",
                session(Some(99.0), Some(reset), fresh),
                true,
            ),
            (
                "below threshold",
                session(Some(98.9), Some(reset), fresh),
                false,
            ),
            ("unknown percent", session(None, Some(reset), fresh), false),
            ("no reset time", session(Some(100.0), None, fresh), false),
            (
                "reset in the past",
                session(Some(100.0), Some(NOW - mins(1)), fresh),
                false,
            ),
            (
                "balance window",
                read(WindowKind::Balance, Some(100.0), Some(reset), fresh),
                false,
            ),
            (
                "stale",
                with_status(session(Some(100.0), Some(reset), fresh), Status::Stale),
                false,
            ),
            (
                "degraded",
                with_status(session(Some(100.0), Some(reset), fresh), Status::Degraded),
                false,
            ),
            (
                "unavailable",
                with_status(
                    session(Some(100.0), Some(reset), fresh),
                    Status::Unavailable,
                ),
                false,
            ),
            (
                "read too old",
                session(Some(100.0), Some(reset), NOW - mins(16)),
                false,
            ),
            (
                "weekly",
                read(WindowKind::Weekly, Some(100.0), Some(reset), fresh),
                true,
            ),
        ];
        for (name, snapshot, expect) in cases {
            let actions = run(&[], &[snapshot], NOW);
            assert_eq!(!actions.is_empty(), expect, "{name}: {actions:?}");
        }
    }

    #[test]
    fn auto_arming_can_be_turned_off() {
        let cfg = ReconcileConfig {
            auto_arm_at_percent: None,
            ..ReconcileConfig::default()
        };
        assert!(reconcile_with(&cfg, &[], &[exhausted(NOW + mins(90))], NOW).is_empty());
    }

    #[test]
    fn an_existing_alert_prevents_a_second_schedule() {
        let reset = NOW + mins(90);
        let actions = run(&[alert(reset)], &[exhausted(reset)], NOW);
        assert!(actions.is_empty());
    }

    #[test]
    fn duplicate_snapshots_schedule_once() {
        let reset = NOW + mins(90);
        let a = session(Some(100.0), Some(reset), NOW - mins(2));
        let b = session(Some(100.0), Some(reset), NOW - mins(1));
        assert_eq!(run(&[], &[a, b], NOW).len(), 1);
    }

    #[test]
    fn schedule_uses_the_latest_read() {
        let a = session(Some(100.0), Some(NOW + mins(80)), NOW - mins(2));
        let b = session(Some(100.0), Some(NOW + mins(90)), NOW - mins(1));
        assert_eq!(
            run(&[], &[b, a], NOW),
            vec![AlertAction::Schedule {
                id: ID.into(),
                fire_at: NOW + mins(90)
            }]
        );
    }

    #[test]
    fn actions_follow_the_order_of_the_input() {
        let a = AlertIntent::new("reset:claude:default:weekly", NOW - secs(1));
        let b = alert(NOW - secs(1));
        let actions = run(&[a, b], &[], NOW);
        assert_eq!(
            actions.iter().map(AlertAction::id).collect::<Vec<_>>(),
            vec!["reset:claude:default:weekly", ID]
        );
    }

    // ---- should_fire ----

    fn gate(a: &AlertIntent, s: Option<&Snapshot>, now: OffsetDateTime) -> bool {
        should_fire(a, s, now)
    }

    #[test]
    fn should_fire_basics() {
        let a = alert(NOW);
        let agree = session(Some(100.0), Some(NOW), NOW - mins(1));
        assert!(gate(&a, Some(&agree), NOW));
        assert!(
            gate(&a, None, NOW),
            "no snapshot: the window vanished, time says fire"
        );
        assert!(gate(&a, Some(&agree), NOW + secs(1)));
        // disabled never
        assert!(!gate(&disabled(NOW), Some(&agree), NOW));
        assert!(!gate(&disabled(NOW), None, NOW));
    }

    #[test]
    fn should_fire_respects_the_lead_and_the_overdue_limit() {
        let a = alert(NOW);
        assert!(!gate(&a, None, NOW - secs(31)));
        assert!(gate(&a, None, NOW - secs(30)));
        assert!(gate(&a, None, NOW + mins(120)));
        assert!(!gate(&a, None, NOW + mins(120) + secs(1)));
    }

    #[test]
    fn should_fire_is_vetoed_by_a_fresh_read_that_says_the_reset_moved_away() {
        let a = alert(NOW);
        let moved = session(Some(100.0), Some(NOW + mins(45)), NOW - secs(40));
        assert!(!gate(&a, Some(&moved), NOW));
        let lost = session(Some(100.0), None, NOW - mins(1));
        // due, window lost its reset time: the reset happened
        assert!(gate(&a, Some(&lost), NOW));
        // within the lead window the same read is also "reset happened"
        assert!(gate(&a, Some(&lost), NOW - secs(10)));
    }

    #[test]
    fn should_fire_ignores_reads_that_are_not_evidence() {
        let a = alert(NOW);
        let moved = session(Some(100.0), Some(NOW + mins(45)), NOW - secs(40));
        assert!(gate(
            &a,
            Some(&with_status(moved.clone(), Status::Stale)),
            NOW
        ));
        assert!(gate(
            &a,
            Some(&with_status(moved.clone(), Status::Degraded)),
            NOW
        ));
        let old = session(Some(100.0), Some(NOW + mins(45)), NOW - mins(16));
        assert!(gate(&a, Some(&old), NOW));
        // a read of another window is not about this alert
        let mut other = moved;
        other.window = WindowKind::Weekly;
        other.id = "claude:default:weekly".into();
        assert!(gate(&a, Some(&other), NOW));
    }

    #[test]
    fn should_fire_accepts_reads_that_show_the_reset_happened() {
        let a = alert(NOW);
        let rolled = session(Some(1.0), Some(NOW + mins(300)), NOW - secs(5));
        assert!(gate(&a, Some(&rolled), NOW));
        let earlier = session(Some(100.0), Some(NOW - mins(10)), NOW - secs(5));
        assert!(gate(&a, Some(&earlier), NOW));
        // usage resumed before the due time vetoes
        let resumed = session(Some(1.0), Some(NOW + mins(300)), NOW - mins(2));
        assert!(!gate(&a, Some(&resumed), NOW));
    }

    #[test]
    fn should_fire_with_skewed_future_reads_is_still_sane() {
        let a = alert(NOW);
        let agree = session(Some(100.0), Some(NOW), NOW + mins(30));
        assert!(gate(&a, Some(&agree), NOW));
    }

    // ---- Reconciler (memory across polls) ----

    fn step(
        r: &mut Reconciler,
        alerts: &mut Vec<AlertIntent>,
        reads: &[Snapshot],
        now: OffsetDateTime,
    ) -> Vec<AlertAction> {
        let actions = r.reconcile(alerts, reads, now);
        apply_actions(alerts, &actions);
        actions
    }

    #[test]
    fn the_reconciler_confirms_across_polls() {
        let mut r = Reconciler::default();
        let mut alerts = vec![alert(f())];
        let later = f() + mins(30);
        let t1 = NOW;
        let t2 = NOW + mins(1);
        assert!(step(
            &mut r,
            &mut alerts,
            &[session(Some(100.0), Some(later), t1)],
            t1
        )
        .is_empty());
        let actions = step(
            &mut r,
            &mut alerts,
            &[session(Some(100.0), Some(later), t2)],
            t2,
        );
        assert_eq!(actions, vec![reschedule(f(), later)]);
        assert_eq!(alerts, vec![alert(later)]);
        // and it settles: the same reads again do nothing
        let t3 = NOW + mins(2);
        assert!(step(
            &mut r,
            &mut alerts,
            &[session(Some(100.0), Some(later), t3)],
            t3
        )
        .is_empty());
    }

    #[test]
    fn redelivering_the_same_read_never_confirms() {
        let mut r = Reconciler::default();
        let alerts = vec![alert(f())];
        let later = f() + mins(30);
        let read = session(Some(100.0), Some(later), NOW);
        for i in 0..5 {
            let actions = r.reconcile(&alerts, std::slice::from_ref(&read), NOW + secs(i * 10));
            assert!(actions.is_empty(), "redelivery {i}");
        }
    }

    #[test]
    fn a_glitch_that_is_not_repeated_changes_nothing() {
        let mut r = Reconciler::default();
        let mut alerts = vec![alert(f())];
        let reads = [
            session(Some(100.0), Some(f()), NOW),
            session(Some(100.0), Some(f() + mins(300)), NOW + mins(1)), // glitch
            session(Some(100.0), Some(f()), NOW + mins(2)),
            session(Some(100.0), Some(f() + mins(300)), NOW + mins(3)), // glitch again
            session(Some(100.0), Some(f()), NOW + mins(4)),
        ];
        for (i, read) in reads.iter().enumerate() {
            let now = read.observed_at;
            assert!(
                step(&mut r, &mut alerts, std::slice::from_ref(read), now).is_empty(),
                "read {i}"
            );
        }
        assert_eq!(alerts, vec![alert(f())]);
    }

    #[test]
    fn the_reconciler_fires_once() {
        let mut r = Reconciler::default();
        let fire_at = NOW;
        let alerts = vec![alert(fire_at)];
        let agree = session(Some(100.0), Some(fire_at), NOW - mins(1));
        let first = r.reconcile(&alerts, std::slice::from_ref(&agree), NOW);
        assert_eq!(first, vec![fire(fire_at, FireReason::Due)]);
        // the caller forgot to remove the alert: still no second fire
        assert!(r
            .reconcile(&alerts, std::slice::from_ref(&agree), NOW + secs(30))
            .is_empty());
        assert!(r
            .reconcile(&alerts, std::slice::from_ref(&agree), NOW + mins(5))
            .is_empty());
        // a new alert for the next window fires normally
        let next = alert(NOW + mins(300));
        let t = NOW + mins(300);
        let agree = session(Some(100.0), Some(t), t - mins(1));
        assert_eq!(
            r.reconcile(&[next], &[agree], t),
            vec![fire(t, FireReason::Due)]
        );
    }

    #[test]
    fn applying_a_fire_consumes_the_alert() {
        let fire_at = NOW;
        let mut alerts = vec![alert(fire_at)];
        let agree = session(Some(100.0), Some(fire_at), NOW - mins(1));
        let first = run(&alerts, std::slice::from_ref(&agree), NOW);
        assert_eq!(first.len(), 1);
        apply_actions(&mut alerts, &first);
        assert!(alerts.is_empty());
        assert!(run(&alerts, &[agree], NOW + secs(1)).is_empty());
    }

    #[test]
    fn a_fresh_reconciler_forgets_and_needs_two_reads_again() {
        let alerts = vec![alert(f())];
        let later = f() + mins(30);
        let mut a = Reconciler::default();
        assert!(a
            .reconcile(&alerts, &[session(Some(100.0), Some(later), NOW)], NOW)
            .is_empty());
        let mut b = Reconciler::default();
        assert!(b
            .reconcile(
                &alerts,
                &[session(Some(100.0), Some(later), NOW + mins(1))],
                NOW + mins(1)
            )
            .is_empty());
    }

    #[test]
    fn confirm_reads_is_configurable() {
        let cfg = ReconcileConfig {
            confirm_reads: 1,
            ..ReconcileConfig::default()
        };
        let later = f() + mins(30);
        let actions = reconcile_with(
            &cfg,
            &[alert(f())],
            &[session(Some(100.0), Some(later), NOW - mins(1))],
            NOW,
        );
        assert_eq!(actions, vec![reschedule(f(), later)]);

        let three = ReconcileConfig {
            confirm_reads: 3,
            ..ReconcileConfig::default()
        };
        let reads = [
            session(Some(100.0), Some(later), NOW - mins(3)),
            session(Some(100.0), Some(later), NOW - mins(2)),
        ];
        assert!(reconcile_with(&three, &[alert(f())], &reads, NOW).is_empty());
        let mut r = Reconciler::new(three);
        let mut got = Vec::new();
        for (i, m) in [3, 2, 1].into_iter().enumerate() {
            let read = session(Some(100.0), Some(later), NOW - mins(m));
            got.push(r.reconcile(&[alert(f())], &[read], NOW - mins(m)));
            assert_eq!(got[i].is_empty(), i < 2);
        }
    }

    #[test]
    fn a_full_day_in_the_life() {
        // 1. The session window is exhausted: with auto-arming opted in, an alert is armed automatically.
        let mut r = Reconciler::new(auto_cfg());
        let mut alerts: Vec<AlertIntent> = Vec::new();
        let reset = NOW + mins(120);
        let a1 = step(
            &mut r,
            &mut alerts,
            &[session(Some(100.0), Some(reset), NOW)],
            NOW,
        );
        assert_eq!(
            a1,
            vec![AlertAction::Schedule {
                id: ID.into(),
                fire_at: reset
            }]
        );
        assert_eq!(alerts, vec![alert(reset)]);

        // 2. The provider shifts the reset by 10 minutes; two reads confirm.
        let moved = reset + mins(10);
        let t = NOW + mins(10);
        assert!(step(
            &mut r,
            &mut alerts,
            &[session(Some(100.0), Some(moved), t)],
            t
        )
        .is_empty());
        let t = NOW + mins(11);
        let a2 = step(
            &mut r,
            &mut alerts,
            &[session(Some(100.0), Some(moved), t)],
            t,
        );
        assert_eq!(a2, vec![reschedule(reset, moved)]);

        // 3. Quiet until the time comes.
        let t = moved - mins(1);
        assert!(step(
            &mut r,
            &mut alerts,
            &[session(Some(100.0), Some(moved), t)],
            t
        )
        .is_empty());

        // 4. At the reset time, the alert fires and is removed.
        let a3 = step(
            &mut r,
            &mut alerts,
            &[session(Some(100.0), Some(moved), moved - secs(20))],
            moved,
        );
        assert_eq!(a3, vec![fire(moved, FireReason::Due)]);
        assert!(alerts.is_empty());

        // 5. After the reset the window is fresh: nothing is re-armed.
        let t = moved + mins(1);
        let fresh = session(Some(0.0), None, t);
        assert!(step(&mut r, &mut alerts, &[fresh], t).is_empty());
        assert!(alerts.is_empty());
    }

    // ---- apply_actions ----

    #[test]
    fn apply_actions_bookkeeping() {
        let weekly = "reset:claude:default:weekly";
        let mut alerts = vec![alert(f()), AlertIntent::new(weekly, NOW + mins(500))];
        apply_actions(&mut alerts, &[cancel(CancelReason::Disabled)]);
        assert_eq!(alerts.len(), 2, "a disabled alert keeps its row");

        apply_actions(&mut alerts, &[reschedule(f(), f() + mins(5))]);
        assert_eq!(alerts[0], alert(f() + mins(5)));
        assert_eq!(
            alerts[1].fire_at,
            NOW + mins(500),
            "other windows are untouched"
        );

        apply_actions(&mut alerts, &[fire(f() + mins(5), FireReason::Due)]);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].id, weekly);
    }

    #[test]
    fn apply_actions_schedule_replaces_and_cancel_removes() {
        let mut alerts = vec![alert(f())];
        apply_actions(
            &mut alerts,
            &[AlertAction::Schedule {
                id: ID.into(),
                fire_at: f() + mins(1),
            }],
        );
        assert_eq!(alerts, vec![alert(f() + mins(1))]);
        apply_actions(&mut alerts, &[cancel(CancelReason::UsageResumed)]);
        assert!(alerts.is_empty());
        apply_actions(&mut alerts, &[cancel(CancelReason::Expired)]);
        assert!(alerts.is_empty());
    }

    #[test]
    fn reset_titles_name_the_provider_and_the_window() {
        assert_eq!(
            reset_title("claude", WindowKind::Session5h),
            "Claude 5-hour limit is back"
        );
        assert_eq!(
            reset_title("xai", WindowKind::Balance),
            "xAI balance is back"
        );
        assert_eq!(
            reset_title("somebody", WindowKind::Weekly),
            "somebody weekly limit is back"
        );
    }
}
