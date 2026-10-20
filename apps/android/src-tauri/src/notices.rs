//! "Running low" and "ran out" on the phone.
//!
//! The rules are `cooldown_core::usage`, the same as the PC's. They are shown through the alarm
//! pipeline that already exists: a notice becomes an alarm due a moment from now, so the Kotlin
//! side needs nothing new and the background worker and the open app both deliver them. A notice
//! stays in the alarm list until it is due, because every alarm sync cancels what it does not list.
//!
//! Kept in `notices.json` in the app's private data folder: the user's choices, what was already
//! shown this cycle (so a restart or the next sync does not ring again) and the notices still due.
//! A missing or damaged file means defaults.

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use cooldown_core::usage::{prune, usage_alerts, UsageConfig, UsageKind, UsageNotified};
use cooldown_core::Snapshot;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::{Duration, OffsetDateTime};

/// How far ahead of now a notice is scheduled: far enough that the alarm sync does not count it as
/// already past.
const DELIVER_IN_MS: i64 = 2_000;

/// The phone's own choices. "Limit is back" bells come from the PC, so they are set there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PhoneNotifications {
    /// Ring when a limit reaches 100%.
    pub ran_out: bool,
    /// Ring once per cycle when a limit passes this percentage. `None` is off.
    pub low_at_percent: Option<u8>,
}

impl Default for PhoneNotifications {
    fn default() -> Self {
        PhoneNotifications {
            ran_out: true,
            low_at_percent: Some(90),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingNotice {
    id: String,
    at_ms: i64,
    title: String,
    body: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct State {
    settings: PhoneNotifications,
    shown: Vec<UsageNotified>,
    pending: Vec<PendingNotice>,
}

/// The notices file and its in-memory copy.
pub struct Notices {
    path: PathBuf,
    state: Mutex<State>,
}

impl Notices {
    /// Load `path`, falling back to defaults.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let state = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Notices {
            path,
            state: Mutex::new(state),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn save(&self, state: &State) {
        let Ok(bytes) = serde_json::to_vec(state) else {
            return;
        };
        let tmp = self.path.with_extension("json.tmp");
        if fs::write(&tmp, bytes)
            .and_then(|()| fs::rename(&tmp, &self.path))
            .is_err()
        {
            eprintln!("cooldown: notices not saved");
        }
    }

    /// The user's choices.
    pub fn settings(&self) -> PhoneNotifications {
        self.lock().settings
    }

    /// Change the choices.
    pub fn set_settings(&self, next: PhoneNotifications) -> Result<(), String> {
        if next.low_at_percent.is_some_and(|p| !(1..=99).contains(&p)) {
            return Err("The warning level must be between 1% and 99%.".into());
        }
        let mut state = self.lock();
        state.settings = next;
        self.save(&state);
        Ok(())
    }

    /// The notices to hand to the alarm sync: new ones for `snapshots`, and earlier ones that are
    /// not due yet. Same shape as `alarm_list`.
    pub fn alarms(&self, snapshots: &[Snapshot], now_ms: i64) -> Vec<Value> {
        let now = OffsetDateTime::from_unix_timestamp_nanos(i128::from(now_ms) * 1_000_000)
            .unwrap_or_else(|_| OffsetDateTime::now_utc());
        let mut state = self.lock();
        let config = UsageConfig {
            low_at_percent: state.settings.low_at_percent.map(f64::from),
            ran_out: state.settings.ran_out,
            // The phone syncs about every 15 minutes, so a read can be older here than on the PC.
            max_read_age: Duration::minutes(35),
        };
        let due = usage_alerts(&config, snapshots, &state.shown, now);
        let changed = !due.is_empty() || state.pending.iter().any(|p| p.at_ms <= now_ms);
        for alert in &due {
            let kind = match alert.kind {
                UsageKind::Low => "low",
                UsageKind::RanOut => "out",
            };
            let id = format!("usage:{kind}:{}", alert.id);
            state.pending.retain(|p| p.id != id);
            state.pending.push(PendingNotice {
                id,
                at_ms: now_ms + DELIVER_IN_MS,
                title: alert.title(),
                body: alert.body(now),
            });
            state.shown.push(alert.notified());
        }
        state.pending.retain(|p| p.at_ms > now_ms);
        if changed {
            prune(&mut state.shown, now);
            self.save(&state);
        }
        state
            .pending
            .iter()
            .map(|p| json!({ "id": p.id, "atMs": p.at_ms, "title": p.title, "body": p.body }))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cooldown_core::{SnapshotSource, SourceKind, Unit, WindowKind};

    const NOW_MS: i64 = 1_790_000_000_000;

    fn read(percent: f64) -> Snapshot {
        let now =
            OffsetDateTime::from_unix_timestamp_nanos(i128::from(NOW_MS) * 1_000_000).unwrap();
        let mut s = Snapshot::new(
            "codex",
            "default",
            WindowKind::Weekly,
            Unit::Percent,
            SnapshotSource {
                kind: SourceKind::Scraped,
                adapter: "codex-app-server".into(),
                adapter_version: "0.1.0".into(),
            },
            now - Duration::minutes(20),
        );
        s.percent_used = Some(percent);
        s.resets_at = Some(now + Duration::days(2));
        s
    }

    #[test]
    fn a_notice_is_listed_until_due_and_shown_once() {
        let dir = tempfile::tempdir().unwrap();
        let notices = Notices::open(dir.path().join("notices.json"));
        let first = notices.alarms(&[read(100.0)], NOW_MS);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0]["title"], "Codex weekly limit ran out");
        assert_eq!(first[0]["atMs"], NOW_MS + DELIVER_IN_MS);
        // A second sync a second later still lists it, so the alarm sync does not cancel it.
        assert_eq!(notices.alarms(&[read(100.0)], NOW_MS + 1_000).len(), 1);
        // Once due it is gone, and it does not come back (also after a restart).
        assert!(notices.alarms(&[read(100.0)], NOW_MS + 5_000).is_empty());
        let reopened = Notices::open(dir.path().join("notices.json"));
        assert!(reopened.alarms(&[read(100.0)], NOW_MS + 6_000).is_empty());
    }

    #[test]
    fn settings_are_kept_and_checked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notices.json");
        let notices = Notices::open(&path);
        assert_eq!(notices.settings(), PhoneNotifications::default());
        let off = PhoneNotifications {
            ran_out: false,
            low_at_percent: None,
        };
        notices.set_settings(off).unwrap();
        assert!(notices.alarms(&[read(100.0)], NOW_MS).is_empty());
        assert_eq!(Notices::open(&path).settings(), off);
        assert!(notices
            .set_settings(PhoneNotifications {
                low_at_percent: Some(100),
                ..off
            })
            .is_err());
    }
}
