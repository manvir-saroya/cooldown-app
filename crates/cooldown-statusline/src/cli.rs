//! Command-line entry point: hook mode (no arguments) plus `install`,
//! `uninstall` and `status`.
//!
//! Exit codes: 0 success, 1 the operation failed or was refused, 2 bad usage.
//! Hook mode always exits 0 unless it is returning a chained command's status.

use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::panic;
use std::path::PathBuf;

use time::OffsetDateTime;

use crate::command::{command_for_exe, default_flavor, detect_git_bash, ShellFlavor, BIN_STEM};
use crate::hook::{self, HookContext};
use crate::install::{self, Target};
use crate::parse::Observation;
use crate::paths;

const USAGE: &str = "\
cooldown-statusline: captures Claude Code's rate limits for Cooldown

USAGE:
  cooldown-statusline                 Hook mode. Claude Code runs this on every
                                       status line update (reads JSON on stdin).
  cooldown-statusline install         Show what installing would change (dry run).
  cooldown-statusline install --yes   Install into Claude's settings file.
  cooldown-statusline uninstall [--yes]
                                       Restore the previous statusLine.
  cooldown-statusline status [--json] Report installed / not installed / modified.
  cooldown-statusline antigravity     Hook mode for Antigravity CLI (records its
                                       quota object instead of Claude's limits).

OPTIONS:
  --agent <claude|antigravity>
                      Which agent to install into (install, uninstall, status;
                      default: claude). Antigravity uses
                      ~/.gemini/antigravity-cli/settings.json.
  --yes, -y           Actually write. Without it install and uninstall only print.
  --settings <path>   Claude settings file (default: ~/.claude/settings.json).
  --exe <path>        Binary to register (install only; default: this binary).
  --shell <bash|powershell>
                      Command syntax to write (install only; default: bash,
                      or powershell on Windows without Git Bash).
  --json              Machine-readable output (status only).
  -h, --help          Show this help.
  -V, --version       Show the version.

ENVIRONMENT:
  COOLDOWN_INBOX_DIR    Where claude-statusline.json is written.
  COOLDOWN_CONFIG_DIR   Where chain.json is kept.
";

/// A parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cli {
    Hook,
    /// Hook mode for Antigravity CLI.
    AntigravityHook,
    Install(Options),
    Uninstall(Options),
    Status(Options),
    Help,
    Version,
}

/// The agent whose settings file install, uninstall and status work on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Agent {
    #[default]
    Claude,
    Antigravity,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    pub agent: Agent,
    pub yes: bool,
    pub settings: Option<PathBuf>,
    pub exe: Option<PathBuf>,
    pub shell: Option<ShellFlavor>,
    pub json: bool,
}

fn take_value(
    arg: &str,
    inline: Option<&str>,
    rest: &mut std::slice::Iter<'_, OsString>,
) -> Result<OsString, String> {
    match inline {
        Some(v) => Ok(OsString::from(v)),
        None => rest
            .next()
            .cloned()
            .ok_or_else(|| format!("{arg} needs a value")),
    }
}

/// Parse the arguments after the program name.
pub fn parse_args(args: &[OsString]) -> Result<Cli, String> {
    let mut iter = args.iter();
    let Some(first) = iter.next() else {
        return Ok(Cli::Hook);
    };
    let first = first
        .to_str()
        .ok_or_else(|| "arguments must be valid UTF-8".to_owned())?;
    let kind = match first {
        "hook" => "hook",
        crate::antigravity::SUBCOMMAND => {
            if iter.next().is_some() {
                return Err("`antigravity` takes no options".to_owned());
            }
            return Ok(Cli::AntigravityHook);
        }
        "install" => "install",
        "uninstall" => "uninstall",
        "status" => "status",
        "-h" | "--help" | "help" => return Ok(Cli::Help),
        "-V" | "--version" => return Ok(Cli::Version),
        other => return Err(format!("unknown command `{other}`")),
    };

    let mut opts = Options::default();
    while let Some(arg) = iter.next() {
        let text = arg
            .to_str()
            .ok_or_else(|| "arguments must be valid UTF-8".to_owned())?;
        let (flag, inline) = match text.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v)),
            _ => (text, None),
        };
        match (kind, flag) {
            (_, "-h" | "--help") => return Ok(Cli::Help),
            ("install" | "uninstall", "--yes" | "-y") if inline.is_none() => opts.yes = true,
            ("install" | "uninstall" | "status", "--agent") => {
                let value = take_value(flag, inline, &mut iter)?;
                opts.agent = match value.to_str() {
                    Some("claude") => Agent::Claude,
                    Some("antigravity") => Agent::Antigravity,
                    _ => return Err("--agent must be `claude` or `antigravity`".to_owned()),
                };
            }
            ("install" | "uninstall" | "status", "--settings") => {
                opts.settings = Some(PathBuf::from(take_value(flag, inline, &mut iter)?));
            }
            ("install", "--exe") => {
                opts.exe = Some(PathBuf::from(take_value(flag, inline, &mut iter)?));
            }
            ("install", "--shell") => {
                let value = take_value(flag, inline, &mut iter)?;
                opts.shell = Some(match value.to_str() {
                    Some("bash" | "sh" | "posix") => ShellFlavor::Posix,
                    Some("powershell" | "pwsh") => ShellFlavor::PowerShell,
                    _ => return Err("--shell must be `bash` or `powershell`".to_owned()),
                });
            }
            ("status", "--json") if inline.is_none() => opts.json = true,
            _ => return Err(format!("unexpected argument `{text}` for `{kind}`")),
        }
    }

    Ok(match kind {
        "install" => Cli::Install(opts),
        "uninstall" => Cli::Uninstall(opts),
        "status" => Cli::Status(opts),
        _ => {
            if opts != Options::default() {
                return Err("`hook` takes no options".to_owned());
            }
            Cli::Hook
        }
    })
}

