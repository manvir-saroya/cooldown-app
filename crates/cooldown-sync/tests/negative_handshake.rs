//! Negative tests for the handshake: every way a first pairing or a reconnect must fail.

mod common;

use common::raw::{handshake, hello, pair_payload, reconnect_payload, start, Raw};
use common::*;
use cooldown_sync::frame::MsgType;
use cooldown_sync::hello::{ClientHello, Mode};
use cooldown_sync::message::{ErrorBody, ErrorCode};
use cooldown_sync::server::{ServerEvent, WindowClosed};
use cooldown_sync::{ClientError, StaticKeypair, TrustStore};
use std::time::Duration;

const VERSION: u8 = 0x01;
const NOT_PAIRING: u8 = 0x02;
const BUSY: u8 = 0x03;
const FAIL: u8 = 0x10;

fn wide_limits(limits: &mut cooldown_sync::Limits) {
    limits.handshakes_per_minute_per_ip = 1000;
    limits.handshakes_per_minute_total = 1000;
}

#[tokio::test]
async fn wrong_psk_gets_a_generic_fail_and_nothing_is_revealed() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let mut events = pc.server.subscribe();
    let offer = pc.server.open_pairing(&[]).unwrap();
    let qr = cooldown_sync::QrPayload::decode(&offer.qr_text).unwrap();

    let attacker = StaticKeypair::generate().unwrap();
    let started = start(
        Mode::Pair,
        &attacker,
        &qr.pc_static_pub,
        Some(&[0x11; 32]),
        qr.pair_id,
        &pair_payload("Mallory", 1),
        None,
    );
    let mut raw = Raw::connect(pc.addr()).await;
    raw.write(&started.wire()).await;
    // One status byte and then the connection closes: no message 2, no key, no detail.
    assert_eq!(raw.read_byte().await, Some(FAIL));
    assert_eq!(raw.read_byte().await, None);
    settle().await;
    assert!(!drain(&mut events)
        .iter()
        .any(|e| matches!(e, ServerEvent::PairingConfirmationRequested { .. })));

    // One failure does not burn the window: the real phone still pairs with the same QR.
    let phone = phone(&clock, "Pixel 8");
    let connection = pair_using(&pc, &phone, &mut events, &offer.qr_text).await;
    drop(connection);
}

#[tokio::test]
async fn wrong_pair_id_is_not_pairing_and_never_burns_the_window() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let mut events = pc.server.subscribe();
    let offer = pc.server.open_pairing(&[]).unwrap();
    let qr = cooldown_sync::QrPayload::decode(&offer.qr_text).unwrap();
    let attacker = StaticKeypair::generate().unwrap();
    for i in 0..8u8 {
        let started = start(
            Mode::Pair,
            &attacker,
            &qr.pc_static_pub,
            Some(&*qr.psk),
            [0xA0 + i; 16],
            &pair_payload("Mallory", 1),
            None,
        );
        let result = handshake(Raw::connect(pc.addr()).await, started).await;
        assert_eq!(result.err(), Some(Some(NOT_PAIRING)));
    }
    let phone = phone(&clock, "Pixel 8");
    pair_using(&pc, &phone, &mut events, &offer.qr_text).await;
}

#[tokio::test]
async fn an_expired_window_answers_not_pairing() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let mut events = pc.server.subscribe();
    let qr = open_window(&pc);
    // Just before expiry the window still works.
    clock.advance(Duration::from_secs(119)).await;
    // (A probe with a wrong PSK consumes one failure but proves the window is still open.)
    let kp = StaticKeypair::generate().unwrap();
    let probe = start(
        Mode::Pair,
        &kp,
        &qr.pc_static_pub,
        Some(&[1; 32]),
        qr.pair_id,
        &pair_payload("x", 1),
        None,
    );
    assert_eq!(
        handshake(Raw::connect(pc.addr()).await, probe).await.err(),
        Some(Some(FAIL))
    );
    clock.advance(Duration::from_secs(2)).await;
    next_event(&mut events, |e| {
        matches!(e, ServerEvent::PairingWindowClosed(WindowClosed::Expired)).then_some(())
    })
    .await;
    let started = start(
        Mode::Pair,
        &kp,
        &qr.pc_static_pub,
        Some(&*qr.psk),
        qr.pair_id,
        &pair_payload("x", 1),
        None,
    );
    assert_eq!(
        handshake(Raw::connect(pc.addr()).await, started)
            .await
            .err(),
        Some(Some(NOT_PAIRING))
    );
}

