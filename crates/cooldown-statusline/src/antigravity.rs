//! Antigravity CLI (Google's successor to Gemini CLI): the same hook idea as for Claude Code.
//!
//! Antigravity CLI runs the configured `statusLine` command whenever its state changes and pipes a
//! JSON document to it (documented at antigravity.google/docs/cli/statusline). That document has a
//! `quota` object: one entry per model or bucket id (for example `gemini-weekly`), each with
//! `remaining_fraction` (0..1), `reset_time` (RFC 3339) and `reset_in_seconds`.
//!
//! Hook mode (`cooldown-statusline antigravity`) keeps only that object, writes it to
//! `antigravity-statusline.json` in the inbox directory, then chains to the user's previous status
//! line (kept in `chain-antigravity.json`) or prints a short default line. Nothing else from the
//! payload (working directory, conversation id, model) is stored.
//!
//! Install and uninstall reuse [`crate::install`] against `~/.gemini/antigravity-cli/settings.json`,
//! whose `statusLine` block has the same shape as Claude Code's.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::atomic::{cleanup_stale_temps, write_atomic};
use crate::chain;
use crate::hook::HookContext;
use crate::inbox::format_now;
use crate::parse::MAX_INPUT_BYTES;

/// The subcommand that runs hook mode for Antigravity CLI. Also appended to the registered command.
pub const SUBCOMMAND: &str = "antigravity";
/// Inbox file name.
pub const INBOX_FILE_NAME: &str = "antigravity-statusline.json";
/// Where the user's previous Antigravity status line is kept.
pub const CHAIN_FILE_NAME: &str = "chain-antigravity.json";
/// Inbox schema version.
pub const INBOX_VERSION: u32 = 1;
/// At most this many buckets are kept (a real payload has a handful).
const MAX_BUCKETS: usize = 32;

/// One quota bucket as stored in the inbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bucket {
    /// 0..=1, the share of the quota left.
    pub remaining_fraction: Option<f64>,
    /// RFC 3339 UTC.
    pub reset_time: Option<String>,
}

/// The inbox document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inbox {
    pub version: u32,
    /// RFC 3339 UTC, whole seconds: when the hook saw this data.
    pub observed_at: String,
    /// Bucket id to bucket.
    pub buckets: BTreeMap<String, Bucket>,
}

/// A bucket id worth keeping: short, printable, no path or control characters.
fn is_bucket_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' ' | '(' | ')'))
}

fn reset_time(raw: &Value) -> Option<String> {
    let text = raw.as_str()?;
    let parsed = OffsetDateTime::parse(text, &Rfc3339).ok()?;
    parsed.to_offset(time::UtcOffset::UTC).format(&Rfc3339).ok()
}

/// The quota buckets in a status line payload. `None` when there is no `quota` object (nothing to
/// record: overwriting the inbox would erase the last good reading).
pub fn parse_quota(bytes: &[u8]) -> Option<BTreeMap<String, Bucket>> {
    if bytes.len() > MAX_INPUT_BYTES {
        return None;
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let quota = value.get("quota")?.as_object()?;
    let mut out = BTreeMap::new();
    for (id, raw) in quota.iter().take(MAX_BUCKETS) {
        if !is_bucket_id(id) || !raw.is_object() {
            continue;
        }
        let fraction = raw
            .get("remaining_fraction")
            .and_then(Value::as_f64)
            .filter(|f| f.is_finite())
            .map(|f| f.clamp(0.0, 1.0));
        out.insert(
            id.clone(),
            Bucket {
                remaining_fraction: fraction,
                reset_time: raw.get("reset_time").and_then(reset_time),
            },
        );
    }
    Some(out)
}

/// Write the inbox atomically into `dir`.
pub fn write_inbox(dir: &Path, inbox: &Inbox) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let target = dir.join(INBOX_FILE_NAME);
    let mut bytes = serde_json::to_vec_pretty(inbox).unwrap_or_else(|_| b"{}".to_vec());
    bytes.push(b'\n');
    write_atomic(&target, &bytes)?;
    cleanup_stale_temps(dir, INBOX_FILE_NAME, Duration::from_secs(60));
    Ok(target)
}

/// The default status line: `gemini-weekly 6% used`, the tightest bucket first.
pub fn default_line(buckets: Option<&BTreeMap<String, Bucket>>) -> String {
    let mut parts: Vec<(f64, String)> = buckets
        .into_iter()
        .flatten()
        .filter_map(|(id, b)| {
            let used = (1.0 - b.remaining_fraction?) * 100.0;
            Some((used, format!("{id} {}% used", used.round())))
        })
        .collect();
    parts.sort_by(|a, b| b.0.total_cmp(&a.0));
    match parts.first() {
        Some((_, text)) => text.clone(),
        None => "quota --".to_owned(),
    }
}

