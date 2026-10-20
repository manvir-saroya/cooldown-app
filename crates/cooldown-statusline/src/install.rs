//! Installing, removing and inspecting the `statusLine` entry in Claude's
//! settings file.
//!
//! Safety rules enforced here:
//!
//! * invalid JSON (or a non-object document) is refused and left untouched;
//! * nothing is written unless `apply` is true (the CLI's `--yes`);
//! * the settings file is backed up byte-for-byte before it is changed;
//! * every other key in the file survives, in its original order (the file is
//!   re-serialised with `serde_json`'s `preserve_order`; comments are not
//!   supported because Claude's settings are plain JSON);
//! * the previous `statusLine` is saved in the chain config so it keeps running
//!   and can be restored;
//! * install is idempotent and never records Cooldown as its own chain target;
//! * uninstall refuses to overwrite a `statusLine` somebody else changed.
//!
//! The planning functions are pure (`Map` in, plan out); the thin IO wrappers
//! around them take explicit paths, so tests never touch a real home directory.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};
use time::OffsetDateTime;

use crate::atomic::write_atomic;
use crate::chain::{self, ChainConfig, CHAIN_VERSION};
use crate::command::{is_our_command, BIN_STEM};
use crate::inbox::format_now;

/// The settings key Claude Code reads.
pub const STATUS_LINE_KEY: &str = "statusLine";

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// Why an operation did not happen.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A safety rule said no. The message tells the user what to do.
    #[error("{0}")]
    Refused(String),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
}

impl Error {
    fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The two files an install touches.
#[derive(Debug, Clone)]
pub struct Target {
    /// Claude's settings file (`~/.claude/settings.json` by default).
    pub settings: PathBuf,
    /// Cooldown's `chain.json`.
    pub chain: PathBuf,
}

/// The outcome of an install or uninstall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The operation involves a change (made, or proposed in a dry run).
    pub changed: bool,
    /// Files were actually written.
    pub applied: bool,
    /// Human-readable description, ready to print.
    pub message: String,
}

// ---------------------------------------------------------------------------
// Settings file
// ---------------------------------------------------------------------------

/// A parsed settings file plus the exact bytes it was read from.
#[derive(Debug, Clone)]
pub struct Settings {
    pub path: PathBuf,
    /// `None` when the file does not exist yet.
    pub raw: Option<Vec<u8>>,
    pub root: Map<String, Value>,
}

impl Settings {
    /// Read and validate a settings file. A missing file is an empty document.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = match fs::read(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    path: path.to_path_buf(),
                    raw: None,
                    root: Map::new(),
                });
            }
            Err(e) => return Err(Error::io(format!("could not read {}", path.display()), e)),
        };
        let body = raw.strip_prefix(BOM).unwrap_or(&raw);
        let value: Value = serde_json::from_slice(body).map_err(|e| {
            Error::Refused(format!(
                "{} is not valid JSON ({e}). Cooldown will not touch it. \
                 Fix or remove the file, then try again.",
                path.display()
            ))
        })?;
        match value {
            Value::Object(root) => Ok(Self {
                path: path.to_path_buf(),
                raw: Some(raw),
                root,
            }),
            _ => Err(Error::Refused(format!(
                "{} must contain a JSON object at the top level. Cooldown will not touch it.",
                path.display()
            ))),
        }
    }

    /// The current `statusLine` value (an explicit `null` counts as unset).
    pub fn status_line(&self) -> Option<&Value> {
        self.root.get(STATUS_LINE_KEY).filter(|v| !v.is_null())
    }
}

fn settings_bytes(root: &Map<String, Value>) -> Vec<u8> {
    let mut bytes =
        serde_json::to_vec_pretty(&Value::Object(root.clone())).unwrap_or_else(|_| b"{}".to_vec());
    bytes.push(b'\n');
    bytes
}

/// Fail if the settings file differs from what we read earlier, so we never
/// overwrite an edit Claude Code or the user made in the meantime.
fn ensure_unchanged(path: &Path, expected: Option<&[u8]>) -> Result<()> {
    let current = match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(Error::io(
                format!("could not re-read {}", path.display()),
                e,
            ))
        }
    };
    if current.as_deref() == expected {
        Ok(())
    } else {
        Err(Error::Refused(format!(
            "{} changed while Cooldown was working on it. Nothing was written; run the command again.",
            path.display()
        )))
    }
}

fn write_settings(settings: &Settings, root: &Map<String, Value>) -> Result<()> {
    ensure_unchanged(&settings.path, settings.raw.as_deref())?;
    if let Some(dir) = settings.path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)
            .map_err(|e| Error::io(format!("could not create {}", dir.display()), e))?;
    }
    write_atomic(&settings.path, &settings_bytes(root))
        .map_err(|e| Error::io(format!("could not write {}", settings.path.display()), e))
}

