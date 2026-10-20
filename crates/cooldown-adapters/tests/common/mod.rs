//! Helpers shared by the integration tests. Not every test binary uses every helper.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use cooldown_adapters::MapContext;
use cooldown_core::{PollContext, Snapshot};
use serde_json::Value;

pub fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/snapshot-schema/fixtures/adapters")
}

pub fn read_json(path: &Path) -> Value {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// One fixture case.
pub struct Fixture {
    pub input: Value,
    pub context: MapContext,
    pub expected: Vec<Snapshot>,
}

pub fn fixture(adapter: &str, case: &str) -> Fixture {
    let dir = fixtures_root().join(adapter);
    let context = serde_json::from_value(read_json(&dir.join(format!("{case}.context.json"))))
        .expect("fixture context");
    let expected = serde_json::from_value(read_json(&dir.join(format!("{case}.expected.json"))))
        .expect("fixture expected");
    Fixture {
        input: read_json(&dir.join(format!("{case}.input.json"))),
        context,
        expected,
    }
}

impl Fixture {
    /// The registry's view of the same moment: the fixture's clock and previous snapshots.
    pub fn poll_context(&self) -> PollContext {
        PollContext {
            now: self.context.now,
            previous: self.context.previous.clone(),
            consecutive_failures: 0,
        }
    }
}

/// Every string in the snapshots' JSON, to search for leaked secrets.
pub fn all_text(snapshots: &[Snapshot]) -> String {
    serde_json::to_string(snapshots).unwrap()
}
