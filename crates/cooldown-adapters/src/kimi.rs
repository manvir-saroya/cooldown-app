//! Kimi (Moonshot AI): `GET https://api.moonshot.ai/v1/users/me/balance` with a bearer key
//! (documented at platform.kimi.ai, "Balance check").
//!
//! The response is `{"code": 0, "data": {"available_balance", "voucher_balance", "cash_balance"},
//! "status": true}`, in USD on the global platform. One balance is reported (`remaining =
//! available_balance`, which is what API calls are checked against). A key from the China platform
//! (`api.moonshot.cn`, CNY) does not work on the global endpoint; that is a 401, not a balance.
//!
//! The key is read from the [`SecretStore`] under `kimi:<account>`.

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
    balance_failure, carry_or_unavailable, http_failure, make, obj, round_money, MapContext, Meta,
};
use crate::secrets::{secret_name, SecretStore};
use crate::{validate_account, ConfigError, ADAPTER_VERSION};

/// `source.adapter`, fixture directory name and the first half of the secret name.
pub const ADAPTER_ID: &str = "kimi";

/// Production base URL (the global platform).
pub const DEFAULT_BASE_URL: &str = "https://api.moonshot.ai";

const NAME: &str = "Kimi";

const META: Meta = Meta {
    id: ADAPTER_ID,
    provider: "kimi",
    kind: SourceKind::Official,
};

/// Map the response body of `GET /v1/users/me/balance` (with `ctx.http_status` set) to the
/// `balance` snapshot. Pure.
pub fn map_input(input: &Value, ctx: &MapContext) -> Vec<Snapshot> {
    let fail = || balance_failure(&META, ctx, NAME);
    if http_failure(NAME, ctx).is_some() {
        return fail();
    }
    let Some(root) = obj(Some(input)) else {
        return fail();
    };
    let ok = root.get("code").and_then(Value::as_i64) == Some(0)
        && root.get("status").and_then(Value::as_bool) == Some(true);
    let available = obj(root.get("data"))
        .and_then(|data| data.get("available_balance"))
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite());
    let (true, Some(available)) = (ok, available) else {
        return fail();
    };
    let mut snapshot = make(&META, ctx, WindowKind::Balance);
    snapshot.remaining = Some(round_money(available));
    if available <= 0.0 {
        snapshot.note = Some("Kimi reports no balance left for API calls".to_owned());
    }
    vec![snapshot]
}

/// Settings of [`KimiAdapter`]. The API key is NOT here; it lives in the secret store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct KimiConfig {
    /// Local account label; the key is stored under `kimi:<account>`.
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

impl Default for KimiConfig {
    fn default() -> Self {
        KimiConfig {
            account: "default".to_owned(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            request_timeout_secs: 15,
            poll_interval_secs: 300,
            use_system_proxy: true,
        }
    }
}

/// Reads a Kimi (Moonshot) account's prepaid balance.
#[derive(Debug, Clone)]
pub struct KimiAdapter {
    config: KimiConfig,
    balance_url: Url,
    secret: String,
    secrets: Arc<dyn SecretStore>,
    client: ApiClient,
}

impl KimiAdapter {
    /// Create the adapter. Fails on a bad account label or base URL.
    pub fn new(config: KimiConfig, secrets: Arc<dyn SecretStore>) -> Result<Self, ConfigError> {
        validate_account(META.provider, &config.account)?;
        let base = validate_base_url(&config.base_url)?;
        let client = ApiClient::new(&HttpSettings {
            request_timeout: Duration::from_secs(config.request_timeout_secs.max(1)),
            use_system_proxy: config.use_system_proxy,
        })?;
        Ok(KimiAdapter {
            balance_url: join_path(&base, "/v1/users/me/balance"),
            secret: secret_name(ADAPTER_ID, &config.account),
            config,
            secrets,
            client,
        })
    }

    /// The settings in use.
    pub fn config(&self) -> &KimiConfig {
        &self.config
    }
}

#[async_trait]
impl Adapter for KimiAdapter {
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
            Err(problem) => return failure(&problem.reason("Kimi API key")),
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
    use cooldown_core::model::{Status, Unit};
    use serde_json::json;
    use time::macros::datetime;

    fn ctx() -> MapContext {
        MapContext::new(datetime!(2026-10-10 12:00:00 UTC), "default", "0.1.0")
            .with_http_status(200)
    }

    fn body(available: f64) -> Value {
        json!({"code": 0, "data": {"available_balance": available, "voucher_balance": 1.0, "cash_balance": available - 1.0}, "scode": "0x0", "status": true})
    }

    #[test]
    fn the_available_balance_is_reported_in_usd() {
        let out = map_input(&body(49.58894), &ctx());
        assert_eq!(out[0].remaining, Some(49.58894));
        assert_eq!(out[0].unit, Unit::Usd);
        assert_eq!(out[0].status, Status::Ok);
        assert_eq!(out[0].note, None);
    }

    #[test]
    fn an_empty_balance_says_so() {
        let out = map_input(&body(-0.5), &ctx());
        assert_eq!(out[0].status, Status::Ok);
        assert_eq!(out[0].remaining, Some(-0.5));
        assert!(out[0].note.is_some());
    }

    #[test]
    fn drift_is_not_a_zero_balance() {
        for bad in [
            json!({}),
            json!({"code": 1, "data": {"available_balance": 3.0}, "status": true}),
            json!({"code": 0, "data": {"available_balance": 3.0}, "status": false}),
            json!({"code": 0, "data": {"available_balance": "3"}, "status": true}),
            json!({"code": 0, "data": null, "status": true}),
            json!(null),
        ] {
            let out = map_input(&bad, &ctx());
            assert_eq!(out[0].status, Status::Unavailable, "{bad}");
            assert_eq!(out[0].remaining, None, "{bad}");
        }
    }
}
