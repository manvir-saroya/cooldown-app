//! Antigravity CLI (Google's successor to Gemini CLI), through the inbox file the
//! `cooldown-statusline antigravity` hook writes.
//!
//! Antigravity CLI pipes a `quota` object to its status line command: one entry per model or
//! bucket id, each with `remaining_fraction` (0..1) and `reset_time` (documented at
//! antigravity.google/docs/cli/statusline). The hook saves those entries to
//! `<data_local_dir>/cooldown/inbox/antigravity-statusline.json`; this adapter reads that file. It
//! never talks to Google and never reads a login: it only reads a file Cooldown itself wrote.
//!
//! Rules:
//!
//! - The documented bucket is weekly (`gemini-weekly`). Buckets whose id contains `week` become the
//!   `weekly` window; when there are several (per model), the one with the least left wins, because
//!   that is the one that stops you. Other buckets are not guessed at.
//! - `percentUsed = (1 - remaining_fraction) * 100`, rounded to 4 decimals. Source kind `official`.
//! - `observedAt` is when the hook wrote the file. A reset time in the past makes the window
//!   `stale`. A missing inbox is `unavailable` with a hint to install the hook.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use cooldown_core::adapter::{Adapter, PollContext};
use cooldown_core::model::{Snapshot, SourceKind, WindowKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::OffsetDateTime;
use tokio::io::AsyncReadExt;

use crate::mapping::{
    carry_or_unavailable, num, obj, parse_rfc3339_secs, percent_window, MapContext, Meta,
};
use crate::{refuse_credential_file, validate_account, ConfigError, ADAPTER_VERSION};

/// `source.adapter` and fixture directory name.
pub const ADAPTER_ID: &str = "antigravity-statusline";

/// Name of the inbox file inside the inbox directory (same directory as Claude's).
pub const INBOX_FILE_NAME: &str = "antigravity-statusline.json";

const INBOX_VERSION: u64 = 1;
const MAX_INBOX_BYTES: u64 = 256 * 1024;

const META: Meta = Meta {
    id: ADAPTER_ID,
    provider: "antigravity",
    kind: SourceKind::Official,
};

const NOTE_NOT_REPORTED: &str =
    "Antigravity CLI has not reported a weekly quota (it sends one once a session has started)";
const NOTE_MISSING: &str = "Weekly quota missing from the latest Antigravity CLI update";
const NOTE_NO_INBOX: &str =
    "No Antigravity CLI data yet. Run Antigravity CLI with the Cooldown hook installed";

/// `(1 - fraction) * 100`, rounded to 4 decimals so Rust and TypeScript agree to the digit.
fn used_percent(remaining_fraction: f64) -> f64 {
    ((1.0 - remaining_fraction.clamp(0.0, 1.0)) * 100.0 * 10_000.0).round() / 10_000.0
}

/// Map the JSON Antigravity CLI writes to a status line command's stdin (the fixture input) to the
/// `weekly` window. Pure: no clock, no disk. `ctx.observed_at` defaults to `ctx.now`.
pub fn map_input(input: &Value, ctx: &MapContext) -> Vec<Snapshot> {
    let observed_at = ctx.observed_at.unwrap_or(ctx.now);
    let tightest = obj(input.get("quota"))
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(id, _)| id.to_ascii_lowercase().contains("week"))
        .filter_map(|(_, bucket)| {
            let fraction = num(obj(Some(bucket))?.get("remaining_fraction"))?;
            Some((fraction, bucket))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0));
    let snapshot = match tightest {
        Some((fraction, bucket)) => {
            let resets = bucket
                .get("reset_time")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339_secs)
                .map(|t| t.unix_timestamp() as f64);
            percent_window(
                &META,
                ctx,
                WindowKind::Weekly,
                used_percent(fraction),
                resets,
                observed_at,
            )
        }
        None => carry_or_unavailable(
            &META,
            ctx,
            WindowKind::Weekly,
            NOTE_MISSING,
            Some(NOTE_NOT_REPORTED),
        ),
    };
    vec![snapshot]
}

/// A parsed inbox: the buckets in the shape of Antigravity's status line input, and when they were
/// observed.
#[derive(Debug, Clone, PartialEq)]
pub struct InboxReading {
    /// `{"quota": {"<id>": {"remaining_fraction", "reset_time"}}}`, ready for [`map_input`].
    pub statusline: Value,
    /// The inbox's `observedAt` (whole seconds, UTC).
    pub observed_at: OffsetDateTime,
}

/// Parse the inbox file the hook writes. `Err` carries the reason for a `degraded` snapshot.
pub fn parse_inbox(bytes: &[u8]) -> Result<InboxReading, &'static str> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "Antigravity inbox file is not valid JSON")?;
    if value.get("version").and_then(Value::as_u64) != Some(INBOX_VERSION) {
        return Err("Antigravity inbox file has a version this Cooldown does not understand");
    }
    let observed_at = value
        .get("observedAt")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_secs)
        .ok_or("Antigravity inbox file has no valid observedAt")?;
    let mut quota = serde_json::Map::new();
    if let Some(buckets) = value.get("buckets").and_then(Value::as_object) {
        for (id, bucket) in buckets {
            let mut out = serde_json::Map::new();
            if let Some(f) = num(bucket.get("remainingFraction")) {
                out.insert("remaining_fraction".to_owned(), json!(f));
            }
            if let Some(t) = bucket.get("resetTime").and_then(Value::as_str) {
                out.insert("reset_time".to_owned(), json!(t));
            }
            quota.insert(id.clone(), Value::Object(out));
        }
    }
    Ok(InboxReading {
        statusline: json!({ "quota": Value::Object(quota) }),
        observed_at,
    })
}

