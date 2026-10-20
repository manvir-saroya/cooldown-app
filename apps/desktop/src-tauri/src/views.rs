//! What the commands return to the UI: the types in `packages/ui/src/data/types.ts`, spelled the
//! same way (camelCase, ISO-8601 UTC strings, epoch milliseconds in chart series).
//!
//! The mapping functions are pure so they are tested without a window.

use cooldown_core::{HistoryPoint, Snapshot, SourceKind, Status, Unit, WindowKind};
use cooldown_sync::PeerRecord;
use serde::Serialize;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// RFC 3339 in UTC, the format every timestamp in the contract uses.
pub fn iso(time: OffsetDateTime) -> String {
    time.to_offset(time::UtcOffset::UTC)
        .format(&Rfc3339)
        .unwrap_or_default()
}

fn ms(time: OffsetDateTime) -> i64 {
    (time.unix_timestamp_nanos() / 1_000_000) as i64
}

fn from_ms(ms: i64) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()
}

/// `AdapterInfo`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterInfo {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub kind: SourceKind,
    pub version: String,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_polled_at: Option<String>,
    pub credentials: &'static str,
    pub has_key: bool,
    /// Whether the user has this source switched on (read and shown on the dashboard).
    pub enabled: bool,
    /// The agent whose status line hook feeds this source, when it is not Claude Code (which has
    /// its own section): `antigravity`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_agent: Option<&'static str>,
    /// One line on how this source connects, for the Settings list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup: Option<&'static str>,
}

/// `HistoryRange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryRange {
    Day,
    Week,
    Month,
}

impl HistoryRange {
    /// Parse the UI's `'24h' | '7d' | '30d'`.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "24h" => Some(HistoryRange::Day),
            "7d" => Some(HistoryRange::Week),
            "30d" => Some(HistoryRange::Month),
            _ => None,
        }
    }

    /// The UI spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            HistoryRange::Day => "24h",
            HistoryRange::Week => "7d",
            HistoryRange::Month => "30d",
        }
    }

    /// How far back the range reaches.
    pub fn span(self) -> time::Duration {
        match self {
            HistoryRange::Day => time::Duration::hours(24),
            HistoryRange::Week => time::Duration::days(7),
            HistoryRange::Month => time::Duration::days(30),
        }
    }
}

/// `HistoryPoint` in the UI's shape.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ChartPoint {
    /// Epoch milliseconds.
    pub t: i64,
    /// Percent used, or remaining USD for balances.
    pub value: f64,
}

/// `HistorySeries`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySeries {
    pub snapshot_id: String,
    pub unit: Unit,
    pub kind: &'static str,
    pub range: &'static str,
    pub points: Vec<ChartPoint>,
    pub resets: Vec<i64>,
}

/// A usage drop of more than this many percentage points between two readings is drawn as a reset.
const RESET_DROP_POINTS: f64 = 10.0;

/// The most points a series carries. A chart is a few hundred pixels wide, so more only makes it slow
/// to send and to draw: a month of Claude statusline readings can be tens of thousands of points.
const MAX_CHART_POINTS: usize = 800;

/// Thin `points` to at most `max` (oldest first stays oldest first). Each stretch of the series keeps
/// its first, lowest, highest and last point, so peaks, the drop at a reset and both ends survive.
fn thin(points: Vec<ChartPoint>, max: usize) -> Vec<ChartPoint> {
    let buckets = max / 4;
    if points.len() <= max || buckets == 0 {
        return points;
    }
    let len = points.len();
    let mut kept = Vec::with_capacity(max);
    for bucket in 0..buckets {
        let start = bucket * len / buckets;
        let end = (bucket + 1) * len / buckets;
        let (mut lowest, mut highest) = (start, start);
        for i in start..end {
            if points[i].value < points[lowest].value {
                lowest = i;
            }
            if points[i].value > points[highest].value {
                highest = i;
            }
        }
        let mut picks = [start, lowest, highest, end - 1];
        picks.sort_unstable();
        let mut previous = None;
        for i in picks {
            if previous != Some(i) {
                kept.push(points[i]);
                previous = Some(i);
            }
        }
    }
    kept
}

/// Build the chart series for `snapshot` from its stored history (oldest first).
pub fn history_series(
    snapshot: &Snapshot,
    points: &[HistoryPoint],
    range: HistoryRange,
) -> HistorySeries {
    let balance = snapshot.window == WindowKind::Balance;
    let limit = snapshot.limit;
    let value_of = |p: &HistoryPoint| -> Option<f64> {
        if balance {
            p.remaining.or_else(|| Some(limit? - p.used?))
        } else {
            p.percent_used.or_else(|| {
                let limit = limit.filter(|l| *l > 0.0)?;
                Some(p.used? / limit * 100.0)
            })
        }
    };

    let chart: Vec<ChartPoint> = points
        .iter()
        .filter_map(|p| {
            value_of(p)
                .filter(|v| v.is_finite())
                .map(|value| ChartPoint {
                    t: ms(p.observed_at),
                    value,
                })
        })
        .collect();

    let resets = if balance {
        Vec::new()
    } else {
        chart
            .windows(2)
            .filter(|pair| pair[1].value < pair[0].value - RESET_DROP_POINTS)
            .map(|pair| pair[1].t)
            .collect()
    };

    HistorySeries {
        snapshot_id: snapshot.id.clone(),
        unit: snapshot.unit,
        kind: if balance { "balance" } else { "usage" },
        range: range.as_str(),
        // Resets were found on the full series above; thinning must not hide them.
        points: thin(chart, MAX_CHART_POINTS),
        resets,
    }
}

