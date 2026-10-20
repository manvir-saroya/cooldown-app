//! Negative tests after the handshake: tampering, replay, malformed envelopes and messages a
//! peer must never send.

mod common;

use common::raw::RawSession;
use common::*;
use cooldown_core::AlertIntent;
use cooldown_sync::frame::MsgType;
use cooldown_sync::frame::{encode_envelope, MAX_BODY_LEN};
use cooldown_sync::message::{AckBody, ErrorBody, ErrorCode};
use cooldown_sync::server::ServerEvent;
use cooldown_sync::{ServerError, StaticKeypair};
use std::time::Duration;
use tokio::sync::broadcast;

fn wide(limits: &mut cooldown_sync::Limits) {
    limits.handshakes_per_minute_per_ip = 1000;
    limits.handshakes_per_minute_total = 1000;
}

/// Pair a raw phone and wait until its session is live.
async fn live(
    pc: &Pc,
    events: &mut broadcast::Receiver<ServerEvent>,
) -> (StaticKeypair, RawSession) {
    let kp = StaticKeypair::generate().unwrap();
    let session = raw_pair(pc, events, &kp, "Raw phone").await;
    next_event(events, |e| {
        matches!(e, ServerEvent::DeviceConnected { .. }).then_some(())
    })
    .await;
    (kp, session)
}

/// A fresh authenticated reconnect (handshake plus HELLO exchange).
async fn live_again(pc: &Pc, kp: &StaticKeypair, ts: u64) -> RawSession {
    let mut session = raw_reconnect(pc, kp, ts).await.expect("reconnect");
    session.send_hello(1).await;
    session.recv_until(MsgType::Hello).await.expect("PC HELLO");
    session
}

async fn disconnect_reason(events: &mut broadcast::Receiver<ServerEvent>) -> &'static str {
    next_event(events, |e| match e {
        ServerEvent::DeviceDisconnected { reason, .. } => Some(*reason),
        _ => None,
    })
    .await
}

async fn expect_error(session: &mut RawSession) -> ErrorCode {
    let body = session
        .recv_until(MsgType::Error)
        .await
        .expect("ERROR message");
    let error: ErrorBody = serde_json::from_slice(&body).unwrap();
    // After ERROR the sender closes.
    assert!(
        session.recv().await.is_none(),
        "connection must close after ERROR"
    );
    error.code
}

#[tokio::test]
async fn a_tampered_ciphertext_closes_the_connection_without_a_reply() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut events = pc.server.subscribe();
    let (_kp, mut session) = live(&pc, &mut events).await;
    let mut frame = session.encrypt_frame(MsgType::Ping, br#"{"n":1}"#);
    let last = frame.len() - 1;
    frame[last] ^= 0x80;
    session.raw.write(&frame).await;
    // No ERROR, no PONG: the stream just ends.
    assert!(session.raw.closed_silently().await);
    assert_eq!(disconnect_reason(&mut events).await, "decrypt");
    assert!(pc.server.connected_peers().is_empty());
}

#[tokio::test]
async fn a_tag_stripped_or_truncated_ciphertext_is_rejected() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut events = pc.server.subscribe();
    let (_kp, mut session) = live(&pc, &mut events).await;
    // Fewer bytes than an authentication tag.
    session.send_ciphertext(&[0u8; 8]).await;
    assert!(session.raw.closed_silently().await);
    assert_eq!(disconnect_reason(&mut events).await, "decrypt");
}

#[tokio::test]
async fn a_replayed_transport_frame_is_rejected() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut events = pc.server.subscribe();
    let (_kp, mut session) = live(&pc, &mut events).await;
    let frame = session.encrypt_frame(MsgType::Ping, br#"{"n":1}"#);
    session.raw.write(&frame).await;
    assert!(session.recv_until(MsgType::Pong).await.is_some());
    session.raw.write(&frame).await; // the same bytes again
    assert!(session.raw.closed_silently().await);
    assert_eq!(disconnect_reason(&mut events).await, "decrypt");
}