#[tokio::test]
async fn five_failures_burn_the_window_and_even_the_right_psk_is_refused() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let mut events = pc.server.subscribe();
    let qr = open_window(&pc);
    let kp = StaticKeypair::generate().unwrap();
    for _ in 0..5 {
        let started = start(
            Mode::Pair,
            &kp,
            &qr.pc_static_pub,
            Some(&[2; 32]),
            qr.pair_id,
            &pair_payload("x", 1),
            None,
        );
        assert_eq!(
            handshake(Raw::connect(pc.addr()).await, started)
                .await
                .err(),
            Some(Some(FAIL))
        );
    }
    next_event(&mut events, |e| {
        matches!(e, ServerEvent::PairingWindowClosed(WindowClosed::Burned)).then_some(())
    })
    .await;
    // The 6th attempt carries the correct PSK and is still refused: the window is burned.
    let started = start(
        Mode::Pair,
        &kp,
        &qr.pc_static_pub,
        Some(&*qr.psk),
        qr.pair_id,
        &pair_payload("x", 1),
        None,
    );
    assert_eq!(
        handshake(Raw::connect(pc.addr()).await, started)
            .await
            .err(),
        Some(Some(NOT_PAIRING))
    );
    // A fresh window works again (a new PSK and pair_id).
    let phone = phone(&clock, "Pixel 8");
    pair(&pc, &phone, &mut events).await;
}

#[tokio::test]
async fn a_second_phone_gets_busy_while_one_is_awaiting_confirmation_and_the_psk_is_single_use() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let mut events = pc.server.subscribe();
    let qr = open_window(&pc);
    let first = StaticKeypair::generate().unwrap();
    let started = start(
        Mode::Pair,
        &first,
        &qr.pc_static_pub,
        Some(&*qr.psk),
        qr.pair_id,
        &pair_payload("First", 1),
        None,
    );
    let mut session = handshake(Raw::connect(pc.addr()).await, started)
        .await
        .unwrap();
    session.send_hello(1).await;
    next_event(&mut events, |e| {
        matches!(e, ServerEvent::PairingConfirmationRequested { .. }).then_some(())
    })
    .await;

    // The right PSK, but the window is locked.
    let second = StaticKeypair::generate().unwrap();
    let started = start(
        Mode::Pair,
        &second,
        &qr.pc_static_pub,
        Some(&*qr.psk),
        qr.pair_id,
        &pair_payload("Second", 1),
        None,
    );
    assert_eq!(
        handshake(Raw::connect(pc.addr()).await, started)
            .await
            .err(),
        Some(Some(BUSY))
    );

    // The user rejects: the first phone is told, nothing is stored, and the window is burned.
    pc.server.confirm_pairing(false).unwrap();
    let body = session.recv_until(MsgType::Revoke).await.expect("REVOKE");
    assert!(String::from_utf8_lossy(&body).contains("rejected"));
    assert!(pc.server.peers().unwrap().is_empty());
    next_event(&mut events, |e| {
        matches!(e, ServerEvent::PairingFailed { .. }).then_some(())
    })
    .await;
    let started = start(
        Mode::Pair,
        &second,
        &qr.pc_static_pub,
        Some(&*qr.psk),
        qr.pair_id,
        &pair_payload("Second", 1),
        None,
    );
    assert_eq!(
        handshake(Raw::connect(pc.addr()).await, started)
            .await
            .err(),
        Some(Some(NOT_PAIRING))
    );
}

