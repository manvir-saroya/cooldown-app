//! GitHub Copilot: AI credits spent this month, from GitHub's documented billing API.
//!
//! Since 1 June 2026 Copilot plans bill in GitHub AI Credits (1 credit = USD 0.01). The user-level
//! report is `GET /users/{username}/settings/billing/ai_credit/usage?year=&month=` (REST "Billing
//! usage"), readable with a fine-grained token that has the **Plan** user permission (read). The
//! username comes from `GET /user` with the same token.
//!
//! The report lists `usageItems`; each has `grossAmount` (USD consumed), `discountAmount` (covered
//! by the plan's included credits) and `netAmount` (billed beyond them). The snapshot reports
//! `used = sum(grossAmount)` for the calendar month in a `monthly` window that resets when the
//! month ends (UTC), because that is the period the report covers.
//!
//! The report does not say how many credits the plan includes, and published figures disagree, so
//! nothing is guessed: [`apply_allowance`] adds `limit`, `remaining` and `percentUsed` only when
//! the user entered their monthly allowance.
//!
//! The token is read from the [`SecretStore`] under `copilot:<account>`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cooldown_core::adapter::{Adapter, PollContext};
use cooldown_core::model::{Snapshot, SourceKind, Unit, WindowKind};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::{Month, OffsetDateTime, Time};

use crate::http::{join_path, validate_base_url, ApiClient, HttpSettings};
use crate::keys::load_key;
use crate::mapping::{
    carry_or_unavailable, http_failure, make, obj, round_money, MapContext, Meta,
};
use crate::secrets::{secret_name, SecretStore};
use crate::{validate_account, ConfigError, ADAPTER_VERSION};

/// `source.adapter`, fixture directory name and the first half of the secret name.
pub const ADAPTER_ID: &str = "copilot";

/// Production base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.github.com";

const NAME: &str = "GitHub";

const META: Meta = Meta {
    id: ADAPTER_ID,
    provider: "copilot",
    kind: SourceKind::Official,
};

/// The first instant of the month after the one `now` is in (UTC).
pub fn next_month_start(now: OffsetDateTime) -> OffsetDateTime {
    let now = now.to_offset(time::UtcOffset::UTC);
    let (year, month) = match now.month() {
        Month::December => (now.year() + 1, Month::January),
        m => (now.year(), m.next()),
    };
    time::Date::from_calendar_date(year, month, 1)
        .map(|d| d.with_time(Time::MIDNIGHT).assume_utc())
        .unwrap_or(now)
}

fn failure(ctx: &MapContext, reason: String) -> Vec<Snapshot> {
    let mut s = carry_or_unavailable(&META, ctx, WindowKind::Monthly, &reason, None);
    s.unit = Unit::Usd;
    vec![s]
}

/// Map the body of `GET /users/{username}/settings/billing/ai_credit/usage` for the month `ctx.now`
/// is in (with `ctx.http_status` set) to the `monthly` snapshot. Pure.
pub fn map_input(input: &Value, ctx: &MapContext) -> Vec<Snapshot> {
    if let Some(reason) = http_failure(NAME, ctx) {
        let reason = match ctx.http_status {
            Some(403) | Some(404) => format!(
                "{reason}. The token needs the Plan permission (read) on your personal account"
            ),
            _ => reason,
        };
        return failure(ctx, reason);
    }
    let drift = || {
        failure(
            ctx,
            format!("{NAME} response did not match the expected shape"),
        )
    };
    let Some(items) = obj(Some(input))
        .and_then(|root| root.get("usageItems"))
        .and_then(Value::as_array)
    else {
        return drift();
    };
    let mut used = 0.0;
    for item in items {
        let Some(amount) = item
            .get("grossAmount")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v >= 0.0)
        else {
            return drift();
        };
        used += amount;
    }
    let mut snapshot = make(&META, ctx, WindowKind::Monthly);
    snapshot.unit = Unit::Usd;
    snapshot.used = Some(round_money(used));
    snapshot.resets_at = Some(next_month_start(ctx.now));
    vec![snapshot]
}

