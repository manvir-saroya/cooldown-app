//! The snapshot contract, mirrored from `packages/snapshot-schema/src/types.ts`.
//!
//! Every type here serializes to exactly the JSON the TypeScript validators accept: camelCase keys,
//! unknown keys rejected, optional fields omitted when `None`, timestamps as RFC 3339 UTC with a
//! `Z` suffix. [`Snapshot::validate`] mirrors the extra rules the zod schema enforces beyond shapes
//! (id equality, percent range, account hygiene).

use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// The only schema version this crate understands.
pub const SCHEMA_VERSION: u32 = 1;

/// Default age after which an observation counts as stale: 30 minutes (same as the TS helpers).
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(30 * 60);

/// Serde adapters for the `time` crate: RFC 3339, always UTC.
///
/// Serializing normalizes to a UTC offset (so the output always ends in `Z`). Deserializing accepts
/// `Z` or `+00:00` and rejects any other offset, matching the TypeScript validators, which refuse
/// offsets such as `+02:00`.
pub mod rfc3339_utc {
    use serde::{Deserializer, Serializer};
    use time::{OffsetDateTime, UtcOffset};

    /// Serialize as RFC 3339 in UTC.
    pub fn serialize<S: Serializer>(value: &OffsetDateTime, ser: S) -> Result<S::Ok, S::Error> {
        time::serde::rfc3339::serialize(&value.to_offset(UtcOffset::UTC), ser)
    }

    /// Deserialize RFC 3339, requiring a UTC offset.
    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<OffsetDateTime, D::Error> {
        let value = time::serde::rfc3339::deserialize(de)?;
        require_utc(value)
    }

    fn require_utc<E: serde::de::Error>(value: OffsetDateTime) -> Result<OffsetDateTime, E> {
        if value.offset() == UtcOffset::UTC {
            Ok(value)
        } else {
            Err(E::custom(
                "timestamp must be UTC (suffix \"Z\"), not a local offset",
            ))
        }
    }

    /// The same for `Option<OffsetDateTime>`.
    pub mod option {
        use serde::{Deserializer, Serializer};
        use time::{OffsetDateTime, UtcOffset};

        /// Serialize `Some` as RFC 3339 UTC, `None` as null.
        pub fn serialize<S: Serializer>(
            value: &Option<OffsetDateTime>,
            ser: S,
        ) -> Result<S::Ok, S::Error> {
            time::serde::rfc3339::option::serialize(
                &value.map(|v| v.to_offset(UtcOffset::UTC)),
                ser,
            )
        }

        /// Deserialize an optional RFC 3339 timestamp, requiring a UTC offset.
        pub fn deserialize<'de, D: Deserializer<'de>>(
            de: D,
        ) -> Result<Option<OffsetDateTime>, D::Error> {
            match time::serde::rfc3339::option::deserialize(de)? {
                Some(value) => super::require_utc(value).map(Some),
                None => Ok(None),
            }
        }
    }
}

/// How trustworthy a figure is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    /// Straight from the provider's documented interface.
    Official,
    /// Computed from local logs.
    Estimated,
    /// Read from an undocumented or experimental interface.
    Scraped,
    /// Typed in by the user.
    Manual,
}

/// Which limit a snapshot describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum WindowKind {
    /// Rolling 5-hour session window.
    #[serde(rename = "session_5h")]
    Session5h,
    /// Weekly window.
    #[serde(rename = "weekly")]
    Weekly,
    /// Monthly window.
    #[serde(rename = "monthly")]
    Monthly,
    /// Daily window.
    #[serde(rename = "daily")]
    Daily,
    /// Prepaid or capped spend balance (no reset).
    #[serde(rename = "balance")]
    Balance,
}

impl WindowKind {
    /// Every window kind.
    pub const ALL: [WindowKind; 5] = [
        WindowKind::Session5h,
        WindowKind::Weekly,
        WindowKind::Monthly,
        WindowKind::Daily,
        WindowKind::Balance,
    ];

    /// The wire name, e.g. `"session_5h"`.
    pub fn as_str(self) -> &'static str {
        match self {
            WindowKind::Session5h => "session_5h",
            WindowKind::Weekly => "weekly",
            WindowKind::Monthly => "monthly",
            WindowKind::Daily => "daily",
            WindowKind::Balance => "balance",
        }
    }
}

impl fmt::Display for WindowKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WindowKind {
    type Err = ValidationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        WindowKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == s)
            .ok_or(ValidationError::UnknownWindow)
    }
}

/// Health of a snapshot's figures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Fresh and trustworthy.
    Ok,
    /// Old, or past its reset time.
    Stale,
    /// The adapter is struggling; the figures are the last known ones.
    Degraded,
    /// The adapter has no figures at all.
    Unavailable,
}

impl Status {
    /// Higher is worse. A derived status may never be better than the stored one.
    pub fn severity(self) -> u8 {
        match self {
            Status::Ok => 0,
            Status::Stale => 1,
            Status::Degraded => 2,
            Status::Unavailable => 3,
        }
    }

