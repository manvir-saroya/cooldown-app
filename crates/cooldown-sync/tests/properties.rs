//! Property tests (proptest): the parsers never panic on arbitrary bytes, round trips hold, and
//! the idempotency and scrubbing invariants hold for arbitrary inputs.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use cooldown_core::{Snapshot, SnapshotSource, SourceKind, SyncPayload, Unit, WindowKind};
use cooldown_sync::beacon::{Beacon, BeaconKind, BEACON_LEN};
use cooldown_sync::codec::{b64_decode_fixed, b64_encode, base32_decode, base32_encode};
use cooldown_sync::frame::{
    decode_envelope, decode_frame, encode_envelope, encode_frame, FrameReader, MsgType,
    MAX_BODY_LEN,
};
use cooldown_sync::hello::{ClientHello, Mode};
use cooldown_sync::keys::is_small_order;
use cooldown_sync::sanitize::{looks_secret, sanitize_payload, REDACTED};
use cooldown_sync::BeaconError;
use cooldown_sync::{should_apply, DeviceId, MemoryTrustStore, PeerRecord, QrPayload, TrustStore};
use proptest::prelude::*;
use time::OffsetDateTime;
use zeroize::Zeroizing;

// ---------------------------------------------------------------------------------------------
// QR payload
// ---------------------------------------------------------------------------------------------

fn dialable(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !(v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast()),
        IpAddr::V6(v6) => {
            !(v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6.to_ipv4_mapped().is_some())
        }
    }
}

fn ip_strategy() -> impl Strategy<Value = IpAddr> {
    prop_oneof![
        any::<[u8; 4]>().prop_map(|o| IpAddr::V4(Ipv4Addr::from(o))),
        any::<[u8; 16]>().prop_map(|o| IpAddr::V6(Ipv6Addr::from(o))),
    ]
}

fn qr_strategy() -> impl Strategy<Value = QrPayload> {
    (
        any::<[u8; 32]>(),
        any::<[u8; 32]>(),
        any::<[u8; 16]>(),
        1u16..=u16::MAX,
        proptest::collection::vec(ip_strategy(), 1..=4),
    )
        .prop_filter_map("valid payload", |(key, psk, pair_id, port, addrs)| {
            let mut seen = HashSet::new();
            let addrs: Vec<IpAddr> = addrs
                .into_iter()
                .filter(dialable)
                .filter(|a| seen.insert(*a))
                .collect();
            (!addrs.is_empty() && !is_small_order(&key)).then_some(QrPayload {
                pc_static_pub: key,
                psk: Zeroizing::new(psk),
                pair_id,
                tcp_port: port,
                addrs,
            })
        })
}

