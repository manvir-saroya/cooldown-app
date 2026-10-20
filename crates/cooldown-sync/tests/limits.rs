//! Deadlines, rate limits, caps, flow control and session lifetime, driven by fake clocks.

mod common;

use std::time::Duration;

use common::raw::{hello, Raw};
use common::*;
use cooldown_sync::frame::MsgType;
use cooldown_sync::hello::Mode;
use cooldown_sync::message::{AckBody, ErrorBody, ErrorCode};
use cooldown_sync::server::ServerEvent;
use cooldown_sync::{CloseReason, ServerError, StaticKeypair};

fn wide(limits: &mut cooldown_sync::Limits) {
    limits.handshakes_per_minute_per_ip = 1000;
    limits.handshakes_per_minute_total = 1000;
}

async fn live(
    pc: &Pc,
    events: &mut tokio::sync::broadcast::Receiver<ServerEvent>,
) -> (StaticKeypair, common::raw::RawSession) {
    let kp = StaticKeypair::generate().unwrap();
    let session = raw_pair(pc, events, &kp, "Raw phone").await;
    next_event(events, |e| {
        matches!(e, ServerEvent::DeviceConnected { .. }).then_some(())
    })
    .await;
    (kp, session)
}

#[tokio::test]
async fn the_hello_deadline_closes_a_silent_connection() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut raw = Raw::connect(pc.addr()).await;
    eventually(|| clock.sleepers() >= 1).await;
    assert_eq!(pc.server.unauthenticated_connections(), 1);
    clock.advance(Duration::from_secs(3)).await; // past the 2 s ClientHello deadline
    assert!(raw.closed_silently().await);
    eventually(|| pc.server.unauthenticated_connections() == 0).await;
}

#[tokio::test]
async fn the_handshake_deadline_closes_a_connection_that_never_sends_message_one() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut raw = Raw::connect(pc.addr()).await;
    raw.write(&hello(Mode::Reconnect, [0; 16]).encode()).await;
    // Wait until the server is waiting on message 1 (the hello timer was replaced).
    settle().await;
    eventually(|| clock.sleepers() >= 1).await;
    clock.advance(Duration::from_secs(6)).await; // past the 5 s handshake deadline
    assert_eq!(raw.read_byte().await, Some(0x10));
    assert_eq!(raw.read_byte().await, None);
}

#[tokio::test]
async fn the_first_message_deadline_closes_a_session_that_never_sends_hello() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut events = pc.server.subscribe();
    let (kp, _session) = live(&pc, &mut events).await;
    let mut silent = raw_reconnect(&pc, &kp, 50_000).await.unwrap();
    settle().await;
    eventually(|| clock.sleepers() >= 1).await;
    clock.advance(Duration::from_secs(11)).await; // past the 10 s first-message deadline
    assert!(silent.recv().await.is_none());
    // The unauthenticated slot was released.
    eventually(|| pc.server.unauthenticated_connections() == 0).await;
}

#[tokio::test]
async fn silence_is_pinged_at_30_seconds_and_closed_at_90() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut events = pc.server.subscribe();
    let (_kp, mut session) = live(&pc, &mut events).await;
    settle().await;
    clock.advance(Duration::from_secs(31)).await;
    let ping = session.recv_until(MsgType::Ping).await;
    assert!(ping.is_some(), "the PC pings after 30 s of silence");
    // Never answer: at 90 s the PC gives up.
    clock.advance(Duration::from_secs(65)).await;
    assert!(session.recv_until(MsgType::Pong).await.is_none());
    let reason = next_event(&mut events, |e| match e {
        ServerEvent::DeviceDisconnected { reason, .. } => Some(*reason),
        _ => None,
    })
    .await;
    assert_eq!(reason, "silence");
}

#[tokio::test]
async fn an_answered_ping_keeps_both_real_peers_alive() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let phone = phone(&clock, "Pixel 8");
    let mut events = pc.server.subscribe();
    let connection = pair(&pc, &phone, &mut events).await;
    // Five minutes of fake time in 20 s steps, letting the sockets run in between.
    for _ in 0..15 {
        clock.advance(Duration::from_secs(20)).await;
        settle().await;
    }
    assert_eq!(connection.state(), cooldown_sync::ConnState::Ready);
    assert_eq!(pc.server.connected_peers().len(), 1);
    pc.server.push_sync(&sample_payload(3.0)).unwrap();
    eventually(|| phone.client.latest_sync().is_some()).await;
}

