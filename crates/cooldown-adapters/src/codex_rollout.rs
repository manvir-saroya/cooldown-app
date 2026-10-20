//! Codex, from its own session logs (the fallback when the app-server is not available).
//!
//! Codex appends every session to `~/.codex/sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl`. After a
//! model response it writes a `token_count` event that carries the account's current
//! `rate_limits` (`used_percent`, `window_minutes`, `resets_at`). This adapter reads the tail of
//! the newest such log and maps the last event that carries them.
//!
//! Strictly read-only and narrow:
//!
//! - Only regular files named `rollout-*.jsonl` inside numeric `YYYY/MM/DD` directories under the
//!   sessions directory are ever opened. Symbolic links are skipped. `auth.json` and every other
//!   file under `~/.codex` are never touched.
//! - At most [`CodexRolloutConfig::tail_bytes`] are read from the end of a file (default 512 KiB),
//!   and a line cut off by that limit, or by a writer that is mid-append, is ignored.
//! - Source kind is `scraped`: the log format is Codex's own and not a stable interface. The event's
//!   `timestamp` becomes `observedAt`, so the UI shows how old the reading really is.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use cooldown_core::adapter::{Adapter, PollContext};
use cooldown_core::model::{Snapshot, SourceKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codex_map::{self, SlotKeys};
use crate::mapping::{num, obj, parse_rfc3339_secs, MapContext, Meta};
use crate::{validate_account, ConfigError, ADAPTER_VERSION};

/// `source.adapter` and fixture directory name.
pub const ADAPTER_ID: &str = "codex-rollout-log";

const META: Meta = Meta {
    id: ADAPTER_ID,
    provider: "codex",
    kind: SourceKind::Scraped,
};

const NOTE_NONE: &str = "No Codex rate-limit data in the latest rollout event";
const NOTE_NO_LOGS: &str = "No Codex session logs found";

const SLOT_KEYS: SlotKeys = SlotKeys {
    used_percent: "used_percent",
    window_minutes: "window_minutes",
    resets_at: "resets_at",
};

/// Day directories examined (newest first) when looking for recent logs.
const MAX_DAY_DIRS: usize = 3;
/// Files collected from those directories before sorting by modification time.
const MAX_CANDIDATES: usize = 64;

/// Map one rollout line (the fixture input: a `token_count` event) to the `session_5h` and
/// `weekly` snapshots. Pure.
///
/// - `rate_limits` counts only when `limit_id` is absent, `null` or `codex`.
/// - Windows are assigned by duration, never by slot.
/// - `observedAt` is the event's own `timestamp`; a reset time already in the past is `stale`.
pub fn map_input(input: &Value, ctx: &MapContext) -> Vec<Snapshot> {
    let fail = || codex_map::fail_all(&META, ctx, NOTE_NONE);

    let Some(payload) = obj(input.get("payload")) else {
        return fail();
    };
    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return fail();
    }
    let rate_limits = obj(payload.get("rate_limits"));
    let observed_at = input
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_secs);
    let (Some(rate_limits), Some(observed_at)) = (rate_limits, observed_at) else {
        return fail();
    };
    if !is_main_limit(rate_limits) {
        return fail();
    }
    let mapped = codex_map::windows_by_duration(
        &[rate_limits.get("primary"), rate_limits.get("secondary")],
        &SLOT_KEYS,
    );
    codex_map::snapshots(&META, ctx, mapped, observed_at, NOTE_NONE)
}

/// `limit_id` absent, `null` or `codex`: the main subscription limit, not a per-model one.
fn is_main_limit(rate_limits: &Value) -> bool {
    match rate_limits.get("limit_id") {
        None | Some(Value::Null) => true,
        Some(Value::String(id)) => id == "codex",
        Some(_) => false,
    }
}

/// True for a `token_count` event line whose `rate_limits` is the main limit and has at least one
/// window with a numeric percentage. Used to pick the event to map.
fn carries_main_rate_limits(event: &Value) -> bool {
    let Some(payload) = obj(event.get("payload")) else {
        return false;
    };
    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return false;
    }
    let Some(rate_limits) = obj(payload.get("rate_limits")) else {
        return false;
    };
    is_main_limit(rate_limits)
        && ["primary", "secondary"].iter().any(|slot| {
            obj(rate_limits.get(slot)).is_some_and(|w| num(w.get("used_percent")).is_some())
        })
}

