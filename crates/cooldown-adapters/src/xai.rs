//! xAI: the prepaid balance from the Management API (`https://management-api.x.ai`).
//!
//! Needs a *management key* (not an inference key). Each poll makes two requests:
//!
//! 1. `GET /auth/management-keys/validation` checks the key and tells which team it belongs to.
//! 2. `GET /v1/billing/teams/{team}/prepaid/balance` returns `{"changes": [...], "total": {"val":
//!    "-2500"}}`, where `total.val` is a string of USD cents in an inverted ledger (purchases are
//!    negative), so the remaining balance is `-val / 100`. A negative result is kept with a note.
//!
//! A team id set in the configuration wins over the one the validation reports. A key scoped to a
//! whole organization names an organization, not a team, so then the team id must be configured.
//! Ported in idea from the MIT-licensed `ai-usagebar` (`src/grok`); see `THIRD_PARTY_NOTICES.md`.
//!
//! The management key is read from the [`SecretStore`] under `xai:<account>`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cooldown_core::adapter::{Adapter, PollContext};
use cooldown_core::model::{Snapshot, SourceKind, WindowKind};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::http::{join_path, validate_base_url, ApiClient, HttpSettings};
use crate::keys::load_key;
use crate::mapping::{
    balance_failure, carry_or_unavailable, decimal, http_failure, make, obj, round_money,
    MapContext, Meta,
};
use crate::secrets::{secret_name, SecretStore};
use crate::{validate_account, ConfigError, ADAPTER_VERSION};

/// `source.adapter`, fixture directory name and the first half of the secret name.
pub const ADAPTER_ID: &str = "xai";

/// Production base URL.
pub const DEFAULT_BASE_URL: &str = "https://management-api.x.ai";

const NAME: &str = "xAI";

const META: Meta = Meta {
    id: ADAPTER_ID,
    provider: "xai",
    kind: SourceKind::Official,
};

/// Longest team id accepted.
const MAX_TEAM_ID_LEN: usize = 128;

/// Map the response body of the prepaid-balance request (with `ctx.http_status` set) to the
/// `balance` snapshot. Pure.
pub fn map_input(input: &Value, ctx: &MapContext) -> Vec<Snapshot> {
    let fail = || balance_failure(&META, ctx, NAME);
    if http_failure(NAME, ctx).is_some() {
        return fail();
    }
    let total = obj(input.get("total")).and_then(|total| decimal(total.get("val")));
    let Some(cents) = total else {
        return fail();
    };
    // Inverted ledger: purchases are negative, so the remaining balance is -cents / 100.
    let remaining = round_money(-cents / 100.0);
    let mut snapshot = make(&META, ctx, WindowKind::Balance);
    snapshot.remaining = Some(remaining);
    if remaining < 0.0 {
        snapshot.note = Some("Prepaid balance is negative (more spent than purchased)".to_owned());
    }
    vec![snapshot]
}

/// True for a plain identifier: 1 to 128 of letters, digits, `-` and `_`. Anything else must not
/// be placed in a URL path.
pub fn is_valid_team_id(team: &str) -> bool {
    !team.is_empty()
        && team.len() <= MAX_TEAM_ID_LEN
        && team
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Why the team could not be determined from the validation response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamProblem {
    /// An organization-scoped key without a `teamId`: the team must be configured.
    OrganizationScope,
    /// The response named no usable team.
    NoTeam,
    /// A scope this adapter does not know.
    UnknownScope,
}

impl TeamProblem {
    fn reason(self) -> &'static str {
        match self {
            TeamProblem::OrganizationScope => {
                "This xAI management key is organization-scoped. Fill in the Team ID in Settings and save the key again"
            }
            TeamProblem::NoTeam => {
                "xAI did not say which team this key belongs to. Fill in the Team ID in Settings and save the key again"
            }
            TeamProblem::UnknownScope => {
                "xAI returned an unrecognized key scope. Fill in the Team ID in Settings and save the key again"
            }
        }
    }
}