#[tokio::test]
async fn malformed_envelopes_get_error_protocol_and_a_close() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut events = pc.server.subscribe();
    let (kp, session) = live(&pc, &mut events).await;
    drop(session);

    let ping = encode_envelope(MsgType::Ping, br#"{"n":1}"#).unwrap();
    let mut nonzero_padding = ping.clone();
    let last = nonzero_padding.len() - 1;
    nonzero_padding[last] = 1;
    let mut huge = vec![MsgType::Ping as u8, 0];
    huge.extend_from_slice(&(MAX_BODY_LEN as u16 + 1).to_be_bytes());
    huge.resize(4 + MAX_BODY_LEN + 1, b' ');
    let mut runs_past_end = ping.clone();
    runs_past_end[2..4].copy_from_slice(&5000u16.to_be_bytes());
    let mut flags = ping.clone();
    flags[1] = 1;
    let mut unknown = ping.clone();
    unknown[0] = 0x55;
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("non-zero flags", flags),
        ("unknown type", unknown),
        ("non-zero padding", nonzero_padding),
        ("body over 60 KiB", huge),
        ("body_len past the end", runs_past_end),
        ("shorter than the header", vec![0x06, 0, 0]),
    ];
    for (index, (label, plain)) in cases.into_iter().enumerate() {
        let mut session = live_again(&pc, &kp, 10_000 + index as u64).await;
        session.send_plain(&plain).await;
        assert_eq!(
            expect_error(&mut session).await,
            ErrorCode::Protocol,
            "{label}"
        );
    }
}

