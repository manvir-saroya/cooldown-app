//! The phone side of pairing and sync, on `cooldown_sync::PhoneClient`.
//!
//! The phone's static key is handed in by the caller: on Android it is wrapped with a
//! non-exportable Android Keystore key (see `identity.rs`). Everything else lives in app-private
//! files that are excluded from backup (`data_extraction_rules.xml`): `trust.json` (paired PCs),
//! `pc-addresses.json` (last addresses that worked) and `latest.json` (the newest sync, so the app
//! and widget show something before the first reconnect).

use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cooldown_core::{AlertIntent, Snapshot, SyncPayload};
use cooldown_sync::{
    ClientConfig, CloseReason, ConnState, Connection, DeviceId, FileTrustStore, PcLocator,
    PeerRecord, PendingPairing, PhoneClient, QrPayload, ReceivedSync, StaticKeypair,
    SystemSyncClock, TrustStore, UdpPcLocator,
};
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::sync::{broadcast, watch};

/// How long a discovery probe listens for answers.
const DISCOVERY_WINDOW: Duration = Duration::from_millis(2500);

/// The name this phone gives itself when pairing (the PC lists it).
pub const PHONE_NAME: &str = "Cooldown phone";

/// How long to wait for a closing connection to wind down.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// Lock a mutex and carry on if a thread panicked while holding it.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn iso_ms(ms: i64) -> Option<String> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

/// Write `bytes` to `path` through a sibling temp file and a rename. Failures are ignored: these
/// are caches, and the next write tries again.
fn write_atomic(path: &Path, bytes: &[u8]) {
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    if fs::write(&temp, bytes).is_ok() && fs::rename(&temp, path).is_err() {
        let _ = fs::remove_file(&temp);
    }
}

/// What the screen shows after a pairing starts.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairStart {
    /// The six-digit code; the PC shows the same one.
    pub code: String,
    pub pc_name: String,
}

/// A paired PC, as the phone UI lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedPc {
    pub id: String,
    pub name: String,
    pub paired_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
    pub connected: bool,
}

/// The newest sync as stored on disk (`ReceivedSync` itself is not serializable).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredSync {
    pc: String,
    epoch: u64,
    rev: u64,
    payload: SyncPayload,
    received_at_ms: i64,
}

/// This device's private IPv4 addresses (the interfaces to probe on).
pub fn lan_ipv4() -> Vec<Ipv4Addr> {
    let mut out: Vec<Ipv4Addr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            IpAddr::V4(v4) if v4.is_private() => Some(v4),
            _ => None,
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// A session with a PC is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ended {
    pub pc: DeviceId,
    pub reason: CloseReason,
}

/// A pairing in progress: the handshake is done, the user has to confirm on the PC.
struct Pending {
    pairing: PendingPairing,
    /// Where the PC said it is (from the QR). Remembered only once the PC confirms.
    addrs: Vec<SocketAddr>,
}

/// The phone.
pub struct Phone {
    client: PhoneClient,
    pending: tokio::sync::Mutex<Option<Pending>>,
    /// Shared with the task that reports when it ends (see [`Phone::subscribe_ended`]). Always
    /// closed explicitly: that task keeps the handle alive.
    connection: Mutex<Option<Arc<Connection>>>,
    ended: broadcast::Sender<Ended>,
    addrs_path: PathBuf,
    latest_path: PathBuf,
    /// "Running low" and "ran out" notices and their settings.
    notices: crate::notices::Notices,
    /// Look for the PC with a multicast probe when its last address fails. Off only in tests, which
    /// must not send packets onto the developer's network.
    discovery: bool,
}

impl Drop for Phone {
    fn drop(&mut self) {
        if let Some(connection) = lock(&self.connection).take() {
            connection.close();
        }
    }
}

