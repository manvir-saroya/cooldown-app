//! OpenRouter, DeepSeek and xAI against a local mock server. No real network access.
//!
//! Besides the happy paths (which must reproduce the shared fixtures) this covers the safety
//! rules: the key travels only as a bearer token, never leaks into a snapshot, redirects never
//! leave the origin, bodies are capped, and every request has a timeout.

mod common;

use std::sync::Arc;
use std::time::Duration;

use cooldown_adapters::deepseek::{DeepSeekAdapter, DeepSeekConfig};
use cooldown_adapters::openrouter::{OpenRouterAdapter, OpenRouterConfig};
use cooldown_adapters::secrets::{secret_name, InMemorySecretStore, SecretStore, SecretString};
use cooldown_adapters::xai::{XaiAdapter, XaiConfig};
use cooldown_core::{Adapter, PollContext, Snapshot, Status};
use httpmock::prelude::*;
use serde_json::json;
use time::macros::datetime;

use common::{all_text, fixture};

const KEY: &str = "sk-or-v1-SUPERSECRETKEY0123456789abcdef";

fn store_with(adapter: &str, account: &str, key: &str) -> Arc<dyn SecretStore> {
    Arc::new(InMemorySecretStore::with(
        &secret_name(adapter, account),
        key,
    ))
}

fn empty_store() -> Arc<dyn SecretStore> {
    Arc::new(InMemorySecretStore::new())
}

fn now_ctx() -> PollContext {
    PollContext::new(datetime!(2026-10-10 12:00:00 UTC))
}

fn openrouter(
    server: &MockServer,
    store: Arc<dyn SecretStore>,
    timeout_secs: u64,
) -> OpenRouterAdapter {
    OpenRouterAdapter::new(
        OpenRouterConfig {
            base_url: server.url("/api/v1"),
            request_timeout_secs: timeout_secs,
            use_system_proxy: false,
            ..OpenRouterConfig::default()
        },
        store,
    )
    .unwrap()
}

fn deepseek(server: &MockServer, store: Arc<dyn SecretStore>) -> DeepSeekAdapter {
    DeepSeekAdapter::new(
        DeepSeekConfig {
            base_url: server.base_url(),
            use_system_proxy: false,
            ..DeepSeekConfig::default()
        },
        store,
    )
    .unwrap()
}

fn xai(server: &MockServer, team_id: Option<&str>, store: Arc<dyn SecretStore>) -> XaiAdapter {
    XaiAdapter::new(
        XaiConfig {
            base_url: server.base_url(),
            team_id: team_id.map(str::to_owned),
            use_system_proxy: false,
            ..XaiConfig::default()
        },
        store,
    )
    .unwrap()
}

fn assert_no_leak(snapshots: &[Snapshot], adapter_debug: &str) {
    let text = all_text(snapshots);
    assert!(!text.contains(KEY), "key leaked into snapshots: {text}");
    assert!(
        !text.contains("SUPERSECRET"),
        "key leaked into snapshots: {text}"
    );
    assert!(
        !adapter_debug.contains("SUPERSECRET"),
        "key leaked into Debug: {adapter_debug}"
    );
    for snapshot in snapshots {
        snapshot.validate().unwrap();
    }
}

// ---------------------------------------------------------------------------------------------
// OpenRouter

#[tokio::test]
async fn openrouter_success_reproduces_the_fixture() {
    for case in ["capped-key", "uncapped-key", "overspent"] {
        let fx = fixture("openrouter", case);
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api/v1/key")
                    .header("authorization", format!("Bearer {KEY}"))
                    .header("accept", "application/json");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(serde_json::to_string(&fx.input).unwrap());
            })
            .await;
        let adapter = openrouter(&server, store_with("openrouter", "default", KEY), 5);
        let got = adapter.poll(&fx.poll_context()).await;
        assert_eq!(got, fx.expected, "case {case}");
        mock.assert_async().await;
        assert_no_leak(&got, &format!("{adapter:?}"));
    }
}

