//! One sync round with the app closed.
//!
//! Android starts a WorkManager job (`SyncWorker.kt`) about every 15 minutes. The worker unwraps
//! the phone key with the Keystore (Kotlin) and calls [`run_headless`] through the JNI entry point
//! in `jni_entry.rs`; this module does the rest without a Tauri window, WebView or plugin: open the
//! phone's stores, reconnect to the paired PC, take the newest sync, keep it, disconnect, and hand
//! the worker what it must apply (the bells to ring and the widget file).
//!
//! Only one thing may own the phone's files at a time: two `PhoneClient`s on one trust store would
//! race on the reconnect timestamps. The [`Registry`] tells apart the three situations a round can
//! start in: nobody is running (open the phone from disk), the app is running in this very process
//! (use its phone, and leave its connection alone), or the app is just starting (come back later).

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use cooldown_sync::StaticKeypair;
use serde_json::{json, Value};
use zeroize::Zeroizing;

use crate::now_ms;
use crate::phone::{lock, Phone, PHONE_NAME};
use crate::widget::widget_json;

/// The whole round must be over by then, so a stuck network can never hold a worker (Android
/// gives a job ten minutes; a sync round needs seconds).
pub const ROUND_DEADLINE: Duration = Duration::from_secs(40);

/// After the PC answers, how long to wait for it to push a newer sync. It does so right behind its
/// HELLO when this phone is out of date and says nothing when it is not, so silence is the answer
/// "nothing new".
const SYNC_GRACE: Duration = Duration::from_secs(3);

/// How long the app waits for a background round to let go of the phone's files before it opens
/// them anyway (a round is over after [`ROUND_DEADLINE`], so this is never reached in practice).
pub const APP_WAITS_FOR_ROUND: Duration = Duration::from_secs(60);

/// How often a waiter looks again.
const POLL: Duration = Duration::from_millis(100);

// ------------------------------------------------------------------ who owns the phone

enum Owner {
    Free,
    /// A background round has the phone open.
    Headless,
    /// The app is running in this process. `None` while it is still starting.
    App(Option<Arc<Phone>>),
}

/// Which of the app and a background round has the phone's files open (see the module notes).
pub struct Registry {
    owner: Mutex<Owner>,
}

/// How a background round may proceed.
pub enum Begin<'a> {
    /// Nobody has the phone: open it from disk. Dropping this lets go.
    Own(Background<'a>),
    /// The app is running here: use its phone and leave its connection up.
    Live(Arc<Phone>),
    /// The app is starting right now. It syncs by itself; try again later.
    Busy,
}

/// A background round's hold on the phone's files. Lets go when dropped, also while unwinding.
pub struct Background<'a>(&'a Registry);

impl Drop for Background<'_> {
    fn drop(&mut self) {
        let mut owner = self.0.owner();
        if matches!(*owner, Owner::Headless) {
            *owner = Owner::Free;
        }
    }
}

impl Registry {
    pub const fn new() -> Self {
        Registry {
            owner: Mutex::new(Owner::Free),
        }
    }

    fn owner(&self) -> MutexGuard<'_, Owner> {
        lock(&self.owner)
    }

    /// The app is about to open the phone. Waits (up to `wait`) for a background round that has it,
    /// then claims it. Blocking: call it off the async workers.
    pub fn app_starting(&self, wait: Duration) {
        let mut waited = Duration::ZERO;
        loop {
            {
                let mut owner = self.owner();
                if !matches!(*owner, Owner::Headless) || waited >= wait {
                    *owner = Owner::App(None);
                    return;
                }
            }
            std::thread::sleep(POLL);
            waited += POLL;
        }
    }

    /// The app has its phone open.
    pub fn app_ready(&self, phone: Arc<Phone>) {
        *self.owner() = Owner::App(Some(phone));
    }

    /// The app could not open the phone: background rounds may use the files again.
    pub fn app_failed(&self) {
        let mut owner = self.owner();
        if matches!(*owner, Owner::App(None)) {
            *owner = Owner::Free;
        }
    }

    /// A background round wants to start.
    pub fn begin_background(&self) -> Begin<'_> {
        let mut owner = self.owner();
        match &*owner {
            Owner::Free => {
                *owner = Owner::Headless;
                Begin::Own(Background(self))
            }
            Owner::App(Some(phone)) => Begin::Live(phone.clone()),
            Owner::App(None) | Owner::Headless => Begin::Busy,
        }
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-wide registry.
pub static REGISTRY: Registry = Registry::new();

