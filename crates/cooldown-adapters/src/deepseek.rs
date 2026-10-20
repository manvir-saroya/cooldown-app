//! DeepSeek: `GET https://api.deepseek.com/user/balance` with a bearer key (verified).
//!
//! The response lists balances per currency. One balance is reported (`remaining =
//! total_balance`): the `USD` entry when there is one, otherwise the `CNY` entry with unit `cny`.
//! Two entries of the chosen currency count as schema drift. Ported in idea from the MIT-licensed `ai-usagebar`
//! (`src/deepseek`); see `THIRD_PARTY_NOTICES.md`.
//!
//! The key is read from the [`SecretStore`] under `deepseek:<account>`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cooldown_core::adapter::{Adapter, PollContext};
use cooldown_core::model::{Snapshot, SourceKind, Unit, WindowKind};
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
pub const ADAPTER_ID: &str = "deepseek";

/// Production base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";

const NAME: &str = "DeepSeek";

const META: Meta = Meta {
    id: ADAPTER_ID,
    provider: "deepseek",
    kind: SourceKind::Official,
};

/// Map the response body of `GET /user/balance` (with `ctx.http_status` set) to the `balance`
/// snapshot. Pure.
pub fn map_input(input: &Value, ctx: &MapContext) -> Vec<Snapshot> {
    let fail = || balance_failure(&META, ctx, NAME);
    if http_failure(NAME, ctx).is_some() {
        return fail();
    }
    let Some(root) = obj(Some(input)) else {
        return fail();
    };
    let (Some(is_available), Some(infos)) = (
        root.get("is_available").and_then(Value::as_bool),
        root.get("balance_infos").and_then(Value::as_array),
    ) else {
        return fail();
    };
    let infos: Vec<&Value> = infos.iter().filter(|info| info.is_object()).collect();
    let with_currency = |currency: &str| -> Vec<&Value> {
        infos
            .iter()
            .copied()
            .filter(|info| info.get("currency").and_then(Value::as_str) == Some(currency))
            .collect()
    };
    let usd = with_currency("USD");
    let (entries, unit) = if usd.is_empty() {
        (with_currency("CNY"), Unit::Cny)
    } else {
        (usd, Unit::Usd)
    };
    let [entry] = entries.as_slice() else {
        return fail();
    };
    let Some(total) = decimal(entry.get("total_balance")) else {
        return fail();
    };
    let mut snapshot = make(&META, ctx, WindowKind::Balance);
    snapshot.unit = unit;
    snapshot.remaining = Some(round_money(total));
    if !is_available {
        snapshot.note = Some("DeepSeek reports the balance as too low for API calls".to_owned());
    }
    vec![snapshot]
}

/// Settings of [`DeepSeekAdapter`]. The API key is NOT here; it lives in the secret store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DeepSeekConfig {
    /// Local account label; the key is stored under `deepseek:<account>`.
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

impl Default for DeepSeekConfig {
    fn default() -> Self {
        DeepSeekConfig {
            account: "default".to_owned(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            request_timeout_secs: 15,
            poll_interval_secs: 300,
            use_system_proxy: true,
        }
    }
}

/// Reads a DeepSeek account's prepaid balance.
#[derive(Debug, Clone)]
pub struct DeepSeekAdapter {
    config: DeepSeekConfig,
    balance_url: Url,
    secret: String,
    secrets: Arc<dyn SecretStore>,
    client: ApiClient,
}

impl DeepSeekAdapter {
    /// Create the adapter. Fails on a bad account label or base URL.
    pub fn new(config: DeepSeekConfig, secrets: Arc<dyn SecretStore>) -> Result<Self, ConfigError> {
        validate_account(META.provider, &config.account)?;
        let base = validate_base_url(&config.base_url)?;
        let client = ApiClient::new(&HttpSettings {
            request_timeout: Duration::from_secs(config.request_timeout_secs.max(1)),
            use_system_proxy: config.use_system_proxy,
        })?;
        Ok(DeepSeekAdapter {
            balance_url: join_path(&base, "/user/balance"),
            secret: secret_name(ADAPTER_ID, &config.account),
            config,
            secrets,
            client,
        })
    }

    /// The settings in use.
    pub fn config(&self) -> &DeepSeekConfig {
        &self.config
    }

    /// The name of the secret this adapter reads (`deepseek:<account>`).
    pub fn secret_name(&self) -> &str {
        &self.secret
    }
}

#[async_trait]
impl Adapter for DeepSeekAdapter {
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
            Err(problem) => return failure(&problem.reason("DeepSeek API key")),
        };
        match self.client.get_json(&self.balance_url, &key).await {
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

    fn info(currency: &str, total: &str) -> Value {
        json!({"currency": currency, "total_balance": total, "granted_balance": "0.00", "topped_up_balance": total})
    }

    #[test]
    fn usd_wins_over_cny() {
        let out = map_input(
            &json!({"is_available": true, "balance_infos": [info("CNY", "100.00"), info("USD", "12.345678")]}),
            &ctx().with_http_status(200),
        );
        assert_eq!(out[0].remaining, Some(12.345678));
        assert_eq!(out[0].status, Status::Ok);
        assert_eq!(out[0].note, None);
    }

    #[test]
    fn cny_only_is_reported_in_cny_and_duplicates_are_drift() {
        let out = map_input(
            &json!({"is_available": true, "balance_infos": [info("CNY", "100.00")]}),
            &ctx().with_http_status(200),
        );
        assert_eq!(out[0].status, Status::Ok);
        assert_eq!(out[0].unit, Unit::Cny);
        assert_eq!(out[0].remaining, Some(100.0));
        for body in [
            json!({"is_available": true, "balance_infos": [info("USD", "1"), info("USD", "2")]}),
            json!({"is_available": true, "balance_infos": [info("CNY", "1"), info("CNY", "2")]}),
        ] {
            let out = map_input(&body, &ctx().with_http_status(200));
            assert_eq!(
                out[0].note.as_deref(),
                Some("DeepSeek response did not match the expected shape"),
                "{body}"
            );
        }
    }

    #[test]
    fn too_low_balance_keeps_ok_status_with_a_note() {
        let out = map_input(
            &json!({"is_available": false, "balance_infos": [info("USD", "0.00")]}),
            &ctx(),
        );
        assert_eq!(out[0].status, Status::Ok);
        assert_eq!(out[0].remaining, Some(0.0));
        assert!(out[0].note.is_some());
    }

    #[test]
    fn drift_is_not_a_zero_balance() {
        for body in [
            json!({}),
            json!({"is_available": true}),
            json!({"is_available": true, "balance_infos": []}),
            json!({"is_available": true, "balance_infos": [info("USD", "n/a")]}),
            json!({"is_available": "yes", "balance_infos": [info("USD", "1")]}),
            json!(null),
        ] {
            let out = map_input(&body, &ctx().with_http_status(200));
            assert_eq!(out[0].status, Status::Unavailable, "{body}");
            assert_eq!(out[0].remaining, None, "{body}");
        }
    }
}
