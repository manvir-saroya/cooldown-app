//! The desktop app's engine, free of any window code so it can be tested on its own.
//!
//! - Polls the adapters through `cooldown_core::Registry` and keeps every reading in the SQLite
//!   store.
//! - Owns the reset alerts: the `Reconciler` decides, this module applies its actions and shows the
//!   notification. The tray app's own timer (`run_alert_timer`) does this; Tauri's notification
//!   plugin cannot schedule on desktop.
//! - Pushes the latest state to paired phones.
//!
//! The window side is reached through two callbacks: `notify` (an OS notification) and `changed`
//! (tell the UI to refetch).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cooldown_adapters::{SecretStore, SecretString};
use cooldown_core::{
    reset_title, should_fire, AlertAction, AlertIntent, CancelReason, PollEvent, ReconcileConfig,
    Reconciler, Registry, Snapshot, Store, StoreError, SyncPayload, SystemClock, SCHEMA_VERSION,
};
use time::OffsetDateTime;
use tokio::sync::{watch, Notify};

use crate::adapters::{self, catalog_entry, summarize, AdapterRuntime, Credentials, CATALOG};
use crate::phone::{HostEnv, PhoneLink, Started};
use crate::secrets;
use crate::settings::{Notifications, SettingsFile, SyncResume};
use crate::views::{history_series, iso, AdapterInfo, HistoryRange, HistorySeries};

/// How often the alert timer wakes up when nothing else happens.
pub const ALERT_TICK: Duration = Duration::from_secs(15);

/// How often the running phone server re-checks which networks it may listen on.
pub const NETWORK_CHECK: Duration = Duration::from_secs(120);

/// What the network watch does on one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchAction {
    /// Nothing to serve: no server and no paired phone.
    Idle,
    /// The server runs: check the networks again; it restarts on the allowed addresses or stops.
    Recheck,
    /// Phones are paired but the server is down (it failed closed, or never started): start it.
    Start,
}

/// The network watch's decision. A running server is always re-checked. A stopped one is started
/// only when a paired phone needs it, so a fresh install still opens no port.
pub fn watch_action(server_running: bool, has_peers: bool) -> WatchAction {
    match (server_running, has_peers) {
        (true, _) => WatchAction::Recheck,
        (false, true) => WatchAction::Start,
        (false, false) => WatchAction::Idle,
    }
}

/// How a watch tick ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchOutcome {
    /// Nothing to do.
    Idle,
    /// The server runs. `restored` is true when this tick brought it back from stopped.
    Listening {
        /// Whether the server was down before this tick.
        restored: bool,
    },
    /// The server is not running (or was just stopped), with the reason shown to the user.
    Down(String),
}

/// Shows an OS notification (title, body).
pub type Notifier = Box<dyn Fn(&str, &str) + Send + Sync>;
/// Tells the UI that data changed.
pub type ChangeSignal = Box<dyn Fn() + Send + Sync>;

/// Lock a mutex and carry on if a thread panicked while holding it.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The engine.
pub struct Backend {
    store: Store,
    secrets: Arc<dyn SecretStore>,
    settings: SettingsFile,
    keys: Mutex<HashMap<&'static str, bool>>,
    runtime: Mutex<HashMap<String, AdapterRuntime>>,
    problems: Mutex<HashMap<&'static str, String>>,
    reconciler: Mutex<Reconciler>,
    restart: Notify,
    polls: watch::Sender<u64>,
    phone: PhoneLink,
    notify: Notifier,
    changed: ChangeSignal,
    /// Tests only: how many times the alerts or the snapshots were read from the store.
    #[cfg(test)]
    store_reads: std::sync::atomic::AtomicUsize,
}

/// What one change reads from the store: the alerts and the latest snapshot of every window.
/// Loaded once, then handed to the alert reconciliation and the phone payload.
struct Stored {
    /// Ordered by id, like `Store::alerts`.
    alerts: Vec<AlertIntent>,
    latest: Vec<Snapshot>,
}

impl Stored {
    fn into_payload(self) -> SyncPayload {
        SyncPayload {
            schema_version: SCHEMA_VERSION,
            // The server stamps its own device id; this value is replaced.
            device_id: "pc".to_owned(),
            generated_at: OffsetDateTime::now_utc(),
            snapshots: self.latest,
            alerts: self.alerts,
        }
    }
}

impl Backend {
    /// Open the store and settings in `data_dir`. Reads which API keys are saved (once, here, so
    /// the credential store is not asked on every screen refresh).
    pub fn open(
        data_dir: &Path,
        secrets: Arc<dyn SecretStore>,
        notify: Notifier,
        changed: ChangeSignal,
    ) -> Result<Arc<Self>, String> {
        Self::open_with_env(data_dir, secrets, notify, changed, HostEnv::system())
    }

