//! The window-mapping rules the two Codex adapters share.
//!
//! Codex reports two windows per limit bucket, a `primary` and a `secondary`. Which slot holds the
//! five-hour window and which the weekly one is NOT fixed (the weekly window has been seen in the
//! `primary` slot), so windows are assigned by their DURATION: about 300 minutes is `session_5h`,
//! about 10080 is `weekly`. A window with any other duration is ignored, and the first window of a
//! kind wins.

use cooldown_core::model::{window_from_duration_minutes, Snapshot, WindowKind};
use serde_json::Value;
use time::OffsetDateTime;

use crate::mapping::{carry_or_unavailable, num, obj, percent_window, MapContext, Meta};

/// A reset time with an absolute value of at least this is milliseconds, not seconds.
const MILLISECONDS_FROM: f64 = 10_000_000_000.0;

/// The field names of one slot (they differ between the app-server protocol and rollout logs).
pub(crate) struct SlotKeys {
    pub used_percent: &'static str,
    pub window_minutes: &'static str,
    pub resets_at: &'static str,
}

/// One usable window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CodexWindow {
    pub percent: f64,
    pub resets_at_seconds: Option<f64>,
}

/// The two windows Codex adapters emit, in emission order.
pub(crate) const WINDOWS: [WindowKind; 2] = [WindowKind::Session5h, WindowKind::Weekly];

/// Assign windows by duration. Returns `[session_5h, weekly]`.
pub(crate) fn windows_by_duration(
    slots: &[Option<&Value>],
    keys: &SlotKeys,
) -> [Option<CodexWindow>; 2] {
    let mut mapped: [Option<CodexWindow>; 2] = [None, None];
    for slot in slots {
        let Some(slot) = obj(*slot) else { continue };
        let (Some(percent), Some(minutes)) = (
            num(slot.get(keys.used_percent)),
            num(slot.get(keys.window_minutes)),
        ) else {
            continue;
        };
        let index = match window_from_duration_minutes(minutes) {
            Some(WindowKind::Session5h) => 0,
            Some(WindowKind::Weekly) => 1,
            _ => continue,
        };
        if mapped[index].is_some() {
            continue;
        }
        // Seconds per the Codex protocol; milliseconds are tolerated.
        let resets_at_seconds = num(slot.get(keys.resets_at)).map(|raw| {
            if raw.abs() < MILLISECONDS_FROM {
                raw
            } else {
                raw / 1000.0
            }
        });
        mapped[index] = Some(CodexWindow {
            percent,
            resets_at_seconds,
        });
    }
    mapped
}

/// One snapshot per window: the mapped value, or the carried-forward / unavailable fallback.
pub(crate) fn snapshots(
    meta: &Meta,
    ctx: &MapContext,
    mapped: [Option<CodexWindow>; 2],
    observed_at: OffsetDateTime,
    none_note: &str,
) -> Vec<Snapshot> {
    WINDOWS
        .iter()
        .zip(mapped)
        .map(|(&window, found)| match found {
            Some(w) => percent_window(
                meta,
                ctx,
                window,
                w.percent,
                w.resets_at_seconds,
                observed_at,
            ),
            None => carry_or_unavailable(meta, ctx, window, none_note, None),
        })
        .collect()
}

/// The fallback for a poll that produced nothing usable at all.
pub(crate) fn fail_all(meta: &Meta, ctx: &MapContext, reason: &str) -> Vec<Snapshot> {
    WINDOWS
        .iter()
        .map(|&window| carry_or_unavailable(meta, ctx, window, reason, None))
        .collect()
}