/// `PairedDevice`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDevice {
    pub id: String,
    pub name: String,
    pub platform: &'static str,
    pub paired_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
}

/// A trusted phone as the UI lists it.
pub fn paired_device(record: &PeerRecord) -> PairedDevice {
    PairedDevice {
        id: record.device_id.to_hex(),
        name: record.name.clone(),
        platform: if record.platform == "windows" {
            "windows"
        } else {
            "android"
        },
        paired_at: from_ms(record.paired_at_ms).map(iso).unwrap_or_default(),
        last_seen_at: record.last_seen_ms.and_then(from_ms).map(iso),
    }
}

/// `HookStatus`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookStatus {
    pub installed: bool,
    pub chains_existing: bool,
    pub settings_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<String>,
    pub inbox_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_data_at: Option<String>,
}

/// `PairingPayload`.
///
/// `Debug` is written by hand: the QR text is a one-time secret and must not reach a log, even by
/// accident.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingPayload {
    /// The QR text. It carries a one-time secret: shown in this window only.
    pub uri: String,
    /// The same text as `uri`, for a person to copy to the phone when its camera cannot scan
    /// (the phone's "Paste code instead"). It is just as secret as the QR code, and the window
    /// shows it only while the QR code is shown.
    pub manual_code: String,
    pub host: String,
    pub port: u16,
    pub expires_at: String,
    /// This PC's short device id, so the user can tell PCs apart on the phone.
    pub fingerprint: String,
    pub placeholder: bool,
}

impl std::fmt::Debug for PairingPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingPayload")
            .field("uri", &"<secret>")
            .field("manual_code", &"<secret>")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("expires_at", &self.expires_at)
            .field("fingerprint", &self.fingerprint)
            .field("placeholder", &self.placeholder)
            .finish()
    }
}

/// Where a pairing stands, for the Pairing screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "phase")]
pub enum PairingState {
    /// No pairing window is open.
    Idle,
    /// The QR code is up; waiting for a phone.
    Waiting,
    /// A phone finished the handshake. Ask the user to compare the code, then confirm.
    Confirm {
        name: String,
        platform: String,
        code: String,
    },
    /// The phone was paired.
    Paired { name: String },
    /// The attempt ended without a pairing.
    Failed { reason: String },
}