    /// True for `Degraded` and `Unavailable`, the adapter-failure statuses.
    pub fn is_failure(self) -> bool {
        matches!(self, Status::Degraded | Status::Unavailable)
    }
}

/// What the numbers count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    /// Percent of a limit.
    Percent,
    /// US dollars.
    Usd,
    /// Chinese yuan (renminbi). Only balances use it.
    Cny,
    /// Tokens.
    Tokens,
    /// Requests.
    Requests,
}

/// Where a snapshot came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotSource {
    /// How trustworthy the figure is.
    pub kind: SourceKind,
    /// Adapter id, also the fixture directory name.
    pub adapter: String,
    /// Adapter version.
    pub adapter_version: String,
}

/// One limit window of one account at one provider, as last seen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Snapshot {
    /// Always [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// `{provider}:{account}:{window}`: stable, also the basis of alert ids.
    pub id: String,
    /// `claude`, `codex`, `openrouter`, ...
    pub provider: String,
    /// Local label chosen by the user. Never a token or an email address.
    pub account: String,
    /// Which limit.
    pub window: WindowKind,
    /// What the numbers count.
    pub unit: Unit,
    /// Percent used, 0 to 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent_used: Option<f64>,
    /// Amount used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<f64>,
    /// The limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    /// Amount remaining (negative when overspent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<f64>,
    /// When the window resets.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "rfc3339_utc::option"
    )]
    pub resets_at: Option<OffsetDateTime>,
    /// When the adapter last saw this value.
    #[serde(with = "rfc3339_utc")]
    pub observed_at: OffsetDateTime,
    /// Provenance.
    pub source: SnapshotSource,
    /// Health.
    pub status: Status,
    /// Human-readable reason when the status is not `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A user's request to be told when a limit resets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AlertIntent {
    /// `reset:{provider}:{account}:{window}`.
    pub id: String,
    /// When the alert is armed for.
    #[serde(with = "rfc3339_utc")]
    pub fire_at: OffsetDateTime,
    /// The user's switch. A disabled alert never fires.
    pub enabled: bool,
}

/// What crosses the wire between paired devices.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncPayload {
    /// Always [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Sender's device id.
    pub device_id: String,
    /// When the payload was produced.
    #[serde(with = "rfc3339_utc")]
    pub generated_at: OffsetDateTime,
    /// Sanitized snapshots.
    pub snapshots: Vec<Snapshot>,
    /// Reset alerts.
    pub alerts: Vec<AlertIntent>,
}

/// Why a value failed validation. Messages never echo the account label (it must not be a secret,
/// but a mislabelled one should not be copied into logs either).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ValidationError {
    /// `schemaVersion` is not 1.
    #[error("unsupported schemaVersion {0} (expected {SCHEMA_VERSION})")]
    SchemaVersion(u32),
    /// `id` is not `{provider}:{account}:{window}`.
    #[error("id {actual:?} must equal {expected:?}")]
    IdMismatch {
        /// What the id should be.
        expected: String,
        /// What it was.
        actual: String,
    },
    /// `provider` is not a lowercase slug.
    #[error(
        "provider {0:?} must be a lowercase slug of 1 to 32 characters (a-z, 0-9, '_', '.', '-')"
    )]
    Provider(String),
    /// `account` breaks a rule.
    #[error("account is invalid: {0}")]
    Account(AccountProblem),
    /// `percentUsed` outside 0..=100 (or not finite).
    #[error("percentUsed {0} must be within 0..=100")]
    PercentUsed(f64),
    /// A numeric field is NaN or infinite.
    #[error("{0} must be a finite number")]
    NotFinite(&'static str),
    /// A numeric field that cannot be negative is.
    #[error("{0} must not be negative")]
    Negative(&'static str),
    /// A text field is empty or too long.
    #[error("{field} must be 1 to {max} characters")]
    TextLength {
        /// Field name.
        field: &'static str,
        /// Maximum length.
        max: usize,
    },
    /// `note` is too long.
    #[error("note must be at most {0} characters")]
    NoteTooLong(usize),
    /// A window name is not one of the known kinds.
    #[error("unknown window kind")]
    UnknownWindow,
    /// An alert id is not `reset:{provider}:{account}:{window}`.
    #[error("alert id {0:?} must be `reset:<provider>:<account>:<window>`")]
    AlertId(String),
    /// Two snapshots (or alerts) in a payload share an id.
    #[error("duplicate id {0:?}")]
    DuplicateId(String),
    /// A payload list is too long.
    #[error("{what} may hold at most {max} entries")]
    TooMany {
        /// Which list.
        what: &'static str,
        /// The cap.
        max: usize,
    },
}

/// The rule an account label broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AccountProblem {
    /// Empty string.
    #[error("must not be empty")]
    Empty,
    /// More than 64 characters.
    #[error("must be at most 64 characters")]
    TooLong,
    /// Contains `@` (looks like an email address).
    #[error("must not contain '@'")]
    ContainsAt,
    /// Contains `:` (ids are colon-joined).
    #[error("must not contain ':'")]
    ContainsColon,
    /// Contains a control character.
    #[error("must not contain control characters")]
    ControlCharacter,
    /// Starts like a well-known credential.
    #[error("must not look like a token")]
    LooksLikeToken,
}

