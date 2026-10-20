//! The Codex app-server adapter against a fake `codex` executable.
//!
//! On Unix the fake is a `sh` script; on Windows a `.cmd` shim (that part only compiles there and
//! has not been run on a real Windows machine). Nothing here starts the real Codex.

#![cfg_attr(windows, allow(dead_code, unused_imports))]

mod common;

use std::path::PathBuf;
use std::time::Duration;

use cooldown_adapters::codex_app_server::{CodexAppServerAdapter, CodexAppServerConfig};
use cooldown_core::{Adapter, PollContext, Status};
use serde_json::Value;
use time::macros::datetime;

use common::{all_text, fixture};

/// A fake Codex: where it lives and where it records what it saw.
struct FakeCodex {
    dir: tempfile::TempDir,
    exe: PathBuf,
}

impl FakeCodex {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.path(name)).unwrap_or_default()
    }

    fn adapter(&self, timeout_secs: u64) -> CodexAppServerAdapter {
        CodexAppServerAdapter::new(CodexAppServerConfig {
            binary: Some(self.exe.clone()),
            timeout_secs,
            ..CodexAppServerConfig::default()
        })
        .unwrap()
    }
}

/// How the fake behaves after the handshake.
#[derive(Clone, Copy, PartialEq)]
enum After {
    /// Answer and exit.
    Exit,
    /// Answer and keep running (the adapter must kill it).
    Linger,
    /// Never answer the rate-limits request.
    Hang,
    /// Close the pipe without answering.
    ExitSilently,
}

#[cfg(unix)]
fn fake_codex(response: &Value, after: After) -> FakeCodex {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    std::fs::write(
        base.join("response.json"),
        serde_json::to_string(response).unwrap(),
    )
    .unwrap();
    let on_request = match after {
        After::Exit => "cat \"$DIR/response.json\"; printf '\\n'; exit 0",
        After::Linger => "cat \"$DIR/response.json\"; printf '\\n'; exec sleep 60",
        After::Hang => "exec sleep 60",
        After::ExitSilently => "exit 0",
    };
    let script = format!(
        r#"#!/bin/sh
DIR='{dir}'
echo "$@" > "$DIR/args.txt"
env > "$DIR/env.txt"
echo $$ > "$DIR/pid.txt"
echo 'codex banner: not json'
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$DIR/requests.txt"
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{{"method":"account/rateLimits/updated","params":{{}}}}'
      printf '%s\n' '{{"id":1,"result":{{"userAgent":"fake-codex"}}}}' ;;
    *'"method":"account/rateLimits/read"'*)
      {on_request} ;;
  esac
done
"#,
        dir = base.display(),
    );
    let exe = base.join("codex");
    std::fs::write(&exe, script).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    FakeCodex { dir, exe }
}

#[cfg(windows)]
fn fake_codex(response: &Value, after: After) -> FakeCodex {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    // `type` copies the file as is, so the line must end in a newline.
    std::fs::write(
        base.join("response.json"),
        format!("{}\r\n", serde_json::to_string(response).unwrap()),
    )
    .unwrap();
    // Reads the three requests in order; ignores their content. PowerShell does the reading:
    // `set /p` on a piped stdin may consume several lines in one read, which made this test hang
    // whenever the client wrote two requests back to back.
    let on_request = match after {
        After::Exit | After::Linger => {
            "[Console]::Out.Write([IO.File]::ReadAllText(\"$PSScriptRoot\\response.json\")); [Console]::Out.Flush()"
        }
        After::Hang => "Start-Sleep -Seconds 60",
        After::ExitSilently => "exit 0",
    };
    let reader = format!(
        "$in = [Console]::In\r\n\
         $null = $in.ReadLine()\r\n\
         [Console]::Out.WriteLine('{{\"id\":1,\"result\":{{\"userAgent\":\"fake-codex\"}}}}')\r\n\
         [Console]::Out.Flush()\r\n\
         $null = $in.ReadLine()\r\n\
         $null = $in.ReadLine()\r\n\
         {on_request}\r\n"
    );
    std::fs::write(base.join("fake.ps1"), reader).unwrap();
    let script = "@echo off\r\n\
         echo %*> \"%~dp0args.txt\"\r\n\
         set > \"%~dp0env.txt\"\r\n\
         powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"%~dp0fake.ps1\"\r\n";
    let exe = base.join("codex.cmd");
    std::fs::write(&exe, script).unwrap();
    FakeCodex { dir, exe }
}

