//! Claude Code, through the inbox file the `cooldown-statusline` hook writes.
//!
//! Claude Code pipes `rate_limits.five_hour` and `rate_limits.seven_day` to the user's configured
//! statusline command. The `cooldown-statusline` hook saves them to
//! `<data_local_dir>/cooldown/inbox/claude-statusline.json` (see that crate's README for the file
//! format), and this adapter reads that file. It never talks to Claude, never reads a credential
//! file and never calls an undocumented endpoint: it only reads a file Cooldown itself wrote.
//!
//! Rules (from the fixture contract):
//!
//! - `five_hour` becomes `session_5h`, `seven_day` becomes `weekly`, source kind `official`.
//! - The inbox is only as fresh as the last Claude Code update. `observedAt` is the time the hook
//!   wrote the file, not the time of the poll, so the UI shows the true age.
//! - A window whose `resetsAt` is in the past is `stale`.
//! - Claude Code drops a window once its reset time passes, so a window that is missing after a
//!   known reset means "the reset happened": `ok`, 0 percent, note `window reset`.
//! - A missing inbox file is `unavailable` with a hint to install the hook; an unreadable or
//!   malformed one is `degraded` and keeps the last good value.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use cooldown_core::adapter::{Adapter, PollContext};
use cooldown_core::model::{snapshot_id, Snapshot, SourceKind, Status, WindowKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::OffsetDateTime;
use tokio::io::AsyncReadExt;

use crate::mapping::{
    carry_or_unavailable, make, num, obj, parse_rfc3339_secs, percent_window, MapContext, Meta,
    NOTE_RESET,
};
use crate::{refuse_credential_file, validate_account, ConfigError, ADAPTER_VERSION};

/// `source.adapter` and fixture directory name.
pub const ADAPTER_ID: &str = "claude-statusline";

/// Name of the inbox file inside the inbox directory.
pub const INBOX_FILE_NAME: &str = "claude-statusline.json";

/// Environment variable that replaces the whole inbox directory (same one the hook honors).
pub const ENV_INBOX_DIR: &str = "COOLDOWN_INBOX_DIR";

/// The inbox schema version this adapter understands.
const INBOX_VERSION: u64 = 1;

/// The hook caps what it writes at 1 MiB of input; a real inbox is a few hundred bytes. Anything
/// bigger is not ours.
const MAX_INBOX_BYTES: u64 = 1024 * 1024;

const META: Meta = Meta {
    id: ADAPTER_ID,
    provider: "claude",
    kind: SourceKind::Official,
};

const NOTE_NOT_REPORTED: &str = "Claude Code has not reported this window (it only does for Pro/Max subscribers, after the first response of a session)";
const NOTE_MISSING: &str = "Window missing from the latest Claude Code update";
const NOTE_NO_INBOX: &str =
    "No Claude Code data yet. Run Claude Code with the Cooldown hook installed";

const WINDOWS: [(WindowKind, &str); 2] = [
    (WindowKind::Session5h, "five_hour"),
    (WindowKind::Weekly, "seven_day"),
];

/// Map the JSON Claude Code writes to a statusline command's stdin (the fixture input) to the two
/// windows. Pure: no clock, no disk.
///
/// `ctx.observed_at` is when the data was produced (the inbox's `observedAt`); it defaults to
/// `ctx.now`.
pub fn map_input(input: &Value, ctx: &MapContext) -> Vec<Snapshot> {
    let rate_limits = obj(input.get("rate_limits"));
    let observed_at = ctx.observed_at.unwrap_or(ctx.now);
    WINDOWS
        .iter()
        .map(|&(window, key)| {
            let raw = obj(rate_limits.and_then(|r| r.get(key)));
            let percent = raw.and_then(|r| num(r.get("used_percentage")));
            if let (Some(raw), Some(percent)) = (raw, percent) {
                return percent_window(
                    &META,
                    ctx,
                    window,
                    percent,
                    num(raw.get("resets_at")),
                    observed_at,
                );
            }
            if window_has_reset(ctx, window) {
                let mut snapshot = make(&META, ctx, window);
                snapshot.percent_used = Some(0.0);
                snapshot.note = Some(NOTE_RESET.to_owned());
                return snapshot;
            }
            carry_or_unavailable(&META, ctx, window, NOTE_MISSING, Some(NOTE_NOT_REPORTED))
        })
        .collect()
}

/// Claude Code drops a window once its `resets_at` passes: a missing window after a known reset
/// means the reset happened. A previous `window reset` marker that is still missing repeats.
fn window_has_reset(ctx: &MapContext, window: WindowKind) -> bool {
    let id = snapshot_id(META.provider, &ctx.account, window);
    let Some(previous) = ctx.previous.iter().find(|s| s.id == id) else {
        return false;
    };
    match previous.resets_at {
        Some(resets_at) => resets_at <= ctx.now,
        None => previous.status == Status::Ok && previous.note.as_deref() == Some(NOTE_RESET),
    }
}

/// Why an inbox file could not be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxProblem {
    /// Not JSON, or not a JSON object.
    Malformed,
    /// `version` is missing or not one this adapter understands.
    UnsupportedVersion,
    /// `observedAt` is missing or not an RFC 3339 timestamp.
    NoObservedAt,
}