fn compact_timestamp(now: OffsetDateTime) -> String {
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

/// `settings.json.cooldown-backup-<timestamp>` next to the settings file.
pub fn backup_path(settings: &Path, now: OffsetDateTime) -> PathBuf {
    let name = settings.file_name().map_or_else(
        || "settings.json".into(),
        |n| n.to_string_lossy().into_owned(),
    );
    settings.with_file_name(format!("{name}.cooldown-backup-{}", compact_timestamp(now)))
}

/// Copy the original bytes to a fresh backup file. Returns `None` when there
/// is no existing file to back up.
fn write_backup(settings: &Settings, now: OffsetDateTime) -> Result<Option<PathBuf>> {
    let Some(raw) = &settings.raw else {
        return Ok(None);
    };
    let base = backup_path(&settings.path, now);
    for attempt in 0..100u32 {
        let candidate = if attempt == 0 {
            base.clone()
        } else {
            let mut name = base.file_name().unwrap_or_default().to_os_string();
            name.push(format!("-{attempt}"));
            base.with_file_name(name)
        };
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                file.write_all(raw)
                    .and_then(|()| file.flush())
                    .map_err(|e| {
                        Error::io(format!("could not write backup {}", candidate.display()), e)
                    })?;
                if let Ok(meta) = fs::metadata(&settings.path) {
                    let _ = fs::set_permissions(&candidate, meta.permissions());
                }
                return Ok(Some(candidate));
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(Error::io(
                    format!("could not create backup {}", candidate.display()),
                    e,
                ));
            }
        }
    }
    Err(Error::Refused(format!(
        "too many backups already exist next to {}",
        settings.path.display()
    )))
}

// ---------------------------------------------------------------------------
// Helpers on statusLine values
// ---------------------------------------------------------------------------

fn command_of(status_line: &Value) -> Option<&str> {
    status_line.get("command")?.as_str()
}

fn is_ours(status_line: &Value) -> bool {
    command_of(status_line).is_some_and(is_our_command)
}

/// A copy of `status_line` with `command` replaced, keeping every other field
/// (`padding`, `refreshInterval`, ...) and the key order.
fn with_command(status_line: &Value, command: &str) -> Value {
    let mut object = status_line.as_object().cloned().unwrap_or_default();
    object.insert("type".into(), Value::String("command".into()));
    object.insert("command".into(), Value::String(command.into()));
    Value::Object(object)
}

/// The statusLine to write: the original's extra fields carried over when it
/// was a command entry, otherwise a minimal `{type, command}`.
fn build_status_line(original: Option<&Value>, command: &str) -> Value {
    let carry = original.filter(|o| {
        o.as_object()
            .is_some_and(|m| m.get("type").is_none_or(|t| t.as_str() == Some("command")))
    });
    with_command(carry.unwrap_or(&Value::Null), command)
}

fn show(value: Option<&Value>) -> String {
    value.map_or_else(
        || "(not set)".to_owned(),
        |v| serde_json::to_string(v).unwrap_or_else(|_| "?".to_owned()),
    )
}