/// Maximum account label length (UTF-16 code units, same as the TS validators).
pub const MAX_ACCOUNT_LEN: usize = 64;
const MAX_PROVIDER_LEN: usize = 32;
const MAX_NOTE_LEN: usize = 500;
const MAX_ADAPTER_LEN: usize = 64;
const MAX_ADAPTER_VERSION_LEN: usize = 32;
const MAX_DEVICE_ID_LEN: usize = 128;
const MAX_PAYLOAD_ENTRIES: usize = 256;

/// Prefixes of well-known credential formats, compared case-insensitively.
const TOKEN_PREFIXES: [&str; 12] = [
    "sk-",
    "sk_",
    "pk_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "github_pat_",
    "xai-",
    "xoxb-",
    "xoxp-",
    "eyj",
];

/// Length as JavaScript counts it (UTF-16 code units), so limits match the TS validators.
fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn validate_provider(provider: &str) -> Result<(), ValidationError> {
    let mut chars = provider.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest_ok =
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'));
    if first_ok && rest_ok && provider.len() <= MAX_PROVIDER_LEN {
        Ok(())
    } else {
        Err(ValidationError::Provider(provider.to_owned()))
    }
}

/// Checks the structural account rules shared by snapshot ids and alert ids.
fn validate_account_chars(account: &str) -> Result<(), AccountProblem> {
    if account.is_empty() {
        return Err(AccountProblem::Empty);
    }
    if js_len(account) > MAX_ACCOUNT_LEN {
        return Err(AccountProblem::TooLong);
    }
    for c in account.chars() {
        match c {
            '@' => return Err(AccountProblem::ContainsAt),
            ':' => return Err(AccountProblem::ContainsColon),
            c if c <= '\u{1f}' || c == '\u{7f}' => return Err(AccountProblem::ControlCharacter),
            _ => {}
        }
    }
    Ok(())
}

fn looks_like_token(account: &str) -> bool {
    let lower = account.to_lowercase();
    if TOKEN_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix))
    {
        return true;
    }
    lower
        .strip_prefix("bearer")
        .and_then(|rest| rest.chars().next())
        .is_some_and(char::is_whitespace)
}

fn validate_account(account: &str) -> Result<(), ValidationError> {
    validate_account_chars(account).map_err(ValidationError::Account)?;
    if looks_like_token(account) {
        return Err(ValidationError::Account(AccountProblem::LooksLikeToken));
    }
    Ok(())
}

fn check_text(field: &'static str, value: &str, max: usize) -> Result<(), ValidationError> {
    let len = js_len(value);
    if len == 0 || len > max {
        Err(ValidationError::TextLength { field, max })
    } else {
        Ok(())
    }
}

fn check_finite(field: &'static str, value: Option<f64>) -> Result<(), ValidationError> {
    match value {
        Some(v) if !v.is_finite() => Err(ValidationError::NotFinite(field)),
        _ => Ok(()),
    }
}

fn check_non_negative(field: &'static str, value: Option<f64>) -> Result<(), ValidationError> {
    check_finite(field, value)?;
    match value {
        Some(v) if v < 0.0 => Err(ValidationError::Negative(field)),
        _ => Ok(()),
    }
}

impl Snapshot {
    /// A minimal `ok` snapshot with the id derived from its parts. Fill in the figures with struct
    /// update syntax or by assigning fields.
    pub fn new(
        provider: impl Into<String>,
        account: impl Into<String>,
        window: WindowKind,
        unit: Unit,
        source: SnapshotSource,
        observed_at: OffsetDateTime,
    ) -> Self {
        let provider = provider.into();
        let account = account.into();
        Snapshot {
            schema_version: SCHEMA_VERSION,
            id: snapshot_id(&provider, &account, window),
            provider,
            account,
            window,
            unit,
            percent_used: None,
            used: None,
            limit: None,
            remaining: None,
            resets_at: None,
            observed_at,
            source,
            status: Status::Ok,
            note: None,
        }
    }

    /// `reset:{provider}:{account}:{window}`: the id of this window's reset alert.
    pub fn alert_id(&self) -> String {
        alert_id(&self.provider, &self.account, self.window)
    }

    /// True when any figure (percent, used, limit, remaining) is present.
    pub fn has_value(&self) -> bool {
        self.percent_used.is_some()
            || self.used.is_some()
            || self.limit.is_some()
            || self.remaining.is_some()
    }

