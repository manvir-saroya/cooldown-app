//! Which adapters the desktop app runs, and how each one is described in Settings.
//!
//! Claude and Codex need nothing from the user and always run. The three API-balance adapters run
//! only once a key is saved: without one they would only produce "no key" cards on the overview.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cooldown_adapters::{
    AntigravityInboxAdapter, AntigravityInboxConfig, ClaudeInboxAdapter, ClaudeInboxConfig,
    CodexAppServerAdapter, CodexAppServerConfig, CodexRolloutAdapter, CodexRolloutConfig,
    CopilotAdapter, CopilotConfig, DeepSeekAdapter, DeepSeekConfig, KimiAdapter, KimiConfig,
    OpenRouterAdapter, OpenRouterConfig, SecretStore, XaiAdapter, XaiConfig, ADAPTER_VERSION,
};
use cooldown_core::{is_failed_poll, Adapter, PollContext, Snapshot, SourceKind, Status};

use crate::settings::Settings;

/// What an adapter needs from the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credentials {
    /// Nothing.
    None,
    /// An API key.
    ApiKey,
    /// An xAI management key (plus an optional team id).
    ManagementKey,
    /// A GitHub fine-grained token with the Plan permission (read).
    GithubToken,
}

impl Credentials {
    /// The UI's spelling (`AdapterInfo.credentials`).
    pub fn as_str(self) -> &'static str {
        match self {
            Credentials::None => "none",
            Credentials::ApiKey => "api-key",
            Credentials::ManagementKey => "management-key",
            Credentials::GithubToken => "github-token",
        }
    }
}

/// One row of the Settings "Sources" list.
#[derive(Debug, Clone, Copy)]
pub struct CatalogEntry {
    /// The id the registry reports (`PollEvent.adapter`) and the UI passes back.
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Provider slug.
    pub provider: &'static str,
    /// Where the numbers come from.
    pub kind: SourceKind,
    /// What the user must supply.
    pub credentials: Credentials,
    /// Shown on the dashboard until the user switches it off. Sources that need a separate tool
    /// set up (a hook in another agent) start off, so a fresh install shows no empty panels.
    pub on_by_default: bool,
}

impl CatalogEntry {
    /// Whether the user wants this source: their choice, or the default.
    pub fn enabled(&self, settings: &Settings) -> bool {
        settings
            .sources
            .get(self.id)
            .copied()
            .unwrap_or(self.on_by_default)
    }
}

/// The id of the combined Codex adapter (app-server first, session logs as fallback).
pub const CODEX_ID: &str = "codex";

/// Every adapter the desktop app knows, in the order Settings lists them.
pub const CATALOG: [CatalogEntry; 8] = [
    CatalogEntry {
        id: cooldown_adapters::claude::ADAPTER_ID,
        name: "Claude Code statusline",
        provider: "claude",
        kind: SourceKind::Official,
        credentials: Credentials::None,
        on_by_default: true,
    },
    CatalogEntry {
        id: cooldown_adapters::antigravity::ADAPTER_ID,
        name: "Antigravity CLI statusline",
        provider: "antigravity",
        kind: SourceKind::Official,
        credentials: Credentials::None,
        // Needs its own hook installed in Antigravity CLI first.
        on_by_default: false,
    },
    CatalogEntry {
        id: CODEX_ID,
        name: "Codex (app-server, then session logs)",
        provider: "codex",
        kind: SourceKind::Scraped,
        credentials: Credentials::None,
        on_by_default: true,
    },
    CatalogEntry {
        id: cooldown_adapters::openrouter::ADAPTER_ID,
        name: "OpenRouter",
        provider: "openrouter",
        kind: SourceKind::Official,
        credentials: Credentials::ApiKey,
        on_by_default: true,
    },
    CatalogEntry {
        id: cooldown_adapters::deepseek::ADAPTER_ID,
        name: "DeepSeek",
        provider: "deepseek",
        kind: SourceKind::Official,
        credentials: Credentials::ApiKey,
        on_by_default: true,
    },
    CatalogEntry {
        id: cooldown_adapters::xai::ADAPTER_ID,
        name: "xAI",
        provider: "xai",
        kind: SourceKind::Official,
        credentials: Credentials::ManagementKey,
        on_by_default: true,
    },
    CatalogEntry {
        id: cooldown_adapters::kimi::ADAPTER_ID,
        name: "Kimi (Moonshot)",
        provider: "kimi",
        kind: SourceKind::Official,
        credentials: Credentials::ApiKey,
        on_by_default: true,
    },
    CatalogEntry {
        id: cooldown_adapters::copilot::ADAPTER_ID,
        name: "GitHub Copilot",
        provider: "copilot",
        kind: SourceKind::Official,
        credentials: Credentials::GithubToken,
        on_by_default: true,
    },
];