#[tokio::test]
async fn a_pair_attempt_without_an_open_window_is_not_pairing_before_any_dh() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let kp = StaticKeypair::generate().unwrap();
    let started = start(
        Mode::Pair,
        &kp,
        &pc.keypair_pub,
        Some(&[3; 32]),
        [9; 16],
        &pair_payload("x", 1),
        None,
    );
    // Send only the hello: the answer arrives without message 1, so no DH can have happened.
    let mut raw = Raw::connect(pc.addr()).await;
    raw.write(&started.hello_bytes).await;
    assert_eq!(raw.read_byte().await, Some(NOT_PAIRING));
    assert_eq!(raw.read_byte().await, None);
}

#[tokio::test]
async fn wrong_user_confirmation_code_rejects_the_pairing_and_stores_nothing() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let mut events = pc.server.subscribe();
    let phone = phone(&clock, "Pixel 8");
    let offer = pc.server.open_pairing(&[]).unwrap();
    let pending = within(phone.client.begin_pairing(&offer.qr_text))
        .await
        .unwrap();
    let shown = next_event(&mut events, |e| match e {
        ServerEvent::PairingConfirmationRequested { code, .. } => Some(code.clone()),
        _ => None,
    })
    .await;
    assert_eq!(shown, pending.code());
    // The user types a code that is not the one on their phone.
    let wrong: String = if shown.starts_with("000") {
        "999 999"
    } else {
        "000 000"
    }
    .into();
    assert!(matches!(
        pc.server.confirm_pairing_with_code(&wrong),
        Err(cooldown_sync::ServerError::WrongCode)
    ));
    assert!(matches!(
        within(pending.wait_for_confirmation()).await,
        Err(ClientError::PairingRefused)
    ));
    assert!(pc.server.peers().unwrap().is_empty());
    assert!(phone.client.paired_pcs().unwrap().is_empty());
    assert!(pc.trust.beacon_key().unwrap().is_none());
    // The window was burned by the rejection.
    assert!(matches!(
        phone.client.begin_pairing(&offer.qr_text).await,
        Err(ClientError::NotPairing)
    ));
}

#[tokio::test]
async fn the_right_typed_code_is_accepted_with_or_without_the_space() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let mut events = pc.server.subscribe();
    let phone = phone(&clock, "Pixel 8");
    let offer = pc.server.open_pairing(&[]).unwrap();
    let pending = within(phone.client.begin_pairing(&offer.qr_text))
        .await
        .unwrap();
    next_event(&mut events, |e| {
        matches!(e, ServerEvent::PairingConfirmationRequested { .. }).then_some(())
    })
    .await;
    let typed = pending.code().replace(' ', "");
    pc.server.confirm_pairing_with_code(&typed).unwrap();
    within(pending.wait_for_confirmation()).await.unwrap();
    assert_eq!(pc.server.peers().unwrap().len(), 1);
}

#[tokio::test]
async fn unconfirmed_pairing_times_out_with_a_revoke_and_stores_nothing() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let mut events = pc.server.subscribe();
    let phone = phone(&clock, "Pixel 8");
    let offer = pc.server.open_pairing(&[]).unwrap();
    let pending = within(phone.client.begin_pairing(&offer.qr_text))
        .await
        .unwrap();
    next_event(&mut events, |e| {
        matches!(e, ServerEvent::PairingConfirmationRequested { .. }).then_some(())
    })
    .await;
    // Wait for the confirmation timer to be registered, then let 60 s pass without a click.
    eventually(|| clock.sleepers() >= 2).await;
    clock.advance(Duration::from_secs(61)).await;
    let result = within(pending.wait_for_confirmation()).await;
    assert!(
        matches!(result, Err(ClientError::PairingRefused)),
        "{result:?}"
    );
    let reason = next_event(&mut events, |e| match e {
        ServerEvent::PairingFailed { reason } => Some(*reason),
        _ => None,
    })
    .await;
    assert_eq!(reason, "timeout");
    assert!(pc.server.peers().unwrap().is_empty());
    assert!(phone.client.paired_pcs().unwrap().is_empty());
}