impl InboxProblem {
    fn reason(self) -> &'static str {
        match self {
            InboxProblem::Malformed => "Claude Code inbox file is not valid JSON",
            InboxProblem::UnsupportedVersion => {
                "Claude Code inbox file has a version this Cooldown does not understand"
            }
            InboxProblem::NoObservedAt => "Claude Code inbox file has no valid observedAt",
        }
    }
}

/// A parsed inbox: the windows in the shape of Claude Code's statusline input, and when they were
/// observed.
#[derive(Debug, Clone, PartialEq)]
pub struct InboxReading {
    /// `{"rate_limits": {"five_hour": {...}, "seven_day": {...}}}`, ready for [`map_input`].
    pub statusline: Value,
    /// The inbox's `observedAt` (whole seconds, UTC).
    pub observed_at: OffsetDateTime,
}

/// Parse the inbox file (schema in the `cooldown-statusline` README) into an [`InboxReading`].
pub fn parse_inbox(bytes: &[u8]) -> Result<InboxReading, InboxProblem> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let value: Value = serde_json::from_slice(bytes).map_err(|_| InboxProblem::Malformed)?;
    if !value.is_object() {
        return Err(InboxProblem::Malformed);
    }
    if value.get("version").and_then(Value::as_u64) != Some(INBOX_VERSION) {
        return Err(InboxProblem::UnsupportedVersion);
    }
    let observed_at = value
        .get("observedAt")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_secs)
        .ok_or(InboxProblem::NoObservedAt)?;

    let mut rate_limits = serde_json::Map::new();
    for (key, source_key) in [("five_hour", "fiveHour"), ("seven_day", "sevenDay")] {
        let Some(window) = obj(value.get(source_key)) else {
            continue;
        };
        let mut out = serde_json::Map::new();
        if let Some(percent) = num(window.get("usedPercentage")) {
            out.insert("used_percentage".to_owned(), json!(percent));
        }
        if let Some(resets_at) = window
            .get("resetsAt")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_secs)
        {
            out.insert("resets_at".to_owned(), json!(resets_at.unix_timestamp()));
        }
        rate_limits.insert(key.to_owned(), Value::Object(out));
    }
    Ok(InboxReading {
        statusline: json!({ "rate_limits": Value::Object(rate_limits) }),
        observed_at,
    })
}

/// `override_dir` if set and non-empty, else `<data_local_dir>/cooldown/inbox`; plus the file name.
///
/// Pure (the environment is passed in) so it is tested on every OS. Mirrors the hook's own rule.
pub fn resolve_inbox_path(
    override_dir: Option<std::ffi::OsString>,
    data_local_dir: Option<PathBuf>,
) -> PathBuf {
    let dir = override_dir
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| data_local_dir.map(|d| d.join("cooldown").join("inbox")))
        .unwrap_or_else(|| PathBuf::from("cooldown-inbox"));
    dir.join(INBOX_FILE_NAME)
}

/// The inbox file the hook writes on this machine:
/// `dirs::data_local_dir()/cooldown/inbox/claude-statusline.json` (on Windows
/// `%LOCALAPPDATA%\cooldown\inbox\claude-statusline.json`), or under `COOLDOWN_INBOX_DIR`.
pub fn default_inbox_path() -> PathBuf {
    resolve_inbox_path(std::env::var_os(ENV_INBOX_DIR), dirs::data_local_dir())
}

