//! Chaining to the statusline the user had before installing Cooldown.
//!
//! `install` saves the previous `statusLine` object into `chain.json`. At run
//! time the hook feeds the same stdin to that command and returns its stdout
//! and exit status, so the user's status line looks exactly as before.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::atomic::write_atomic;
use crate::command::is_our_command;

/// Schema version of `chain.json`.
pub const CHAIN_VERSION: u32 = 1;

/// Set on the chained child. If a wrapper script re-enters the hook, the inner
/// run sees it and does not chain again, which breaks any possible loop.
pub const ENV_DEPTH: &str = "COOLDOWN_STATUSLINE_DEPTH";

/// The saved state needed to chain and to restore the previous statusLine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainConfig {
    pub version: u32,
    /// The settings file this chain belongs to.
    pub settings_path: String,
    /// RFC 3339 UTC.
    pub installed_at: String,
    /// The `statusLine` value Cooldown wrote.
    pub installed_status_line: Value,
    /// The user's `statusLine` value before install, or `null` if there was
    /// none.
    #[serde(default)]
    pub original: Option<Value>,
}

impl ChainConfig {
    /// The command to chain to: the saved original's `command`, unless there is
    /// none, it is not a command-type entry, or it is Cooldown itself.
    pub fn original_command(&self) -> Option<&str> {
        let original = self.original.as_ref()?.as_object()?;
        if let Some(kind) = original.get("type") {
            if kind.as_str() != Some("command") {
                return None;
            }
        }
        let command = original.get("command")?.as_str()?;
        (!command.trim().is_empty() && !is_our_command(command)).then_some(command)
    }
}

/// Read `chain.json`; any failure (missing, unreadable, malformed) is `None` so
/// the hook degrades to "no chain" instead of failing.
pub fn load(path: &Path) -> Option<ChainConfig> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Write `chain.json` atomically, creating its directory.
pub fn save(path: &Path, config: &ChainConfig) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut bytes = serde_json::to_vec_pretty(config)?;
    bytes.push(b'\n');
    write_atomic(path, &bytes)
}

/// Delete `chain.json`; an already-missing file is success.
pub fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A program and its leading arguments; the command string is appended as the
/// final argument (`sh -c <command>`).
#[derive(Debug, Clone)]
pub struct ChainShell {
    pub program: OsString,
    pub args: Vec<OsString>,
}

impl ChainShell {
    /// `sh -c`.
    pub fn posix() -> Self {
        Self {
            program: "sh".into(),
            args: vec!["-c".into()],
        }
    }

    /// Git Bash's `bash.exe -c`.
    pub fn git_bash(bash: PathBuf) -> Self {
        Self {
            program: bash.into_os_string(),
            args: vec!["-c".into()],
        }
    }

    /// `powershell -NoProfile -NonInteractive -Command`.
    pub fn powershell() -> Self {
        Self {
            program: "powershell".into(),
            args: vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
            ],
        }
    }

    /// Pick the shell the way Claude Code does: POSIX `sh` off Windows; on
    /// Windows Git Bash when found, otherwise PowerShell.
    pub fn for_platform(windows: bool, git_bash: Option<PathBuf>) -> Self {
        match (windows, git_bash) {
            (false, _) => Self::posix(),
            (true, Some(bash)) => Self::git_bash(bash),
            (true, None) => Self::powershell(),
        }
    }

    /// [`Self::for_platform`] for the machine we are running on.
    pub fn detect() -> Self {
        let windows = cfg!(windows);
        let bash = windows.then(crate::command::detect_git_bash).flatten();
        Self::for_platform(windows, bash)
    }
}

/// What the chained command produced.
#[derive(Debug)]
pub struct ChainOutput {
    pub stdout: Vec<u8>,
    /// The child's exit code (1 if it was killed by a signal).
    pub code: i32,
}

