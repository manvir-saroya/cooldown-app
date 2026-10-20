//! The phone client against a misbehaving PC.

mod common;

use common::raw::{pc_hello_body, FakePc};
use common::*;
use cooldown_sync::frame::MsgType;
use cooldown_sync::hello::Mode;
use cooldown_sync::message::{AckBody, EmptyBody, SyncBody};
use cooldown_sync::{ClientError, CloseReason, ConnState, TrustStore};

fn sync_body(pc: &FakePc, epoch: u64, rev: u64, device_id: Option<String>) -> Vec<u8> {
    let mut payload = sample_payload(30.0);
    payload.device_id = device_id.unwrap_or_else(|| pc.device_id().to_hex());
    serde_json::to_vec(&SyncBody {
        epoch,
        rev,
        payload,
    })
    .unwrap()
}

#[tokio::test]
async fn sync_before_pair_ok_is_a_violation_and_nothing_is_stored() {
    let clock = fake_clock();
    let phone = phone(&clock, "Pixel 8");
    let pc = FakePc::new().await;
    let qr = pc.qr_text();
    let (begin, accepted) = tokio::join!(phone.client.begin_pairing(&qr), pc.accept(Mode::Pair));
    let pending = begin.unwrap();
    let (mut session, payload) = accepted;
    assert!(String::from_utf8_lossy(&payload).contains("Pixel 8"));
    // Both sides derive the same code from the handshake hash.
    let expected = cooldown_sync::noise::format_confirmation_code(
        cooldown_sync::noise::confirmation_code(&session.handshake_hash),
    );
    assert_eq!(pending.code(), expected);
    // The PC jumps the gun and sends data before the user confirmed anything.
    let body = sync_body(&pc, 1, 1, None);
    session.send(MsgType::Sync, &body).await;
    let result = within(pending.wait_for_confirmation()).await;
    assert!(
        matches!(result, Err(ClientError::Protocol(_))),
        "{result:?}"
    );
    assert!(phone.client.paired_pcs().unwrap().is_empty());
    assert!(phone.client.latest_sync().is_none());
}