    /// Like [`open`](Self::open), with the machine's networks and identity store replaced (tests).
    pub fn open_with_env(
        data_dir: &Path,
        secrets: Arc<dyn SecretStore>,
        notify: Notifier,
        changed: ChangeSignal,
        host: HostEnv,
    ) -> Result<Arc<Self>, String> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| format!("Cannot create the data folder: {e}"))?;
        let store = Store::open(data_dir.join("cooldown.sqlite3"))
            .map_err(|e| format!("Cannot open the local database: {e}"))?;
        let mut keys = HashMap::new();
        for entry in CATALOG
            .iter()
            .filter(|e| e.credentials != Credentials::None)
        {
            let saved = secrets
                .get(&adapters::secret_name(entry.id))
                .ok()
                .flatten()
                .is_some();
            keys.insert(entry.id, saved);
        }
        let settings = SettingsFile::load(data_dir.join("settings.json"));
        let reconcile = reconcile_config(&settings.get().notifications);
        Ok(Arc::new(Backend {
            store,
            secrets,
            settings,
            keys: Mutex::new(keys),
            runtime: Mutex::new(HashMap::new()),
            problems: Mutex::new(HashMap::new()),
            reconciler: Mutex::new(Reconciler::new(reconcile)),
            restart: Notify::new(),
            polls: watch::channel(0).0,
            phone: PhoneLink::with_env(data_dir, host),
            notify,
            changed,
            #[cfg(test)]
            store_reads: std::sync::atomic::AtomicUsize::new(0),
        }))
    }

    /// App settings.
    pub fn settings(&self) -> &SettingsFile {
        &self.settings
    }

    /// The phone link.
    pub fn phone(&self) -> &PhoneLink {
        &self.phone
    }

    fn has_key(&self, id: &str) -> bool {
        lock(&self.keys).get(id).copied().unwrap_or(false)
    }

    // ------------------------------------------------------------------ polling

    /// Run the adapters until the process ends. A restart request (Refresh, a saved key) stops
    /// every worker and starts them again, which polls everything at once.
    pub async fn run_polling(self: Arc<Self>) {
        loop {
            let (list, problems) = {
                let keys = lock(&self.keys).clone();
                adapters::build(&self.settings.get(), self.secrets.clone(), &|id| {
                    keys.get(id).copied().unwrap_or(false)
                })
            };
            *lock(&self.problems) = problems.into_iter().collect();
            let registry = list
                .into_iter()
                .fold(Registry::new(Arc::new(SystemClock)), Registry::register);
            let (handle, mut events) = registry.start();
            loop {
                tokio::select! {
                    event = events.recv() => match event {
                        Some(event) => self.on_poll(event),
                        None => {
                            // No adapters are running: wait for a change.
                            self.restart.notified().await;
                            break;
                        }
                    },
                    () = self.restart.notified() => break,
                }
            }
            handle.shutdown().await;
        }
    }

    /// Ask for every adapter to poll now.
    pub fn restart_polling(&self) {
        self.restart.notify_one();
    }

    /// Restart polling and wait (up to `timeout`) for the first fresh reading.
    pub async fn refresh(&self, timeout: Duration) {
        let mut polls = self.polls.subscribe();
        polls.borrow_and_update();
        self.restart_polling();
        let _ = tokio::time::timeout(timeout, polls.changed()).await;
    }

    /// Store one poll's readings and react to them.
    pub fn on_poll(&self, event: PollEvent) {
        // A poll that was already running when its key was removed must not bring it back.
        let needs_key = catalog_entry(event.adapter)
            .is_some_and(|entry| entry.credentials != Credentials::None);
        if needs_key && !self.has_key(event.adapter) {
            return;
        }
        for snapshot in &event.snapshots {
            if let Err(e) = self.store.upsert(snapshot) {
                log::warn!("dropped a reading from {}: {e}", event.adapter);
            }
        }
        lock(&self.runtime).insert(
            event.adapter.to_owned(),
            summarize(event.polled_at, &event.snapshots),
        );
        self.polls.send_modify(|n| *n += 1);
        self.after_change();
    }

    /// Reconcile alerts, sync phones and tell the UI. The store is read once; the alerts and the
    /// phones both work from that read.
    pub fn after_change(&self) {
        if let Ok(mut stored) = self.load() {
            let now = OffsetDateTime::now_utc();
            self.reconcile_stored(&mut stored, now);
            self.notify_usage(&stored.latest, now);
            self.push_to_phones(stored);
        }
        (self.changed)();
    }

    // ------------------------------------------------------------------ notifications

    /// The automatic notifications the user chose.
    pub fn notifications(&self) -> Notifications {
        self.settings.get().notifications
    }

    /// Change the automatic notifications. Takes effect at once: "limit is back" arms (or stops
    /// arming) bells on the next reconcile, which runs here.
    pub fn set_notifications(&self, next: Notifications) -> Result<(), String> {
        next.validate()?;
        self.settings
            .update(|s| s.notifications = next)
            .map_err(|e| format!("Could not save the setting: {e}"))?;
        lock(&self.reconciler).set_config(reconcile_config(&next));
        self.after_change();
        Ok(())
    }

    /// Show the "running low" and "ran out" notifications that are due, once per cycle each.
    fn notify_usage(&self, latest: &[Snapshot], now: OffsetDateTime) {
        let settings = self.settings.get();
        let config = usage_config(&settings.notifications);
        let due = cooldown_core::usage_alerts(&config, latest, &settings.usage_notified, now);
        if due.is_empty() {
            return;
        }
        for alert in &due {
            (self.notify)(&alert.title(), &alert.body(now));
        }
        let _ = self.settings.update(|s| {
            s.usage_notified
                .extend(due.iter().map(cooldown_core::UsageAlert::notified));
            cooldown_core::usage::prune(&mut s.usage_notified, now);
        });
    }

    /// Read the alerts and the latest snapshots (one read of each).
    fn load(&self) -> Result<Stored, String> {
        Ok(Stored {
            alerts: self.alerts()?,
            latest: self.snapshots()?,
        })
    }

    // ------------------------------------------------------------------ reads for the UI

    /// The latest snapshot of every window, leaving out providers the user switched off (their
    /// readings stay in the store and come back when the source is switched on again).
    pub fn snapshots(&self) -> Result<Vec<Snapshot>, String> {
        #[cfg(test)]
        self.store_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let settings = self.settings.get();
        let hidden: Vec<&str> = CATALOG
            .iter()
            .filter(|e| !e.enabled(&settings))
            .map(|e| e.provider)
            .collect();
        let mut all = self.store.latest().map_err(|e| e.to_string())?;
        all.retain(|s| !hidden.contains(&s.provider.as_str()));
        Ok(all)
    }

    /// Set (or clear) the Copilot plan's monthly allowance in USD. The adapter restarts with it.
    pub fn set_copilot_allowance(&self, usd: Option<f64>) -> Result<(), String> {
        let cents = match usd {
            None => None,
            Some(v) if v.is_finite() && v > 0.0 && v <= 100_000.0 => {
                Some((v * 100.0).round() as u32)
            }
            Some(_) => return Err("Enter the allowance in dollars, for example 10 or 39.".into()),
        };
        self.settings
            .update(|s| s.copilot_allowance_cents = cents)
            .map_err(|e| format!("Could not save the allowance: {e}"))?;
        self.restart_polling();
        (self.changed)();
        Ok(())
    }

    /// The Copilot allowance in USD, if one is set.
    pub fn copilot_allowance(&self) -> Option<f64> {
        self.settings
            .get()
            .copilot_allowance_cents
            .map(|c| f64::from(c) / 100.0)
    }

    /// Switch a source on or off. Its adapter starts or stops at once, and the dashboard and
    /// paired phones follow.
    pub fn set_source_enabled(&self, adapter_id: &str, enabled: bool) -> Result<(), String> {
        let entry = catalog_entry(adapter_id).ok_or("Unknown source.")?;
        self.settings
            .update(|s| {
                s.sources.insert(entry.id.to_owned(), enabled);
            })
            .map_err(|e| format!("Could not save the setting: {e}"))?;
        if !enabled {
            lock(&self.runtime).remove(entry.id);
        }
        self.restart_polling();
        self.after_change();
        Ok(())
    }

    /// Chart data for one window.
    pub fn history(&self, snapshot_id: &str, range: &str) -> Result<HistorySeries, String> {
        let range = HistoryRange::parse(range).ok_or("Unknown history range.")?;
        let snapshot = self
            .store
            .get(snapshot_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("No readings for {snapshot_id} yet."))?;
        let since = OffsetDateTime::now_utc() - range.span();
        let points = self
            .store
            .history(snapshot_id, since)
            .map_err(|e| e.to_string())?;
        Ok(history_series(&snapshot, &points, range))
    }

    /// The Settings "Sources" list.
    pub fn adapters(&self) -> Vec<AdapterInfo> {
        let runtime = lock(&self.runtime).clone();
        let problems = lock(&self.problems).clone();
        let settings = self.settings.get();
        CATALOG
            .iter()
            .map(|entry| {
                let has_key = self.has_key(entry.id);
                let needs_key = entry.credentials != Credentials::None && !has_key;
                let enabled = entry.enabled(&settings);
                let state = runtime.get(entry.id);
                let (status, note) = if !enabled {
                    (
                        cooldown_core::Status::Unavailable,
                        Some("Switched off. It is not read and not shown.".to_owned()),
                    )
                } else if let Some(problem) = problems.get(entry.id) {
                    (cooldown_core::Status::Unavailable, Some(problem.clone()))
                } else if needs_key {
                    (
                        cooldown_core::Status::Unavailable,
                        Some("No key saved. Add one below to read this balance.".to_owned()),
                    )
                } else if let Some(state) = state {
                    (state.status, state.note.clone())
                } else {
                    (
                        cooldown_core::Status::Stale,
                        Some("Waiting for the first reading.".to_owned()),
                    )
                };
                AdapterInfo {
                    id: entry.id.to_owned(),
                    name: entry.name.to_owned(),
                    provider: entry.provider.to_owned(),
                    kind: entry.kind,
                    version: cooldown_adapters::ADAPTER_VERSION.to_owned(),
                    status,
                    note,
                    last_polled_at: state.map(|s| iso(s.last_polled_at)),
                    credentials: entry.credentials.as_str(),
                    has_key,
                    enabled,
                    hook_agent: (entry.id == cooldown_adapters::antigravity::ADAPTER_ID)
                        .then_some("antigravity"),
                    setup: adapters::setup_hint(entry.id),
                }
            })
            .collect()
    }

    // ------------------------------------------------------------------ keys

    /// Save an API key in the credential store (and, for xAI, the team id in settings). Blocking.
    pub fn save_key(
        &self,
        adapter_id: &str,
        key: &str,
        team_id: Option<&str>,
    ) -> Result<(), String> {
        let entry = catalog_entry(adapter_id).ok_or("Unknown source.")?;
        if entry.credentials == Credentials::None {
            return Err("This source does not use a key.".into());
        }
        let secret = SecretString::new(key);
        if !secret.is_valid_api_key() {
            return Err(
                "That does not look like an API key. Paste it again without spaces.".into(),
            );
        }
        if entry.credentials == Credentials::ManagementKey {
            let team = team_id.map(str::trim).filter(|t| !t.is_empty());
            if let Some(team) = team {
                if !cooldown_adapters::xai::is_valid_team_id(team) {
                    return Err("A team ID uses only letters, digits, '-' and '_'.".into());
                }
            }
            let team = team.map(str::to_owned);
            self.settings
                .update(|s| s.xai_team_id = team)
                .map_err(|e| format!("Could not save the team ID: {e}"))?;
        }
        self.secrets
            .set(&adapters::secret_name(entry.id), &secret)
            .map_err(|e| secrets::describe_error(&e))?;
        lock(&self.keys).insert(entry.id, true);
        self.restart_polling();
        (self.changed)();
        Ok(())
    }

    /// Delete an API key. Blocking.
    pub fn remove_key(&self, adapter_id: &str) -> Result<(), String> {
        let entry = catalog_entry(adapter_id).ok_or("Unknown source.")?;
        self.secrets
            .delete(&adapters::secret_name(entry.id))
            .map_err(|e| secrets::describe_error(&e))?;
        lock(&self.keys).insert(entry.id, false);
        lock(&self.runtime).remove(entry.id);
        // Without this the overview (and paired phones) keep the last balance forever.
        if let Err(e) = self.store.remove_account(entry.provider, adapters::ACCOUNT) {
            log::warn!("could not forget the readings of {}: {e}", entry.id);
        }
        self.restart_polling();
        self.after_change();
        Ok(())
    }

    // ------------------------------------------------------------------ alerts

    /// Every stored alert.
    pub fn alerts(&self) -> Result<Vec<AlertIntent>, String> {
        #[cfg(test)]
        self.store_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.store.alerts().map_err(|e| e.to_string())
    }

    /// Store (replace) an alert the user armed or switched.
    pub fn set_alert(&self, intent: AlertIntent) -> Result<(), String> {
        if cooldown_core::parse_alert_id(&intent.id).is_none() {
            return Err("That is not a reset alert id.".into());
        }
        self.store.put_alert(&intent).map_err(|e| e.to_string())?;
        self.after_change();
        Ok(())
    }

    /// Delete an alert.
    pub fn remove_alert(&self, id: &str) -> Result<(), String> {
        self.store.remove_alert(id).map_err(|e| e.to_string())?;
        self.after_change();
        Ok(())
    }

    /// Tests only: read the store and run [`Self::reconcile_stored`] on it, as the alert timer does.
    /// Returns true when anything changed.
    #[cfg(test)]
    pub fn reconcile_alerts(&self, now: OffsetDateTime) -> bool {
        match self.load() {
            Ok(mut stored) => self.reconcile_stored(&mut stored, now),
            Err(_) => false,
        }
    }

    /// Apply the reconciler's decisions to the stored alerts and ring the ones that are due, on a
    /// state that was already read. Every write that goes through is mirrored into
    /// `stored.alerts`, so afterwards it is what the store holds and nobody has to read it back.
    /// Returns true when anything changed.
    fn reconcile_stored(&self, stored: &mut Stored, now: OffsetDateTime) -> bool {
        let actions = lock(&self.reconciler).reconcile(&stored.alerts, &stored.latest, now);
        // The actions were decided from the alerts as read, so each one looks its alert up there.
        let mut after = stored.alerts.clone();
        let mut changed = false;
        for action in &actions {
            changed |= self.apply(action, stored, &mut after, now);
        }
        stored.alerts = after;
        changed
    }

    fn apply(
        &self,
        action: &AlertAction,
        stored: &Stored,
        after: &mut Vec<AlertIntent>,
        now: OffsetDateTime,
    ) -> bool {
        let intent = stored.alerts.iter().find(|i| i.id == action.id());
        let result = match action {
            AlertAction::Schedule { id, fire_at } => self.put_mirrored(
                AlertIntent {
                    id: id.clone(),
                    fire_at: *fire_at,
                    enabled: true,
                },
                after,
            ),
            AlertAction::Reschedule { id, to, .. } => self.put_mirrored(
                AlertIntent {
                    id: id.clone(),
                    fire_at: *to,
                    enabled: intent.is_none_or(|i| i.enabled),
                },
                after,
            ),
            AlertAction::Cancel {
                reason: CancelReason::Disabled,
                ..
            } => return false,
            AlertAction::Cancel { id, .. } => self.remove_mirrored(id, after),
            AlertAction::Fire { id, reason, .. } => {
                let Some(intent) = intent else { return false };
                let read = stored.latest.iter().find(|s| s.alert_id() == *id);
                // The last check before ringing. A veto keeps the alert; later reads decide.
                let due = matches!(reason, cooldown_core::FireReason::Due);
                if due && !should_fire(intent, read, now) {
                    return false;
                }
                let (title, body) = notification_text(id, read);
                (self.notify)(&title, &body);
                self.remove_mirrored(id, after)
            }
        };
        if let Err(e) = result {
            log::warn!("could not update alert {}: {e}", action.id());
        }
        true
    }

    /// Store an alert and, once that went through, mirror it into `after` the way the store gives
    /// it back: whole milliseconds, UTC, one per id, ordered by id.
    fn put_mirrored(
        &self,
        alert: AlertIntent,
        after: &mut Vec<AlertIntent>,
    ) -> Result<(), StoreError> {
        self.store.put_alert(&alert)?;
        let millis = alert.fire_at.unix_timestamp_nanos() / 1_000_000;
        let fire_at =
            OffsetDateTime::from_unix_timestamp_nanos(millis * 1_000_000).unwrap_or(alert.fire_at);
        after.retain(|a| a.id != alert.id);
        after.push(AlertIntent { fire_at, ..alert });
        after.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(())
    }

    /// Delete an alert and, once that went through, drop it from `after`.
    fn remove_mirrored(&self, id: &str, after: &mut Vec<AlertIntent>) -> Result<(), StoreError> {
        self.store.remove_alert(id)?;
        after.retain(|a| a.id != id);
        Ok(())
    }

    /// The tray app's own timer: re-check alerts every `ALERT_TICK`.
    pub async fn run_alert_timer(self: Arc<Self>) {
        let mut tick = tokio::time::interval(ALERT_TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if let Ok(mut stored) = self.load() {
                if self.reconcile_stored(&mut stored, OffsetDateTime::now_utc()) {
                    self.push_to_phones(stored);
                    (self.changed)();
                }
            }
        }
    }

    // ------------------------------------------------------------------ phones

    /// What phones receive: every window and every alert. Snapshots carry no secrets (adapters
    /// never put one in a note) and the sync crate sanitizes again before sending.
    pub fn sync_payload(&self) -> Result<SyncPayload, String> {
        Ok(self.load()?.into_payload())
    }

    /// Push the current state to paired phones, if the server runs.
    pub fn sync_phones(&self) {
        if self.phone.server().is_none() {
            return;
        }
        let Ok(payload) = self.sync_payload() else {
            return;
        };
        self.push_payload(&payload);
    }

    /// [`Self::sync_phones`] for a state that was already read. Without a server nothing is built.
    fn push_to_phones(&self, stored: Stored) {
        if self.phone.server().is_some() {
            self.push_payload(&stored.into_payload());
        }
    }

    fn push_payload(&self, payload: &SyncPayload) {
        if let Some(stamp) = self.phone.push(payload) {
            let _ = self.settings.update(|s| {
                s.sync_resume = Some(SyncResume {
                    epoch: stamp.epoch,
                    rev: stamp.rev,
                })
            });
        }
    }

    /// Start the phone server (if needed) and run its event loops.
    pub async fn start_phone_link(self: &Arc<Self>) -> Result<(), String> {
        let resume = self.settings.get().sync_resume;
        let (_server, started) = self.phone.ensure_server(resume).await?;
        if let Some(started) = started {
            self.spawn_phone_loops(started);
            self.sync_phones();
        }
        Ok(())
    }

    /// Every `NETWORK_CHECK`, look after the phone link: while the server runs, re-check the
    /// networks (a network that changed or turned Public restarts the server on the allowed
    /// addresses, or stops it); while it is down but phones are paired, try to start it again.
    /// The server fails closed when no network is trusted (a Wi-Fi switch, waking from sleep), and
    /// this is what brings it back once one is. It also retries a start that failed at launch.
    pub async fn run_network_watch(self: Arc<Self>) {
        let mut tick = tokio::time::interval(NETWORK_CHECK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The last failure already logged, so a café network does not fill the log every 2 minutes.
        let mut logged: Option<String> = None;
        loop {
            tick.tick().await;
            match self.network_watch_tick().await {
                WatchOutcome::Idle => logged = None,
                WatchOutcome::Listening { restored } => {
                    logged = None;
                    if restored {
                        log::info!("listening for phones again");
                        (self.changed)();
                    }
                }
                WatchOutcome::Down(reason) => {
                    if logged.as_deref() != Some(reason.as_str()) {
                        log::warn!("not listening for phones: {reason}");
                        (self.changed)();
                        logged = Some(reason);
                    }
                }
            }
        }
    }

    /// One pass of [`run_network_watch`](Self::run_network_watch).
    pub async fn network_watch_tick(self: &Arc<Self>) -> WatchOutcome {
        let action = watch_action(self.phone.server().is_some(), self.phone.has_peers());
        if action == WatchAction::Idle {
            return WatchOutcome::Idle;
        }
        match self.start_phone_link().await {
            Ok(()) => WatchOutcome::Listening {
                restored: action == WatchAction::Start,
            },
            Err(reason) => WatchOutcome::Down(reason),
        }
    }

    /// A phone flipped a bell. The PC is authoritative: switch an existing alert, or arm a new one
    /// at the window's known future reset; anything else just re-syncs so the phone shows the truth.
    pub fn apply_phone_request(&self, id: &str, enabled: bool, now: OffsetDateTime) {
        let existing = self
            .alerts()
            .ok()
            .and_then(|all| all.into_iter().find(|a| a.id == id));
        let next = match existing {
            Some(mut alert) => {
                alert.enabled = enabled;
                Some(alert)
            }
            None if enabled => self
                .snapshots()
                .ok()
                .and_then(|all| all.into_iter().find(|s| s.alert_id() == id))
                .and_then(|s| s.resets_at)
                .filter(|reset| *reset > now)
                .map(|fire_at| AlertIntent {
                    id: id.to_owned(),
                    fire_at,
                    enabled: true,
                }),
            None => None,
        };
        match next {
            Some(alert) => {
                let _ = self.set_alert(alert);
            }
            None => self.sync_phones(),
        }
    }

    fn spawn_phone_loops(self: &Arc<Self>, started: Started) {
        let Started {
            mut events,
            alert_requests,
        } = started;
        let backend = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        if backend.phone.on_event(&event) {
                            backend.sync_phones();
                        }
                        // Pairing and revoking can create or rotate the beacon key.
                        if matches!(
                            event,
                            cooldown_sync::ServerEvent::DevicePaired { .. }
                                | cooldown_sync::ServerEvent::DeviceRevoked { .. }
                        ) {
                            backend.phone.reannounce().await;
                        }
                        (backend.changed)();
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        if let Some(mut requests) = alert_requests {
            let backend = Arc::clone(self);
            tokio::spawn(async move {
                while let Some(request) = requests.recv().await {
                    backend.apply_phone_request(
                        &request.id,
                        request.enabled,
                        OffsetDateTime::now_utc(),
                    );
                }
            });
        }
    }
}