/// Add the user's monthly allowance (USD) to a mapped snapshot: `limit`, `remaining` and
/// `percentUsed`. Without an allowance the snapshot is left as reported. Pure.
pub fn apply_allowance(mut snapshots: Vec<Snapshot>, allowance_usd: Option<f64>) -> Vec<Snapshot> {
    let Some(allowance) = allowance_usd.filter(|a| a.is_finite() && *a > 0.0) else {
        return snapshots;
    };
    for s in &mut snapshots {
        if let Some(used) = s.used {
            s.limit = Some(round_money(allowance));
            s.remaining = Some(round_money(allowance - used));
            s.percent_used = Some(((used / allowance) * 100.0).clamp(0.0, 100.0));
        }
    }
    snapshots
}

/// Settings of [`CopilotAdapter`]. The token is NOT here; it lives in the secret store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CopilotConfig {
    /// Local account label; the token is stored under `copilot:<account>`.
    pub account: String,
    /// The plan's monthly included AI credits in USD, as the user entered it. `None`: show spend only.
    pub monthly_allowance_usd: Option<f64>,
    /// API base URL. `https://` only (plain `http://` for loopback, so tests can use a mock).
    pub base_url: String,
    /// Seconds allowed per request.
    pub request_timeout_secs: u64,
    /// Seconds between polls. GitHub's billing figures update with a delay, so this is slow.
    pub poll_interval_secs: u64,
    /// Honor the system proxy settings.
    pub use_system_proxy: bool,
}

impl Default for CopilotConfig {
    fn default() -> Self {
        CopilotConfig {
            account: "default".to_owned(),
            monthly_allowance_usd: None,
            base_url: DEFAULT_BASE_URL.to_owned(),
            request_timeout_secs: 20,
            poll_interval_secs: 900,
            use_system_proxy: true,
        }
    }
}

/// Reads the AI credits a GitHub account spent this month.
#[derive(Debug, Clone)]
pub struct CopilotAdapter {
    config: CopilotConfig,
    base: Url,
    secret: String,
    secrets: Arc<dyn SecretStore>,
    client: ApiClient,
}

