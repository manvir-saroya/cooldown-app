//! Codex, through `codex app-server --listen stdio://`.
//!
//! The Codex CLI ships an experimental JSON-RPC server. Cooldown starts it, speaks the handshake
//! (`initialize`, `initialized`) and asks `account/rateLimits/read`, then stops it again. The
//! protocol is undocumented and may change, so the source kind is `scraped` and failures degrade
//! gracefully. Ported in idea from the MIT-licensed `burnrate` (`src/providers/codex.rs`); see
//! `THIRD_PARTY_NOTICES.md`.
//!
//! What this adapter does NOT do: read `~/.codex/auth.json`, read any token, or call `wham/usage`.
//! The Codex process uses its own login; Cooldown only talks to it over a pipe.
//!
//! Process hygiene: the child is spawned with `kill_on_drop`, is always killed and reaped (even on
//! timeout), gets no console window on Windows, and has `OPENAI_API_KEY` and friends removed from
//! its environment so it cannot be switched to API-key auth by an inherited variable.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use cooldown_core::adapter::{Adapter, PollContext};
use cooldown_core::model::{Snapshot, SourceKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};

use crate::codex_launcher::{kill_tree, locate_blocking, CliEnv, Launcher};
use crate::codex_map::{self, SlotKeys};
use crate::mapping::{carry_or_unavailable, obj, truncate_units, MapContext, Meta};
use crate::scrub::scrub;
use crate::{validate_account, ConfigError, ADAPTER_VERSION};

/// `source.adapter` and fixture directory name.
pub const ADAPTER_ID: &str = "codex-app-server";

const META: Meta = Meta {
    id: ADAPTER_ID,
    provider: "codex",
    kind: SourceKind::Scraped,
};

/// Most output ever read from the child, so a runaway process cannot fill memory.
const MAX_OUTPUT_BYTES: u64 = 2 * 1024 * 1024;

/// Non-JSON lines tolerated before giving up (Codex may print banners).
const MAX_NOISE_LINES: usize = 20;

/// Longest quoted provider error message.
const MAX_ERROR_MESSAGE_UNITS: usize = 200;

const REQUEST_INITIALIZE: i64 = 1;
const REQUEST_RATE_LIMITS: i64 = 2;

const NOTE_NO_USABLE: &str = "Codex app-server returned no usable rate limits";
const NOTE_NOT_REPORTED: &str = "Codex did not report this window";

const SLOT_KEYS: SlotKeys = SlotKeys {
    used_percent: "usedPercent",
    window_minutes: "windowDurationMins",
    resets_at: "resetsAt",
};

/// Map the JSON-RPC response line to `account/rateLimits/read` (the fixture input) to the
/// `session_5h` and `weekly` snapshots. Pure.
///
/// - The bucket is `rateLimitsByLimitId.codex`, falling back to `rateLimits`.
/// - Windows are assigned by duration, never by slot.
/// - A JSON-RPC `error` becomes `Codex app-server error: <message>` (at most 200 characters, with
///   anything shaped like a credential scrubbed).
pub fn map_input(input: &Value, ctx: &MapContext) -> Vec<Snapshot> {
    let fail = |reason: &str| codex_map::fail_all(&META, ctx, reason);

    if !input.is_object() {
        return fail(NOTE_NO_USABLE);
    }
    if let Some(error) = obj(input.get("error")) {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .map(|m| truncate_units(&scrub(m, &[]), MAX_ERROR_MESSAGE_UNITS))
            .unwrap_or_else(|| "unknown error".to_owned());
        return fail(&format!("Codex app-server error: {message}"));
    }
    let Some(result) = obj(input.get("result")) else {
        return fail(NOTE_NO_USABLE);
    };
    let bucket = obj(result
        .get("rateLimitsByLimitId")
        .and_then(|by_id| by_id.get("codex")))
    .or_else(|| obj(result.get("rateLimits")));
    let Some(bucket) = bucket else {
        return fail(NOTE_NO_USABLE);
    };
    let mapped = codex_map::windows_by_duration(
        &[bucket.get("primary"), bucket.get("secondary")],
        &SLOT_KEYS,
    );
    codex_map::snapshots(&META, ctx, mapped, ctx.now, NOTE_NOT_REPORTED)
}

/// Why the exchange with the app-server did not produce a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExchangeError {
    /// Reading from or writing to the child failed.
    Io,
    /// The child closed its output before answering.
    ExitedEarly,
    /// Too many lines that were not JSON.
    TooMuchNoise,
    /// A line that looked like JSON was not.
    InvalidJson,
}