/// Hook mode for Antigravity CLI. Same guarantees as [`crate::hook::run`]: never fails the status
/// line; the observation is recorded before chaining.
pub fn run(input: &[u8], ctx: &HookContext, out: &mut dyn Write) -> i32 {
    let buckets = parse_quota(input);
    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
        if let (Some(buckets), Some(dir)) = (&buckets, &ctx.inbox_dir) {
            let inbox = Inbox {
                version: INBOX_VERSION,
                observed_at: format_now(ctx.now),
                buckets: buckets.clone(),
            };
            let _ = write_inbox(dir, &inbox);
        }
    }));
    let chained = panic::catch_unwind(AssertUnwindSafe(|| {
        if ctx.nested {
            return None;
        }
        let command = chain::load(ctx.chain_path.as_ref()?)?
            .original_command()?
            .to_owned();
        chain::run_chain(&ctx.shell, &command, input).ok()
    }))
    .ok()
    .flatten();
    if let Some(output) = chained {
        let _ = out.write_all(&output.stdout);
        let _ = out.flush();
        return output.code;
    }
    let _ = writeln!(out, "{}", default_line(buckets.as_ref()));
    let _ = out.flush();
    0
}

/// `<home>/.gemini/antigravity-cli/settings.json`.
pub fn resolve_settings_path(home_dir: Option<PathBuf>) -> Option<PathBuf> {
    home_dir.map(|h| {
        h.join(".gemini")
            .join("antigravity-cli")
            .join("settings.json")
    })
}

/// The Antigravity CLI settings file for the current user.
pub fn default_settings_path() -> Option<PathBuf> {
    resolve_settings_path(dirs::home_dir())
}

/// The chain file for Antigravity: beside Claude's `chain.json`, under its own name.
pub fn chain_config_path() -> Option<PathBuf> {
    crate::paths::chain_config_path().map(|p| p.with_file_name(CHAIN_FILE_NAME))
}

/// The hook context for a real Antigravity run.
pub fn context_from_env() -> HookContext {
    HookContext {
        chain_path: chain_config_path(),
        ..HookContext::from_env()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::ChainShell;
    use time::macros::datetime;

    const SAMPLE: &[u8] = br#"{
      "cwd": "/home/example/project",
      "conversation_id": "00000000-0000-4000-8000-000000000001",
      "model": {"id": "Gemini 3.5 Flash (High)", "display_name": "Gemini 3.5 Flash (High)"},
      "quota": {
        "gemini-weekly": {"remaining_fraction": 0.9378, "reset_time": "2026-10-13T07:50:32Z", "reset_in_seconds": 560580}
      },
      "agent_state": "idle"
    }"#;

    fn ctx(dir: &Path) -> HookContext {
        HookContext {
            inbox_dir: Some(dir.join("inbox")),
            chain_path: Some(dir.join("cfg").join(CHAIN_FILE_NAME)),
            shell: ChainShell::for_platform(false, None),
            now: datetime!(2026-10-10 12:00:00.5 UTC),
            nested: false,
        }
    }

    #[test]
    fn records_only_the_quota_and_prints_the_tightest_bucket() {
        let tmp = tempfile::tempdir().unwrap();
        let mut out = Vec::new();
        assert_eq!(run(SAMPLE, &ctx(tmp.path()), &mut out), 0);
        assert_eq!(String::from_utf8(out).unwrap(), "gemini-weekly 6% used\n");
        let text = fs::read_to_string(tmp.path().join("inbox").join(INBOX_FILE_NAME)).unwrap();
        let inbox: Inbox = serde_json::from_str(&text).unwrap();
        assert_eq!(inbox.observed_at, "2026-10-10T12:00:00Z");
        assert_eq!(
            inbox.buckets["gemini-weekly"].remaining_fraction,
            Some(0.9378)
        );
        assert_eq!(
            inbox.buckets["gemini-weekly"].reset_time.as_deref(),
            Some("2026-10-13T07:50:32Z")
        );
        assert!(
            !text.contains("home/example"),
            "nothing but the quota is kept: {text}"
        );
        assert!(!text.contains("conversation"), "{text}");
    }

    #[test]
    fn a_payload_without_quota_leaves_the_last_reading_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let mut out = Vec::new();
        run(SAMPLE, &ctx(tmp.path()), &mut Vec::new());
        run(br#"{"agent_state":"busy"}"#, &ctx(tmp.path()), &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "quota --\n");
        let text = fs::read_to_string(tmp.path().join("inbox").join(INBOX_FILE_NAME)).unwrap();
        assert!(text.contains("gemini-weekly"));
    }

    #[test]
    fn hostile_input_is_tolerated() {
        assert_eq!(parse_quota(b"not json"), None);
        assert_eq!(parse_quota(br#"{"quota": []}"#), None);
        let parsed = parse_quota(
            br#"{"quota": {"../evil": {"remaining_fraction": 0.5}, "ok": {"remaining_fraction": 7, "reset_time": "soon"}}}"#,
        )
        .unwrap();
        assert!(!parsed.contains_key("../evil"));
        assert_eq!(parsed["ok"].remaining_fraction, Some(1.0));
        assert_eq!(parsed["ok"].reset_time, None);
    }

    #[test]
    fn paths_are_beside_claudes_but_separate() {
        assert_eq!(
            resolve_settings_path(Some(PathBuf::from("/home/u"))).unwrap(),
            Path::new("/home/u/.gemini/antigravity-cli/settings.json")
        );
        if let Some(p) = chain_config_path() {
            assert_eq!(p.file_name().unwrap(), CHAIN_FILE_NAME);
        }
    }
}