#[tokio::test]
async fn replayed_first_messages_are_rejected_for_equal_and_lower_timestamps() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let mut events = pc.server.subscribe();
    let kp = StaticKeypair::generate().unwrap();
    let pair_session = raw_pair(&pc, &mut events, &kp, "Raw phone").await;
    drop(pair_session);

    // A legitimate reconnect at ts=5000 (pairing used 1000).
    let started = start(
        Mode::Reconnect,
        &kp,
        &pc.keypair_pub,
        None,
        [0; 16],
        &reconnect_payload(5000),
        None,
    );
    let wire = started.wire();
    let mut session = handshake(Raw::connect(pc.addr()).await, started)
        .await
        .unwrap();
    session.send_hello(1).await;
    session.recv_until(MsgType::Hello).await.expect("PC HELLO");
    drop(session);

    // The captured message 1, replayed verbatim.
    let mut replay = Raw::connect(pc.addr()).await;
    replay.write(&wire).await;
    assert_eq!(replay.read_byte().await, Some(FAIL));
    assert_eq!(replay.read_byte().await, None);
    // A lower timestamp, and the pairing-time one, fail too.
    for ts in [4999, 1000, 1] {
        assert_eq!(
            raw_reconnect(&pc, &kp, ts).await.err(),
            Some(Some(FAIL)),
            "ts {ts}"
        );
    }
    // A higher one is fine.
    assert!(raw_reconnect(&pc, &kp, 5001).await.is_ok());
}

#[tokio::test]
async fn a_replayed_message_one_cannot_evict_a_live_session() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let mut events = pc.server.subscribe();
    let kp = StaticKeypair::generate().unwrap();
    let mut live = raw_pair(&pc, &mut events, &kp, "Raw phone").await;
    next_event(&mut events, |e| {
        matches!(e, ServerEvent::DeviceConnected { .. }).then_some(())
    })
    .await;
    assert_eq!(pc.server.connected_peers().len(), 1);

    // Someone completes a handshake with a higher timestamp (as a delayed original message 1
    // would) but never sends the first transport message, which only the key holder can.
    let mut pending = raw_reconnect(&pc, &kp, 9_000).await.unwrap();
    settle().await;
    assert!(!drain(&mut events)
        .iter()
        .any(|e| matches!(e, ServerEvent::PossibleClone { .. })));
    // The live session still works.
    live.send(MsgType::Ping, br#"{"n":7}"#).await;
    assert!(live.recv_until(MsgType::Pong).await.is_some());

    // Only when the first transport message arrives does the new session replace the old one,
    // and the PC flags it as a possible clone.
    pending.send_hello(1).await;
    next_event(&mut events, |e| {
        matches!(e, ServerEvent::PossibleClone { .. }).then_some(())
    })
    .await;
    assert!(live.recv_until(MsgType::Pong).await.is_none());
    assert_eq!(pc.server.connected_peers().len(), 1);
}

#[tokio::test]
async fn unknown_phone_and_wrong_pc_key_get_the_same_generic_fail() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let stranger = StaticKeypair::generate().unwrap();
    assert_eq!(
        raw_reconnect(&pc, &stranger, 10).await.err(),
        Some(Some(FAIL))
    );
    // The phone believes the PC has a different static key.
    let wrong_pc = *StaticKeypair::generate().unwrap().public();
    let started = start(
        Mode::Reconnect,
        &stranger,
        &wrong_pc,
        None,
        [0; 16],
        &reconnect_payload(10),
        None,
    );
    // The prologue also differs (it names the PC's id), and either way the answer is identical.
    assert_eq!(
        handshake(Raw::connect(pc.addr()).await, started)
            .await
            .err(),
        Some(Some(FAIL))
    );
}

#[tokio::test]
async fn a_revoked_device_cannot_reconnect_and_learns_nothing_specific() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let mut events = pc.server.subscribe();
    let kp = StaticKeypair::generate().unwrap();
    let session = raw_pair(&pc, &mut events, &kp, "Raw phone").await;
    drop(session);
    assert!(raw_reconnect(&pc, &kp, 2000).await.is_ok());
    pc.server.revoke(&kp.device_id()).unwrap();
    let result = raw_reconnect(&pc, &kp, 3000).await;
    assert_eq!(result.err(), Some(Some(FAIL)));
    // The same bytes an unknown phone would get: nothing distinguishes "revoked" from "never".
    let stranger = StaticKeypair::generate().unwrap();
    assert_eq!(
        raw_reconnect(&pc, &stranger, 3000).await.err(),
        Some(Some(FAIL))
    );
}