/// One line on how a source connects, shown under its name in Settings.
pub fn setup_hint(id: &str) -> Option<&'static str> {
    Some(match id {
        "claude-statusline" => "Claude Code passes your limits to its status line. Set up the hook below.",
        "codex" => "Read from the Codex CLI on this PC (its app-server, then its session logs).",
        "antigravity-statusline" => {
            "Antigravity CLI (formerly Gemini CLI) passes its quota to its status line. Install the hook to connect it."
        }
        "copilot" => "AI credits spent this month, from GitHub's billing API.",
        "kimi" => "Prepaid balance of a platform.kimi.ai API key.",
        "openrouter" | "deepseek" | "xai" => "Prepaid balance, read with your own key.",
        _ => return None,
    })
}

/// The catalog entry for `id`.
pub fn catalog_entry(id: &str) -> Option<&'static CatalogEntry> {
    CATALOG.iter().find(|entry| entry.id == id)
}

/// The account label every desktop adapter uses. One account per provider for v1.
pub const ACCOUNT: &str = "default";

/// The name a key is stored under for a catalog adapter (`<adapter id>:default`).
pub fn secret_name(adapter_id: &str) -> String {
    cooldown_adapters::secrets::secret_name(adapter_id, ACCOUNT)
}

/// Adapters that could not be configured, as `(adapter id, reason)`.
pub type BuildProblems = Vec<(&'static str, String)>;

/// Build the adapters to run. `has_key` says whether a key is saved for an adapter id. Adapters
/// that cannot be configured are left out and reported as `(id, reason)`.
pub fn build(
    settings: &Settings,
    secrets: Arc<dyn SecretStore>,
    has_key: &dyn Fn(&str) -> bool,
) -> (Vec<Arc<dyn Adapter>>, BuildProblems) {
    let mut adapters: Vec<Arc<dyn Adapter>> = Vec::new();
    let mut problems = Vec::new();
    let mut add = |id: &'static str, built: Result<Arc<dyn Adapter>, String>| match built {
        Ok(adapter) => adapters.push(adapter),
        Err(reason) => problems.push((id, reason)),
    };

    let on = |id: &str| catalog_entry(id).is_some_and(|e| e.enabled(settings));

    if on(cooldown_adapters::claude::ADAPTER_ID) {
        add(
            cooldown_adapters::claude::ADAPTER_ID,
            ClaudeInboxAdapter::new(ClaudeInboxConfig::default())
                .map(|a| Arc::new(a) as Arc<dyn Adapter>)
                .map_err(|e| e.to_string()),
        );
    }

    if on(cooldown_adapters::antigravity::ADAPTER_ID) {
        add(
            cooldown_adapters::antigravity::ADAPTER_ID,
            AntigravityInboxAdapter::new(AntigravityInboxConfig::default())
                .map(|a| Arc::new(a) as Arc<dyn Adapter>)
                .map_err(|e| e.to_string()),
        );
    }

    if on(CODEX_ID) {
        let codex = CodexAppServerAdapter::new(CodexAppServerConfig::default())
            .and_then(|primary| {
                CodexRolloutAdapter::new(CodexRolloutConfig::default())
                    .map(|fallback| (primary, fallback))
            })
            .map(|(primary, fallback)| {
                Arc::new(FallbackAdapter::new(
                    CODEX_ID,
                    Arc::new(primary),
                    Arc::new(fallback),
                )) as Arc<dyn Adapter>
            })
            .map_err(|e| e.to_string());
        add(CODEX_ID, codex);
    }

    let openrouter = cooldown_adapters::openrouter::ADAPTER_ID;
    if on(openrouter) && has_key(openrouter) {
        add(
            openrouter,
            OpenRouterAdapter::new(OpenRouterConfig::default(), secrets.clone())
                .map(|a| Arc::new(a) as Arc<dyn Adapter>)
                .map_err(|e| e.to_string()),
        );
    }

    let deepseek = cooldown_adapters::deepseek::ADAPTER_ID;
    if on(deepseek) && has_key(deepseek) {
        add(
            deepseek,
            DeepSeekAdapter::new(DeepSeekConfig::default(), secrets.clone())
                .map(|a| Arc::new(a) as Arc<dyn Adapter>)
                .map_err(|e| e.to_string()),
        );
    }

    let xai = cooldown_adapters::xai::ADAPTER_ID;
    if on(xai) && has_key(xai) {
        let config = XaiConfig {
            team_id: settings.xai_team_id.clone(),
            ..XaiConfig::default()
        };
        add(
            xai,
            XaiAdapter::new(config, secrets.clone())
                .map(|a| Arc::new(a) as Arc<dyn Adapter>)
                .map_err(|e| e.to_string()),
        );
    }

    let kimi = cooldown_adapters::kimi::ADAPTER_ID;
    if on(kimi) && has_key(kimi) {
        add(
            kimi,
            KimiAdapter::new(KimiConfig::default(), secrets.clone())
                .map(|a| Arc::new(a) as Arc<dyn Adapter>)
                .map_err(|e| e.to_string()),
        );
    }

    let copilot = cooldown_adapters::copilot::ADAPTER_ID;
    if on(copilot) && has_key(copilot) {
        let config = CopilotConfig {
            monthly_allowance_usd: settings
                .copilot_allowance_cents
                .map(|c| f64::from(c) / 100.0),
            ..CopilotConfig::default()
        };
        add(
            copilot,
            CopilotAdapter::new(config, secrets)
                .map(|a| Arc::new(a) as Arc<dyn Adapter>)
                .map_err(|e| e.to_string()),
        );
    }

    (adapters, problems)
}