#[tokio::test]
async fn openrouter_http_errors_follow_the_fixtures_and_never_echo_the_body() {
    for (case, status) in [("unauthorized", 401), ("unauthorized-with-previous", 401)] {
        let fx = fixture("openrouter", case);
        let server = MockServer::start_async().await;
        // A hostile or careless server that echoes the key back in its error body.
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api/v1/key");
                then.status(status)
                    .body(format!(r#"{{"error":{{"message":"invalid key {KEY}"}}}}"#));
            })
            .await;
        let adapter = openrouter(&server, store_with("openrouter", "default", KEY), 5);
        let got = adapter.poll(&fx.poll_context()).await;
        assert_eq!(got, fx.expected, "case {case}");
        assert_no_leak(&got, &format!("{adapter:?}"));
    }
}

#[tokio::test]
async fn openrouter_schema_drift_follows_the_fixture() {
    let fx = fixture("openrouter", "schema-drift");
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/api/v1/key");
            then.status(200)
                .body(serde_json::to_string(&fx.input).unwrap());
        })
        .await;
    let got = openrouter(&server, store_with("openrouter", "default", KEY), 5)
        .poll(&fx.poll_context())
        .await;
    assert_eq!(got, fx.expected);
}

#[tokio::test]
async fn a_body_that_is_not_json_is_drift_not_a_crash() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/api/v1/key");
            then.status(200).body("<html>captive portal</html>");
        })
        .await;
    let got = openrouter(&server, store_with("openrouter", "default", KEY), 5)
        .poll(&now_ctx())
        .await;
    assert_eq!(got[0].status, Status::Unavailable);
    assert_eq!(
        got[0].note.as_deref(),
        Some("OpenRouter response did not match the expected shape")
    );
}

#[tokio::test]
async fn without_a_saved_key_no_request_is_made() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.any_request();
            then.status(200).body("{}");
        })
        .await;
    let got = openrouter(&server, empty_store(), 5).poll(&now_ctx()).await;
    assert_eq!(got[0].status, Status::Unavailable);
    assert_eq!(
        got[0].note.as_deref(),
        Some("No OpenRouter API key saved. Add one in Settings")
    );
    assert_eq!(mock.calls_async().await, 0);
}

#[tokio::test]
async fn a_key_that_cannot_be_a_header_value_is_never_sent() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.any_request();
            then.status(200).body("{}");
        })
        .await;
    // Control characters and spaces are the way to inject extra headers.
    let store = store_with("openrouter", "default", "good\r\nX-Evil: 1");
    let got = openrouter(&server, store, 5).poll(&now_ctx()).await;
    assert_eq!(got[0].status, Status::Unavailable);
    assert!(got[0].note.as_deref().unwrap().contains("not valid"));
    assert_eq!(mock.calls_async().await, 0);
}

#[tokio::test]
async fn a_cross_origin_redirect_is_not_followed_and_the_key_stays_home() {
    let origin = MockServer::start_async().await;
    let elsewhere = MockServer::start_async().await;
    let target = elsewhere
        .mock_async(|when, then| {
            when.any_request();
            then.status(200).body("{}");
        })
        .await;
    origin
        .mock_async(|when, then| {
            when.method(GET).path("/api/v1/key");
            then.status(302)
                .header("location", elsewhere.url("/api/v1/key"));
        })
        .await;
    let got = openrouter(&origin, store_with("openrouter", "default", KEY), 5)
        .poll(&now_ctx())
        .await;
    assert_eq!(
        target.calls_async().await,
        0,
        "the redirect target was contacted"
    );
    assert_eq!(got[0].status, Status::Unavailable);
    assert_eq!(
        got[0].note.as_deref(),
        Some("OpenRouter request failed (HTTP 302)")
    );
}

#[tokio::test]
async fn a_same_origin_redirect_is_followed() {
    let fx = fixture("openrouter", "capped-key");
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/api/v1/key");
            then.status(307).header("location", "/api/v1/moved/key");
        })
        .await;
    let moved = server
        .mock_async(|when, then| {
            when.method(GET)
                .path("/api/v1/moved/key")
                .header("authorization", format!("Bearer {KEY}"));
            then.status(200)
                .body(serde_json::to_string(&fx.input).unwrap());
        })
        .await;
    let got = openrouter(&server, store_with("openrouter", "default", KEY), 5)
        .poll(&fx.poll_context())
        .await;
    assert_eq!(got, fx.expected);
    moved.assert_async().await;
}