impl ExchangeError {
    fn reason(self) -> &'static str {
        match self {
            ExchangeError::Io => "Could not talk to the Codex app-server",
            ExchangeError::ExitedEarly => "Codex app-server exited before returning rate limits",
            ExchangeError::TooMuchNoise | ExchangeError::InvalidJson => NOTE_NO_USABLE,
        }
    }
}

async fn write_line<W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &Value,
) -> Result<(), ExchangeError> {
    let mut text = serde_json::to_string(value).map_err(|_| ExchangeError::Io)?;
    text.push('\n');
    writer
        .write_all(text.as_bytes())
        .await
        .map_err(|_| ExchangeError::Io)?;
    writer.flush().await.map_err(|_| ExchangeError::Io)
}

/// Read lines until the JSON-RPC response with `id`. Notifications, requests from the server and
/// answers to other ids are skipped.
async fn read_response<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    id: i64,
) -> Result<Value, ExchangeError> {
    let mut line = String::new();
    let mut noise = 0usize;
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .await
            .map_err(|_| ExchangeError::Io)?;
        if read == 0 {
            return Err(ExchangeError::ExitedEarly);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !trimmed.starts_with('{') {
            noise += 1;
            if noise > MAX_NOISE_LINES {
                return Err(ExchangeError::TooMuchNoise);
            }
            continue;
        }
        let value: Value = serde_json::from_str(trimmed).map_err(|_| ExchangeError::InvalidJson)?;
        let is_response = value.get("method").is_none();
        if is_response && value.get("id").and_then(Value::as_i64) == Some(id) {
            return Ok(value);
        }
    }
}

/// Run the handshake and the rate-limits call over an already connected pair of streams.
///
/// 1. `initialize` (id 1); wait for its response. If it is an error, that error is the result.
/// 2. `initialized` notification, then `account/rateLimits/read` (id 2).
/// 3. Return the response to id 2 exactly as received (`{"id":2,"result":...}` or `{"error":...}`).
pub async fn exchange<R, W>(reader: &mut R, writer: &mut W) -> Result<Value, ExchangeError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    write_line(
        writer,
        &json!({
            "id": REQUEST_INITIALIZE,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "cooldown",
                    "title": "Cooldown",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": { "experimentalApi": true },
            },
        }),
    )
    .await?;
    let initialized = read_response(reader, REQUEST_INITIALIZE).await?;
    if initialized.get("error").is_some() {
        return Ok(initialized);
    }
    write_line(writer, &json!({ "method": "initialized" })).await?;
    write_line(
        writer,
        &json!({ "id": REQUEST_RATE_LIMITS, "method": "account/rateLimits/read" }),
    )
    .await?;
    read_response(reader, REQUEST_RATE_LIMITS).await
}

/// Settings of [`CodexAppServerAdapter`]. Nothing secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CodexAppServerConfig {
    /// Local account label (becomes part of the snapshot ids).
    pub account: String,
    /// The Codex command: a path or a command name. `None` searches for `codex` (on Windows
    /// `codex.cmd`, `codex.ps1`, `codex.exe`, then `where.exe codex`).
    pub binary: Option<PathBuf>,
    /// Value for `CODEX_HOME` in the child, for a non-default Codex profile. Cooldown never reads
    /// that directory itself.
    pub codex_home: Option<PathBuf>,
    /// Seconds allowed for the whole handshake and call.
    pub timeout_secs: u64,
    /// Seconds between polls.
    pub poll_interval_secs: u64,
}

impl Default for CodexAppServerConfig {
    fn default() -> Self {
        CodexAppServerConfig {
            account: "default".to_owned(),
            binary: None,
            codex_home: None,
            timeout_secs: 20,
            poll_interval_secs: 120,
        }
    }
}

/// Reads Codex's rate limits from a short-lived `codex app-server` process.
#[derive(Debug, Clone)]
pub struct CodexAppServerAdapter {
    config: CodexAppServerConfig,
}

#[derive(Debug)]
enum FetchFailure {
    NotFound,
    Spawn,
    Timeout,
    Exchange(ExchangeError),
}

impl FetchFailure {
    fn reason(&self) -> &'static str {
        match self {
            FetchFailure::NotFound => {
                "Codex CLI not found. Make sure the codex command works in a terminal, then use the refresh button"
            }
            FetchFailure::Spawn => "Could not start the Codex CLI",
            FetchFailure::Timeout => "Codex app-server did not answer in time",
            FetchFailure::Exchange(e) => e.reason(),
        }
    }
}

impl CodexAppServerAdapter {
    /// Create the adapter. Fails only if the account label is not a legal snapshot account.
    pub fn new(config: CodexAppServerConfig) -> Result<Self, ConfigError> {
        validate_account(META.provider, &config.account)?;
        Ok(CodexAppServerAdapter { config })
    }

