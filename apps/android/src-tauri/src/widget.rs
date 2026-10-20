//! What the home-screen widget is given, and which windows it may show.
//!
//! The Glance widget (Kotlin, `CooldownWidget.kt`) reads one JSON file the app and the background
//! sync write. It shows the windows in the order of that file, so the order is decided here, the
//! same way the app's own screens order them (`sortSnapshots` in `packages/ui`). Which of those
//! windows the widget shows is the user's choice and lives on the Kotlin side (see
//! [`normalize_selection`] for what a choice may look like).

use cooldown_core::{AlertIntent, Snapshot, WindowKind};
use serde_json::json;

use crate::phone::iso_ms;

/// The order providers are listed in, as in `packages/ui/src/lib/snapshot-view.ts`. Others follow,
/// alphabetically.
const PROVIDER_ORDER: [&str; 5] = ["claude", "codex", "openrouter", "deepseek", "xai"];

/// The most windows a widget choice may name. A PC cannot send more snapshots than this anyway.
pub const MAX_SELECTED: usize = 64;

/// The longest window id accepted (`provider:account:window`).
pub const MAX_ID_LEN: usize = 160;

fn provider_rank(provider: &str) -> usize {
    PROVIDER_ORDER
        .iter()
        .position(|p| *p == provider)
        .unwrap_or(PROVIDER_ORDER.len())
}

fn window_rank(window: WindowKind) -> usize {
    match window {
        WindowKind::Session5h => 0,
        WindowKind::Daily => 1,
        WindowKind::Weekly => 2,
        WindowKind::Monthly => 3,
        WindowKind::Balance => 4,
    }
}

/// The snapshots in the order the app shows them: known providers first (Claude, Codex, ...), then
/// the rest alphabetically; within a provider 5-hour, daily, weekly, monthly, balance. Windows that
/// tie keep the order the PC sent them in.
pub fn in_ui_order(snapshots: &[Snapshot]) -> Vec<Snapshot> {
    let mut sorted = snapshots.to_vec();
    sorted.sort_by(|a, b| {
        provider_rank(&a.provider)
            .cmp(&provider_rank(&b.provider))
            .then_with(|| a.provider.cmp(&b.provider))
            .then_with(|| window_rank(a.window).cmp(&window_rank(b.window)))
    });
    sorted
}

/// The file the widget reads: every window in the app's order, the bells, and when the data came
/// from the PC (`syncedAt`, so a widget refreshed from old data says so instead of "just now").
pub fn widget_json(
    snapshots: &[Snapshot],
    alerts: &[AlertIntent],
    synced_at_ms: Option<i64>,
) -> String {
    let mut payload = json!({
        "snapshots": in_ui_order(snapshots),
        "alerts": alerts,
    });
    if let Some(at) = synced_at_ms.and_then(iso_ms) {
        payload["syncedAt"] = json!(at);
    }
    payload.to_string()
}

