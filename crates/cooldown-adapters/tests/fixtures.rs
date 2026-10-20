//! Runs every case under `packages/snapshot-schema/fixtures/adapters` through the adapters'
//! pure mapping functions and requires exactly the snapshots in `<case>.expected.json`.
//!
//! The same files are consumed by the TypeScript suite (`test/fixtures.test.ts`), so the Rust
//! adapters and the shared contract cannot drift apart. Fixtures are the authority: when Rust and
//! the TypeScript reference disagree, `expected.json` wins.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cooldown_adapters::MapContext;
use cooldown_adapters::{
    antigravity, claude, codex_app_server, codex_rollout, copilot, deepseek, kimi, openrouter, xai,
};
use cooldown_core::{effective_status, Snapshot};
use serde_json::Value;

/// The pure function each adapter's fixtures are tested through.
type MapFn = fn(&Value, &MapContext) -> Vec<Snapshot>;

const ADAPTERS: [(&str, MapFn); 9] = [
    ("claude-statusline", claude::map_input),
    ("codex-app-server", codex_app_server::map_input),
    ("codex-rollout-log", codex_rollout::map_input),
    ("openrouter", openrouter::map_input),
    ("deepseek", deepseek::map_input),
    ("xai", xai::map_input),
    ("kimi", kimi::map_input),
    ("copilot", copilot::map_input),
    ("antigravity-statusline", antigravity::map_input),
];

/// The number of cases the repository ships. Adding a case is fine; losing one is not.
const MINIMUM_CASES: usize = 48;

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/snapshot-schema/fixtures/adapters")
}

fn read_json(path: &Path) -> Value {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn case_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()?
                .strip_suffix(".input.json")
                .map(str::to_owned)
        })
        .collect();
    names.sort();
    names
}

/// Structural JSON equality where numbers compare by value.
fn same_json(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_json(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(key, p)| y.get(key).is_some_and(|q| same_json(p, q)))
        }
        _ => a == b,
    }
}

fn pretty(snapshots: &[Snapshot]) -> String {
    serde_json::to_string_pretty(snapshots).unwrap()
}

/// One case. Returns a description of every way it failed.
fn run_case(adapter: &str, map: MapFn, dir: &Path, case: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let input = read_json(&dir.join(format!("{case}.input.json")));
    let context_value = read_json(&dir.join(format!("{case}.context.json")));
    let context: MapContext = serde_json::from_value(context_value)
        .unwrap_or_else(|e| panic!("{adapter}/{case}: bad context: {e}"));
    let expected_value = read_json(&dir.join(format!("{case}.expected.json")));
    let expected: Vec<Snapshot> = serde_json::from_value(expected_value.clone())
        .unwrap_or_else(|e| panic!("{adapter}/{case}: bad expected: {e}"));

    let actual = map(&input, &context);

    if actual != expected {
        problems.push(format!(
            "output differs\n--- expected\n{}\n--- actual\n{}",
            pretty(&expected),
            pretty(&actual)
        ));
    }
    // The serialized form must also match (key presence, no nulls, key names). Numbers compare by
    // value: `64` and `64.0` are the same JSON number.
    let actual_json = serde_json::to_value(&actual).unwrap();
    if !same_json(&actual_json, &expected_value) {
        problems.push("serialized JSON differs from expected.json".to_owned());
    }
    for snapshot in &actual {
        if let Err(e) = snapshot.validate() {
            problems.push(format!("{} does not validate: {e}", snapshot.id));
        }
        // Adapters encode only what is certain at poll time: no age limit is applied, so the
        // consumer-side effective status (without an age limit) equals the stored one.
        let effective = effective_status(snapshot, context.now, Duration::MAX);
        if effective != snapshot.status {
            problems.push(format!(
                "{}: stored status {:?} but effective status is {:?}",
                snapshot.id, snapshot.status, effective
            ));
        }
    }
    problems
}

#[test]
fn every_fixture_case_produces_exactly_the_expected_snapshots() {
    let root = fixtures_root();
    assert!(
        root.is_dir(),
        "fixtures directory not found: {} (this crate must be built inside the Cooldown monorepo)",
        root.display()
    );

    let mut total = 0usize;
    let mut passed = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for (adapter, map) in ADAPTERS {
        let dir = root.join(adapter);
        let cases = case_names(&dir);
        assert!(!cases.is_empty(), "no fixture cases for {adapter}");
        for case in cases {
            total += 1;
            let problems = run_case(adapter, map, &dir, &case);
            if problems.is_empty() {
                passed += 1;
            } else {
                failures.push(format!("{adapter}/{case}:\n{}", problems.join("\n")));
            }
        }
    }

    println!("{passed} of {total} fixtures pass");
    assert!(
        total >= MINIMUM_CASES,
        "expected at least {MINIMUM_CASES} fixture cases, found {total}"
    );
    assert!(
        failures.is_empty(),
        "{passed} of {total} fixtures pass; failures:\n\n{}",
        failures.join("\n\n")
    );
}

#[test]
fn every_fixture_directory_belongs_to_a_known_adapter() {
    let root = fixtures_root();
    let mut on_disk: Vec<String> = fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    on_disk.sort();
    let mut known: Vec<String> = ADAPTERS
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    known.sort();
    assert_eq!(
        on_disk, known,
        "a fixture directory has no adapter under test"
    );
}
