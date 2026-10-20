//! The Noise layer: a thin wrapper over `snow` that fixes the two protocol names, the prologue and
//! the confirmation code, and structurally enforces the `snow` handling rules from the protocol
//! document.
//!
//! * A failed [`Handshake::read`] consumes the state, so a handshake that errored can never be
//!   retried (snow restores a checkpoint on error, which would otherwise let an attacker probe).
//! * Only the stateful `TransportState` is used. There is no rekey, no manual nonce and no raw
//!   split anywhere in this crate, and `fixed_ephemeral_key_for_testing_only` appears only in test
//!   code.
//! * Every message length is checked against the Noise 65535-byte limit before snow sees it, and
//!   all buffers are sized explicitly.
//! * Snow does not depend on `zeroize`, so its internal copies of keys are not wiped; only the
//!   copies this crate owns are. See the README.

use blake2::{Blake2s256, Digest};

use crate::hello::{Mode, CLIENT_HELLO_LEN};
use crate::keys::{DeviceId, PublicKey, StaticKeypair};

/// First-pairing protocol name.
pub const NOISE_PARAMS_PAIR: &str = "Noise_IKpsk1_25519_ChaChaPoly_BLAKE2s";
/// Reconnect protocol name.
pub const NOISE_PARAMS_RECONNECT: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
/// Prologue domain separator. A new major protocol version means a new string.
pub const PROLOGUE_TAG: &[u8; 17] = b"cooldown/noise/v1";
/// Prologue length: tag, `ClientHello` and the PC's device id.
pub const PROLOGUE_LEN: usize = PROLOGUE_TAG.len() + CLIENT_HELLO_LEN + 16;
/// Largest Noise message, including the 16-byte tag.
pub const MAX_NOISE_MESSAGE: usize = 65535;
/// Authentication tag length of ChaChaPoly.
pub const TAG_LEN: usize = 16;
/// Handshake messages are capped far below the Noise limit.
pub const MAX_HANDSHAKE_MESSAGE: usize = 512;

/// A Noise failure with no detail beyond its class (snow's own errors must not be shown to peers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NoiseError {
    /// Could not build the handshake (missing key, bad parameters).
    #[error("handshake setup failed")]
    Setup,
    /// A message was out of order for the pattern.
    #[error("handshake state error")]
    State,
    /// Authentication failed or the message was malformed.
    #[error("decryption failed")]
    Decrypt,
    /// A message is longer than allowed.
    #[error("message too long")]
    TooLong,
    /// A message is shorter than the minimum.
    #[error("message too short")]
    TooShort,
}

/// The prologue that both sides feed to Noise: `"cooldown/noise/v1" || ClientHello || pc_device_id`.
pub fn prologue(hello: &[u8; CLIENT_HELLO_LEN], pc: &DeviceId) -> [u8; PROLOGUE_LEN] {
    let mut out = [0u8; PROLOGUE_LEN];
    let tag = PROLOGUE_TAG.len();
    out[..tag].copy_from_slice(PROLOGUE_TAG);
    out[tag..tag + CLIENT_HELLO_LEN].copy_from_slice(hello);
    out[tag + CLIENT_HELLO_LEN..].copy_from_slice(pc.as_bytes());
    out
}

/// The six-digit confirmation code from the handshake hash `h`:
/// `u64_be(BLAKE2s-256("cooldown-sas-v1" || h)[0..8]) mod 10^6`.
pub fn confirmation_code(handshake_hash: &[u8]) -> u32 {
    let mut hasher = Blake2s256::new();
    hasher.update(b"cooldown-sas-v1");
    hasher.update(handshake_hash);
    let digest = hasher.finalize();
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(head) % 1_000_000) as u32
}

/// Format a confirmation code the way it is shown: `123 456`.
pub fn format_confirmation_code(code: u32) -> String {
    let code = code % 1_000_000;
    format!("{:03} {:03}", code / 1000, code % 1000)
}

/// What a handshake is built from.
pub struct HandshakeParams<'a> {
    /// PAIR uses `IKpsk1` and needs `psk`; RECONNECT uses `IK` and must not have one.
    pub mode: Mode,
    /// This side's static key pair.
    pub local: &'a StaticKeypair,
    /// The responder's static key. Required for the initiator, ignored for the responder.
    pub remote_public: Option<&'a PublicKey>,
    /// The pairing PSK (PAIR only).
    pub psk: Option<&'a [u8; 32]>,
    /// From [`prologue`].
    pub prologue: &'a [u8],
}

