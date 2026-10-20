//! End-to-end tests that run the real `cooldown-statusline` binary.
//!
//! Safety: every invocation runs with HOME, USERPROFILE, APPDATA,
//! LOCALAPPDATA and the XDG directories pointing into a tempdir, plus explicit
//! `--settings`, `COOLDOWN_INBOX_DIR` and `COOLDOWN_CONFIG_DIR`. Nothing here
//! can reach the real `~/.claude`, `~/.codex`, credentials or the keychain.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[cfg(unix)]
use serde_json::json;
use serde_json::Value;

const SAMPLE: &str = r#"{
  "session_id": "secret-session-id",
  "cwd": "/home/someone/private-project",
  "version": "2.1.90",
  "cost": {"total_cost_usd": 1.23},
  "rate_limits": {
    "five_hour": {"used_percentage": 42, "resets_at": 1738425600},
    "seven_day": {"used_percentage": 18, "resets_at": 1738857600}
  }
}"#;

struct Sandbox {
    root: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for dir in ["home", "inbox", "config", "claude", "bin"] {
            fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        // The registered binary must be called `cooldown-statusline`; a copy
        // keeps the path under the sandbox like a real install would.
        let exe = root.path().join("bin").join(exe_name());
        fs::copy(env!("CARGO_BIN_EXE_cooldown-statusline"), &exe).unwrap();
        Self { root }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    fn exe(&self) -> PathBuf {
        self.path("bin").join(exe_name())
    }

    fn settings(&self) -> PathBuf {
        self.path("claude").join("settings.json")
    }

    fn inbox_file(&self) -> PathBuf {
        self.path("inbox").join("claude-statusline.json")
    }

    /// Point every place a program could look for a home or config directory
    /// at the sandbox.
    fn sandbox_env(&self, cmd: &mut Command) {
        let home = self.path("home");
        cmd.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("APPDATA", self.path("config"))
            .env("LOCALAPPDATA", self.path("inbox"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_DATA_HOME", self.path("inbox"))
            .env("COOLDOWN_INBOX_DIR", self.path("inbox"))
            .env("COOLDOWN_CONFIG_DIR", self.path("config"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(self.exe());
        self.sandbox_env(&mut cmd);
        cmd
    }

    /// `sh -c <command>`: how Claude Code runs a statusLine command.
    #[cfg(unix)]
    fn sh_command(&self, command: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        self.sandbox_env(&mut cmd);
        cmd
    }

    fn hook(&self, stdin: &str) -> Output {
        let mut child = self
            .command()
            .stdin(Stdio::piped())
            .spawn()
            .expect("spawn hook");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn with_settings(&self, extra: &[&str]) -> Vec<String> {
        let settings = self.settings();
        let mut args = vec![extra[0].to_owned()];
        args.extend(extra[1..].iter().map(|s| (*s).to_owned()));
        args.push("--settings".into());
        args.push(settings.to_string_lossy().into_owned());
        args
    }

    fn manage(&self, args: &[&str]) -> Output {
        let owned = self.with_settings(args);
        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        self.run(&refs)
    }
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "cooldown-statusline.exe"
    } else {
        "cooldown-statusline"
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn hook_prints_default_line_and_writes_the_inbox() {
    let sb = Sandbox::new();
    let out = sb.hook(SAMPLE);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "5h 42% · 7d 18%\n");

    let inbox = read_json(&sb.inbox_file());
    assert_eq!(inbox["version"], 1);
    assert_eq!(inbox["fiveHour"]["usedPercentage"], 42.0);
    assert_eq!(inbox["fiveHour"]["resetsAt"], "2025-02-01T16:00:00Z");
    assert_eq!(inbox["sevenDay"]["resetsAt"], "2025-02-06T16:00:00Z");
    assert_eq!(inbox["spendLimit"], Value::Null);
    assert_eq!(inbox["claudeCodeVersion"], "2.1.90");
    assert!(inbox["observedAt"].as_str().unwrap().ends_with('Z'));

    // Nothing identifying is stored.
    let text = fs::read_to_string(sb.inbox_file()).unwrap();
    for secret in ["secret-session-id", "private-project", "someone", "1.23"] {
        assert!(!text.contains(secret), "{secret} leaked into the inbox");
    }
    let keys: Vec<_> = inbox.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        [
            "version",
            "observedAt",
            "fiveHour",
            "sevenDay",
            "spendLimit",
            "claudeCodeVersion"
        ]
    );
}

#[test]
fn hook_survives_garbage_and_empty_input() {
    let sb = Sandbox::new();
    for input in [
        "",
        "not json",
        "{",
        "[]",
        "\u{feff}",
        "{\"rate_limits\": 5}",
    ] {
        let out = sb.hook(input);
        assert_eq!(out.status.code(), Some(0), "input {input:?}");
        assert_eq!(stdout(&out), "5h -- · 7d --\n");
    }
    assert!(
        !sb.inbox_file().exists(),
        "no rate_limits means no inbox write"
    );
}

#[test]
fn hook_with_a_terminal_free_empty_stdin_does_not_hang() {
    let sb = Sandbox::new();
    let started = std::time::Instant::now();
    let out = sb.run(&[]);
    assert_eq!(out.status.code(), Some(0));
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn hook_copes_with_stdin_far_over_the_cap() {
    let sb = Sandbox::new();
    let mut child = sb.command().stdin(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    // 4 MiB of junk; the hook stops reading at 1 MiB, so the writer may see a
    // broken pipe, which is fine.
    let writer = std::thread::spawn(move || {
        let chunk = vec![b'{'; 64 * 1024];
        for _ in 0..64 {
            if stdin.write_all(&chunk).is_err() {
                break;
            }
        }
    });
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "5h -- · 7d --\n");
}

#[test]
fn hook_gives_up_on_a_stdin_that_never_closes() {
    let sb = Sandbox::new();
    let mut child = sb.command().stdin(Stdio::piped()).spawn().unwrap();
    // Keep the pipe open and silent for the whole run.
    let _held_open = child.stdin.take().unwrap();
    let started = std::time::Instant::now();
    let status = child.wait().unwrap();
    let elapsed = started.elapsed();
    assert_eq!(status.code(), Some(0));
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "hook hung for {elapsed:?}"
    );
}

#[test]
fn hook_is_fast() {
    let sb = Sandbox::new();
    sb.hook(SAMPLE); // warm the filesystem cache
    let runs = 10;
    let started = std::time::Instant::now();
    for _ in 0..runs {
        sb.hook(SAMPLE);
    }
    let per_run = started.elapsed() / runs;
    // The target is < 50 ms; leave generous headroom for loaded CI machines.
    assert!(
        per_run < std::time::Duration::from_millis(250),
        "hook took {per_run:?} per run"
    );
}

#[test]
fn unknown_subcommand_and_help_and_version() {
    let sb = Sandbox::new();
    let out = sb.run(&["frobnicate"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("unknown command"));

    let out = sb.run(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("install --yes"));

    let out = sb.run(&["--version"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).starts_with("cooldown-statusline "));
}

#[cfg(unix)]
#[test]
fn full_lifecycle_install_chain_uninstall() {
    use std::os::unix::fs::PermissionsExt;

    let sb = Sandbox::new();
    let user_script = sb.path("claude").join("my-statusline.sh");
    let seen = sb.path("claude").join("seen-by-user-script.json");
    fs::write(
        &user_script,
        format!(
            "#!/bin/sh\ncat > '{}'\nprintf 'user line'\n",
            seen.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&user_script, fs::Permissions::from_mode(0o755)).unwrap();

    let original = json!({
        "model": "opus",
        "statusLine": {"type": "command", "command": user_script.to_string_lossy(), "padding": 1},
        "env": {"KEEP": "me"}
    });
    let original_bytes = {
        let mut b = serde_json::to_vec_pretty(&original).unwrap();
        b.push(b'\n');
        b
    };
    fs::write(sb.settings(), &original_bytes).unwrap();

    let exe = sb.exe();
    let exe_arg = exe.to_string_lossy().into_owned();

    // Dry run: prints the plan, writes nothing.
    let out = sb.manage(&["install", "--exe", &exe_arg]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("DRY RUN"), "{text}");
    assert!(text.contains("--yes"), "{text}");
    assert!(text.contains(&exe_arg), "{text}");
    assert_eq!(fs::read(sb.settings()).unwrap(), original_bytes);
    assert!(!sb.path("config").join("chain.json").exists());

    // Status before.
    let out = sb.manage(&["status", "--json"]);
    assert_eq!(read_status(&out)["state"], "notInstalled");

    // Apply.
    let out = sb.manage(&["install", "--exe", &exe_arg, "--yes"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let after = read_json(&sb.settings());
    assert_eq!(after["statusLine"]["command"], exe_arg.as_str());
    assert_eq!(after["statusLine"]["padding"], 1);
    assert_eq!(after["env"], json!({"KEEP": "me"}));
    assert_eq!(after["model"], "opus");
    let backups: Vec<_> = fs::read_dir(sb.path("claude"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.to_string_lossy()
                .contains("settings.json.cooldown-backup-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).unwrap(), original_bytes);

    let status = read_status(&sb.manage(&["status", "--json"]));
    assert_eq!(status["state"], "installed");
    assert_eq!(
        status["chainedCommand"],
        user_script.to_string_lossy().as_ref()
    );

    // Idempotent.
    let out = sb.manage(&["install", "--exe", &exe_arg, "--yes"]);
    assert!(stdout(&out).contains("already installed"));

    // The installed command works as Claude Code would run it: through sh,
    // with the JSON on stdin. The user's script must see the same bytes and
    // its output must come back untouched.
    let command = after["statusLine"]["command"].as_str().unwrap();
    let mut child = sb
        .sh_command(command)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(SAMPLE.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "user line");
    assert_eq!(fs::read_to_string(&seen).unwrap(), SAMPLE);
    assert_eq!(
        read_json(&sb.inbox_file())["fiveHour"]["usedPercentage"],
        42.0
    );

    // Uninstall: dry run first, then for real.
    let out = sb.manage(&["uninstall"]);
    assert!(stdout(&out).contains("DRY RUN"));
    assert_eq!(
        read_json(&sb.settings())["statusLine"]["command"],
        exe_arg.as_str()
    );
    let out = sb.manage(&["uninstall", "--yes"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(fs::read(sb.settings()).unwrap(), original_bytes);
    assert!(!sb.path("config").join("chain.json").exists());
    assert_eq!(
        read_status(&sb.manage(&["status", "--json"]))["state"],
        "notInstalled"
    );
}

fn read_status(out: &Output) -> Value {
    assert_eq!(out.status.code(), Some(0), "{}", stderr(out));
    serde_json::from_str(&stdout(out)).unwrap()
}

#[test]
fn install_refuses_invalid_json_and_touches_nothing() {
    let sb = Sandbox::new();
    let garbage = "{ not: valid json }";
    fs::write(sb.settings(), garbage).unwrap();
    let exe = sb.exe().to_string_lossy().into_owned();

    for args in [
        vec!["install", "--exe", &exe, "--yes"],
        vec!["install", "--exe", &exe],
        vec!["uninstall", "--yes"],
        vec!["status"],
    ] {
        let out = sb.manage(&args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(stderr(&out).contains("not valid JSON"), "{}", stderr(&out));
    }
    assert_eq!(fs::read_to_string(sb.settings()).unwrap(), garbage);
    assert!(!sb.path("config").join("chain.json").exists());
    let leftovers = fs::read_dir(sb.path("claude")).unwrap().count();
    assert_eq!(leftovers, 1, "no backup or temp files");
}

#[test]
fn install_refuses_a_binary_with_the_wrong_name() {
    let sb = Sandbox::new();
    fs::write(sb.settings(), "{}").unwrap();
    let wrong = sb.path("bin").join("something-else");
    fs::copy(sb.exe(), &wrong).unwrap();
    let out = sb.manage(&["install", "--exe", &wrong.to_string_lossy(), "--yes"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("must be named"));
    assert_eq!(fs::read_to_string(sb.settings()).unwrap(), "{}");
}

#[test]
fn uninstall_refuses_a_statusline_someone_else_changed() {
    let sb = Sandbox::new();
    fs::write(
        sb.settings(),
        r#"{"statusLine":{"type":"command","command":"old.sh"}}"#,
    )
    .unwrap();
    let exe = sb.exe().to_string_lossy().into_owned();
    assert_eq!(
        sb.manage(&["install", "--exe", &exe, "--yes"])
            .status
            .code(),
        Some(0)
    );

    let theirs = r#"{"statusLine":{"type":"command","command":"theirs.sh"}}"#;
    fs::write(sb.settings(), theirs).unwrap();
    let out = sb.manage(&["uninstall", "--yes"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("theirs.sh"), "{}", stderr(&out));
    assert_eq!(fs::read_to_string(sb.settings()).unwrap(), theirs);
    assert_eq!(
        read_status(&sb.manage(&["status", "--json"]))["state"],
        "modified"
    );
}
