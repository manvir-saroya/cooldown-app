//! The HTTPS client shared by the API-key adapters, and its safety rules.
//!
//! Ported from the MIT-licensed `ai-usagebar` (`src/vendor.rs`): a hard cap on response bodies, a
//! same-origin-only redirect policy, request timeouts, and no secret in any error text.
//!
//! - Bodies are read in chunks and refused past [`MAX_BODY_BYTES`] (2 MiB), so a hijacked or broken
//!   endpoint cannot exhaust memory. `Content-Length` is checked first, but a lying or missing
//!   length cannot get past the chunk loop.
//! - A redirect is followed only when scheme, host and port match the original request. The API key
//!   travels in an `Authorization` header and must never be forwarded to another origin (or
//!   downgraded to plain HTTP).
//! - Base URLs must be `https://`. Plain `http://` is accepted only for loopback addresses so tests
//!   can use a local mock server.
//! - Errors are classified ([`HttpError`]); their text is fixed and never includes the URL, the
//!   key or anything the server said.

use std::time::Duration;

use reqwest::Url;

use crate::secrets::SecretString;
use crate::ConfigError;

/// Largest response body accepted: 2 MiB. Every endpoint used here answers with a few kilobytes.
pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Default time allowed for one request, from connecting to the last body byte.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Time allowed to open a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Redirects followed at most.
const MAX_REDIRECTS: usize = 10;

/// How a request failed, without any detail that could carry a secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HttpError {
    /// The request did not finish in time.
    #[error("timed out")]
    Timeout,
    /// The server could not be reached.
    #[error("could not connect")]
    Connect,
    /// The response body was larger than the cap.
    #[error("response too large")]
    TooLarge,
    /// Too many redirects, or a redirect the policy refused.
    #[error("redirect refused")]
    Redirect,
    /// The API key is not a valid HTTP header value.
    #[error("invalid API key")]
    InvalidKey,
    /// Any other transport failure.
    #[error("network error")]
    Other,
}

impl HttpError {
    /// Short phrase for a snapshot note: `<Provider> request failed (<phrase>)`.
    pub fn phrase(self) -> &'static str {
        match self {
            HttpError::Timeout => "timed out",
            HttpError::Connect => "could not connect",
            HttpError::TooLarge => "response too large",
            HttpError::Redirect => "redirect refused",
            HttpError::InvalidKey => "invalid API key",
            HttpError::Other => "network error",
        }
    }
}

/// Client tuning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpSettings {
    /// Time allowed for one request, connection to last byte.
    pub request_timeout: Duration,
    /// Honor the system proxy settings (`HTTPS_PROXY` and the OS proxy configuration). Tests turn
    /// this off so a developer's proxy cannot interfere with the local mock server.
    pub use_system_proxy: bool,
}

impl Default for HttpSettings {
    fn default() -> Self {
        HttpSettings {
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            use_system_proxy: true,
        }
    }
}

/// A status code and the (capped) body of a finished request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiResponse {
    /// HTTP status.
    pub status: u16,
    /// Body, empty for a non-2xx response (only the status matters then).
    pub body: Vec<u8>,
}

impl ApiResponse {
    /// True for 2xx.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The body as JSON; `None` when it is not valid JSON.
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }
}

/// An HTTPS client with the safety rules built in.
#[derive(Debug, Clone)]
pub struct ApiClient {
    client: reqwest::Client,
}

impl ApiClient {
    /// Build the client.
    pub fn new(settings: &HttpSettings) -> Result<Self, ConfigError> {
        let mut builder = reqwest::Client::builder()
            .redirect(same_origin_redirect_policy())
            .timeout(settings.request_timeout)
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(concat!("cooldown/", env!("CARGO_PKG_VERSION")));
        if !settings.use_system_proxy {
            builder = builder.no_proxy();
        }
        let client = builder
            .build()
            .map_err(|e| ConfigError::HttpClient(crate::scrub::scrub(&e.to_string(), &[])))?;
        Ok(ApiClient { client })
    }

    /// `GET url` with `Authorization: Bearer <key>`.
    ///
    /// A 2xx response comes back with its body (read under the 2 MiB cap). Any other status comes
    /// back with an empty body: the adapters only use the status.
    pub async fn get_json(&self, url: &Url, key: &SecretString) -> Result<ApiResponse, HttpError> {
        if !key.is_valid_api_key() {
            return Err(HttpError::InvalidKey);
        }
        let response = self
            .client
            .get(url.clone())
            .bearer_auth(key.expose())
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(classify)?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Ok(ApiResponse {
                status,
                body: Vec::new(),
            });
        }
        let body = read_body_capped(response, MAX_BODY_BYTES).await?;
        Ok(ApiResponse { status, body })
    }
}