// ------------------------------------------------------------------ the round

/// How a round ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The PC answered. Its newest data (if it had any news) is kept.
    Synced,
    /// The PC could not be reached: away from home, PC off or asleep, app not running on it.
    Unreachable,
    /// No PC is paired (any more): nothing to ring, nothing to show.
    NoPc,
    /// The app was starting; nothing was done.
    Busy,
    /// Something broke (a damaged file, a deadline).
    Failed,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Synced => "synced",
            Status::Unreachable => "unreachable",
            Status::NoPc => "no_pc",
            Status::Busy => "busy",
            Status::Failed => "failed",
        }
    }
}

/// What the Kotlin worker gets back, as JSON ([`Report::to_json`]).
#[derive(Debug, Clone)]
pub struct Report {
    pub status: Status,
    /// A sentence for the log. Never holds a key or a pairing code.
    pub message: Option<String>,
    /// The bells that should be scheduled (see [`alarm_list`]); an empty list cancels them all.
    /// `None` when the phone knows nothing yet: the worker then leaves the scheduled alarms alone
    /// instead of cancelling them.
    pub alarms: Option<Vec<Value>>,
    /// The text of the widget file (see [`widget_json`]); empty means "no data, show the pairing
    /// hint". `None` as for `alarms`.
    pub widget: Option<String>,
}

impl Report {
    pub fn bare(status: Status, message: &str) -> Self {
        Report {
            status,
            message: Some(message.to_owned()),
            alarms: None,
            widget: None,
        }
    }

    /// The PC is gone (or never was): clear the alarms and the widget.
    fn nothing_paired() -> Self {
        Report {
            status: Status::NoPc,
            message: Some("No PC is paired.".to_owned()),
            alarms: Some(Vec::new()),
            widget: Some(String::new()),
        }
    }

    pub fn to_json(&self) -> String {
        json!({
            "status": self.status.as_str(),
            "message": self.message,
            "alarms": self.alarms,
            "widget": self.widget,
        })
        .to_string()
    }
}

/// Reconnect if needed, take what the PC has, and report what the device should now show and ring.
///
/// `keep_connection` is for a phone the app owns: a connection this round opened stays open (the
/// app's own loops would only open it again). Otherwise the round closes the connection it opened
/// before it returns. A connection that was already open is never touched.
pub async fn sync_round(phone: &Phone, keep_connection: bool, now_ms: i64) -> Report {
    if phone.pcs_by_recency().is_empty() {
        return Report::nothing_paired();
    }
    let mut syncs = phone.subscribe();
    syncs.borrow_and_update();

    let was_connected = phone.connected();
    let reached = was_connected || phone.connect_cached().await || phone.connect_discovered().await;
    if reached && !was_connected {
        let news = tokio::time::timeout(SYNC_GRACE, syncs.changed()).await;
        if matches!(news, Ok(Ok(()))) {
            if let Some(latest) = phone.latest() {
                phone.persist_latest(&latest);
            }
        }
        if !keep_connection {
            phone.disconnect().await;
        }
    }

    // The PC may have removed this phone while the round talked to it.
    if phone.pcs_by_recency().is_empty() {
        return Report::nothing_paired();
    }
    let (status, message) = if reached {
        (Status::Synced, None)
    } else {
        (
            Status::Unreachable,
            Some("Your PC was not found on this network.".to_owned()),
        )
    };
    match phone.latest() {
        Some(latest) => Report {
            status,
            message,
            alarms: Some(phone.alarms(now_ms)),
            widget: Some(widget_json(
                &latest.payload.snapshots,
                &latest.payload.alerts,
                Some(latest.received_at_ms),
            )),
        },
        None => Report {
            status,
            message,
            alarms: None,
            widget: None,
        },
    }
}