    /// Check the rules the TypeScript validators enforce beyond the bare shape.
    ///
    /// - `schemaVersion` is 1;
    /// - `provider` is a lowercase slug;
    /// - `account` is 1 to 64 characters, with no `@`, `:`, control characters or token-like prefix;
    /// - `id` equals `{provider}:{account}:{window}`;
    /// - `percentUsed` is within 0..=100; `used` and `limit` are non-negative; all are finite;
    /// - `source.adapter` and `source.adapterVersion` are non-empty and short; `note` is at most
    ///   500 characters.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ValidationError::SchemaVersion(self.schema_version));
        }
        validate_provider(&self.provider)?;
        validate_account(&self.account)?;
        let expected = snapshot_id(&self.provider, &self.account, self.window);
        if self.id != expected {
            return Err(ValidationError::IdMismatch {
                expected,
                actual: self.id.clone(),
            });
        }
        if let Some(p) = self.percent_used {
            if !p.is_finite() || !(0.0..=100.0).contains(&p) {
                return Err(ValidationError::PercentUsed(p));
            }
        }
        check_non_negative("used", self.used)?;
        check_non_negative("limit", self.limit)?;
        check_finite("remaining", self.remaining)?;
        check_text("source.adapter", &self.source.adapter, MAX_ADAPTER_LEN)?;
        check_text(
            "source.adapterVersion",
            &self.source.adapter_version,
            MAX_ADAPTER_VERSION_LEN,
        )?;
        if self
            .note
            .as_deref()
            .is_some_and(|n| js_len(n) > MAX_NOTE_LEN)
        {
            return Err(ValidationError::NoteTooLong(MAX_NOTE_LEN));
        }
        Ok(())
    }
}

/// The parts of a parsed alert id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertIdParts {
    /// Provider slug.
    pub provider: String,
    /// Account label.
    pub account: String,
    /// Window kind.
    pub window: WindowKind,
}

/// Parse `reset:{provider}:{account}:{window}`. Returns `None` for anything else.
pub fn parse_alert_id(id: &str) -> Option<AlertIdParts> {
    let rest = id.strip_prefix("reset:")?;
    let mut parts = rest.split(':');
    let (provider, account, window) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    validate_provider(provider).ok()?;
    validate_account_chars(account).ok()?;
    Some(AlertIdParts {
        provider: provider.to_owned(),
        account: account.to_owned(),
        window: window.parse().ok()?,
    })
}

impl AlertIntent {
    /// An enabled alert.
    pub fn new(id: impl Into<String>, fire_at: OffsetDateTime) -> Self {
        AlertIntent {
            id: id.into(),
            fire_at,
            enabled: true,
        }
    }

    /// Check that the id is `reset:{provider}:{account}:{window}`.
    pub fn validate(&self) -> Result<(), ValidationError> {
        parse_alert_id(&self.id)
            .map(|_| ())
            .ok_or_else(|| ValidationError::AlertId(self.id.clone()))
    }
}

impl SyncPayload {
    /// Validate the version, device id, list sizes, every snapshot and alert, and id uniqueness
    /// (alerts are replaced, never stacked).
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ValidationError::SchemaVersion(self.schema_version));
        }
        check_text("deviceId", &self.device_id, MAX_DEVICE_ID_LEN)?;
        if self.snapshots.len() > MAX_PAYLOAD_ENTRIES {
            return Err(ValidationError::TooMany {
                what: "snapshots",
                max: MAX_PAYLOAD_ENTRIES,
            });
        }
        if self.alerts.len() > MAX_PAYLOAD_ENTRIES {
            return Err(ValidationError::TooMany {
                what: "alerts",
                max: MAX_PAYLOAD_ENTRIES,
            });
        }
        let mut seen = HashSet::new();
        for snapshot in &self.snapshots {
            snapshot.validate()?;
            if !seen.insert(snapshot.id.as_str()) {
                return Err(ValidationError::DuplicateId(snapshot.id.clone()));
            }
        }
        let mut seen = HashSet::new();
        for alert in &self.alerts {
            alert.validate()?;
            if !seen.insert(alert.id.as_str()) {
                return Err(ValidationError::DuplicateId(alert.id.clone()));
            }
        }
        Ok(())
    }
}

/// `{provider}:{account}:{window}`.
pub fn snapshot_id(provider: &str, account: &str, window: WindowKind) -> String {
    format!("{provider}:{account}:{window}")
}

/// `reset:{provider}:{account}:{window}`.
pub fn alert_id(provider: &str, account: &str, window: WindowKind) -> String {
    format!("reset:{provider}:{account}:{window}")
}

/// Nominal window lengths in minutes, in the order the TS helper tries them.
const NOMINAL_WINDOW_MINUTES: [(WindowKind, f64); 4] = [
    (WindowKind::Session5h, 300.0),
    (WindowKind::Daily, 1440.0),
    (WindowKind::Weekly, 10_080.0),
    (WindowKind::Monthly, 43_200.0),
];

/// Default relative tolerance for [`window_from_duration_minutes`]: 10%, which also covers
/// 28 to 31 day months.
pub const DEFAULT_WINDOW_TOLERANCE: f64 = 0.1;