fn classify(error: reqwest::Error) -> HttpError {
    if error.is_timeout() {
        HttpError::Timeout
    } else if error.is_redirect() {
        HttpError::Redirect
    } else if error.is_connect() {
        HttpError::Connect
    } else {
        HttpError::Other
    }
}

/// Read a response body, refusing more than `max` bytes.
pub async fn read_body_capped(
    mut response: reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, HttpError> {
    if response
        .content_length()
        .is_some_and(|len| len > max as u64)
    {
        return Err(HttpError::TooLarge);
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(classify)? {
        if chunk.len() > max.saturating_sub(body.len()) {
            return Err(HttpError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Follow a redirect only to the same scheme, host and port as the original request.
///
/// `reqwest` already strips `Authorization` on a cross-origin redirect, but the rule is kept
/// explicit (and stricter: no https to http downgrade, no hop to another port) so the key cannot
/// leave its origin whatever a library default does. A refused redirect ends the request with the
/// 3xx response itself, which the adapters report as `request failed (HTTP 3xx)`.
pub fn same_origin_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        let Some(origin) = attempt.previous().first() else {
            return attempt.stop();
        };
        if same_origin(origin, attempt.url()) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

/// True when both URLs have the same scheme, host and port.
pub fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// Parse and check a base URL: `https://`, or `http://` to a loopback address.
///
/// Credentials in the URL, a query string and a fragment are refused. The returned URL has no
/// trailing slash in its path, so `join_path` can append to it.
pub fn validate_base_url(text: &str) -> Result<Url, ConfigError> {
    let url = Url::parse(text).map_err(|_| ConfigError::InvalidBaseUrl("not a valid URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ConfigError::InvalidBaseUrl("must not contain credentials"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ConfigError::InvalidBaseUrl(
            "must not contain a query or fragment",
        ));
    }
    let Some(host) = url.host_str() else {
        return Err(ConfigError::InvalidBaseUrl("has no host"));
    };
    match url.scheme() {
        "https" => Ok(url),
        "http" if is_loopback_host(host) => Ok(url),
        "http" => Err(ConfigError::InvalidBaseUrl(
            "plain http is only allowed for loopback addresses",
        )),
        _ => Err(ConfigError::InvalidBaseUrl("scheme must be https")),
    }
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// `base` + `/segment/...`, with exactly one slash between them. `path` must start with `/`.
pub fn join_path(base: &Url, path: &str) -> Url {
    let mut url = base.clone();
    let joined = format!("{}{}", base.path().trim_end_matches('/'), path);
    url.set_path(&joined);
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_origin_compares_scheme_host_and_port() {
        let a = Url::parse("https://api.example.com/a").unwrap();
        assert!(same_origin(
            &a,
            &Url::parse("https://api.example.com:443/b").unwrap()
        ));
        assert!(!same_origin(
            &a,
            &Url::parse("https://other.example.com/a").unwrap()
        ));
        assert!(!same_origin(
            &a,
            &Url::parse("http://api.example.com/a").unwrap()
        ));
        assert!(!same_origin(
            &a,
            &Url::parse("https://api.example.com:8443/a").unwrap()
        ));
    }

    #[test]
    fn base_urls_must_be_https_or_loopback() {
        assert!(validate_base_url("https://openrouter.ai/api/v1").is_ok());
        assert!(validate_base_url("http://127.0.0.1:8080").is_ok());
        assert!(validate_base_url("http://localhost:9").is_ok());
        assert!(validate_base_url("http://[::1]:9").is_ok());
        for bad in [
            "http://openrouter.ai/api/v1",
            "http://192.168.1.5",
            "ftp://example.com",
            "https://user:pw@example.com",
            "https://example.com/?x=1",
            "https://example.com/#frag",
            "not a url",
            "",
        ] {
            assert!(validate_base_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn join_path_has_exactly_one_slash() {
        let base = validate_base_url("https://openrouter.ai/api/v1/").unwrap();
        assert_eq!(
            join_path(&base, "/key").as_str(),
            "https://openrouter.ai/api/v1/key"
        );
        let base = validate_base_url("https://api.deepseek.com").unwrap();
        assert_eq!(
            join_path(&base, "/user/balance").as_str(),
            "https://api.deepseek.com/user/balance"
        );
    }

    #[test]
    fn error_text_is_fixed() {
        for error in [
            HttpError::Timeout,
            HttpError::Connect,
            HttpError::TooLarge,
            HttpError::Redirect,
            HttpError::InvalidKey,
            HttpError::Other,
        ] {
            assert_eq!(error.to_string(), error.phrase());
        }
    }
}
