//! OpenRouter: `GET https://openrouter.ai/api/v1/key`.
//!
//! The response is `{"data": {...}}`. For a key with a spending `limit` the adapter reports
//! `used`, `limit`, `remaining` (which may go negative) and a percentage capped at 100. For an
//! uncapped key it reports `used` only, with a note. Ported in idea from the MIT-licensed
//! `ai-usagebar` (`src/openrouter`); see `THIRD_PARTY_NOTICES.md`.
//!
//! The key is read from the [`SecretStore`] under `openrouter:<account>` for every poll and is
//! sent only as a bearer token. See [`crate::http`] for the body cap, redirect policy and timeouts.

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
    balance_failure, carry_or_unavailable, clamp_percent, http_failure, make, num, obj,
    round_money, round_percent, MapContext, Meta,
};
use crate::secrets::{secret_name, SecretStore};
use crate::{validate_account, ConfigError, ADAPTER_VERSION};

/// `source.adapter`, fixture directory name and the first half of the secret name.
pub const ADAPTER_ID: &str = "openrouter";

/// Production base URL.
pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

const NAME: &str = "OpenRouter";

const META: Meta = Meta {
    id: ADAPTER_ID,
    provider: "openrouter",
    kind: SourceKind::Official,
};

/// Map the response body of `GET /api/v1/key` (with `ctx.http_status` set) to the `balance`
/// snapshot. Pure.
///
/// - `limit` present: `used = limit - limit_remaining` (rounded to 6 decimals), `limit`,
///   `remaining`, and `percentUsed = min(100, used / limit * 100)` (omitted when `limit` is 0).
/// - `limit` null: `used = usage`, with a note.
/// - Non-2xx status, or a body that does not match: `degraded` (value carried) or `unavailable`.
pub fn map_input(input: &Value, ctx: &MapContext) -> Vec<Snapshot> {
    let fail = || balance_failure(&META, ctx, NAME);
    if http_failure(NAME, ctx).is_some() {
        return fail();
    }
    let Some(data) = obj(input.get("data")) else {
        return fail();
    };
    let Some(usage) = num(data.get("usage")).filter(|u| *u >= 0.0) else {
        return fail();
    };
    let mut snapshot = make(&META, ctx, WindowKind::Balance);
    match data.get("limit") {
        None | Some(Value::Null) => {
            snapshot.used = Some(usage);
            snapshot.note = Some("No spending limit set on this key".to_owned());
        }
        Some(limit) => {
            let (Some(limit), Some(remaining)) = (
                num(Some(limit)).filter(|l| *l >= 0.0),
                num(data.get("limit_remaining")),
            ) else {
                return fail();
            };
            let used = round_money(limit - remaining);
            snapshot.percent_used =
                (limit > 0.0).then(|| round_percent(clamp_percent(used / limit * 100.0)));
            snapshot.used = Some(used);
            snapshot.limit = Some(limit);
            snapshot.remaining = Some(remaining);
        }
    }
    vec![snapshot]
}

/// Settings of [`OpenRouterAdapter`]. The API key is NOT here; it lives in the secret store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OpenRouterConfig {
    /// Local account label; the key is stored under `openrouter:<account>`.
    pub account: String,
    /// API base URL. `https://` only (plain `http://` for loopback, so tests can use a mock).
    pub base_url: String,
    /// Seconds allowed per request.
    pub request_timeout_secs: u64,
    /// Seconds between polls.
    pub poll_interval_secs: u64,
    /// Honor the system proxy settings.
    pub use_system_proxy: bool,
}