#[tokio::test]
async fn a_body_over_two_mebibytes_is_refused() {
    let server = MockServer::start_async().await;
    let padding = " ".repeat(2 * 1024 * 1024 + 16);
    server
        .mock_async(|when, then| {
            when.method(GET).path("/api/v1/key");
            then.status(200)
                .body(format!(r#"{{"data":{{"usage":1}}}}{padding}"#));
        })
        .await;
    let got = openrouter(&server, store_with("openrouter", "default", KEY), 10)
        .poll(&now_ctx())
        .await;
    assert_eq!(got[0].status, Status::Unavailable);
    assert_eq!(
        got[0].note.as_deref(),
        Some("OpenRouter request failed (response too large)")
    );
}

#[tokio::test]
async fn a_body_just_under_the_cap_is_accepted() {
    let server = MockServer::start_async().await;
    let body = r#"{"data":{"usage":2.5}}"#;
    let padding = " ".repeat(2 * 1024 * 1024 - body.len() - 1);
    server
        .mock_async(|when, then| {
            when.method(GET).path("/api/v1/key");
            then.status(200).body(format!("{body}{padding}"));
        })
        .await;
    let got = openrouter(&server, store_with("openrouter", "default", KEY), 10)
        .poll(&now_ctx())
        .await;
    assert_eq!(got[0].status, Status::Ok);
    assert_eq!(got[0].used, Some(2.5));
}

#[tokio::test]
async fn a_slow_server_times_out() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/api/v1/key");
            then.status(200)
                .delay(Duration::from_secs(4))
                .body(r#"{"data":{"usage":1}}"#);
        })
        .await;
    let started = std::time::Instant::now();
    let got = openrouter(&server, store_with("openrouter", "default", KEY), 1)
        .poll(&now_ctx())
        .await;
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(
        got[0].note.as_deref(),
        Some("OpenRouter request failed (timed out)")
    );
}

#[tokio::test]
async fn a_refused_connection_is_reported_without_the_address() {
    // Bind and release a port so nothing is listening on it.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let adapter = OpenRouterAdapter::new(
        OpenRouterConfig {
            base_url: format!("http://127.0.0.1:{port}/api/v1"),
            request_timeout_secs: 3,
            use_system_proxy: false,
            ..OpenRouterConfig::default()
        },
        store_with("openrouter", "default", KEY),
    )
    .unwrap();
    let got = adapter.poll(&now_ctx()).await;
    assert_eq!(got[0].status, Status::Unavailable);
    let note = got[0].note.as_deref().unwrap();
    assert!(note.starts_with("OpenRouter request failed ("), "{note}");
    assert!(!note.contains(&port.to_string()), "{note}");
}

#[tokio::test]
async fn a_failed_poll_keeps_the_previous_value() {
    let fx = fixture("openrouter", "capped-key");
    let server = MockServer::start_async().await;
    let good = server
        .mock_async(|when, then| {
            when.method(GET).path("/api/v1/key");
            then.status(200)
                .body(serde_json::to_string(&fx.input).unwrap());
        })
        .await;
    let adapter = openrouter(&server, store_with("openrouter", "default", KEY), 5);
    let first = adapter.poll(&fx.poll_context()).await;
    assert_eq!(first, fx.expected);

    // The service starts failing; the registry hands the last snapshots back as `previous`.
    good.delete_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/api/v1/key");
            then.status(503);
        })
        .await;
    let ctx = PollContext {
        now: fx.context.now + time::Duration::minutes(5),
        previous: first.clone(),
        consecutive_failures: 0,
    };
    let carried = adapter.poll(&ctx).await;
    assert_eq!(carried[0].status, Status::Degraded);
    assert_eq!(carried[0].used, first[0].used);
    assert_eq!(carried[0].remaining, first[0].remaining);
    assert_eq!(carried[0].observed_at, first[0].observed_at);
    assert_eq!(
        carried[0].note.as_deref(),
        Some("OpenRouter request failed (HTTP 503); showing the last known value")
    );
}

// ---------------------------------------------------------------------------------------------
// DeepSeek

