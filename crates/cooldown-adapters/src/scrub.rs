//! Scrubbing secrets out of text before it reaches an error message, a note or a log line.
//!
//! Cooldown never logs, but error text still travels: it ends up in a snapshot note that is shown
//! in the UI and synced to a phone. Everything that is derived from a provider's text or from an
//! operating-system error goes through [`scrub`] first.
//!
//! Two layers:
//!
//! 1. Exact matches of the secrets the caller knows about (the API key in use).
//! 2. Anything shaped like a credential: `sk-`/`xai-`/`ghp_`... prefixes, JWTs (`eyJ`), the word
//!    after `Bearer`, and long unbroken runs of token characters.

/// What a secret is replaced with.
pub const REDACTED: &str = "[redacted]";

/// Secrets shorter than this are not matched exactly: replacing a 2-character "secret" would shred
/// ordinary words.
const MIN_EXACT_LEN: usize = 4;

/// A run of letters, digits, `_` and `-` at least this long is treated as a credential.
const LONG_TOKEN_LEN: usize = 32;

/// Prefixes of well-known credential formats, compared case-insensitively.
const TOKEN_PREFIXES: [&str; 11] = [
    "sk-",
    "sk_",
    "pk_",
    "xai-",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
];

/// Replace every known secret and every credential-shaped word in `text` with [`REDACTED`].
pub fn scrub(text: &str, known_secrets: &[&str]) -> String {
    let mut out = text.to_owned();
    for secret in known_secrets {
        if secret.len() >= MIN_EXACT_LEN {
            out = out.replace(secret, REDACTED);
        }
    }
    redact_token_shapes(&out)
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn looks_like_credential(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    if word.len() >= 8 && TOKEN_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return true;
    }
    // JWT parts: base64url of `{"`, which always begins `eyJ`.
    if word.len() >= 8 && lower.starts_with("eyj") {
        return true;
    }
    word.len() >= LONG_TOKEN_LEN
}

fn redact_token_shapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // True right after the word `Bearer`, through whitespace and `:`/`=`.
    let mut after_bearer = false;
    let mut word_start: Option<usize> = None;

    let flush = |word: &str, out: &mut String, after_bearer: &mut bool| {
        if *after_bearer || looks_like_credential(word) {
            out.push_str(REDACTED);
        } else {
            out.push_str(word);
        }
        *after_bearer = word.eq_ignore_ascii_case("bearer");
    };

    for (index, c) in text.char_indices() {
        if is_token_char(c) {
            if word_start.is_none() {
                word_start = Some(index);
            }
            continue;
        }
        if let Some(start) = word_start.take() {
            flush(&text[start..index], &mut out, &mut after_bearer);
        }
        if after_bearer && !(c.is_whitespace() || c == ':' || c == '=') {
            after_bearer = false;
        }
        out.push(c);
    }
    if let Some(start) = word_start {
        flush(&text[start..], &mut out, &mut after_bearer);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_secrets_are_replaced_everywhere() {
        let text = "failed with key hunter2-secret-value at hunter2-secret-value";
        let out = scrub(text, &["hunter2-secret-value"]);
        assert_eq!(out, "failed with key [redacted] at [redacted]");
    }

    #[test]
    fn tiny_secrets_are_not_matched_exactly() {
        assert_eq!(scrub("an error occurred", &["an"]), "an error occurred");
    }

    #[test]
    fn credential_prefixes_are_redacted() {
        for token in [
            "sk-or-v1-abcdef0123456789",
            "xai-AbCdEf0123456789",
            "ghp_0123456789abcdef",
            "github_pat_11ABCDEFG",
            "SK-UPPER-CASE-TOKEN",
        ] {
            let out = scrub(&format!("bad key {token} rejected"), &[]);
            assert_eq!(out, "bad key [redacted] rejected", "{token}");
        }
    }

    #[test]
    fn bearer_values_are_redacted_whatever_they_look_like() {
        assert_eq!(
            scrub("Authorization: Bearer abc123 was refused", &[]),
            "Authorization: Bearer [redacted] was refused"
        );
        assert_eq!(scrub("bearer=short", &[]), "bearer=[redacted]");
        assert_eq!(
            scrub("the Bearer scheme is required", &[]),
            "the Bearer [redacted] is required"
        );
    }

    #[test]
    fn jwts_and_long_tokens_are_redacted() {
        let jwt =
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.c2lnbmF0dXJlLXNpZ25hdHVyZS1zaWduYXR1cmU";
        assert_eq!(
            scrub(&format!("token {jwt}!"), &[]),
            "token [redacted].[redacted].[redacted]!"
        );
        let long = "a".repeat(40);
        assert_eq!(scrub(&format!("id {long}"), &[]), "id [redacted]");
    }

    #[test]
    fn ordinary_messages_survive_unchanged() {
        for text in [
            "failed to fetch rate limits: not signed in with ChatGPT",
            "connection refused (os error 111)",
            "HTTP 401 Unauthorized",
            "",
            "ünïcödé text — with dashes and 12345",
        ] {
            assert_eq!(scrub(text, &[]), text);
        }
    }
}
