//! Cooldown core: the pieces of the app that do not depend on the UI or the operating system.
//!
//! - [`model`]: the snapshot contract (`Snapshot`, `AlertIntent`, `SyncPayload`) with validation and
//!   helpers, mirroring `packages/snapshot-schema`.
//! - [`adapter`]: the `Adapter` trait, failure-to-snapshot helpers and a `Registry` that polls
//!   adapters on their own schedules with backoff and jitter.
//! - [`store`]: SQLite persistence that never loses the last good value.
//! - [`alerts`]: pure reset-alert reconciliation shared by the Windows tray timer and Android.
//! - [`usage`]: pure "running low" and "ran out" notifications.
//! - [`clock`]: the `Clock` trait and a fake clock for tests.
//!
//! Nothing here performs network access or reads any credential.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod adapter;
pub mod alerts;
pub mod clock;
pub mod model;
pub mod store;
pub mod usage;

pub use adapter::{
    is_failed_poll, next_delay, Adapter, AdapterFailure, PollContext, PollEvent, Registry,
    RegistryConfig, RegistryHandle, SnapshotTemplate,
};
pub use alerts::{
    apply_actions, limit_label, reconcile, reconcile_with, reset_title, should_fire,
    should_fire_with, AlertAction, CancelReason, FireReason, ReconcileConfig, Reconciler,
};
pub use clock::{Clock, FakeClock, SystemClock};
pub use model::{
    alert_id, effective_status, parse_alert_id, seconds_until_reset, snapshot_id,
    window_from_duration_minutes, AlertIntent, Snapshot, SnapshotSource, SourceKind, Status,
    SyncPayload, Unit, ValidationError, WindowKind, DEFAULT_MAX_AGE, SCHEMA_VERSION,
};
pub use store::{HistoryPoint, Store, StoreError, StoreOptions, UpsertKind, UpsertOutcome};
pub use usage::{usage_alerts, UsageAlert, UsageConfig, UsageKind, UsageNotified};