impl Phone {
    /// Open the trust store and caches in `data_dir` with this phone's key.
    pub fn open(data_dir: &Path, name: &str, keypair: StaticKeypair) -> Result<Self, String> {
        fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
        let trust: Arc<dyn TrustStore> =
            Arc::new(FileTrustStore::open(data_dir.join("trust.json")).map_err(|e| e.to_string())?);
        let client = PhoneClient::new(
            ClientConfig::new(name, "android"),
            keypair,
            trust,
            Arc::new(SystemSyncClock::new()),
        )
        .map_err(|e| e.to_string())?;
        let phone = Phone {
            client,
            pending: tokio::sync::Mutex::new(None),
            connection: Mutex::new(None),
            ended: broadcast::channel(16).0,
            addrs_path: data_dir.join("pc-addresses.json"),
            latest_path: data_dir.join("latest.json"),
            notices: crate::notices::Notices::open(data_dir.join("notices.json")),
            discovery: true,
        };
        phone.restore_latest();
        Ok(phone)
    }

    /// The "running low" and "ran out" notices and their settings.
    pub fn notices(&self) -> &crate::notices::Notices {
        &self.notices
    }

    /// Everything the alarm sync should hold: the PC's bells and this phone's usage notices.
    pub fn alarms(&self, now_ms: i64) -> Vec<serde_json::Value> {
        let Some(latest) = self.latest() else {
            return Vec::new();
        };
        let mut alarms = crate::alarm_list(&latest.payload.alerts, now_ms);
        alarms.extend(self.notices.alarms(&latest.payload.snapshots, now_ms));
        alarms
    }

    /// Tests only: never probe the network with multicast.
    #[cfg(test)]
    pub(crate) fn without_discovery(mut self) -> Self {
        self.discovery = false;
        self
    }

    /// Every time a session with a PC ends, and why. A `Revoked` end means the PC removed this
    /// phone: call [`pc_revoked`](Self::pc_revoked) and refresh whatever shows its data.
    pub fn subscribe_ended(&self) -> broadcast::Receiver<Ended> {
        self.ended.subscribe()
    }

    /// New syncs as they arrive.
    pub fn subscribe(&self) -> watch::Receiver<Option<Arc<ReceivedSync>>> {
        self.client.subscribe()
    }

    // -------------------------------------------------------------- pairing

    /// Start pairing from the QR text. Nothing is stored until the PC confirms.
    pub async fn begin_pairing(&self, qr_text: &str) -> Result<PairStart, String> {
        let qr_text = qr_text.trim();
        let qr = QrPayload::decode(qr_text).map_err(|_| "That is not a Cooldown pairing code.")?;
        let pending = self
            .client
            .begin_pairing(qr_text)
            .await
            .map_err(|e| format!("Could not reach the PC: {e}"))?;
        let start = PairStart {
            code: pending.code().to_owned(),
            pc_name: pending.pc_name().to_owned(),
        };
        let addrs: Vec<SocketAddr> = qr
            .addrs
            .iter()
            .map(|ip| SocketAddr::new(*ip, qr.tcp_port))
            .collect();
        // Not remembered yet: until the PC confirms, nothing about it is stored.
        *self.pending.lock().await = Some(Pending {
            pairing: pending,
            addrs,
        });
        Ok(start)
    }

    /// Wait until the user confirms the code on the PC (or it is rejected or times out).
    pub async fn finish_pairing(&self) -> Result<(), String> {
        let Pending { pairing, addrs } = self
            .pending
            .lock()
            .await
            .take()
            .ok_or("No pairing is in progress.")?;
        let pc = pairing.pc_device_id();
        let connection = pairing
            .wait_for_confirmation()
            .await
            .map_err(|e| format!("The PC did not confirm: {e}"))?;
        self.remember_addrs(&pc, &addrs);
        self.install(connection);
        Ok(())
    }

    // -------------------------------------------------------------- connecting

    /// True while a session with a PC is open.
    pub fn connected(&self) -> bool {
        lock(&self.connection)
            .as_ref()
            .is_some_and(|c| !matches!(c.state(), ConnState::Closed(_)))
    }

    /// Keep `connection` as the current session (closing any earlier one) and report when it ends.
    fn install(&self, connection: Connection) {
        let connection = Arc::new(connection);
        let previous = lock(&self.connection).replace(connection.clone());
        if let Some(previous) = previous {
            previous.close();
        }
        let ended = self.ended.clone();
        tokio::spawn(async move {
            let reason = connection.closed().await;
            let _ = ended.send(Ended {
                pc: connection.pc_device_id(),
                reason,
            });
        });
    }