/// The reset-alert tuning for these settings: "limit is back" arms a bell for every used-up window.
pub fn reconcile_config(n: &Notifications) -> ReconcileConfig {
    ReconcileConfig {
        auto_arm_at_percent: n.limit_back.then_some(99.0),
        ..ReconcileConfig::default()
    }
}

/// The usage-notification tuning for these settings.
pub fn usage_config(n: &Notifications) -> cooldown_core::UsageConfig {
    cooldown_core::UsageConfig {
        low_at_percent: n.low_at_percent.map(f64::from),
        ran_out: n.ran_out,
        ..cooldown_core::UsageConfig::default()
    }
}

/// The notification for alert `id`, worded from its latest reading when there is one.
pub fn notification_text(id: &str, read: Option<&Snapshot>) -> (String, String) {
    let parsed = cooldown_core::parse_alert_id(id);
    let (provider, window) = match (read, &parsed) {
        (Some(s), _) => (s.provider.clone(), Some(s.window)),
        (None, Some(p)) => (p.provider.clone(), Some(p.window)),
        (None, None) => (String::new(), None),
    };
    let title = match window {
        Some(window) if !provider.is_empty() => reset_title(&provider, window),
        _ => "A limit is back".to_owned(),
    };
    (
        title,
        "Your usage limit has reset. You can work again.".to_owned(),
    )
}