fn builder<'a>(p: &'a HandshakeParams<'a>) -> Result<snow::Builder<'a>, NoiseError> {
    let name = match (p.mode, p.psk.is_some()) {
        (Mode::Pair, true) => NOISE_PARAMS_PAIR,
        (Mode::Reconnect, false) => NOISE_PARAMS_RECONNECT,
        _ => return Err(NoiseError::Setup),
    };
    let params = name.parse().map_err(|_| NoiseError::Setup)?;
    let mut builder = snow::Builder::new(params)
        .local_private_key(p.local.private())
        .map_err(|_| NoiseError::Setup)?
        .prologue(p.prologue)
        .map_err(|_| NoiseError::Setup)?;
    if let Some(remote) = p.remote_public {
        builder = builder
            .remote_public_key(remote)
            .map_err(|_| NoiseError::Setup)?;
    }
    if let Some(psk) = p.psk {
        builder = builder.psk(1, psk).map_err(|_| NoiseError::Setup)?;
    }
    Ok(builder)
}

/// A Noise handshake in progress (two messages for both patterns).
pub struct Handshake {
    state: snow::HandshakeState,
}

impl Handshake {
    /// The side that sends message 1 (the phone). Needs `remote_public`.
    pub fn initiator(params: &HandshakeParams<'_>) -> Result<Self, NoiseError> {
        if params.remote_public.is_none() {
            return Err(NoiseError::Setup);
        }
        let state = builder(params)?
            .build_initiator()
            .map_err(|_| NoiseError::Setup)?;
        Ok(Handshake { state })
    }

    /// The side that reads message 1 (the PC).
    pub fn responder(params: &HandshakeParams<'_>) -> Result<Self, NoiseError> {
        let state = builder(params)?
            .build_responder()
            .map_err(|_| NoiseError::Setup)?;
        Ok(Handshake { state })
    }

    /// Write the next handshake message carrying `payload` (encrypted by the pattern).
    pub fn write(&mut self, payload: &[u8]) -> Result<Vec<u8>, NoiseError> {
        let mut out = vec![0u8; MAX_HANDSHAKE_MESSAGE];
        let len = self
            .state
            .write_message(payload, &mut out)
            .map_err(|_| NoiseError::State)?;
        out.truncate(len);
        Ok(out)
    }

    /// Read the next handshake message. **Consumes the state**: on `Err` the handshake is gone and
    /// the connection must be closed, so a failed read can never be retried.
    pub fn read(mut self, message: &[u8]) -> Result<(Self, Vec<u8>), NoiseError> {
        if message.len() > MAX_NOISE_MESSAGE {
            return Err(NoiseError::TooLong);
        }
        let mut payload = vec![0u8; message.len()];
        match self.state.read_message(message, &mut payload) {
            Ok(len) => {
                payload.truncate(len);
                Ok((self, payload))
            }
            Err(_) => Err(NoiseError::Decrypt),
        }
    }

    /// The peer's static key, once the pattern has delivered it (the responder after message 1).
    pub fn remote_static(&self) -> Option<PublicKey> {
        let bytes = self.state.get_remote_static()?;
        <PublicKey>::try_from(bytes).ok()
    }

    /// Leave the handshake: capture the handshake hash `h` and the peer's static key, then switch
    /// to transport mode.
    pub fn finish(self) -> Result<Established, NoiseError> {
        if !self.state.is_handshake_finished() {
            return Err(NoiseError::State);
        }
        let mut handshake_hash = [0u8; 32];
        let hash = self.state.get_handshake_hash();
        if hash.len() != 32 {
            return Err(NoiseError::State);
        }
        handshake_hash.copy_from_slice(hash);
        let remote_static = self
            .state
            .get_remote_static()
            .and_then(|bytes| <PublicKey>::try_from(bytes).ok())
            .ok_or(NoiseError::State)?;
        let state = self
            .state
            .into_transport_mode()
            .map_err(|_| NoiseError::State)?;
        Ok(Established {
            transport: Transport {
                state,
                sent: 0,
                received: 0,
            },
            handshake_hash,
            remote_static,
        })
    }
}