/// The inbox file the hook writes on this machine (beside Claude's, under `COOLDOWN_INBOX_DIR`
/// when that is set).
pub fn default_inbox_path() -> PathBuf {
    crate::claude::default_inbox_path().with_file_name(INBOX_FILE_NAME)
}

/// Settings of [`AntigravityInboxAdapter`]. Nothing secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AntigravityInboxConfig {
    /// The inbox file. Defaults to [`default_inbox_path`].
    pub inbox_path: PathBuf,
    /// Local account label (becomes part of the snapshot ids).
    pub account: String,
    /// Seconds between polls.
    pub poll_interval_secs: u64,
}

impl Default for AntigravityInboxConfig {
    fn default() -> Self {
        AntigravityInboxConfig {
            inbox_path: default_inbox_path(),
            account: "default".to_owned(),
            poll_interval_secs: 30,
        }
    }
}

/// Reads Antigravity CLI's quota from the hook's inbox file.
#[derive(Debug, Clone)]
pub struct AntigravityInboxAdapter {
    config: AntigravityInboxConfig,
}

impl AntigravityInboxAdapter {
    /// Create the adapter.
    pub fn new(config: AntigravityInboxConfig) -> Result<Self, ConfigError> {
        validate_account(META.provider, &config.account)?;
        refuse_credential_file(&config.inbox_path)?;
        Ok(AntigravityInboxAdapter { config })
    }

    fn failure(&self, ctx: &MapContext, reason: &str) -> Vec<Snapshot> {
        vec![carry_or_unavailable(
            &META,
            ctx,
            WindowKind::Weekly,
            reason,
            None,
        )]
    }
}

#[async_trait]
impl Adapter for AntigravityInboxAdapter {
    fn id(&self) -> &'static str {
        ADAPTER_ID
    }

    fn version(&self) -> &'static str {
        ADAPTER_VERSION
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.config.poll_interval_secs.max(1))
    }

    /// A local file: keep checking at the normal pace (see `ClaudeInboxAdapter`).
    fn backs_off(&self) -> bool {
        false
    }

    async fn poll(&self, ctx: &PollContext) -> Vec<Snapshot> {
        let map_ctx = MapContext::from_poll(ctx, &self.config.account, ADAPTER_VERSION);
        let file = match tokio::fs::File::open(&self.config.inbox_path).await {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return self.failure(&map_ctx, NOTE_NO_INBOX)
            }
            Err(_) => return self.failure(&map_ctx, "Cannot read the Antigravity inbox file"),
        };
        let mut bytes = Vec::new();
        if file
            .take(MAX_INBOX_BYTES + 1)
            .read_to_end(&mut bytes)
            .await
            .is_err()
            || bytes.len() as u64 > MAX_INBOX_BYTES
        {
            return self.failure(&map_ctx, "Cannot read the Antigravity inbox file");
        }
        match parse_inbox(&bytes) {
            Ok(reading) => {
                let observed_at = reading.observed_at.min(map_ctx.now);
                map_input(&reading.statusline, &map_ctx.with_observed_at(observed_at))
            }
            Err(reason) => self.failure(&map_ctx, reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cooldown_core::model::Status;
    use time::macros::datetime;

    fn ctx() -> MapContext {
        MapContext::new(datetime!(2026-10-10 12:00:00 UTC), "default", "0.1.0")
    }

    #[test]
    fn the_tightest_weekly_bucket_wins() {
        let input = json!({"quota": {
            "gemini-weekly": {"remaining_fraction": 0.9378, "reset_time": "2026-10-13T07:50:32Z"},
            "gemini-pro-weekly": {"remaining_fraction": 0.4, "reset_time": "2026-10-14T00:00:00Z"},
            "something-else": {"remaining_fraction": 0.01}
        }});
        let out = map_input(&input, &ctx());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].window, WindowKind::Weekly);
        assert_eq!(out[0].percent_used, Some(60.0));
        assert_eq!(out[0].resets_at, Some(datetime!(2026-10-14 0:00 UTC)));
        assert_eq!(out[0].status, Status::Ok);
        assert_eq!(used_percent(0.9378), 6.22);
    }

    #[test]
    fn no_weekly_bucket_is_unavailable_not_zero() {
        let out = map_input(
            &json!({"quota": {"x": {"remaining_fraction": 0.5}}}),
            &ctx(),
        );
        assert_eq!(out[0].status, Status::Unavailable);
        assert_eq!(out[0].percent_used, None);
    }

    #[test]
    fn the_inbox_becomes_the_statusline_shape() {
        let bytes = br#"{"version":1,"observedAt":"2026-10-10T11:55:00Z","buckets":{"gemini-weekly":{"remainingFraction":0.5,"resetTime":"2026-10-13T07:50:32Z"}}}"#;
        let reading = parse_inbox(bytes).unwrap();
        assert_eq!(reading.observed_at, datetime!(2026-10-10 11:55:00 UTC));
        assert_eq!(
            reading.statusline,
            json!({"quota": {"gemini-weekly": {"remaining_fraction": 0.5, "reset_time": "2026-10-13T07:50:32Z"}}})
        );
        assert!(parse_inbox(b"{}").is_err());
        assert!(parse_inbox(b"nope").is_err());
    }
}