impl Default for OpenRouterConfig {
    fn default() -> Self {
        OpenRouterConfig {
            account: "default".to_owned(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            request_timeout_secs: 15,
            poll_interval_secs: 300,
            use_system_proxy: true,
        }
    }
}

/// Reads an OpenRouter key's usage and spending limit.
#[derive(Debug, Clone)]
pub struct OpenRouterAdapter {
    config: OpenRouterConfig,
    key_url: Url,
    secret: String,
    secrets: Arc<dyn SecretStore>,
    client: ApiClient,
}

impl OpenRouterAdapter {
    /// Create the adapter. Fails on a bad account label or base URL.
    pub fn new(
        config: OpenRouterConfig,
        secrets: Arc<dyn SecretStore>,
    ) -> Result<Self, ConfigError> {
        validate_account(META.provider, &config.account)?;
        let base = validate_base_url(&config.base_url)?;
        let client = ApiClient::new(&HttpSettings {
            request_timeout: Duration::from_secs(config.request_timeout_secs.max(1)),
            use_system_proxy: config.use_system_proxy,
        })?;
        Ok(OpenRouterAdapter {
            key_url: join_path(&base, "/key"),
            secret: secret_name(ADAPTER_ID, &config.account),
            config,
            secrets,
            client,
        })
    }

    /// The settings in use.
    pub fn config(&self) -> &OpenRouterConfig {
        &self.config
    }

    /// The name of the secret this adapter reads (`openrouter:<account>`).
    pub fn secret_name(&self) -> &str {
        &self.secret
    }
}

#[async_trait]
impl Adapter for OpenRouterAdapter {
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
        let key = match load_key(&self.secrets, &self.secret).await {
            Ok(key) => key,
            Err(problem) => return failure(&problem.reason("OpenRouter API key")),
        };
        match self.client.get_json(&self.key_url, &key).await {
            Ok(response) => {
                let body = if response.is_success() {
                    response.json().unwrap_or(Value::Null)
                } else {
                    Value::Null
                };
                map_input(&body, &map_ctx.clone().with_http_status(response.status))
            }
            Err(error) => failure(&format!("{NAME} request failed ({})", error.phrase())),
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
    fn capped_key() {
        let out = map_input(
            &json!({"data": {"limit": 50, "limit_remaining": 24.5, "usage": 25.5}}),
            &ctx().with_http_status(200),
        );
        assert_eq!(out.len(), 1);
        let s = &out[0];
        assert_eq!(s.used, Some(25.5));
        assert_eq!(s.limit, Some(50.0));
        assert_eq!(s.remaining, Some(24.5));
        assert_eq!(s.percent_used, Some(51.0));
        assert_eq!(s.status, Status::Ok);
    }

    #[test]
    fn overspent_is_clamped_but_remaining_stays_negative() {
        let out = map_input(
            &json!({"data": {"limit": 10, "limit_remaining": -2.5, "usage": 12.5}}),
            &ctx(),
        );
        assert_eq!(out[0].percent_used, Some(100.0));
        assert_eq!(out[0].remaining, Some(-2.5));
    }

    #[test]
    fn a_zero_limit_has_no_percentage() {
        let out = map_input(
            &json!({"data": {"limit": 0, "limit_remaining": 0, "usage": 0}}),
            &ctx(),
        );
        assert_eq!(out[0].percent_used, None);
        assert_eq!(out[0].limit, Some(0.0));
    }

    #[test]
    fn a_missing_envelope_or_usage_is_drift() {
        for body in [
            json!({}),
            json!({"data": {}}),
            json!({"data": {"usage": -1}}),
            json!({"data": {"usage": 1, "limit": 5}}),
            json!(null),
        ] {
            let out = map_input(&body, &ctx().with_http_status(200));
            assert_eq!(out[0].status, Status::Unavailable, "{body}");
            assert_eq!(
                out[0].note.as_deref(),
                Some("OpenRouter response did not match the expected shape")
            );
        }
    }

    #[test]
    fn construction_checks_the_account_and_url() {
        let store: Arc<dyn SecretStore> = Arc::new(crate::secrets::InMemorySecretStore::new());
        assert!(OpenRouterAdapter::new(OpenRouterConfig::default(), store.clone()).is_ok());
        assert!(OpenRouterAdapter::new(
            OpenRouterConfig {
                base_url: "http://openrouter.ai/api/v1".into(),
                ..OpenRouterConfig::default()
            },
            store.clone()
        )
        .is_err());
        assert!(OpenRouterAdapter::new(
            OpenRouterConfig {
                account: "a@b".into(),
                ..OpenRouterConfig::default()
            },
            store
        )
        .is_err());
    }
}
