//! Shared harness for the integration tests: real sockets on 127.0.0.1, fake clocks, in-memory
//! stores. Nothing here touches the OS keychain or the network beyond loopback.
#![allow(dead_code)]

pub mod raw;

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use cooldown_core::{
    AlertIntent, FakeClock, Snapshot, SnapshotSource, SourceKind, SyncPayload, Unit, WindowKind,
};
use cooldown_sync::client::{ClientConfig, PhoneClient};
use cooldown_sync::server::{PcServer, ServerConfig, ServerEvent};
use cooldown_sync::{Limits, MemoryTrustStore, StaticKeypair, SyncClock, TrustStore};
use time::OffsetDateTime;
use tokio::sync::broadcast;

pub const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

pub fn fake_clock() -> Arc<FakeClock> {
    FakeClock::new(OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1_700_000_000))
}

pub struct Pc {
    pub server: PcServer,
    pub trust: Arc<MemoryTrustStore>,
    pub clock: Arc<FakeClock>,
    pub keypair_pub: [u8; 32],
    pub keypair_private: [u8; 32],
}

impl Pc {
    pub fn addr(&self) -> SocketAddr {
        SocketAddr::new(LOOPBACK, self.server.port())
    }
}

pub async fn start_pc(clock: &Arc<FakeClock>, tweak: impl FnOnce(&mut Limits)) -> Pc {
    start_pc_with(clock, |config| tweak(&mut config.limits)).await
}

pub async fn start_pc_with(clock: &Arc<FakeClock>, tweak: impl FnOnce(&mut ServerConfig)) -> Pc {
    let mut config = ServerConfig::new("Test PC");
    config.limits.min_push_interval = Duration::ZERO;
    tweak(&mut config);
    let keypair = StaticKeypair::generate().unwrap();
    let keypair_pub = *keypair.public();
    let keypair_private = *keypair.private_bytes();
    let trust = Arc::new(MemoryTrustStore::new());
    let server = PcServer::start(
        config,
        keypair,
        trust.clone() as Arc<dyn TrustStore>,
        clock.clone() as Arc<dyn SyncClock>,
        &[LOOPBACK],
        0,
    )
    .await
    .unwrap();
    Pc {
        server,
        trust,
        clock: clock.clone(),
        keypair_pub,
        keypair_private,
    }
}

pub struct Phone {
    pub client: PhoneClient,
    pub trust: Arc<MemoryTrustStore>,
    pub id: cooldown_sync::DeviceId,
    pub public: [u8; 32],
    pub private: [u8; 32],
}

pub fn phone(clock: &Arc<FakeClock>, name: &str) -> Phone {
    phone_with(clock, name, |_| {})
}

pub fn phone_with(clock: &Arc<FakeClock>, name: &str, tweak: impl FnOnce(&mut Limits)) -> Phone {
    let mut config = ClientConfig::new(name, "android");
    tweak(&mut config.limits);
    let keypair = StaticKeypair::generate().unwrap();
    let id = keypair.device_id();
    let public = *keypair.public();
    let private = *keypair.private_bytes();
    let trust = Arc::new(MemoryTrustStore::new());
    let client = PhoneClient::new(
        config,
        keypair,
        trust.clone() as Arc<dyn TrustStore>,
        clock.clone() as Arc<dyn SyncClock>,
    )
    .unwrap();
    Phone {
        client,
        trust,
        id,
        public,
        private,
    }
}

/// Real-time bound for anything that waits on sockets.
pub async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("timed out waiting for a real socket event")
}

/// Wait for the first event matching `pick`.
pub async fn next_event<T>(
    events: &mut broadcast::Receiver<ServerEvent>,
    mut pick: impl FnMut(&ServerEvent) -> Option<T>,
) -> T {
    within(async {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if let Some(found) = pick(&event) {
                        return found;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => panic!("event channel closed"),
            }
        }
    })
    .await
}