proptest! {
    #[test]
    fn qr_round_trips(payload in qr_strategy()) {
        let text = payload.encode().unwrap();
        prop_assert!(text.len() <= cooldown_sync::qr::MAX_TEXT_LEN);
        prop_assert_eq!(QrPayload::decode(&text).unwrap(), payload);
    }

    /// Whatever the input, decoding returns a value or an error, and a value re-encodes to exactly
    /// the text it came from: there is one spelling per payload.
    #[test]
    fn qr_decode_never_panics_and_is_canonical(text in "\\PC{0,300}") {
        if let Ok(payload) = QrPayload::decode(&text) {
            prop_assert_eq!(&*payload.encode().unwrap(), text.as_str());
        }
    }

    #[test]
    fn qr_decode_of_prefixed_noise_is_canonical(body in "[A-Z2-7]{0,260}") {
        let text = format!("CD1:{body}");
        if let Ok(payload) = QrPayload::decode(&text) {
            prop_assert_eq!(&*payload.encode().unwrap(), text.as_str());
        }
    }

    #[test]
    fn qr_binary_decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
        let _ = QrPayload::decode_bytes(&bytes);
    }

    /// Changing one character of a valid payload either fails or yields a payload whose canonical
    /// text is the changed text.
    #[test]
    fn qr_single_character_mutations(
        payload in qr_strategy(),
        position in 0usize..300,
        replacement in "[A-Z2-7]",
    ) {
        let text = payload.encode().unwrap().to_string();
        let index = 4 + position % (text.len() - 4);
        let mut chars: Vec<char> = text.chars().collect();
        chars[index] = replacement.chars().next().unwrap();
        let mutated: String = chars.into_iter().collect();
        if let Ok(decoded) = QrPayload::decode(&mutated) {
            prop_assert_eq!(&*decoded.encode().unwrap(), mutated.as_str());
        }
    }

    #[test]
    fn base32_and_base64_round_trip(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
        prop_assert_eq!(base32_decode(&base32_encode(&bytes)).unwrap(), bytes.clone());
        if bytes.len() == 32 {
            let key: [u8; 32] = bytes.as_slice().try_into().unwrap();
            prop_assert_eq!(b64_decode_fixed::<32>(&b64_encode(&key)), Some(key));
        }
    }

    #[test]
    fn base32_decode_never_panics_and_is_canonical(text in "\\PC{0,64}") {
        if let Ok(bytes) = base32_decode(&text) {
            prop_assert_eq!(base32_encode(&bytes), text);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Discovery beacons
// ---------------------------------------------------------------------------------------------

/// Any beacon this crate can build: an ANNOUNCE with a non-zero port, or a PROBE.
fn beacon_strategy() -> impl Strategy<Value = Beacon> {
    prop_oneof![
        (1u16..=u16::MAX, any::<[u8; 8]>())
            .prop_map(|(port, nonce)| Beacon::announce(port, nonce).unwrap()),
        any::<[u8; 8]>().prop_map(Beacon::probe),
    ]
}

proptest! {
    #[test]
    fn beacons_round_trip(beacon in beacon_strategy(), key in any::<[u8; 32]>()) {
        let datagram = beacon.encode(&key);
        prop_assert_eq!(datagram.len(), BEACON_LEN);
        prop_assert_eq!(Beacon::decode(&datagram, &key), Ok(beacon));
    }

    /// Arbitrary bytes under an arbitrary key: an error or a value, never a panic, and a value
    /// re-encodes to exactly the bytes it came from (one spelling per beacon). Random input all
    /// but never carries a valid tag, so this mostly exercises the rejection paths.
    #[test]
    fn beacon_decode_never_panics_and_is_canonical(
        bytes in proptest::collection::vec(any::<u8>(), 0..80),
        key in any::<[u8; 32]>(),
    ) {
        if let Ok(beacon) = Beacon::decode(&bytes, &key) {
            let again = beacon.encode(&key);
            prop_assert_eq!(again.as_slice(), bytes.as_slice());
        }
    }

    /// Starting from a header whose magic and version are right, the verdict is fully determined:
    /// a port that does not match the kind is a port error, and otherwise a random tag does not
    /// verify (it would take a 2^-128 coincidence), so the tag comparison is reached every time.
    #[test]
    fn beacon_decode_of_well_formed_headers_is_decided_by_port_then_tag(
        kind in prop_oneof![Just(0x01u8), Just(0x02u8)],
        port in any::<u16>(),
        nonce in any::<[u8; 8]>(),
        tag in any::<[u8; 16]>(),
        key in any::<[u8; 32]>(),
    ) {
        let mut datagram = Vec::from(*b"CDBC");
        datagram.extend_from_slice(&[1, kind]);
        datagram.extend_from_slice(&port.to_be_bytes());
        datagram.extend_from_slice(&nonce);
        datagram.extend_from_slice(&tag);
        let port_matches_kind = (kind == 0x01) == (port != 0);
        let expected = if port_matches_kind { BeaconError::Tag } else { BeaconError::Port };
        prop_assert_eq!(Beacon::decode(&datagram, &key), Err(expected));
    }

    /// Flipping any one bit of a valid datagram, or using any other key, is rejected.
    #[test]
    fn a_mutated_or_foreign_beacon_is_rejected(
        beacon in beacon_strategy(),
        key in any::<[u8; 32]>(),
        other in any::<[u8; 32]>(),
        bit in 0usize..(BEACON_LEN * 8),
    ) {
        let datagram = beacon.encode(&key);
        let mut mutated = datagram;
        mutated[bit / 8] ^= 1 << (bit % 8);
        prop_assert!(Beacon::decode(&mutated, &key).is_err());
        if other != key {
            prop_assert_eq!(Beacon::decode(&datagram, &other), Err(BeaconError::Tag));
        }
    }

    /// Replaying an ANNOUNCE with a different port never works, whatever the new port.
    #[test]
    fn a_replayed_announce_with_another_port_is_rejected(
        port in 1u16..=u16::MAX,
        new_port in any::<u16>(),
        nonce in any::<[u8; 8]>(),
        key in any::<[u8; 32]>(),
    ) {
        prop_assume!(new_port != port);
        let mut datagram = Beacon::announce(port, nonce).unwrap().encode(&key);
        datagram[6..8].copy_from_slice(&new_port.to_be_bytes());
        let expected = if new_port == 0 { BeaconError::Port } else { BeaconError::Tag };
        prop_assert_eq!(Beacon::decode(&datagram, &key), Err(expected));
    }

    /// Anything shorter or longer than 32 bytes is a length error, before anything else is read.
    #[test]
    fn beacons_of_the_wrong_length_are_rejected(
        beacon in beacon_strategy(),
        key in any::<[u8; 32]>(),
        cut in 0usize..BEACON_LEN,
        extra in proptest::collection::vec(any::<u8>(), 1..40),
    ) {
        let datagram = beacon.encode(&key);
        prop_assert_eq!(Beacon::decode(&datagram[..cut], &key), Err(BeaconError::Length));
        let mut longer = datagram.to_vec();
        longer.extend_from_slice(&extra);
        prop_assert_eq!(Beacon::decode(&longer, &key), Err(BeaconError::Length));
    }

    /// The kind and port always travel together in the way the document says.
    #[test]
    fn beacon_kind_and_port_agree(beacon in beacon_strategy()) {
        match beacon.kind() {
            BeaconKind::Probe => prop_assert_eq!(beacon.tcp_port(), 0),
            BeaconKind::Announce => prop_assert!(beacon.tcp_port() != 0),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Frames, envelopes, hello
// ---------------------------------------------------------------------------------------------

proptest! {
    #[test]
    fn frame_round_trip_with_trailing_bytes(
        payload in proptest::collection::vec(any::<u8>(), 1..3000),
        trailing in proptest::collection::vec(any::<u8>(), 0..50),
    ) {
        let mut bytes = encode_frame(&payload).unwrap();
        let used = bytes.len();
        bytes.extend_from_slice(&trailing);
        let (body, consumed) = decode_frame(&bytes, 65535).unwrap().unwrap();
        prop_assert_eq!(body, payload.as_slice());
        prop_assert_eq!(consumed, used);
    }

    #[test]
    fn frame_decoder_never_panics_and_is_consistent(
        bytes in proptest::collection::vec(any::<u8>(), 0..600),
        max in 1usize..=65535,
    ) {
        match decode_frame(&bytes, max) {
            Ok(Some((body, used))) => {
                prop_assert!(!body.is_empty() && body.len() <= max);
                prop_assert_eq!(used, 2 + body.len());
                prop_assert_eq!(body, &bytes[2..used]);
            }
            Ok(None) => prop_assert!(bytes.len() < 2 + max.min(65535) + 1),
            Err(_) => {}
        }
    }

    /// The streaming reader agrees with the slice decoder however the bytes are chunked.
    #[test]
    fn streaming_reader_matches_the_slice_decoder(
        payloads in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 1..300), 1..6),
        chunk in 1usize..40,
    ) {
        let mut stream = Vec::new();
        for payload in &payloads {
            stream.extend(encode_frame(payload).unwrap());
        }
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let got = runtime.block_on(async {
            let (mut tx, rx) = tokio::io::duplex(64);
            let writer = tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                for piece in stream.chunks(chunk) {
                    tx.write_all(piece).await.unwrap();
                }
            });
            let mut reader = FrameReader::new(rx, 1000);
            let mut out = Vec::new();
            while let Ok(frame) = reader.read_frame().await {
                out.push(frame);
            }
            writer.await.unwrap();
            out
        });
        prop_assert_eq!(got, payloads);
    }

    #[test]
    fn envelope_round_trip_and_padding(
        type_index in 0usize..9,
        body in proptest::collection::vec(any::<u8>(), 0..3000),
    ) {
        let types = [
            MsgType::Hello, MsgType::PairOk, MsgType::Sync, MsgType::Ack, MsgType::AlertSet,
            MsgType::Ping, MsgType::Pong, MsgType::Revoke, MsgType::Error,
        ];
        let msg_type = types[type_index];
        let plain = encode_envelope(msg_type, &body).unwrap();
        let unit = if msg_type == MsgType::Sync { 1024 } else { 64 };
        prop_assert_eq!(plain.len() % unit, 0);
        let (decoded_type, decoded_body) = decode_envelope(&plain).unwrap();
        prop_assert_eq!(decoded_type, msg_type);
        prop_assert_eq!(decoded_body, body.as_slice());
        // Any non-zero byte in the padding is refused.
        if plain.len() > 4 + body.len() {
            for index in [4 + body.len(), plain.len() - 1] {
                let mut bad = plain.clone();
                bad[index] = 1;
                prop_assert!(decode_envelope(&bad).is_err());
            }
        }
    }

    #[test]
    fn envelope_decoder_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..70000)) {
        if let Ok((_, body)) = decode_envelope(&bytes) {
            prop_assert!(body.len() <= MAX_BODY_LEN);
        }
    }

    #[test]
    fn client_hello_decoder_never_panics_and_is_canonical(
        bytes in proptest::collection::vec(any::<u8>(), 0..40),
    ) {
        if let Ok(hello) = ClientHello::decode(&bytes) {
            let encoded = hello.encode();
            prop_assert_eq!(encoded.as_slice(), bytes.as_slice());
        }
    }

    #[test]
    fn client_hello_round_trip(
        min in 1u8..=5, extra in 0u8..=5, pair in any::<bool>(), id in any::<[u8; 16]>(),
    ) {
        prop_assume!(!pair || id.iter().any(|&b| b != 0));
        let hello = ClientHello {
            proto_min: min,
            proto_max: min + extra,
            mode: if pair { Mode::Pair } else { Mode::Reconnect },
            pair_id: if pair { id } else { [0; 16] },
        };
        prop_assert_eq!(ClientHello::decode(&hello.encode()).unwrap(), hello);
    }
}

