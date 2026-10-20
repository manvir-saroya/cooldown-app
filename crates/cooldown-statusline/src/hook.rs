//! Hook mode: what runs on every Claude Code statusline update.
//!
//! Order of work, chosen so the user's status line can never be broken:
//!
//! 1. parse stdin tolerantly (never fails);
//! 2. record the observation in the inbox (errors and panics are swallowed);
//! 3. chain to the user's previous statusline if there is one and return its
//!    stdout and exit status, otherwise print a short default line and exit 0.
//!
//! The inbox is written *before* chaining so a cancelled or slow chained
//! command cannot lose the observation.

use std::io::{Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use time::OffsetDateTime;

use crate::chain::{self, ChainShell, ENV_DEPTH};
use crate::inbox::{write_inbox, Inbox};
use crate::parse::{parse_stdin, Observation, MAX_INPUT_BYTES};

/// How long to wait for Claude Code to finish writing stdin before giving up
/// and working with whatever arrived.
pub const STDIN_TIMEOUT: Duration = Duration::from_millis(1000);

/// Everything the hook needs from its environment, injectable for tests.
#[derive(Debug, Clone)]
pub struct HookContext {
    /// Directory for `claude-statusline.json`; `None` disables recording.
    pub inbox_dir: Option<PathBuf>,
    /// Path of `chain.json`; `None` disables chaining.
    pub chain_path: Option<PathBuf>,
    pub shell: ChainShell,
    pub now: OffsetDateTime,
    /// True when we were started by our own chained child (loop guard).
    pub nested: bool,
}

impl HookContext {
    /// The context for a real run on this machine.
    pub fn from_env() -> Self {
        Self {
            inbox_dir: crate::paths::inbox_dir(),
            chain_path: crate::paths::chain_config_path(),
            shell: ChainShell::detect(),
            now: OffsetDateTime::now_utc(),
            nested: std::env::var_os(ENV_DEPTH).is_some_and(|v| !v.is_empty()),
        }
    }
}

/// Read at most `cap` bytes from `reader`, giving up after `timeout`.
///
/// Reading happens on a helper thread so a stdin that never closes cannot hang
/// the hook; whatever arrived before the deadline is returned.
pub fn read_capped<R: Read + Send + 'static>(
    mut reader: R,
    cap: usize,
    timeout: Duration,
) -> Vec<u8> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });

    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    while out.len() < cap {
        let remaining = deadline.saturating_duration_since(Instant::now());
        // `Err` is either EOF (sender dropped) or the deadline passing.
        let Ok(chunk) = rx.recv_timeout(remaining) else {
            break;
        };
        let room = cap - out.len();
        out.extend_from_slice(&chunk[..chunk.len().min(room)]);
    }
    out
}

/// Read the hook's stdin with the standard cap and timeout.
pub fn read_stdin() -> Vec<u8> {
    read_capped(std::io::stdin(), MAX_INPUT_BYTES, STDIN_TIMEOUT)
}

/// The default status line, used when there is nothing to chain to:
/// `5h 42% · 7d 18%`, with `--` for a window that is not reported.
pub fn default_line(obs: &Observation) -> String {
    let part = |label: &str, window: &Option<crate::parse::Window>| {
        let value = window.as_ref().and_then(|w| w.used_percentage).map_or_else(
            || "--".to_owned(),
            |p| format!("{}%", p.clamp(0.0, 100.0).round()),
        );
        format!("{label} {value}")
    };
    format!(
        "{} · {}",
        part("5h", &obs.five_hour),
        part("7d", &obs.seven_day)
    )
}

/// Write the observation to the inbox. Does nothing when the input carried no
/// `rate_limits` object: that means "no information" (an API-key user, or a
/// session that has not had its first response yet), and overwriting the file
/// would erase the last good reading.
fn record(obs: &Observation, ctx: &HookContext) {
    if !obs.rate_limits_present {
        return;
    }
    let Some(dir) = &ctx.inbox_dir else {
        return;
    };
    let _ = write_inbox(dir, &Inbox::from_observation(obs, ctx.now));
}