    /// Paired PCs, the most recently seen (or paired) first: the order to try reconnecting in.
    pub fn pcs_by_recency(&self) -> Vec<PeerRecord> {
        let mut pcs = self.client.paired_pcs().unwrap_or_default();
        pcs.sort_by_key(|p| std::cmp::Reverse(p.last_seen_ms.unwrap_or(0).max(p.paired_at_ms)));
        pcs
    }

    /// The addresses that last worked for `pc`.
    pub fn known_addrs(&self, pc: &DeviceId) -> Vec<SocketAddr> {
        self.known().remove(&pc.to_hex()).unwrap_or_default()
    }

    /// Connect to `pc` at one of `addrs` and keep the session. Remembers the addresses.
    pub async fn connect_to(&self, pc: &DeviceId, addrs: &[SocketAddr]) -> Result<(), String> {
        if addrs.is_empty() {
            return Err("No address is known for the PC.".into());
        }
        let connection = self
            .client
            .connect(pc, addrs)
            .await
            .map_err(|e| format!("Could not reach the PC: {e}"))?;
        connection
            .wait_ready()
            .await
            .map_err(|reason| format!("The PC closed the connection: {reason:?}"))?;
        self.install(connection);
        self.remember_addrs(pc, addrs);
        Ok(())
    }

    /// Try each paired PC, the most recently seen first, at the address that last worked for it.
    /// True once one answers. The session is kept.
    pub async fn connect_cached(&self) -> bool {
        for pc in &self.pcs_by_recency() {
            let known = self.known_addrs(&pc.device_id);
            if !known.is_empty() && self.connect_to(&pc.device_id, &known).await.is_ok() {
                return true;
            }
        }
        false
    }

    /// Probe the network for each paired PC's beacon and connect to the first that answers. True
    /// once one does. On Android the caller holds the Wi-Fi multicast lock around this.
    pub async fn connect_discovered(&self) -> bool {
        for pc in &self.pcs_by_recency() {
            let found = self.discover(pc).await;
            if !found.is_empty() && self.connect_to(&pc.device_id, &found).await.is_ok() {
                return true;
            }
        }
        false
    }

    /// End the session, if any, and give it a moment to say goodbye. The pairing stays.
    pub async fn disconnect(&self) {
        let connection = lock(&self.connection).take();
        if let Some(connection) = connection {
            connection.close();
            let _ = tokio::time::timeout(CLOSE_WAIT, connection.closed()).await;
        }
    }

    /// Ask the local network where `pc` is now (its beacon). Candidates only: the Noise handshake
    /// proves identity. Hold a Wi-Fi multicast lock around this on Android.
    pub async fn discover(&self, pc: &PeerRecord) -> Vec<SocketAddr> {
        let Some(key) = pc.beacon_key.filter(|_| self.discovery) else {
            return Vec::new();
        };
        let interfaces = lan_ipv4();
        if interfaces.is_empty() {
            return Vec::new();
        }
        let locator = UdpPcLocator::multicast(interfaces, Arc::new(SystemSyncClock::new()));
        locator
            .locate(key, DISCOVERY_WINDOW)
            .await
            .unwrap_or_default()
    }

    // -------------------------------------------------------------- PCs, bells, data

    /// Paired PCs.
    pub fn pcs(&self) -> Vec<PairedPc> {
        let connected_to = lock(&self.connection)
            .as_ref()
            .filter(|c| !matches!(c.state(), ConnState::Closed(_)))
            .map(|c| c.pc_device_id());
        self.client
            .paired_pcs()
            .unwrap_or_default()
            .into_iter()
            .map(|p| PairedPc {
                id: p.device_id.to_hex(),
                name: p.name,
                paired_at: iso_ms(p.paired_at_ms).unwrap_or_default(),
                last_seen_at: p.last_seen_ms.and_then(iso_ms),
                connected: connected_to == Some(p.device_id),
            })
            .collect()
    }

