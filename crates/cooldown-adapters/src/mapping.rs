//! Shared building blocks of the pure mapping functions.
//!
//! The rules here are the executable form of `packages/snapshot-schema/fixtures/README.md`
//! ("Semantic rules"): identity and time, the status table, and the note catalogue. They are pure:
//! no clock, no disk, no network. The adapters feed them a [`MapContext`] and a decoded payload.

use cooldown_core::adapter::{truncate_note, PollContext};
use cooldown_core::model::{
    rfc3339_utc, snapshot_id, Snapshot, SnapshotSource, SourceKind, Status, Unit, WindowKind,
};
use serde::Deserialize;
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// Note on a window that Claude Code dropped after its reset time.
pub const NOTE_RESET: &str = "window reset";
/// Note on a snapshot whose reset time has already passed.
pub const NOTE_PAST_RESET: &str = "Reset time has already passed; waiting for a fresh reading";

/// Everything a mapping function needs besides the raw payload. It is also the shape of a fixture's
/// `<case>.context.json`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MapContext {
    /// The time of this poll.
    #[serde(with = "rfc3339_utc")]
    pub now: OffsetDateTime,
    /// Local account label the adapter was configured with.
    pub account: String,
    /// Goes into `source.adapterVersion`.
    pub adapter_version: String,
    /// When the source produced the payload, if that differs from `now` (stale inbox files).
    #[serde(default, with = "rfc3339_utc::option")]
    pub observed_at: Option<OffsetDateTime>,
    /// The adapter's last emitted snapshots: how a value is carried forward and a reset recognised.
    #[serde(default)]
    pub previous: Vec<Snapshot>,
    /// HTTP status of the response (API-key adapters only).
    #[serde(default)]
    pub http_status: Option<u16>,
}

impl MapContext {
    /// A context with no previous snapshots, no observation time and no HTTP status.
    pub fn new(
        now: OffsetDateTime,
        account: impl Into<String>,
        adapter_version: impl Into<String>,
    ) -> Self {
        MapContext {
            now,
            account: account.into(),
            adapter_version: adapter_version.into(),
            observed_at: None,
            previous: Vec::new(),
            http_status: None,
        }
    }

    /// The context of a real poll: time and previous snapshots come from the registry.
    pub fn from_poll(poll: &PollContext, account: &str, adapter_version: &str) -> Self {
        MapContext {
            now: poll.now,
            account: account.to_owned(),
            adapter_version: adapter_version.to_owned(),
            observed_at: None,
            previous: poll.previous.clone(),
            http_status: None,
        }
    }

    /// The same context with an HTTP status set.
    pub fn with_http_status(mut self, status: u16) -> Self {
        self.http_status = Some(status);
        self
    }

    /// The same context with an observation time set.
    pub fn with_observed_at(mut self, observed_at: OffsetDateTime) -> Self {
        self.observed_at = Some(observed_at);
        self
    }
}

/// What differs between adapters when building a snapshot.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Meta {
    /// `source.adapter`, also the fixture directory name.
    pub id: &'static str,
    /// `provider`.
    pub provider: &'static str,
    /// `source.kind`.
    pub kind: SourceKind,
}

/// A minimal `ok` snapshot: observed `now`, no figures.
pub(crate) fn make(meta: &Meta, ctx: &MapContext, window: WindowKind) -> Snapshot {
    let unit = if window == WindowKind::Balance {
        Unit::Usd
    } else {
        Unit::Percent
    };
    Snapshot::new(
        meta.provider,
        ctx.account.as_str(),
        window,
        unit,
        SnapshotSource {
            kind: meta.kind,
            adapter: meta.id.to_owned(),
            adapter_version: ctx.adapter_version.clone(),
        },
        ctx.now,
    )
}

/// No fresh value for this window. Keep the last good value if there is one (`degraded`, or
/// `stale` when its reset time has passed); otherwise `unavailable`.
///
/// `unavailable_note` replaces `reason` as the note of an `unavailable` snapshot.
pub(crate) fn carry_or_unavailable(
    meta: &Meta,
    ctx: &MapContext,
    window: WindowKind,
    reason: &str,
    unavailable_note: Option<&str>,
) -> Snapshot {
    let id = snapshot_id(meta.provider, &ctx.account, window);
    if let Some(previous) = ctx.previous.iter().find(|s| s.id == id && s.has_value()) {
        let expired = previous.resets_at.is_some_and(|r| r <= ctx.now);
        let mut carried = previous.clone();
        if expired {
            carried.status = Status::Stale;
            carried.note = Some(truncate_note(&format!(
                "{reason}; the last known value is past its reset time"
            )));
        } else {
            carried.status = Status::Degraded;
            carried.note = Some(truncate_note(&format!(
                "{reason}; showing the last known value"
            )));
        }
        return carried;
    }
    let mut snapshot = make(meta, ctx, window);
    snapshot.status = Status::Unavailable;
    snapshot.note = Some(truncate_note(unavailable_note.unwrap_or(reason)));
    snapshot
}

