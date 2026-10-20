//! Where Cooldown keeps its files.
//!
//! The `resolve_*` functions are pure (they take the environment values as
//! arguments) so they are tested on any OS. The un-prefixed wrappers read the
//! real environment and are only called from the CLI entry points.

use std::ffi::OsString;
use std::path::PathBuf;

/// Overrides the inbox directory itself (used by tests and by the app).
pub const ENV_INBOX_DIR: &str = "COOLDOWN_INBOX_DIR";
/// Overrides the directory that holds `chain.json` (the directory itself, not
/// its parent).
pub const ENV_CONFIG_DIR: &str = "COOLDOWN_CONFIG_DIR";
/// File name of the chain config.
pub const CHAIN_FILE_NAME: &str = "chain.json";

const APP_DIR: &str = "cooldown";

fn non_empty(value: Option<OsString>) -> Option<PathBuf> {
    value.filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// `override` if set and non-empty, else `<data_local_dir>/cooldown/inbox`
/// (`%LOCALAPPDATA%\cooldown\inbox` on Windows).
pub fn resolve_inbox_dir(
    override_dir: Option<OsString>,
    data_local_dir: Option<PathBuf>,
) -> Option<PathBuf> {
    non_empty(override_dir).or_else(|| data_local_dir.map(|d| d.join(APP_DIR).join("inbox")))
}

/// `<override or config_dir/cooldown>/chain.json` (`%APPDATA%\cooldown` on
/// Windows).
pub fn resolve_chain_path(
    override_dir: Option<OsString>,
    config_dir: Option<PathBuf>,
) -> Option<PathBuf> {
    non_empty(override_dir)
        .or_else(|| config_dir.map(|d| d.join(APP_DIR)))
        .map(|d| d.join(CHAIN_FILE_NAME))
}

/// `<home>/.claude/settings.json`.
pub fn resolve_settings_path(home_dir: Option<PathBuf>) -> Option<PathBuf> {
    home_dir.map(|h| h.join(".claude").join("settings.json"))
}

/// The inbox directory for this process.
pub fn inbox_dir() -> Option<PathBuf> {
    resolve_inbox_dir(std::env::var_os(ENV_INBOX_DIR), dirs::data_local_dir())
}

/// The chain config path for this process.
pub fn chain_config_path() -> Option<PathBuf> {
    resolve_chain_path(std::env::var_os(ENV_CONFIG_DIR), dirs::config_dir())
}

/// The default Claude settings file for the current user.
pub fn default_settings_path() -> Option<PathBuf> {
    resolve_settings_path(dirs::home_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn inbox_default_is_under_data_local_dir() {
        let got = resolve_inbox_dir(None, Some(PathBuf::from("/data/local"))).unwrap();
        assert_eq!(got, Path::new("/data/local/cooldown/inbox"));
    }

    #[test]
    fn inbox_override_wins_and_is_used_verbatim() {
        let got = resolve_inbox_dir(
            Some(OsString::from("/tmp/x")),
            Some(PathBuf::from("/data/local")),
        )
        .unwrap();
        assert_eq!(got, Path::new("/tmp/x"));
    }

    #[test]
    fn empty_override_is_ignored() {
        let got = resolve_inbox_dir(Some(OsString::new()), Some(PathBuf::from("/d"))).unwrap();
        assert_eq!(got, Path::new("/d/cooldown/inbox"));
        assert_eq!(resolve_inbox_dir(Some(OsString::new()), None), None);
    }

    #[test]
    fn chain_path_default_and_override() {
        let got = resolve_chain_path(None, Some(PathBuf::from("/cfg"))).unwrap();
        assert_eq!(got, Path::new("/cfg/cooldown/chain.json"));
        let got = resolve_chain_path(Some(OsString::from("/o")), Some(PathBuf::from("/cfg")));
        assert_eq!(got.unwrap(), Path::new("/o/chain.json"));
    }

    #[test]
    fn settings_path_is_dot_claude_in_home() {
        let got = resolve_settings_path(Some(PathBuf::from("/home/u"))).unwrap();
        assert_eq!(got, Path::new("/home/u/.claude/settings.json"));
        assert_eq!(resolve_settings_path(None), None);
    }
}