/// A finished handshake.
pub struct Established {
    /// The encrypted channel.
    pub transport: Transport,
    /// The handshake hash `h`, read before switching to transport mode. Input to the code.
    pub handshake_hash: [u8; 32],
    /// The peer's authenticated static key.
    pub remote_static: PublicKey,
}

/// The encrypted channel. Messages are strictly ordered by Noise nonces; any `Err` from
/// [`Transport::decrypt`] means the connection must be closed (never resynchronise).
pub struct Transport {
    state: snow::TransportState,
    sent: u64,
    received: u64,
}

impl Transport {
    /// Encrypt one message (`plaintext` up to 65519 bytes).
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, NoiseError> {
        if plaintext.len() + TAG_LEN > MAX_NOISE_MESSAGE {
            return Err(NoiseError::TooLong);
        }
        let mut out = vec![0u8; plaintext.len() + TAG_LEN];
        let len = self
            .state
            .write_message(plaintext, &mut out)
            .map_err(|_| NoiseError::State)?;
        out.truncate(len);
        self.sent += 1;
        Ok(out)
    }

    /// Encrypt one message and prepend its 2-byte big-endian length, producing the exact bytes of a
    /// frame in one allocation (one copy fewer than encrypting and then framing).
    pub fn encrypt_framed(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, NoiseError> {
        if plaintext.len() + TAG_LEN > MAX_NOISE_MESSAGE {
            return Err(NoiseError::TooLong);
        }
        let mut out = vec![0u8; 2 + plaintext.len() + TAG_LEN];
        let len = self
            .state
            .write_message(plaintext, &mut out[2..])
            .map_err(|_| NoiseError::State)?;
        out.truncate(2 + len);
        let header = u16::try_from(len).map_err(|_| NoiseError::TooLong)?;
        out[..2].copy_from_slice(&header.to_be_bytes());
        self.sent += 1;
        Ok(out)
    }

    /// Decrypt one message. On `Err` close the connection.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, NoiseError> {
        if ciphertext.len() > MAX_NOISE_MESSAGE {
            return Err(NoiseError::TooLong);
        }
        if ciphertext.len() < TAG_LEN {
            return Err(NoiseError::TooShort);
        }
        let mut out = vec![0u8; ciphertext.len()];
        let len = self
            .state
            .read_message(ciphertext, &mut out)
            .map_err(|_| NoiseError::Decrypt)?;
        out.truncate(len);
        self.received += 1;
        Ok(out)
    }

    /// Messages encrypted so far.
    pub fn messages_sent(&self) -> u64 {
        self.sent
    }