#[tokio::test]
async fn deepseek_cases_reproduce_the_fixtures() {
    for (case, status) in [
        ("usd-and-cny", 200),
        ("cny-only", 200),
        ("insufficient-balance", 200),
        ("empty-body", 200),
        ("unauthorized", 401),
    ] {
        let fx = fixture("deepseek", case);
        assert_eq!(fx.context.http_status, Some(status), "fixture {case}");
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/user/balance")
                    .header("authorization", format!("Bearer {KEY}"));
                then.status(status)
                    .body(serde_json::to_string(&fx.input).unwrap());
            })
            .await;
        let adapter = deepseek(&server, store_with("deepseek", "default", KEY));
        let got = adapter.poll(&fx.poll_context()).await;
        assert_eq!(got, fx.expected, "case {case}");
        mock.assert_async().await;
        assert_no_leak(&got, &format!("{adapter:?}"));
    }
}

#[tokio::test]
async fn deepseek_uses_its_own_secret_name() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.any_request();
            then.status(200).body("{}");
        })
        .await;
    // An OpenRouter key under another name must not be picked up.
    let store = store_with("openrouter", "default", KEY);
    let adapter = deepseek(&server, store);
    assert_eq!(adapter.secret_name(), "deepseek:default");
    let got = adapter.poll(&now_ctx()).await;
    assert_eq!(got[0].status, Status::Unavailable);
    assert_eq!(mock.calls_async().await, 0);
}

// ---------------------------------------------------------------------------------------------
// xAI

fn validation_body(team: &str) -> String {
    json!({"scope": "SCOPE_TEAM", "scopeId": team, "teamId": team}).to_string()
}

const TEAM: &str = "00000000-0000-4000-8000-000000000001";

#[tokio::test]
async fn xai_validates_the_key_then_reads_the_prepaid_balance() {
    for case in ["prepaid-balance", "zero-balance", "overdrawn", "malformed"] {
        let fx = fixture("xai", case);
        let server = MockServer::start_async().await;
        let validation = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/auth/management-keys/validation")
                    .header("authorization", format!("Bearer {KEY}"));
                then.status(200).body(validation_body(TEAM));
            })
            .await;
        let balance = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path(format!("/v1/billing/teams/{TEAM}/prepaid/balance"))
                    .header("authorization", format!("Bearer {KEY}"));
                then.status(200)
                    .body(serde_json::to_string(&fx.input).unwrap());
            })
            .await;
        let adapter = xai(&server, None, store_with("xai", "default", KEY));
        let got = adapter.poll(&fx.poll_context()).await;
        assert_eq!(got, fx.expected, "case {case}");
        validation.assert_async().await;
        balance.assert_async().await;
        assert_no_leak(&got, &format!("{adapter:?}"));
    }
}

#[tokio::test]
async fn xai_forbidden_follows_the_fixture() {
    let fx = fixture("xai", "forbidden");
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/auth/management-keys/validation");
            then.status(200).body(validation_body(TEAM));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(GET).path_includes("/prepaid/balance");
            then.status(403)
                .body(serde_json::to_string(&fx.input).unwrap());
        })
        .await;
    let got = xai(&server, None, store_with("xai", "default", KEY))
        .poll(&fx.poll_context())
        .await;
    assert_eq!(got, fx.expected);
}

#[tokio::test]
async fn a_rejected_management_key_stops_after_validation() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/auth/management-keys/validation");
            then.status(401).body(format!("bad key {KEY}"));
        })
        .await;
    let balance = server
        .mock_async(|when, then| {
            when.path_includes("/prepaid/balance");
            then.status(200).body("{}");
        })
        .await;
    let adapter = xai(&server, None, store_with("xai", "default", KEY));
    let got = adapter.poll(&now_ctx()).await;
    assert_eq!(got[0].status, Status::Unavailable);
    assert_eq!(
        got[0].note.as_deref(),
        Some("xAI request failed (HTTP 401)")
    );
    assert_eq!(balance.calls_async().await, 0);
    assert_no_leak(&got, &format!("{adapter:?}"));
}

#[tokio::test]
async fn a_configured_team_id_wins_over_the_validated_one() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/auth/management-keys/validation");
            then.status(200)
                .body(json!({"scope": "SCOPE_ORGANIZATION", "scopeId": "org-1"}).to_string());
        })
        .await;
    let balance = server
        .mock_async(|when, then| {
            when.method(GET)
                .path("/v1/billing/teams/my-team_2/prepaid/balance");
            then.status(200)
                .body(r#"{"changes":[],"total":{"val":"-1000"}}"#);
        })
        .await;
    let got = xai(
        &server,
        Some("my-team_2"),
        store_with("xai", "default", KEY),
    )
    .poll(&now_ctx())
    .await;
    assert_eq!(got[0].remaining, Some(10.0));
    balance.assert_async().await;
}