#[tokio::test]
async fn a_revoked_client_reports_rejected_but_keeps_its_record() {
    // The phone's record only goes away on an authenticated REVOKE. A bare FAIL byte (which anyone
    // on the path can forge) must never unpair.
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let mut events = pc.server.subscribe();
    let phone = phone(&clock, "Pixel 8");
    let connection = pair(&pc, &phone, &mut events).await;
    let pc_id = connection.pc_device_id();
    connection.close();
    within(connection.closed()).await;
    // Revoked while the phone is offline: the PC forgets the phone, the phone never hears.
    pc.server.revoke(&phone.id).unwrap();
    let result = phone.client.connect(&pc_id, &[pc.addr()]).await;
    assert!(
        matches!(result, Err(ClientError::Rejected)),
        "{:?}",
        result.err()
    );
    assert_eq!(
        phone.client.paired_pcs().unwrap().len(),
        1,
        "FAIL must not unpair"
    );
}

#[tokio::test]
async fn version_mismatch_is_reported_and_downgrades_are_caught_by_the_prologue() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let kp = StaticKeypair::generate().unwrap();

    // A hello that only offers versions 2 to 3: VERSION with the server's maximum, then close.
    let mut raw = Raw::connect(pc.addr()).await;
    let mut offer = hello(Mode::Reconnect, [0; 16]);
    offer.proto_min = 2;
    offer.proto_max = 3;
    raw.write(&offer.encode()).await;
    assert_eq!(raw.read_byte().await, Some(VERSION));
    assert_eq!(raw.read_byte().await, Some(1));
    assert_eq!(raw.read_byte().await, None);

    // proto_min of zero or min > max is malformed: silent close.
    for (min, max) in [(0u8, 1u8), (3, 2)] {
        let mut raw = Raw::connect(pc.addr()).await;
        let mut bad = hello(Mode::Reconnect, [0; 16]).encode();
        bad[4] = min;
        bad[5] = max;
        raw.write(&bad).await;
        assert!(raw.closed_silently().await);
    }

    // An on-path attacker raises proto_max on the wire while the phone's Noise prologue says
    // [1, 1]. The prologues differ, so message 1 fails: the offer cannot be altered.
    let started = start(
        Mode::Reconnect,
        &kp,
        &pc.keypair_pub,
        None,
        [0; 16],
        &reconnect_payload(5),
        Some(ClientHello {
            proto_min: 1,
            proto_max: 1,
            mode: Mode::Reconnect,
            pair_id: [0; 16],
        }),
    );
    let mut wire = started.wire();
    wire[5] = 2; // proto_max on the wire
    let mut raw = Raw::connect(pc.addr()).await;
    raw.write(&wire).await;
    assert_eq!(raw.read_byte().await, Some(FAIL));
}

#[tokio::test]
async fn bad_magic_and_http_probes_are_closed_silently() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    for probe in [
        &b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"[..],
        &b"RSBX0000000000000000000000000000"[..],
    ] {
        let mut raw = Raw::connect(pc.addr()).await;
        raw.write(probe).await;
        assert!(raw.closed_silently().await);
    }
    // Non-zero flags and a wrong mode are malformed too.
    for (index, value) in [(7usize, 1u8), (6, 9)] {
        let mut bytes = hello(Mode::Reconnect, [0; 16]).encode();
        bytes[index] = value;
        let mut raw = Raw::connect(pc.addr()).await;
        raw.write(&bytes).await;
        assert!(raw.closed_silently().await);
    }
}