/// The last event in `tail` that carries the main rate limits, newest first.
///
/// `tail` is the last bytes of a log. When `starts_mid_file` is true the first line is a fragment
/// and is dropped. Lines that are not valid JSON (a cut-off first line, a record the writer has
/// not finished) are ignored.
pub fn last_rate_limit_event(tail: &[u8], starts_mid_file: bool) -> Option<Value> {
    let tail = if starts_mid_file {
        let newline = tail.iter().position(|&b| b == b'\n')?;
        &tail[newline + 1..]
    } else {
        tail
    };
    tail.split(|&b| b == b'\n')
        .rev()
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .find(carries_main_rate_limits)
}

/// Read at most `cap` bytes from the end of the file at `path`. Returns the bytes and whether they
/// start in the middle of the file.
pub fn read_tail(path: &Path, cap: u64) -> std::io::Result<(Vec<u8>, bool)> {
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(cap);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(cap).read_to_end(&mut bytes)?;
    Ok((bytes, start > 0))
}

fn is_digits(name: &str, len: usize) -> bool {
    name.len() == len && name.bytes().all(|b| b.is_ascii_digit())
}

/// Sub-directories of `dir` whose name is `len` digits, newest (largest) first. Symbolic links are
/// not followed.
fn numeric_subdirs(dir: &Path, len: usize) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            is_digits(&name, len).then(|| (name, entry.path()))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, path)| path).collect()
}

fn is_rollout_name(name: &str) -> bool {
    name.starts_with("rollout-") && name.ends_with(".jsonl")
}

/// The most recently written rollout logs under `sessions_dir`, newest first (by modification
/// time), at most `limit`.
///
/// Walks the `YYYY/MM/DD` layout newest day first, looks at the last few day directories only,
/// and never follows symbolic links.
pub fn find_recent_rollouts(sessions_dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut candidates: Vec<(SystemTime, PathBuf)> = Vec::new();
    let mut days_with_logs = 0;
    'walk: for year in numeric_subdirs(sessions_dir, 4) {
        for month in numeric_subdirs(&year, 2) {
            for day in numeric_subdirs(&month, 2) {
                let Ok(entries) = fs::read_dir(&day) else {
                    continue;
                };
                let before = candidates.len();
                for entry in entries.filter_map(Result::ok) {
                    let Ok(file_type) = entry.file_type() else {
                        continue;
                    };
                    let Ok(name) = entry.file_name().into_string() else {
                        continue;
                    };
                    if !file_type.is_file() || !is_rollout_name(&name) {
                        continue;
                    }
                    let modified = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    candidates.push((modified, entry.path()));
                }
                if candidates.len() > before {
                    days_with_logs += 1;
                }
                if days_with_logs >= MAX_DAY_DIRS || candidates.len() >= MAX_CANDIDATES {
                    break 'walk;
                }
            }
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    candidates
        .into_iter()
        .take(limit)
        .map(|(_, path)| path)
        .collect()
}

/// The sessions directory Codex uses: `$CODEX_HOME/sessions`, else `~/.codex/sessions`.
///
/// Pure (the environment is passed in).
pub fn resolve_sessions_dir(
    codex_home: Option<std::ffi::OsString>,
    home: Option<PathBuf>,
) -> PathBuf {
    codex_home
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
        .join("sessions")
}

/// The sessions directory for this machine.
pub fn default_sessions_dir() -> PathBuf {
    resolve_sessions_dir(std::env::var_os("CODEX_HOME"), dirs::home_dir())
}

/// Settings of [`CodexRolloutAdapter`]. Nothing secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CodexRolloutConfig {
    /// Local account label (becomes part of the snapshot ids).
    pub account: String,
    /// The Codex sessions directory (the one that contains `YYYY/MM/DD`).
    pub sessions_dir: PathBuf,
    /// Most bytes read from the end of one log.
    pub tail_bytes: u64,
    /// Most logs examined per poll (newest first) before giving up.
    pub max_files: usize,
    /// Seconds between polls.
    pub poll_interval_secs: u64,
}

