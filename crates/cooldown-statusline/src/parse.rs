//! Tolerant parsing of the JSON Claude Code pipes to a statusline command.
//!
//! The parser never fails and never panics. It extracts only what Cooldown
//! needs and ignores everything else (paths, session ids, account names, cost
//! fields...), so nothing sensitive can reach the inbox by accident.
//!
//! Tolerance rules:
//! * unknown fields are ignored;
//! * `null`, missing, or wrong-typed values become `None`;
//! * numbers may arrive as numeric strings (`"42"`), which are accepted;
//! * malformed, empty or oversized input yields an empty [`Observation`];
//! * trailing bytes after the first JSON value and a UTF-8 BOM are ignored.
//!
//! The approach (default-on-malformed, optional windows) is adapted from the
//! MIT-licensed `ccstatus` project; see `THIRD_PARTY_NOTICES.md`.

use serde_json::Value;

/// Upper bound on how much stdin the hook reads (1 MiB). Real payloads are a
/// few KiB.
pub const MAX_INPUT_BYTES: usize = 1024 * 1024;

/// A 5-hour or 7-day window as reported by Claude Code.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Window {
    /// Percentage of the window consumed (raw, unclamped), if reported.
    pub used_percentage: Option<f64>,
    /// Unix epoch seconds at which the window resets, if reported.
    pub resets_at: Option<i64>,
}

/// The optional gateway-only `rate_limits.spend_limit` object.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SpendLimit {
    pub used_percentage: Option<f64>,
    pub resets_at: Option<i64>,
    pub used_usd: Option<f64>,
    pub limit_usd: Option<f64>,
    /// One of `daily`, `weekly`, `monthly`; anything else is dropped.
    pub period: Option<String>,
}

/// Everything the hook keeps from one statusline invocation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Observation {
    /// True when the input had a `rate_limits` object at all. When false the
    /// run learned nothing about limits (API-key user, or session start before
    /// the first API response).
    pub rate_limits_present: bool,
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    pub spend_limit: Option<SpendLimit>,
    pub claude_code_version: Option<String>,
}

/// Parse the raw bytes Claude Code sent on stdin.
pub fn parse_stdin(bytes: &[u8]) -> Observation {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut values = serde_json::Deserializer::from_slice(bytes).into_iter::<Value>();
    match values.next() {
        Some(Ok(root)) => observation_from_value(&root),
        _ => Observation::default(),
    }
}

fn observation_from_value(root: &Value) -> Observation {
    let rate_limits = root.get("rate_limits").and_then(Value::as_object);
    let window = |key: &str| {
        rate_limits
            .and_then(|m| m.get(key))
            .and_then(window_from_value)
    };
    Observation {
        rate_limits_present: rate_limits.is_some(),
        five_hour: window("five_hour"),
        seven_day: window("seven_day"),
        spend_limit: rate_limits
            .and_then(|m| m.get("spend_limit"))
            .and_then(spend_limit_from_value),
        claude_code_version: root.get("version").and_then(version_from_value),
    }
}

fn window_from_value(value: &Value) -> Option<Window> {
    let map = value.as_object()?;
    let window = Window {
        used_percentage: lenient_f64(map.get("used_percentage")),
        resets_at: lenient_unix_seconds(map.get("resets_at")),
    };
    (window != Window::default()).then_some(window)
}

fn spend_limit_from_value(value: &Value) -> Option<SpendLimit> {
    let map = value.as_object()?;
    let spend = SpendLimit {
        used_percentage: lenient_f64(map.get("used_percentage")),
        resets_at: lenient_unix_seconds(map.get("resets_at")),
        used_usd: lenient_f64(map.get("used_usd")),
        limit_usd: lenient_f64(map.get("limit_usd")),
        period: map
            .get("period")
            .and_then(Value::as_str)
            .filter(|p| matches!(*p, "daily" | "weekly" | "monthly"))
            .map(str::to_owned),
    };
    (spend != SpendLimit::default()).then_some(spend)
}

/// Accept only short, version-looking strings so arbitrary text can never be
/// smuggled into the inbox through this field.
fn version_from_value(value: &Value) -> Option<String> {
    let s = value.as_str()?.trim();
    let ok = !s.is_empty()
        && s.len() <= 40
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'));
    ok.then(|| s.to_owned())
}

