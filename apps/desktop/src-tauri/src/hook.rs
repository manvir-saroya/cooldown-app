//! The Claude Code statusline hook, driven from Settings.
//!
//! The hook binary (`cooldown-statusline`) ships next to the app executable as a Tauri sidecar.
//! Installing edits the user's real `~/.claude/settings.json` (with a backup of the old
//! `statusLine` entry), so it only ever runs when the user presses Install and confirms.

use std::path::{Path, PathBuf};

use cooldown_statusline::command::{command_for_exe, default_flavor, detect_git_bash, BIN_STEM};
use cooldown_statusline::install::{self, State, StatusReport, Target};
use cooldown_statusline::paths;
use time::OffsetDateTime;

use crate::views::{iso, HookStatus};

/// Where the sidecar lives: beside the running executable.
pub fn sidecar_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe
        .parent()?
        .join(format!("{BIN_STEM}{}", std::env::consts::EXE_SUFFIX));
    path.is_file().then_some(path)
}

/// The agents whose status line Cooldown can hook into.
pub use cooldown_statusline::cli::Agent;

/// The agent named by the UI (`claude` or `antigravity`).
pub fn parse_agent(name: &str) -> Result<Agent, String> {
    match name {
        "claude" => Ok(Agent::Claude),
        "antigravity" => Ok(Agent::Antigravity),
        _ => Err("Unknown agent.".into()),
    }
}

fn target(agent: Agent) -> Result<Target, String> {
    let (settings, chain) = match agent {
        Agent::Claude => (paths::default_settings_path(), paths::chain_config_path()),
        Agent::Antigravity => (
            cooldown_statusline::antigravity::default_settings_path(),
            cooldown_statusline::antigravity::chain_config_path(),
        ),
    };
    let settings = settings
        .ok_or("Cannot find your home folder, so the agent's settings file cannot be located.")?;
    let settings = std::path::absolute(&settings).unwrap_or(settings);
    let chain = chain.ok_or("Cannot find a configuration folder for Cooldown.")?;
    Ok(Target { settings, chain })
}

/// The exact `statusLine.command` for the sidecar at `exe`, for `agent`.
fn command(exe: &Path, agent: Agent) -> Result<String, String> {
    let text = exe
        .to_str()
        .ok_or("The app's folder path is not valid UTF-8, so it cannot go into settings.json.")?;
    let windows = cfg!(windows);
    let flavor = default_flavor(windows, windows && detect_git_bash().is_some());
    Ok(cooldown_statusline::cli::command_for_agent(
        command_for_exe(text, windows, flavor),
        agent,
    ))
}

/// The inbox file the hook writes for `agent` and its adapter reads.
pub fn inbox_path(agent: Agent) -> PathBuf {
    match agent {
        Agent::Claude => cooldown_adapters::claude::default_inbox_path(),
        Agent::Antigravity => cooldown_adapters::antigravity::default_inbox_path(),
    }
}

/// Map the statusline crate's report to the UI's `HookStatus`.
pub fn to_view(
    report: &StatusReport,
    inbox: &Path,
    last_data_at: Option<OffsetDateTime>,
) -> HookStatus {
    let installed = report.state == State::Installed;
    HookStatus {
        installed,
        chains_existing: installed && report.chained_command.is_some(),
        settings_path: report.settings_path.clone(),
        // The previous statusLine command is kept in chain.json and restored on uninstall.
        backup_path: (installed && report.chained_command.is_some())
            .then(|| report.chain_path.clone()),
        inbox_path: inbox.display().to_string(),
        last_data_at: last_data_at.map(iso),
    }
}

fn modified_at(path: &Path) -> Option<OffsetDateTime> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(OffsetDateTime::from(modified))
}

/// Current state of the Claude Code hook. Blocking (reads files).
pub fn status() -> Result<HookStatus, String> {
    status_for(Agent::Claude)
}

/// Current state of `agent`'s hook. Blocking (reads files).
pub fn status_for(agent: Agent) -> Result<HookStatus, String> {
    let target = target(agent)?;
    let ours = sidecar_path().and_then(|exe| command(&exe, agent).ok());
    let report = install::status(&target, ours.as_deref()).map_err(|e| e.to_string())?;
    let inbox = inbox_path(agent);
    let mut view = to_view(&report, &inbox, modified_at(&inbox));
    if report.state == State::Modified {
        // Not cleanly installed: show it as not installed so Install can repair it.
        view.installed = false;
    }
    Ok(view)
}

/// Install the hook into Claude's settings. Blocking. Only call after the user confirmed.
pub fn install() -> Result<String, String> {
    install_for(Agent::Claude)
}

/// Install the hook into `agent`'s settings. Blocking. Only call after the user confirmed.
pub fn install_for(agent: Agent) -> Result<String, String> {
    let exe = sidecar_path().ok_or(
        "The hook program is missing from the app folder. Reinstall Cooldown and try again.",
    )?;
    let command = command(&exe, agent)?;
    install::install(&target(agent)?, &command, true, OffsetDateTime::now_utc())
        .map(|report| report.message)
        .map_err(|e| e.to_string())
}

/// Remove the Claude hook and restore the previous statusline. Blocking.
pub fn uninstall() -> Result<String, String> {
    uninstall_for(Agent::Claude)
}

/// Remove `agent`'s hook and restore its previous status line. Blocking.
pub fn uninstall_for(agent: Agent) -> Result<String, String> {
    install::uninstall(&target(agent)?, true, OffsetDateTime::now_utc())
        .map(|report| report.message)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn report(state: State, chained: Option<&str>) -> StatusReport {
        StatusReport {
            state,
            reason: None,
            settings_path: "/home/me/.claude/settings.json".into(),
            chain_path: "/home/me/.config/cooldown/chain.json".into(),
            status_line_command: Some("cooldown-statusline".into()),
            chained_command: chained.map(str::to_owned),
            notes: vec![],
        }
    }

    #[test]
    fn installed_with_a_previous_statusline() {
        let view = to_view(
            &report(State::Installed, Some("starship statusline")),
            Path::new("/tmp/inbox.json"),
            Some(datetime!(2026-10-15 09:30:00 UTC)),
        );
        assert!(view.installed);
        assert!(view.chains_existing);
        assert_eq!(
            view.backup_path.as_deref(),
            Some("/home/me/.config/cooldown/chain.json")
        );
        assert_eq!(view.last_data_at.as_deref(), Some("2026-10-15T09:30:00Z"));
        assert_eq!(view.inbox_path, "/tmp/inbox.json");
    }

    #[test]
    fn not_installed_has_no_backup() {
        let view = to_view(&report(State::NotInstalled, None), Path::new("/i"), None);
        assert!(!view.installed);
        assert!(!view.chains_existing);
        assert_eq!(view.backup_path, None);
        assert_eq!(view.last_data_at, None);
    }

    #[test]
    fn the_command_names_the_sidecar() {
        let exe = Path::new("/Applications/Cooldown.app/Contents/MacOS/cooldown-statusline");
        let cmd = command(exe, Agent::Claude).unwrap();
        assert!(cmd.contains("cooldown-statusline"), "{cmd}");
        assert!(cooldown_statusline::command::is_our_command(&cmd));
        let ag = command(exe, Agent::Antigravity).unwrap();
        assert!(ag.ends_with(" antigravity"), "{ag}");
        assert!(cooldown_statusline::command::is_our_command(&ag));
        assert_eq!(parse_agent("antigravity"), Ok(Agent::Antigravity));
        assert!(parse_agent("cursor").is_err());
    }
}