    /// Messages successfully decrypted so far.
    pub fn messages_received(&self) -> u64 {
        self.received
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hello::ClientHello;
    use zeroize::Zeroizing;

    struct Pair {
        pc: StaticKeypair,
        phone: StaticKeypair,
        hello: ClientHello,
        psk: Zeroizing<[u8; 32]>,
    }

    fn pair(mode: Mode) -> Pair {
        Pair {
            pc: StaticKeypair::generate().unwrap(),
            phone: StaticKeypair::generate().unwrap(),
            hello: ClientHello {
                proto_min: 1,
                proto_max: 1,
                mode,
                pair_id: if mode == Mode::Pair { [7; 16] } else { [0; 16] },
            },
            psk: Zeroizing::new([0x42; 32]),
        }
    }

    fn run(
        p: &Pair,
        responder_psk: Option<&[u8; 32]>,
        responder_prologue_hello: &ClientHello,
    ) -> Result<(Established, Established, Vec<u8>, Vec<u8>), NoiseError> {
        let mode = p.hello.mode;
        let pc_id = p.pc.device_id();
        let init_pro = prologue(&p.hello.encode(), &pc_id);
        let resp_pro = prologue(&responder_prologue_hello.encode(), &pc_id);
        let psk = (mode == Mode::Pair).then_some(&*p.psk);
        let mut init = Handshake::initiator(&HandshakeParams {
            mode,
            local: &p.phone,
            remote_public: Some(p.pc.public()),
            psk,
            prologue: &init_pro,
        })?;
        let resp = Handshake::responder(&HandshakeParams {
            mode,
            local: &p.pc,
            remote_public: None,
            psk: responder_psk,
            prologue: &resp_pro,
        })?;
        let msg1 = init.write(b"one")?;
        let (mut resp, got1) = resp.read(&msg1)?;
        let msg2 = resp.write(b"two")?;
        let (init, got2) = init.read(&msg2)?;
        Ok((init.finish()?, resp.finish()?, got1, got2))
    }

    #[test]
    fn prologue_layout_is_tag_hello_device_id() {
        let p = pair(Mode::Pair);
        let id = p.pc.device_id();
        let pro = prologue(&p.hello.encode(), &id);
        assert_eq!(pro.len(), 57);
        assert_eq!(&pro[..17], b"cooldown/noise/v1");
        assert_eq!(&pro[17..41], &p.hello.encode());
        assert_eq!(&pro[41..], id.as_bytes());
    }

    #[test]
    fn ikpsk1_round_trip_and_matching_codes() {
        let p = pair(Mode::Pair);
        let (mut init, mut resp, got1, got2) = run(&p, Some(&*p.psk), &p.hello).unwrap();
        assert_eq!(
            (got1.as_slice(), got2.as_slice()),
            (&b"one"[..], &b"two"[..])
        );
        assert_eq!(init.handshake_hash, resp.handshake_hash);
        assert_eq!(
            confirmation_code(&init.handshake_hash),
            confirmation_code(&resp.handshake_hash)
        );
        assert_eq!(&init.remote_static, p.pc.public());
        assert_eq!(&resp.remote_static, p.phone.public());
        let ct = init.transport.encrypt(b"hello").unwrap();
        assert_eq!(resp.transport.decrypt(&ct).unwrap(), b"hello");
        let ct = resp.transport.encrypt(b"back").unwrap();
        assert_eq!(init.transport.decrypt(&ct).unwrap(), b"back");
    }

    #[test]
    fn ik_round_trip() {
        let p = pair(Mode::Reconnect);
        let (init, resp, ..) = run(&p, None, &p.hello).unwrap();
        assert_eq!(init.handshake_hash, resp.handshake_hash);
    }

    #[test]
    fn wrong_psk_fails_at_message_one() {
        let p = pair(Mode::Pair);
        let wrong = [0x43u8; 32];
        assert_eq!(
            run(&p, Some(&wrong), &p.hello).err(),
            Some(NoiseError::Decrypt)
        );
    }

    #[test]
    fn tampered_hello_fails_at_message_one() {
        let p = pair(Mode::Pair);
        let mut lowered = p.hello;
        lowered.proto_max = 2;
        assert_eq!(
            run(&p, Some(&*p.psk), &lowered).err(),
            Some(NoiseError::Decrypt)
        );
        let mut other_pair_id = p.hello;
        other_pair_id.pair_id = [8; 16];
        assert_eq!(
            run(&p, Some(&*p.psk), &other_pair_id).err(),
            Some(NoiseError::Decrypt)
        );
    }

    #[test]
    fn mode_and_psk_must_agree() {
        let p = pair(Mode::Reconnect);
        let pro = prologue(&p.hello.encode(), &p.pc.device_id());
        let result = Handshake::responder(&HandshakeParams {
            mode: Mode::Reconnect,
            local: &p.pc,
            remote_public: None,
            psk: Some(&*p.psk),
            prologue: &pro,
        });
        assert!(result.is_err());
        let result = Handshake::initiator(&HandshakeParams {
            mode: Mode::Pair,
            local: &p.phone,
            remote_public: Some(p.pc.public()),
            psk: None,
            prologue: &pro,
        });
        assert!(result.is_err());
    }

    #[test]
    fn tampered_and_replayed_transport_messages_fail() {
        let p = pair(Mode::Reconnect);
        let (mut init, mut resp, ..) = run(&p, None, &p.hello).unwrap();
        let ct = init.transport.encrypt(b"payload").unwrap();
        let mut bad = ct.clone();
        bad[0] ^= 1;
        assert_eq!(resp.transport.decrypt(&bad), Err(NoiseError::Decrypt));
        // Snow does not advance the nonce on failure, so the untouched message still decrypts here;
        // the production loops never rely on that and close on the first error.
        assert_eq!(resp.transport.decrypt(&ct).unwrap(), b"payload");
        // A replay of an already accepted message is rejected.
        assert_eq!(resp.transport.decrypt(&ct), Err(NoiseError::Decrypt));
        assert_eq!(resp.transport.decrypt(&[0u8; 5]), Err(NoiseError::TooShort));
    }

    #[test]
    fn framed_encryption_matches_encrypt_then_frame() {
        let p = pair(Mode::Reconnect);
        let (mut a, mut b, ..) = run(&p, None, &p.hello).unwrap();
        let framed = a.transport.encrypt_framed(b"hello there").unwrap();
        let len = usize::from(u16::from_be_bytes([framed[0], framed[1]]));
        assert_eq!(len, framed.len() - 2);
        assert_eq!(b.transport.decrypt(&framed[2..]).unwrap(), b"hello there");
        assert_eq!(
            a.transport.encrypt_framed(&vec![0u8; MAX_NOISE_MESSAGE]),
            Err(NoiseError::TooLong)
        );
    }

    #[test]
    fn length_limits_are_enforced_before_snow() {
        let p = pair(Mode::Reconnect);
        let (mut init, ..) = run(&p, None, &p.hello).unwrap();
        assert_eq!(
            init.transport.encrypt(&vec![0u8; MAX_NOISE_MESSAGE]),
            Err(NoiseError::TooLong)
        );
        assert!(init
            .transport
            .encrypt(&vec![0u8; MAX_NOISE_MESSAGE - TAG_LEN])
            .is_ok());
        assert_eq!(
            init.transport.decrypt(&vec![0u8; MAX_NOISE_MESSAGE + 1]),
            Err(NoiseError::TooLong)
        );
    }

    #[test]
    fn confirmation_code_is_six_digits() {
        assert_eq!(format_confirmation_code(123_456), "123 456");
        assert_eq!(format_confirmation_code(7), "000 007");
        for byte in 0..=255u8 {
            assert!(confirmation_code(&[byte; 32]) < 1_000_000);
        }
    }

    #[test]
    fn confirmation_code_matches_its_definition() {
        let h = [0xAB; 32];
        let mut hasher = Blake2s256::new();
        hasher.update(b"cooldown-sas-v1");
        hasher.update(h);
        let d = hasher.finalize();
        let n = u64::from_be_bytes(d[..8].try_into().unwrap()) % 1_000_000;
        assert_eq!(u64::from(confirmation_code(&h)), n);
    }
}

/// Known-answer tests. They run `snow`'s Cacophony vectors for both protocol names through this
/// module's own builder (so the PSK position, prologue handling and message order used in
/// production are the ones checked), then pin the protocol's own bytes.
///
/// `fixed_ephemeral_key_for_testing_only` appears here and nowhere in production code.
#[cfg(test)]
mod vectors {
    use super::*;
    use crate::codec::hex_encode;
    use crate::hello::ClientHello;
    use serde_json::Value;
    use zeroize::Zeroizing;