/// Run the program for `args` (without the program name) and return the exit
/// code.
pub fn run(args: &[OsString]) -> i32 {
    match parse_args(args) {
        Ok(Cli::Hook) => hook_main(false),
        Ok(Cli::AntigravityHook) => hook_main(true),
        Ok(Cli::Install(o)) => finish(install_cmd(&o)),
        Ok(Cli::Uninstall(o)) => finish(uninstall_cmd(&o)),
        Ok(Cli::Status(o)) => finish(status_cmd(&o)),
        Ok(Cli::Help) => {
            print!("{USAGE}");
            0
        }
        Ok(Cli::Version) => {
            println!("{BIN_STEM} {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Err(message) => {
            eprintln!("error: {message}\n\n{USAGE}");
            2
        }
    }
}

fn finish(result: Result<String, String>) -> i32 {
    match result {
        Ok(text) => {
            let mut out = io::stdout().lock();
            let _ = out.write_all(text.as_bytes());
            if !text.ends_with('\n') {
                let _ = out.write_all(b"\n");
            }
            let _ = out.flush();
            0
        }
        Err(message) => {
            eprintln!("error: {message}");
            1
        }
    }
}

/// Hook mode. Cannot fail the user's status line: stdin trouble, inbox errors,
/// chain errors and even panics all end in a printed line and a clean exit.
fn hook_main(antigravity: bool) -> i32 {
    panic::set_hook(Box::new(|_| {}));
    let result = panic::catch_unwind(|| {
        let input = if io::stdin().is_terminal() {
            Vec::new()
        } else {
            hook::read_stdin()
        };
        let stdout = io::stdout();
        let mut out = stdout.lock();
        if antigravity {
            crate::antigravity::run(&input, &crate::antigravity::context_from_env(), &mut out)
        } else {
            hook::run(&input, &HookContext::from_env(), &mut out)
        }
    });
    result.unwrap_or_else(|_| {
        let line = if antigravity {
            crate::antigravity::default_line(None)
        } else {
            hook::default_line(&Observation::default())
        };
        let _ = writeln!(io::stdout(), "{line}");
        0
    })
}

fn resolve_target(agent: Agent, settings: Option<PathBuf>) -> Result<Target, String> {
    let (default_settings, chain) = match agent {
        Agent::Claude => (paths::default_settings_path(), paths::chain_config_path()),
        Agent::Antigravity => (
            crate::antigravity::default_settings_path(),
            crate::antigravity::chain_config_path(),
        ),
    };
    let settings = settings
        .or(default_settings)
        .ok_or("cannot find your home directory; pass --settings <path>")?;
    let settings = std::path::absolute(&settings).unwrap_or(settings);
    let chain = chain.ok_or("cannot find a config directory; set COOLDOWN_CONFIG_DIR")?;
    Ok(Target { settings, chain })
}

/// The registered command for `agent`: the binary, plus the `antigravity` subcommand for that agent.
pub fn command_for_agent(base: String, agent: Agent) -> String {
    match agent {
        Agent::Claude => base,
        Agent::Antigravity => format!("{base} {}", crate::antigravity::SUBCOMMAND),
    }
}

/// The command string to register for the binary at `exe` (default: this one).
fn resolve_command(exe: Option<PathBuf>, shell: Option<ShellFlavor>) -> Result<String, String> {
    let exe = match exe {
        Some(p) => p,
        None => std::env::current_exe().map_err(|e| format!("cannot locate this binary: {e}"))?,
    };
    let exe = std::path::absolute(&exe).unwrap_or(exe);
    if !exe.is_file() {
        return Err(format!("{} does not exist", exe.display()));
    }
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase());
    if stem.as_deref() != Some(BIN_STEM) {
        return Err(format!(
            "the registered binary must be named `{BIN_STEM}` (got {}); \
             Cooldown recognises its own entry by that name",
            exe.display()
        ));
    }
    let exe_text = exe
        .to_str()
        .ok_or("the binary path is not valid UTF-8, so it cannot go into settings.json")?;
    let windows = cfg!(windows);
    let flavor =
        shell.unwrap_or_else(|| default_flavor(windows, windows && detect_git_bash().is_some()));
    Ok(command_for_exe(exe_text, windows, flavor))
}