/// A GitHub login is letters, digits and single hyphens, at most 39 characters.
fn is_login(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 39
        && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

impl CopilotAdapter {
    /// Create the adapter. Fails on a bad account label or base URL.
    pub fn new(config: CopilotConfig, secrets: Arc<dyn SecretStore>) -> Result<Self, ConfigError> {
        validate_account(META.provider, &config.account)?;
        let base = validate_base_url(&config.base_url)?;
        let client = ApiClient::new(&HttpSettings {
            request_timeout: Duration::from_secs(config.request_timeout_secs.max(1)),
            use_system_proxy: config.use_system_proxy,
        })?;
        Ok(CopilotAdapter {
            base,
            secret: secret_name(ADAPTER_ID, &config.account),
            config,
            secrets,
            client,
        })
    }

    /// The settings in use.
    pub fn config(&self) -> &CopilotConfig {
        &self.config
    }
}

#[async_trait]
impl Adapter for CopilotAdapter {
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
        let fail = |reason: String| {
            apply_allowance(failure(&map_ctx, reason), self.config.monthly_allowance_usd)
        };
        let token = match load_key(&self.secrets, &self.secret).await {
            Ok(token) => token,
            Err(problem) => return fail(problem.reason("GitHub token")),
        };
        // Who the token belongs to.
        let user_url = join_path(&self.base, "/user");
        let login = match self.client.get_json(&user_url, &token).await {
            Ok(r) if r.is_success() => r
                .json()
                .and_then(|v| v.get("login").and_then(Value::as_str).map(str::to_owned))
                .filter(|l| is_login(l)),
            Ok(r) => return fail(format!("{NAME} request failed (HTTP {})", r.status)),
            Err(e) => return fail(format!("{NAME} request failed ({})", e.phrase())),
        };
        let Some(login) = login else {
            return fail(format!(
                "{NAME} did not say which account the token belongs to"
            ));
        };
        let now = map_ctx.now.to_offset(time::UtcOffset::UTC);
        let mut url = join_path(
            &self.base,
            &format!("/users/{login}/settings/billing/ai_credit/usage"),
        );
        url.query_pairs_mut()
            .append_pair("year", &now.year().to_string())
            .append_pair("month", &u8::from(now.month()).to_string());
        let mapped = match self.client.get_json(&url, &token).await {
            Ok(response) => {
                let body = if response.is_success() {
                    response.json().unwrap_or(Value::Null)
                } else {
                    Value::Null
                };
                map_input(&body, &map_ctx.clone().with_http_status(response.status))
            }
            Err(error) => failure(
                &map_ctx,
                format!("{NAME} request failed ({})", error.phrase()),
            ),
        };
        apply_allowance(mapped, self.config.monthly_allowance_usd)
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
            .with_http_status(200)
    }

    fn item(gross: f64) -> Value {
        json!({"product": "copilot", "sku": "copilot_ai_credit", "model": "x", "unitType": "credits",
               "pricePerUnit": 0.01, "grossQuantity": gross * 100.0, "grossAmount": gross,
               "discountQuantity": 0, "discountAmount": 0, "netQuantity": 0, "netAmount": 0})
    }

    #[test]
    fn spend_is_summed_for_the_month_and_resets_when_it_ends() {
        let out = map_input(&json!({"usageItems": [item(3.25), item(1.5)]}), &ctx());
        assert_eq!(out[0].used, Some(4.75));
        assert_eq!(out[0].unit, Unit::Usd);
        assert_eq!(out[0].window, WindowKind::Monthly);
        assert_eq!(out[0].resets_at, Some(datetime!(2026-11-01 0:00 UTC)));
        assert_eq!(out[0].percent_used, None, "no allowance, no guess");
        assert_eq!(
            next_month_start(datetime!(2026-12-31 23:59 UTC)),
            datetime!(2027-01-01 0:00 UTC)
        );
    }

    #[test]
    fn an_allowance_turns_spend_into_a_percentage() {
        let out = apply_allowance(
            map_input(&json!({"usageItems": [item(7.5)]}), &ctx()),
            Some(10.0),
        );
        assert_eq!(out[0].limit, Some(10.0));
        assert_eq!(out[0].remaining, Some(2.5));
        assert_eq!(out[0].percent_used, Some(75.0));
        let over = apply_allowance(
            map_input(&json!({"usageItems": [item(12.0)]}), &ctx()),
            Some(10.0),
        );
        assert_eq!(over[0].percent_used, Some(100.0));
        assert_eq!(over[0].remaining, Some(-2.0));
        let none = apply_allowance(map_input(&json!({"usageItems": []}), &ctx()), Some(0.0));
        assert_eq!(none[0].percent_used, None);
        assert_eq!(none[0].used, Some(0.0));
    }

    #[test]
    fn a_missing_permission_says_which_one() {
        let out = map_input(&Value::Null, &ctx().with_http_status(403));
        assert_eq!(out[0].status, Status::Unavailable);
        assert!(out[0].note.as_deref().unwrap().contains("Plan permission"));
    }

    #[test]
    fn drift_is_not_zero_spend() {
        for bad in [
            json!({}),
            json!({"usageItems": "none"}),
            json!({"usageItems": [{"grossAmount": "3"}]}),
            json!({"usageItems": [{"grossAmount": -1.0}]}),
            json!(null),
        ] {
            let out = map_input(&bad, &ctx());
            assert_eq!(out[0].status, Status::Unavailable, "{bad}");
            assert_eq!(out[0].used, None, "{bad}");
        }
    }

    #[test]
    fn logins_are_checked_before_they_go_into_a_path() {
        assert!(is_login("example-user"));
        assert!(!is_login("../admin"));
        assert!(!is_login(""));
        assert!(!is_login(&"a".repeat(40)));
    }
}