    /// Unpair a PC: tell it when connected, then forget it and everything it sent. When this
    /// returns the PC's readings and bells are gone from every view (`snapshots`, `alerts`,
    /// `latest`), whatever the connection's own task has or has not got round to yet.
    pub fn unpair(&self, pc_hex: &str) -> Result<(), String> {
        let pc = DeviceId::from_hex(pc_hex).ok_or("That is not a PC id.")?;
        let connection = {
            let mut guard = lock(&self.connection);
            if guard.as_ref().is_some_and(|c| c.pc_device_id() == pc) {
                guard.take()
            } else {
                None
            }
        };
        if let Some(connection) = connection {
            // Tells the PC, then ends the session.
            connection.unpair();
        }
        // The session does the same on its own task, a moment later; this does not wait for it.
        self.client.forget(&pc).map_err(|e| e.to_string())?;
        self.drop_data_of(&pc);
        Ok(())
    }

    /// The PC removed this phone (a session ended with [`CloseReason::Revoked`]): forget what it
    /// sent. The pairing record is already gone, the session removed it. Does nothing for a PC that
    /// is still paired, so a late report cannot wipe a pairing made since.
    pub fn pc_revoked(&self, pc: &DeviceId) {
        if self.is_paired(pc) {
            return;
        }
        {
            let mut guard = lock(&self.connection);
            if guard.as_ref().is_some_and(|c| c.pc_device_id() == *pc) {
                guard.take();
            }
        }
        self.drop_data_of(pc);
    }

    fn is_paired(&self, pc: &DeviceId) -> bool {
        self.client
            .paired_pcs()
            .is_ok_and(|pcs| pcs.iter().any(|p| p.device_id == *pc))
    }

    /// Forget the addresses and the saved sync of a PC that is no longer paired.
    fn drop_data_of(&self, pc: &DeviceId) {
        let mut all = self.known();
        if all.remove(&pc.to_hex()).is_some() {
            self.write_known(&all);
        }
        let saved = fs::read(&self.latest_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<StoredSync>(&bytes).ok());
        // A file that cannot be read is not worth keeping either: nothing could use it.
        if saved.is_none_or(|stored| stored.pc == pc.to_hex()) {
            let _ = fs::remove_file(&self.latest_path);
        }
    }

    /// Ask the PC to switch a bell (the PC is authoritative and syncs back).
    pub fn set_alert(&self, id: &str, enabled: bool) -> Result<(), String> {
        let guard = lock(&self.connection);
        let connection = guard
            .as_ref()
            .filter(|c| !matches!(c.state(), ConnState::Closed(_)))
            .ok_or("Not connected to your PC. Bells are kept there; try again when it is on.")?;
        connection.set_alert(id, enabled).map_err(|e| e.to_string())
    }

    /// The windows from the newest sync of a PC that is still paired.
    pub fn snapshots(&self) -> Vec<Snapshot> {
        self.latest()
            .map(|s| s.payload.snapshots)
            .unwrap_or_default()
    }

    /// The bells from the newest sync.
    pub fn alerts(&self) -> Vec<AlertIntent> {
        self.latest().map(|s| s.payload.alerts).unwrap_or_default()
    }

    /// The newest sync received (or restored from disk) from a PC that is still paired. A PC that
    /// was unpaired or revoked never shows up here, even if the client has not dropped its copy
    /// yet; another paired PC's sync takes its place.
    pub fn latest(&self) -> Option<ReceivedSync> {
        let paired = self.client.paired_pcs().unwrap_or_default();
        let is_paired = |pc: &DeviceId| paired.iter().any(|p| p.device_id == *pc);
        self.client
            .latest_sync()
            .filter(|s| is_paired(&s.pc))
            .or_else(|| {
                paired
                    .iter()
                    .filter_map(|p| self.client.latest_sync_for(&p.device_id))
                    .max_by_key(|s| s.received_at_ms)
            })
    }

    /// Keep the newest sync on disk for the next start. Written beside the target and renamed, so
    /// the app and a background round saving at once can never leave half a file.
    pub fn persist_latest(&self, sync: &ReceivedSync) {
        // A sync that was still on its way when the PC was removed must not bring its file back.
        if !self.is_paired(&sync.pc) {
            return;
        }
        let stored = StoredSync {
            pc: sync.pc.to_hex(),
            epoch: sync.epoch,
            rev: sync.rev,
            payload: sync.payload.clone(),
            received_at_ms: sync.received_at_ms,
        };
        if let Ok(bytes) = serde_json::to_vec(&stored) {
            write_atomic(&self.latest_path, &bytes);
        }
    }