/// Settings of [`ClaudeInboxAdapter`]. Nothing secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClaudeInboxConfig {
    /// The inbox file. Defaults to [`default_inbox_path`].
    pub inbox_path: PathBuf,
    /// Local account label (becomes part of the snapshot ids).
    pub account: String,
    /// Seconds between polls. Reading a small local file is cheap.
    pub poll_interval_secs: u64,
}

impl Default for ClaudeInboxConfig {
    fn default() -> Self {
        ClaudeInboxConfig {
            inbox_path: default_inbox_path(),
            account: "default".to_owned(),
            poll_interval_secs: 30,
        }
    }
}

/// Reads Claude Code's rate limits from the hook's inbox file.
#[derive(Debug, Clone)]
pub struct ClaudeInboxAdapter {
    config: ClaudeInboxConfig,
}

impl ClaudeInboxAdapter {
    /// Create the adapter. Fails only if the account label is not a legal snapshot account.
    pub fn new(config: ClaudeInboxConfig) -> Result<Self, ConfigError> {
        validate_account(META.provider, &config.account)?;
        refuse_credential_file(&config.inbox_path)?;
        Ok(ClaudeInboxAdapter { config })
    }

    /// The settings in use.
    pub fn config(&self) -> &ClaudeInboxConfig {
        &self.config
    }

    /// The `degraded`/`unavailable`/carry-forward snapshots for a poll that produced no reading.
    fn failure(&self, ctx: &MapContext, reason: &str) -> Vec<Snapshot> {
        WINDOWS
            .iter()
            .map(|&(window, _)| carry_or_unavailable(&META, ctx, window, reason, None))
            .collect()
    }
}

#[derive(Debug)]
enum ReadFailure {
    Missing,
    TooLarge,
    PermissionDenied,
    Other,
}

async fn read_inbox_bytes(path: &Path) -> Result<Vec<u8>, ReadFailure> {
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(ReadFailure::Missing),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(ReadFailure::PermissionDenied)
        }
        Err(_) => return Err(ReadFailure::Other),
    };
    let mut bytes = Vec::new();
    file.take(MAX_INBOX_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| ReadFailure::Other)?;
    if bytes.len() as u64 > MAX_INBOX_BYTES {
        return Err(ReadFailure::TooLarge);
    }
    Ok(bytes)
}