// ---------------------------------------------------------------------------------------------
// Idempotency
// ---------------------------------------------------------------------------------------------

fn apply(have: Option<(u64, u64)>, epoch: u64, rev: u64) -> Option<(u64, u64)> {
    if should_apply(have, epoch, rev) {
        Some((epoch, rev))
    } else {
        have
    }
}

proptest! {
    #[test]
    fn applying_the_same_sync_twice_is_the_same_as_once(
        have in proptest::option::of((0u64..5, 0u64..5)),
        epoch in 0u64..5,
        rev in 0u64..5,
    ) {
        let once = apply(have, epoch, rev);
        prop_assert_eq!(apply(once, epoch, rev), once);
    }

    /// Within an epoch the held revision never goes backwards, whatever order SYNCs arrive in.
    #[test]
    fn revisions_only_move_forward_within_an_epoch(
        syncs in proptest::collection::vec((0u64..3, 0u64..10), 0..40),
    ) {
        let mut have: Option<(u64, u64)> = None;
        for (epoch, rev) in syncs {
            let next = apply(have, epoch, rev);
            if let (Some((e0, r0)), Some((e1, r1))) = (have, next) {
                if e0 == e1 {
                    prop_assert!(r1 >= r0);
                }
            }
            have = next;
        }
    }

    /// The server accepts a reconnect timestamp exactly when it is higher than every earlier
    /// accepted one: replays and reordering can never be accepted.
    #[test]
    fn reconnect_timestamps_are_strictly_increasing(
        timestamps in proptest::collection::vec(0u64..50, 0..60),
    ) {
        let store = MemoryTrustStore::new();
        let record = PeerRecord::new([5; 32], "p".into(), "android".into(), 0, 0, 1);
        let id = record.device_id;
        store.upsert(record).unwrap();
        let mut highest = 0u64;
        for ts in timestamps {
            let accepted = store.advance_ts(&id, ts).unwrap();
            prop_assert_eq!(accepted, ts > highest);
            if accepted {
                highest = ts;
            }
            prop_assert_eq!(store.get(&id).unwrap().unwrap().last_ts, highest);
        }
    }

    /// The phone's reserved timestamps are strictly increasing even if its clock goes backwards.
    #[test]
    fn reserved_timestamps_never_repeat(clock in proptest::collection::vec(0i64..1000, 1..40)) {
        let store = MemoryTrustStore::new();
        let record = PeerRecord::new([6; 32], "pc".into(), "windows".into(), 0, 0, 1);
        let id = record.device_id;
        store.upsert(record).unwrap();
        let mut last = 0u64;
        for now in clock {
            let ts = store.reserve_ts(&id, now).unwrap().unwrap();
            prop_assert!(ts > last);
            last = ts;
        }
        prop_assert_eq!(DeviceId::from_public(&[6; 32]), id);
    }
}