/// The phone link's network watch, with fake networks and a fake identity store (never the real
/// ones).
#[cfg(test)]
mod network_watch_tests {
    use super::*;
    use crate::views::PairingState;
    use cooldown_adapters::InMemorySecretStore;
    use cooldown_sync::{
        load_or_create_pc_identity, FileTrustStore, IdentityError, IdentityStore,
        MemoryIdentityStore, PeerRecord, StaticKeypair, TrustStore,
    };
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    /// One identity store shared by every start attempt, like the real credential store.
    struct SharedVault(Arc<MemoryIdentityStore>);

    impl IdentityStore for SharedVault {
        fn load(&self) -> Result<Option<StaticKeypair>, IdentityError> {
            self.0.load()
        }
        fn store(&self, keypair: &StaticKeypair) -> Result<(), IdentityError> {
            self.0.store(keypair)
        }
        fn delete(&self) -> Result<(), IdentityError> {
            self.0.delete()
        }
    }

    struct Rig {
        _dir: tempfile::TempDir,
        backend: Arc<Backend>,
        /// The addresses the fake machine says are on trusted networks.
        networks: Arc<Mutex<Vec<IpAddr>>>,
        /// How many times the machine was asked.
        asked: Arc<AtomicUsize>,
    }

    fn rig(paired: bool) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let vault = Arc::new(MemoryIdentityStore::new());
        if paired {
            // A PC identity that is already recorded, and one paired phone, as after a restart.
            let trust = FileTrustStore::open(dir.path().join("trust.json")).unwrap();
            load_or_create_pc_identity(&*vault, &trust).unwrap();
            let phone = StaticKeypair::generate().unwrap();
            trust
                .upsert(PeerRecord::new(
                    *phone.public(),
                    "Pixel".into(),
                    "android".into(),
                    1,
                    0,
                    1,
                ))
                .unwrap();
        }
        let networks = Arc::new(Mutex::new(Vec::new()));
        let asked = Arc::new(AtomicUsize::new(0));
        let (n, a) = (networks.clone(), asked.clone());
        let host = HostEnv::new(
            move || {
                a.fetch_add(1, Ordering::SeqCst);
                n.lock().unwrap().clone()
            },
            move || Ok(Box::new(SharedVault(vault.clone())) as Box<dyn IdentityStore>),
        );
        let backend = Backend::open_with_env(
            dir.path(),
            Arc::new(InMemorySecretStore::new()),
            Box::new(|_, _| {}),
            Box::new(|| {}),
            host,
        )
        .unwrap();
        Rig {
            _dir: dir,
            backend,
            networks,
            asked,
        }
    }

    fn set_networks(rig: &Rig, ips: &[IpAddr]) {
        *rig.networks.lock().unwrap() = ips.to_vec();
    }

    #[test]
    fn the_watch_decides_from_two_facts() {
        use WatchAction::{Idle, Recheck, Start};
        assert_eq!(watch_action(true, true), Recheck);
        assert_eq!(
            watch_action(true, false),
            Recheck,
            "a pairing-only server is still re-checked"
        );
        assert_eq!(
            watch_action(false, true),
            Start,
            "failed closed: bring it back"
        );
        assert_eq!(
            watch_action(false, false),
            Idle,
            "nobody to serve: open no port"
        );
    }

    /// M2: after failing closed (no trusted network), the next tick with a trusted network starts
    /// the server again; before the fix every later tick skipped it forever.
    #[tokio::test]
    async fn the_phone_link_recovers_after_failing_closed() {
        let rig = rig(true);
        let backend = &rig.backend;
        assert!(backend.phone().has_peers());

        // Startup (or a wake) with no trusted network: nothing listens, and the reason is kept.
        let first = backend.network_watch_tick().await;
        assert!(matches!(first, WatchOutcome::Down(_)), "{first:?}");
        assert!(backend.phone().server().is_none());

        // Still nothing, still not listening, however often it is asked.
        let again = backend.network_watch_tick().await;
        assert!(matches!(again, WatchOutcome::Down(_)), "{again:?}");
        assert!(backend.phone().server().is_none());

        // Home Wi-Fi appears: the next tick starts it, on exactly that address.
        set_networks(&rig, &[LOOPBACK]);
        let up = backend.network_watch_tick().await;
        assert_eq!(up, WatchOutcome::Listening { restored: true });
        let server = backend.phone().server().expect("listening again");
        let bound: Vec<IpAddr> = server.local_addrs().iter().map(|a| a.ip()).collect();
        assert_eq!(bound, vec![LOOPBACK]);

        // Unchanged networks: a plain re-check, no restart.
        let steady = backend.network_watch_tick().await;
        assert_eq!(steady, WatchOutcome::Listening { restored: false });
        assert!(Arc::ptr_eq(&server, &backend.phone().server().unwrap()));

        // A Wi-Fi switch to an untrusted network shuts it down (fail closed)...
        set_networks(&rig, &[]);
        let down = backend.network_watch_tick().await;
        assert!(matches!(down, WatchOutcome::Down(_)), "{down:?}");
        assert!(
            backend.phone().server().is_none(),
            "never listens untrusted"
        );

        // ...and the way back works the same as the first time.
        set_networks(&rig, &[LOOPBACK]);
        let back = backend.network_watch_tick().await;
        assert_eq!(back, WatchOutcome::Listening { restored: true });
        assert!(backend.phone().server().is_some());
        backend.phone().shutdown();
    }

    /// The typed code on the Pairing screen is the QR text itself (so the phone's "Paste code
    /// instead" accepts it), and a payload never prints its secret.
    #[tokio::test]
    async fn the_typed_pairing_code_is_the_qr_text() {
        let rig = rig(false);
        set_networks(&rig, &[LOOPBACK]);
        rig.backend.start_phone_link().await.unwrap();
        let server = rig.backend.phone().server().expect("listening");

        let payload = rig.backend.phone().open_pairing(&server).unwrap();
        assert!(payload.uri.starts_with("CD1:"), "{}", payload.uri.len());
        assert_eq!(payload.manual_code, payload.uri);
        cooldown_sync::QrPayload::decode(&payload.manual_code)
            .expect("the typed code is a valid pairing payload");

        // It reaches the page under the name the UI reads.
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["manualCode"], json["uri"]);

        // Each window has its own secret, and neither shows up when the payload is printed.
        let second = rig.backend.phone().open_pairing(&server).unwrap();
        assert_ne!(second.manual_code, payload.manual_code);
        let printed = format!("{payload:?} {second:?}");
        assert!(!printed.contains(&payload.manual_code[4..]));
        assert!(!printed.contains(&second.manual_code[4..]));

        // Closing the window (the pairing screen's cancel) is what ends the code's life.
        rig.backend.phone().cancel_pairing();
        assert_eq!(rig.backend.phone().state(), PairingState::Idle);
        rig.backend.phone().shutdown();
    }

    /// A fresh install (nobody paired) still opens no port and does not even ask about networks.
    #[tokio::test]
    async fn without_paired_phones_the_watch_starts_nothing() {
        let rig = rig(false);
        set_networks(&rig, &[LOOPBACK]);
        assert!(!rig.backend.phone().has_peers());
        assert_eq!(rig.backend.network_watch_tick().await, WatchOutcome::Idle);
        assert!(rig.backend.phone().server().is_none());
        assert_eq!(rig.asked.load(Ordering::SeqCst), 0);
    }
}