/// The team a validated management key belongs to.
///
/// `scopeId` is the team only for a team-scoped key (or a legacy response without `scope`); for an
/// organization-scoped key it is the organization, which must not be used as a team, so only the
/// deprecated `teamId` is accepted then.
pub fn resolve_team(validation: &Value) -> Result<String, TeamProblem> {
    let text = |key: &str| {
        validation
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| is_valid_team_id(v))
            .map(str::to_owned)
    };
    let scope = validation
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase();
    match scope.as_str() {
        "SCOPE_ORGANIZATION" => text("teamId").ok_or(TeamProblem::OrganizationScope),
        "" | "SCOPE_TEAM" => text("scopeId")
            .or_else(|| text("teamId"))
            .ok_or(TeamProblem::NoTeam),
        _ => Err(TeamProblem::UnknownScope),
    }
}

/// Settings of [`XaiAdapter`]. The management key is NOT here; it lives in the secret store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct XaiConfig {
    /// Local account label; the management key is stored under `xai:<account>`.
    pub account: String,
    /// The team whose prepaid balance to read. `None` uses the team the key belongs to.
    pub team_id: Option<String>,
    /// API base URL. `https://` only (plain `http://` for loopback, so tests can use a mock).
    pub base_url: String,
    /// Seconds allowed per request.
    pub request_timeout_secs: u64,
    /// Seconds between polls.
    pub poll_interval_secs: u64,
    /// Honor the system proxy settings.
    pub use_system_proxy: bool,
}

impl Default for XaiConfig {
    fn default() -> Self {
        XaiConfig {
            account: "default".to_owned(),
            team_id: None,
            base_url: DEFAULT_BASE_URL.to_owned(),
            request_timeout_secs: 15,
            poll_interval_secs: 600,
            use_system_proxy: true,
        }
    }
}

/// Reads an xAI team's prepaid balance.
#[derive(Debug, Clone)]
pub struct XaiAdapter {
    config: XaiConfig,
    base: Url,
    secret: String,
    secrets: Arc<dyn SecretStore>,
    client: ApiClient,
}

impl XaiAdapter {
    /// Create the adapter. Fails on a bad account label, base URL or team id.
    pub fn new(config: XaiConfig, secrets: Arc<dyn SecretStore>) -> Result<Self, ConfigError> {
        validate_account(META.provider, &config.account)?;
        if let Some(team) = &config.team_id {
            if !is_valid_team_id(team) {
                return Err(ConfigError::InvalidTeamId);
            }
        }
        let base = validate_base_url(&config.base_url)?;
        let client = ApiClient::new(&HttpSettings {
            request_timeout: Duration::from_secs(config.request_timeout_secs.max(1)),
            use_system_proxy: config.use_system_proxy,
        })?;
        Ok(XaiAdapter {
            base,
            secret: secret_name(ADAPTER_ID, &config.account),
            config,
            secrets,
            client,
        })
    }

    /// The settings in use.
    pub fn config(&self) -> &XaiConfig {
        &self.config
    }

    /// The name of the secret this adapter reads (`xai:<account>`).
    pub fn secret_name(&self) -> &str {
        &self.secret
    }
}

