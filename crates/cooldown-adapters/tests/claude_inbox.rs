//! The Claude inbox adapter against real files in temporary directories.

mod common;

use std::sync::Arc;
use std::time::Duration;

use cooldown_adapters::claude::{ClaudeInboxAdapter, ClaudeInboxConfig};
use cooldown_core::{Adapter, PollContext, Registry, Snapshot, Status, SystemClock};
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::macros::datetime;
use time::OffsetDateTime;

use common::{all_text, fixture, fixtures_root, read_json};

fn adapter_in(dir: &std::path::Path) -> ClaudeInboxAdapter {
    ClaudeInboxAdapter::new(ClaudeInboxConfig {
        inbox_path: dir.join("claude-statusline.json"),
        ..ClaudeInboxConfig::default()
    })
    .unwrap()
}

fn poll_at(now: OffsetDateTime, previous: Vec<Snapshot>) -> PollContext {
    PollContext {
        now,
        previous,
        consecutive_failures: 0,
    }
}

fn write_inbox(dir: &std::path::Path, value: &Value) {
    std::fs::write(
        dir.join("claude-statusline.json"),
        serde_json::to_vec_pretty(value).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn a_missing_file_is_unavailable_with_a_helpful_note() {
    let dir = tempfile::tempdir().unwrap();
    let now = datetime!(2026-10-10 12:00:00 UTC);
    let out = adapter_in(dir.path()).poll(&poll_at(now, vec![])).await;
    assert_eq!(out.len(), 2);
    for snapshot in &out {
        assert_eq!(snapshot.status, Status::Unavailable);
        assert_eq!(snapshot.provider, "claude");
        assert_eq!(snapshot.observed_at, now);
        assert!(snapshot
            .note
            .as_deref()
            .unwrap()
            .contains("Run Claude Code with the Cooldown hook installed"));
        // The note must not leak where the file is.
        assert!(!snapshot
            .note
            .as_deref()
            .unwrap()
            .contains("claude-statusline.json"));
        snapshot.validate().unwrap();
    }
    assert_eq!(out[0].id, "claude:default:session_5h");
    assert_eq!(out[1].id, "claude:default:weekly");
}

#[tokio::test]
async fn a_good_inbox_gives_official_snapshots_with_the_files_age() {
    let dir = tempfile::tempdir().unwrap();
    write_inbox(
        dir.path(),
        &json!({
            "version": 1,
            "observedAt": "2026-10-10T11:50:00Z",
            "fiveHour": {"usedPercentage": 23.5, "resetsAt": "2026-10-10T16:00:00Z"},
            "sevenDay": {"usedPercentage": 41.2, "resetsAt": "2026-10-14T16:00:00Z"},
            "spendLimit": null,
            "claudeCodeVersion": "2.1.90"
        }),
    );
    let now = datetime!(2026-10-10 12:00:00 UTC);
    let out = adapter_in(dir.path()).poll(&poll_at(now, vec![])).await;
    assert_eq!(out[0].percent_used, Some(23.5));
    assert_eq!(out[1].percent_used, Some(41.2));
    for snapshot in &out {
        assert_eq!(snapshot.status, Status::Ok);
        assert_eq!(snapshot.source.adapter, "claude-statusline");
        assert_eq!(snapshot.observed_at, datetime!(2026-10-10 11:50:00 UTC));
        snapshot.validate().unwrap();
    }
    assert_eq!(
        serde_json::to_value(&out[0].source).unwrap()["kind"],
        "official"
    );
}

#[tokio::test]
async fn a_stale_file_keeps_its_observed_time_and_is_marked_stale_after_the_reset() {
    let dir = tempfile::tempdir().unwrap();
    write_inbox(
        dir.path(),
        &json!({
            "version": 1,
            "observedAt": "2026-10-10T08:30:00Z",
            "fiveHour": {"usedPercentage": 64, "resetsAt": "2026-10-10T10:00:00Z"},
            "sevenDay": {"usedPercentage": 31.5, "resetsAt": "2026-10-13T09:00:00Z"},
        }),
    );
    let now = datetime!(2026-10-10 12:00:00 UTC);
    let out = adapter_in(dir.path()).poll(&poll_at(now, vec![])).await;
    assert_eq!(out[0].status, Status::Stale);
    assert_eq!(out[0].observed_at, datetime!(2026-10-10 08:30:00 UTC));
    assert_eq!(out[0].percent_used, Some(64.0));
    assert_eq!(out[1].status, Status::Ok);
    assert_eq!(out[1].observed_at, datetime!(2026-10-10 08:30:00 UTC));
}

#[tokio::test]
async fn a_window_that_disappears_after_its_reset_means_the_reset_happened() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = adapter_in(dir.path());
    write_inbox(
        dir.path(),
        &json!({
            "version": 1,
            "observedAt": "2026-10-10T11:00:00Z",
            "fiveHour": {"usedPercentage": 88, "resetsAt": "2026-10-10T12:30:00Z"},
            "sevenDay": {"usedPercentage": 20, "resetsAt": "2026-10-14T00:00:00Z"},
        }),
    );
    let first = adapter
        .poll(&poll_at(datetime!(2026-10-10 11:05:00 UTC), vec![]))
        .await;
    assert_eq!(first[0].status, Status::Ok);

    // Claude Code runs again after the reset and no longer reports the five-hour window.
    write_inbox(
        dir.path(),
        &json!({
            "version": 1,
            "observedAt": "2026-10-10T13:00:00Z",
            "fiveHour": null,
            "sevenDay": {"usedPercentage": 22, "resetsAt": "2026-10-14T00:00:00Z"},
        }),
    );
    let now = datetime!(2026-10-10 13:01:00 UTC);
    let second = adapter.poll(&poll_at(now, first)).await;
    assert_eq!(second[0].status, Status::Ok);
    assert_eq!(second[0].percent_used, Some(0.0));
    assert_eq!(second[0].note.as_deref(), Some("window reset"));
    assert_eq!(second[0].resets_at, None);
    assert_eq!(second[0].observed_at, now);
    assert_eq!(second[1].percent_used, Some(22.0));

    // Still missing on the next poll: the marker repeats.
    let third = adapter
        .poll(&poll_at(datetime!(2026-10-10 13:02:00 UTC), second))
        .await;
    assert_eq!(third[0].note.as_deref(), Some("window reset"));
}

#[tokio::test]
async fn a_window_missing_before_its_reset_keeps_the_last_value_as_degraded() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = adapter_in(dir.path());
    write_inbox(
        dir.path(),
        &json!({
            "version": 1,
            "observedAt": "2026-10-10T11:00:00Z",
            "fiveHour": {"usedPercentage": 50, "resetsAt": "2026-10-10T15:00:00Z"},
            "sevenDay": null,
        }),
    );
    let first = adapter
        .poll(&poll_at(datetime!(2026-10-10 11:05:00 UTC), vec![]))
        .await;
    write_inbox(
        dir.path(),
        &json!({"version": 1, "observedAt": "2026-10-10T12:00:00Z", "fiveHour": null, "sevenDay": null}),
    );
    let second = adapter
        .poll(&poll_at(datetime!(2026-10-10 12:01:00 UTC), first))
        .await;
    assert_eq!(second[0].status, Status::Degraded);
    assert_eq!(second[0].percent_used, Some(50.0));
    assert_eq!(second[0].observed_at, datetime!(2026-10-10 11:00:00 UTC));
}

