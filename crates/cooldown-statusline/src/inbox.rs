//! The inbox file Cooldown's Claude Code adapter reads.
//!
//! One file, `claude-statusline.json`, replaced atomically on every run that
//! carries rate-limit data. The schema is versioned; see the crate README for
//! the field-by-field description.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::atomic::{cleanup_stale_temps, write_atomic};
use crate::parse::{Observation, SpendLimit, Window};

/// Schema version written to `version`.
pub const INBOX_VERSION: u32 = 1;
/// File name inside the inbox directory.
pub const INBOX_FILE_NAME: &str = "claude-statusline.json";

/// A window as stored in the inbox. Either field is `null` when Claude Code did
/// not report it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxWindow {
    /// 0..=100.
    pub used_percentage: Option<f64>,
    /// RFC 3339 UTC.
    pub resets_at: Option<String>,
}

/// The gateway-only spend limit as stored in the inbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxSpendLimit {
    /// 0 or more; can exceed 100 once the limit is passed.
    pub used_percentage: Option<f64>,
    pub resets_at: Option<String>,
    pub used_usd: Option<f64>,
    pub limit_usd: Option<f64>,
    /// `daily`, `weekly` or `monthly`.
    pub period: Option<String>,
}

/// The whole inbox document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inbox {
    pub version: u32,
    /// RFC 3339 UTC, whole seconds: when the hook saw this data.
    pub observed_at: String,
    pub five_hour: Option<InboxWindow>,
    pub seven_day: Option<InboxWindow>,
    pub spend_limit: Option<InboxSpendLimit>,
    pub claude_code_version: Option<String>,
}

