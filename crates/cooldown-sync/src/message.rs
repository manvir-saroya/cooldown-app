//! Message bodies (UTF-8 JSON) for the handshake payloads and the transport message types.
//!
//! Every struct rejects unknown fields: version 1 is a closed schema, and an addition needs a
//! version bump (which the strict negotiation in the prologue then enforces).

use cooldown_core::SyncPayload;
use serde::{Deserialize, Serialize};

/// The `app` field of HELLO.
pub const APP_NAME: &str = "cooldown";
/// Longest device or PC name, in characters.
pub const MAX_NAME_CHARS: usize = 32;
/// Longest platform string.
pub const MAX_PLATFORM_CHARS: usize = 16;
/// Longest `REVOKE` reason.
pub const MAX_REASON_CHARS: usize = 64;
/// Longest alert id accepted in `ALERT_SET`.
pub const MAX_ALERT_ID_CHARS: usize = 200;

/// Handshake message 1 in PAIR mode (encrypted by `IKpsk1` after the PSK is mixed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairMsg1 {
    /// The phone's display name, at most 32 characters.
    pub name: String,
    /// `android`, `windows`, ...
    pub platform: String,
    /// Monotonic reconnect timestamp counter at pairing time.
    pub ts: u64,
}

/// Handshake message 1 in RECONNECT mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconnectMsg1 {
    /// `max(now_ms, last_sent + 1)`; the PC rejects `ts <= last_ts` for the peer.
    pub ts: u64,
}

/// Handshake message 2.
///
/// The protocol document lists `beacon_key` here as well; this implementation withholds it until
/// the user has confirmed (see the README), so it travels in the PC's first HELLO instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Msg2 {
    /// The PC's display name (PAIR only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pc_name: Option<String>,
    /// The negotiated protocol version, also covered by the prologue.
    pub proto: u8,
    /// The PC's wall clock, for the clock-offset estimate.
    pub server_time_ms: i64,
}

/// `HELLO`, the first transport message in each direction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloBody {
    /// Always [`APP_NAME`].
    pub app: String,
    /// `android` or `windows`.
    pub platform: String,
    /// The negotiated version; must equal the version echoed in message 2.
    pub proto: u8,
    /// Sender's wall clock.
    pub now_ms: i64,
    /// Phone to PC: the `epoch` of the newest SYNC it applied (0 if none).
    pub last_epoch: u64,
    /// Phone to PC: the `rev` of the newest SYNC it applied (0 if none).
    pub last_rev: u64,
    /// PC to phone only: the 32-byte beacon key, base64url without padding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beacon_key: Option<String>,
}

/// `SYNC`: a snapshot push.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncBody {
    /// Random per database (and again after a restore).
    pub epoch: u64,
    /// Strictly increasing within an epoch.
    pub rev: u64,
    /// The sanitized payload.
    pub payload: SyncPayload,
}

/// `ACK`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AckBody {
    /// Epoch of the acknowledged SYNC.
    pub epoch: u64,
    /// Revision of the acknowledged SYNC.
    pub rev: u64,
}

/// `ALERT_SET`: the phone toggles one reset alert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertSetBody {
    /// Phone-chosen operation id; the PC applies each of the last 64 at most once.
    pub op_id: u64,
    /// `reset:<provider>:<account>:<window>`.
    pub id: String,
    /// The new switch position.
    pub enabled: bool,
}

/// `PING` and `PONG`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PingBody {
    /// Echoed by the PONG.
    pub n: u64,
}

/// `REVOKE`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeBody {
    /// A short machine-readable reason (`removed`, `rejected`, ...).
    pub reason: String,
}

/// `PAIR_OK` has an empty body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct EmptyBody {}

/// Why a connection is being closed with `ERROR`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Bad envelope (flags, padding, length, unknown type).
    Protocol,
    /// The JSON body did not parse or failed validation.
    Malformed,
    /// A message that is not allowed in this state or direction.
    Unexpected,
    /// A version mismatch inside the encrypted channel.
    Version,
    /// A size limit was exceeded.
    TooLarge,
    /// A deadline passed.
    Timeout,
    /// Something went wrong on our side.
    Internal,
    /// A code from a newer peer.
    #[serde(other)]
    Unknown,
}

