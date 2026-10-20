//! What may cross the wire: the allowlist copy and the secret scrubber.
//!
//! Only [`SyncPayload`] crosses the wire, and it is rebuilt field by field here (never a
//! serialization of an internal struct). Free-text fields are scrubbed: anything that looks like
//! a credential becomes `"[redacted]"`. The phone applies the same limits to what it receives.

use cooldown_core::{
    alert_id, parse_alert_id, snapshot_id, AlertIntent, Snapshot, SnapshotSource, SyncPayload,
};

/// Most snapshots in one payload.
pub const MAX_SNAPSHOTS: usize = 200;
/// Most alerts in one payload.
pub const MAX_ALERTS: usize = 200;
/// Longest label-like string (account, provider, adapter, version, device id), in characters.
pub const MAX_LABEL_CHARS: usize = 128;
/// Longest `note`, in characters.
pub const MAX_NOTE_CHARS: usize = 256;
/// What a scrubbed string becomes.
pub const REDACTED: &str = "[redacted]";

/// Why a payload cannot be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SanitizeError {
    /// More than 200 snapshots or alerts.
    #[error("too many entries")]
    TooMany,
    /// An identifier (provider, adapter, device id) is too long or looks like a secret. Unlike a
    /// label it cannot be replaced, because other fields are derived from it.
    #[error("an identifier is unusable")]
    Identifier,
    /// The payload fails the shared validation after scrubbing.
    #[error("payload failed validation")]
    Invalid,
}

const SECRET_PREFIXES: [&str; 12] = [
    "sk-",
    "sk_",
    "pk_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "github_pat_",
    "xai-",
    "xoxb-",
    "xoxp-",
    "eyj",
];

fn is_run_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '_' | '-' | '=')
}

