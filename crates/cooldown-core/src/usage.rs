//! Usage notifications: "running low" and "ran out".
//!
//! Pure, like [`crate::alerts`]: give it the latest snapshots, what has already been notified and
//! `now`, and it says which notifications to show. The caller shows them and remembers them.
//!
//! # Rules
//!
//! - Only limit windows count (never a balance), and only a fresh `ok` read with a percentage and
//!   a future reset time. A stale or failing read is not evidence that anything changed.
//! - **Ran out:** `percentUsed >= 100`.
//! - **Running low:** `percentUsed >= low_at_percent` and below 100. A window that jumps straight
//!   past 100 gets only "ran out".
//! - **Once per cycle.** A cycle is one reset time. Reset times reported by a provider can wobble
//!   by a few seconds between reads, so a notification counts as already shown when one of the same
//!   kind, for the same window, was shown for a reset within [`SAME_CYCLE`].

use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::alerts::limit_label;
use crate::model::{Snapshot, Status, WindowKind};

/// Two reset times closer than this are the same cycle.
pub const SAME_CYCLE: Duration = Duration::minutes(30);

/// Which usage notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsageKind {
    /// Past the warning threshold.
    Low,
    /// At 100%.
    RanOut,
}

/// Tuning, from the user's settings.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageConfig {
    /// Warn at this percentage. `None` turns "running low" off.
    pub low_at_percent: Option<f64>,
    /// Whether to notify at 100%.
    pub ran_out: bool,
    /// Reads older than this are not evidence. Default 15 minutes, like the reset alerts.
    pub max_read_age: Duration,
}

impl Default for UsageConfig {
    fn default() -> Self {
        UsageConfig {
            low_at_percent: Some(90.0),
            ran_out: true,
            max_read_age: Duration::minutes(15),
        }
    }
}

/// A usage notification that was shown, kept so it is not shown twice in one cycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageNotified {
    /// Which notification.
    pub kind: UsageKind,
    /// The snapshot id of the window.
    pub id: String,
    /// The reset time of the cycle it was shown in.
    #[serde(with = "crate::model::rfc3339_utc")]
    pub resets_at: OffsetDateTime,
}

/// One notification to show now.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageAlert {
    /// Which notification.
    pub kind: UsageKind,
    /// Provider id, e.g. `claude`.
    pub provider: String,
    /// The window.
    pub window: WindowKind,
    /// The snapshot id of the window.
    pub id: String,
    /// Percent used in the read that triggered it.
    pub percent: f64,
    /// When the window resets.
    pub resets_at: OffsetDateTime,
}

impl UsageAlert {
    /// What to remember once it was shown.
    pub fn notified(&self) -> UsageNotified {
        UsageNotified {
            kind: self.kind,
            id: self.id.clone(),
            resets_at: self.resets_at,
        }
    }

    /// Notification title: "Claude 5-hour limit ran out" / "Claude 5-hour limit is at 90%".
    pub fn title(&self) -> String {
        let label = limit_label(&self.provider, self.window);
        match self.kind {
            UsageKind::RanOut => format!("{label} ran out"),
            UsageKind::Low => format!("{label} is at {}%", self.percent.floor() as i64),
        }
    }

    /// Notification body, with the time left until the reset.
    pub fn body(&self, now: OffsetDateTime) -> String {
        let left = format_left(self.resets_at - now);
        match self.kind {
            UsageKind::RanOut => format!("It comes back in {left}."),
            UsageKind::Low => format!("It resets in {left}."),
        }
    }
}

/// "2 h 10 min", "45 min", "3 d 4 h". Under a minute reads "under a minute".
pub fn format_left(left: Duration) -> String {
    let minutes = left.whole_minutes().max(0);
    if minutes < 1 {
        return "under a minute".to_owned();
    }
    let (days, hours, mins) = (minutes / 1440, (minutes % 1440) / 60, minutes % 60);
    match (days, hours, mins) {
        (0, 0, m) => format!("{m} min"),
        (0, h, 0) => format!("{h} h"),
        (0, h, m) => format!("{h} h {m} min"),
        (d, 0, _) => format!("{d} d"),
        (d, h, _) => format!("{d} d {h} h"),
    }
}

/// The notifications to show now. `shown` is what was already shown (any age; old entries are
/// harmless, see [`prune`]).
pub fn usage_alerts(
    config: &UsageConfig,
    snapshots: &[Snapshot],
    shown: &[UsageNotified],
    now: OffsetDateTime,
) -> Vec<UsageAlert> {
    let mut out = Vec::new();
    for snapshot in snapshots {
        let Some(kind) = kind_for(config, snapshot, now) else {
            continue;
        };
        let (Some(percent), Some(resets_at)) = (snapshot.percent_used, snapshot.resets_at) else {
            continue;
        };
        let seen = shown.iter().any(|s| {
            s.kind == kind && s.id == snapshot.id && (s.resets_at - resets_at).abs() < SAME_CYCLE
        });
        if !seen {
            out.push(UsageAlert {
                kind,
                provider: snapshot.provider.clone(),
                window: snapshot.window,
                id: snapshot.id.clone(),
                percent,
                resets_at,
            });
        }
    }
    out
}