/// Classify a window by its DURATION, never by which slot a provider put it in (Codex has been
/// seen returning the weekly window in its `primary` slot). About 300 minutes is `session_5h`,
/// 1440 `daily`, 10080 `weekly`, 43200 `monthly`, each within 10%. Never returns `balance`.
pub fn window_from_duration_minutes(minutes: f64) -> Option<WindowKind> {
    window_from_duration_minutes_tol(minutes, DEFAULT_WINDOW_TOLERANCE)
}

/// [`window_from_duration_minutes`] with an explicit relative tolerance.
pub fn window_from_duration_minutes_tol(minutes: f64, tolerance: f64) -> Option<WindowKind> {
    if !minutes.is_finite() || minutes <= 0.0 {
        return None;
    }
    NOMINAL_WINDOW_MINUTES
        .iter()
        .map(|&(kind, nominal)| (kind, (minutes - nominal).abs() / nominal))
        .filter(|&(_, distance)| distance <= tolerance)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(kind, _)| kind)
}

/// The status a consumer should display right now.
///
/// - A snapshot whose `resetsAt` is at or before `now` is `Stale` (spec section 4).
/// - A snapshot observed more than `max_age` before `now` is `Stale`. A future `observedAt`
///   (clock skew) counts as age zero.
/// - `Degraded` and `Unavailable` are never upgraded: the result is the worse of the stored status
///   and the time-derived one.
/// - `max_age` of zero falls back to [`DEFAULT_MAX_AGE`]; `Duration::MAX` disables the age check.
pub fn effective_status(snapshot: &Snapshot, now: OffsetDateTime, max_age: Duration) -> Status {
    let max_age = if max_age.is_zero() {
        DEFAULT_MAX_AGE
    } else {
        max_age
    };
    let mut derived = Status::Ok;
    if snapshot.resets_at.is_some_and(|resets_at| resets_at <= now) {
        derived = Status::Stale;
    }
    let age = now - snapshot.observed_at;
    let too_old = match time::Duration::try_from(max_age) {
        Ok(limit) => age > limit,
        Err(_) => false,
    };
    if too_old {
        derived = Status::Stale;
    }
    if derived.severity() > snapshot.status.severity() {
        derived
    } else {
        snapshot.status
    }
}

