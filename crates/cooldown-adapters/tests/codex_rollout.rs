//! The Codex rollout-log adapter against session directories in temporary directories.

mod common;

use std::fs;
use std::path::Path;

use cooldown_adapters::codex_rollout::{CodexRolloutAdapter, CodexRolloutConfig};
use cooldown_core::{Adapter, PollContext, Status};
use time::macros::datetime;

use common::{fixture, fixtures_root};

fn adapter(sessions: &Path) -> CodexRolloutAdapter {
    CodexRolloutAdapter::new(CodexRolloutConfig {
        sessions_dir: sessions.to_path_buf(),
        ..CodexRolloutConfig::default()
    })
    .unwrap()
}

fn write_log(sessions: &Path, relative: &str, lines: &[String]) {
    let path = sessions.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut text = lines.join("\n");
    text.push('\n');
    fs::write(path, text).unwrap();
}

/// The single line of a rollout fixture's input.
fn fixture_line(case: &str) -> String {
    let path = fixtures_root().join(format!("codex-rollout-log/{case}.input.json"));
    let text = fs::read_to_string(path).unwrap();
    serde_json::to_string(&serde_json::from_str::<serde_json::Value>(&text).unwrap()).unwrap()
}

const FILLER: &str = r#"{"timestamp":"2026-10-10T11:58:00.000Z","type":"response_item","payload":{"type":"message","content":"hello"}}"#;

#[tokio::test]
async fn the_last_rate_limits_in_the_newest_log_become_the_fixture_snapshots() {
    let fx = fixture("codex-rollout-log", "both-windows");
    let dir = tempfile::tempdir().unwrap();
    write_log(
        dir.path(),
        "2026/10/10/rollout-2026-10-10T11-00-00-aaaa.jsonl",
        &[
            FILLER.to_owned(),
            fixture_line("both-windows"),
            FILLER.to_owned(),
        ],
    );
    let got = adapter(dir.path()).poll(&fx.poll_context()).await;
    assert_eq!(got, fx.expected);
}

#[tokio::test]
async fn a_huge_log_is_only_read_from_its_tail_and_a_cut_line_is_ignored() {
    let fx = fixture("codex-rollout-log", "both-windows");
    let dir = tempfile::tempdir().unwrap();
    // 3 MiB of junk before the event, a tail cap of 64 KiB, and a half-written record at the end.
    let huge = format!(
        r#"{{"type":"response_item","payload":{{"type":"message","content":"{}"}}}}"#,
        "x".repeat(3 * 1024 * 1024)
    );
    let path = dir
        .path()
        .join("2026/10/10/rollout-2026-10-10T11-00-00-big.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut text = huge;
    text.push('\n');
    text.push_str(&fixture_line("both-windows"));
    text.push('\n');
    text.push_str(r#"{"timestamp":"2026-10-10T11:59:59.000Z","type":"event_msg","payload":{"type":"token_cou"#);
    fs::write(&path, text).unwrap();

    let adapter = CodexRolloutAdapter::new(CodexRolloutConfig {
        sessions_dir: dir.path().to_path_buf(),
        tail_bytes: 64 * 1024,
        ..CodexRolloutConfig::default()
    })
    .unwrap();
    let got = adapter.poll(&fx.poll_context()).await;
    assert_eq!(got, fx.expected);
}

#[tokio::test]
async fn an_old_event_whose_window_has_reset_is_stale() {
    let fx = fixture("codex-rollout-log", "stale-event");
    let dir = tempfile::tempdir().unwrap();
    write_log(
        dir.path(),
        "2026/10/10/rollout-2026-10-10T01-00-00-aaaa.jsonl",
        &[fixture_line("stale-event")],
    );
    let got = adapter(dir.path()).poll(&fx.poll_context()).await;
    assert_eq!(got, fx.expected);
    assert_eq!(got[0].status, Status::Stale);
}

#[tokio::test]
async fn an_event_for_another_limit_is_skipped_for_an_earlier_main_one() {
    let fx = fixture("codex-rollout-log", "both-windows");
    let dir = tempfile::tempdir().unwrap();
    write_log(
        dir.path(),
        "2026/10/10/rollout-2026-10-10T11-00-00-aaaa.jsonl",
        &[fixture_line("both-windows"), fixture_line("other-limit-id")],
    );
    let got = adapter(dir.path()).poll(&fx.poll_context()).await;
    assert_eq!(got, fx.expected);
}

#[tokio::test]
async fn it_falls_back_to_an_older_log_when_the_newest_has_no_rate_limits() {
    let fx = fixture("codex-rollout-log", "both-windows");
    let dir = tempfile::tempdir().unwrap();
    write_log(
        dir.path(),
        "2026/10/09/rollout-2026-10-09T11-00-00-old.jsonl",
        &[fixture_line("both-windows")],
    );
    std::thread::sleep(std::time::Duration::from_millis(30));
    write_log(
        dir.path(),
        "2026/10/10/rollout-2026-10-10T11-00-00-new.jsonl",
        &[FILLER.to_owned(), fixture_line("null-rate-limits")],
    );
    let got = adapter(dir.path()).poll(&fx.poll_context()).await;
    assert_eq!(got, fx.expected);
}

#[tokio::test]
async fn no_logs_and_no_events_are_unavailable_with_distinct_notes() {
    let now = datetime!(2026-10-10 12:00:00 UTC);
    let dir = tempfile::tempdir().unwrap();

    let got = adapter(dir.path()).poll(&PollContext::new(now)).await;
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].status, Status::Unavailable);
    assert_eq!(got[0].note.as_deref(), Some("No Codex session logs found"));

    write_log(
        dir.path(),
        "2026/10/10/rollout-2026-10-10T11-00-00-aaaa.jsonl",
        &[FILLER.to_owned()],
    );
    let got = adapter(dir.path()).poll(&PollContext::new(now)).await;
    assert_eq!(
        got[1].note.as_deref(),
        Some("No Codex rate-limit data in the latest rollout event")
    );

    let missing = adapter(&dir.path().join("does-not-exist"))
        .poll(&PollContext::new(now))
        .await;
    assert_eq!(missing[0].status, Status::Unavailable);
}

#[tokio::test]
async fn files_that_are_not_session_logs_are_never_read() {
    // The codex home itself is configured by mistake: auth.json and config.toml sit next to
    // `sessions`, and a rollout-looking file sits where a log must not be.
    let home = tempfile::tempdir().unwrap();
    fs::write(
        home.path().join("auth.json"),
        r#"{"tokens":{"access_token":"SECRET"}}"#,
    )
    .unwrap();
    fs::write(
        home.path().join("rollout-fake.jsonl"),
        fixture_line("both-windows"),
    )
    .unwrap();
    let fx = fixture("codex-rollout-log", "both-windows");
    let got = adapter(home.path()).poll(&fx.poll_context()).await;
    assert_eq!(got[0].status, Status::Unavailable);
    assert_eq!(got[0].note.as_deref(), Some("No Codex session logs found"));
}