/// Saving keys when the credential store could not be opened (L2).
#[cfg(test)]
mod key_store_tests {
    use super::*;
    use crate::secrets::{UnavailableSecretStore, WINDOWS_STORE_UNAVAILABLE};

    #[test]
    fn saving_a_key_without_a_credential_store_fails_with_a_clear_sentence() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Backend::open(
            dir.path(),
            Arc::new(UnavailableSecretStore::new(WINDOWS_STORE_UNAVAILABLE)),
            Box::new(|_, _| {}),
            Box::new(|| {}),
        )
        .unwrap();
        let error = backend
            .save_key("openrouter", "sk-or-v1-abc", None)
            .unwrap_err();
        assert_eq!(
            error,
            "Cooldown could not open Windows Credential Manager, so it can't save keys on this PC."
        );
        assert!(!backend.has_key("openrouter"), "nothing was saved");
        assert_eq!(backend.remove_key("openrouter").unwrap_err(), error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cooldown_adapters::InMemorySecretStore;
    use cooldown_core::{SnapshotSource, SourceKind, Status, Unit, WindowKind};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Harness {
        _dir: tempfile::TempDir,
        backend: Arc<Backend>,
        notified: Arc<Mutex<Vec<(String, String)>>>,
        changes: Arc<AtomicUsize>,
    }

    fn harness() -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let notified = Arc::new(Mutex::new(Vec::new()));
        let changes = Arc::new(AtomicUsize::new(0));
        let n = notified.clone();
        let c = changes.clone();
        let backend = Backend::open(
            dir.path(),
            Arc::new(InMemorySecretStore::new()),
            Box::new(move |t, b| n.lock().unwrap().push((t.to_owned(), b.to_owned()))),
            Box::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .unwrap();
        Harness {
            _dir: dir,
            backend,
            notified,
            changes,
        }
    }

    fn reading(percent: f64, resets_in: time::Duration, observed_at: OffsetDateTime) -> Snapshot {
        let mut s = Snapshot::new(
            "claude",
            "default",
            WindowKind::Session5h,
            Unit::Percent,
            SnapshotSource {
                kind: SourceKind::Official,
                adapter: "claude-statusline".into(),
                adapter_version: "0.1.0".into(),
            },
            observed_at,
        );
        s.percent_used = Some(percent);
        s.resets_at = Some(observed_at + resets_in);
        s
    }

    fn poll(snapshots: Vec<Snapshot>, at: OffsetDateTime) -> PollEvent {
        PollEvent {
            slot: 0,
            adapter: "claude-statusline",
            adapter_version: "0.1.0",
            polled_at: at,
            snapshots,
            failed: false,
            consecutive_failures: 0,
            next_poll_in: Duration::from_secs(30),
        }
    }

    #[test]
    fn polls_are_stored_and_summarised() {
        let h = harness();
        let now = OffsetDateTime::now_utc();
        h.backend.on_poll(poll(
            vec![reading(40.0, time::Duration::hours(2), now)],
            now,
        ));
        let snapshots = h.backend.snapshots().unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].percent_used, Some(40.0));
        assert!(h.changes.load(Ordering::SeqCst) >= 1, "the UI is told");