/// `aabbccdd…` → `AABB CCDD`, the short form of a device id people compare.
pub fn short_fingerprint(hex: &str) -> String {
    let head: String = hex.chars().take(8).collect::<String>().to_ascii_uppercase();
    if head.len() == 8 {
        format!("{} {}", &head[..4], &head[4..])
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cooldown_core::SnapshotSource;
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-10-15 12:00:00 UTC);

    fn snapshot(window: WindowKind, unit: Unit) -> Snapshot {
        Snapshot::new(
            "claude",
            "default",
            window,
            unit,
            SnapshotSource {
                kind: SourceKind::Official,
                adapter: "claude-statusline".into(),
                adapter_version: "0.1.0".into(),
            },
            NOW,
        )
    }

    fn point(
        minutes_ago: i64,
        percent: Option<f64>,
        used: Option<f64>,
        remaining: Option<f64>,
    ) -> HistoryPoint {
        HistoryPoint {
            observed_at: NOW - time::Duration::minutes(minutes_ago),
            percent_used: percent,
            used,
            remaining,
        }
    }

    #[test]
    fn iso_is_utc_rfc3339() {
        assert_eq!(iso(NOW), "2026-10-15T12:00:00Z");
        let shifted = NOW.to_offset(time::UtcOffset::from_hms(5, 30, 0).unwrap());
        assert_eq!(iso(shifted), "2026-10-15T12:00:00Z");
    }

    #[test]
    fn ranges_parse_and_round_trip() {
        for text in ["24h", "7d", "30d"] {
            assert_eq!(HistoryRange::parse(text).unwrap().as_str(), text);
        }
        assert_eq!(HistoryRange::parse("1y"), None);
        assert_eq!(HistoryRange::Week.span(), time::Duration::days(7));
    }

    #[test]
    fn usage_series_marks_resets_on_big_drops() {
        let s = snapshot(WindowKind::Session5h, Unit::Percent);
        let points = [
            point(40, Some(20.0), None, None),
            point(30, Some(85.0), None, None),
            point(20, Some(3.0), None, None),
            point(10, Some(1.0), None, None),
            point(5, None, None, None),
        ];
        let series = history_series(&s, &points, HistoryRange::Day);
        assert_eq!(series.kind, "usage");
        assert_eq!(series.range, "24h");
        assert_eq!(series.points.len(), 4, "a point with no value is skipped");
        assert_eq!(series.resets, vec![ms(NOW - time::Duration::minutes(20))]);
        let json = serde_json::to_value(&series).unwrap();
        assert_eq!(json["snapshotId"], "claude:default:session_5h");
        assert_eq!(json["unit"], "percent");
    }

    #[test]
    fn a_dense_series_is_thinned_but_keeps_its_ends_peaks_and_resets() {
        let s = snapshot(WindowKind::Session5h, Unit::Percent);
        // Two sawtooth windows over 5000 minutes: rising 0..95, dropping back, rising again.
        let points: Vec<HistoryPoint> = (0..5000)
            .map(|i| {
                let value = f64::from(i % 2500) / 2500.0 * 95.0;
                point(5000 - i64::from(i), Some(value), None, None)
            })
            .collect();
        let series = history_series(&s, &points, HistoryRange::Month);
        assert!(
            series.points.len() <= MAX_CHART_POINTS,
            "{} points",
            series.points.len()
        );
        assert!(series.points.len() > 200, "still a real curve");
        assert!(
            series.points.windows(2).all(|w| w[0].t < w[1].t),
            "still oldest first"
        );
        assert_eq!(
            series.points.first().unwrap().t,
            ms(NOW - time::Duration::minutes(5000))
        );
        assert_eq!(
            series.points.last().unwrap().t,
            ms(NOW - time::Duration::minutes(1))
        );
        let peak = series
            .points
            .iter()
            .map(|p| p.value)
            .fold(f64::MIN, f64::max);
        assert!(
            (peak - 94.96).abs() < 0.01,
            "the peak of the window survives, got {peak}"
        );
        // The reset is found on the full series, so it is exactly where the data dropped.
        assert_eq!(series.resets, vec![ms(NOW - time::Duration::minutes(2500))]);
    }

    #[test]
    fn a_short_series_is_left_alone() {
        let s = snapshot(WindowKind::Session5h, Unit::Percent);
        let points: Vec<HistoryPoint> = (0..MAX_CHART_POINTS as i64)
            .map(|i| point(i, Some(10.0), None, None))
            .collect();
        let series = history_series(&s, &points, HistoryRange::Day);
        assert_eq!(series.points.len(), MAX_CHART_POINTS);
    }

    #[test]
    fn usage_without_percent_uses_used_over_limit() {
        let mut s = snapshot(WindowKind::Daily, Unit::Requests);
        s.limit = Some(200.0);
        let series = history_series(&s, &[point(1, None, Some(50.0), None)], HistoryRange::Day);
        assert_eq!(series.points[0].value, 25.0);
    }

    #[test]
    fn balance_series_uses_remaining_and_never_resets() {
        let mut s = snapshot(WindowKind::Balance, Unit::Usd);
        s.limit = Some(100.0);
        let points = [
            point(30, None, None, Some(80.0)),
            point(20, None, Some(50.0), None),
            point(10, None, None, Some(10.0)),
        ];
        let series = history_series(&s, &points, HistoryRange::Month);
        assert_eq!(series.kind, "balance");
        let values: Vec<f64> = series.points.iter().map(|p| p.value).collect();
        assert_eq!(values, vec![80.0, 50.0, 10.0]);
        assert!(series.resets.is_empty());
    }

    #[test]
    fn fingerprints_are_short_and_grouped() {
        assert_eq!(short_fingerprint("a1b2c3d4e5f60718"), "A1B2 C3D4");
        assert_eq!(short_fingerprint("abc"), "ABC");
    }

    #[test]
    fn pairing_state_serializes_with_a_phase_tag() {
        let json = serde_json::to_value(PairingState::Confirm {
            name: "Pixel".into(),
            platform: "android".into(),
            code: "123 456".into(),
        })
        .unwrap();
        assert_eq!(json["phase"], "confirm");
        assert_eq!(json["code"], "123 456");
        assert_eq!(
            serde_json::to_value(PairingState::Idle).unwrap()["phase"],
            "idle"
        );
    }

    #[test]
    fn adapter_info_omits_empty_optionals() {
        let info = AdapterInfo {
            id: "codex".into(),
            name: "Codex".into(),
            provider: "codex".into(),
            kind: SourceKind::Scraped,
            version: "0.1.0".into(),
            status: Status::Ok,
            note: None,
            last_polled_at: None,
            credentials: "none",
            has_key: false,
            enabled: true,
            hook_agent: None,
            setup: None,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert!(json.get("note").is_none());
        assert_eq!(json["hasKey"], false);
        assert_eq!(json["enabled"], true);
        assert_eq!(json["kind"], "scraped");
    }
}