#[async_trait]
impl Adapter for XaiAdapter {
    fn id(&self) -> &'static str {
        ADAPTER_ID
    }

    fn version(&self) -> &'static str {
        ADAPTER_VERSION
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.config.poll_interval_secs.max(1))
    }

    async fn poll(&self, ctx: &PollContext) -> Vec<Snapshot> {
        let map_ctx = MapContext::from_poll(ctx, &self.config.account, ADAPTER_VERSION);
        let failure = |reason: &str| {
            vec![carry_or_unavailable(
                &META,
                &map_ctx,
                WindowKind::Balance,
                reason,
                None,
            )]
        };
        let transport = |error: crate::http::HttpError| {
            failure(&format!("{NAME} request failed ({})", error.phrase()))
        };
        let key = match load_key(&self.secrets, &self.secret).await {
            Ok(key) => key,
            Err(problem) => return failure(&problem.reason("xAI management key")),
        };

        // 1. Validate the management key (and learn its team).
        let validation_url = join_path(&self.base, "/auth/management-keys/validation");
        let validation = match self.client.get_json(&validation_url, &key).await {
            Ok(response) => response,
            Err(error) => return transport(error),
        };
        if !validation.is_success() {
            return map_input(
                &Value::Null,
                &map_ctx.clone().with_http_status(validation.status),
            );
        }
        let team = match &self.config.team_id {
            Some(team) => team.clone(),
            None => match resolve_team(&validation.json().unwrap_or(Value::Null)) {
                Ok(team) => team,
                Err(problem) => return failure(problem.reason()),
            },
        };

        // 2. The prepaid balance of that team.
        let balance_url = join_path(
            &self.base,
            &format!("/v1/billing/teams/{team}/prepaid/balance"),
        );
        match self.client.get_json(&balance_url, &key).await {
            Ok(response) => {
                let body = if response.is_success() {
                    response.json().unwrap_or(Value::Null)
                } else {
                    Value::Null
                };
                map_input(&body, &map_ctx.clone().with_http_status(response.status))
            }
            Err(error) => transport(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cooldown_core::model::Status;
    use serde_json::json;
    use time::macros::datetime;

    fn ctx() -> MapContext {
        MapContext::new(datetime!(2026-10-10 12:00:00 UTC), "default", "0.1.0")
    }

    #[test]
    fn the_ledger_is_inverted() {
        let out = map_input(&json!({"changes": [], "total": {"val": "-2500"}}), &ctx());
        assert_eq!(out[0].remaining, Some(25.0));
        assert_eq!(out[0].note, None);
        let out = map_input(&json!({"total": {"val": "0"}}), &ctx());
        assert_eq!(out[0].remaining, Some(0.0));
        assert!(out[0].remaining.unwrap().is_sign_positive());
        let out = map_input(&json!({"total": {"val": "150"}}), &ctx());
        assert_eq!(out[0].remaining, Some(-1.5));
        assert!(out[0].note.as_deref().unwrap().contains("negative"));
    }

    #[test]
    fn a_200_error_envelope_is_not_a_zero_balance() {
        for body in [
            json!({}),
            json!({"total": {}}),
            json!({"total": {"val": ""}}),
            json!({"total": {"val": "n/a"}}),
            json!({"total": {"val": 5}}),
        ] {
            let out = map_input(&body, &ctx().with_http_status(200));
            assert_eq!(out[0].status, Status::Unavailable, "{body}");
            assert_eq!(out[0].remaining, None, "{body}");
        }
    }

    #[test]
    fn team_resolution_follows_the_key_scope() {
        assert_eq!(
            resolve_team(&json!({"scope": "SCOPE_TEAM", "scopeId": "team-1"})),
            Ok("team-1".to_owned())
        );
        // Legacy response without a scope.
        assert_eq!(
            resolve_team(&json!({"scopeId": "team-y"})),
            Ok("team-y".to_owned())
        );
        assert_eq!(
            resolve_team(&json!({"scopeId": null, "teamId": "team-x"})),
            Ok("team-x".to_owned())
        );
        // An organization id is never a team.
        assert_eq!(
            resolve_team(&json!({"scope": "SCOPE_ORGANIZATION", "scopeId": "org-123"})),
            Err(TeamProblem::OrganizationScope)
        );
        assert_eq!(
            resolve_team(
                &json!({"scope": "SCOPE_ORGANIZATION", "scopeId": "org-123", "teamId": "team-9"})
            ),
            Ok("team-9".to_owned())
        );
        assert_eq!(resolve_team(&json!({})), Err(TeamProblem::NoTeam));
        assert_eq!(
            resolve_team(&json!({"scope": "SCOPE_GALAXY", "scopeId": "x-1"})),
            Err(TeamProblem::UnknownScope)
        );
        // A team id that could escape the URL path is refused.
        assert_eq!(
            resolve_team(&json!({"scopeId": "../../etc"})),
            Err(TeamProblem::NoTeam)
        );
    }

    #[test]
    fn team_ids_must_be_plain_identifiers() {
        assert!(is_valid_team_id("0b1d9c2e-1111-4222-8333-444455556666"));
        assert!(is_valid_team_id("team_1"));
        for bad in [
            "",
            "a/b",
            "a b",
            "a?x=1",
            "a#b",
            "..",
            "é",
            &"a".repeat(129),
        ] {
            assert!(!is_valid_team_id(bad), "{bad}");
        }
    }
}