        let adapters = h.backend.adapters();
        let claude = adapters
            .iter()
            .find(|a| a.id == "claude-statusline")
            .unwrap();
        assert_eq!(claude.status, Status::Ok);
        assert!(claude.last_polled_at.is_some());
        let codex = adapters.iter().find(|a| a.id == "codex").unwrap();
        assert_eq!(codex.status, Status::Stale, "not polled yet");
        let xai = adapters.iter().find(|a| a.id == "xai").unwrap();
        assert!(!xai.has_key);
        assert_eq!(xai.status, Status::Unavailable);

        let series = h
            .backend
            .history("claude:default:session_5h", "24h")
            .unwrap();
        assert_eq!(series.points.len(), 1);
        assert!(h
            .backend
            .history("claude:default:session_5h", "1y")
            .is_err());
        assert!(h.backend.history("nope:default:weekly", "24h").is_err());
    }

    #[test]
    fn keys_are_validated_saved_and_removed() {
        let h = harness();
        assert!(
            h.backend.save_key("codex", "abc", None).is_err(),
            "no key needed"
        );
        assert!(h.backend.save_key("nope", "abc", None).is_err());
        assert!(h.backend.save_key("openrouter", "has space", None).is_err());
        h.backend
            .save_key("openrouter", " sk-or-v1-abc\n", None)
            .unwrap();
        assert!(h.backend.has_key("openrouter"));
        assert_eq!(
            h.backend
                .secrets
                .get("openrouter:default")
                .unwrap()
                .unwrap()
                .expose(),
            "sk-or-v1-abc"
        );
        assert!(h
            .backend
            .save_key("xai", "xai-key", Some("bad team!"))
            .is_err());
        h.backend
            .save_key("xai", "xai-key", Some("team_1"))
            .unwrap();
        assert_eq!(
            h.backend.settings.get().xai_team_id.as_deref(),
            Some("team_1")
        );
        // The team ID is optional: a team key alone works, and saving without one clears the old.
        h.backend.save_key("xai", "xai-key-2", None).unwrap();
        assert_eq!(h.backend.settings.get().xai_team_id, None);
        h.backend.save_key("xai", "xai-key-3", Some("  ")).unwrap();
        assert_eq!(h.backend.settings.get().xai_team_id, None);

        h.backend.remove_key("openrouter").unwrap();
        assert!(!h.backend.has_key("openrouter"));
        assert!(h
            .backend
            .secrets
            .get("openrouter:default")
            .unwrap()
            .is_none());
    }

    #[test]
    fn removing_a_key_forgets_its_balance_and_ignores_late_polls() {
        let h = harness();
        h.backend
            .save_key("openrouter", "sk-or-v1-abc", None)
            .unwrap();
        let now = OffsetDateTime::now_utc();
        let mut balance = Snapshot::new(
            "openrouter",
            "default",
            WindowKind::Balance,
            Unit::Usd,
            SnapshotSource {
                kind: SourceKind::Official,
                adapter: "openrouter".into(),
                adapter_version: "0.1.0".into(),
            },
            now,
        );
        balance.remaining = Some(12.5);
        let event = PollEvent {
            adapter: "openrouter",
            ..poll(vec![balance], now)
        };
        h.backend.on_poll(event.clone());
        h.backend.on_poll(poll(
            vec![reading(40.0, time::Duration::hours(2), now)],
            now,
        ));
        assert_eq!(h.backend.snapshots().unwrap().len(), 2);

        h.backend.remove_key("openrouter").unwrap();
        let left = h.backend.snapshots().unwrap();
        assert_eq!(left.len(), 1, "only the Claude window is left");
        assert_eq!(left[0].provider, "claude");

        h.backend.on_poll(event);
        assert_eq!(h.backend.snapshots().unwrap().len(), 1, "late poll ignored");
    }

    #[test]
    fn keys_found_at_startup_are_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Backend::open(
            dir.path(),
            Arc::new(InMemorySecretStore::with("deepseek:default", "sk-1")),
            Box::new(|_, _| {}),
            Box::new(|| {}),
        )
        .unwrap();
        assert!(backend.has_key("deepseek"));
        assert!(!backend.has_key("openrouter"));
    }

    #[test]
    fn a_due_alert_rings_once_and_is_removed() {
        let h = harness();
        // Only the reset bell is under test here, not the "ran out" notification.
        h.backend
            .set_notifications(Notifications {
                ran_out: false,
                low_at_percent: None,
                ..Notifications::default()
            })
            .unwrap();
        let now = OffsetDateTime::now_utc();
        let observed = now - time::Duration::minutes(1);
        h.backend.on_poll(poll(
            vec![reading(100.0, time::Duration::minutes(10), observed)],
            observed,
        ));
        let fire_at = observed + time::Duration::minutes(10);
        h.backend
            .set_alert(AlertIntent {
                id: "reset:claude:default:session_5h".into(),
                fire_at,
                enabled: true,
            })
            .unwrap();
        assert!(h.notified.lock().unwrap().is_empty(), "not yet");

        let later = fire_at + time::Duration::seconds(5);
        assert!(h.backend.reconcile_alerts(later));
        let notified = h.notified.lock().unwrap().clone();
        assert_eq!(notified.len(), 1);
        assert_eq!(notified[0].0, "Claude 5-hour limit is back");
        assert!(
            h.backend.alerts().unwrap().is_empty(),
            "removed after firing"
        );

        assert!(!h
            .backend
            .reconcile_alerts(later + time::Duration::seconds(15)));
        assert_eq!(h.notified.lock().unwrap().len(), 1, "never twice");
    }

    #[test]
    fn a_disabled_alert_never_rings() {
        let h = harness();
        let now = OffsetDateTime::now_utc();
        let fire_at = now - time::Duration::seconds(5);
        h.backend
            .set_alert(AlertIntent {
                id: "reset:claude:default:session_5h".into(),
                fire_at,
                enabled: false,
            })
            .unwrap();
        h.backend.reconcile_alerts(now);
        assert!(h.notified.lock().unwrap().is_empty());
        assert_eq!(
            h.backend.alerts().unwrap().len(),
            1,
            "the user's switch stays"
        );
    }

    #[test]
    fn a_phone_can_arm_switch_and_not_invent_bells() {
        let h = harness();
        let now = OffsetDateTime::now_utc();
        h.backend.on_poll(poll(
            vec![reading(90.0, time::Duration::hours(2), now)],
            now,
        ));
        let id = "reset:claude:default:session_5h";

        h.backend.apply_phone_request(id, true, now);
        let alerts = h.backend.alerts().unwrap();
        assert_eq!(alerts.len(), 1, "armed at the window's reset");
        let reset = h.backend.snapshots().unwrap()[0].resets_at.unwrap();
        // The alerts table keeps milliseconds.
        let ms = |t: OffsetDateTime| t.unix_timestamp_nanos() / 1_000_000;
        assert_eq!(ms(alerts[0].fire_at), ms(reset));

        h.backend.apply_phone_request(id, false, now);
        assert!(
            !h.backend.alerts().unwrap()[0].enabled,
            "switched off, kept"
        );

        h.backend
            .apply_phone_request("reset:codex:default:weekly", true, now);
        assert_eq!(h.backend.alerts().unwrap().len(), 1, "no window, no bell");
        h.backend.remove_alert(id).unwrap();
        h.backend.apply_phone_request(id, false, now);
        assert!(
            h.backend.alerts().unwrap().is_empty(),
            "switching off nothing creates nothing"
        );
    }

    #[test]
    fn bad_alert_ids_are_refused() {
        let h = harness();
        let result = h.backend.set_alert(AlertIntent {
            id: "claude:default:session_5h".into(),
            fire_at: OffsetDateTime::now_utc(),
            enabled: true,
        });
        assert!(result.is_err());
        h.backend
            .remove_alert("reset:claude:default:weekly")
            .unwrap();
    }

    #[test]
    fn a_source_switched_off_is_hidden_and_comes_back_when_switched_on() {
        let h = harness();
        let at = OffsetDateTime::now_utc() - time::Duration::seconds(30);
        h.backend
            .on_poll(poll(vec![reading(40.0, time::Duration::hours(2), at)], at));
        assert_eq!(h.backend.snapshots().unwrap().len(), 1);
        let id = cooldown_adapters::claude::ADAPTER_ID;

        h.backend.set_source_enabled(id, false).unwrap();
        assert!(h.backend.snapshots().unwrap().is_empty(), "hidden");
        assert!(
            h.backend.sync_payload().unwrap().snapshots.is_empty(),
            "not sent to phones"
        );
        let info = h
            .backend
            .adapters()
            .into_iter()
            .find(|a| a.id == id)
            .unwrap();
        assert!(!info.enabled);
        let (built, _) = adapters::build(
            &h.backend.settings.get(),
            h.backend.secrets.clone(),
            &|_| true,
        );
        assert!(built.iter().all(|a| a.id() != id), "not polled");

        h.backend.set_source_enabled(id, true).unwrap();
        assert_eq!(
            h.backend.snapshots().unwrap().len(),
            1,
            "the last reading is back"
        );
        assert!(h.backend.set_source_enabled("nope", true).is_err());
    }

    #[test]
    fn running_low_and_ran_out_each_ring_once_per_cycle() {
        let h = harness();
        let at = OffsetDateTime::now_utc() - time::Duration::seconds(30);
        let reset = time::Duration::hours(2);
        h.backend.on_poll(poll(vec![reading(50.0, reset, at)], at));
        assert!(h.notified.lock().unwrap().is_empty());
        h.backend.on_poll(poll(
            vec![reading(91.0, reset, at + time::Duration::seconds(1))],
            at,
        ));
        h.backend.on_poll(poll(
            vec![reading(95.0, reset, at + time::Duration::seconds(2))],
            at,
        ));
        h.backend.on_poll(poll(
            vec![reading(100.0, reset, at + time::Duration::seconds(3))],
            at,
        ));
        h.backend.on_poll(poll(
            vec![reading(100.0, reset, at + time::Duration::seconds(4))],
            at,
        ));
        let titles: Vec<String> = h
            .notified
            .lock()
            .unwrap()
            .iter()
            .map(|n| n.0.clone())
            .collect();
        assert_eq!(
            titles,
            [
                "Claude 5-hour limit is at 91%",
                "Claude 5-hour limit ran out"
            ]
        );
        assert_eq!(
            h.backend.settings.get().usage_notified.len(),
            2,
            "kept across restarts"
        );
    }

    #[test]
    fn limit_back_arms_a_bell_by_default_and_can_be_switched_off() {
        let h = harness();
        let at = OffsetDateTime::now_utc() - time::Duration::seconds(30);
        h.backend
            .on_poll(poll(vec![reading(100.0, time::Duration::hours(2), at)], at));
        assert_eq!(
            h.backend.alerts().unwrap().len(),
            1,
            "armed without a button press"
        );

        let h = harness();
        h.backend
            .set_notifications(Notifications {
                limit_back: false,
                ran_out: false,
                low_at_percent: None,
            })
            .unwrap();
        h.backend
            .on_poll(poll(vec![reading(100.0, time::Duration::hours(2), at)], at));
        assert!(h.backend.alerts().unwrap().is_empty());
        assert!(h.notified.lock().unwrap().is_empty());
        assert!(h
            .backend
            .set_notifications(Notifications {
                low_at_percent: Some(100),
                ..Notifications::default()
            })
            .is_err());
    }

    #[test]
    fn notification_text_names_the_window() {
        let now = OffsetDateTime::now_utc();
        let read = reading(10.0, time::Duration::hours(1), now);
        assert_eq!(
            notification_text("reset:claude:default:session_5h", Some(&read)).0,
            "Claude 5-hour limit is back"
        );
        assert_eq!(
            notification_text("reset:codex:default:weekly", None).0,
            "Codex weekly limit is back"
        );
        assert_eq!(notification_text("garbage", None).0, "A limit is back");
        assert_eq!(read.alert_id(), "reset:claude:default:session_5h");
    }

    #[test]
    fn the_sync_payload_carries_everything() {
        let h = harness();
        let now = OffsetDateTime::now_utc();
        h.backend
            .on_poll(poll(vec![reading(5.0, time::Duration::hours(3), now)], now));
        let payload = h.backend.sync_payload().unwrap();
        assert_eq!(payload.snapshots.len(), 1);
        assert_eq!(payload.schema_version, SCHEMA_VERSION);
        h.backend.sync_phones(); // no server: a no-op, not a panic
    }

    fn store_reads(h: &Harness) -> usize {
        h.backend
            .store_reads
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// What phones would get, as bytes, without the generation time.
    fn payload_bytes(stored: &Stored) -> String {
        serde_json::to_string(&(&stored.alerts, &stored.latest)).unwrap()
    }

    #[test]
    fn a_change_reads_the_store_once() {
        let h = harness();
        // Ten seconds back, so the reads below are not stamped in the future.
        let now = OffsetDateTime::now_utc() - time::Duration::seconds(10);
        let id = "reset:claude:default:session_5h";
        h.backend.on_poll(poll(
            vec![reading(100.0, time::Duration::hours(2), now)],
            now,
        ));

        // One read of the alerts and one of the snapshots serve the reconciliation and the phones,
        // whether or not the reconciliation writes anything.
        let before = store_reads(&h);
        h.backend.after_change();
        assert_eq!(store_reads(&h) - before, 2, "nothing to do");

        let fire_at = now + time::Duration::hours(2);
        h.backend
            .set_alert(AlertIntent {
                id: id.into(),
                fire_at: fire_at + time::Duration::minutes(5),
                enabled: true,
            })
            .unwrap();
        // The alert sits five minutes after the window's reset. Two agreeing reads make the
        // reconciler move it (a write inside the poll), and still only two reads of the store.
        for n in 1..=2 {
            let at = now + time::Duration::seconds(n);
            let before = store_reads(&h);
            h.backend
                .on_poll(poll(vec![reading(100.0, fire_at - at, at)], at));
            assert_eq!(store_reads(&h) - before, 2, "poll {n}");
        }
        let moved = h.backend.alerts().unwrap();
        assert_eq!(moved.len(), 1);
        assert_ne!(
            moved[0].fire_at,
            fire_at + time::Duration::minutes(5),
            "the alert was moved while only those two reads happened"
        );
    }

    #[test]
    fn the_state_after_reconciling_is_what_the_store_holds() {
        let h = harness();
        *lock(&h.backend.reconciler) = Reconciler::new(ReconcileConfig {
            auto_arm_at_percent: Some(99.0),
            ..ReconcileConfig::default()
        });
        let observed = OffsetDateTime::now_utc() - time::Duration::minutes(1);
        let store = &h.backend.store;
        let same_as_store = |stored: &Stored| {
            let fresh = h.backend.load().unwrap();
            assert_eq!(payload_bytes(stored), payload_bytes(&fresh));
        };

        // Nothing stored, nothing to do.
        let mut stored = h.backend.load().unwrap();
        assert!(!h.backend.reconcile_stored(&mut stored, observed));
        same_as_store(&stored);

        // A full window with a reset time to the nanosecond: armed (Schedule), and the table keeps
        // milliseconds.
        let reset =
            observed + time::Duration::minutes(10) + time::Duration::nanoseconds(123_456_789);
        let mut first = reading(100.0, time::Duration::minutes(10), observed);
        first.resets_at = Some(reset);
        store.upsert(&first).unwrap();
        let mut stored = h.backend.load().unwrap();
        assert!(h.backend.reconcile_stored(&mut stored, observed));
        assert_eq!(stored.alerts.len(), 1);
        assert_ne!(stored.alerts[0].fire_at, reset, "cut to milliseconds");
        same_as_store(&stored);

        // The reset moves 30 minutes later, seen twice (Reschedule).
        let moved = reset + time::Duration::minutes(30);
        for n in 1..=2 {
            let at = observed + time::Duration::seconds(n);
            let mut read = reading(100.0, time::Duration::minutes(10), at);
            read.resets_at = Some(moved);
            store.upsert(&read).unwrap();
            let mut stored = h.backend.load().unwrap();
            h.backend.reconcile_stored(&mut stored, at);
            same_as_store(&stored);
        }
        let mut stored = h.backend.load().unwrap();
        assert_eq!(stored.alerts.len(), 1);
        assert!(stored.alerts[0].fire_at > reset, "rescheduled");

        // It comes due and rings (Fire): the alert is gone from the state and from the store.
        let later = moved + time::Duration::seconds(5);
        assert!(h.backend.reconcile_stored(&mut stored, later));
        assert_eq!(h.notified.lock().unwrap().len(), 1);
        assert!(stored.alerts.is_empty());
        same_as_store(&stored);

        // The payload built from that state carries the same alerts and snapshots.
        let payload = stored.into_payload();
        assert_eq!(payload.snapshots.len(), 1);
        assert!(payload.alerts.is_empty());
    }

    #[tokio::test]
    async fn refresh_returns_after_a_poll_or_the_timeout() {
        let h = harness();
        let started = std::time::Instant::now();
        h.backend.refresh(Duration::from_millis(50)).await;
        assert!(started.elapsed() >= Duration::from_millis(50));
    }
}