// ---------------------------------------------------------------------------------------------
// Scrubber
// ---------------------------------------------------------------------------------------------

fn payload_with(account: &str, note: &str) -> SyncPayload {
    let mut snapshot = Snapshot::new(
        "claude",
        account,
        WindowKind::Weekly,
        Unit::Percent,
        SnapshotSource {
            kind: SourceKind::Manual,
            adapter: "manual".into(),
            adapter_version: "1".into(),
        },
        OffsetDateTime::UNIX_EPOCH,
    );
    snapshot.percent_used = Some(1.0);
    snapshot.note = Some(note.to_owned());
    SyncPayload {
        schema_version: 1,
        device_id: "0123456789abcdef0123456789abcdef".into(),
        generated_at: OffsetDateTime::UNIX_EPOCH,
        snapshots: vec![snapshot],
        alerts: vec![],
    }
}

proptest! {
    /// Whatever labels and notes go in, nothing that matches a secret pattern comes out, and
    /// sanitising again changes nothing.
    #[test]
    fn serializer_output_never_matches_a_scrubber_pattern(
        account in "\\PC{0,80}",
        note in "\\PC{0,400}",
    ) {
        let input = payload_with(&account, &note);
        if let Ok(clean) = sanitize_payload(&input) {
            for snapshot in &clean.snapshots {
                prop_assert!(snapshot.account == REDACTED || !looks_secret(&snapshot.account));
                let expected_id = format!("claude:{}:weekly", snapshot.account);
                prop_assert_eq!(&snapshot.id, &expected_id);
                if let Some(note) = &snapshot.note {
                    prop_assert!(note == REDACTED || !looks_secret(note));
                    prop_assert!(note.chars().count() <= 256);
                }
            }
            prop_assert_eq!(sanitize_payload(&clean).unwrap(), clean);
        }
    }

    #[test]
    fn known_secret_shapes_in_notes_are_always_redacted(
        prefix in "[a-z ]{0,10}",
        secret in prop_oneof![
            "sk-[A-Za-z0-9]{8,40}",
            "ghp_[A-Za-z0-9]{8,40}",
            "[A-Za-z0-9+/]{32,60}",
            "[a-z]{3,8}@[a-z]{3,8}\\.com",
            "Bearer [A-Za-z0-9]{4,20}",
        ],
    ) {
        let note = format!("{prefix} {secret}");
        let clean = sanitize_payload(&payload_with("work", &note)).unwrap();
        prop_assert_eq!(clean.snapshots[0].note.as_deref(), Some(REDACTED));
    }
}