    /// The settings in use.
    pub fn config(&self) -> &CodexAppServerConfig {
        &self.config
    }

    async fn fetch(&self) -> Result<Value, FetchFailure> {
        let configured = self.config.binary.clone();
        let located = tokio::task::spawn_blocking(move || locate_blocking(configured.as_deref()))
            .await
            .ok()
            .flatten();
        let Some((launcher, env)) = located else {
            return Err(FetchFailure::NotFound);
        };
        run_app_server(
            &launcher,
            &env,
            self.config.codex_home.as_deref(),
            Duration::from_secs(self.config.timeout_secs.max(1)),
        )
        .await
    }
}

/// Start the app-server, run the exchange under `timeout`, and always kill and reap the child.
async fn run_app_server(
    launcher: &Launcher,
    env: &CliEnv,
    codex_home: Option<&std::path::Path>,
    timeout: Duration,
) -> Result<Value, FetchFailure> {
    let mut command = launcher.command(["app-server", "--listen", "stdio://"]);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(path) = env.joined_path() {
        command.env("PATH", path);
    }
    if let Some(home) = codex_home.filter(|p| !p.as_os_str().is_empty()) {
        command.env("CODEX_HOME", home);
    }
    let mut child = command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            FetchFailure::NotFound
        } else {
            FetchFailure::Spawn
        }
    })?;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        kill_tree(&mut child).await;
        return Err(FetchFailure::Spawn);
    };

    let mut reader = BufReader::new(stdout.take(MAX_OUTPUT_BYTES));
    let result = tokio::time::timeout(timeout, exchange(&mut reader, &mut stdin)).await;

    // Whatever happened: close the pipe, then kill and reap, so no process is left behind.
    drop(stdin);
    drop(reader);
    kill_tree(&mut child).await;

    match result {
        Err(_) => Err(FetchFailure::Timeout),
        Ok(Err(e)) => Err(FetchFailure::Exchange(e)),
        Ok(Ok(value)) => Ok(value),
    }
}