/// Whole seconds from `now` until `resetsAt`, rounded up, never negative. `None` when the snapshot
/// has no reset time.
pub fn seconds_until_reset(snapshot: &Snapshot, now: OffsetDateTime) -> Option<u64> {
    let delta = snapshot.resets_at? - now;
    if delta.is_negative() || delta.is_zero() {
        return Some(0);
    }
    let whole = delta.whole_seconds();
    let rounded = if delta.subsec_nanoseconds() > 0 {
        whole + 1
    } else {
        whole
    };
    Some(u64::try_from(rounded).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn source() -> SnapshotSource {
        SnapshotSource {
            kind: SourceKind::Official,
            adapter: "claude-statusline".into(),
            adapter_version: "0.1.0".into(),
        }
    }

    fn sample() -> Snapshot {
        let mut s = Snapshot::new(
            "claude",
            "default",
            WindowKind::Session5h,
            Unit::Percent,
            source(),
            datetime!(2026-10-10 12:00:00 UTC),
        );
        s.percent_used = Some(23.5);
        s.resets_at = Some(datetime!(2026-10-10 14:37:00 UTC));
        s
    }

    #[test]
    fn json_matches_the_typescript_shape() {
        let json = serde_json::to_value(sample()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "schemaVersion": 1,
                "id": "claude:default:session_5h",
                "provider": "claude",
                "account": "default",
                "window": "session_5h",
                "unit": "percent",
                "percentUsed": 23.5,
                "resetsAt": "2026-10-10T14:37:00Z",
                "observedAt": "2026-10-10T12:00:00Z",
                "source": {"kind": "official", "adapter": "claude-statusline", "adapterVersion": "0.1.0"},
                "status": "ok"
            })
        );
    }

    #[test]
    fn json_round_trips_and_skips_none() {
        let s = sample();
        let text = serde_json::to_string(&s).unwrap();
        assert!(!text.contains("note"));
        assert!(!text.contains("used\""));
        let back: Snapshot = serde_json::from_str(&text).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn unknown_fields_are_rejected_everywhere() {
        let mut v = serde_json::to_value(sample()).unwrap();
        v["extra"] = 1.into();
        assert!(serde_json::from_value::<Snapshot>(v).is_err());

        let mut v = serde_json::to_value(sample()).unwrap();
        v["source"]["extra"] = 1.into();
        assert!(serde_json::from_value::<Snapshot>(v).is_err());

        let alert = serde_json::json!({"id": "reset:a:b:weekly", "fireAt": "2026-10-10T12:00:00Z", "enabled": true, "x": 1});
        assert!(serde_json::from_value::<AlertIntent>(alert).is_err());
    }

    #[test]
    fn timestamps_must_be_utc_and_are_normalized() {
        let mut v = serde_json::to_value(sample()).unwrap();
        v["observedAt"] = "2026-10-10T14:00:00+02:00".into();
        assert!(serde_json::from_value::<Snapshot>(v).is_err());

        let mut v = serde_json::to_value(sample()).unwrap();
        v["resetsAt"] = "2026-10-10T14:37:00+00:00".into();
        let parsed: Snapshot = serde_json::from_value(v).unwrap();
        let out = serde_json::to_value(&parsed).unwrap();
        assert_eq!(out["resetsAt"], "2026-10-10T14:37:00Z");

        // A non-UTC value built in code still serializes as UTC.
        let mut local = sample();
        local.observed_at = datetime!(2026-10-10 14:00:00 +2);
        let out = serde_json::to_value(&local).unwrap();
        assert_eq!(out["observedAt"], "2026-10-10T12:00:00Z");
    }

    #[test]
    fn enums_use_wire_names() {
        for (kind, name) in [
            (WindowKind::Session5h, "session_5h"),
            (WindowKind::Weekly, "weekly"),
            (WindowKind::Monthly, "monthly"),
            (WindowKind::Daily, "daily"),
            (WindowKind::Balance, "balance"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), name);
            assert_eq!(kind.to_string(), name);
            assert_eq!(name.parse::<WindowKind>().unwrap(), kind);
        }
        assert!("hourly".parse::<WindowKind>().is_err());
        assert_eq!(
            serde_json::to_value(Status::Unavailable).unwrap(),
            "unavailable"
        );
        assert_eq!(
            serde_json::to_value(SourceKind::Scraped).unwrap(),
            "scraped"
        );
        assert_eq!(serde_json::to_value(Unit::Usd).unwrap(), "usd");
        assert_eq!(serde_json::to_value(Unit::Cny).unwrap(), "cny");
    }

    #[test]
    fn validate_accepts_a_good_snapshot() {
        assert_eq!(sample().validate(), Ok(()));
    }

    #[test]
    fn validate_rules_table() {
        type Case = (
            &'static str,
            fn(&mut Snapshot),
            fn(&ValidationError) -> bool,
        );
        let cases: Vec<Case> = vec![
            (
                "schema version",
                |s| s.schema_version = 2,
                |e| matches!(e, ValidationError::SchemaVersion(2)),
            ),
            (
                "id mismatch",
                |s| s.id = "claude:default:weekly".into(),
                |e| matches!(e, ValidationError::IdMismatch { .. }),
            ),
            (
                "percent too high",
                |s| s.percent_used = Some(100.1),
                |e| matches!(e, ValidationError::PercentUsed(_)),
            ),
            (
                "percent negative",
                |s| s.percent_used = Some(-0.1),
                |e| matches!(e, ValidationError::PercentUsed(_)),
            ),
            (
                "percent NaN",
                |s| s.percent_used = Some(f64::NAN),
                |e| matches!(e, ValidationError::PercentUsed(_)),
            ),
            (
                "used negative",
                |s| s.used = Some(-1.0),
                |e| matches!(e, ValidationError::Negative("used")),
            ),
            (
                "limit infinite",
                |s| s.limit = Some(f64::INFINITY),
                |e| matches!(e, ValidationError::NotFinite("limit")),
            ),
            (
                "provider upper case",
                |s| {
                    s.provider = "Claude".into();
                    s.id = "Claude:default:session_5h".into();
                },
                |e| matches!(e, ValidationError::Provider(_)),
            ),
            (
                "account email",
                |s| {
                    s.account = "me@example.com".into();
                    s.id = snapshot_id(&s.provider, &s.account, s.window);
                },
                |e| matches!(e, ValidationError::Account(AccountProblem::ContainsAt)),
            ),
            (
                "account colon",
                |s| {
                    s.account = "a:b".into();
                    s.id = snapshot_id(&s.provider, &s.account, s.window);
                },
                |e| matches!(e, ValidationError::Account(AccountProblem::ContainsColon)),
            ),
            (
                "account empty",
                |s| {
                    s.account = String::new();
                    s.id = snapshot_id(&s.provider, &s.account, s.window);
                },
                |e| matches!(e, ValidationError::Account(AccountProblem::Empty)),
            ),
            (
                "account too long",
                |s| {
                    s.account = "a".repeat(65);
                    s.id = snapshot_id(&s.provider, &s.account, s.window);
                },
                |e| matches!(e, ValidationError::Account(AccountProblem::TooLong)),
            ),
            (
                "account control char",
                |s| {
                    s.account = "a\nb".into();
                    s.id = snapshot_id(&s.provider, &s.account, s.window);
                },
                |e| {
                    matches!(
                        e,
                        ValidationError::Account(AccountProblem::ControlCharacter)
                    )
                },
            ),
            (
                "account token",
                |s| {
                    s.account = "sk-or-v1-abc".into();
                    s.id = snapshot_id(&s.provider, &s.account, s.window);
                },
                |e| matches!(e, ValidationError::Account(AccountProblem::LooksLikeToken)),
            ),
            (
                "account bearer",
                |s| {
                    s.account = "Bearer abc".into();
                    s.id = snapshot_id(&s.provider, &s.account, s.window);
                },
                |e| matches!(e, ValidationError::Account(AccountProblem::LooksLikeToken)),
            ),
            (
                "account jwt",
                |s| {
                    s.account = "eyJhbGciOi".into();
                    s.id = snapshot_id(&s.provider, &s.account, s.window);
                },
                |e| matches!(e, ValidationError::Account(AccountProblem::LooksLikeToken)),
            ),
            (
                "empty adapter",
                |s| s.source.adapter = String::new(),
                |e| {
                    matches!(
                        e,
                        ValidationError::TextLength {
                            field: "source.adapter",
                            ..
                        }
                    )
                },
            ),
            (
                "note too long",
                |s| s.note = Some("x".repeat(501)),
                |e| matches!(e, ValidationError::NoteTooLong(500)),
            ),
        ];
        for (name, mutate, expect) in cases {
            let mut snapshot = sample();
            mutate(&mut snapshot);
            let err = snapshot.validate().expect_err(name);
            assert!(expect(&err), "{name}: unexpected error {err:?}");
        }
    }

    #[test]
    fn validate_boundaries_are_inclusive() {
        let mut s = sample();
        s.percent_used = Some(0.0);
        assert_eq!(s.validate(), Ok(()));
        s.percent_used = Some(100.0);
        assert_eq!(s.validate(), Ok(()));
        s.account = "a".repeat(64);
        s.id = snapshot_id(&s.provider, &s.account, s.window);
        assert_eq!(s.validate(), Ok(()));
        s.remaining = Some(-12.5); // overspent keys are legal
        assert_eq!(s.validate(), Ok(()));
        s.note = Some("x".repeat(500));
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn account_errors_do_not_echo_the_label() {
        let mut s = sample();
        s.account = "sk-secret-value".into();
        s.id = snapshot_id(&s.provider, &s.account, s.window);
        let message = s.validate().unwrap_err().to_string();
        assert!(!message.contains("secret"));
    }

    #[test]
    fn id_helpers() {
        assert_eq!(
            snapshot_id("codex", "work", WindowKind::Weekly),
            "codex:work:weekly"
        );
        assert_eq!(
            alert_id("codex", "work", WindowKind::Weekly),
            "reset:codex:work:weekly"
        );
        assert_eq!(sample().alert_id(), "reset:claude:default:session_5h");
        let parts = parse_alert_id("reset:claude:default:session_5h").unwrap();
        assert_eq!(parts.provider, "claude");
        assert_eq!(parts.window, WindowKind::Session5h);
        for bad in [
            "claude:default:weekly",
            "reset:claude:default",
            "reset:claude:default:weekly:extra",
            "reset:Claude:default:weekly",
            "reset:claude:me@x.com:weekly",
            "reset:claude:default:hourly",
            "reset:claude::weekly",
        ] {
            assert_eq!(parse_alert_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn alert_intent_validation() {
        let ok = AlertIntent::new(
            "reset:claude:default:weekly",
            datetime!(2026-10-10 12:00 UTC),
        );
        assert_eq!(ok.validate(), Ok(()));
        let bad = AlertIntent::new("nope", datetime!(2026-10-10 12:00 UTC));
        assert!(matches!(bad.validate(), Err(ValidationError::AlertId(_))));
    }

    #[test]
    fn sync_payload_validation() {
        let payload = SyncPayload {
            schema_version: 1,
            device_id: "pc-1".into(),
            generated_at: datetime!(2026-10-10 12:00 UTC),
            snapshots: vec![sample()],
            alerts: vec![AlertIntent::new(
                "reset:claude:default:session_5h",
                datetime!(2026-10-10 14:37 UTC),
            )],
        };
        assert_eq!(payload.validate(), Ok(()));
        let back: SyncPayload =
            serde_json::from_str(&serde_json::to_string(&payload).unwrap()).unwrap();
        assert_eq!(back, payload);

        let mut dup = payload.clone();
        dup.snapshots.push(sample());
        assert!(matches!(
            dup.validate(),
            Err(ValidationError::DuplicateId(_))
        ));

        let mut dup = payload.clone();
        dup.alerts.push(dup.alerts[0].clone());
        assert!(matches!(
            dup.validate(),
            Err(ValidationError::DuplicateId(_))
        ));

        let mut bad = payload.clone();
        bad.schema_version = 2;
        assert!(matches!(
            bad.validate(),
            Err(ValidationError::SchemaVersion(2))
        ));

        let mut bad = payload.clone();
        bad.device_id = String::new();
        assert!(matches!(
            bad.validate(),
            Err(ValidationError::TextLength { .. })
        ));

        let mut bad = payload;
        bad.snapshots = (0..257)
            .map(|i| {
                let mut s = sample();
                s.account = format!("a{i}");
                s.id = snapshot_id(&s.provider, &s.account, s.window);
                s
            })
            .collect();
        assert!(matches!(
            bad.validate(),
            Err(ValidationError::TooMany { .. })
        ));
    }

    #[test]
    fn window_from_duration_table() {
        let cases: &[(f64, Option<WindowKind>)] = &[
            (300.0, Some(WindowKind::Session5h)),
            (299.0, Some(WindowKind::Session5h)),
            (330.0, Some(WindowKind::Session5h)),
            (331.0, None),
            (270.0, Some(WindowKind::Session5h)),
            (269.0, None),
            (1440.0, Some(WindowKind::Daily)),
            (10_080.0, Some(WindowKind::Weekly)),
            (10_079.0, Some(WindowKind::Weekly)),
            (43_200.0, Some(WindowKind::Monthly)),
            (40_320.0, Some(WindowKind::Monthly)), // 28 days
            (44_640.0, Some(WindowKind::Monthly)), // 31 days
            (60.0, None),
            (0.0, None),
            (-300.0, None),
            (f64::NAN, None),
            (f64::INFINITY, None),
        ];
        for &(minutes, expected) in cases {
            assert_eq!(window_from_duration_minutes(minutes), expected, "{minutes}");
        }
        assert_eq!(
            window_from_duration_minutes_tol(350.0, 0.2),
            Some(WindowKind::Session5h)
        );
    }

    fn status_snapshot(status: Status) -> Snapshot {
        let mut s = sample();
        s.status = status;
        s
    }

    #[test]
    fn effective_status_table() {
        let max_age = Duration::from_secs(30 * 60);
        let now = datetime!(2026-10-10 12:10:00 UTC);
        // fresh, reset in the future
        assert_eq!(effective_status(&sample(), now, max_age), Status::Ok);

        // reset in the past (or exactly now) is stale
        let mut past = sample();
        past.resets_at = Some(datetime!(2026-10-10 12:05:00 UTC));
        assert_eq!(effective_status(&past, now, max_age), Status::Stale);
        past.resets_at = Some(now);
        assert_eq!(effective_status(&past, now, max_age), Status::Stale);

        // observed too long ago is stale; exactly at max age is still ok
        let mut old = sample();
        old.observed_at = datetime!(2026-10-10 11:39:59 UTC);
        assert_eq!(effective_status(&old, now, max_age), Status::Stale);
        old.observed_at = datetime!(2026-10-10 11:40:00 UTC);
        assert_eq!(effective_status(&old, now, max_age), Status::Ok);

        // never upgrade degraded or unavailable; stale is upgraded by degraded only downwards
        for stored in [Status::Degraded, Status::Unavailable] {
            let fresh = status_snapshot(stored);
            assert_eq!(effective_status(&fresh, now, max_age), stored);
            let mut stale = status_snapshot(stored);
            stale.resets_at = Some(datetime!(2026-10-10 12:00:00 UTC));
            assert_eq!(effective_status(&stale, now, max_age), stored);
        }
        assert_eq!(
            effective_status(&status_snapshot(Status::Stale), now, max_age),
            Status::Stale
        );

        // clock skew: observed in the future counts as fresh
        let mut future = sample();
        future.observed_at = datetime!(2026-10-10 13:00:00 UTC);
        assert_eq!(effective_status(&future, now, max_age), Status::Ok);

        // no resetsAt is fine
        let mut none = sample();
        none.resets_at = None;
        assert_eq!(effective_status(&none, now, max_age), Status::Ok);

        // zero max age falls back to the default; MAX disables the age check
        assert_eq!(effective_status(&old, now, Duration::ZERO), Status::Ok);
        let mut ancient = sample();
        ancient.observed_at = datetime!(2020-01-01 0:00 UTC);
        assert_eq!(effective_status(&ancient, now, Duration::MAX), Status::Ok);
        assert_eq!(effective_status(&ancient, now, max_age), Status::Stale);
    }

    #[test]
    fn seconds_until_reset_rounds_up_and_never_goes_negative() {
        let mut s = sample();
        let now = datetime!(2026-10-10 14:36:00 UTC);
        assert_eq!(seconds_until_reset(&s, now), Some(60));
        assert_eq!(
            seconds_until_reset(&s, now + time::Duration::milliseconds(500)),
            Some(60)
        );
        assert_eq!(
            seconds_until_reset(&s, now - time::Duration::milliseconds(500)),
            Some(61)
        );
        assert_eq!(
            seconds_until_reset(&s, datetime!(2026-10-10 14:37:00 UTC)),
            Some(0)
        );
        assert_eq!(
            seconds_until_reset(&s, datetime!(2026-10-11 0:00 UTC)),
            Some(0)
        );
        s.resets_at = None;
        assert_eq!(seconds_until_reset(&s, now), None);
    }
}