#[tokio::test]
async fn a_deleted_file_keeps_the_last_value() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = adapter_in(dir.path());
    write_inbox(
        dir.path(),
        &json!({
            "version": 1,
            "observedAt": "2026-10-10T11:00:00Z",
            "fiveHour": {"usedPercentage": 50, "resetsAt": "2026-10-10T15:00:00Z"},
            "sevenDay": {"usedPercentage": 5, "resetsAt": "2026-10-14T00:00:00Z"},
        }),
    );
    let first = adapter
        .poll(&poll_at(datetime!(2026-10-10 11:05:00 UTC), vec![]))
        .await;
    std::fs::remove_file(dir.path().join("claude-statusline.json")).unwrap();
    let second = adapter
        .poll(&poll_at(datetime!(2026-10-10 11:10:00 UTC), first))
        .await;
    assert_eq!(second[0].status, Status::Degraded);
    assert_eq!(second[0].percent_used, Some(50.0));
    assert!(second[0]
        .note
        .as_deref()
        .unwrap()
        .contains("Run Claude Code with the Cooldown hook installed"));
}

#[tokio::test]
async fn unusable_files_never_panic_and_degrade() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = adapter_in(dir.path());
    let now = datetime!(2026-10-10 12:00:00 UTC);
    let path = dir.path().join("claude-statusline.json");
    let big = vec![b' '; 2 * 1024 * 1024];
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("garbage", b"\x00\x01\x02 not json".to_vec()),
        (
            "truncated mid-write",
            br#"{"version":1,"observedAt":"2026-10-10T11:00:00Z","fiveHour":{"usedPerc"#.to_vec(),
        ),
        (
            "future version",
            br#"{"version":2,"observedAt":"2026-10-10T11:00:00Z"}"#.to_vec(),
        ),
        ("oversized", big),
        ("array", b"[1,2,3]".to_vec()),
    ];
    for (name, bytes) in cases {
        std::fs::write(&path, bytes).unwrap();
        let out = adapter.poll(&poll_at(now, vec![])).await;
        assert_eq!(out.len(), 2, "{name}");
        for snapshot in &out {
            assert_eq!(snapshot.status, Status::Unavailable, "{name}");
            assert!(snapshot.note.is_some(), "{name}");
            snapshot.validate().unwrap();
        }
    }
}

