//! Cooldown source adapters.
//!
//! Each adapter implements [`cooldown_core::Adapter`]: it reads one source and returns
//! [`cooldown_core::Snapshot`]s, and `poll` never fails (problems become `degraded`, `stale` or
//! `unavailable` snapshots that keep the last good value).
//!
//! | Adapter | Source | `source.kind` |
//! |---|---|---|
//! | [`claude::ClaudeInboxAdapter`] | inbox file written by the `cooldown-statusline` hook | official |
//! | [`codex_app_server::CodexAppServerAdapter`] | `codex app-server --listen stdio://` | scraped |
//! | [`codex_rollout::CodexRolloutAdapter`] | `~/.codex/sessions/**/rollout-*.jsonl` tail | scraped |
//! | [`openrouter::OpenRouterAdapter`] | `GET /api/v1/key` | official |
//! | [`deepseek::DeepSeekAdapter`] | `GET /user/balance` | official |
//! | [`xai::XaiAdapter`] | management API prepaid balance | official |
//! | [`kimi::KimiAdapter`] | `GET /v1/users/me/balance` | official |
//! | [`copilot::CopilotAdapter`] | GitHub `GET /users/{login}/settings/billing/ai_credit/usage` | official |
//!
//! Every adapter has a pure `map_input(&serde_json::Value, &MapContext) -> Vec<Snapshot>` function.
//! The fixtures in `packages/snapshot-schema/fixtures/adapters` run through exactly those functions
//! (see `tests/fixtures.rs`), so the mapping rules cannot drift from the shared contract.
//!
//! Safety rules, enforced by design and by tests:
//!
//! - No credential file is ever read (not `~/.claude/.credentials.json`, not `~/.codex/auth.json`),
//!   no browser cookies, no OS keychain except Cooldown's own [`secrets::SecretStore`] entries,
//!   and no undocumented usage endpoint is called.
//! - API keys live only in a [`secrets::SecretStore`]. They are never put in configuration, never
//!   logged, never placed in a snapshot note and never part of an error message ([`scrub`]).
//! - HTTP responses are capped at 2 MiB, redirects never leave the original origin, and every
//!   request has a timeout.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod antigravity;
pub mod claude;
pub mod codex_app_server;
pub mod codex_launcher;
pub mod codex_rollout;
pub mod copilot;
pub mod deepseek;
pub mod http;
pub mod kimi;
pub mod mapping;
pub mod openrouter;
pub mod scrub;
pub mod secrets;
pub mod xai;

mod codex_map;
mod keys;

pub use antigravity::{AntigravityInboxAdapter, AntigravityInboxConfig};
pub use claude::{ClaudeInboxAdapter, ClaudeInboxConfig};
pub use codex_app_server::{CodexAppServerAdapter, CodexAppServerConfig};
pub use codex_rollout::{CodexRolloutAdapter, CodexRolloutConfig};
pub use copilot::{CopilotAdapter, CopilotConfig};
pub use deepseek::{DeepSeekAdapter, DeepSeekConfig};
pub use kimi::{KimiAdapter, KimiConfig};
pub use mapping::MapContext;
pub use openrouter::{OpenRouterAdapter, OpenRouterConfig};
pub use secrets::{InMemorySecretStore, KeyringSecretStore, SecretStore, SecretString};
pub use xai::{XaiAdapter, XaiConfig};

/// The `source.adapterVersion` every adapter in this crate stamps on its snapshots. Bump it when a
/// mapping rule changes in a way consumers could notice.
pub const ADAPTER_VERSION: &str = "0.1.0";

/// A configuration problem found when an adapter is constructed (never at poll time: polling
/// problems become snapshots).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// The account label breaks the snapshot rules (empty, `@`, `:`, token-like, ...).
    #[error("invalid account label: {0}")]
    InvalidAccount(String),
    /// A base URL is not `https://`, or `http://` to a loopback address (tests).
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(&'static str),
    /// A configured path points at a credential file. Cooldown never reads those.
    #[error("refusing to read a credential file: {0}")]
    ForbiddenPath(&'static str),
    /// A team id is not a plain identifier.
    #[error(
        "invalid team id: only letters, digits, '-' and '_' are allowed (1 to 128 characters)"
    )]
    InvalidTeamId,
    /// The HTTPS client could not be built.
    #[error("could not set up the HTTPS client: {0}")]
    HttpClient(String),
}

/// Check that `account` is a legal account label by building a throwaway snapshot id and running
/// the core validator on it.
pub(crate) fn validate_account(provider: &str, account: &str) -> Result<(), ConfigError> {
    use cooldown_core::model::{SnapshotSource, SourceKind, Unit, WindowKind};
    let probe = cooldown_core::Snapshot::new(
        provider,
        account,
        WindowKind::Balance,
        Unit::Usd,
        SnapshotSource {
            kind: SourceKind::Official,
            adapter: "probe".to_owned(),
            adapter_version: ADAPTER_VERSION.to_owned(),
        },
        time::OffsetDateTime::UNIX_EPOCH,
    );
    probe
        .validate()
        .map_err(|e| ConfigError::InvalidAccount(e.to_string()))
}

/// File names that hold credentials. No adapter is ever configured to read one: a settings file
/// that names one of these (by mistake or by a hostile edit) is refused up front.
const CREDENTIAL_FILE_NAMES: [&str; 4] = [
    ".credentials.json",
    "credentials.json",
    "auth.json",
    "oauth_creds.json",
];

/// Refuse a path whose file name is a known credential file.
pub(crate) fn refuse_credential_file(path: &std::path::Path) -> Result<(), ConfigError> {
    let name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase);
    match name {
        Some(name) if CREDENTIAL_FILE_NAMES.contains(&name.as_str()) => Err(
            ConfigError::ForbiddenPath("this looks like a credential file, not a Cooldown file"),
        ),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_files_are_refused() {
        use std::path::Path;
        for bad in [
            "/home/me/.claude/.credentials.json",
            "/home/me/.codex/auth.json",
            "C:/Users/me/.codex/AUTH.JSON",
            "credentials.json",
        ] {
            assert!(refuse_credential_file(Path::new(bad)).is_err(), "{bad}");
        }
        for fine in [
            "/home/me/.local/share/cooldown/inbox/claude-statusline.json",
            "/tmp/inbox.json",
        ] {
            assert!(refuse_credential_file(Path::new(fine)).is_ok(), "{fine}");
        }
    }

    #[test]
    fn account_labels_are_validated_like_snapshots() {
        assert_eq!(validate_account("claude", "default"), Ok(()));
        assert!(validate_account("claude", "").is_err());
        assert!(validate_account("claude", "me@example.com").is_err());
        assert!(validate_account("claude", "a:b").is_err());
        assert!(validate_account("claude", "sk-live-1234").is_err());
    }
}
