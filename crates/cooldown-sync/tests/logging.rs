//! "No logging of payloads or keys": run real flows under a capturing `tracing` subscriber and
//! check that nothing secret, nothing identifying and no payload text appears in any log line.

mod common;

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::raw::{handshake, pair_payload, start, Raw};
use common::*;
use cooldown_sync::codec::{b64_encode, base32_encode, hex_encode};
use cooldown_sync::hello::Mode;
use cooldown_sync::server::ServerEvent;
use cooldown_sync::{QrPayload, StaticKeypair, TrustStore};
use tracing::field::{Field, Visit};
use tracing::{span, Event, Metadata, Subscriber};

#[derive(Clone, Default)]
struct Capture {
    lines: Arc<Mutex<Vec<String>>>,
}

struct Line(String);

impl Visit for Line {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, "{}={:?} ", field.name(), value);
    }
}

impl Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }
    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut line = Line(format!("{} ", event.metadata().target()));
        event.record(&mut line);
        self.lines.lock().unwrap().push(line.0);
    }
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
}

#[tokio::test]
async fn logs_contain_event_codes_only_never_keys_payloads_names_or_codes() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());

    let clock = fake_clock();
    let pc = start_pc(&clock, |l| {
        l.handshakes_per_minute_per_ip = 1000;
        l.handshakes_per_minute_total = 1000;
    })
    .await;
    let mut events = pc.server.subscribe();
    let phone = phone(&clock, "Secret Phone Name");

    // A pairing, a sync with distinctive text, and a revocation.
    let offer = pc.server.open_pairing(&[]).unwrap();
    let qr_text = offer.qr_text.to_string();
    let qr = QrPayload::decode(&qr_text).unwrap();
    let pending = within(phone.client.begin_pairing(&qr_text)).await.unwrap();
    let shown_code = next_event(&mut events, |e| match e {
        ServerEvent::PairingConfirmationRequested { code, .. } => Some(code.clone()),
        _ => None,
    })
    .await;
    pc.server.confirm_pairing(true).unwrap();
    let connection = within(pending.wait_for_confirmation()).await.unwrap();
    within(connection.wait_ready()).await.unwrap();
    let mut payload = sample_payload(42.0);
    payload.snapshots[0].note = Some("TOPSECRETNOTE-1234".into());
    pc.server.push_sync(&payload).unwrap();
    eventually(|| phone.client.latest_sync().is_some()).await;
    let beacon = pc.trust.beacon_key().unwrap().unwrap();

    // Hostile traffic against a second window: a wrong PSK, garbage, an unknown phone.
    let qr_text_2 = pc.server.open_pairing(&[]).unwrap().qr_text.to_string();
    let qr2 = QrPayload::decode(&qr_text_2).unwrap();
    let attacker = StaticKeypair::generate().unwrap();
    let wrong_psk = [0x5A; 32];
    for _ in 0..2 {
        let started = start(
            Mode::Pair,
            &attacker,
            &qr2.pc_static_pub,
            Some(&wrong_psk),
            qr2.pair_id,
            &pair_payload("Mallory Name", 1),
            None,
        );
        let _ = handshake(Raw::connect(pc.addr()).await, started).await;
    }
    let mut junk = Raw::connect(pc.addr()).await;
    junk.write(b"GET /secret HTTP/1.1\r\n\r\n").await;
    junk.closed_silently().await;
    let _ = raw_reconnect(&pc, &attacker, 77).await;
    pc.server.revoke(&phone.id).unwrap();
    within(connection.closed()).await;
    settle().await;
    clock.advance(Duration::from_secs(1)).await;

    let log = capture.lines.lock().unwrap().join("\n");
    if std::env::var_os("COOLDOWN_SHOW_LOG").is_some() {
        println!("{log}");
    }
    assert!(
        log.contains("device_connected")
            || log.contains("not_pairing")
            || log.contains("msg1_auth"),
        "the capture should have seen events, got:\n{log}"
    );

    let mut needles: Vec<(&str, String)> = vec![
        ("QR text", qr_text.clone()),
        ("second QR text", qr_text_2.clone()),
        ("second PSK hex", hex_encode(&*qr2.psk)),
        ("second pair id hex", hex_encode(&qr2.pair_id)),
        ("QR body", qr_text[4..].to_owned()),
        ("PSK hex", hex_encode(&*qr.psk)),
        ("PSK base64", b64_encode(&*qr.psk)),
        ("PSK base32", base32_encode(&*qr.psk)),
        ("wrong PSK hex", hex_encode(&wrong_psk)),
        ("PC private hex", hex_encode(&pc.keypair_private)),
        ("PC private base64", b64_encode(&pc.keypair_private)),
        ("phone private hex", hex_encode(&phone.private)),
        ("phone private base64", b64_encode(&phone.private)),
        ("PC public hex", hex_encode(&pc.keypair_pub)),
        ("phone public hex", hex_encode(&phone.public)),
        ("beacon key hex", hex_encode(&beacon)),
        ("beacon key base64", b64_encode(&beacon)),
        ("pair id hex", hex_encode(&qr.pair_id)),
        ("confirmation code", shown_code.clone()),
        ("confirmation code digits", shown_code.replace(' ', "")),
        ("phone name", "Secret Phone Name".to_owned()),
        ("attacker name", "Mallory Name".to_owned()),
        ("PC name", "Test PC".to_owned()),
        ("note text", "TOPSECRETNOTE".to_owned()),
        ("account label", "work".to_owned()),
        ("full phone device id", phone.id.to_hex()),
        ("full PC device id", pc.server.device_id().to_hex()),
        ("request text", "/secret".to_owned()),
    ];
    needles.retain(|(_, text)| !text.is_empty());
    for (what, needle) in &needles {
        assert!(
            !log.contains(needle.as_str()),
            "log leaks the {what}:\n{log}"
        );
    }
    // Device ids appear only as 4-byte (8 hex character) prefixes, and never beyond that.
    assert!(
        !log.contains(&phone.id.to_hex()[..12]),
        "log shows more than a 4-byte id prefix"
    );
}