#[tokio::test]
async fn a_future_observed_time_counts_as_now() {
    let dir = tempfile::tempdir().unwrap();
    write_inbox(
        dir.path(),
        &json!({
            "version": 1,
            "observedAt": "2026-10-10T13:00:00Z",
            "fiveHour": {"usedPercentage": 10, "resetsAt": "2026-10-10T16:00:00Z"},
            "sevenDay": null,
        }),
    );
    let now = datetime!(2026-10-10 12:00:00 UTC);
    let out = adapter_in(dir.path()).poll(&poll_at(now, vec![])).await;
    assert_eq!(out[0].observed_at, now);
}

/// Build the inbox file the hook would have written for a fixture's `rate_limits`.
fn inbox_for(input: &Value, observed_at: &str) -> Value {
    let window = |key: &str| -> Value {
        let Some(raw) = input
            .get("rate_limits")
            .and_then(|r| r.get(key))
            .filter(|w| w.is_object())
        else {
            return Value::Null;
        };
        // The hook keeps only numbers, clamps percentages and drops non-positive reset times.
        let percent = raw
            .get("used_percentage")
            .and_then(Value::as_f64)
            .map(|p| p.clamp(0.0, 100.0));
        let resets = raw
            .get("resets_at")
            .and_then(Value::as_i64)
            .filter(|s| *s > 0)
            .and_then(|s| OffsetDateTime::from_unix_timestamp(s).ok())
            .map(|t| t.format(&Rfc3339).unwrap());
        if percent.is_none() && resets.is_none() {
            return Value::Null;
        }
        json!({"usedPercentage": percent, "resetsAt": resets})
    };
    json!({
        "version": 1,
        "observedAt": observed_at,
        "fiveHour": window("five_hour"),
        "sevenDay": window("seven_day"),
        "spendLimit": null,
        "claudeCodeVersion": null,
    })
}

#[tokio::test]
async fn the_inbox_path_reproduces_every_statusline_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = adapter_in(dir.path());
    let root = fixtures_root().join("claude-statusline");
    let mut checked = 0;
    for entry in std::fs::read_dir(&root).unwrap().filter_map(Result::ok) {
        let name = entry.file_name().into_string().unwrap();
        let Some(case) = name.strip_suffix(".input.json") else {
            continue;
        };
        let fx = fixture("claude-statusline", case);
        let context = read_json(&root.join(format!("{case}.context.json")));
        let observed_at = context
            .get("observedAt")
            .or_else(|| context.get("now"))
            .and_then(Value::as_str)
            .unwrap()
            .to_owned();
        write_inbox(dir.path(), &inbox_for(&fx.input, &observed_at));
        let got = adapter.poll(&fx.poll_context()).await;
        assert_eq!(got, fx.expected, "fixture case {case}\n{}", all_text(&got));
        checked += 1;
    }
    assert!(checked >= 9, "only {checked} claude cases found");
}

#[tokio::test]
async fn the_registry_can_drive_it() {
    let dir = tempfile::tempdir().unwrap();
    write_inbox(
        dir.path(),
        &json!({
            "version": 1,
            "observedAt": OffsetDateTime::now_utc().format(&Rfc3339).unwrap(),
            "fiveHour": {"usedPercentage": 10, "resetsAt": null},
            "sevenDay": null,
        }),
    );
    let adapter: Arc<dyn Adapter> = Arc::new(adapter_in(dir.path()));
    let (handle, mut events) = Registry::new(Arc::new(SystemClock))
        .register(adapter)
        .start();
    let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("a poll event within 5 s")
        .expect("channel open");
    assert_eq!(event.adapter, "claude-statusline");
    assert!(!event.failed);
    assert_eq!(event.snapshots[0].percent_used, Some(10.0));
    handle.shutdown().await;
}

#[test]
fn the_default_path_is_under_the_local_data_dir() {
    // The environment override is the hook's own and may be set on a developer machine.
    if std::env::var_os("COOLDOWN_INBOX_DIR").is_some() {
        return;
    }
    let expected = dirs::data_local_dir()
        .unwrap()
        .join("cooldown")
        .join("inbox")
        .join("claude-statusline.json");
    assert_eq!(ClaudeInboxConfig::default().inbox_path, expected);
}