/// Check a widget choice before it is stored. `None` means "every window" (the default, which also
/// picks up windows the PC adds later); a list names windows by snapshot id. Duplicates and blank
/// ids are dropped, order is kept.
pub fn normalize_selection(ids: Option<Vec<String>>) -> Result<Option<Vec<String>>, String> {
    let Some(ids) = ids else {
        return Ok(None);
    };
    let mut kept: Vec<String> = Vec::new();
    for id in ids {
        if id.trim().is_empty() {
            continue;
        }
        if id.len() > MAX_ID_LEN {
            return Err("That is not a window of Cooldown.".into());
        }
        if !kept.contains(&id) {
            kept.push(id);
        }
    }
    if kept.is_empty() {
        return Err("Pick at least one window, or show all of them.".into());
    }
    if kept.len() > MAX_SELECTED {
        return Err("That is more windows than Cooldown can have.".into());
    }
    Ok(Some(kept))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cooldown_core::model::{SnapshotSource, SourceKind, Unit};
    use time::macros::datetime;

    fn snap(provider: &str, account: &str, window: WindowKind) -> Snapshot {
        Snapshot::new(
            provider,
            account,
            window,
            Unit::Percent,
            SnapshotSource {
                kind: SourceKind::Official,
                adapter: "test".into(),
                adapter_version: "0".into(),
            },
            datetime!(2026-10-19 12:00:00 UTC),
        )
    }

    fn ids(list: &[Snapshot]) -> Vec<&str> {
        list.iter().map(|s| s.id.as_str()).collect()
    }

    #[test]
    fn windows_are_listed_the_way_the_app_lists_them() {
        let sorted = in_ui_order(&[
            snap("xai", "team", WindowKind::Balance),
            snap("claude", "default", WindowKind::Weekly),
            snap("zeta", "x", WindowKind::Daily),
            snap("openrouter", "default", WindowKind::Balance),
            snap("claude", "default", WindowKind::Session5h),
            snap("codex", "default", WindowKind::Weekly),
            snap("alpha", "x", WindowKind::Monthly),
            snap("codex", "default", WindowKind::Session5h),
        ]);
        assert_eq!(
            ids(&sorted),
            [
                "claude:default:session_5h",
                "claude:default:weekly",
                "codex:default:session_5h",
                "codex:default:weekly",
                "openrouter:default:balance",
                "xai:team:balance",
                "alpha:x:monthly",
                "zeta:x:daily",
            ]
        );
    }

    #[test]
    fn windows_that_tie_keep_the_order_the_pc_sent() {
        let sorted = in_ui_order(&[
            snap("claude", "work", WindowKind::Weekly),
            snap("claude", "home", WindowKind::Weekly),
        ]);
        assert_eq!(ids(&sorted), ["claude:work:weekly", "claude:home:weekly"]);
    }

    #[test]
    fn the_widget_file_has_ordered_windows_bells_and_the_sync_time() {
        let text = widget_json(
            &[
                snap("codex", "default", WindowKind::Weekly),
                snap("claude", "default", WindowKind::Session5h),
            ],
            &[AlertIntent {
                id: "reset:claude:default:session_5h".into(),
                fire_at: datetime!(2026-10-19 15:00:00 UTC),
                enabled: true,
            }],
            Some(1_792_411_200_000),
        );
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["snapshots"][0]["id"], "claude:default:session_5h");
        assert_eq!(value["snapshots"][1]["id"], "codex:default:weekly");
        assert_eq!(value["alerts"][0]["enabled"], true);
        assert_eq!(value["syncedAt"], "2026-10-19T12:00:00Z");
    }

    #[test]
    fn no_sync_time_is_left_out_not_invented() {
        let value: serde_json::Value = serde_json::from_str(&widget_json(&[], &[], None)).unwrap();
        assert!(value.get("syncedAt").is_none());
        assert_eq!(value["snapshots"].as_array().unwrap().len(), 0);
    }

    fn strings(list: &[&str]) -> Option<Vec<String>> {
        Some(list.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn no_choice_means_every_window() {
        assert_eq!(normalize_selection(None), Ok(None));
    }

    #[test]
    fn a_choice_drops_blanks_and_repeats_and_keeps_the_order() {
        assert_eq!(
            normalize_selection(strings(&[
                "codex:default:weekly",
                "",
                "  ",
                "claude:default:session_5h",
                "codex:default:weekly",
            ])),
            Ok(strings(&[
                "codex:default:weekly",
                "claude:default:session_5h"
            ]))
        );
    }

    #[test]
    fn an_empty_or_absurd_choice_is_refused() {
        assert!(normalize_selection(strings(&[])).is_err());
        assert!(normalize_selection(strings(&["", " "])).is_err());
        assert!(normalize_selection(Some(vec!["x".repeat(MAX_ID_LEN + 1)])).is_err());
        let many: Vec<String> = (0..=MAX_SELECTED)
            .map(|n| format!("p:{n}:weekly"))
            .collect();
        assert!(normalize_selection(Some(many)).is_err());
        let limit: Vec<String> = (0..MAX_SELECTED).map(|n| format!("p:{n}:weekly")).collect();
        assert_eq!(normalize_selection(Some(limit.clone())), Ok(Some(limit)));
    }
}