fn chained_command(ctx: &HookContext) -> Option<String> {
    if ctx.nested {
        return None;
    }
    let config = chain::load(ctx.chain_path.as_ref()?)?;
    config.original_command().map(str::to_owned)
}

/// Run the hook for the given stdin bytes, writing the status line to `out`.
/// Returns the process exit code. Never panics and never fails the status line:
/// every internal error falls through to the default line and exit code 0.
pub fn run(input: &[u8], ctx: &HookContext, out: &mut dyn Write) -> i32 {
    let obs = parse_stdin(input);

    let _ = panic::catch_unwind(AssertUnwindSafe(|| record(&obs, ctx)));

    let chained = panic::catch_unwind(AssertUnwindSafe(|| {
        let command = chained_command(ctx)?;
        chain::run_chain(&ctx.shell, &command, input).ok()
    }))
    .ok()
    .flatten();

    if let Some(output) = chained {
        let _ = out.write_all(&output.stdout);
        let _ = out.flush();
        return output.code;
    }

    let _ = writeln!(out, "{}", default_line(&obs));
    let _ = out.flush();
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::chain::{save, ChainConfig, CHAIN_VERSION};
    use crate::inbox::INBOX_FILE_NAME;
    use crate::parse::Window;
    #[cfg(unix)]
    use serde_json::json;
    use std::fs;
    use time::macros::datetime;

    const SAMPLE: &[u8] = br#"{"version":"2.1.90","rate_limits":{
        "five_hour":{"used_percentage":23.5,"resets_at":1738425600},
        "seven_day":{"used_percentage":41.2,"resets_at":1738857600}}}"#;

    struct Env {
        _tmp: tempfile::TempDir,
        ctx: HookContext,
    }

    fn env() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = HookContext {
            inbox_dir: Some(tmp.path().join("inbox")),
            chain_path: Some(tmp.path().join("cfg").join("chain.json")),
            shell: ChainShell::for_platform(false, None),
            now: datetime!(2026-10-10 12:00:00 UTC),
            nested: false,
        };
        Env { _tmp: tmp, ctx }
    }

    // Used only by the Unix-only `chaining` tests below.
    #[cfg(unix)]
    fn save_chain(ctx: &HookContext, original: Option<serde_json::Value>) {
        save(
            ctx.chain_path.as_ref().unwrap(),
            &ChainConfig {
                version: CHAIN_VERSION,
                settings_path: "/s.json".into(),
                installed_at: "2026-10-10T00:00:00Z".into(),
                installed_status_line: json!({"type":"command","command":"cooldown-statusline"}),
                original,
            },
        )
        .unwrap();
    }

    fn run_hook(input: &[u8], ctx: &HookContext) -> (i32, String) {
        let mut out = Vec::new();
        let code = run(input, ctx, &mut out);
        (code, String::from_utf8_lossy(&out).into_owned())
    }

    fn inbox_file(ctx: &HookContext) -> std::path::PathBuf {
        ctx.inbox_dir.as_ref().unwrap().join(INBOX_FILE_NAME)
    }

    #[test]
    fn default_line_formats_both_windows() {
        let obs = parse_stdin(SAMPLE);
        assert_eq!(default_line(&obs), "5h 24% · 7d 41%");
        let obs = parse_stdin(br#"{"rate_limits":{"five_hour":{"used_percentage":42,"resets_at":1738425600},"seven_day":{"used_percentage":18,"resets_at":1738857600}}}"#);
        assert_eq!(default_line(&obs), "5h 42% · 7d 18%");
    }

    #[test]
    fn default_line_marks_missing_windows() {
        assert_eq!(default_line(&Observation::default()), "5h -- · 7d --");
        let obs = Observation {
            five_hour: Some(Window {
                used_percentage: Some(100.0),
                resets_at: None,
            }),
            ..Observation::default()
        };
        assert_eq!(default_line(&obs), "5h 100% · 7d --");
    }

    #[test]
    fn records_inbox_and_prints_default_line_without_a_chain() {
        let e = env();
        let (code, out) = run_hook(SAMPLE, &e.ctx);
        assert_eq!(code, 0);
        assert_eq!(out, "5h 24% · 7d 41%\n");
        let inbox: Inbox = serde_json::from_slice(&fs::read(inbox_file(&e.ctx)).unwrap()).unwrap();
        assert_eq!(inbox.observed_at, "2026-10-10T12:00:00Z");
        assert_eq!(
            inbox.five_hour.unwrap().resets_at.as_deref(),
            Some("2025-02-01T16:00:00Z")
        );
        assert_eq!(inbox.claude_code_version.as_deref(), Some("2.1.90"));
    }

    #[test]
    fn no_rate_limits_keeps_the_last_good_inbox() {
        let e = env();
        run_hook(SAMPLE, &e.ctx);
        let before = fs::read(inbox_file(&e.ctx)).unwrap();

        let mut later = e.ctx.clone();
        later.now = datetime!(2026-10-10 13:00:00 UTC);
        for input in [&b""[..], b"garbage", br#"{"model":{"display_name":"X"}}"#] {
            let (code, out) = run_hook(input, &later);
            assert_eq!(code, 0);
            assert_eq!(out, "5h -- · 7d --\n");
        }
        assert_eq!(fs::read(inbox_file(&e.ctx)).unwrap(), before);
    }

    #[test]
    fn no_rate_limits_never_creates_an_inbox() {
        let e = env();
        run_hook(b"", &e.ctx);
        assert!(!e.ctx.inbox_dir.as_ref().unwrap().exists());
    }

    #[test]
    fn rate_limits_object_without_windows_is_recorded_as_nulls() {
        let e = env();
        run_hook(br#"{"rate_limits":{}}"#, &e.ctx);
        let inbox: Inbox = serde_json::from_slice(&fs::read(inbox_file(&e.ctx)).unwrap()).unwrap();
        assert!(inbox.five_hour.is_none() && inbox.seven_day.is_none());
    }

    #[test]
    fn unwritable_inbox_still_prints_and_exits_zero() {
        let e = env();
        let blocker = e._tmp.path().join("file-not-dir");
        fs::write(&blocker, "x").unwrap();
        let mut ctx = e.ctx.clone();
        ctx.inbox_dir = Some(blocker.join("inbox"));
        let (code, out) = run_hook(SAMPLE, &ctx);
        assert_eq!(code, 0);
        assert_eq!(out, "5h 24% · 7d 41%\n");
    }

    #[test]
    fn disabled_paths_are_fine() {
        let e = env();
        let mut ctx = e.ctx.clone();
        ctx.inbox_dir = None;
        ctx.chain_path = None;
        assert_eq!(run_hook(SAMPLE, &ctx), (0, "5h 24% · 7d 41%\n".to_owned()));
    }

    #[test]
    fn huge_and_garbage_input_never_break_the_line() {
        let e = env();
        let huge = vec![b'{'; 3 * 1024 * 1024];
        assert_eq!(run_hook(&huge, &e.ctx), (0, "5h -- · 7d --\n".to_owned()));
        assert_eq!(run_hook(b"\x00\xff\xfe", &e.ctx).0, 0);
    }

    #[cfg(unix)]
    mod chaining {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn script(dir: &std::path::Path, name: &str, body: &str) -> String {
            let path = dir.join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path.to_string_lossy().into_owned()
        }

        #[test]
        fn chains_to_the_saved_command_and_passes_everything_through() {
            let e = env();
            let seen = e._tmp.path().join("seen");
            let cmd = script(
                e._tmp.path(),
                "user.sh",
                &format!("cat > '{}'\nprintf 'my custom status\\n'", seen.display()),
            );
            save_chain(
                &e.ctx,
                Some(json!({"type":"command","command":cmd,"padding":2})),
            );

            let (code, out) = run_hook(SAMPLE, &e.ctx);
            assert_eq!(code, 0);
            assert_eq!(out, "my custom status\n");
            // The user's script received the original bytes untouched...
            assert_eq!(fs::read(&seen).unwrap(), SAMPLE);
            // ...and the observation was still recorded.
            assert!(inbox_file(&e.ctx).exists());
        }

        #[test]
        fn returns_the_chained_exit_status() {
            let e = env();
            let cmd = script(e._tmp.path(), "fail.sh", "echo partial\nexit 3");
            save_chain(&e.ctx, Some(json!({"type":"command","command":cmd})));
            let (code, out) = run_hook(SAMPLE, &e.ctx);
            assert_eq!(code, 3);
            assert_eq!(out, "partial\n");
            assert!(inbox_file(&e.ctx).exists(), "recorded before chaining");
        }

        #[test]
        fn chained_command_that_prints_nothing_prints_nothing() {
            let e = env();
            let cmd = script(e._tmp.path(), "quiet.sh", "exit 0");
            save_chain(&e.ctx, Some(json!({"type":"command","command":cmd})));
            assert_eq!(run_hook(SAMPLE, &e.ctx), (0, String::new()));
        }

        #[test]
        fn chain_receives_garbage_stdin_byte_for_byte() {
            let e = env();
            let seen = e._tmp.path().join("seen");
            let cmd = script(
                e._tmp.path(),
                "user.sh",
                &format!("cat > '{}'", seen.display()),
            );
            save_chain(&e.ctx, Some(json!({"type":"command","command":cmd})));
            let junk = b"\xff\xfenot json \x00 at all";
            run_hook(junk, &e.ctx);
            assert_eq!(fs::read(&seen).unwrap(), junk);
        }

        #[test]
        fn never_chains_to_itself() {
            let e = env();
            save_chain(
                &e.ctx,
                Some(json!({"type":"command","command":"/opt/cooldown/cooldown-statusline"})),
            );
            assert_eq!(
                run_hook(SAMPLE, &e.ctx),
                (0, "5h 24% · 7d 41%\n".to_owned())
            );
        }

        #[test]
        fn nested_runs_do_not_chain() {
            let e = env();
            let cmd = script(e._tmp.path(), "user.sh", "echo should-not-run");
            save_chain(&e.ctx, Some(json!({"type":"command","command":cmd})));
            let mut ctx = e.ctx.clone();
            ctx.nested = true;
            assert_eq!(run_hook(SAMPLE, &ctx), (0, "5h 24% · 7d 41%\n".to_owned()));
        }

        #[test]
        fn unstartable_shell_falls_back_to_the_default_line() {
            let e = env();
            save_chain(&e.ctx, Some(json!({"type":"command","command":"echo hi"})));
            let mut ctx = e.ctx.clone();
            ctx.shell = ChainShell {
                program: "/definitely/not/a/shell".into(),
                args: vec![],
            };
            assert_eq!(run_hook(SAMPLE, &ctx), (0, "5h 24% · 7d 41%\n".to_owned()));
        }

        #[test]
        fn no_original_or_corrupt_chain_config_means_default_line() {
            let e = env();
            save_chain(&e.ctx, None);
            assert_eq!(run_hook(SAMPLE, &e.ctx).1, "5h 24% · 7d 41%\n");
            fs::write(e.ctx.chain_path.as_ref().unwrap(), "{ broken").unwrap();
            assert_eq!(run_hook(SAMPLE, &e.ctx).1, "5h 24% · 7d 41%\n");
        }
    }

    #[test]
    fn read_capped_returns_everything_below_the_cap() {
        let data = b"hello world".to_vec();
        let got = read_capped(
            std::io::Cursor::new(data.clone()),
            1024,
            Duration::from_secs(2),
        );
        assert_eq!(got, data);
    }

    #[test]
    fn read_capped_truncates_at_the_cap() {
        let data = vec![b'a'; 100_000];
        let got = read_capped(std::io::Cursor::new(data), 4096, Duration::from_secs(2));
        assert_eq!(got.len(), 4096);
    }

    #[test]
    fn read_capped_handles_empty_input() {
        let got = read_capped(std::io::empty(), 1024, Duration::from_secs(2));
        assert!(got.is_empty());
    }

    #[test]
    fn read_capped_gives_up_on_a_stalled_reader_but_keeps_what_arrived() {
        struct Stall(bool);
        impl Read for Stall {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.0 {
                    thread::sleep(Duration::from_secs(30));
                    return Ok(0);
                }
                self.0 = true;
                buf[..5].copy_from_slice(b"first");
                Ok(5)
            }
        }
        let started = Instant::now();
        let got = read_capped(Stall(false), 1024, Duration::from_millis(150));
        assert_eq!(got, b"first");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