/// Runs `primary`; when that poll fails as a whole (or returns nothing), runs `fallback` and uses
/// its answer if it has a usable one. Both read the same windows, so they share snapshot ids and
/// must never be registered side by side (they would overwrite each other every poll).
pub struct FallbackAdapter {
    id: &'static str,
    primary: Arc<dyn Adapter>,
    fallback: Arc<dyn Adapter>,
}

impl FallbackAdapter {
    /// Combine two adapters under one id.
    pub fn new(id: &'static str, primary: Arc<dyn Adapter>, fallback: Arc<dyn Adapter>) -> Self {
        FallbackAdapter {
            id,
            primary,
            fallback,
        }
    }
}

#[async_trait]
impl Adapter for FallbackAdapter {
    fn id(&self) -> &'static str {
        self.id
    }

    fn version(&self) -> &'static str {
        ADAPTER_VERSION
    }

    fn poll_interval(&self) -> Duration {
        self.primary.poll_interval()
    }

    async fn poll(&self, ctx: &PollContext) -> Vec<Snapshot> {
        let primary = self.primary.poll(ctx).await;
        if !primary.is_empty() && !is_failed_poll(&primary) {
            return primary;
        }
        let fallback = self.fallback.poll(ctx).await;
        if !fallback.is_empty() && !is_failed_poll(&fallback) {
            return fallback;
        }
        // Neither has figures: report the primary's reason, it is the better explanation.
        if primary.is_empty() {
            fallback
        } else {
            primary
        }
    }
}

/// How an adapter is doing, from its last poll.
#[derive(Debug, Clone, PartialEq)]
pub struct AdapterRuntime {
    /// When the last poll started.
    pub last_polled_at: time::OffsetDateTime,
    /// The worst status among the windows it returned.
    pub status: Status,
    /// The note of a window with that status, when it is not `ok`.
    pub note: Option<String>,
}