#[tokio::test]
async fn a_revoke_or_a_hangup_before_pair_ok_stores_nothing() {
    let clock = fake_clock();
    let phone = phone(&clock, "Pixel 8");
    for revoke in [true, false] {
        let expected_refused = revoke;
        let pc = FakePc::new().await;
        let qr = pc.qr_text();
        let (begin, accepted) =
            tokio::join!(phone.client.begin_pairing(&qr), pc.accept(Mode::Pair));
        let pending = begin.unwrap();
        let (mut session, _) = accepted;
        if revoke {
            session
                .send(MsgType::Revoke, br#"{"reason":"rejected"}"#)
                .await;
        } else {
            drop(session);
        }
        let result = within(pending.wait_for_confirmation()).await;
        if expected_refused {
            assert!(
                matches!(result, Err(ClientError::PairingRefused)),
                "{result:?}"
            );
        } else {
            assert!(matches!(result, Err(ClientError::Closed)), "{result:?}");
        }
        assert!(phone.client.paired_pcs().unwrap().is_empty());
    }
}

#[tokio::test]
async fn the_phone_stores_the_pc_only_after_pair_ok_and_reads_the_beacon_key_from_hello() {
    let clock = fake_clock();
    let phone = phone(&clock, "Pixel 8");
    let pc = FakePc::new().await;
    let qr = pc.qr_text();
    let (begin, accepted) = tokio::join!(phone.client.begin_pairing(&qr), pc.accept(Mode::Pair));
    let pending = begin.unwrap();
    let (mut session, _) = accepted;
    // The phone's HELLO is the first transport message.
    session
        .recv_until(MsgType::Hello)
        .await
        .expect("phone HELLO");
    assert!(
        phone.client.paired_pcs().unwrap().is_empty(),
        "nothing before PAIR_OK"
    );
    session.send(MsgType::PairOk, b"{}").await;
    let connection = within(pending.wait_for_confirmation()).await.unwrap();
    assert_eq!(connection.pc_device_id(), pc.device_id());
    assert_eq!(phone.client.paired_pcs().unwrap().len(), 1);
    session
        .send(MsgType::Hello, &pc_hello_body(Some([9; 32])))
        .await;
    within(connection.wait_ready()).await.unwrap();
    let record = phone.trust.get(&pc.device_id()).unwrap().unwrap();
    assert_eq!(record.beacon_key, Some([9; 32]));
    assert_eq!(record.name, "Fake PC");
    // The offset estimate exists and shows the two clocks agree to within the fake round trip.
    assert!(connection.clock_offset_ms().is_some());
}

async fn paired_with_fake_pc(
    phone: &Phone,
    pc: &FakePc,
) -> (cooldown_sync::Connection, common::raw::RawSession) {
    let qr = pc.qr_text();
    let (begin, accepted) = tokio::join!(phone.client.begin_pairing(&qr), pc.accept(Mode::Pair));
    let pending = begin.unwrap();
    let (mut session, _) = accepted;
    session.recv_until(MsgType::Hello).await.unwrap();
    session.send(MsgType::PairOk, b"{}").await;
    let connection = within(pending.wait_for_confirmation()).await.unwrap();
    session.send(MsgType::Hello, &pc_hello_body(None)).await;
    within(connection.wait_ready()).await.unwrap();
    (connection, session)
}

#[tokio::test]
async fn bad_payloads_close_the_connection_and_are_not_applied() {
    let clock = fake_clock();
    let phone = phone(&clock, "Pixel 8");
    let pc = FakePc::new().await;
    let (connection, mut session) = paired_with_fake_pc(&phone, &pc).await;

    // A SYNC whose deviceId is not the PC it came from.
    let body = sync_body(&pc, 1, 1, Some("someone-else".into()));
    session.send(MsgType::Sync, &body).await;
    assert_eq!(
        within(connection.closed()).await,
        CloseReason::Remote("bad_payload")
    );
    assert!(phone.client.latest_sync().is_none());
    // The pairing itself survives a protocol error (only an authenticated REVOKE unpairs).
    assert_eq!(phone.client.paired_pcs().unwrap().len(), 1);
}

#[tokio::test]
async fn payloads_over_the_shared_limits_are_refused() {
    use cooldown_core::AlertIntent;
    let clock = fake_clock();
    // Core would accept up to 256 entries; the protocol caps lists at 200 and notes at 256.
    for case in ["alerts", "note"] {
        let phone = phone(&clock, "Pixel 8");
        let pc = FakePc::new().await;
        let (connection, mut session) = paired_with_fake_pc(&phone, &pc).await;
        let mut payload = sample_payload(1.0);
        payload.device_id = pc.device_id().to_hex();
        if case == "alerts" {
            payload.alerts = (0..201)
                .map(|i| AlertIntent {
                    id: format!("reset:claude:acct{i}:weekly"),
                    fire_at: payload.generated_at,
                    enabled: true,
                })
                .collect();
        } else {
            payload.snapshots[0].note = Some("n".repeat(257));
        }
        let body = serde_json::to_vec(&SyncBody {
            epoch: 1,
            rev: 1,
            payload,
        })
        .unwrap();
        assert!(body.len() <= cooldown_sync::frame::MAX_BODY_LEN, "{case}");
        session.send(MsgType::Sync, &body).await;
        assert_eq!(
            within(connection.closed()).await,
            CloseReason::Remote("bad_payload"),
            "{case}"
        );
        assert!(phone.client.latest_sync().is_none(), "{case}");
    }
}

#[tokio::test]
async fn idempotent_epoch_rev_semantics() {
    let clock = fake_clock();
    let phone = phone(&clock, "Pixel 8");
    let pc = FakePc::new().await;
    let (connection, mut session) = paired_with_fake_pc(&phone, &pc).await;
    let mut updates = phone.client.subscribe();
    updates.borrow_and_update();

    // First SYNC applies and is acknowledged.
    session
        .send(MsgType::Sync, &sync_body(&pc, 7, 5, None))
        .await;
    let ack: AckBody = session.recv_json(MsgType::Ack).await;
    assert_eq!((ack.epoch, ack.rev), (7, 5));
    assert!(updates.has_changed().unwrap());
    updates.borrow_and_update();

    // The same (epoch, rev) again, and an older rev: acknowledged, not applied.
    for rev in [5u64, 4, 1] {
        session
            .send(MsgType::Sync, &sync_body(&pc, 7, rev, None))
            .await;
        let ack: AckBody = session.recv_json(MsgType::Ack).await;
        assert_eq!((ack.epoch, ack.rev), (7, rev));
    }
    assert!(!updates.has_changed().unwrap());
    assert_eq!(phone.client.latest_sync().unwrap().rev, 5);

    // A higher rev applies; a new epoch applies even with a lower rev (a restored PC).
    session
        .send(MsgType::Sync, &sync_body(&pc, 7, 6, None))
        .await;
    let _: AckBody = session.recv_json(MsgType::Ack).await;
    assert_eq!(phone.client.latest_sync().unwrap().rev, 6);
    session
        .send(MsgType::Sync, &sync_body(&pc, 8, 1, None))
        .await;
    let _: AckBody = session.recv_json(MsgType::Ack).await;
    let latest = phone.client.latest_sync().unwrap();
    assert_eq!((latest.epoch, latest.rev), (8, 1));
    assert_eq!(connection.state(), ConnState::Ready);
}

#[tokio::test]
async fn an_authenticated_revoke_unpairs_and_cancels_nothing_else() {
    let clock = fake_clock();
    let phone = phone(&clock, "Pixel 8");
    let pc = FakePc::new().await;
    let (connection, mut session) = paired_with_fake_pc(&phone, &pc).await;
    session
        .send(MsgType::Sync, &sync_body(&pc, 1, 1, None))
        .await;
    let _: AckBody = session.recv_json(MsgType::Ack).await;
    session
        .send(MsgType::Revoke, br#"{"reason":"removed"}"#)
        .await;
    assert_eq!(within(connection.closed()).await, CloseReason::Revoked);
    assert!(phone.client.paired_pcs().unwrap().is_empty());
    assert!(phone.client.latest_sync_for(&pc.device_id()).is_none());
}

#[tokio::test]
async fn a_corrupted_frame_closes_the_phone_side_too() {
    let clock = fake_clock();
    let phone = phone(&clock, "Pixel 8");
    let pc = FakePc::new().await;
    let (connection, mut session) = paired_with_fake_pc(&phone, &pc).await;
    let mut frame = session.encrypt_frame(MsgType::Ping, br#"{"n":1}"#);
    let last = frame.len() - 1;
    frame[last] ^= 0x01;
    session.raw.write(&frame).await;
    assert_eq!(
        within(connection.closed()).await,
        CloseReason::Remote("decrypt")
    );
}

#[tokio::test]
async fn messages_the_phone_never_accepts_from_a_pc() {
    let clock = fake_clock();
    for (msg_type, body) in [
        (MsgType::PairOk, &b"{}"[..]),
        (MsgType::Ack, br#"{"epoch":1,"rev":1}"#),
        (
            MsgType::AlertSet,
            br#"{"op_id":1,"id":"reset:a:b:weekly","enabled":true}"#,
        ),
    ] {
        let phone = phone(&clock, "Pixel 8");
        let pc = FakePc::new().await;
        let (connection, mut session) = paired_with_fake_pc(&phone, &pc).await;
        session.send(msg_type, body).await;
        assert_eq!(
            within(connection.closed()).await,
            CloseReason::Remote("unexpected")
        );
    }
    // An empty-body PAIR_OK with extra fields is malformed even where PAIR_OK is expected.
    let phone = phone(&clock, "Pixel 8");
    let pc = FakePc::new().await;
    let qr = pc.qr_text();
    let (begin, accepted) = tokio::join!(phone.client.begin_pairing(&qr), pc.accept(Mode::Pair));
    let pending = begin.unwrap();
    let (mut session, _) = accepted;
    session.send(MsgType::PairOk, br#"{"surprise":1}"#).await;
    assert!(within(pending.wait_for_confirmation()).await.is_err());
    assert!(phone.client.paired_pcs().unwrap().is_empty());
    let _ = EmptyBody {};
}

#[tokio::test]
async fn a_qr_naming_the_phone_itself_is_refused_before_any_connection() {
    let clock = fake_clock();
    let phone = phone(&clock, "Pixel 8");
    let qr = cooldown_sync::QrPayload {
        pc_static_pub: phone.public,
        psk: zeroize::Zeroizing::new([1; 32]),
        pair_id: [2; 16],
        tcp_port: 9,
        addrs: vec![LOOPBACK],
    }
    .encode()
    .unwrap();
    assert!(matches!(
        phone.client.begin_pairing(&qr).await,
        Err(ClientError::Protocol(_))
    ));
}

/// A persisted state restored at app start is what `latest_sync` shows until something newer
/// arrives (it used to be stored but invisible until the next live SYNC).
#[test]
fn a_restored_state_is_the_latest_until_a_newer_one() {
    use cooldown_core::SyncPayload;
    use cooldown_sync::{
        ClientConfig, MemoryTrustStore, PeerRecord, PhoneClient, ReceivedSync, StaticKeypair,
        SystemSyncClock, TrustStore,
    };
    use std::sync::Arc;

    let pc = StaticKeypair::generate().unwrap();
    let trust = Arc::new(MemoryTrustStore::new());
    trust
        .upsert(PeerRecord::new(
            *pc.public(),
            "PC".into(),
            "windows".into(),
            1,
            0,
            1,
        ))
        .unwrap();
    let client = PhoneClient::new(
        ClientConfig::new("Pixel", "android"),
        StaticKeypair::generate().unwrap(),
        trust,
        Arc::new(SystemSyncClock::new()),
    )
    .unwrap();
    let state = |received_at_ms: i64, rev: u64| ReceivedSync {
        pc: pc.device_id(),
        epoch: 7,
        rev,
        payload: SyncPayload {
            schema_version: cooldown_core::SCHEMA_VERSION,
            device_id: pc.device_id().to_hex(),
            generated_at: time::OffsetDateTime::UNIX_EPOCH,
            snapshots: vec![],
            alerts: vec![],
        },
        received_at_ms,
    };

    assert!(client.latest_sync().is_none());
    client.restore_latest(state(200, 2));
    assert_eq!(client.latest_sync().unwrap().rev, 2);
    client.restore_latest(state(100, 1));
    assert_eq!(
        client.latest_sync().unwrap().rev,
        2,
        "an older state does not replace it"
    );

    // Unknown PC: ignored.
    let stranger = StaticKeypair::generate().unwrap();
    let mut foreign = state(300, 3);
    foreign.pc = stranger.device_id();
    client.restore_latest(foreign);
    assert_eq!(client.latest_sync().unwrap().rev, 2);
}

/// Two PCs paired with one phone, each having synced; the second one synced later, so its state
/// is `latest_sync()`.
struct TwoPcs {
    older: FakePc,
    older_connection: cooldown_sync::Connection,
    older_session: common::raw::RawSession,
    newer: FakePc,
    newer_connection: cooldown_sync::Connection,
    newer_session: common::raw::RawSession,
}

async fn two_synced_pcs(clock: &std::sync::Arc<cooldown_core::FakeClock>, phone: &Phone) -> TwoPcs {
    let older = FakePc::new().await;
    let (older_connection, mut older_session) = paired_with_fake_pc(phone, &older).await;
    older_session
        .send(MsgType::Sync, &sync_body(&older, 1, 1, None))
        .await;
    let _: AckBody = older_session.recv_json(MsgType::Ack).await;
    clock.advance(std::time::Duration::from_secs(5)).await;
    let newer = FakePc::new().await;
    let (newer_connection, mut newer_session) = paired_with_fake_pc(phone, &newer).await;
    newer_session
        .send(MsgType::Sync, &sync_body(&newer, 1, 1, None))
        .await;
    let _: AckBody = newer_session.recv_json(MsgType::Ack).await;
    assert_eq!(
        phone.client.latest_sync().unwrap().pc,
        newer.device_id(),
        "the later sync is the latest"
    );
    assert_eq!(phone.client.paired_pcs().unwrap().len(), 2);
    TwoPcs {
        older,
        older_connection,
        older_session,
        newer,
        newer_connection,
        newer_session,
    }
}

/// M1: after an authenticated REVOKE from the PC whose state is `latest_sync()`, the phone must
/// not keep showing (or scheduling alarms from) that PC. It falls back to the other PC's state,
/// and to nothing once that PC is gone too.
#[tokio::test]
async fn a_revoke_of_the_latest_pc_drops_its_state_from_latest_sync() {
    let clock = fake_clock();
    let phone = phone(&clock, "Pixel 8");
    let mut two = two_synced_pcs(&clock, &phone).await;
    let mut updates = phone.client.subscribe();
    updates.borrow_and_update();

    two.newer_session
        .send(MsgType::Revoke, br#"{"reason":"removed"}"#)
        .await;
    assert_eq!(
        within(two.newer_connection.closed()).await,
        CloseReason::Revoked
    );
    assert!(phone
        .client
        .latest_sync_for(&two.newer.device_id())
        .is_none());
    let fallback = phone.client.latest_sync().expect("the other PC's state");
    assert_eq!(fallback.pc, two.older.device_id());
    // The app's subscription saw the change too.
    assert!(updates.has_changed().unwrap());
    assert_eq!(
        updates.borrow_and_update().as_ref().unwrap().pc,
        two.older.device_id()
    );

    two.older_session
        .send(MsgType::Revoke, br#"{"reason":"removed"}"#)
        .await;
    assert_eq!(
        within(two.older_connection.closed()).await,
        CloseReason::Revoked
    );
    assert!(phone.client.latest_sync().is_none());
    assert!(phone
        .client
        .latest_sync_for(&two.older.device_id())
        .is_none());
    assert!(updates.has_changed().unwrap());
    assert!(updates.borrow_and_update().is_none());
    assert!(phone.client.paired_pcs().unwrap().is_empty());
}

/// Revoking a PC whose state is not the latest leaves `latest_sync()` alone (and does not wake
/// subscribers for nothing).
#[tokio::test]
async fn a_revoke_of_another_pc_leaves_latest_sync_unchanged() {
    let clock = fake_clock();
    let phone = phone(&clock, "Pixel 8");
    let mut two = two_synced_pcs(&clock, &phone).await;
    let mut updates = phone.client.subscribe();
    updates.borrow_and_update();

    two.older_session
        .send(MsgType::Revoke, br#"{"reason":"removed"}"#)
        .await;
    assert_eq!(
        within(two.older_connection.closed()).await,
        CloseReason::Revoked
    );
    assert!(phone
        .client
        .latest_sync_for(&two.older.device_id())
        .is_none());
    assert_eq!(
        phone.client.latest_sync().unwrap().pc,
        two.newer.device_id()
    );
    assert!(!updates.has_changed().unwrap());
    assert_eq!(phone.client.paired_pcs().unwrap().len(), 1);
}

/// M1: the phone unpairing from the PC whose state is `latest_sync()` has the same effect.
#[tokio::test]
async fn unpairing_the_latest_pc_drops_its_state_from_latest_sync() {
    let clock = fake_clock();
    let phone = phone(&clock, "Pixel 8");
    let two = two_synced_pcs(&clock, &phone).await;

    two.newer_connection.unpair();
    assert_eq!(
        within(two.newer_connection.closed()).await,
        CloseReason::Unpaired
    );
    assert!(phone
        .client
        .latest_sync_for(&two.newer.device_id())
        .is_none());
    assert_eq!(
        phone.client.latest_sync().unwrap().pc,
        two.older.device_id()
    );

    two.older_connection.unpair();
    assert_eq!(
        within(two.older_connection.closed()).await,
        CloseReason::Unpaired
    );
    assert!(phone.client.latest_sync().is_none());
    assert!(phone
        .client
        .latest_sync_for(&two.older.device_id())
        .is_none());
    drop((two.older_session, two.newer_session));
}

/// M1: `forget` (the user removed a PC on the phone while not connected to it).
#[test]
fn forgetting_the_latest_pc_drops_its_state_from_latest_sync() {
    use cooldown_core::SyncPayload;
    use cooldown_sync::{
        ClientConfig, MemoryTrustStore, PeerRecord, PhoneClient, ReceivedSync, StaticKeypair,
        SystemSyncClock,
    };
    use std::sync::Arc;

    let trust = Arc::new(MemoryTrustStore::new());
    let pcs: Vec<StaticKeypair> = (0..2).map(|_| StaticKeypair::generate().unwrap()).collect();
    for pc in &pcs {
        trust
            .upsert(PeerRecord::new(
                *pc.public(),
                "PC".into(),
                "windows".into(),
                1,
                0,
                1,
            ))
            .unwrap();
    }
    let client = PhoneClient::new(
        ClientConfig::new("Pixel", "android"),
        StaticKeypair::generate().unwrap(),
        trust.clone(),
        Arc::new(SystemSyncClock::new()),
    )
    .unwrap();
    let state = |pc: &StaticKeypair, received_at_ms: i64| ReceivedSync {
        pc: pc.device_id(),
        epoch: 7,
        rev: 1,
        payload: SyncPayload {
            schema_version: cooldown_core::SCHEMA_VERSION,
            device_id: pc.device_id().to_hex(),
            generated_at: time::OffsetDateTime::UNIX_EPOCH,
            snapshots: vec![],
            alerts: vec![],
        },
        received_at_ms,
    };
    let (first, second) = (&pcs[0], &pcs[1]);
    client.restore_latest(state(first, 100));
    client.restore_latest(state(second, 200));
    assert_eq!(client.latest_sync().unwrap().pc, second.device_id());

    // Forgetting the PC that is not the latest changes nothing visible.
    assert!(client.forget(&first.device_id()).unwrap());
    assert!(client.latest_sync_for(&first.device_id()).is_none());
    assert_eq!(client.latest_sync().unwrap().pc, second.device_id());

    // Put it back, then forget the latest one: the other PC's state takes over.
    trust
        .upsert(PeerRecord::new(
            *first.public(),
            "PC".into(),
            "windows".into(),
            1,
            0,
            1,
        ))
        .unwrap();
    client.restore_latest(state(first, 100));
    assert!(client.forget(&second.device_id()).unwrap());
    assert!(client.latest_sync_for(&second.device_id()).is_none());
    assert_eq!(client.latest_sync().unwrap().pc, first.device_id());

    // And with nothing left, there is no latest state at all.
    assert!(client.forget(&first.device_id()).unwrap());
    assert!(client.latest_sync().is_none());
    assert!(client.latest_sync_for(&first.device_id()).is_none());
    assert!(client.paired_pcs().unwrap().is_empty());
}