#[async_trait]
impl Adapter for CodexAppServerAdapter {
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
        match self.fetch().await {
            Ok(value) => map_input(&value, &map_ctx),
            Err(failure) => codex_map::WINDOWS
                .iter()
                .map(|&window| {
                    carry_or_unavailable(&META, &map_ctx, window, failure.reason(), None)
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cooldown_core::model::Status;
    use time::macros::datetime;
    use tokio::io::duplex;

    fn ctx() -> MapContext {
        MapContext::new(datetime!(2026-10-10 12:00:00 UTC), "default", "0.1.0")
    }

    fn response(primary: Value, secondary: Value) -> Value {
        json!({"id": 2, "result": {"rateLimits": {"limitId": "codex", "primary": primary, "secondary": secondary}}})
    }

    #[test]
    fn weekly_window_in_the_primary_slot_is_still_weekly() {
        let input = response(
            json!({"usedPercent": 38, "windowDurationMins": 10080, "resetsAt": 1_792_051_200}),
            Value::Null,
        );
        let out = map_input(&input, &ctx());
        assert_eq!(out[0].status, Status::Unavailable);
        assert_eq!(
            out[0].note.as_deref(),
            Some("Codex did not report this window")
        );
        assert_eq!(out[1].percent_used, Some(38.0));
    }

    #[test]
    fn error_messages_are_cut_and_scrubbed() {
        let long = "x ".repeat(300);
        let input = json!({"id": 2, "error": {"code": -1, "message": long}});
        let note = map_input(&input, &ctx())[0].note.clone().unwrap();
        assert_eq!(
            note.chars().count(),
            "Codex app-server error: ".chars().count() + 200
        );
        let input =
            json!({"id": 2, "error": {"message": "bad token sk-live-abcdef123456 rejected"}});
        let note = map_input(&input, &ctx())[0].note.clone().unwrap();
        assert_eq!(
            note,
            "Codex app-server error: bad token [redacted] rejected"
        );
        let input = json!({"id": 2, "error": {}});
        let note = map_input(&input, &ctx())[0].note.clone().unwrap();
        assert_eq!(note, "Codex app-server error: unknown error");
    }

    #[test]
    fn not_an_object_is_no_usable_limits() {
        for input in [Value::Null, json!([1]), json!("x"), json!({"id": 2})] {
            let out = map_input(&input, &ctx());
            assert_eq!(out.len(), 2);
            assert_eq!(out[0].note.as_deref(), Some(NOTE_NO_USABLE));
        }
    }

    /// A scripted server: answers like Codex and records what the client sent.
    async fn run_server(
        server_side: tokio::io::DuplexStream,
        noise: bool,
        reply_to_initialize: Value,
        final_reply: Value,
    ) -> Vec<Value> {
        let (read, mut write) = tokio::io::split(server_side);
        let mut lines = BufReader::new(read).lines();
        let mut seen = Vec::new();
        while let Ok(Some(line)) = lines.next_line().await {
            let value: Value = serde_json::from_str(&line).unwrap();
            let method = value
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            seen.push(value);
            match method.as_str() {
                "initialize" => {
                    if noise {
                        write
                            .write_all(b"codex banner that is not json\n{\"method\":\"account/rateLimits/updated\",\"params\":{}}\n{\"id\":2,\"method\":\"item/tool/call\"}\n{\"id\":99,\"result\":{}}\n")
                            .await
                            .unwrap();
                    }
                    let mut text = serde_json::to_string(&reply_to_initialize).unwrap();
                    text.push('\n');
                    write.write_all(text.as_bytes()).await.unwrap();
                }
                "account/rateLimits/read" => {
                    let mut text = serde_json::to_string(&final_reply).unwrap();
                    text.push('\n');
                    write.write_all(text.as_bytes()).await.unwrap();
                    break;
                }
                _ => {}
            }
        }
        seen
    }

    #[tokio::test]
    async fn the_handshake_runs_in_order_and_skips_noise() {
        let (client, server) = duplex(64 * 1024);
        let reply = response(
            json!({"usedPercent": 51, "windowDurationMins": 300, "resetsAt": 1_791_641_400}),
            Value::Null,
        );
        let server_task = tokio::spawn(run_server(
            server,
            true,
            json!({"id": 1, "result": {"userAgent": "fake"}}),
            reply.clone(),
        ));
        let (read, mut write) = tokio::io::split(client);
        let mut reader = BufReader::new(read);
        let got = exchange(&mut reader, &mut write).await.unwrap();
        assert_eq!(got, reply);

        let seen = server_task.await.unwrap();
        let methods: Vec<_> = seen
            .iter()
            .map(|v| v.get("method").and_then(Value::as_str).unwrap())
            .collect();
        assert_eq!(
            methods,
            ["initialize", "initialized", "account/rateLimits/read"]
        );
        assert_eq!(seen[0]["id"], 1);
        assert!(seen[1].get("id").is_none(), "initialized is a notification");
        assert_eq!(seen[2]["id"], 2);
        assert_eq!(seen[0]["params"]["clientInfo"]["name"], "cooldown");
    }

    #[tokio::test]
    async fn an_initialize_error_is_returned_as_the_result() {
        let (client, server) = duplex(64 * 1024);
        let error = json!({"id": 1, "error": {"code": -32600, "message": "Not initialized"}});
        let _server = tokio::spawn(run_server(server, false, error.clone(), json!({})));
        let (read, mut write) = tokio::io::split(client);
        let mut reader = BufReader::new(read);
        assert_eq!(exchange(&mut reader, &mut write).await.unwrap(), error);
    }

    #[tokio::test]
    async fn a_closed_pipe_is_reported() {
        let (client, server) = duplex(1024);
        drop(server);
        let (read, mut write) = tokio::io::split(client);
        let mut reader = BufReader::new(read);
        assert!(exchange(&mut reader, &mut write).await.is_err());

        let mut empty = BufReader::new(&b""[..]);
        assert_eq!(
            read_response(&mut empty, 2).await,
            Err(ExchangeError::ExitedEarly)
        );
    }

    #[tokio::test]
    async fn too_much_noise_gives_up() {
        let noise = "not json\n".repeat(MAX_NOISE_LINES + 5);
        let mut reader = BufReader::new(noise.as_bytes());
        assert_eq!(
            read_response(&mut reader, 2).await,
            Err(ExchangeError::TooMuchNoise)
        );
        let mut reader = BufReader::new(&b"{broken json\n"[..]);
        assert_eq!(
            read_response(&mut reader, 2).await,
            Err(ExchangeError::InvalidJson)
        );
    }

    #[test]
    fn config_defaults_and_validation() {
        let config = CodexAppServerConfig::default();
        assert_eq!(config.timeout_secs, 20);
        let adapter = CodexAppServerAdapter::new(config).unwrap();
        assert_eq!(adapter.id(), "codex-app-server");
        assert!(CodexAppServerAdapter::new(CodexAppServerConfig {
            account: "a:b".into(),
            ..CodexAppServerConfig::default()
        })
        .is_err());
        // Partial JSON config fills in defaults.
        let parsed: CodexAppServerConfig = serde_json::from_str(r#"{"account":"work"}"#).unwrap();
        assert_eq!(parsed.account, "work");
        assert_eq!(parsed.poll_interval_secs, 120);
    }
}