#[async_trait]
impl Adapter for ClaudeInboxAdapter {
    fn id(&self) -> &'static str {
        ADAPTER_ID
    }

    fn version(&self) -> &'static str {
        ADAPTER_VERSION
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.config.poll_interval_secs.max(1))
    }

    /// A missing inbox only means Claude Code has not run with the hook yet. Reading a small local
    /// file is cheap, so keep checking every 30 s instead of backing off for up to half an hour.
    fn backs_off(&self) -> bool {
        false
    }

    async fn poll(&self, ctx: &PollContext) -> Vec<Snapshot> {
        let map_ctx = MapContext::from_poll(ctx, &self.config.account, ADAPTER_VERSION);
        let bytes = match read_inbox_bytes(&self.config.inbox_path).await {
            Ok(bytes) => bytes,
            Err(ReadFailure::Missing) => return self.failure(&map_ctx, NOTE_NO_INBOX),
            Err(ReadFailure::TooLarge) => {
                return self.failure(&map_ctx, "Claude Code inbox file is too large")
            }
            Err(ReadFailure::PermissionDenied) => {
                return self.failure(
                    &map_ctx,
                    "Cannot read the Claude Code inbox file (permission denied)",
                )
            }
            Err(ReadFailure::Other) => {
                return self.failure(&map_ctx, "Cannot read the Claude Code inbox file")
            }
        };
        match parse_inbox(&bytes) {
            Ok(reading) => {
                // A timestamp from the future (clock skew) counts as "just now".
                let observed_at = reading.observed_at.min(map_ctx.now);
                let map_ctx = map_ctx.with_observed_at(observed_at);
                map_input(&reading.statusline, &map_ctx)
            }
            Err(problem) => self.failure(&map_ctx, problem.reason()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cooldown_core::model::Unit;
    use time::macros::datetime;

    fn ctx() -> MapContext {
        MapContext::new(datetime!(2026-10-10 12:00:00 UTC), "default", "0.1.0")
    }

    fn inbox(five: Value, seven: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "version": 1,
            "observedAt": "2026-10-10T11:55:00Z",
            "fiveHour": five,
            "sevenDay": seven,
            "spendLimit": null,
            "claudeCodeVersion": "2.1.90",
        }))
        .unwrap()
    }

    #[test]
    fn inbox_windows_become_statusline_shape() {
        let bytes = inbox(
            json!({"usedPercentage": 23.5, "resetsAt": "2026-10-10T16:00:00Z"}),
            Value::Null,
        );
        let reading = parse_inbox(&bytes).unwrap();
        assert_eq!(reading.observed_at, datetime!(2026-10-10 11:55:00 UTC));
        assert_eq!(
            reading.statusline,
            json!({"rate_limits": {"five_hour": {"used_percentage": 23.5, "resets_at": 1_791_648_000}}})
        );
        let snapshots = map_input(&reading.statusline, &ctx());
        assert_eq!(snapshots[0].percent_used, Some(23.5));
        assert_eq!(snapshots[0].unit, Unit::Percent);
        assert_eq!(
            snapshots[0].resets_at,
            Some(datetime!(2026-10-10 16:00:00 UTC))
        );
        assert_eq!(snapshots[1].status, Status::Unavailable);
    }

    #[test]
    fn bad_inboxes_are_classified() {
        assert_eq!(parse_inbox(b"").unwrap_err(), InboxProblem::Malformed);
        assert_eq!(parse_inbox(b"[1]").unwrap_err(), InboxProblem::Malformed);
        assert_eq!(
            parse_inbox(br#"{"version":2,"observedAt":"2026-10-10T11:55:00Z"}"#).unwrap_err(),
            InboxProblem::UnsupportedVersion
        );
        assert_eq!(
            parse_inbox(br#"{"observedAt":"2026-10-10T11:55:00Z"}"#).unwrap_err(),
            InboxProblem::UnsupportedVersion
        );
        assert_eq!(
            parse_inbox(br#"{"version":1,"observedAt":"soon"}"#).unwrap_err(),
            InboxProblem::NoObservedAt
        );
        // A byte order mark is tolerated.
        let mut with_bom = b"\xEF\xBB\xBF".to_vec();
        with_bom.extend(inbox(Value::Null, Value::Null));
        assert!(parse_inbox(&with_bom).is_ok());
    }

    #[test]
    fn a_reset_marker_repeats_until_the_window_returns() {
        let mut c = ctx();
        let mut marker = make(&META, &c, WindowKind::Session5h);
        marker.percent_used = Some(0.0);
        marker.note = Some(NOTE_RESET.to_owned());
        c.previous = vec![marker];
        let out = map_input(&json!({}), &c);
        assert_eq!(out[0].note.as_deref(), Some(NOTE_RESET));
        assert_eq!(out[0].status, Status::Ok);
        assert_eq!(out[0].percent_used, Some(0.0));
        // The other window had nothing before: unavailable.
        assert_eq!(out[1].status, Status::Unavailable);
    }

    #[test]
    fn inbox_path_follows_the_hook_rules() {
        assert_eq!(
            resolve_inbox_path(None, Some(PathBuf::from("/data/local"))),
            PathBuf::from("/data/local/cooldown/inbox/claude-statusline.json")
        );
        assert_eq!(
            resolve_inbox_path(Some("/tmp/x".into()), Some(PathBuf::from("/data/local"))),
            PathBuf::from("/tmp/x/claude-statusline.json")
        );
        assert_eq!(
            resolve_inbox_path(Some("".into()), Some(PathBuf::from("/d"))),
            PathBuf::from("/d/cooldown/inbox/claude-statusline.json")
        );
    }

    #[test]
    fn adapter_identity() {
        let adapter = ClaudeInboxAdapter::new(ClaudeInboxConfig::default()).unwrap();
        assert_eq!(adapter.id(), "claude-statusline");
        assert_eq!(adapter.version(), ADAPTER_VERSION);
        assert_eq!(adapter.poll_interval(), Duration::from_secs(30));
        assert!(ClaudeInboxAdapter::new(ClaudeInboxConfig {
            account: "me@example.com".into(),
            ..ClaudeInboxConfig::default()
        })
        .is_err());
        assert!(matches!(
            ClaudeInboxAdapter::new(ClaudeInboxConfig {
                inbox_path: "/home/me/.claude/.credentials.json".into(),
                ..ClaudeInboxConfig::default()
            }),
            Err(ConfigError::ForbiddenPath(_))
        ));
    }
}
