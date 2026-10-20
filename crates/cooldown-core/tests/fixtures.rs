//! Fixture harness: every snapshot the shared adapter fixtures expect must deserialize into the
//! Rust model and pass `validate()`.
//!
//! The fixtures live in `packages/snapshot-schema/fixtures/adapters/<adapter>/<case>.expected.json`
//! (each a JSON array of snapshots) and are shared with the TypeScript validators. While they are
//! still being written this test passes with a note instead of failing the build.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use cooldown_core::{Snapshot, SnapshotSource};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/snapshot-schema/fixtures/adapters")
}

fn collect(dir: &Path, suffix: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, suffix, out);
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(suffix))
        {
            out.push(path);
        }
    }
}

fn find(suffix: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect(&fixtures_dir(), suffix, &mut files);
    files
}

fn display(path: &Path) -> String {
    path.strip_prefix(fixtures_dir())
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Compare JSON ignoring the integer-versus-float spelling of numbers (`9` and `9.0`).
fn normalize(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                *value = serde_json::json!(f);
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(normalize),
        serde_json::Value::Object(map) => map.values_mut().for_each(normalize),
        _ => {}
    }
}

#[test]
fn expected_snapshots_deserialize_and_validate() {
    let files = find(".expected.json");
    if files.is_empty() {
        println!(
            "fixtures pending: no *.expected.json under {}; nothing to check yet",
            fixtures_dir().display()
        );
        return;
    }

    let mut checked = 0;
    let mut adapters = HashSet::new();
    for path in &files {
        let name = display(path);
        let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{name}: cannot read: {e}"));
        let snapshots: Vec<Snapshot> =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: not a Snapshot[]: {e}"));

        let mut ids = HashSet::new();
        for snapshot in &snapshots {
            snapshot
                .validate()
                .unwrap_or_else(|e| panic!("{name}: {} is invalid: {e}", snapshot.id));
            assert!(
                ids.insert(snapshot.id.clone()),
                "{name}: duplicate id {}",
                snapshot.id
            );
            adapters.insert(snapshot.source.adapter.clone());
            checked += 1;
        }

        // The Rust model must write back exactly what the fixture says (same keys, same values).
        let mut original: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut rewritten = serde_json::to_value(&snapshots).unwrap();
        normalize(&mut original);
        normalize(&mut rewritten);
        assert_eq!(
            original, rewritten,
            "{name}: Rust serialization differs from the fixture"
        );
    }
    println!(
        "checked {checked} snapshot(s) in {} fixture file(s) from {} adapter(s)",
        files.len(),
        adapters.len()
    );
}

#[test]
fn fixture_contexts_carry_valid_previous_snapshots() {
    let files = find(".context.json");
    if files.is_empty() {
        println!("fixtures pending: no *.context.json yet; nothing to check");
        return;
    }
    for path in &files {
        let name = display(path);
        let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{name}: cannot read: {e}"));
        let value: serde_json::Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: not valid JSON: {e}"));
        let Some(previous) = value.get("previous") else {
            continue;
        };
        let snapshots: Vec<Snapshot> = serde_json::from_value(previous.clone())
            .unwrap_or_else(|e| panic!("{name}: previous is not a Snapshot[]: {e}"));
        for snapshot in snapshots {
            snapshot
                .validate()
                .unwrap_or_else(|e| panic!("{name}: previous {} is invalid: {e}", snapshot.id));
        }
    }
}

#[test]
fn every_expected_snapshot_matches_its_adapter_directory() {
    // `fixtures/adapters/<adapter>/...`: the directory name is the `source.adapter` value.
    for path in find(".expected.json") {
        let adapter_dir = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        let text = fs::read_to_string(&path).unwrap();
        let snapshots: Vec<Snapshot> = serde_json::from_str(&text).unwrap();
        for snapshot in snapshots {
            let SnapshotSource { adapter, .. } = &snapshot.source;
            assert_eq!(
                *adapter,
                adapter_dir,
                "{}: source.adapter must equal the directory name",
                display(&path)
            );
        }
    }
}