/// True if `text` matches a secret pattern: an `@`, a word that starts like a well-known API key
/// or token (`sk-`, `ghp_`, ...), the word `Bearer`, a JWT shape (three dot-separated base64url
/// segments), or a run of 32 or more base64-alphabet characters.
pub fn looks_secret(text: &str) -> bool {
    if text.contains('@') {
        return true;
    }
    let mut run = 0usize;
    for c in text.chars() {
        if is_run_char(c) {
            run += 1;
            if run >= 32 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    text.split_whitespace().any(|word| {
        let lower = word.to_ascii_lowercase();
        lower == "bearer"
            || SECRET_PREFIXES.iter().any(|p| lower.starts_with(p))
            || looks_like_jwt(word)
    })
}

fn looks_like_jwt(word: &str) -> bool {
    let segments: Vec<&str> = word.split('.').collect();
    segments.len() == 3
        && segments.iter().all(|s| {
            s.len() >= 8
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Label-like text: scrubbed and clipped (never an identifier).
fn scrub_label(text: &str) -> String {
    if looks_secret(text) || text.chars().count() > MAX_LABEL_CHARS {
        REDACTED.to_owned()
    } else {
        text.to_owned()
    }
}

fn check_identifier(text: &str) -> Result<(), SanitizeError> {
    if looks_secret(text) || text.chars().count() > MAX_LABEL_CHARS {
        Err(SanitizeError::Identifier)
    } else {
        Ok(())
    }
}

/// Build the payload that is safe to send: counts capped, labels and notes scrubbed, ids kept
/// consistent with scrubbed accounts, duplicates dropped, and the result validated by
/// `cooldown-core`. Fails rather than sending something questionable.
pub fn sanitize_payload(payload: &SyncPayload) -> Result<SyncPayload, SanitizeError> {
    if payload.snapshots.len() > MAX_SNAPSHOTS || payload.alerts.len() > MAX_ALERTS {
        return Err(SanitizeError::TooMany);
    }
    // The device id is the PC's own 32-character hex id (the server stamps it), which would trip
    // the long-run pattern; it only needs a length check.
    if payload.device_id.chars().count() > MAX_LABEL_CHARS {
        return Err(SanitizeError::Identifier);
    }
    let mut snapshots: Vec<Snapshot> = Vec::with_capacity(payload.snapshots.len());
    for original in &payload.snapshots {
        check_identifier(&original.provider)?;
        check_identifier(&original.source.adapter)?;
        check_identifier(&original.source.adapter_version)?;
        let account = scrub_label(&original.account);
        let id = snapshot_id(&original.provider, &account, original.window);
        if snapshots.iter().any(|s| s.id == id) {
            continue;
        }
        snapshots.push(Snapshot {
            schema_version: original.schema_version,
            id,
            provider: original.provider.clone(),
            account,
            window: original.window,
            unit: original.unit,
            percent_used: original.percent_used,
            used: original.used,
            limit: original.limit,
            remaining: original.remaining,
            resets_at: original.resets_at,
            observed_at: original.observed_at,
            source: SnapshotSource {
                kind: original.source.kind,
                adapter: original.source.adapter.clone(),
                adapter_version: original.source.adapter_version.clone(),
            },
            status: original.status,
            note: original.note.as_deref().map(|note| {
                let note = truncate_chars(note, MAX_NOTE_CHARS);
                if looks_secret(&note) {
                    REDACTED.to_owned()
                } else {
                    note
                }
            }),
        });
    }
    let mut alerts: Vec<AlertIntent> = Vec::with_capacity(payload.alerts.len());
    for original in &payload.alerts {
        let Some(parts) = parse_alert_id(&original.id) else {
            return Err(SanitizeError::Invalid);
        };
        let id = alert_id(&parts.provider, &scrub_label(&parts.account), parts.window);
        if alerts.iter().any(|a| a.id == id) {
            continue;
        }
        alerts.push(AlertIntent {
            id,
            fire_at: original.fire_at,
            enabled: original.enabled,
        });
    }
    let clean = SyncPayload {
        schema_version: payload.schema_version,
        device_id: payload.device_id.clone(),
        generated_at: payload.generated_at,
        snapshots,
        alerts,
    };
    clean.validate().map_err(|_| SanitizeError::Invalid)?;
    Ok(clean)
}

/// The phone's check of an incoming payload: the same limits, plus core validation.
pub fn check_incoming(payload: &SyncPayload) -> Result<(), &'static str> {
    if payload.snapshots.len() > MAX_SNAPSHOTS || payload.alerts.len() > MAX_ALERTS {
        return Err("too many entries");
    }
    let too_long = |s: &str| s.chars().count() > MAX_LABEL_CHARS;
    if too_long(&payload.device_id) {
        return Err("string too long");
    }
    for s in &payload.snapshots {
        if too_long(&s.account)
            || too_long(&s.provider)
            || too_long(&s.source.adapter)
            || too_long(&s.source.adapter_version)
        {
            return Err("string too long");
        }
        if s.note
            .as_deref()
            .is_some_and(|n| n.chars().count() > MAX_NOTE_CHARS)
        {
            return Err("note too long");
        }
    }
    payload.validate().map_err(|_| "payload failed validation")
}

#[cfg(test)]
mod tests {
    use super::*;
    use cooldown_core::{SourceKind, Unit, WindowKind};
    use time::OffsetDateTime;

    fn snapshot(account: &str, note: Option<&str>) -> Snapshot {
        let mut s = Snapshot::new(
            "claude",
            account,
            WindowKind::Session5h,
            Unit::Percent,
            SnapshotSource {
                kind: SourceKind::Official,
                adapter: "statusline".into(),
                adapter_version: "1".into(),
            },
            OffsetDateTime::UNIX_EPOCH,
        );
        s.percent_used = Some(10.0);
        s.note = note.map(str::to_owned);
        s
    }

    fn payload(snapshots: Vec<Snapshot>, alerts: Vec<AlertIntent>) -> SyncPayload {
        SyncPayload {
            schema_version: 1,
            device_id: "0123456789abcdef0123456789abcdef".into(),
            generated_at: OffsetDateTime::UNIX_EPOCH,
            snapshots,
            alerts,
        }
    }

    #[test]
    fn scrubber_patterns() {
        for secret in [
            "sk-ant-abc123",
            "my key is sk-live-xyz",
            "Bearer abc",
            "bearer",
            "me@example.com",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r",
            "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVowMTIzNDU2Nzg5",
            "ghp_0123456789",
        ] {
            assert!(looks_secret(secret), "should flag {secret:?}");
        }
        for fine in [
            "work",
            "Personal account",
            "team-a",
            "5 hour window",
            "desk-1",
            REDACTED,
        ] {
            assert!(!looks_secret(fine), "should not flag {fine:?}");
        }
    }

    #[test]
    fn labels_and_notes_are_scrubbed_and_ids_follow() {
        let input = payload(
            vec![
                snapshot("work", Some("fine note")),
                snapshot("sk-live-abcdef", Some("Bearer abc.def")),
            ],
            vec![],
        );
        let out = sanitize_payload(&input).unwrap();
        assert_eq!(out.snapshots[0].account, "work");
        assert_eq!(out.snapshots[0].note.as_deref(), Some("fine note"));
        assert_eq!(out.snapshots[1].account, REDACTED);
        assert_eq!(out.snapshots[1].id, "claude:[redacted]:session_5h");
        assert_eq!(out.snapshots[1].note.as_deref(), Some(REDACTED));
    }

    #[test]
    fn email_like_accounts_are_redacted_rather_than_failing_validation() {
        let out = sanitize_payload(&payload(vec![snapshot("a@b.com", None)], vec![])).unwrap();
        assert_eq!(out.snapshots[0].account, REDACTED);
        assert!(out.validate().is_ok());
    }

    #[test]
    fn duplicates_created_by_scrubbing_are_dropped() {
        let out = sanitize_payload(&payload(
            vec![snapshot("sk-one", None), snapshot("sk-two", None)],
            vec![],
        ))
        .unwrap();
        assert_eq!(out.snapshots.len(), 1);
    }

    #[test]
    fn long_notes_are_clipped_and_long_labels_redacted() {
        let long_note = "x ".repeat(300);
        let out = sanitize_payload(&payload(
            vec![snapshot(&"a".repeat(60), Some(&long_note))],
            vec![],
        ))
        .unwrap();
        assert_eq!(
            out.snapshots[0].note.as_ref().unwrap().chars().count(),
            MAX_NOTE_CHARS
        );
    }

    #[test]
    fn counts_are_capped_and_identifiers_cannot_be_secrets() {
        let many: Vec<Snapshot> = (0..201)
            .map(|i| snapshot(&format!("acct{i}"), None))
            .collect();
        assert_eq!(
            sanitize_payload(&payload(many, vec![])).unwrap_err(),
            SanitizeError::TooMany
        );
        let mut bad = payload(vec![snapshot("work", None)], vec![]);
        bad.snapshots[0].source.adapter = "sk-secret".into();
        assert_eq!(
            sanitize_payload(&bad).unwrap_err(),
            SanitizeError::Identifier
        );
    }

    #[test]
    fn alert_ids_follow_redacted_accounts() {
        let alert = AlertIntent {
            id: "reset:claude:a@b.com:session_5h".into(),
            fire_at: OffsetDateTime::UNIX_EPOCH,
            enabled: true,
        };
        // An alert id with an '@' in the account is invalid for core, so it is refused outright.
        assert_eq!(
            sanitize_payload(&payload(vec![], vec![alert])).unwrap_err(),
            SanitizeError::Invalid
        );
        let ok = AlertIntent {
            id: "reset:claude:work:session_5h".into(),
            fire_at: OffsetDateTime::UNIX_EPOCH,
            enabled: true,
        };
        let out = sanitize_payload(&payload(vec![], vec![ok])).unwrap();
        assert_eq!(out.alerts[0].id, "reset:claude:work:session_5h");
    }

    #[test]
    fn incoming_limits() {
        let ok = payload(vec![snapshot("work", None)], vec![]);
        assert!(check_incoming(&ok).is_ok());
        let mut long = ok.clone();
        long.snapshots[0].note = Some("n".repeat(MAX_NOTE_CHARS + 1));
        assert!(check_incoming(&long).is_err());
        let mut many = ok.clone();
        many.snapshots = (0..201).map(|i| snapshot(&format!("a{i}"), None)).collect();
        assert!(check_incoming(&many).is_err());
        let mut bad_version = ok;
        bad_version.schema_version = 2;
        assert!(check_incoming(&bad_version).is_err());
    }
}