fn same_settings_path(recorded: &str, actual: &str) -> bool {
    if recorded == actual {
        return true;
    }
    match (fs::canonicalize(recorded), fs::canonicalize(actual)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn settings_key(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// A chain config that belongs to a different settings file must not be
/// overwritten or consumed from here.
fn check_chain_owner(
    chain: Option<&ChainConfig>,
    settings: &Path,
    chain_path: &Path,
) -> Result<()> {
    match chain {
        Some(c) if !same_settings_path(&c.settings_path, &settings_key(settings)) => {
            Err(Error::Refused(format!(
                "Cooldown is already set up for a different settings file ({}). \
                 Run `{BIN_STEM} uninstall --settings <that file> --yes` first, \
                 or delete {} if that file is gone.",
                c.settings_path,
                chain_path.display()
            )))
        }
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    /// No statusLine existed.
    Fresh,
    /// The user's statusLine will be saved and chained.
    ChainExisting,
    /// Our entry pointed at a different binary path; it is updated.
    Repoint,
    /// Our entry exists but its chain config was missing; it is recreated
    /// (the previous statusLine, if any, cannot be recovered).
    RepairChain,
    /// An old chain config existed but the statusLine had since been replaced.
    ReplaceStaleChain,
}

/// What an install would change.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub kind: InstallKind,
    pub old_status_line: Option<Value>,
    pub new_status_line: Value,
    pub chain: ChainConfig,
    pub notes: Vec<String>,
}

impl Change {
    fn settings_changed(&self) -> bool {
        self.old_status_line.as_ref() != Some(&self.new_status_line)
    }
}

/// Decide what installing `command` should do. `None` means already installed.
pub fn plan_install(
    root: &Map<String, Value>,
    chain: Option<&ChainConfig>,
    settings_path: &str,
    command: &str,
    now: &str,
) -> Option<Change> {
    let current = root.get(STATUS_LINE_KEY).filter(|v| !v.is_null());
    let mut notes = Vec::new();

    let (kind, original, new_status_line) = match current {
        Some(cur) if is_ours(cur) => {
            if chain.is_some() && command_of(cur) == Some(command) {
                return None;
            }
            let new = with_command(cur, command);
            match chain {
                Some(c) => (InstallKind::Repoint, c.original.clone(), new),
                None => {
                    notes.push(
                        "The chain config was missing, so the statusLine that was there before \
                         Cooldown cannot be recovered; uninstall will simply remove the entry."
                            .to_owned(),
                    );
                    (InstallKind::RepairChain, None, new)
                }
            }
        }
        _ => {
            let kind = if chain.is_some() {
                notes.push(
                    "An older Cooldown chain config exists, but the statusLine was changed \
                     after it was written; the config is replaced to match what is there now."
                        .to_owned(),
                );
                InstallKind::ReplaceStaleChain
            } else if current.is_some() {
                InstallKind::ChainExisting
            } else {
                InstallKind::Fresh
            };
            let original = current.cloned();
            let new = build_status_line(original.as_ref(), command);
            (kind, original, new)
        }
    };

    Some(Change {
        kind,
        old_status_line: current.cloned(),
        chain: ChainConfig {
            version: CHAIN_VERSION,
            settings_path: settings_path.to_owned(),
            installed_at: now.to_owned(),
            installed_status_line: new_status_line.clone(),
            original,
        },
        new_status_line,
        notes,
    })
}

/// Install the Cooldown statusline: dry run unless `apply`.
pub fn install(target: &Target, command: &str, apply: bool, now: OffsetDateTime) -> Result<Report> {
    let settings = Settings::load(&target.settings)?;
    let chain = chain::load(&target.chain);
    check_chain_owner(chain.as_ref(), &target.settings, &target.chain)?;

    let Some(change) = plan_install(
        &settings.root,
        chain.as_ref(),
        &settings_key(&target.settings),
        command,
        &format_now(now),
    ) else {
        return Ok(Report {
            changed: false,
            applied: false,
            message: format!(
                "Cooldown is already installed in {}. Nothing to do.",
                target.settings.display()
            ),
        });
    };

    let mut backup = None;
    if apply {
        if change.settings_changed() {
            backup = write_backup(&settings, now)?;
        }
        chain::save(&target.chain, &change.chain)
            .map_err(|e| Error::io(format!("could not write {}", target.chain.display()), e))?;
        if change.settings_changed() {
            let mut root = settings.root.clone();
            root.insert(STATUS_LINE_KEY.into(), change.new_status_line.clone());
            write_settings(&settings, &root)?;
        }
    }

    Ok(Report {
        changed: true,
        applied: apply,
        message: describe_install(target, &settings, &change, apply, now, backup.as_deref()),
    })
}

fn describe_install(
    target: &Target,
    settings: &Settings,
    change: &Change,
    applied: bool,
    now: OffsetDateTime,
    backup: Option<&Path>,
) -> String {
    let mut out = String::new();
    out.push_str(if applied {
        "Cooldown statusline installed.\n\n"
    } else {
        "DRY RUN: nothing has been written.\n\n"
    });

    let created = settings.raw.is_none();
    out.push_str(&format!(
        "Settings file : {}{}\n",
        target.settings.display(),
        if created { " (will be created)" } else { "" }
    ));
    if change.settings_changed() {
        let planned = backup_path(&target.settings, now);
        let shown = backup.unwrap_or(&planned);
        out.push_str(&if created {
            "Backup        : none (the file does not exist yet)\n".to_owned()
        } else {
            format!("Backup        : {}\n", shown.display())
        });
    }
    out.push_str(&format!("Chain config  : {}\n\n", target.chain.display()));

    out.push_str(&format!(
        "statusLine before:\n  {}\n",
        show(change.old_status_line.as_ref())
    ));
    out.push_str(&format!(
        "statusLine after:\n  {}\n\n",
        show(Some(&change.new_status_line))
    ));

    out.push_str(&match change.kind {
        InstallKind::Fresh => "No statusLine was set. The Cooldown hook will print `5h N% · 7d N%`.\n".to_owned(),
        InstallKind::ChainExisting | InstallKind::ReplaceStaleChain => format!(
            "Your existing statusLine is saved to the chain config and keeps running: Cooldown \
             passes it the same input and shows its output unchanged ({}).\n",
            change
                .chain
                .original_command()
                .map_or_else(|| "it has no command to run".to_owned(), |c| format!("`{c}`"))
        ),
        InstallKind::Repoint => "Cooldown was already installed; the command now points at this binary. The saved chain is kept.\n".to_owned(),
        InstallKind::RepairChain => "Cooldown was already installed but its chain config was missing; it is recreated.\n".to_owned(),
    });
    for note in &change.notes {
        out.push_str(&format!("Note: {note}\n"));
    }
    out.push_str("All other settings are preserved.\n");
    if applied {
        out.push_str(&format!("To undo: {BIN_STEM} uninstall --yes\n"));
    } else {
        out.push_str("Re-run with --yes to apply.\n");
    }
    out
}

// ---------------------------------------------------------------------------
// Uninstall
// ---------------------------------------------------------------------------

/// What an uninstall would do.
#[derive(Debug, Clone, PartialEq)]
pub enum UninstallPlan {
    /// Cooldown is not installed.
    NothingToDo,
    /// The statusLine is already gone; only the stale chain config is removed.
    CleanChainOnly,
    /// Put the saved statusLine back (or remove the entry when `restore` is
    /// `None`).
    Restore {
        current: Value,
        restore: Option<Value>,
        notes: Vec<String>,
    },
}

/// Decide what uninstalling should do, refusing to clobber a statusLine that
/// was changed after install.
pub fn plan_uninstall(
    root: &Map<String, Value>,
    chain: Option<&ChainConfig>,
) -> Result<UninstallPlan> {
    let current = root.get(STATUS_LINE_KEY).filter(|v| !v.is_null());
    match (current, chain) {
        (Some(cur), _) if is_ours(cur) => {
            let mut notes = Vec::new();
            match chain {
                None => notes.push(
                    "No chain config was found, so there is nothing to restore; the Cooldown \
                     entry is simply removed."
                        .to_owned(),
                ),
                Some(c) if &c.installed_status_line != cur => notes.push(
                    "The Cooldown statusLine was edited after install; the saved original \
                     replaces it."
                        .to_owned(),
                ),
                Some(_) => {}
            }
            Ok(UninstallPlan::Restore {
                current: cur.clone(),
                restore: chain.and_then(|c| c.original.clone()),
                notes,
            })
        }
        (Some(cur), Some(c)) => Err(Error::Refused(format!(
            "The statusLine in your settings was changed since Cooldown was installed \
             (it now is {}), so it was left alone. The one saved at install time was {}. \
             Edit the settings file by hand if you want to replace it.",
            show(Some(cur)),
            show(c.original.as_ref())
        ))),
        (Some(_), None) => Ok(UninstallPlan::NothingToDo),
        (None, Some(_)) => Ok(UninstallPlan::CleanChainOnly),
        (None, None) => Ok(UninstallPlan::NothingToDo),
    }
}

/// Uninstall the Cooldown statusline: dry run unless `apply`.
pub fn uninstall(target: &Target, apply: bool, now: OffsetDateTime) -> Result<Report> {
    let settings = Settings::load(&target.settings)?;
    let chain = chain::load(&target.chain);
    check_chain_owner(chain.as_ref(), &target.settings, &target.chain)?;

    let plan = plan_uninstall(&settings.root, chain.as_ref())?;
    let header = if apply {
        "Cooldown statusline removed.\n\n"
    } else {
        "DRY RUN: nothing has been written.\n\n"
    };

    match plan {
        UninstallPlan::NothingToDo => Ok(Report {
            changed: false,
            applied: false,
            message: format!(
                "Cooldown is not installed in {}. Nothing to do.",
                target.settings.display()
            ),
        }),
        UninstallPlan::CleanChainOnly => {
            if apply {
                chain::remove(&target.chain).map_err(|e| {
                    Error::io(format!("could not remove {}", target.chain.display()), e)
                })?;
            }
            Ok(Report {
                changed: true,
                applied: apply,
                message: format!(
                    "{header}The statusLine is already gone from {}. Only the leftover chain \
                     config {} is removed.\n{}",
                    target.settings.display(),
                    target.chain.display(),
                    if apply {
                        ""
                    } else {
                        "Re-run with --yes to apply.\n"
                    }
                ),
            })
        }
        UninstallPlan::Restore {
            current,
            restore,
            notes,
        } => {
            let mut backup = None;
            if apply {
                backup = write_backup(&settings, now)?;
                let mut root = settings.root.clone();
                match &restore {
                    Some(original) => {
                        root.insert(STATUS_LINE_KEY.into(), original.clone());
                    }
                    None => {
                        root.shift_remove(STATUS_LINE_KEY);
                    }
                }
                write_settings(&settings, &root)?;
                chain::remove(&target.chain).map_err(|e| {
                    Error::io(format!("could not remove {}", target.chain.display()), e)
                })?;
            }

            let planned = backup_path(&target.settings, now);
            let mut out = String::from(header);
            out.push_str(&format!("Settings file : {}\n", target.settings.display()));
            out.push_str(&format!(
                "Backup        : {}\n\n",
                backup.as_deref().unwrap_or(&planned).display()
            ));
            out.push_str(&format!("statusLine before:\n  {}\n", show(Some(&current))));
            out.push_str(&format!(
                "statusLine after:\n  {}\n\n",
                show(restore.as_ref())
            ));
            for note in &notes {
                out.push_str(&format!("Note: {note}\n"));
            }
            out.push_str("All other settings are preserved.\n");
            if !apply {
                out.push_str("Re-run with --yes to apply.\n");
            }
            Ok(Report {
                changed: true,
                applied: apply,
                message: out,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    Installed,
    NotInstalled,
    /// Cooldown's entry was changed, removed or is inconsistent.
    Modified,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusReport {
    pub state: State,
    /// Why the state is `modified` (or other detail).
    pub reason: Option<String>,
    pub settings_path: String,
    pub chain_path: String,
    /// The `statusLine.command` currently in the settings file.
    pub status_line_command: Option<String>,
    /// The saved command Cooldown chains to, if any.
    pub chained_command: Option<String>,
    pub notes: Vec<String>,
}

impl StatusReport {
    /// A short, human-readable rendering.
    pub fn to_text(&self) -> String {
        let headline = match self.state {
            State::Installed => "installed".to_owned(),
            State::NotInstalled => "not installed".to_owned(),
            State::Modified => format!(
                "modified: {}",
                self.reason
                    .as_deref()
                    .unwrap_or("the statusLine entry changed")
            ),
        };
        let mut out = format!("{headline}\nSettings file : {}\n", self.settings_path);
        if let Some(c) = &self.status_line_command {
            out.push_str(&format!("statusLine    : {c}\n"));
        }
        if let Some(c) = &self.chained_command {
            out.push_str(&format!("Chained to    : {c}\n"));
        }
        for note in &self.notes {
            out.push_str(&format!("Note: {note}\n"));
        }
        out
    }
}

/// Classify the installation state from already-loaded data.
pub fn compute_status(
    root: &Map<String, Value>,
    chain: Option<&ChainConfig>,
    settings_path: &str,
    chain_path: &str,
    our_command: Option<&str>,
) -> StatusReport {
    let current = root.get(STATUS_LINE_KEY).filter(|v| !v.is_null());
    let mut notes = Vec::new();

    let (state, reason) = match (current, chain) {
        (Some(cur), Some(c)) if is_ours(cur) => {
            if !same_settings_path(&c.settings_path, settings_path) {
                (
                    State::Modified,
                    Some(format!(
                        "the chain config belongs to another settings file ({})",
                        c.settings_path
                    )),
                )
            } else {
                if let (Some(ours), Some(actual)) = (our_command, command_of(cur)) {
                    if ours != actual {
                        notes.push(format!(
                            "the installed command points at a different binary ({actual}); \
                             run `{BIN_STEM} install --yes` to point it at this one"
                        ));
                    }
                }
                (State::Installed, None)
            }
        }
        (Some(cur), None) if is_ours(cur) => (
            State::Modified,
            Some(
                "the statusLine runs Cooldown but its chain config is missing, so the previous \
                 statusLine cannot be restored"
                    .to_owned(),
            ),
        ),
        (Some(_), Some(_)) => (
            State::Modified,
            Some("the statusLine was changed after Cooldown was installed".to_owned()),
        ),
        (None, Some(_)) => (
            State::Modified,
            Some("the statusLine was removed after Cooldown was installed".to_owned()),
        ),
        (Some(_), None) | (None, None) => (State::NotInstalled, None),
    };

    StatusReport {
        state,
        reason,
        settings_path: settings_path.to_owned(),
        chain_path: chain_path.to_owned(),
        status_line_command: current.and_then(command_of).map(str::to_owned),
        chained_command: chain.and_then(|c| c.original_command()).map(str::to_owned),
        notes,
    }
}

/// Report the installation state for `target`.
pub fn status(target: &Target, our_command: Option<&str>) -> Result<StatusReport> {
    let settings = Settings::load(&target.settings)?;
    let chain = chain::load(&target.chain);
    Ok(compute_status(
        &settings.root,
        chain.as_ref(),
        &settings_key(&target.settings),
        &target.chain.to_string_lossy(),
        our_command,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::macros::datetime;

    const CMD: &str = "/opt/cooldown/cooldown-statusline";

    fn now() -> OffsetDateTime {
        datetime!(2026-10-10 19:55:01 UTC)
    }

    struct Fixture {
        dir: tempfile::TempDir,
        target: Target,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let target = Target {
                settings: dir.path().join("claude").join("settings.json"),
                chain: dir.path().join("config").join("chain.json"),
            };
            fs::create_dir_all(target.settings.parent().unwrap()).unwrap();
            Self { dir, target }
        }

        /// Write `value` formatted exactly as Cooldown would write it.
        fn write_settings(&self, value: &Value) -> Vec<u8> {
            let bytes = settings_bytes(value.as_object().unwrap());
            fs::write(&self.target.settings, &bytes).unwrap();
            bytes
        }

        fn settings_value(&self) -> Value {
            serde_json::from_slice(&fs::read(&self.target.settings).unwrap()).unwrap()
        }

        fn settings_bytes(&self) -> Vec<u8> {
            fs::read(&self.target.settings).unwrap()
        }

        fn backups(&self) -> Vec<PathBuf> {
            let mut found: Vec<_> = fs::read_dir(self.target.settings.parent().unwrap())
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.to_string_lossy().contains("cooldown-backup"))
                .collect();
            found.sort();
            found
        }

        fn install(&self, apply: bool) -> Result<Report> {
            install(&self.target, CMD, apply, now())
        }

        fn uninstall(&self, apply: bool) -> Result<Report> {
            uninstall(&self.target, apply, now())
        }

        fn state(&self) -> State {
            status(&self.target, Some(CMD)).unwrap().state
        }
    }

    fn user_settings() -> Value {
        json!({
            "zeta": 1,
            "env": {"B": "2", "A": "1"},
            "statusLine": {"type": "command", "command": "~/.claude/statusline.sh", "padding": 2},
            "permissions": {"allow": ["Bash(ls)"], "deny": []},
            "alpha": [3, 2, 1],
            "ratio": 1.5,
            "unicode": "héllo ✓",
            "nothing": null
        })
    }

    fn keys(value: &Value) -> Vec<String> {
        value.as_object().unwrap().keys().cloned().collect()
    }

    // --- dry run ---------------------------------------------------------

    #[test]
    fn dry_run_writes_nothing_and_says_what_would_change() {
        let f = Fixture::new();
        let before = f.write_settings(&user_settings());
        let report = f.install(false).unwrap();
        assert!(report.changed && !report.applied);
        assert!(report.message.contains("DRY RUN"));
        assert!(report.message.contains("~/.claude/statusline.sh"));
        assert!(report.message.contains(CMD));
        assert!(report.message.contains("cooldown-backup-20261010T195501Z"));
        assert!(report.message.contains("--yes"));
        assert_eq!(f.settings_bytes(), before, "settings untouched");
        assert!(f.backups().is_empty(), "no backup in a dry run");
        assert!(!f.target.chain.exists(), "no chain config in a dry run");
    }

    #[test]
    fn dry_run_on_missing_settings_creates_nothing() {
        let f = Fixture::new();
        let report = f.install(false).unwrap();
        assert!(report.message.contains("will be created"));
        assert!(!f.target.settings.exists());
        assert!(!f.target.chain.exists());
    }

    // --- install / uninstall round trips ---------------------------------

    #[test]
    fn install_chains_existing_statusline_and_preserves_every_other_key_in_order() {
        let f = Fixture::new();
        f.write_settings(&user_settings());
        let report = f.install(true).unwrap();
        assert!(report.applied);

        let after = f.settings_value();
        assert_eq!(keys(&after), keys(&user_settings()), "key order preserved");
        for key in [
            "zeta",
            "env",
            "permissions",
            "alpha",
            "ratio",
            "unicode",
            "nothing",
        ] {
            assert_eq!(after[key], user_settings()[key], "{key} preserved");
        }
        assert_eq!(
            after["statusLine"],
            json!({"type": "command", "command": CMD, "padding": 2}),
            "padding carried over"
        );

        let chain = chain::load(&f.target.chain).unwrap();
        assert_eq!(chain.original, Some(user_settings()["statusLine"].clone()));
        assert_eq!(chain.original_command(), Some("~/.claude/statusline.sh"));
        assert_eq!(chain.installed_status_line, after["statusLine"]);
        assert_eq!(f.state(), State::Installed);
    }

    #[test]
    fn install_then_uninstall_restores_the_file_byte_for_byte() {
        let f = Fixture::new();
        let original = f.write_settings(&user_settings());
        f.install(true).unwrap();
        assert_ne!(f.settings_bytes(), original);
        f.uninstall(true).unwrap();
        assert_eq!(f.settings_bytes(), original);
        assert!(!f.target.chain.exists());
        assert_eq!(f.state(), State::NotInstalled);
    }

    #[test]
    fn install_without_a_previous_statusline_appends_it_and_uninstall_removes_it() {
        let f = Fixture::new();
        let mut settings = user_settings();
        settings.as_object_mut().unwrap().shift_remove("statusLine");
        let original = f.write_settings(&settings);

        f.install(true).unwrap();
        let after = f.settings_value();
        let mut expected_keys = keys(&settings);
        expected_keys.push("statusLine".into());
        assert_eq!(keys(&after), expected_keys);
        assert_eq!(
            after["statusLine"],
            json!({"type": "command", "command": CMD})
        );
        assert_eq!(chain::load(&f.target.chain).unwrap().original, None);

        f.uninstall(true).unwrap();
        assert_eq!(f.settings_bytes(), original);
    }

    #[test]
    fn install_creates_settings_file_and_directory_when_missing() {
        let f = Fixture::new();
        fs::remove_dir_all(f.target.settings.parent().unwrap()).unwrap();
        let report = f.install(true).unwrap();
        assert!(report.applied);
        assert_eq!(
            f.settings_value(),
            json!({"statusLine": {"type": "command", "command": CMD}})
        );
        assert!(f.backups().is_empty(), "nothing to back up");

        f.uninstall(true).unwrap();
        assert_eq!(f.settings_value(), json!({}));
    }

    #[test]
    fn unknown_statusline_fields_survive_the_round_trip() {
        let f = Fixture::new();
        let settings = json!({"statusLine": {
            "type": "command", "command": "x.sh", "refreshInterval": 5, "futureField": {"a": [1]}
        }});
        let original = f.write_settings(&settings);
        f.install(true).unwrap();
        let after = f.settings_value();
        assert_eq!(after["statusLine"]["refreshInterval"], 5);
        assert_eq!(after["statusLine"]["futureField"], json!({"a": [1]}));
        f.uninstall(true).unwrap();
        assert_eq!(f.settings_bytes(), original);
    }

    #[test]
    fn non_command_statusline_is_saved_and_replaced_by_a_minimal_entry() {
        let f = Fixture::new();
        let settings = json!({"statusLine": {"type": "static", "text": "hi"}});
        let original = f.write_settings(&settings);
        f.install(true).unwrap();
        assert_eq!(
            f.settings_value()["statusLine"],
            json!({"type": "command", "command": CMD})
        );
        assert_eq!(
            chain::load(&f.target.chain).unwrap().original_command(),
            None
        );
        f.uninstall(true).unwrap();
        assert_eq!(f.settings_bytes(), original);
    }

    // --- backups ---------------------------------------------------------

    #[test]
    fn backup_is_an_exact_copy_of_the_original_bytes() {
        let f = Fixture::new();
        // Deliberately not in Cooldown's own formatting.
        let raw = b"{\"a\":1,\n  \"statusLine\":   {\"type\":\"command\",\"command\":\"x\"}}";
        fs::write(&f.target.settings, raw).unwrap();
        f.install(true).unwrap();
        let backups = f.backups();
        assert_eq!(backups.len(), 1);
        assert!(backups[0]
            .to_string_lossy()
            .ends_with("settings.json.cooldown-backup-20261010T195501Z"));
        assert_eq!(fs::read(&backups[0]).unwrap(), raw);
    }

    #[test]
    fn uninstall_also_backs_up_and_backups_never_overwrite() {
        let f = Fixture::new();
        f.write_settings(&user_settings());
        f.install(true).unwrap();
        let installed = f.settings_bytes();
        f.uninstall(true).unwrap();
        // Same timestamp for both operations: the second backup gets a suffix.
        let backups = f.backups();
        assert_eq!(backups.len(), 2, "{backups:?}");
        let contents: Vec<_> = backups.iter().map(|b| fs::read(b).unwrap()).collect();
        assert!(contents.contains(&installed));
    }

    // --- idempotence -----------------------------------------------------

    #[test]
    fn installing_twice_changes_nothing_the_second_time() {
        let f = Fixture::new();
        f.write_settings(&user_settings());
        f.install(true).unwrap();
        let settings_after_first = f.settings_bytes();
        let chain_after_first = fs::read(&f.target.chain).unwrap();

        let second = f.install(true).unwrap();
        assert!(!second.changed && !second.applied);
        assert!(second.message.contains("already installed"));
        assert_eq!(f.settings_bytes(), settings_after_first);
        assert_eq!(fs::read(&f.target.chain).unwrap(), chain_after_first);
        assert_eq!(f.backups().len(), 1, "no second backup");
        // The chain still points at the user's script, never at ourselves.
        assert_eq!(
            chain::load(&f.target.chain).unwrap().original_command(),
            Some("~/.claude/statusline.sh")
        );
    }

    #[test]
    fn never_records_ourselves_as_the_command_to_chain_to() {
        let f = Fixture::new();
        // Settings already point at Cooldown but the chain config is gone.
        f.write_settings(&json!({"statusLine": {"type": "command", "command": CMD}}));
        let report = f.install(true).unwrap();
        assert!(report.applied);
        let chain = chain::load(&f.target.chain).unwrap();
        assert_eq!(chain.original, None);
        assert_eq!(chain.original_command(), None);
        assert!(report.message.contains("cannot be recovered"));
        assert_eq!(f.state(), State::Installed);
    }

    #[test]
    fn a_moved_binary_is_repointed_and_the_saved_chain_is_kept() {
        let f = Fixture::new();
        f.write_settings(&user_settings());
        f.install(true).unwrap();

        let new_cmd = "/new/place/cooldown-statusline";
        let report = install(&f.target, new_cmd, true, now()).unwrap();
        assert!(report.applied);
        assert_eq!(f.settings_value()["statusLine"]["command"], new_cmd);
        assert_eq!(f.settings_value()["statusLine"]["padding"], 2);
        let chain = chain::load(&f.target.chain).unwrap();
        assert_eq!(chain.original_command(), Some("~/.claude/statusline.sh"));
        assert_eq!(
            chain.installed_status_line,
            f.settings_value()["statusLine"]
        );

        // Status flags the stale path when asked about the old binary.
        let s = status(&f.target, Some(CMD)).unwrap();
        assert_eq!(s.state, State::Installed);
        assert!(s.notes.iter().any(|n| n.contains("different binary")));
    }

    // --- refusals --------------------------------------------------------

    #[test]
    fn invalid_json_is_refused_everywhere_and_left_untouched() {
        let f = Fixture::new();
        let garbage = b"{ \"statusLine\": oops, // comment\n}";
        fs::write(&f.target.settings, garbage).unwrap();

        for result in [
            f.install(true).map(|_| ()),
            f.install(false).map(|_| ()),
            f.uninstall(true).map(|_| ()),
            status(&f.target, None).map(|_| ()),
        ] {
            let err = result.unwrap_err();
            assert!(matches!(err, Error::Refused(_)), "{err}");
            assert!(err.to_string().contains("not valid JSON"), "{err}");
        }
        assert_eq!(f.settings_bytes(), garbage);
        assert!(f.backups().is_empty());
        assert!(!f.target.chain.exists());
    }

    #[test]
    fn empty_and_non_object_documents_are_refused() {
        let f = Fixture::new();
        for content in ["", "   ", "[]", "42", "\"str\"", "null"] {
            fs::write(&f.target.settings, content).unwrap();
            let err = f.install(true).unwrap_err();
            assert!(matches!(err, Error::Refused(_)), "{content:?}: {err}");
            assert_eq!(fs::read_to_string(&f.target.settings).unwrap(), content);
        }
        assert!(!f.target.chain.exists());
    }

    #[test]
    fn utf8_bom_is_tolerated_on_read() {
        let f = Fixture::new();
        let mut raw = BOM.to_vec();
        raw.extend_from_slice(br#"{"keep": true}"#);
        fs::write(&f.target.settings, &raw).unwrap();
        f.install(true).unwrap();
        let after = f.settings_value();
        assert_eq!(after["keep"], true);
        // The backup keeps the BOM: it is an exact copy.
        assert_eq!(fs::read(&f.backups()[0]).unwrap(), raw);
    }

    #[test]
    fn uninstall_refuses_to_clobber_a_statusline_changed_since_install() {
        let f = Fixture::new();
        f.write_settings(&user_settings());
        f.install(true).unwrap();

        // Someone replaces our entry with their own.
        let mut edited = f.settings_value();
        edited["statusLine"] = json!({"type": "command", "command": "~/their/new-line.sh"});
        let edited_bytes = f.write_settings(&edited);
        let backups_before = f.backups().len();

        assert_eq!(f.state(), State::Modified);
        let err = f.uninstall(true).unwrap_err();
        assert!(matches!(err, Error::Refused(_)));
        assert!(err.to_string().contains("their/new-line.sh"), "{err}");
        assert_eq!(f.settings_bytes(), edited_bytes, "settings untouched");
        assert!(f.target.chain.exists(), "chain config kept");
        assert_eq!(f.backups().len(), backups_before);
        // A dry run refuses too.
        assert!(f.uninstall(false).is_err());
    }

    #[test]
    fn uninstall_after_the_statusline_was_deleted_only_cleans_the_chain_config() {
        let f = Fixture::new();
        f.write_settings(&user_settings());
        f.install(true).unwrap();
        let mut edited = f.settings_value();
        edited.as_object_mut().unwrap().shift_remove("statusLine");
        let edited_bytes = f.write_settings(&edited);

        assert_eq!(f.state(), State::Modified);
        let dry = f.uninstall(false).unwrap();
        assert!(dry.changed && !dry.applied);
        assert!(f.target.chain.exists());
        let report = f.uninstall(true).unwrap();
        assert!(report.applied);
        assert!(!f.target.chain.exists());
        assert_eq!(f.settings_bytes(), edited_bytes, "settings untouched");
        assert_eq!(f.state(), State::NotInstalled);
    }

    #[test]
    fn install_over_a_replaced_statusline_adopts_the_new_one_as_the_original() {
        let f = Fixture::new();
        f.write_settings(&user_settings());
        f.install(true).unwrap();
        let mut edited = f.settings_value();
        edited["statusLine"] = json!({"type": "command", "command": "~/their/new-line.sh"});
        f.write_settings(&edited);

        let report = f.install(true).unwrap();
        assert!(report.message.contains("older Cooldown chain config"));
        let chain = chain::load(&f.target.chain).unwrap();
        assert_eq!(chain.original_command(), Some("~/their/new-line.sh"));
        assert_eq!(f.state(), State::Installed);
    }

    #[test]
    fn a_chain_config_for_another_settings_file_blocks_install_and_uninstall() {
        let f = Fixture::new();
        f.write_settings(&user_settings());
        f.install(true).unwrap();

        let other = Target {
            settings: f.dir.path().join("other").join("settings.json"),
            chain: f.target.chain.clone(),
        };
        fs::create_dir_all(other.settings.parent().unwrap()).unwrap();
        fs::write(&other.settings, "{}").unwrap();
        let err = install(&other, CMD, true, now()).unwrap_err();
        assert!(err.to_string().contains("different settings file"), "{err}");
        assert!(uninstall(&other, true, now()).is_err());
        assert_eq!(fs::read_to_string(&other.settings).unwrap(), "{}");
        assert_eq!(
            chain::load(&f.target.chain).unwrap().original_command(),
            Some("~/.claude/statusline.sh")
        );
    }

    #[test]
    fn uninstall_when_nothing_is_installed_is_a_no_op() {
        let f = Fixture::new();
        let before = f.write_settings(&user_settings());
        let report = f.uninstall(true).unwrap();
        assert!(!report.changed && !report.applied);
        assert!(report.message.contains("not installed"));
        assert_eq!(f.settings_bytes(), before);

        // And with no settings file at all.
        fs::remove_file(&f.target.settings).unwrap();
        assert!(!f.uninstall(true).unwrap().changed);
        assert!(
            !f.target.settings.exists(),
            "uninstall never creates the file"
        );
    }

    #[test]
    fn uninstall_with_our_entry_but_no_chain_config_removes_the_entry() {
        let f = Fixture::new();
        f.write_settings(
            &json!({"a": 1, "statusLine": {"type": "command", "command": CMD}, "b": 2}),
        );
        assert_eq!(f.state(), State::Modified);
        let report = f.uninstall(true).unwrap();
        assert!(report.message.contains("nothing to restore"));
        assert_eq!(f.settings_value(), json!({"a": 1, "b": 2}));
    }

    #[test]
    fn a_write_is_refused_if_the_file_changed_underneath_us() {
        let f = Fixture::new();
        f.write_settings(&user_settings());
        let loaded = Settings::load(&f.target.settings).unwrap();
        // Claude Code (or the user) writes the file after we read it.
        fs::write(&f.target.settings, r#"{"edited": "meanwhile"}"#).unwrap();
        let err = write_settings(&loaded, &loaded.root).unwrap_err();
        assert!(matches!(err, Error::Refused(_)));
        assert!(err.to_string().contains("changed while"));
        assert_eq!(
            fs::read_to_string(&f.target.settings).unwrap(),
            r#"{"edited": "meanwhile"}"#
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_settings_stay_symlinks() {
        let f = Fixture::new();
        let real = f.dir.path().join("dotfiles").join("claude-settings.json");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        fs::write(&real, settings_bytes(user_settings().as_object().unwrap())).unwrap();
        std::os::unix::fs::symlink(&real, &f.target.settings).unwrap();

        f.install(true).unwrap();
        assert!(fs::symlink_metadata(&f.target.settings)
            .unwrap()
            .file_type()
            .is_symlink());
        let real_value: Value = serde_json::from_slice(&fs::read(&real).unwrap()).unwrap();
        assert_eq!(real_value["statusLine"]["command"], CMD);

        f.uninstall(true).unwrap();
        assert!(fs::symlink_metadata(&f.target.settings)
            .unwrap()
            .file_type()
            .is_symlink());
        let real_value: Value = serde_json::from_slice(&fs::read(&real).unwrap()).unwrap();
        assert_eq!(real_value, user_settings());
    }

    // --- status ----------------------------------------------------------

    #[test]
    fn status_covers_the_three_states() {
        let f = Fixture::new();
        // No file at all.
        assert_eq!(f.state(), State::NotInstalled);
        // Someone else's statusLine, no chain config.
        f.write_settings(&user_settings());
        let s = status(&f.target, Some(CMD)).unwrap();
        assert_eq!(s.state, State::NotInstalled);
        assert_eq!(
            s.status_line_command.as_deref(),
            Some("~/.claude/statusline.sh")
        );
        // Installed.
        f.install(true).unwrap();
        let s = status(&f.target, Some(CMD)).unwrap();
        assert_eq!(s.state, State::Installed);
        assert_eq!(
            s.chained_command.as_deref(),
            Some("~/.claude/statusline.sh")
        );
        assert!(s.to_text().starts_with("installed"));
        // Modified.
        let mut edited = f.settings_value();
        edited["statusLine"] = json!({"type": "command", "command": "other.sh"});
        f.write_settings(&edited);
        let s = status(&f.target, Some(CMD)).unwrap();
        assert_eq!(s.state, State::Modified);
        assert!(s.reason.unwrap().contains("changed after"));
    }

    #[test]
    fn status_serialises_with_camel_case_names() {
        let f = Fixture::new();
        f.write_settings(&user_settings());
        f.install(true).unwrap();
        let json = serde_json::to_value(status(&f.target, Some(CMD)).unwrap()).unwrap();
        assert_eq!(json["state"], "installed");
        assert!(json["settingsPath"].is_string());
        assert_eq!(json["chainedCommand"], "~/.claude/statusline.sh");
        let json = serde_json::to_value(compute_status(&Map::new(), None, "s", "c", None)).unwrap();
        assert_eq!(json["state"], "notInstalled");
    }

    #[test]
    fn backup_path_uses_a_filename_safe_utc_timestamp() {
        assert_eq!(
            backup_path(Path::new("/h/.claude/settings.json"), now()),
            Path::new("/h/.claude/settings.json.cooldown-backup-20261010T195501Z")
        );
    }
}