#[tokio::test]
async fn bad_json_and_unknown_fields_get_error_malformed() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut events = pc.server.subscribe();
    let (kp, session) = live(&pc, &mut events).await;
    drop(session);
    let cases: [(MsgType, &[u8]); 5] = [
        (MsgType::Ping, b"not json"),
        (MsgType::Ping, br#"{"n":1,"extra":true}"#),
        (MsgType::Ack, br#"{"epoch":1}"#),
        (MsgType::Ack, br#"{"epoch":"1","rev":1}"#),
        (
            MsgType::AlertSet,
            br#"{"op_id":1,"id":"not-an-alert-id","enabled":true}"#,
        ),
    ];
    for (index, (msg_type, body)) in cases.into_iter().enumerate() {
        let mut session = live_again(&pc, &kp, 20_000 + index as u64).await;
        session.send(msg_type, body).await;
        assert_eq!(
            expect_error(&mut session).await,
            ErrorCode::Malformed,
            "case {index}"
        );
    }
}

#[tokio::test]
async fn truncated_and_zero_length_transport_frames_close_the_session() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut events = pc.server.subscribe();
    let (kp, mut session) = live(&pc, &mut events).await;
    // A header promising 100 bytes, 10 delivered, then the stream ends.
    session.raw.write(&100u16.to_be_bytes()).await;
    session.raw.write(&[7u8; 10]).await;
    session.raw.shutdown_write().await;
    assert_eq!(disconnect_reason(&mut events).await, "frame");

    let mut session = live_again(&pc, &kp, 30_000).await;
    session.raw.write(&0u16.to_be_bytes()).await;
    assert!(session.raw.closed_silently().await);
    assert_eq!(disconnect_reason(&mut events).await, "frame");
}

#[tokio::test]
async fn messages_a_phone_must_not_send_get_error_unexpected() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut events = pc.server.subscribe();
    let (kp, session) = live(&pc, &mut events).await;
    drop(session);
    let cases: [(&str, MsgType, &[u8]); 3] = [
        ("SYNC from a phone", MsgType::Sync, b"{}"),
        ("PAIR_OK from a phone", MsgType::PairOk, b"{}"),
        ("a second HELLO", MsgType::Hello, br#"{"app":"cooldown","platform":"android","proto":1,"now_ms":0,"last_epoch":0,"last_rev":0}"#),
    ];
    for (index, (label, msg_type, body)) in cases.into_iter().enumerate() {
        let mut session = live_again(&pc, &kp, 40_000 + index as u64).await;
        session.send(msg_type, body).await;
        assert_eq!(
            expect_error(&mut session).await,
            ErrorCode::Unexpected,
            "{label}"
        );
    }
}

#[tokio::test]
async fn an_ack_for_a_revision_never_sent_is_an_error_but_a_stale_one_is_ignored() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut events = pc.server.subscribe();
    let (_kp, mut session) = live(&pc, &mut events).await;
    let stamp = pc.server.push_sync(&sample_payload(5.0)).unwrap();
    let body = session.recv_until(MsgType::Sync).await.expect("SYNC");
    assert!(String::from_utf8_lossy(&body).contains("\"rev\":1"));
    // A stale acknowledgement is harmless.
    let stale = serde_json::to_vec(&AckBody {
        epoch: stamp.epoch,
        rev: 0,
    })
    .unwrap();
    session.send(MsgType::Ack, &stale).await;
    session.send(MsgType::Ping, br#"{"n":2}"#).await;
    assert!(session.recv_until(MsgType::Pong).await.is_some());
    // One from the future is a protocol violation.
    let future = serde_json::to_vec(&AckBody {
        epoch: stamp.epoch,
        rev: 99,
    })
    .unwrap();
    session.send(MsgType::Ack, &future).await;
    assert_eq!(expect_error(&mut session).await, ErrorCode::Unexpected);
}

#[tokio::test]
async fn alert_toggles_are_applied_once_per_op_id() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut events = pc.server.subscribe();
    let mut alerts = pc.server.take_alert_requests().unwrap();
    assert!(
        pc.server.take_alert_requests().is_none(),
        "only one consumer"
    );
    let (_kp, mut session) = live(&pc, &mut events).await;
    let body = |op: u64, enabled: bool| {
        serde_json::to_vec(&cooldown_sync::message::AlertSetBody {
            op_id: op,
            id: "reset:claude:work:session_5h".into(),
            enabled,
        })
        .unwrap()
    };
    session.send(MsgType::AlertSet, &body(1, false)).await;
    session.send(MsgType::AlertSet, &body(1, false)).await; // a retry of the same operation
    session.send(MsgType::AlertSet, &body(2, true)).await;
    let first = within(alerts.recv()).await.unwrap();
    let second = within(alerts.recv()).await.unwrap();
    assert!(!first.enabled);
    assert!(
        second.enabled,
        "the duplicate op_id was not delivered a second time"
    );
    session.send(MsgType::Ping, br#"{"n":9}"#).await;
    session.recv_until(MsgType::Pong).await.unwrap();
    assert!(alerts.try_recv().is_err());
}

#[tokio::test]
async fn payloads_are_scrubbed_and_size_capped_before_they_leave_the_pc() {
    let clock = fake_clock();
    let pc = start_pc(&clock, wide).await;
    let mut events = pc.server.subscribe();
    let (_kp, mut session) = live(&pc, &mut events).await;

    let mut payload = sample_payload(5.0);
    payload.snapshots[0].note = Some("token was Bearer abc.def".into());
    payload.snapshots[0].account = "me@example.com".into();
    payload.snapshots[0].id = "claude:me@example.com:session_5h".into();
    payload.alerts.clear();
    pc.server.push_sync(&payload).unwrap();
    let body = session.recv_until(MsgType::Sync).await.expect("SYNC");
    let text = String::from_utf8(body).unwrap();
    assert!(!text.contains("me@example.com"), "{text}");
    assert!(!text.contains("Bearer"), "{text}");
    assert!(text.contains("[redacted]"));
    assert!(
        text.contains(&pc.server.device_id().to_hex()),
        "device id is stamped by the server"
    );

    // 200 snapshots with 256-character notes cannot fit in 60 KiB: refused, not truncated, and
    // the revision does not advance.
    let mut big = sample_payload(1.0);
    let template = big.snapshots[0].clone();
    big.alerts.clear();
    big.snapshots = (0..200)
        .map(|i| {
            let mut s = template.clone();
            s.account = format!("a{i}");
            s.id = format!("claude:a{i}:session_5h");
            s.note = Some("n ".repeat(128));
            s
        })
        .collect();
    assert!(matches!(
        pc.server.push_sync(&big),
        Err(ServerError::Payload("payload exceeds 60 KiB"))
    ));
    let next = pc.server.push_sync(&sample_payload(6.0)).unwrap();
    assert_eq!(next.rev, 2);

    // 201 entries are refused outright; so are alert ids core would reject.
    let mut many = sample_payload(1.0);
    many.alerts = (0..201)
        .map(|i| AlertIntent {
            id: format!("reset:claude:a{i}:weekly"),
            fire_at: many.generated_at,
            enabled: true,
        })
        .collect();
    assert!(pc.server.push_sync(&many).is_err());
    let _ = Duration::ZERO;
}