/// Sum up one poll for the Settings list: the worst status wins, with its note.
pub fn summarize(polled_at: time::OffsetDateTime, snapshots: &[Snapshot]) -> AdapterRuntime {
    match snapshots.iter().max_by_key(|s| s.status.severity()) {
        None => AdapterRuntime {
            last_polled_at: polled_at,
            status: Status::Unavailable,
            note: Some("The source returned nothing.".to_owned()),
        },
        Some(worst) => AdapterRuntime {
            last_polled_at: polled_at,
            status: worst.status,
            note: if worst.status == Status::Ok {
                None
            } else {
                worst.note.clone()
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cooldown_adapters::InMemorySecretStore;
    use cooldown_core::{AdapterFailure, SnapshotSource, SnapshotTemplate, Unit, WindowKind};
    use std::sync::atomic::{AtomicU32, Ordering};
    use time::macros::datetime;

    const NOW: time::OffsetDateTime = datetime!(2026-10-15 12:00:00 UTC);

    fn source(adapter: &str) -> SnapshotSource {
        SnapshotSource {
            kind: SourceKind::Scraped,
            adapter: adapter.into(),
            adapter_version: "0.1.0".into(),
        }
    }

    fn good(adapter: &str) -> Snapshot {
        let mut s = Snapshot::new(
            "codex",
            "default",
            WindowKind::Session5h,
            Unit::Percent,
            source(adapter),
            NOW,
        );
        s.percent_used = Some(12.0);
        s.resets_at = Some(NOW + time::Duration::hours(1));
        s
    }

    fn failed(adapter: &str) -> Snapshot {
        let template = SnapshotTemplate {
            provider: "codex".into(),
            account: "default".into(),
            window: WindowKind::Session5h,
            unit: Unit::Percent,
            source: source(adapter),
        };
        AdapterFailure::unavailable(format!("{adapter} is down"))
            .into_snapshot(&template, None, NOW)
    }

    struct Fixed {
        out: Vec<Snapshot>,
        polls: AtomicU32,
    }

    impl Fixed {
        fn new(out: Vec<Snapshot>) -> Arc<Self> {
            Arc::new(Fixed {
                out,
                polls: AtomicU32::new(0),
            })
        }
    }

    #[async_trait]
    impl Adapter for Fixed {
        fn id(&self) -> &'static str {
            "fixed"
        }
        fn version(&self) -> &'static str {
            "1"
        }
        fn poll_interval(&self) -> Duration {
            Duration::from_secs(42)
        }
        async fn poll(&self, _ctx: &PollContext) -> Vec<Snapshot> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            self.out.clone()
        }
    }

    #[tokio::test]
    async fn primary_success_skips_the_fallback() {
        let primary = Fixed::new(vec![good("primary")]);
        let fallback = Fixed::new(vec![good("fallback")]);
        let adapter = FallbackAdapter::new("codex", primary.clone(), fallback.clone());
        let out = adapter.poll(&PollContext::new(NOW)).await;
        assert_eq!(out[0].source.adapter, "primary");
        assert_eq!(fallback.polls.load(Ordering::SeqCst), 0);
        assert_eq!(adapter.poll_interval(), Duration::from_secs(42));
        assert_eq!(adapter.id(), "codex");
    }

    #[tokio::test]
    async fn primary_failure_uses_a_working_fallback() {
        let adapter = FallbackAdapter::new(
            "codex",
            Fixed::new(vec![failed("primary")]),
            Fixed::new(vec![good("fallback")]),
        );
        let out = adapter.poll(&PollContext::new(NOW)).await;
        assert_eq!(out[0].source.adapter, "fallback");
        assert_eq!(out[0].status, Status::Ok);
    }

    #[tokio::test]
    async fn both_failing_reports_the_primary_reason() {
        let adapter = FallbackAdapter::new(
            "codex",
            Fixed::new(vec![failed("primary")]),
            Fixed::new(vec![failed("fallback")]),
        );
        let out = adapter.poll(&PollContext::new(NOW)).await;
        assert_eq!(out[0].note.as_deref(), Some("primary is down"));

        let empty_primary = FallbackAdapter::new(
            "codex",
            Fixed::new(vec![]),
            Fixed::new(vec![failed("fallback")]),
        );
        let out = empty_primary.poll(&PollContext::new(NOW)).await;
        assert_eq!(out[0].note.as_deref(), Some("fallback is down"));
    }

    #[test]
    fn summarize_takes_the_worst_window() {
        let ok = summarize(NOW, &[good("a")]);
        assert_eq!(ok.status, Status::Ok);
        assert_eq!(ok.note, None);

        let mixed = summarize(NOW, &[good("a"), failed("b")]);
        assert_eq!(mixed.status, Status::Unavailable);
        assert_eq!(mixed.note.as_deref(), Some("b is down"));

        let empty = summarize(NOW, &[]);
        assert_eq!(empty.status, Status::Unavailable);
        assert_eq!(empty.last_polled_at, NOW);
    }

    #[test]
    fn key_adapters_run_only_with_a_key() {
        let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
        let (none, problems) = build(&Settings::default(), secrets.clone(), &|_| false);
        assert!(problems.is_empty(), "{problems:?}");
        let ids: Vec<_> = none.iter().map(|a| a.id()).collect();
        assert_eq!(ids, ["claude-statusline", "codex"]);

        let (all, problems) = build(&Settings::default(), secrets, &|_| true);
        assert!(problems.is_empty(), "{problems:?}");
        let ids: Vec<_> = all.iter().map(|a| a.id()).collect();
        assert_eq!(
            ids,
            [
                "claude-statusline",
                "codex",
                "openrouter",
                "deepseek",
                "xai",
                "kimi",
                "copilot"
            ]
        );
    }

    #[test]
    fn a_bad_team_id_is_reported_not_fatal() {
        let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
        let settings = Settings {
            xai_team_id: Some("bad team!".into()),
            ..Settings::default()
        };
        let (adapters, problems) = build(&settings, secrets, &|id| id == "xai");
        assert_eq!(adapters.len(), 2);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].0, "xai");
    }

    #[test]
    fn catalog_ids_are_unique_and_match_the_registry() {
        let mut ids: Vec<_> = CATALOG.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), CATALOG.len());
        assert_eq!(
            catalog_entry("xai").unwrap().credentials,
            Credentials::ManagementKey
        );
        assert_eq!(secret_name("openrouter"), "openrouter:default");
    }
}