#[tokio::test]
async fn an_organization_key_without_a_team_asks_for_one() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/auth/management-keys/validation");
            then.status(200)
                .body(json!({"scope": "SCOPE_ORGANIZATION", "scopeId": "org-1"}).to_string());
        })
        .await;
    let balance = server
        .mock_async(|when, then| {
            when.path_includes("/prepaid/balance");
            then.status(200).body("{}");
        })
        .await;
    let got = xai(&server, None, store_with("xai", "default", KEY))
        .poll(&now_ctx())
        .await;
    assert_eq!(got[0].status, Status::Unavailable);
    assert!(got[0]
        .note
        .as_deref()
        .unwrap()
        .contains("organization-scoped"));
    // It names the field that exists in Settings, and the step that makes it take effect.
    assert!(got[0]
        .note
        .as_deref()
        .unwrap()
        .contains("Fill in the Team ID in Settings and save the key again"));
    assert!(!all_text(&got).contains("org-1"));
    assert_eq!(balance.calls_async().await, 0);
}

#[tokio::test]
async fn a_team_id_that_could_escape_the_path_is_never_used() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/auth/management-keys/validation");
            then.status(200)
                .body(json!({"scope": "SCOPE_TEAM", "scopeId": "../../v1/admin"}).to_string());
        })
        .await;
    let anything = server
        .mock_async(|when, then| {
            when.path_includes("/billing");
            then.status(200).body("{}");
        })
        .await;
    let got = xai(&server, None, store_with("xai", "default", KEY))
        .poll(&now_ctx())
        .await;
    assert_eq!(got[0].status, Status::Unavailable);
    assert_eq!(anything.calls_async().await, 0);
}

#[tokio::test]
async fn xai_construction_refuses_a_bad_team_id() {
    let server = MockServer::start_async().await;
    let result = XaiAdapter::new(
        XaiConfig {
            base_url: server.base_url(),
            team_id: Some("a/b".into()),
            ..XaiConfig::default()
        },
        empty_store(),
    );
    assert!(result.is_err());
}

#[test]
fn production_defaults_point_at_the_documented_hosts() {
    assert_eq!(
        OpenRouterConfig::default().base_url,
        "https://openrouter.ai/api/v1"
    );
    assert_eq!(
        DeepSeekConfig::default().base_url,
        "https://api.deepseek.com"
    );
    assert_eq!(XaiConfig::default().base_url, "https://management-api.x.ai");
}

#[test]
fn plain_http_to_a_real_host_is_refused_at_construction() {
    let result = DeepSeekAdapter::new(
        DeepSeekConfig {
            base_url: "http://api.deepseek.com".into(),
            ..DeepSeekConfig::default()
        },
        empty_store(),
    );
    assert!(result.is_err());
}

#[tokio::test]
async fn secrets_can_be_replaced_between_polls() {
    // The adapter reads the store on every poll, so a key saved in Settings takes effect at once.
    let server = MockServer::start_async().await;
    let good = server
        .mock_async(|when, then| {
            when.method(GET)
                .path("/user/balance")
                .header("authorization", "Bearer new-key-123");
            then.status(200).body(
                json!({"is_available": true, "balance_infos": [{"currency": "USD", "total_balance": "3.00", "granted_balance": "0", "topped_up_balance": "3.00"}]}).to_string(),
            );
        })
        .await;
    let store = Arc::new(InMemorySecretStore::new());
    let adapter = deepseek(&server, store.clone() as Arc<dyn SecretStore>);
    let before = adapter.poll(&now_ctx()).await;
    assert_eq!(before[0].status, Status::Unavailable);
    store
        .set(adapter.secret_name(), &SecretString::new("new-key-123"))
        .unwrap();
    let after = adapter.poll(&now_ctx()).await;
    assert_eq!(after[0].remaining, Some(3.0));
    good.assert_async().await;
}