/// A finite number, accepting numeric strings.
fn lenient_f64(value: Option<&Value>) -> Option<f64> {
    let n = match value? {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    n.is_finite().then_some(n)
}

/// Unix epoch seconds in `(0, 9999-12-31]`; fractional seconds are floored.
fn lenient_unix_seconds(value: Option<&Value>) -> Option<i64> {
    const MAX_SECONDS: f64 = 253_402_300_799.0;
    let n = lenient_f64(value)?;
    (n > 0.0 && n <= MAX_SECONDS).then(|| n.floor() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"{
        "session_id": "abc-123",
        "transcript_path": "/home/someone/.claude/projects/x.jsonl",
        "cwd": "/home/someone/secret-project",
        "model": {"id": "claude-opus-5-5", "display_name": "Opus"},
        "version": "2.1.90",
        "cost": {"total_cost_usd": 0.01234},
        "rate_limits": {
            "five_hour": {"used_percentage": 23.5, "resets_at": 1738425600},
            "seven_day": {"used_percentage": 41.2, "resets_at": 1738857600},
            "spend_limit": {
                "used_percentage": 62.8, "resets_at": 1740787200,
                "used_usd": 314.12, "limit_usd": 500, "period": "monthly"
            }
        },
        "some_future_field": {"nested": [1, 2, 3]}
    }"#;

    #[test]
    fn parses_the_documented_payload() {
        let o = parse_stdin(FULL.as_bytes());
        assert!(o.rate_limits_present);
        assert_eq!(
            o.five_hour,
            Some(Window {
                used_percentage: Some(23.5),
                resets_at: Some(1_738_425_600)
            })
        );
        assert_eq!(
            o.seven_day,
            Some(Window {
                used_percentage: Some(41.2),
                resets_at: Some(1_738_857_600)
            })
        );
        let spend = o.spend_limit.unwrap();
        assert_eq!(spend.used_usd, Some(314.12));
        assert_eq!(spend.limit_usd, Some(500.0));
        assert_eq!(spend.period.as_deref(), Some("monthly"));
        assert_eq!(o.claude_code_version.as_deref(), Some("2.1.90"));
    }

    #[test]
    fn all_null_fields_become_none() {
        let o = parse_stdin(
            br#"{"version":null,"rate_limits":{"five_hour":null,"seven_day":{"used_percentage":null,"resets_at":null},"spend_limit":null}}"#,
        );
        assert!(o.rate_limits_present);
        assert_eq!(o.five_hour, None);
        assert_eq!(o.seven_day, None);
        assert_eq!(o.spend_limit, None);
        assert_eq!(o.claude_code_version, None);
    }

    #[test]
    fn rate_limits_null_or_missing_is_not_present() {
        for input in [
            r#"{"model":{"display_name":"X"}}"#,
            r#"{"rate_limits":null}"#,
            r#"{"rate_limits":"nope"}"#,
            r#"{"rate_limits":[1,2]}"#,
        ] {
            let o = parse_stdin(input.as_bytes());
            assert!(!o.rate_limits_present, "{input}");
            assert_eq!(o.five_hour, None);
            assert_eq!(o.seven_day, None);
        }
    }

    #[test]
    fn empty_rate_limits_object_is_present_with_no_windows() {
        let o = parse_stdin(br#"{"rate_limits":{}}"#);
        assert!(o.rate_limits_present);
        assert_eq!((o.five_hour, o.seven_day), (None, None));
    }

    #[test]
    fn each_window_may_be_missing_independently() {
        let only_five = parse_stdin(
            br#"{"rate_limits":{"five_hour":{"used_percentage":10,"resets_at":1738425600}}}"#,
        );
        assert!(only_five.five_hour.is_some());
        assert_eq!(only_five.seven_day, None);

        let only_seven = parse_stdin(
            br#"{"rate_limits":{"seven_day":{"used_percentage":10,"resets_at":1738425600}}}"#,
        );
        assert_eq!(only_seven.five_hour, None);
        assert!(only_seven.seven_day.is_some());
    }

    #[test]
    fn partially_filled_window_keeps_what_it_has() {
        let o = parse_stdin(
            br#"{"rate_limits":{"five_hour":{"used_percentage":null,"resets_at":1738425600}}}"#,
        );
        assert_eq!(
            o.five_hour,
            Some(Window {
                used_percentage: None,
                resets_at: Some(1_738_425_600)
            })
        );
    }

    #[test]
    fn numeric_strings_are_accepted_and_junk_strings_dropped() {
        let o = parse_stdin(
            br#"{"rate_limits":{
                "five_hour":{"used_percentage":"42.5","resets_at":"1738425600"},
                "seven_day":{"used_percentage":"lots","resets_at":"soon"}
            }}"#,
        );
        assert_eq!(
            o.five_hour,
            Some(Window {
                used_percentage: Some(42.5),
                resets_at: Some(1_738_425_600)
            })
        );
        assert_eq!(o.seven_day, None);
    }

    #[test]
    fn wrong_types_never_error() {
        let o = parse_stdin(
            br#"{"rate_limits":{
                "five_hour":{"used_percentage":true,"resets_at":[1]},
                "seven_day":"not an object",
                "spend_limit":{"used_percentage":{"x":1},"period":"fortnightly"}
            },"version":{"a":1}}"#,
        );
        assert_eq!(o.five_hour, None);
        assert_eq!(o.seven_day, None);
        assert_eq!(o.spend_limit, None);
        assert_eq!(o.claude_code_version, None);
    }

    #[test]
    fn non_finite_and_out_of_range_numbers_are_dropped() {
        let o = parse_stdin(
            br#"{"rate_limits":{"five_hour":{"used_percentage":"NaN","resets_at":-5},
                                "seven_day":{"used_percentage":"inf","resets_at":1e30}}}"#,
        );
        assert_eq!(o.five_hour, None);
        assert_eq!(o.seven_day, None);
    }

    #[test]
    fn fractional_reset_seconds_are_floored() {
        let o = parse_stdin(
            br#"{"rate_limits":{"five_hour":{"used_percentage":1,"resets_at":1738425600.9}}}"#,
        );
        assert_eq!(o.five_hour.unwrap().resets_at, Some(1_738_425_600));
    }

    #[test]
    fn empty_and_whitespace_input_give_defaults() {
        assert_eq!(parse_stdin(b""), Observation::default());
        assert_eq!(parse_stdin(b"  \n\t "), Observation::default());
    }

    #[test]
    fn malformed_input_gives_defaults() {
        for input in [
            &b"not json at all"[..],
            b"{",
            b"{\"rate_limits\":{\"five_hour\":",
            b"[1,2,3]",
            b"\"just a string\"",
            b"null",
            b"\xff\xfe\x00garbage",
        ] {
            assert_eq!(parse_stdin(input), Observation::default());
        }
    }

    #[test]
    fn deeply_nested_input_does_not_overflow_the_stack() {
        let depth = 100_000;
        let mut s = "[".repeat(depth);
        s.push_str(&"]".repeat(depth));
        assert_eq!(parse_stdin(s.as_bytes()), Observation::default());
        let wrapped = format!(r#"{{"rate_limits":{s}}}"#);
        assert_eq!(parse_stdin(wrapped.as_bytes()), Observation::default());
    }

    #[test]
    fn huge_input_is_handled() {
        // 3 MiB of padding around a valid document.
        let pad = "x".repeat(3 * 1024 * 1024);
        let doc = format!(
            r#"{{"pad":"{pad}","rate_limits":{{"five_hour":{{"used_percentage":5,"resets_at":1738425600}}}}}}"#
        );
        // A complete (uncapped) document still parses...
        assert!(parse_stdin(doc.as_bytes()).five_hour.is_some());
        // ...and a truncated one (what the 1 MiB cap produces) degrades to defaults.
        assert_eq!(
            parse_stdin(&doc.as_bytes()[..MAX_INPUT_BYTES]),
            Observation::default()
        );
    }

    #[test]
    fn bom_and_trailing_bytes_are_tolerated() {
        let mut input = b"\xEF\xBB\xBF".to_vec();
        input.extend_from_slice(
            br#"{"rate_limits":{"five_hour":{"used_percentage":7,"resets_at":1738425600}}}"#,
        );
        input.extend_from_slice(b"\n{garbage after the document");
        assert!(parse_stdin(&input).five_hour.is_some());
    }

    #[test]
    fn version_is_sanitised() {
        let v =
            |s: &str| parse_stdin(format!(r#"{{"version":"{s}"}}"#).as_bytes()).claude_code_version;
        assert_eq!(v("2.1.90").as_deref(), Some("2.1.90"));
        assert_eq!(v("2.1.90-beta+1").as_deref(), Some("2.1.90-beta+1"));
        assert_eq!(v("has spaces"), None);
        assert_eq!(v("/home/me/secret"), None);
        assert_eq!(v(""), None);
        assert_eq!(v(&"9".repeat(41)), None);
    }

    #[test]
    fn sensitive_fields_are_not_represented_at_all() {
        // The Observation type has no field that could carry them; this guards
        // against someone adding one without thinking.
        let o = parse_stdin(FULL.as_bytes());
        let debug = format!("{o:?}");
        for secret in ["abc-123", "someone", "secret-project", "opus", "0.01234"] {
            assert!(!debug.contains(secret), "{secret} leaked into {debug}");
        }
    }
}