    fn restore_latest(&self) {
        let Some(stored) = fs::read(&self.latest_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<StoredSync>(&b).ok())
        else {
            return;
        };
        let Some(pc) = DeviceId::from_hex(&stored.pc) else {
            return;
        };
        // Only for a PC that is still paired.
        if self.is_paired(&pc) {
            self.client.restore_latest(ReceivedSync {
                pc,
                epoch: stored.epoch,
                rev: stored.rev,
                payload: stored.payload,
                received_at_ms: stored.received_at_ms,
            });
        }
    }

    fn known(&self) -> HashMap<String, Vec<SocketAddr>> {
        fs::read(&self.addrs_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn write_known(&self, all: &HashMap<String, Vec<SocketAddr>>) {
        if let Ok(bytes) = serde_json::to_vec(all) {
            write_atomic(&self.addrs_path, &bytes);
        }
    }

    fn remember_addrs(&self, pc: &DeviceId, addrs: &[SocketAddr]) {
        let mut all = self.known();
        all.insert(pc.to_hex(), addrs.to_vec());
        self.write_known(&all);
    }
}

/// An in-process PC and helpers to pair with it, for the tests of this module and `background`.
#[cfg(test)]
pub(crate) mod testkit {
    use super::*;
    use cooldown_core::model::{SnapshotSource, SourceKind, Unit, WindowKind};
    use cooldown_core::{Snapshot, SCHEMA_VERSION};
    use cooldown_sync::{MemoryTrustStore, PcServer, ServerConfig, ServerEvent};

    pub const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    /// One 5-hour Claude window at `percent`, with a bell for when it resets.
    pub fn payload(percent: f64, resets_in: time::Duration) -> SyncPayload {
        let now = OffsetDateTime::now_utc();
        let mut window = Snapshot::new(
            "claude",
            "default",
            WindowKind::Session5h,
            Unit::Percent,
            SnapshotSource {
                kind: SourceKind::Official,
                adapter: "test".into(),
                adapter_version: "0".into(),
            },
            now,
        );
        window.percent_used = Some(percent);
        window.resets_at = Some(now + resets_in);
        SyncPayload {
            schema_version: SCHEMA_VERSION,
            device_id: "pc".into(),
            generated_at: now,
            alerts: vec![AlertIntent {
                id: window.alert_id(),
                fire_at: now + resets_in,
                enabled: true,
            }],
            snapshots: vec![window],
        }
    }

    pub async fn start_pc() -> PcServer {
        let trust: Arc<dyn TrustStore> = Arc::new(MemoryTrustStore::new());
        PcServer::start(
            ServerConfig::new("Test PC"),
            StaticKeypair::generate().unwrap(),
            trust,
            Arc::new(SystemSyncClock::new()),
            &[LOOPBACK],
            0,
        )
        .await
        .unwrap()
    }

    /// A phone on `dir` that never sends multicast.
    pub fn open(dir: &Path, key: &StaticKeypair) -> Phone {
        Phone::open(dir, "Pixel", key.duplicate())
            .unwrap()
            .without_discovery()
    }

    /// Start pairing with `pc`; returns once the phone shows its code and the PC has been asked.
    pub async fn begin(pc: &PcServer, phone: &Phone) -> broadcast::Receiver<ServerEvent> {
        let mut events = pc.subscribe();
        let offer = pc.open_pairing(&[LOOPBACK]).unwrap();
        phone.begin_pairing(&offer.qr_text).await.unwrap();
        // Wait until the PC has the prompt up, so `confirm_pairing` has something to confirm.
        loop {
            if let Ok(ServerEvent::PairingConfirmationRequested { .. }) = events.recv().await {
                return events;
            }
        }
    }

    /// Pair `phone` with `pc` (the PC confirms by itself). The session stays open.
    pub async fn pair(pc: &PcServer, phone: &Phone) {
        let _events = begin(pc, phone).await;
        pc.confirm_pairing(true).unwrap();
        phone.finish_pairing().await.unwrap();
    }

    /// Wait until the phone holds a sync with at least this revision.
    pub async fn wait_for_rev(phone: &Phone, rev: u64) {
        for _ in 0..250 {
            if phone.latest().is_some_and(|sync| sync.rev >= rev) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("no sync with rev {rev} arrived");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phone(dir: &Path) -> Phone {
        Phone::open(dir, "Pixel 9", StaticKeypair::generate().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn a_fresh_phone_has_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let phone = phone(dir.path());
        assert!(phone.pcs().is_empty());
        assert!(!phone.connected());
        assert!(phone.snapshots().is_empty());
        assert!(phone.alerts().is_empty());
        assert!(phone.pcs_by_recency().is_empty());
        assert!(phone.begin_pairing("not a code").await.is_err());
        assert!(phone.finish_pairing().await.is_err());
        assert!(phone
            .set_alert("reset:claude:default:weekly", true)
            .is_err());
        assert!(phone.unpair("zz").is_err());
        let pc = DeviceId::from_hex("00112233445566778899aabbccddeeff").unwrap();
        assert!(phone.connect_to(&pc, &[]).await.is_err());
    }

    #[test]
    fn addresses_are_remembered_per_pc() {
        let dir = tempfile::tempdir().unwrap();
        let phone = phone(dir.path());
        let pc = DeviceId::from_hex("00112233445566778899aabbccddeeff").unwrap();
        let addr: SocketAddr = "192.168.1.20:53320".parse().unwrap();
        phone.remember_addrs(&pc, &[addr]);
        assert_eq!(phone.known_addrs(&pc), vec![addr]);
        // Unpairing a PC clears its cached addresses.
        phone.unpair(&pc.to_hex()).unwrap();
        assert!(phone.known_addrs(&pc).is_empty());
    }

    #[test]
    fn a_stored_sync_for_an_unpaired_pc_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let stored = StoredSync {
            pc: "00112233445566778899aabbccddeeff".into(),
            epoch: 1,
            rev: 2,
            payload: SyncPayload {
                schema_version: cooldown_core::SCHEMA_VERSION,
                device_id: "00112233445566778899aabbccddeeff".into(),
                generated_at: OffsetDateTime::now_utc(),
                snapshots: vec![],
                alerts: vec![],
            },
            received_at_ms: 1,
        };
        fs::write(
            dir.path().join("latest.json"),
            serde_json::to_vec(&stored).unwrap(),
        )
        .unwrap();
        let phone = phone(dir.path());
        assert!(phone.client.latest_sync().is_none());
    }

    #[test]
    fn iso_formatting() {
        assert_eq!(iso_ms(0).as_deref(), Some("1970-01-01T00:00:00Z"));
    }

    // ---- pairing, unpairing and revocation against an in-process PC

    use super::testkit::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_pcs_address_is_stored_only_after_it_confirms() {
        let dir = tempfile::tempdir().unwrap();
        let key = StaticKeypair::generate().unwrap();
        let pc = start_pc().await;
        let phone = open(dir.path(), &key);

        let events = begin(&pc, &phone).await;
        // The code is on screen and the PC is asking its user: the phone knows nothing yet.
        assert!(!dir.path().join("pc-addresses.json").exists());
        assert!(phone.pcs().is_empty());

        pc.confirm_pairing(true).unwrap();
        phone.finish_pairing().await.unwrap();
        drop(events);
        let known = phone.known_addrs(&pc.device_id());
        assert_eq!(known.len(), 1);
        assert_eq!(known[0].port(), pc.port());
        assert!(dir.path().join("pc-addresses.json").exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pairing_the_pc_refuses_leaves_no_address_behind() {
        let dir = tempfile::tempdir().unwrap();
        let pc = start_pc().await;
        let phone = open(dir.path(), &StaticKeypair::generate().unwrap());
        let _events = begin(&pc, &phone).await;
        pc.confirm_pairing(false).unwrap();
        assert!(phone.finish_pairing().await.is_err());
        assert!(!dir.path().join("pc-addresses.json").exists());
        assert!(phone.known_addrs(&pc.device_id()).is_empty());
        assert!(phone.pcs().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unpairing_removes_the_pcs_readings_bells_and_files_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let pc = start_pc().await;
        let phone = open(dir.path(), &StaticKeypair::generate().unwrap());
        pair(&pc, &phone).await;
        let stamp = pc
            .push_sync(&payload(50.0, time::Duration::hours(1)))
            .unwrap();
        wait_for_rev(&phone, stamp.rev).await;
        phone.persist_latest(&phone.latest().unwrap());
        assert_eq!(phone.snapshots().len(), 1);
        assert_eq!(phone.alerts().len(), 1);
        assert!(dir.path().join("latest.json").exists());

        phone.unpair(&pc.device_id().to_hex()).unwrap();

        // Straight away, without waiting for the session's own task to finish.
        assert!(phone.snapshots().is_empty());
        assert!(phone.alerts().is_empty());
        assert!(phone.latest().is_none());
        assert!(phone.pcs().is_empty());
        assert!(!dir.path().join("latest.json").exists());
        assert!(phone.known_addrs(&pc.device_id()).is_empty());
        assert!(!phone.connected());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pc_that_removes_the_phone_takes_its_data_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let key = StaticKeypair::generate().unwrap();
        let pc = start_pc().await;
        let phone = open(dir.path(), &key);
        let mut ended = phone.subscribe_ended();
        pair(&pc, &phone).await;
        let stamp = pc
            .push_sync(&payload(50.0, time::Duration::hours(1)))
            .unwrap();
        wait_for_rev(&phone, stamp.rev).await;
        phone.persist_latest(&phone.latest().unwrap());

        pc.revoke(&key.device_id()).unwrap();
        let end = tokio::time::timeout(Duration::from_secs(5), ended.recv())
            .await
            .expect("the session ends")
            .unwrap();
        assert_eq!(end.reason, CloseReason::Revoked);
        assert_eq!(end.pc, pc.device_id());

        phone.pc_revoked(&end.pc);
        assert!(phone.snapshots().is_empty());
        assert!(phone.alerts().is_empty());
        assert!(phone.pcs().is_empty());
        assert!(!dir.path().join("latest.json").exists());
        assert!(phone.known_addrs(&end.pc).is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_late_revocation_report_cannot_wipe_a_pc_that_is_paired_again() {
        let dir = tempfile::tempdir().unwrap();
        let pc = start_pc().await;
        let phone = open(dir.path(), &StaticKeypair::generate().unwrap());
        pair(&pc, &phone).await;
        let stamp = pc
            .push_sync(&payload(50.0, time::Duration::hours(1)))
            .unwrap();
        wait_for_rev(&phone, stamp.rev).await;
        phone.persist_latest(&phone.latest().unwrap());

        phone.pc_revoked(&pc.device_id());
        assert_eq!(phone.snapshots().len(), 1);
        assert!(dir.path().join("latest.json").exists());
        assert!(phone.connected());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn removing_one_of_two_pcs_keeps_the_other_ones_data() {
        let dir = tempfile::tempdir().unwrap();
        let first = start_pc().await;
        let second = start_pc().await;
        let phone = open(dir.path(), &StaticKeypair::generate().unwrap());

        pair(&first, &phone).await;
        let stamp = first
            .push_sync(&payload(20.0, time::Duration::hours(1)))
            .unwrap();
        wait_for_rev(&phone, stamp.rev).await;
        pair(&second, &phone).await;
        let stamp = second
            .push_sync(&payload(90.0, time::Duration::hours(2)))
            .unwrap();
        for _ in 0..250 {
            if phone.snapshots().first().and_then(|s| s.percent_used) == Some(90.0) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            phone.snapshots()[0].percent_used,
            Some(90.0),
            "rev {}",
            stamp.rev
        );
        assert_eq!(phone.pcs().len(), 2);

        phone.unpair(&second.device_id().to_hex()).unwrap();
        assert_eq!(phone.pcs().len(), 1);
        assert_eq!(
            phone.snapshots()[0].percent_used,
            Some(20.0),
            "the first PC's reading is shown again, never the removed PC's"
        );
        assert_eq!(phone.alerts().len(), 1);
    }
}