/// A usable percentage window: `ok`, or `stale` when its reset time is already in the past.
pub(crate) fn percent_window(
    meta: &Meta,
    ctx: &MapContext,
    window: WindowKind,
    percent: f64,
    resets_at_seconds: Option<f64>,
    observed_at: OffsetDateTime,
) -> Snapshot {
    let resets_at = resets_at_seconds
        .filter(|seconds| *seconds > 0.0)
        .and_then(datetime_from_seconds);
    let stale = resets_at.is_some_and(|r| r <= ctx.now);
    let mut snapshot = make(meta, ctx, window);
    snapshot.percent_used = Some(clamp_percent(percent));
    snapshot.resets_at = resets_at;
    snapshot.observed_at = observed_at;
    if stale {
        snapshot.status = Status::Stale;
        snapshot.note = Some(NOTE_PAST_RESET.to_owned());
    }
    snapshot
}

/// A finite JSON number. Strings, booleans, `null` and absent values are not numbers.
pub(crate) fn num(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64).filter(|v| v.is_finite())
}

/// `value` as a JSON object, or `None`.
pub(crate) fn obj(value: Option<&Value>) -> Option<&Value> {
    value.filter(|v| v.is_object())
}

/// Clamp to 0..=100, without rounding.
pub(crate) fn clamp_percent(value: f64) -> f64 {
    value.clamp(0.0, 100.0)
}

/// `Math.round` as JavaScript does it: ties go toward positive infinity.
fn round_half_up(x: f64) -> f64 {
    let floor = x.floor();
    if x - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    }
}

/// Round to `decimals` places the way the TypeScript reference does (`+ 0` turns `-0` into `0`).
pub(crate) fn round_to(value: f64, decimals: i32) -> f64 {
    let factor = 10f64.powi(decimals);
    round_half_up(value * factor) / factor + 0.0
}

/// Money is rounded to 6 decimals.
pub(crate) fn round_money(value: f64) -> f64 {
    round_to(value, 6)
}

/// Derived percentages are rounded to 4 decimals.
pub(crate) fn round_percent(value: f64) -> f64 {
    round_to(value, 4)
}

/// Whole seconds (rounded down) since the Unix epoch as a UTC time; `None` when out of range.
pub(crate) fn datetime_from_seconds(seconds: f64) -> Option<OffsetDateTime> {
    if !seconds.is_finite() || seconds.abs() > 1e15 {
        return None;
    }
    OffsetDateTime::from_unix_timestamp(seconds.floor() as i64).ok()
}

/// Parse an RFC 3339 timestamp of any offset to UTC, dropping fractional seconds.
pub(crate) fn parse_rfc3339_secs(text: &str) -> Option<OffsetDateTime> {
    let parsed = OffsetDateTime::parse(text, &Rfc3339).ok()?;
    let utc = parsed.to_offset(time::UtcOffset::UTC);
    utc.replace_nanosecond(0).ok()
}

/// At most `max_units` UTF-16 code units of `text`, never splitting a character.
pub(crate) fn truncate_units(text: &str, max_units: usize) -> String {
    let mut out = String::new();
    let mut units = 0;
    for c in text.chars() {
        units += c.len_utf16();
        if units > max_units {
            break;
        }
        out.push(c);
    }
    out
}

/// `<name> request failed (HTTP <status>)` for a non-2xx status.
pub(crate) fn http_failure(name: &str, ctx: &MapContext) -> Option<String> {
    ctx.http_status
        .filter(|status| !(200..300).contains(status))
        .map(|status| format!("{name} request failed (HTTP {status})"))
}

/// The single `balance` snapshot of a failed API call: the HTTP failure if there was one, schema
/// drift otherwise.
pub(crate) fn balance_failure(meta: &Meta, ctx: &MapContext, name: &str) -> Vec<Snapshot> {
    let reason = http_failure(name, ctx)
        .unwrap_or_else(|| format!("{name} response did not match the expected shape"));
    vec![carry_or_unavailable(
        meta,
        ctx,
        WindowKind::Balance,
        &reason,
        None,
    )]
}