#[tokio::test]
async fn pushes_are_one_in_flight_two_seconds_apart_and_newest_wins() {
    let clock = fake_clock();
    let pc = start_pc(&clock, |l| {
        wide(l);
        l.min_push_interval = Duration::from_secs(2);
    })
    .await;
    let mut events = pc.server.subscribe();
    let (_kp, mut session) = live(&pc, &mut events).await;

    // Three pushes before the session task runs: only the newest is sent.
    pc.server.push_sync(&sample_payload(1.0)).unwrap(); // rev 1
    pc.server.push_sync(&sample_payload(2.0)).unwrap(); // rev 2
    let stamp = pc.server.push_sync(&sample_payload(3.0)).unwrap(); // rev 3
    let first = session.recv_until(MsgType::Sync).await.unwrap();
    let first: serde_json::Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(first["rev"].as_u64(), Some(3), "newest state wins");
    let rev_of = |value: &serde_json::Value| value["rev"].as_u64().unwrap();
    assert_eq!(rev_of(&first), stamp.rev);

    // A newer state while rev 3 is unacknowledged: at most one SYNC in flight.
    pc.server.push_sync(&sample_payload(4.0)).unwrap(); // rev 4
    let nothing = tokio::time::timeout(Duration::from_millis(200), session.recv()).await;
    assert!(
        nothing.is_err(),
        "a second SYNC while one is unacknowledged"
    );

    // Acknowledged, but the 2 s spacing between pushes still applies.
    let ack = serde_json::to_vec(&AckBody {
        epoch: stamp.epoch,
        rev: 3,
    })
    .unwrap();
    session.send(MsgType::Ack, &ack).await;
    let nothing = tokio::time::timeout(Duration::from_millis(200), session.recv()).await;
    assert!(nothing.is_err(), "pushed again before 2 s passed");
    clock.advance(Duration::from_secs(2)).await;
    let next = session.recv_until(MsgType::Sync).await.unwrap();
    let next: serde_json::Value = serde_json::from_slice(&next).unwrap();
    assert_eq!(rev_of(&next), 4);
}

#[tokio::test]
async fn an_unacknowledged_sync_times_out_with_an_error() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut events = pc.server.subscribe();
    let (_kp, mut session) = live(&pc, &mut events).await;
    pc.server.push_sync(&sample_payload(1.0)).unwrap();
    session.recv_until(MsgType::Sync).await.unwrap();
    settle().await;
    clock.advance(Duration::from_secs(31)).await;
    let body = session.recv_until(MsgType::Error).await.expect("ERROR");
    let error: ErrorBody = serde_json::from_slice(&body).unwrap();
    assert_eq!(error.code, ErrorCode::Timeout);
}

#[tokio::test]
async fn sessions_end_at_the_lifetime_limit_and_reconnect_cleanly() {
    let clock = fake_clock();
    let long = |l: &mut cooldown_sync::Limits| {
        l.session_lifetime = Duration::from_secs(24 * 3600);
        l.ping_after = Duration::from_secs(48 * 3600);
        l.silence_timeout = Duration::from_secs(48 * 3600);
    };
    let pc = start_pc(&clock, |l| {
        wide(l);
        long(l);
    })
    .await;
    let phone = phone_with(&clock, "Pixel 8", long);
    let mut events = pc.server.subscribe();
    let connection = pair(&pc, &phone, &mut events).await;
    let pc_id = connection.pc_device_id();
    settle().await;
    // Not a second early...
    clock.advance(Duration::from_secs(24 * 3600 - 10)).await;
    settle().await;
    assert_eq!(connection.state(), cooldown_sync::ConnState::Ready);
    // ...and gone just after. Both ends hit the limit at the same instant, so the phone sees
    // either its own lifetime check or the PC's close first.
    clock.advance(Duration::from_secs(11)).await;
    let closed = within(connection.closed()).await;
    assert!(
        matches!(closed, CloseReason::Remote("lifetime" | "closed")),
        "{closed:?}"
    );
    let reason = next_event(&mut events, |e| match e {
        ServerEvent::DeviceDisconnected { reason, .. } => Some(*reason),
        _ => None,
    })
    .await;
    assert_eq!(reason, "lifetime");
    // No in-session rekey: the phone simply handshakes again.
    let again = within(phone.client.connect(&pc_id, &[pc.addr()]))
        .await
        .unwrap();
    within(again.wait_ready()).await.unwrap();
}

#[tokio::test]
async fn handshake_floods_are_rate_limited_per_ip_and_refill() {
    let clock = fake_clock();
    // Default limits: 10 per minute per IP.
    let pc = start_pc(&clock, |_| {}).await;
    let valid = hello(Mode::Reconnect, [0; 16]).encode();
    for attempt in 0..10 {
        let mut raw = Raw::connect(pc.addr()).await;
        raw.write(&valid).await;
        raw.write(&0u16.to_be_bytes()).await; // an invalid message 1, so the connection ends fast
        assert_eq!(raw.read_byte().await, Some(0x10), "attempt {attempt}");
        assert_eq!(raw.read_byte().await, None);
    }
    let mut raw = Raw::connect(pc.addr()).await;
    raw.write(&valid).await;
    assert_eq!(
        raw.read_byte().await,
        Some(0x04),
        "the 11th is RATE_LIMITED"
    );
    assert_eq!(raw.read_byte().await, None);
    // One token every 6 seconds.
    clock.advance(Duration::from_secs(7)).await;
    let mut raw = Raw::connect(pc.addr()).await;
    raw.write(&valid).await;
    raw.write(&0u16.to_be_bytes()).await;
    assert_eq!(raw.read_byte().await, Some(0x10));
}