/// Unix epoch seconds to RFC 3339 UTC (`2025-02-01T16:00:00Z`).
///
/// Returns `None` outside the representable range instead of failing.
pub fn unix_to_rfc3339(seconds: i64) -> Option<String> {
    OffsetDateTime::from_unix_timestamp(seconds)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

/// `now` as RFC 3339 UTC with the sub-second part dropped.
pub fn format_now(now: OffsetDateTime) -> String {
    let whole = now.replace_nanosecond(0).unwrap_or(now);
    whole
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

fn window(w: &Window) -> InboxWindow {
    InboxWindow {
        used_percentage: w.used_percentage.map(|p| p.clamp(0.0, 100.0)),
        resets_at: w.resets_at.and_then(unix_to_rfc3339),
    }
}

fn spend_limit(s: &SpendLimit) -> InboxSpendLimit {
    InboxSpendLimit {
        used_percentage: s.used_percentage.map(|p| p.max(0.0)),
        resets_at: s.resets_at.and_then(unix_to_rfc3339),
        used_usd: s.used_usd.map(|v| v.max(0.0)),
        limit_usd: s.limit_usd.map(|v| v.max(0.0)),
        period: s.period.clone(),
    }
}

impl Inbox {
    /// Build the inbox document for an observation made at `now`.
    pub fn from_observation(obs: &Observation, now: OffsetDateTime) -> Self {
        Self {
            version: INBOX_VERSION,
            observed_at: format_now(now),
            five_hour: obs.five_hour.as_ref().map(window),
            seven_day: obs.seven_day.as_ref().map(window),
            spend_limit: obs.spend_limit.as_ref().map(spend_limit),
            claude_code_version: obs.claude_code_version.clone(),
        }
    }

    /// Pretty JSON with a trailing newline.
    pub fn to_json_bytes(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec_pretty(self).unwrap_or_else(|_| b"{}".to_vec());
        bytes.push(b'\n');
        bytes
    }
}

/// Write `inbox` into `dir` atomically, creating `dir` if needed, and sweep any
/// temp files orphaned by cancelled runs. Returns the path written.
pub fn write_inbox(dir: &Path, inbox: &Inbox) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let target = dir.join(INBOX_FILE_NAME);
    write_atomic(&target, &inbox.to_json_bytes())?;
    cleanup_stale_temps(dir, INBOX_FILE_NAME, Duration::from_secs(60));
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_stdin;
    use serde_json::{json, Value};
    use time::macros::datetime;

    fn now() -> OffsetDateTime {
        datetime!(2026-10-10 12:34:56.789 UTC)
    }

    #[test]
    fn unix_seconds_convert_to_rfc3339_utc() {
        assert_eq!(
            unix_to_rfc3339(1_738_425_600).as_deref(),
            Some("2025-02-01T16:00:00Z")
        );
        assert_eq!(unix_to_rfc3339(0).as_deref(), Some("1970-01-01T00:00:00Z"));
        assert_eq!(
            unix_to_rfc3339(253_402_300_799).as_deref(),
            Some("9999-12-31T23:59:59Z")
        );
        assert_eq!(unix_to_rfc3339(1_000_000_000).unwrap().len(), 20);
    }

    #[test]
    fn out_of_range_seconds_do_not_panic() {
        assert_eq!(unix_to_rfc3339(i64::MAX), None);
        assert_eq!(unix_to_rfc3339(i64::MIN), None);
        assert_eq!(unix_to_rfc3339(253_402_300_800), None);
    }

    #[test]
    fn observed_at_drops_subseconds() {
        assert_eq!(format_now(now()), "2026-10-10T12:34:56Z");
    }

    #[test]
    fn full_observation_has_the_documented_shape() {
        let obs = parse_stdin(
            br#"{"version":"2.1.90","rate_limits":{
                "five_hour":{"used_percentage":23.5,"resets_at":1738425600},
                "seven_day":{"used_percentage":41.2,"resets_at":1738857600}}}"#,
        );
        let value: Value =
            serde_json::from_slice(&Inbox::from_observation(&obs, now()).to_json_bytes()).unwrap();
        assert_eq!(
            value,
            json!({
                "version": 1,
                "observedAt": "2026-10-10T12:34:56Z",
                "fiveHour": {"usedPercentage": 23.5, "resetsAt": "2025-02-01T16:00:00Z"},
                "sevenDay": {"usedPercentage": 41.2, "resetsAt": "2025-02-06T16:00:00Z"},
                "spendLimit": null,
                "claudeCodeVersion": "2.1.90"
            })
        );
    }

    #[test]
    fn missing_windows_serialise_as_explicit_nulls() {
        let obs = parse_stdin(br#"{"rate_limits":{}}"#);
        let value: Value =
            serde_json::from_slice(&Inbox::from_observation(&obs, now()).to_json_bytes()).unwrap();
        assert_eq!(value["fiveHour"], Value::Null);
        assert_eq!(value["sevenDay"], Value::Null);
        assert_eq!(value["spendLimit"], Value::Null);
        assert_eq!(value["claudeCodeVersion"], Value::Null);
        assert!(value.as_object().unwrap().contains_key("fiveHour"));
    }

    #[test]
    fn percentages_are_clamped_for_windows_but_not_the_spend_ceiling() {
        let obs = parse_stdin(
            br#"{"rate_limits":{
                "five_hour":{"used_percentage":104.2,"resets_at":1738425600},
                "seven_day":{"used_percentage":-3,"resets_at":1738425600},
                "spend_limit":{"used_percentage":130.5,"resets_at":1738425600,"used_usd":650,"limit_usd":500,"period":"daily"}}}"#,
        );
        let inbox = Inbox::from_observation(&obs, now());
        assert_eq!(inbox.five_hour.unwrap().used_percentage, Some(100.0));
        assert_eq!(inbox.seven_day.unwrap().used_percentage, Some(0.0));
        let spend = inbox.spend_limit.unwrap();
        assert_eq!(spend.used_percentage, Some(130.5));
        assert_eq!(spend.used_usd, Some(650.0));
        assert_eq!(spend.period.as_deref(), Some("daily"));
    }

    #[test]
    fn unrepresentable_reset_becomes_null_not_an_error() {
        let obs = Observation {
            rate_limits_present: true,
            five_hour: Some(Window {
                used_percentage: Some(10.0),
                resets_at: Some(i64::MAX),
            }),
            ..Observation::default()
        };
        let inbox = Inbox::from_observation(&obs, now());
        assert_eq!(inbox.five_hour.unwrap().resets_at, None);
    }

    #[test]
    fn write_inbox_creates_dir_and_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nested").join("inbox");
        let obs = parse_stdin(
            br#"{"rate_limits":{"five_hour":{"used_percentage":12,"resets_at":1738425600}}}"#,
        );
        let inbox = Inbox::from_observation(&obs, now());
        let path = write_inbox(&dir, &inbox).unwrap();
        assert_eq!(path, dir.join(INBOX_FILE_NAME));
        let read: Inbox = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(read, inbox);
        // Only the final file remains.
        let count = fs::read_dir(&dir).unwrap().count();
        assert_eq!(count, 1);
    }

    #[test]
    fn write_inbox_overwrites_previous_content() {
        let tmp = tempfile::tempdir().unwrap();
        let mk = |pct: u32| {
            let json = format!(
                r#"{{"rate_limits":{{"five_hour":{{"used_percentage":{pct},"resets_at":1738425600}}}}}}"#
            );
            Inbox::from_observation(&parse_stdin(json.as_bytes()), now())
        };
        write_inbox(tmp.path(), &mk(10)).unwrap();
        write_inbox(tmp.path(), &mk(55)).unwrap();
        let read: Inbox =
            serde_json::from_slice(&fs::read(tmp.path().join(INBOX_FILE_NAME)).unwrap()).unwrap();
        assert_eq!(read.five_hour.unwrap().used_percentage, Some(55.0));
    }
}