/// Poll `check` every few real milliseconds until it holds.
pub async fn eventually(mut check: impl FnMut() -> bool) {
    within(async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
}

pub fn sample_payload(percent: f64) -> SyncPayload {
    let mut snapshot = Snapshot::new(
        "claude",
        "work",
        WindowKind::Session5h,
        Unit::Percent,
        SnapshotSource {
            kind: SourceKind::Official,
            adapter: "statusline".into(),
            adapter_version: "1".into(),
        },
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1_700_000_000),
    );
    snapshot.percent_used = Some(percent);
    snapshot.resets_at = Some(OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1_700_010_000));
    SyncPayload {
        schema_version: 1,
        device_id: "ignored-the-server-stamps-its-own".into(),
        generated_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1_700_000_000),
        snapshots: vec![snapshot],
        alerts: vec![AlertIntent {
            id: "reset:claude:work:session_5h".into(),
            fire_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1_700_010_000),
            enabled: true,
        }],
    }
}

/// Run a complete successful pairing and return the live connection.
pub async fn pair(
    pc: &Pc,
    phone: &Phone,
    events: &mut broadcast::Receiver<ServerEvent>,
) -> cooldown_sync::Connection {
    let offer = pc.server.open_pairing(&[]).unwrap();
    pair_using(pc, phone, events, &offer.qr_text).await
}

/// Pair using an already opened window's QR text.
pub async fn pair_using(
    pc: &Pc,
    phone: &Phone,
    events: &mut broadcast::Receiver<ServerEvent>,
    qr_text: &str,
) -> cooldown_sync::Connection {
    let pending = within(phone.client.begin_pairing(qr_text)).await.unwrap();
    let shown = next_event(events, |e| match e {
        ServerEvent::PairingConfirmationRequested { code, .. } => Some(code.clone()),
        _ => None,
    })
    .await;
    assert_eq!(shown, pending.code(), "both sides must show the same code");
    pc.server.confirm_pairing(true).unwrap();
    let connection = within(pending.wait_for_confirmation()).await.unwrap();
    within(connection.wait_ready()).await.unwrap();
    connection
}

use cooldown_sync::frame::MsgType;
use cooldown_sync::hello::Mode;
use cooldown_sync::message::EmptyBody;
use cooldown_sync::QrPayload;
use raw::{handshake, pair_payload, reconnect_payload, start, Raw, RawSession};

/// Open a window and parse its QR (what a scanning phone learns).
pub fn open_window(pc: &Pc) -> QrPayload {
    let offer = pc.server.open_pairing(&[]).unwrap();
    QrPayload::decode(&offer.qr_text).unwrap()
}

/// Pair a hand-driven phone with `keypair`: handshake, HELLO, user confirms, read PAIR_OK and the
/// PC's HELLO. Returns the live raw session.
pub async fn raw_pair(
    pc: &Pc,
    events: &mut broadcast::Receiver<ServerEvent>,
    keypair: &StaticKeypair,
    name: &str,
) -> RawSession {
    let qr = open_window(pc);
    let started = start(
        Mode::Pair,
        keypair,
        &qr.pc_static_pub,
        Some(&*qr.psk),
        qr.pair_id,
        &pair_payload(name, 1_000),
        None,
    );
    let mut session = handshake(Raw::connect(pc.addr()).await, started)
        .await
        .expect("pairing handshake");
    session.send_hello(1).await;
    next_event(events, |e| {
        matches!(e, ServerEvent::PairingConfirmationRequested { .. }).then_some(())
    })
    .await;
    pc.server.confirm_pairing(true).unwrap();
    let _: EmptyBody = session.recv_json(MsgType::PairOk).await;
    session.recv_until(MsgType::Hello).await.expect("PC HELLO");
    session
}

/// A hand-driven reconnect with timestamp `ts`.
pub async fn raw_reconnect(
    pc: &Pc,
    keypair: &StaticKeypair,
    ts: u64,
) -> Result<RawSession, Option<u8>> {
    let started = start(
        Mode::Reconnect,
        keypair,
        &pc.keypair_pub,
        None,
        [0; 16],
        &reconnect_payload(ts),
        None,
    );
    handshake(Raw::connect(pc.addr()).await, started).await
}

/// Drain pending events without waiting.
pub fn drain(events: &mut broadcast::Receiver<ServerEvent>) -> Vec<ServerEvent> {
    let mut out = Vec::new();
    while let Ok(event) = events.try_recv() {
        out.push(event);
    }
    out
}

/// Let spawned tasks and sockets settle in real time.
pub async fn settle() {
    tokio::time::sleep(Duration::from_millis(100)).await;
}