/// Run `command` through `shell`, feeding it `stdin` verbatim.
///
/// stdout is captured so the caller can pass it through; stderr is inherited
/// (Claude Code shows it under `--debug`). An `Err` means the shell could not
/// be started at all; a command that runs and fails is `Ok` with its code.
pub fn run_chain(shell: &ChainShell, command: &str, stdin: &[u8]) -> io::Result<ChainOutput> {
    let mut child = Command::new(&shell.program)
        .args(&shell.args)
        .arg(command)
        .env(ENV_DEPTH, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    // Write on a separate thread so a child that never reads cannot deadlock
    // us; a broken pipe (child exited early) is fine.
    let writer = child.stdin.take().map(|mut pipe| {
        let data = stdin.to_vec();
        thread::spawn(move || {
            let _ = pipe.write_all(&data);
        })
    });
    let output = child.wait_with_output()?;
    if let Some(handle) = writer {
        let _ = handle.join();
    }
    Ok(ChainOutput {
        stdout: output.stdout,
        code: output.status.code().unwrap_or(1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(original: Option<Value>) -> ChainConfig {
        ChainConfig {
            version: CHAIN_VERSION,
            settings_path: "/s.json".into(),
            installed_at: "2026-10-10T00:00:00Z".into(),
            installed_status_line: json!({"type": "command", "command": "cooldown-statusline"}),
            original,
        }
    }

    #[test]
    fn original_command_requires_a_real_foreign_command() {
        let cmd = |v: Value| config(Some(v)).original_command().map(str::to_owned);
        assert_eq!(
            cmd(json!({"type": "command", "command": "~/s.sh", "padding": 2})).as_deref(),
            Some("~/s.sh")
        );
        assert_eq!(cmd(json!({"command": "~/s.sh"})).as_deref(), Some("~/s.sh"));
        assert_eq!(cmd(json!({"type": "static", "command": "x"})), None);
        assert_eq!(cmd(json!({"type": "command", "command": "  "})), None);
        assert_eq!(cmd(json!({"type": "command"})), None);
        assert_eq!(cmd(json!("just a string")), None);
        assert_eq!(config(None).original_command(), None);
    }

    #[test]
    fn never_chains_to_ourselves() {
        let ours = json!({"type": "command", "command": "/opt/r/cooldown-statusline"});
        assert_eq!(config(Some(ours)).original_command(), None);
    }

    #[test]
    fn save_load_round_trip_and_null_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg").join("chain.json");
        let with = config(Some(
            json!({"type": "command", "command": "x", "padding": 1}),
        ));
        save(&path, &with).unwrap();
        assert_eq!(load(&path), Some(with));
        let without = config(None);
        save(&path, &without).unwrap();
        assert_eq!(load(&path), Some(without));
        remove(&path).unwrap();
        remove(&path).unwrap();
        assert_eq!(load(&path), None);
    }

    #[test]
    fn load_is_tolerant() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chain.json");
        assert_eq!(load(&path), None);
        fs::write(&path, "not json").unwrap();
        assert_eq!(load(&path), None);
        fs::write(&path, "{}").unwrap();
        assert_eq!(load(&path), None);
    }

    #[test]
    fn shell_choice_per_platform() {
        assert_eq!(ChainShell::for_platform(false, None).program, "sh");
        assert_eq!(
            ChainShell::for_platform(false, Some("/b/bash.exe".into())).program,
            "sh"
        );
        assert_eq!(
            ChainShell::for_platform(true, Some("C:/Git/bin/bash.exe".into())).program,
            "C:/Git/bin/bash.exe"
        );
        let ps = ChainShell::for_platform(true, None);
        assert_eq!(ps.program, "powershell");
        assert_eq!(ps.args.last().unwrap(), "-Command");
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn script(dir: &Path, body: &str) -> String {
            let path = dir.join("chained.sh");
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path.to_string_lossy().into_owned()
        }

        #[test]
        fn passes_stdin_through_and_returns_stdout() {
            let dir = tempfile::tempdir().unwrap();
            let seen = dir.path().join("seen.json");
            let cmd = script(
                dir.path(),
                &format!("cat > '{}'\nprintf 'custom line'", seen.display()),
            );
            let input = br#"{"rate_limits":{"five_hour":{"used_percentage":1}}}"#;
            let out = run_chain(&ChainShell::posix(), &cmd, input).unwrap();
            assert_eq!(out.stdout, b"custom line");
            assert_eq!(out.code, 0);
            assert_eq!(fs::read(&seen).unwrap(), input);
        }

        #[test]
        fn returns_the_childs_exit_code() {
            let out = run_chain(&ChainShell::posix(), "echo bye; exit 7", b"").unwrap();
            assert_eq!(out.code, 7);
            assert_eq!(out.stdout, b"bye\n");
        }

        #[test]
        fn unknown_command_is_ok_with_failure_code() {
            let out = run_chain(&ChainShell::posix(), "definitely-not-a-command-xyz", b"").unwrap();
            assert_ne!(out.code, 0);
        }

        #[test]
        fn missing_shell_is_an_error() {
            let shell = ChainShell {
                program: "/definitely/not/a/shell".into(),
                args: vec![],
            };
            assert!(run_chain(&shell, "echo hi", b"").is_err());
        }

        #[test]
        fn child_that_ignores_stdin_does_not_deadlock() {
            let big = vec![b'x'; 900_000];
            let out = run_chain(&ChainShell::posix(), "echo done", &big).unwrap();
            assert_eq!(out.stdout, b"done\n");
        }

        #[test]
        fn child_sees_the_recursion_guard() {
            let out = run_chain(
                &ChainShell::posix(),
                &format!("printf %s \"${ENV_DEPTH}\""),
                b"",
            )
            .unwrap();
            assert_eq!(out.stdout, b"1");
        }

        #[test]
        fn killed_child_reports_failure() {
            let out = run_chain(&ChainShell::posix(), "kill -9 $$", b"").unwrap();
            assert_eq!(out.code, 1);
        }
    }

    #[cfg(windows)]
    mod windows {
        use super::*;

        fn cmd_shell() -> ChainShell {
            ChainShell {
                program: "cmd".into(),
                args: vec!["/C".into()],
            }
        }

        #[test]
        fn passes_stdin_through_and_returns_stdout() {
            // `findstr /R .` echoes every non-empty line read from stdin (no
            // quotes, which cmd.exe would not unescape the way Rust writes them).
            let out = run_chain(&cmd_shell(), "findstr /R .", b"hello from stdin\r\n").unwrap();
            assert!(String::from_utf8_lossy(&out.stdout).contains("hello from stdin"));
            assert_eq!(out.code, 0);
        }

        #[test]
        fn returns_the_childs_exit_code() {
            let out = run_chain(&cmd_shell(), "exit /b 7", b"").unwrap();
            assert_eq!(out.code, 7);
        }
    }
}