// ---------------------------------------------------------------------------------------------
// Messages and Noise
// ---------------------------------------------------------------------------------------------

proptest! {
    #[test]
    fn message_bodies_never_panic_on_arbitrary_bytes(
        bytes in proptest::collection::vec(any::<u8>(), 0..2000),
    ) {
        use cooldown_sync::message::*;
        let _ = serde_json::from_slice::<HelloBody>(&bytes);
        let _ = serde_json::from_slice::<SyncBody>(&bytes);
        let _ = serde_json::from_slice::<AckBody>(&bytes);
        let _ = serde_json::from_slice::<AlertSetBody>(&bytes);
        let _ = serde_json::from_slice::<PingBody>(&bytes);
        let _ = serde_json::from_slice::<RevokeBody>(&bytes);
        let _ = serde_json::from_slice::<ErrorBody>(&bytes);
        let _ = serde_json::from_slice::<PairMsg1>(&bytes);
        let _ = serde_json::from_slice::<ReconnectMsg1>(&bytes);
        let _ = serde_json::from_slice::<Msg2>(&bytes);
    }

    #[test]
    fn transport_decrypt_rejects_arbitrary_bytes_without_panicking(
        bytes in proptest::collection::vec(any::<u8>(), 0..400),
    ) {
        use cooldown_sync::noise::{prologue, Handshake, HandshakeParams};
        use cooldown_sync::StaticKeypair;
        let pc = StaticKeypair::generate().unwrap();
        let phone = StaticKeypair::generate().unwrap();
        let hello = ClientHello { proto_min: 1, proto_max: 1, mode: Mode::Reconnect, pair_id: [0; 16] };
        let pro = prologue(&hello.encode(), &pc.device_id());
        let mut init = Handshake::initiator(&HandshakeParams {
            mode: Mode::Reconnect, local: &phone, remote_public: Some(pc.public()), psk: None, prologue: &pro,
        }).unwrap();
        let resp = Handshake::responder(&HandshakeParams {
            mode: Mode::Reconnect, local: &pc, remote_public: None, psk: None, prologue: &pro,
        }).unwrap();
        let m1 = init.write(b"{}").unwrap();
        let (mut resp, _) = resp.read(&m1).unwrap();
        let m2 = resp.write(b"{}").unwrap();
        let (init, _) = init.read(&m2).unwrap();
        let mut init = init.finish().unwrap();
        let mut resp = resp.finish().unwrap();
        prop_assert!(resp.transport.decrypt(&bytes).is_err());
        // Garbage did not poison the channel for a legitimate message (snow does not advance the
        // nonce on failure); production code closes anyway.
        let ct = init.transport.encrypt(b"ok").unwrap();
        prop_assert_eq!(resp.transport.decrypt(&ct).unwrap(), b"ok".to_vec());
    }

    /// The handshake reader consumes garbage as an error and never panics.
    #[test]
    fn handshake_read_rejects_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..600)) {
        use cooldown_sync::noise::{prologue, Handshake, HandshakeParams};
        use cooldown_sync::StaticKeypair;
        let pc = StaticKeypair::generate().unwrap();
        let hello = ClientHello { proto_min: 1, proto_max: 1, mode: Mode::Pair, pair_id: [1; 16] };
        let pro = prologue(&hello.encode(), &pc.device_id());
        let psk = [9u8; 32];
        let resp = Handshake::responder(&HandshakeParams {
            mode: Mode::Pair, local: &pc, remote_public: None, psk: Some(&psk), prologue: &pro,
        }).unwrap();
        prop_assert!(resp.read(&bytes).is_err());
    }
}