impl Default for CodexRolloutConfig {
    fn default() -> Self {
        CodexRolloutConfig {
            account: "default".to_owned(),
            sessions_dir: default_sessions_dir(),
            tail_bytes: 512 * 1024,
            max_files: 5,
            poll_interval_secs: 60,
        }
    }
}

/// Reads Codex's rate limits from the tail of its newest session log.
#[derive(Debug, Clone)]
pub struct CodexRolloutAdapter {
    config: CodexRolloutConfig,
}

impl CodexRolloutAdapter {
    /// Create the adapter. Fails only if the account label is not a legal snapshot account.
    pub fn new(config: CodexRolloutConfig) -> Result<Self, ConfigError> {
        validate_account(META.provider, &config.account)?;
        Ok(CodexRolloutAdapter { config })
    }

    /// The settings in use.
    pub fn config(&self) -> &CodexRolloutConfig {
        &self.config
    }
}

/// What the blocking part of a poll found.
enum Found {
    Event(Value),
    NoLogs,
    NoEvent,
}

fn find_event(config: &CodexRolloutConfig) -> Found {
    let files = find_recent_rollouts(&config.sessions_dir, config.max_files.max(1));
    if files.is_empty() {
        return Found::NoLogs;
    }
    for path in files {
        let Ok((tail, starts_mid_file)) = read_tail(&path, config.tail_bytes.max(1024)) else {
            continue;
        };
        if let Some(event) = last_rate_limit_event(&tail, starts_mid_file) {
            return Found::Event(event);
        }
    }
    Found::NoEvent
}

