//! The app's own settings file (`settings.json` in the app data directory).
//!
//! Nothing secret goes here: API keys live in the OS credential store, the phone identity key in
//! the keyring. A missing or unreadable file means defaults, so a corrupt file can never stop the
//! app from starting.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cooldown_core::UsageNotified;
use serde::{Deserialize, Serialize};

/// Which automatic notifications the user wants. Every one is on by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Notifications {
    /// Ring when a used-up limit resets, without pressing its bell.
    pub limit_back: bool,
    /// Ring when a limit reaches 100%.
    pub ran_out: bool,
    /// Ring once per cycle when a limit passes this percentage. `None` is off.
    pub low_at_percent: Option<u8>,
}

impl Default for Notifications {
    fn default() -> Self {
        Notifications {
            limit_back: true,
            ran_out: true,
            low_at_percent: Some(90),
        }
    }
}

impl Notifications {
    /// The warning threshold must be a real percentage below 100.
    pub fn validate(&self) -> Result<(), String> {
        match self.low_at_percent {
            Some(p) if !(1..=99).contains(&p) => {
                Err("The warning level must be between 1% and 99%.".into())
            }
            _ => Ok(()),
        }
    }
}

/// Everything the desktop app remembers between runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Sources the user switched on or off, by adapter id. A source not listed uses its catalog
    /// default (`CatalogEntry::on_by_default`).
    pub sources: std::collections::BTreeMap<String, bool>,
    /// Automatic notifications.
    pub notifications: Notifications,
    /// "Running low" and "ran out" notifications already shown, so each rings once per cycle,
    /// even across a restart.
    pub usage_notified: Vec<UsageNotified>,
    /// The xAI team whose prepaid balance to read. `None` uses the team the key belongs to.
    pub xai_team_id: Option<String>,
    /// The Copilot plan's monthly included AI credits, in US cents, as the user entered it. GitHub's
    /// billing API does not report it. `None`: show spend only, no percentage.
    pub copilot_allowance_cents: Option<u32>,
    /// True once launch-at-login has been switched on for the first time, so a user who turns it
    /// off is never overruled.
    pub autostart_initialized: bool,
    /// The last sync revision pushed to phones, so a restart continues the sequence.
    pub sync_resume: Option<SyncResume>,
}

/// A persisted `cooldown_sync::SyncStamp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncResume {
    /// The epoch.
    pub epoch: u64,
    /// The revision within the epoch.
    pub rev: u64,
}

/// The settings file plus an in-memory copy.
#[derive(Debug)]
pub struct SettingsFile {
    path: PathBuf,
    value: Mutex<Settings>,
}

impl SettingsFile {
    /// Load `path`, falling back to defaults when it is missing or unreadable.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let value = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        SettingsFile {
            path,
            value: Mutex::new(value),
        }
    }

    /// A copy of the current settings.
    pub fn get(&self) -> Settings {
        self.lock().clone()
    }

    /// Change the settings and write them to disk. The in-memory copy changes even if the write
    /// fails, so the running app behaves as the user asked.
    pub fn update(&self, change: impl FnOnce(&mut Settings)) -> io::Result<()> {
        let snapshot = {
            let mut value = self.lock();
            change(&mut value);
            value.clone()
        };
        write_atomic(&self.path, &serde_json::to_vec_pretty(&snapshot)?)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Settings> {
        crate::backend::lock(&self.value)
    }
}

/// Write to a temporary file in the same directory, flush it, then rename it over the target.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let file = SettingsFile::load(dir.path().join("settings.json"));
        assert_eq!(file.get(), Settings::default());
    }

    #[test]
    fn corrupt_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, b"{ not json").unwrap();
        assert_eq!(SettingsFile::load(&path).get(), Settings::default());
    }

    #[test]
    fn update_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("settings.json");
        let file = SettingsFile::load(&path);
        file.update(|s| {
            s.xai_team_id = Some("team-1".into());
            s.autostart_initialized = true;
            s.sync_resume = Some(SyncResume {
                epoch: u64::MAX,
                rev: 7,
            });
        })
        .unwrap();
        let reloaded = SettingsFile::load(&path).get();
        assert_eq!(reloaded, file.get());
        assert_eq!(reloaded.sync_resume.unwrap().epoch, u64::MAX);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("xaiTeamId"), "camelCase on disk: {text}");
    }

    #[test]
    fn notifications_default_on_for_an_older_settings_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, br#"{"xaiTeamId":"t"}"#).unwrap();
        let n = SettingsFile::load(&path).get().notifications;
        assert_eq!(n, Notifications::default());
        assert!(n.limit_back && n.ran_out);
        assert_eq!(n.low_at_percent, Some(90));
        fs::write(&path, br#"{"notifications":{"ranOut":false}}"#).unwrap();
        let n = SettingsFile::load(&path).get().notifications;
        assert!(
            !n.ran_out && n.limit_back,
            "missing fields keep their defaults"
        );
    }

    #[test]
    fn the_warning_level_is_checked() {
        let at = |p| Notifications {
            low_at_percent: p,
            ..Notifications::default()
        };
        assert!(at(Some(80)).validate().is_ok());
        assert!(at(None).validate().is_ok());
        assert!(at(Some(0)).validate().is_err());
        assert!(at(Some(100)).validate().is_err());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, br#"{"xaiTeamId":"t","future":1}"#).unwrap();
        assert_eq!(
            SettingsFile::load(&path).get().xai_team_id.as_deref(),
            Some("t")
        );
    }
}