/// [`sync_round`] under the round deadline.
async fn bounded_round(phone: &Phone, keep_connection: bool) -> Report {
    match tokio::time::timeout(ROUND_DEADLINE, sync_round(phone, keep_connection, now_ms())).await {
        Ok(report) => report,
        Err(_) => {
            if !keep_connection {
                phone.disconnect().await;
            }
            Report::bare(Status::Failed, "The sync took too long and was stopped.")
        }
    }
}

/// The worker's entry point (through JNI): run one round for the phone whose files are in
/// `data_dir` and whose key the worker has just opened with the Keystore. Blocking.
pub fn run_headless(registry: &Registry, data_dir: &Path, key: Zeroizing<[u8; 32]>) -> Report {
    match registry.begin_background() {
        Begin::Busy => Report::bare(Status::Busy, "The app is starting; it syncs by itself."),
        Begin::Live(phone) => tauri::async_runtime::block_on(bounded_round(&phone, true)),
        Begin::Own(_hold) => {
            let phone = StaticKeypair::from_private(key)
                .map_err(|_| "The stored phone key is damaged.".to_owned())
                .and_then(|keypair| Phone::open(data_dir, PHONE_NAME, keypair));
            match phone {
                Ok(phone) => tauri::async_runtime::block_on(bounded_round(&phone, false)),
                Err(e) => Report::bare(Status::Failed, &e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phone::testkit::{open, pair, payload, start_pc};
    use cooldown_sync::PcServer;

    // ---- the registry

    fn phone_in(dir: &Path) -> Arc<Phone> {
        Arc::new(open(dir, &StaticKeypair::generate().unwrap()))
    }

    #[test]
    fn a_round_takes_the_free_phone_and_gives_it_back() {
        let registry = Registry::new();
        let first = registry.begin_background();
        assert!(matches!(first, Begin::Own(_)));
        assert!(
            matches!(registry.begin_background(), Begin::Busy),
            "two rounds never share the files"
        );
        drop(first);
        assert!(matches!(registry.begin_background(), Begin::Own(_)));
    }

    #[test]
    fn a_round_that_panics_still_lets_go() {
        let registry = Registry::new();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _hold = registry.begin_background();
            panic!("boom");
        }));
        assert!(outcome.is_err());
        assert!(matches!(registry.begin_background(), Begin::Own(_)));
    }

    #[test]
    fn a_starting_app_makes_rounds_wait_and_a_running_app_lends_its_phone() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new();
        registry.app_starting(Duration::ZERO);
        assert!(matches!(registry.begin_background(), Begin::Busy));

        registry.app_failed();
        assert!(matches!(registry.begin_background(), Begin::Own(_)));

        registry.app_starting(Duration::ZERO);
        let phone = phone_in(dir.path());
        registry.app_ready(phone.clone());
        match registry.begin_background() {
            Begin::Live(lent) => assert!(Arc::ptr_eq(&lent, &phone)),
            _ => panic!("the app's phone should be lent"),
        }
        // A failure report from a stale start never takes a ready app's phone away.
        registry.app_failed();
        assert!(matches!(registry.begin_background(), Begin::Live(_)));
    }

    #[test]
    fn the_app_waits_for_a_running_round() {
        let registry = Registry::new();
        let hold = match registry.begin_background() {
            Begin::Own(hold) => hold,
            _ => panic!("free"),
        };
        let started = std::time::Instant::now();
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                registry.app_starting(Duration::from_secs(10));
                started.elapsed()
            });
            std::thread::sleep(Duration::from_millis(300));
            drop(hold);
            let waited = waiter.join().unwrap();
            assert!(waited >= Duration::from_millis(250), "waited {waited:?}");
        });
        assert!(
            matches!(registry.begin_background(), Begin::Busy),
            "the app owns it now"
        );
    }

    #[test]
    fn the_app_stops_waiting_after_its_limit() {
        let registry = Registry::new();
        let _hold = registry.begin_background();
        registry.app_starting(Duration::from_millis(200));
        assert!(matches!(registry.begin_background(), Begin::Busy));
    }

    // ---- reports

    #[test]
    fn a_report_is_plain_json_for_the_worker() {
        let report = Report {
            status: Status::Synced,
            message: None,
            alarms: Some(vec![json!({ "id": "a", "atMs": 1 })]),
            widget: Some("{}".into()),
        };
        let value: Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(value["status"], "synced");
        assert!(value["message"].is_null());
        assert_eq!(value["alarms"][0]["id"], "a");
        assert_eq!(value["widget"], "{}");

        let bare: Value =
            serde_json::from_str(&Report::bare(Status::Failed, "broke").to_json()).unwrap();
        assert_eq!(bare["status"], "failed");
        assert!(
            bare["alarms"].is_null(),
            "unknown is not the same as no bells"
        );
        assert!(bare["widget"].is_null());
    }

    // ---- a round against a real PC server on loopback

    /// Pair, then drop the session so the round has to reconnect.
    async fn pair_and_disconnect(pc: &PcServer, phone: &Phone) {
        pair(pc, phone).await;
        phone.disconnect().await;
        assert!(!phone.connected());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_round_reconnects_keeps_the_news_and_disconnects() {
        let dir = tempfile::tempdir().unwrap();
        let key = StaticKeypair::generate().unwrap();
        let pc = start_pc().await;
        let phone = open(dir.path(), &key);
        pair_and_disconnect(&pc, &phone).await;
        pc.push_sync(&payload(40.0, time::Duration::hours(1)))
            .unwrap();
        assert!(
            phone.latest().is_none(),
            "nothing arrived while disconnected"
        );

        let report = sync_round(&phone, false, now_ms()).await;

        assert_eq!(report.status, Status::Synced);
        assert!(!phone.connected(), "the round hangs up");
        let alarms = report.alarms.expect("the bell");
        assert_eq!(alarms.len(), 1);
        assert_eq!(alarms[0]["title"], "Claude 5-hour limit is back");
        let widget: Value = serde_json::from_str(&report.widget.unwrap()).unwrap();
        assert_eq!(widget["snapshots"][0]["percentUsed"], 40.0);
        assert!(widget["syncedAt"].is_string());

        // The news is on disk: the app, or the next round, starts with it.
        let reopened = open(dir.path(), &key);
        assert_eq!(reopened.snapshots().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_round_with_nothing_new_still_succeeds_and_keeps_the_old_data() {
        let dir = tempfile::tempdir().unwrap();
        let pc = start_pc().await;
        let phone = phone_in(dir.path());
        pair_and_disconnect(&pc, &phone).await;
        pc.push_sync(&payload(10.0, time::Duration::hours(2)))
            .unwrap();
        assert_eq!(
            sync_round(&phone, false, now_ms()).await.status,
            Status::Synced
        );

        // The PC has nothing newer to push: the round waits out its grace and ends.
        let second = sync_round(&phone, false, now_ms()).await;
        assert_eq!(second.status, Status::Synced);
        assert!(!phone.connected());
        assert_eq!(second.alarms.unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_connection_the_app_already_holds_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let pc = start_pc().await;
        let phone = phone_in(dir.path());
        pair(&pc, &phone).await;
        pc.push_sync(&payload(30.0, time::Duration::hours(1)))
            .unwrap();
        crate::phone::testkit::wait_for_rev(&phone, 1).await;

        let report = sync_round(&phone, false, now_ms()).await;
        assert_eq!(report.status, Status::Synced);
        assert!(
            phone.connected(),
            "the round did not open it, so it does not close it"
        );
        assert_eq!(report.alarms.unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_apps_own_phone_stays_connected_after_a_round() {
        let dir = tempfile::tempdir().unwrap();
        let pc = start_pc().await;
        let phone = phone_in(dir.path());
        pair_and_disconnect(&pc, &phone).await;
        pc.push_sync(&payload(30.0, time::Duration::hours(1)))
            .unwrap();

        let report = sync_round(&phone, true, now_ms()).await;
        assert_eq!(report.status, Status::Synced);
        assert!(phone.connected(), "the app wants its session kept");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pc_that_cannot_be_reached_still_refreshes_from_the_stored_data() {
        let dir = tempfile::tempdir().unwrap();
        let pc = start_pc().await;
        let phone = phone_in(dir.path());
        pair_and_disconnect(&pc, &phone).await;
        pc.push_sync(&payload(70.0, time::Duration::hours(1)))
            .unwrap();
        assert_eq!(
            sync_round(&phone, false, now_ms()).await.status,
            Status::Synced
        );

        pc.shutdown();
        drop(pc);
        tokio::time::sleep(Duration::from_millis(300)).await;

        let report = sync_round(&phone, false, now_ms()).await;
        assert_eq!(report.status, Status::Unreachable);
        assert!(report.message.is_some());
        // Bells that are still in the future keep ringing, and the widget still has its numbers.
        assert_eq!(report.alarms.unwrap().len(), 1);
        assert!(report.widget.unwrap().contains("percentUsed"));

        // Two hours on, the bell is in the past: the empty list lets the worker clear it.
        let later = sync_round(&phone, false, now_ms() + 2 * 3_600_000).await;
        assert_eq!(later.alarms.unwrap().len(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unreachable_pc_with_no_data_leaves_the_alarms_alone() {
        let dir = tempfile::tempdir().unwrap();
        let pc = start_pc().await;
        let phone = phone_in(dir.path());
        pair_and_disconnect(&pc, &phone).await;
        pc.shutdown();
        drop(pc);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let report = sync_round(&phone, false, now_ms()).await;
        assert_eq!(report.status, Status::Unreachable);
        assert!(report.alarms.is_none());
        assert!(report.widget.is_none());
    }

    #[tokio::test]
    async fn a_phone_with_no_pc_clears_the_alarms_and_the_widget() {
        let dir = tempfile::tempdir().unwrap();
        let phone = phone_in(dir.path());
        let report = sync_round(&phone, false, now_ms()).await;
        assert_eq!(report.status, Status::NoPc);
        assert_eq!(report.alarms, Some(Vec::new()));
        assert_eq!(report.widget.as_deref(), Some(""));
    }

    // ---- the entry point the worker calls

    #[test]
    fn the_headless_entry_point_opens_the_files_runs_and_lets_go() {
        // Nothing paired in an empty directory: the round reports it, and the registry is free again.
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new();
        let report = run_headless(&registry, dir.path(), Zeroizing::new([7u8; 32]));
        assert_eq!(report.status, Status::NoPc);
        assert!(matches!(registry.begin_background(), Begin::Own(_)));
    }

    #[test]
    fn the_headless_entry_point_refuses_a_zero_key_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new();
        let report = run_headless(&registry, dir.path(), Zeroizing::new([0u8; 32]));
        assert_eq!(report.status, Status::Failed);
        assert!(matches!(registry.begin_background(), Begin::Own(_)));
    }

    #[test]
    fn the_headless_entry_point_steps_aside_for_a_starting_app() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new();
        registry.app_starting(Duration::ZERO);
        let report = run_headless(&registry, dir.path(), Zeroizing::new([7u8; 32]));
        assert_eq!(report.status, Status::Busy);
    }

    #[test]
    fn the_headless_entry_point_uses_the_apps_phone_when_it_runs() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new();
        registry.app_starting(Duration::ZERO);
        registry.app_ready(phone_in(dir.path()));
        // The key is not even looked at: the app's phone is used as it is.
        let report = run_headless(&registry, dir.path(), Zeroizing::new([0u8; 32]));
        assert_eq!(report.status, Status::NoPc);
    }
}