#[async_trait]
impl Adapter for CodexRolloutAdapter {
    fn id(&self) -> &'static str {
        ADAPTER_ID
    }

    fn version(&self) -> &'static str {
        ADAPTER_VERSION
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.config.poll_interval_secs.max(1))
    }

    async fn poll(&self, ctx: &PollContext) -> Vec<Snapshot> {
        let map_ctx = MapContext::from_poll(ctx, &self.config.account, ADAPTER_VERSION);
        let config = self.config.clone();
        let found = tokio::task::spawn_blocking(move || find_event(&config))
            .await
            .unwrap_or(Found::NoEvent);
        match found {
            Found::Event(event) => map_input(&event, &map_ctx),
            Found::NoLogs => codex_map::fail_all(&META, &map_ctx, NOTE_NO_LOGS),
            Found::NoEvent => codex_map::fail_all(&META, &map_ctx, NOTE_NONE),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(timestamp: &str, limit_id: Value, primary_percent: f64) -> Value {
        json!({
            "timestamp": timestamp,
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": null,
                "rate_limits": {
                    "limit_id": limit_id,
                    "primary": {"used_percent": primary_percent, "window_minutes": 300, "resets_at": 1_791_641_400},
                    "secondary": {"used_percent": 10, "window_minutes": 10080, "resets_at": 1_792_051_200},
                },
            },
        })
    }

    fn lines(events: &[Value]) -> Vec<u8> {
        let mut out = Vec::new();
        for event in events {
            out.extend(serde_json::to_vec(event).unwrap());
            out.push(b'\n');
        }
        out
    }

    #[test]
    fn the_last_event_with_main_rate_limits_wins() {
        let bytes = lines(&[
            event("2026-10-10T11:00:00Z", json!("codex"), 10.0),
            event("2026-10-10T11:30:00Z", json!(null), 20.0),
            // A later event for a different limit does not hide the main one.
            event("2026-10-10T11:40:00Z", json!("codex_bengalfox"), 99.0),
            json!({"timestamp": "2026-10-10T11:50:00Z", "type": "event_msg", "payload": {"type": "agent_message"}}),
            json!({"timestamp": "2026-10-10T11:55:00Z", "type": "event_msg", "payload": {"type": "token_count", "rate_limits": null}}),
        ]);
        let found = last_rate_limit_event(&bytes, false).unwrap();
        assert_eq!(found["timestamp"], "2026-10-10T11:30:00Z");
    }

    #[test]
    fn truncated_and_partial_lines_are_tolerated() {
        let good = event("2026-10-10T11:00:00Z", json!("codex"), 10.0);
        let mut bytes = b"stuff that is the end of a cut-off line\n".to_vec();
        bytes.extend(lines(std::slice::from_ref(&good)));
        // A record the writer is still appending.
        bytes.extend(br#"{"timestamp":"2026-10-10T11:59:00Z","type":"event_ms"#);
        assert_eq!(last_rate_limit_event(&bytes, true), Some(good.clone()));
        assert_eq!(last_rate_limit_event(&bytes, false), Some(good));
        // A tail that is one cut-off fragment has nothing.
        assert_eq!(last_rate_limit_event(b"abcdef", true), None);
        assert_eq!(last_rate_limit_event(b"", false), None);
        assert_eq!(last_rate_limit_event(b"\n\n  \n", false), None);
    }

    #[test]
    fn a_fragment_on_the_first_line_is_dropped_only_when_mid_file() {
        // Without the flag the fragment is just an unparseable line and is skipped anyway.
        let good = event("2026-10-10T11:00:00Z", json!("codex"), 10.0);
        let mut bytes = br#"ons":{"x":1}}"#.to_vec();
        bytes.push(b'\n');
        bytes.extend(lines(std::slice::from_ref(&good)));
        assert_eq!(last_rate_limit_event(&bytes, true), Some(good));
    }

    #[test]
    fn read_tail_respects_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-x.jsonl");
        fs::write(&path, "0123456789\nabcdefghij\n").unwrap();
        let (bytes, mid) = read_tail(&path, 8).unwrap();
        assert_eq!(bytes, b"defghij\n");
        assert!(mid);
        let (all, mid) = read_tail(&path, 1000).unwrap();
        assert_eq!(all, b"0123456789\nabcdefghij\n");
        assert!(!mid);
    }

    fn touch(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn only_rollout_logs_in_date_directories_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(
            &root.join("2026/10/09/rollout-2026-10-09T10-00-00-aaa.jsonl"),
            "{}\n",
        );
        touch(
            &root.join("2026/10/10/rollout-2026-10-10T09-00-00-bbb.jsonl"),
            "{}\n",
        );
        // Things that must never be opened.
        touch(&root.join("auth.json"), "SECRET");
        touch(&root.join("2026/auth.json"), "SECRET");
        touch(&root.join("2026/10/10/auth.json"), "SECRET");
        touch(&root.join("2026/10/10/notes.txt"), "x");
        touch(&root.join("latest/10/10/rollout-x.jsonl"), "{}\n");
        touch(&root.join("2026/oct/10/rollout-x.jsonl"), "{}\n");
        let found = find_recent_rollouts(root, 10);
        let names: Vec<_> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.iter().all(|n| n.starts_with("rollout-")));
        assert!(find_recent_rollouts(&root.join("missing"), 10).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        let outside = dir.path().join("outside");
        touch(&outside.join("01/01/rollout-evil.jsonl"), "{}\n");
        fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("2026")).unwrap();
        assert!(find_recent_rollouts(&root, 10).is_empty());

        let day = root.join("2025/01/01");
        fs::create_dir_all(&day).unwrap();
        std::os::unix::fs::symlink(
            outside.join("01/01/rollout-evil.jsonl"),
            day.join("rollout-link.jsonl"),
        )
        .unwrap();
        assert!(find_recent_rollouts(&root, 10).is_empty());
    }

    #[test]
    fn newest_by_modification_time_comes_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // The older session (by name and directory) was written to more recently.
        let old = root.join("2026/10/01/rollout-2026-10-01T00-00-00-old.jsonl");
        let new = root.join("2026/10/10/rollout-2026-10-10T00-00-00-new.jsonl");
        touch(&new, "{}\n");
        std::thread::sleep(Duration::from_millis(30));
        touch(&old, "{}\n");
        let found = find_recent_rollouts(root, 10);
        assert_eq!(found, vec![old, new]);
        assert_eq!(find_recent_rollouts(root, 1).len(), 1);
    }

    #[test]
    fn sessions_dir_follows_codex_home() {
        assert_eq!(
            resolve_sessions_dir(None, Some(PathBuf::from("/home/me"))),
            PathBuf::from("/home/me/.codex/sessions")
        );
        assert_eq!(
            resolve_sessions_dir(Some("/custom".into()), Some(PathBuf::from("/home/me"))),
            PathBuf::from("/custom/sessions")
        );
        assert_eq!(
            resolve_sessions_dir(Some("".into()), Some(PathBuf::from("/home/me"))),
            PathBuf::from("/home/me/.codex/sessions")
        );
    }
}