fn install_cmd(opts: &Options) -> Result<String, String> {
    let target = resolve_target(opts.agent, opts.settings.clone())?;
    let command = command_for_agent(resolve_command(opts.exe.clone(), opts.shell)?, opts.agent);
    install::install(&target, &command, opts.yes, OffsetDateTime::now_utc())
        .map(|r| r.message)
        .map_err(|e| e.to_string())
}

fn uninstall_cmd(opts: &Options) -> Result<String, String> {
    let target = resolve_target(opts.agent, opts.settings.clone())?;
    install::uninstall(&target, opts.yes, OffsetDateTime::now_utc())
        .map(|r| r.message)
        .map_err(|e| e.to_string())
}

fn status_cmd(opts: &Options) -> Result<String, String> {
    let target = resolve_target(opts.agent, opts.settings.clone())?;
    let ours = resolve_command(None, None)
        .ok()
        .map(|c| command_for_agent(c, opts.agent));
    let report = install::status(&target, ours.as_deref()).map_err(|e| e.to_string())?;
    if opts.json {
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
    } else {
        Ok(report.to_text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, String> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        parse_args(&args)
    }

    #[test]
    fn no_arguments_means_hook_mode() {
        assert_eq!(parse(&[]), Ok(Cli::Hook));
        assert_eq!(parse(&["hook"]), Ok(Cli::Hook));
    }

    #[test]
    fn help_and_version() {
        assert_eq!(parse(&["--help"]), Ok(Cli::Help));
        assert_eq!(parse(&["-h"]), Ok(Cli::Help));
        assert_eq!(parse(&["install", "--help"]), Ok(Cli::Help));
        assert_eq!(parse(&["--version"]), Ok(Cli::Version));
    }

    #[test]
    fn install_is_a_dry_run_unless_yes() {
        let Ok(Cli::Install(o)) = parse(&["install"]) else {
            panic!("expected install")
        };
        assert!(!o.yes);
        let Ok(Cli::Install(o)) = parse(&["install", "--yes"]) else {
            panic!("expected install")
        };
        assert!(o.yes);
        let Ok(Cli::Install(o)) = parse(&["install", "-y"]) else {
            panic!("expected install")
        };
        assert!(o.yes);
    }

    #[test]
    fn options_accept_space_and_equals_forms() {
        let Ok(Cli::Install(o)) = parse(&[
            "install",
            "--settings",
            "/a/s.json",
            "--exe=/b/cooldown-statusline",
            "--shell",
            "powershell",
            "--yes",
        ]) else {
            panic!("expected install")
        };
        assert_eq!(o.settings, Some(PathBuf::from("/a/s.json")));
        assert_eq!(o.exe, Some(PathBuf::from("/b/cooldown-statusline")));
        assert_eq!(o.shell, Some(ShellFlavor::PowerShell));
        let Ok(Cli::Status(o)) = parse(&["status", "--settings=/c.json", "--json"]) else {
            panic!("expected status")
        };
        assert_eq!(o.settings, Some(PathBuf::from("/c.json")));
        assert!(o.json);
    }

    #[test]
    fn antigravity_has_its_own_hook_mode_and_agent_option() {
        assert_eq!(parse(&["antigravity"]), Ok(Cli::AntigravityHook));
        assert!(parse(&["antigravity", "--yes"]).is_err());
        let Ok(Cli::Install(o)) = parse(&["install", "--agent", "antigravity", "--yes"]) else {
            panic!("expected install")
        };
        assert_eq!(o.agent, Agent::Antigravity);
        let Ok(Cli::Status(o)) = parse(&["status", "--agent=claude"]) else {
            panic!("expected status")
        };
        assert_eq!(o.agent, Agent::Claude);
        assert!(parse(&["install", "--agent", "cursor"]).is_err());
        assert_eq!(
            command_for_agent("'/x/cooldown-statusline'".into(), Agent::Antigravity),
            "'/x/cooldown-statusline' antigravity"
        );
        assert!(crate::command::is_our_command(
            "'/x/cooldown-statusline' antigravity"
        ));
    }

    #[test]
    fn bad_usage_is_rejected() {
        assert!(parse(&["frobnicate"]).is_err());
        assert!(parse(&["install", "--bogus"]).is_err());
        assert!(parse(&["install", "--settings"]).is_err());
        assert!(parse(&["install", "--shell", "fish"]).is_err());
        assert!(parse(&["uninstall", "--exe", "x"]).is_err());
        assert!(parse(&["status", "--yes"]).is_err());
        assert!(parse(&["hook", "--yes"]).is_err());
        assert!(parse(&["install", "stray"]).is_err());
    }
}