fn kind_for(config: &UsageConfig, s: &Snapshot, now: OffsetDateTime) -> Option<UsageKind> {
    if s.window == WindowKind::Balance
        || s.status != Status::Ok
        || (now - s.observed_at) > config.max_read_age
        || s.resets_at.is_none_or(|r| r <= now)
    {
        return None;
    }
    let percent = s.percent_used?;
    if percent >= 100.0 {
        return config.ran_out.then_some(UsageKind::RanOut);
    }
    let low = config.low_at_percent?;
    (percent >= low).then_some(UsageKind::Low)
}

/// Drop remembered notifications whose cycle ended more than a day ago.
pub fn prune(shown: &mut Vec<UsageNotified>, now: OffsetDateTime) {
    shown.retain(|s| s.resets_at > now - Duration::days(1));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SnapshotSource, SourceKind, Unit};
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-10-19 12:00 UTC);

    fn read(percent: f64, reset_in: Duration) -> Snapshot {
        let mut s = Snapshot::new(
            "claude",
            "default",
            WindowKind::Session5h,
            Unit::Percent,
            SnapshotSource {
                kind: SourceKind::Official,
                adapter: "claude-code-statusline".into(),
                adapter_version: "0.1.0".into(),
            },
            NOW - Duration::minutes(1),
        );
        s.percent_used = Some(percent);
        s.resets_at = Some(NOW + reset_in);
        s
    }

    fn run(config: &UsageConfig, reads: &[Snapshot], shown: &[UsageNotified]) -> Vec<UsageKind> {
        usage_alerts(config, reads, shown, NOW)
            .into_iter()
            .map(|a| a.kind)
            .collect()
    }

    #[test]
    fn below_the_threshold_says_nothing() {
        assert!(run(
            &UsageConfig::default(),
            &[read(89.9, Duration::hours(2))],
            &[]
        )
        .is_empty());
    }

    #[test]
    fn low_and_ran_out_fire_at_their_thresholds() {
        let cfg = UsageConfig::default();
        assert_eq!(
            run(&cfg, &[read(90.0, Duration::hours(2))], &[]),
            [UsageKind::Low]
        );
        assert_eq!(
            run(&cfg, &[read(100.0, Duration::hours(2))], &[]),
            [UsageKind::RanOut]
        );
    }

    #[test]
    fn each_fires_once_per_cycle_even_if_the_reset_time_wobbles() {
        let cfg = UsageConfig::default();
        let first = usage_alerts(&cfg, &[read(95.0, Duration::hours(2))], &[], NOW);
        let shown: Vec<_> = first.iter().map(UsageAlert::notified).collect();
        let wobbled = read(97.0, Duration::hours(2) + Duration::seconds(40));
        assert!(run(&cfg, &[wobbled], &shown).is_empty());
        // Running out later in the same cycle is a new kind of news.
        assert_eq!(
            run(&cfg, &[read(100.0, Duration::hours(2))], &shown),
            [UsageKind::RanOut]
        );
        // The next cycle starts over.
        assert_eq!(
            run(&cfg, &[read(92.0, Duration::hours(7))], &shown),
            [UsageKind::Low]
        );
    }

    #[test]
    fn untrustworthy_reads_and_balances_are_ignored() {
        let cfg = UsageConfig::default();
        let mut stale = read(100.0, Duration::hours(2));
        stale.status = Status::Stale;
        let mut old = read(100.0, Duration::hours(2));
        old.observed_at = NOW - Duration::hours(1);
        let passed = read(100.0, -Duration::minutes(1));
        let mut balance = read(100.0, Duration::hours(2));
        balance.window = WindowKind::Balance;
        let mut no_reset = read(100.0, Duration::hours(2));
        no_reset.resets_at = None;
        assert!(run(&cfg, &[stale, old, passed, balance, no_reset], &[]).is_empty());
    }

    #[test]
    fn each_can_be_switched_off() {
        let off = UsageConfig {
            low_at_percent: None,
            ran_out: false,
            ..UsageConfig::default()
        };
        assert!(run(&off, &[read(95.0, Duration::hours(2))], &[]).is_empty());
        assert!(run(&off, &[read(100.0, Duration::hours(2))], &[]).is_empty());
    }

    #[test]
    fn wording() {
        let alert = &usage_alerts(
            &UsageConfig::default(),
            &[read(93.6, Duration::minutes(130))],
            &[],
            NOW,
        )[0];
        assert_eq!(alert.title(), "Claude 5-hour limit is at 93%");
        assert_eq!(alert.body(NOW), "It resets in 2 h 10 min.");
        assert_eq!(format_left(Duration::seconds(20)), "under a minute");
        assert_eq!(format_left(Duration::hours(76)), "3 d 4 h");
        assert_eq!(format_left(Duration::hours(3)), "3 h");
    }

    #[test]
    fn prune_keeps_recent_cycles() {
        let mut shown = vec![
            UsageNotified {
                kind: UsageKind::Low,
                id: "a".into(),
                resets_at: NOW - Duration::days(2),
            },
            UsageNotified {
                kind: UsageKind::Low,
                id: "b".into(),
                resets_at: NOW - Duration::hours(2),
            },
        ];
        prune(&mut shown, NOW);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].id, "b");
    }
}