#[tokio::test]
async fn truncated_zero_and_oversized_handshake_frames_fail_closed() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let kp = StaticKeypair::generate().unwrap();
    let started = start(
        Mode::Reconnect,
        &kp,
        &pc.keypair_pub,
        None,
        [0; 16],
        &reconnect_payload(5),
        None,
    );
    let hello_bytes = started.hello_bytes;

    // A truncated hello (23 of 24 bytes) then EOF.
    let mut raw = Raw::connect(pc.addr()).await;
    raw.write(&hello_bytes[..23]).await;
    raw.shutdown_write().await;
    assert!(raw.closed_silently().await);

    // Hello, then a frame header promising 100 bytes of which 10 arrive, then EOF.
    let mut raw = Raw::connect(pc.addr()).await;
    raw.write(&hello_bytes).await;
    raw.write(&100u16.to_be_bytes()).await;
    raw.write(&[0u8; 10]).await;
    raw.shutdown_write().await;
    assert_eq!(raw.read_byte().await, Some(FAIL));

    // A zero-length frame.
    let mut raw = Raw::connect(pc.addr()).await;
    raw.write(&hello_bytes).await;
    raw.write(&0u16.to_be_bytes()).await;
    assert_eq!(raw.read_byte().await, Some(FAIL));

    // 513 bytes: over the handshake cap even though the Noise limit is 65535. The cap is applied
    // from the header, before the body arrives.
    let mut raw = Raw::connect(pc.addr()).await;
    raw.write(&hello_bytes).await;
    raw.write(&513u16.to_be_bytes()).await;
    assert_eq!(raw.read_byte().await, Some(FAIL));
    // And the largest legal frame header is refused the same way.
    let mut raw = Raw::connect(pc.addr()).await;
    raw.write(&hello_bytes).await;
    raw.write(&65535u16.to_be_bytes()).await;
    assert_eq!(raw.read_byte().await, Some(FAIL));
}

#[tokio::test]
async fn a_forged_status_never_changes_what_the_phone_stores() {
    // A fake "PC" that answers every connection with a forged VERSION or FAIL byte.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let mut events = pc.server.subscribe();
    let phone = phone(&clock, "Pixel 8");
    let connection = pair(&pc, &phone, &mut events).await;
    let pc_id = connection.pc_device_id();
    connection.close();
    within(connection.closed()).await;
    let before = phone.client.paired_pcs().unwrap();

    for reply in [vec![0x01u8, 0x00], vec![0x10], vec![0x01, 0xFF], vec![0x55]] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let fake = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut hello = [0u8; 24];
            stream.read_exact(&mut hello).await.unwrap();
            stream.write_all(&reply).await.unwrap();
            let mut sink = Vec::new();
            let _ = stream.read_to_end(&mut sink).await;
        });
        let result = within(phone.client.connect(&pc_id, &[addr])).await;
        assert!(result.is_err());
        fake.abort();
    }
    let after = phone.client.paired_pcs().unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(
        after[0].proto_floor, before[0].proto_floor,
        "the floor never moves on a bare byte"
    );
    assert_eq!(after[0].static_pub, before[0].static_pub);
}

#[tokio::test]
async fn the_first_transport_message_must_be_hello() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide_limits).await;
    let mut events = pc.server.subscribe();
    let kp = StaticKeypair::generate().unwrap();
    drop(raw_pair(&pc, &mut events, &kp, "Raw phone").await);

    let mut session = raw_reconnect(&pc, &kp, 4000).await.unwrap();
    session.send(MsgType::Ping, br#"{"n":1}"#).await;
    let body = session.recv_until(MsgType::Error).await.expect("ERROR");
    let error: ErrorBody = serde_json::from_slice(&body).unwrap();
    assert_eq!(error.code, ErrorCode::Unexpected);
    assert!(session.recv().await.is_none());
    assert!(pc.server.connected_peers().is_empty());

    // A HELLO for the wrong protocol version is refused inside the encrypted channel too.
    let mut session = raw_reconnect(&pc, &kp, 4001).await.unwrap();
    session.send_hello(2).await;
    let body = session.recv_until(MsgType::Error).await.expect("ERROR");
    let error: ErrorBody = serde_json::from_slice(&body).unwrap();
    assert_eq!(error.code, ErrorCode::Version);
}