#[tokio::test]
async fn unauthenticated_connections_are_capped_per_ip() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(Raw::connect(pc.addr()).await);
    }
    eventually(|| pc.server.unauthenticated_connections() == 4).await;
    // The fifth from the same address is dropped on the floor.
    let mut fifth = Raw::connect(pc.addr()).await;
    assert!(fifth.closed_silently().await);
    // Freeing a slot lets a new connection in.
    held.pop();
    eventually(|| pc.server.unauthenticated_connections() == 3).await;
    let _sixth = Raw::connect(pc.addr()).await;
    eventually(|| pc.server.unauthenticated_connections() == 4).await;
}

#[tokio::test]
async fn at_most_eight_phones_can_be_paired() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut events = pc.server.subscribe();
    let mut sessions = Vec::new();
    for i in 0..8 {
        let kp = StaticKeypair::generate().unwrap();
        sessions.push(raw_pair(&pc, &mut events, &kp, &format!("Phone {i}")).await);
    }
    assert_eq!(pc.server.peers().unwrap().len(), 8);
    assert!(matches!(
        pc.server.open_pairing(&[]),
        Err(ServerError::PeerLimit)
    ));
    // Revoking one frees a slot.
    let victim = pc.server.peers().unwrap()[0].device_id;
    pc.server.revoke(&victim).unwrap();
    assert!(pc.server.open_pairing(&[]).is_ok());
}

#[tokio::test]
async fn only_one_pairing_window_can_be_open() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let first = pc.server.open_pairing(&[]).unwrap();
    assert!(matches!(
        pc.server.open_pairing(&[]),
        Err(ServerError::PairingAlreadyOpen)
    ));
    pc.server.close_pairing(); // the Hide button
    let second = pc.server.open_pairing(&[]).unwrap();
    assert_ne!(
        *first.qr_text, *second.qr_text,
        "every window has a fresh PSK and pair_id"
    );
}

#[tokio::test]
async fn revoking_rotates_the_beacon_key_and_pushes_it_to_remaining_phones() {
    use cooldown_sync::TrustStore;
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut events = pc.server.subscribe();
    let keeper = phone(&clock, "Keeper");
    let leaver = phone(&clock, "Leaver");
    let keeper_conn = pair(&pc, &keeper, &mut events).await;
    let leaver_conn = pair(&pc, &leaver, &mut events).await;
    let before = pc
        .trust
        .beacon_key()
        .unwrap()
        .expect("a beacon key exists once peers exist");
    let phone_key = |p: &Phone| {
        p.trust
            .get(&pc.server.device_id())
            .unwrap()
            .unwrap()
            .beacon_key
    };
    eventually(|| phone_key(&keeper) == Some(before)).await;
    eventually(|| phone_key(&leaver) == Some(before)).await;

    pc.server.revoke(&leaver.id).unwrap();
    assert_eq!(within(leaver_conn.closed()).await, CloseReason::Revoked);
    let after = pc.trust.beacon_key().unwrap().unwrap();
    assert_ne!(before, after);
    // The remaining phone learns the new key over its existing session; the revoked one does not.
    eventually(|| phone_key(&keeper) == Some(after)).await;
    assert_eq!(keeper_conn.state(), cooldown_sync::ConnState::Ready);
}

#[tokio::test]
async fn staggered_dialing_uses_the_first_address_that_answers() {
    // A real clock on the phone, so the 250 ms stagger actually elapses.
    use cooldown_sync::client::{ClientConfig, PhoneClient};
    use cooldown_sync::{SystemSyncClock, TrustStore};
    use std::sync::Arc;
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let system = PhoneClient::new(
        ClientConfig::new("Pixel 8", "android"),
        StaticKeypair::generate().unwrap(),
        Arc::new(cooldown_sync::MemoryTrustStore::new()) as Arc<dyn TrustStore>,
        Arc::new(SystemSyncClock::new()),
    )
    .unwrap();
    let offer = pc.server.open_pairing(&[]).unwrap();
    let qr = cooldown_sync::QrPayload::decode(&offer.qr_text).unwrap();
    // Nothing answers on port 1: a clean error, not a hang.
    let nothing_listening = cooldown_sync::QrPayload {
        tcp_port: 1,
        ..qr.clone()
    }
    .encode()
    .unwrap();
    assert!(matches!(
        within(system.begin_pairing(&nothing_listening)).await,
        Err(cooldown_sync::ClientError::Connect)
    ));
    // ::1 is not listened on (the server bound IPv4 only), so after the stagger the dialer
    // moves on to 127.0.0.1, and the handshake proves it is the right PC.
    let qr_text = cooldown_sync::QrPayload {
        addrs: vec![
            std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        ],
        ..qr
    }
    .encode()
    .unwrap();
    let pending = within(system.begin_pairing(&qr_text)).await.unwrap();
    assert_eq!(pending.pc_device_id(), pc.server.device_id());
}