fn poll_ctx(fx: &common::Fixture) -> PollContext {
    fx.poll_context()
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    // A zombie (killed but not reaped) still answers `kill -0`, so this also proves reaping.
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(unix)]
async fn wait_until_gone(pid: u32) -> bool {
    for _ in 0..60 {
        if !process_exists(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[cfg(unix)]
fn pid_of(fake: &FakeCodex) -> u32 {
    fake.read("pid.txt")
        .trim()
        .parse()
        .expect("fake recorded its pid")
}

#[cfg(unix)]
#[tokio::test]
async fn the_fixture_responses_come_back_as_the_fixture_snapshots() {
    for case in [
        "both-windows",
        "weekly-in-primary",
        "swapped-slots",
        "resets-at-milliseconds",
        "unrecognised-duration",
        "error-response",
        "error-with-previous",
    ] {
        let fx = fixture("codex-app-server", case);
        let fake = fake_codex(&fx.input, After::Exit);
        let got = fake.adapter(10).poll(&poll_ctx(&fx)).await;
        assert_eq!(got, fx.expected, "case {case}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn it_speaks_the_handshake_in_order_with_the_right_arguments() {
    let fx = fixture("codex-app-server", "both-windows");
    let fake = fake_codex(&fx.input, After::Exit);
    let got = fake.adapter(10).poll(&poll_ctx(&fx)).await;
    assert_eq!(got, fx.expected);

    assert_eq!(fake.read("args.txt").trim(), "app-server --listen stdio://");
    let requests: Vec<Value> = fake
        .read("requests.txt")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let methods: Vec<&str> = requests
        .iter()
        .map(|r| r["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        ["initialize", "initialized", "account/rateLimits/read"]
    );
    assert_eq!(requests[0]["id"], 1);
    assert!(requests[1].get("id").is_none());
    assert_eq!(requests[2]["id"], 2);
}

#[cfg(unix)]
#[tokio::test]
async fn credential_variables_never_reach_the_child() {
    // Edition 2021: set_var is safe. Other tests in this binary do not read these variables, and
    // the adapter strips them from every child anyway.
    for key in ["OPENAI_API_KEY", "CODEX_API_KEY", "ANTHROPIC_API_KEY"] {
        std::env::set_var(key, "sk-test-must-not-leak");
    }
    let fx = fixture("codex-app-server", "both-windows");
    let fake = fake_codex(&fx.input, After::Exit);
    let adapter = CodexAppServerAdapter::new(CodexAppServerConfig {
        binary: Some(fake.exe.clone()),
        codex_home: Some(PathBuf::from("/tmp/some-codex-home")),
        timeout_secs: 10,
        ..CodexAppServerConfig::default()
    })
    .unwrap();
    let got = adapter.poll(&poll_ctx(&fx)).await;
    assert_eq!(got, fx.expected);
    let env = fake.read("env.txt");
    assert!(env.contains("PATH="), "the child still gets a PATH");
    assert!(!env.contains("OPENAI_API_KEY"), "{env}");
    assert!(!env.contains("CODEX_API_KEY"), "{env}");
    assert!(!env.contains("ANTHROPIC_API_KEY"), "{env}");
    assert!(!env.contains("must-not-leak"));
    assert!(env.contains("CODEX_HOME=/tmp/some-codex-home"));
    for key in ["OPENAI_API_KEY", "CODEX_API_KEY", "ANTHROPIC_API_KEY"] {
        std::env::remove_var(key);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_lingering_child_is_killed_and_reaped() {
    let fx = fixture("codex-app-server", "both-windows");
    let fake = fake_codex(&fx.input, After::Linger);
    let got = fake.adapter(10).poll(&poll_ctx(&fx)).await;
    assert_eq!(got, fx.expected);
    let pid = pid_of(&fake);
    assert!(wait_until_gone(pid).await, "process {pid} was left behind");
}

#[cfg(unix)]
#[tokio::test]
async fn a_hung_server_times_out_and_leaves_no_process() {
    let fx = fixture("codex-app-server", "both-windows");
    let fake = fake_codex(&fx.input, After::Hang);
    let started = std::time::Instant::now();
    let got = fake
        .adapter(1)
        .poll(&PollContext::new(fx.context.now))
        .await;
    assert!(started.elapsed() < Duration::from_secs(8));
    assert_eq!(got.len(), 2);
    for snapshot in &got {
        assert_eq!(snapshot.status, Status::Unavailable);
        assert_eq!(
            snapshot.note.as_deref(),
            Some("Codex app-server did not answer in time")
        );
    }
    let pid = pid_of(&fake);
    assert!(wait_until_gone(pid).await, "process {pid} was left behind");
}

#[cfg(unix)]
#[tokio::test]
async fn a_timeout_keeps_the_last_value() {
    let fx = fixture("codex-app-server", "error-with-previous");
    let fake = fake_codex(&fx.input, After::Hang);
    let got = fake.adapter(1).poll(&poll_ctx(&fx)).await;
    assert_eq!(got[0].status, Status::Degraded);
    assert_eq!(got[0].percent_used, Some(51.0));
    assert_eq!(got[1].status, Status::Stale);
    assert!(got[0]
        .note
        .as_deref()
        .unwrap()
        .starts_with("Codex app-server did not answer in time"));
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_the_poll_kills_the_child() {
    let fx = fixture("codex-app-server", "both-windows");
    let fake = fake_codex(&fx.input, After::Hang);
    let adapter = fake.adapter(30);
    let ctx = PollContext::new(fx.context.now);
    // The registry aborts a poll that is cancelled; simulate by dropping the future.
    let result = tokio::time::timeout(Duration::from_millis(600), adapter.poll(&ctx)).await;
    assert!(result.is_err(), "the fake never answers");
    let pid = pid_of(&fake);
    assert!(
        wait_until_gone(pid).await,
        "process {pid} survived the dropped poll"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_server_that_closes_the_pipe_is_reported() {
    let fx = fixture("codex-app-server", "both-windows");
    let fake = fake_codex(&fx.input, After::ExitSilently);
    let got = fake
        .adapter(10)
        .poll(&PollContext::new(fx.context.now))
        .await;
    assert_eq!(got[0].status, Status::Unavailable);
    assert_eq!(
        got[0].note.as_deref(),
        Some("Codex app-server exited before returning rate limits")
    );
}

#[tokio::test]
async fn a_missing_codex_is_unavailable_with_advice() {
    let adapter = CodexAppServerAdapter::new(CodexAppServerConfig {
        binary: Some(PathBuf::from("/definitely/not/here/codex")),
        timeout_secs: 5,
        ..CodexAppServerConfig::default()
    })
    .unwrap();
    let now = datetime!(2026-10-10 12:00:00 UTC);
    let got = adapter.poll(&PollContext::new(now)).await;
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].id, "codex:default:session_5h");
    assert_eq!(got[1].id, "codex:default:weekly");
    for snapshot in &got {
        assert_eq!(snapshot.status, Status::Unavailable);
        assert!(snapshot
            .note
            .as_deref()
            .unwrap()
            .contains("Codex CLI not found"));
        // The advice is something the user can do: the app has no setting for Codex's location.
        let note = snapshot.note.as_deref().unwrap();
        assert!(note.contains("works in a terminal"), "{note}");
        assert!(note.contains("use the refresh button"), "{note}");
        assert!(!note.contains("location"), "{note}");
        snapshot.validate().unwrap();
    }
    // Nothing from the file system leaks into the note.
    assert!(!all_text(&got).contains("/definitely/not/here"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_file_that_is_not_executable_is_reported_not_run() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("codex");
    std::fs::write(&exe, "#!/bin/sh\nexit 0\n").unwrap();
    // Mode 0644: not runnable, so it is not found.
    let adapter = CodexAppServerAdapter::new(CodexAppServerConfig {
        binary: Some(exe),
        timeout_secs: 5,
        ..CodexAppServerConfig::default()
    })
    .unwrap();
    let got = adapter
        .poll(&PollContext::new(datetime!(2026-10-10 12:00:00 UTC)))
        .await;
    assert_eq!(got[0].status, Status::Unavailable);
}

/// Windows: the `.cmd` shim path, with `CREATE_NO_WINDOW`. Only compiled and run on Windows.
#[cfg(windows)]
#[tokio::test]
async fn windows_cmd_shim_round_trip() {
    let fx = fixture("codex-app-server", "both-windows");
    let fake = fake_codex(&fx.input, After::Exit);
    let got = fake.adapter(20).poll(&poll_ctx(&fx)).await;
    assert_eq!(got, fx.expected);
    assert!(fake
        .read("args.txt")
        .contains("app-server --listen stdio://"));
    let env = fake.read("env.txt");
    assert!(!env.contains("OPENAI_API_KEY="));
}