/// `ERROR`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorBody {
    /// What went wrong.
    pub code: ErrorCode,
}

/// Invisible and direction-changing characters (zero-width, bidirectional overrides and isolates,
/// the byte-order mark, soft hyphen, Arabic letter mark) that could make a name on a confirmation
/// prompt read as something else.
fn is_deceptive(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
    )
}

/// A display name: 1 to 32 characters, with no control, invisible or direction-changing
/// characters. It is shown on the PC's confirmation prompt, so it must not be able to spoof one.
pub fn valid_name(name: &str) -> bool {
    let chars = name.chars().count();
    (1..=MAX_NAME_CHARS).contains(&chars)
        && !name.chars().any(|c| c.is_control() || is_deceptive(c))
}

/// A platform tag: 1 to 16 characters of `[a-z0-9_-]`.
pub fn valid_platform(platform: &str) -> bool {
    (1..=MAX_PLATFORM_CHARS).contains(&platform.len())
        && platform
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

impl PairMsg1 {
    /// Field-level validation after JSON parsing.
    pub fn is_valid(&self) -> bool {
        valid_name(&self.name) && valid_platform(&self.platform)
    }
}

impl Msg2 {
    /// Field-level validation after JSON parsing.
    pub fn is_valid(&self) -> bool {
        self.pc_name.as_deref().is_none_or(valid_name)
    }
}

impl HelloBody {
    /// Field-level validation after JSON parsing.
    pub fn is_valid(&self) -> bool {
        self.app == APP_NAME && valid_platform(&self.platform)
    }
}

impl AlertSetBody {
    /// Field-level validation after JSON parsing.
    pub fn is_valid(&self) -> bool {
        self.id.chars().count() <= MAX_ALERT_ID_CHARS
            && cooldown_core::parse_alert_id(&self.id).is_some()
    }
}

impl RevokeBody {
    /// A revoke with a short reason.
    pub fn new(reason: &str) -> Self {
        RevokeBody {
            reason: reason.chars().take(MAX_REASON_CHARS).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(serde_json::from_str::<AckBody>(r#"{"epoch":1,"rev":2}"#).is_ok());
        assert!(serde_json::from_str::<AckBody>(r#"{"epoch":1,"rev":2,"x":3}"#).is_err());
        assert!(serde_json::from_str::<ReconnectMsg1>(r#"{"ts":1,"name":"x"}"#).is_err());
        assert!(serde_json::from_str::<EmptyBody>("{}").is_ok());
        assert!(serde_json::from_str::<EmptyBody>(r#"{"a":1}"#).is_err());
    }

    #[test]
    fn error_codes_tolerate_newer_peers() {
        let body: ErrorBody = serde_json::from_str(r#"{"code":"from_the_future"}"#).unwrap();
        assert_eq!(body.code, ErrorCode::Unknown);
        let text = serde_json::to_string(&ErrorBody {
            code: ErrorCode::TooLarge,
        })
        .unwrap();
        assert_eq!(text, r#"{"code":"too_large"}"#);
    }

    #[test]
    fn name_and_platform_validation() {
        assert!(valid_name("Pixel 8"));
        assert!(valid_name(&"x".repeat(32)));
        assert!(!valid_name(&"x".repeat(33)));
        assert!(!valid_name(""));
        assert!(!valid_name("bad\nname"));
        // Direction overrides and zero-width characters cannot be used to spoof the prompt.
        assert!(!valid_name("Pixel \u{202E}8"));
        assert!(!valid_name("Pi\u{200B}xel"));
        assert!(valid_name("Pixel 8 \u{00e9}\u{1f4f1}"));
        assert!(valid_platform("android"));
        assert!(!valid_platform("Android"));
        assert!(!valid_platform(""));
    }

    #[test]
    fn alert_set_ids_must_be_alert_ids() {
        let mut body = AlertSetBody {
            op_id: 1,
            id: "reset:claude:work:session_5h".into(),
            enabled: true,
        };
        assert!(body.is_valid());
        body.id = "claude:work:session_5h".into();
        assert!(!body.is_valid());
        body.id = format!("reset:claude:{}:weekly", "a".repeat(300));
        assert!(!body.is_valid());
    }
}