    fn bytes(value: &Value, key: &str) -> Option<Vec<u8>> {
        let text = value.get(key)?.as_str()?;
        assert_eq!(text.len() % 2, 0);
        Some(
            (0..text.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
                .collect(),
        )
    }

    fn array(bytes: &[u8]) -> [u8; 32] {
        bytes.try_into().unwrap()
    }

    fn keypair(private: &[u8]) -> StaticKeypair {
        StaticKeypair::from_private(Zeroizing::new(array(private))).unwrap()
    }

    #[test]
    fn cacophony_vectors_for_both_protocol_names() {
        let file: Value =
            serde_json::from_str(include_str!("../tests/vectors/cacophony_ik_subset.json"))
                .unwrap();
        let vectors = file["vectors"].as_array().unwrap();
        let mut names: Vec<&str> = vectors
            .iter()
            .map(|v| v["protocol_name"].as_str().unwrap())
            .collect();
        names.sort_unstable();
        assert_eq!(names, [NOISE_PARAMS_RECONNECT, NOISE_PARAMS_PAIR]);

        for vector in vectors {
            let name = vector["protocol_name"].as_str().unwrap();
            let mode = if name == NOISE_PARAMS_PAIR {
                Mode::Pair
            } else {
                Mode::Reconnect
            };
            let init_static = keypair(&bytes(vector, "init_static").unwrap());
            let resp_static = keypair(&bytes(vector, "resp_static").unwrap());
            let init_remote = array(&bytes(vector, "init_remote_static").unwrap());
            assert_eq!(
                &init_remote,
                resp_static.public(),
                "vector is self-consistent"
            );
            let psk = vector
                .get("init_psks")
                .map(|p| array(&bytes(&serde_json::json!({ "k": p[0] }), "k").unwrap()));
            let init_prologue = bytes(vector, "init_prologue").unwrap();
            let resp_prologue = bytes(vector, "resp_prologue").unwrap();

            let init_params = HandshakeParams {
                mode,
                local: &init_static,
                remote_public: Some(&init_remote),
                psk: psk.as_ref(),
                prologue: &init_prologue,
            };
            let resp_params = HandshakeParams {
                mode,
                local: &resp_static,
                remote_public: None,
                psk: psk.as_ref(),
                prologue: &resp_prologue,
            };
            let mut init = Handshake {
                state: builder(&init_params)
                    .unwrap()
                    .fixed_ephemeral_key_for_testing_only(&bytes(vector, "init_ephemeral").unwrap())
                    .build_initiator()
                    .unwrap(),
            };
            let resp = Handshake {
                state: builder(&resp_params)
                    .unwrap()
                    .fixed_ephemeral_key_for_testing_only(&bytes(vector, "resp_ephemeral").unwrap())
                    .build_responder()
                    .unwrap(),
            };

            let messages = vector["messages"].as_array().unwrap();
            let step = |i: usize| {
                (
                    bytes(&messages[i], "payload").unwrap(),
                    bytes(&messages[i], "ciphertext").unwrap(),
                )
            };

            let (payload, expected) = step(0);
            let wire = init.write(&payload).unwrap();
            assert_eq!(wire, expected, "{name}: message 1");
            let (mut resp, got) = resp.read(&wire).unwrap();
            assert_eq!(got, payload);

            let (payload, expected) = step(1);
            let wire = resp.write(&payload).unwrap();
            assert_eq!(wire, expected, "{name}: message 2");
            let (init, got) = init.read(&wire).unwrap();
            assert_eq!(got, payload);

            let mut init = init.finish().unwrap();
            let mut resp = resp.finish().unwrap();
            let expected_hash = bytes(vector, "handshake_hash").unwrap();
            assert_eq!(init.handshake_hash.as_slice(), expected_hash.as_slice());
            assert_eq!(resp.handshake_hash.as_slice(), expected_hash.as_slice());

            // Transport messages alternate, initiator first.
            for index in 2..messages.len() {
                let (payload, expected) = step(index);
                let (sender, receiver) = if index % 2 == 0 {
                    (&mut init, &mut resp)
                } else {
                    (&mut resp, &mut init)
                };
                let wire = sender.transport.encrypt(&payload).unwrap();
                assert_eq!(wire, expected, "{name}: transport message {index}");
                assert_eq!(receiver.transport.decrypt(&wire).unwrap(), payload);
            }
        }
    }

    /// Fixed inputs for the protocol's own bytes. Inputs are deliberately plain so another
    /// implementation can reproduce them (see the README: they were cross-checked once against
    /// Python `noiseprotocol` 0.3.1).
    struct Golden {
        pc: StaticKeypair,
        phone: StaticKeypair,
        phone_e: [u8; 32],
        pc_e: [u8; 32],
        psk: [u8; 32],
        hello: ClientHello,
    }

    fn golden() -> Golden {
        Golden {
            pc: keypair(&[0x41; 32]),
            phone: keypair(&[0x42; 32]),
            phone_e: [0x43; 32],
            pc_e: [0x44; 32],
            psk: [0x45; 32],
            hello: ClientHello {
                proto_min: 1,
                proto_max: 1,
                mode: Mode::Pair,
                pair_id: [0x46; 16],
            },
        }
    }

    const MSG1_PAYLOAD: &[u8] = br#"{"name":"Pixel 8","platform":"android","ts":1700000000000}"#;
    const MSG2_PAYLOAD: &[u8] =
        br#"{"pc_name":"Test PC","proto":1,"server_time_ms":1700000000000}"#;

    /// Run the pairing handshake with fixed ephemerals; returns (prologue, msg1, msg2, h).
    fn run_golden() -> (Vec<u8>, Vec<u8>, Vec<u8>, [u8; 32]) {
        let g = golden();
        let pro = prologue(&g.hello.encode(), &g.pc.device_id());
        let init_params = HandshakeParams {
            mode: Mode::Pair,
            local: &g.phone,
            remote_public: Some(g.pc.public()),
            psk: Some(&g.psk),
            prologue: &pro,
        };
        let resp_params = HandshakeParams {
            mode: Mode::Pair,
            local: &g.pc,
            remote_public: None,
            psk: Some(&g.psk),
            prologue: &pro,
        };
        let mut init = Handshake {
            state: builder(&init_params)
                .unwrap()
                .fixed_ephemeral_key_for_testing_only(&g.phone_e)
                .build_initiator()
                .unwrap(),
        };
        let resp = Handshake {
            state: builder(&resp_params)
                .unwrap()
                .fixed_ephemeral_key_for_testing_only(&g.pc_e)
                .build_responder()
                .unwrap(),
        };
        let msg1 = init.write(MSG1_PAYLOAD).unwrap();
        let (mut resp, _) = resp.read(&msg1).unwrap();
        let msg2 = resp.write(MSG2_PAYLOAD).unwrap();
        let (init, _) = init.read(&msg2).unwrap();
        let init = init.finish().unwrap();
        let resp = resp.finish().unwrap();
        assert_eq!(init.handshake_hash, resp.handshake_hash);
        (pro.to_vec(), msg1, msg2, init.handshake_hash)
    }

    // Cross-checked once against Python `noiseprotocol` (see tests/vectors/crosscheck_noiseprotocol.py):
    // every value below was reproduced by that independent implementation from the same inputs.
    const PC_PUB: &str = "7a1a4e709bf085ac494aba0469b9b1eda0ab1f78b16aabb79ffeda90623e8522";
    const PHONE_PUB: &str = "132c442be010fbd57e72603328aa76e71fccc1503aae219327d14d9c9993f472";
    const DEVICE_ID: &str = "10c81cd28ff718bea9dfa10536ce46ee";
    const PROLOGUE: &str = "636f6f6c646f776e2f6e6f6973652f76314344574e010101004646464646464646464646464646464610c81cd28ff718bea9dfa10536ce46ee";
    const MSG1: &str = "cdefd8783a91b446640e2e1f95599db35e484a0071bd2182b3b60d0812c10c709da79c05fbfc9ad26ccaa6a0d1a65c928fc79b7b6d719f32b2ef4d42b61af64fbae52c4eb0d64c5159189be3c206726104ce72fd562f4f1cb3bd38eb1c3bc0351d258000caa3cbb53e98304041e95f8ed25c79eae125a931d4db53210b30b138061de6398b5c7c389f9300416341b6c6de66d3c1fb876b2170fd";
    const MSG2: &str = "ff2ee45601ec1b67310c7790404585ae697331eee1c1f8cf2419731c1fff3e6bb916631b2dfb55f83d6b1c2c7315293906bdf8f58179db2cc608cfdd8067760a174b6fc40d42355db1aac5501525469cf29192d1817156a3a6d53450d1ae1989d85d94a94fe4e284491c25b74958";
    const HANDSHAKE_HASH: &str = "4461817e4c634e3884abfefffe35bd5b33a995deef73bd5d1c4b6d80838ca9cc";
    const QR: &str = "CD1:AF5BUTTQTPYILLCJJK5AI2NZWHW2BKY7PCYWVK5XT77NVEDCH2CSERKFIVCUKRKFIVCUKRKFIVCUKRKFIVCUKRKFIVCUKRKFIVCUKRKFIZDEMRSGIZDEMRSGIZDEMRSGI3IEQAIEYCUACFA";

    #[test]
    fn golden_vectors_for_the_pairing_handshake() {
        let g = golden();
        let (pro, msg1, msg2, h) = run_golden();
        assert_eq!(hex_encode(g.pc.public()), PC_PUB);
        assert_eq!(hex_encode(g.phone.public()), PHONE_PUB);
        assert_eq!(g.pc.device_id().to_hex(), DEVICE_ID);
        assert_eq!(hex_encode(&pro), PROLOGUE);
        assert_eq!(hex_encode(&msg1), MSG1);
        assert_eq!(hex_encode(&msg2), MSG2);
        assert_eq!(hex_encode(&h), HANDSHAKE_HASH);
        assert_eq!(format_confirmation_code(confirmation_code(&h)), "833 663");
        let qr = crate::qr::QrPayload {
            pc_static_pub: *g.pc.public(),
            psk: Zeroizing::new(g.psk),
            pair_id: g.hello.pair_id,
            tcp_port: 53320,
            addrs: vec![std::net::IpAddr::V4(std::net::Ipv4Addr::new(
                192, 168, 1, 20,
            ))],
        };
        assert_eq!(&*qr.encode().unwrap(), QR);
        assert_eq!(crate::qr::QrPayload::decode(QR).unwrap(), qr);
    }
}