/// A decimal string as a number: plainly numeric and finite, or `None`.
pub(crate) fn decimal(value: Option<&Value>) -> Option<f64> {
    let text = value.and_then(Value::as_str)?.trim();
    if text.is_empty() {
        return None;
    }
    text.parse::<f64>().ok().filter(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const META: Meta = Meta {
        id: "openrouter",
        provider: "openrouter",
        kind: SourceKind::Official,
    };

    fn ctx() -> MapContext {
        MapContext::new(datetime!(2026-10-10 12:00:00 UTC), "default", "0.1.0")
    }

    #[test]
    fn rounding_matches_javascript() {
        assert_eq!(round_to(2.5, 0), 3.0);
        assert_eq!(round_to(-2.5, 0), -2.0);
        assert_eq!(round_to(0.5, 0), 1.0);
        assert_eq!(round_money(-0.0), 0.0);
        assert!(round_money(-0.0).is_sign_positive());
        assert_eq!(round_percent(49.0), 49.0);
        assert_eq!(round_money(25.5 - 24.5), 1.0);
    }

    #[test]
    fn seconds_to_time_floors_and_rejects_nonsense() {
        assert_eq!(
            datetime_from_seconds(1_791_641_400.9),
            Some(datetime!(2026-10-10 14:10:00 UTC))
        );
        assert_eq!(datetime_from_seconds(f64::NAN), None);
        assert_eq!(datetime_from_seconds(1e300), None);
    }

    #[test]
    fn rfc3339_is_normalized_to_whole_utc_seconds() {
        assert_eq!(
            parse_rfc3339_secs("2026-10-10T11:58:30.412Z"),
            Some(datetime!(2026-10-10 11:58:30 UTC))
        );
        assert_eq!(
            parse_rfc3339_secs("2026-10-10T13:58:30+02:00"),
            Some(datetime!(2026-10-10 11:58:30 UTC))
        );
        assert_eq!(parse_rfc3339_secs("yesterday"), None);
    }

    #[test]
    fn notes_are_cut_at_the_schema_limit_without_splitting_characters() {
        let long = "é".repeat(600);
        let cut = truncate_note(&long);
        assert_eq!(cut.encode_utf16().count(), 500);
        assert!(cut.ends_with('…'));
        assert_eq!(truncate_note("short"), "short");
        assert_eq!(truncate_units("😀😀😀", 3), "😀");
    }

    #[test]
    fn numbers_must_be_json_numbers() {
        let v = serde_json::json!({"a": 1, "b": "2", "c": null, "d": true, "e": 1.5});
        assert_eq!(num(v.get("a")), Some(1.0));
        assert_eq!(num(v.get("b")), None);
        assert_eq!(num(v.get("c")), None);
        assert_eq!(num(v.get("d")), None);
        assert_eq!(num(v.get("e")), Some(1.5));
        assert_eq!(num(v.get("missing")), None);
    }

    #[test]
    fn decimals_must_be_plain_finite_strings() {
        let v =
            serde_json::json!({"a": "12.50", "b": " 3 ", "c": "", "d": "abc", "e": "inf", "f": 5});
        assert_eq!(decimal(v.get("a")), Some(12.5));
        assert_eq!(decimal(v.get("b")), Some(3.0));
        for key in ["c", "d", "e", "f", "missing"] {
            assert_eq!(decimal(v.get(key)), None, "{key}");
        }
    }

    #[test]
    fn http_failures_name_the_status_only() {
        assert_eq!(http_failure("OpenRouter", &ctx()), None);
        assert_eq!(
            http_failure("OpenRouter", &ctx().with_http_status(204)),
            None
        );
        assert_eq!(
            http_failure("OpenRouter", &ctx().with_http_status(401)).as_deref(),
            Some("OpenRouter request failed (HTTP 401)")
        );
    }

    #[test]
    fn carry_forward_keeps_the_previous_value_and_age() {
        let mut previous = make(&META, &ctx(), WindowKind::Balance);
        previous.remaining = Some(5.0);
        previous.observed_at = datetime!(2026-10-10 11:00:00 UTC);
        let mut c = ctx();
        c.previous = vec![previous.clone()];
        let carried = carry_or_unavailable(&META, &c, WindowKind::Balance, "boom", None);
        assert_eq!(carried.status, Status::Degraded);
        assert_eq!(carried.remaining, Some(5.0));
        assert_eq!(carried.observed_at, previous.observed_at);
        assert_eq!(
            carried.note.as_deref(),
            Some("boom; showing the last known value")
        );

        let mut expired = previous;
        expired.resets_at = Some(datetime!(2026-10-10 11:30:00 UTC));
        c.previous = vec![expired];
        let carried = carry_or_unavailable(&META, &c, WindowKind::Balance, "boom", None);
        assert_eq!(carried.status, Status::Stale);
        assert_eq!(
            carried.note.as_deref(),
            Some("boom; the last known value is past its reset time")
        );
    }

    #[test]
    fn nothing_to_carry_is_unavailable_with_the_reason() {
        let s = carry_or_unavailable(&META, &ctx(), WindowKind::Balance, "boom", None);
        assert_eq!(s.status, Status::Unavailable);
        assert!(!s.has_value());
        assert_eq!(s.note.as_deref(), Some("boom"));
        assert_eq!(s.observed_at, ctx().now);
        let s = carry_or_unavailable(&META, &ctx(), WindowKind::Balance, "boom", Some("other"));
        assert_eq!(s.note.as_deref(), Some("other"));
    }

    #[test]
    fn context_json_round_trips_the_fixture_shape() {
        let json = r#"{"now":"2026-10-10T12:00:00Z","account":"default","adapterVersion":"0.1.0","observedAt":"2026-10-10T08:30:00Z","httpStatus":401}"#;
        let parsed: MapContext = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.observed_at, Some(datetime!(2026-10-10 08:30:00 UTC)));
        assert_eq!(parsed.http_status, Some(401));
        assert!(parsed.previous.is_empty());
        assert!(serde_json::from_str::<MapContext>(
            r#"{"now":"2026-10-10T12:00:00Z","account":"a","adapterVersion":"1","extra":1}"#
        )
        .is_err());
    }
}
