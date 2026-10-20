//! A hand-driven protocol peer for negative tests: speaks the wire format directly so a test can
//! send exactly the bytes an attacker (or a buggy client) would.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::time::Duration;

use cooldown_sync::frame::{decode_envelope, encode_envelope, encode_frame, MsgType};
use cooldown_sync::hello::{ClientHello, Mode};
use cooldown_sync::message::{HelloBody, PairMsg1, ReconnectMsg1, APP_NAME};
use cooldown_sync::noise::{prologue, Handshake, HandshakeParams, Transport};
use cooldown_sync::StaticKeypair;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub struct Raw {
    pub stream: TcpStream,
}

impl Raw {
    pub async fn connect(addr: SocketAddr) -> Raw {
        let stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
            .await
            .expect("connect timed out")
            .expect("connect failed");
        Raw { stream }
    }

    pub async fn write(&mut self, bytes: &[u8]) {
        let _ = self.stream.write_all(bytes).await;
        let _ = self.stream.flush().await;
    }

    pub async fn shutdown_write(&mut self) {
        let _ = self.stream.shutdown().await;
    }

    /// One byte, or `None` at end of stream (or a reset).
    pub async fn read_byte(&mut self) -> Option<u8> {
        let mut byte = [0u8; 1];
        match super::within(self.stream.read(&mut byte)).await {
            Ok(1) => Some(byte[0]),
            _ => None,
        }
    }

    /// Up to `n` bytes; fewer if the stream ends first.
    pub async fn read_up_to(&mut self, n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        while out.len() < n {
            let mut chunk = vec![0u8; n - out.len()];
            match super::within(self.stream.read(&mut chunk)).await {
                Ok(0) | Err(_) => break,
                Ok(read) => out.extend_from_slice(&chunk[..read]),
            }
        }
        out
    }

    /// Wait for the server to close; `true` if nothing at all was received first.
    pub async fn closed_silently(&mut self) -> bool {
        self.read_up_to(1).await.is_empty()
    }

    /// Read one `len || bytes` frame, or `None` at end of stream.
    pub async fn read_frame(&mut self) -> Option<Vec<u8>> {
        let header = self.read_up_to(2).await;
        if header.len() < 2 {
            return None;
        }
        let len = usize::from(u16::from_be_bytes([header[0], header[1]]));
        let body = self.read_up_to(len).await;
        (body.len() == len).then_some(body)
    }
}

/// Everything needed to replay or tamper with a first message.
pub struct Started {
    pub hello_bytes: [u8; 24],
    pub msg1: Vec<u8>,
    pub hs: Handshake,
}

impl Started {
    /// The bytes a phone sends first: the hello, then message 1 as a frame.
    pub fn wire(&self) -> Vec<u8> {
        let mut out = self.hello_bytes.to_vec();
        out.extend(encode_frame(&self.msg1).unwrap());
        out
    }
}

pub fn hello(mode: Mode, pair_id: [u8; 16]) -> ClientHello {
    ClientHello {
        proto_min: 1,
        proto_max: 1,
        mode,
        pair_id,
    }
}

/// Build message 1 as the initiator. `prologue_hello` lets a test feed Noise a different hello
/// than the one it puts on the wire (a downgrade or tampering attempt).
pub fn start(
    mode: Mode,
    keypair: &StaticKeypair,
    pc_pub: &[u8; 32],
    psk: Option<&[u8; 32]>,
    pair_id: [u8; 16],
    payload: &[u8],
    prologue_hello: Option<ClientHello>,
) -> Started {
    let on_wire = hello(mode, pair_id);
    let bytes = on_wire.encode();
    let pc_id = cooldown_sync::DeviceId::from_public(pc_pub);
    let pro = prologue(&prologue_hello.unwrap_or(on_wire).encode(), &pc_id);
    let mut hs = Handshake::initiator(&HandshakeParams {
        mode,
        local: keypair,
        remote_public: Some(pc_pub),
        psk,
        prologue: &pro,
    })
    .unwrap();
    let msg1 = hs.write(payload).unwrap();
    Started {
        hello_bytes: bytes,
        msg1,
        hs,
    }
}

pub fn pair_payload(name: &str, ts: u64) -> Vec<u8> {
    serde_json::to_vec(&PairMsg1 {
        name: name.into(),
        platform: "android".into(),
        ts,
    })
    .unwrap()
}

pub fn reconnect_payload(ts: u64) -> Vec<u8> {
    serde_json::to_vec(&ReconnectMsg1 { ts }).unwrap()
}

/// An established raw session.
pub struct RawSession {
    pub raw: Raw,
    pub transport: Transport,
    pub handshake_hash: [u8; 32],
    pub msg2: Vec<u8>,
}

/// Send the first flight and read the reply. `Err(status)` for any status byte other than OK
/// (`Err(None)` if the server closed without one).
pub async fn handshake(mut raw: Raw, started: Started) -> Result<RawSession, Option<u8>> {
    raw.write(&started.wire()).await;
    match raw.read_byte().await {
        Some(0x00) => {}
        other => return Err(other),
    }
    let msg2 = raw.read_frame().await.ok_or(None)?;
    let (hs, payload) = started.hs.read(&msg2).map_err(|_| None)?;
    let finished = hs.finish().map_err(|_| None)?;
    Ok(RawSession {
        raw,
        transport: finished.transport,
        handshake_hash: finished.handshake_hash,
        msg2: payload,
    })
}

impl RawSession {
    pub async fn send(&mut self, msg_type: MsgType, body: &[u8]) {
        let plain = encode_envelope(msg_type, body).unwrap();
        self.send_plain(&plain).await;
    }

    /// Encrypt and frame a hand-built envelope plaintext.
    pub async fn send_plain(&mut self, plain: &[u8]) {
        let ciphertext = self.transport.encrypt(plain).unwrap();
        self.raw.write(&encode_frame(&ciphertext).unwrap()).await;
    }

    /// Frame arbitrary bytes as if they were a ciphertext.
    pub async fn send_ciphertext(&mut self, ciphertext: &[u8]) {
        self.raw.write(&encode_frame(ciphertext).unwrap()).await;
    }

    pub fn encrypt_frame(&mut self, msg_type: MsgType, body: &[u8]) -> Vec<u8> {
        let plain = encode_envelope(msg_type, body).unwrap();
        let ciphertext = self.transport.encrypt(&plain).unwrap();
        encode_frame(&ciphertext).unwrap()
    }

    pub async fn send_hello(&mut self, proto: u8) {
        let body = serde_json::to_vec(&HelloBody {
            app: APP_NAME.into(),
            platform: "android".into(),
            proto,
            now_ms: 0,
            last_epoch: 0,
            last_rev: 0,
            beacon_key: None,
        })
        .unwrap();
        self.send(MsgType::Hello, &body).await;
    }

    /// The next authenticated message, or `None` when the server closed (or the frame could not
    /// be decrypted).
    pub async fn recv(&mut self) -> Option<(MsgType, Vec<u8>)> {
        let frame = self.raw.read_frame().await?;
        let plain = self.transport.decrypt(&frame).ok()?;
        let (msg_type, body) = decode_envelope(&plain).ok()?;
        Some((msg_type, body.to_vec()))
    }

    pub async fn recv_json<T: DeserializeOwned>(&mut self, expect: MsgType) -> T {
        let (msg_type, body) = self.recv().await.expect("connection closed");
        assert_eq!(msg_type, expect);
        serde_json::from_slice(&body).unwrap()
    }

    /// Skip anything until a message of `wanted` arrives; `None` if the stream ends first.
    pub async fn recv_until(&mut self, wanted: MsgType) -> Option<Vec<u8>> {
        loop {
            let (msg_type, body) = self.recv().await?;
            if msg_type == wanted {
                return Some(body);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// A hand-driven *PC*, for testing what the phone client does with a misbehaving peer.
// ---------------------------------------------------------------------------------------------

use cooldown_sync::hello::PROTO_MAX_SUPPORTED;
use cooldown_sync::message::Msg2;
use cooldown_sync::DeviceId;
use tokio::net::TcpListener;

pub struct FakePc {
    pub keypair: StaticKeypair,
    pub listener: TcpListener,
    pub psk: [u8; 32],
    pub pair_id: [u8; 16],
}

impl FakePc {
    pub async fn new() -> FakePc {
        FakePc {
            keypair: StaticKeypair::generate().unwrap(),
            listener: TcpListener::bind("127.0.0.1:0").await.unwrap(),
            psk: [0x77; 32],
            pair_id: [0x55; 16],
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.listener.local_addr().unwrap()
    }

    pub fn device_id(&self) -> DeviceId {
        self.keypair.device_id()
    }

    pub fn qr_text(&self) -> String {
        cooldown_sync::QrPayload {
            pc_static_pub: *self.keypair.public(),
            psk: zeroize::Zeroizing::new(self.psk),
            pair_id: self.pair_id,
            tcp_port: self.addr().port(),
            addrs: vec![self.addr().ip()],
        }
        .encode()
        .unwrap()
        .to_string()
    }

    /// Accept one connection and complete the responder side of the handshake as a well-behaved
    /// PC would, returning the established session and the phone's first message-1 payload.
    pub async fn accept(&self, mode: Mode) -> (RawSession, Vec<u8>) {
        let (stream, _) = super::within(self.listener.accept()).await.unwrap();
        let mut raw = Raw { stream };
        let hello_bytes: [u8; 24] = raw.read_up_to(24).await.try_into().expect("hello");
        let msg1 = raw.read_frame().await.expect("message 1");
        let pro = prologue(&hello_bytes, &self.keypair.device_id());
        let hs = Handshake::responder(&HandshakeParams {
            mode,
            local: &self.keypair,
            remote_public: None,
            psk: (mode == Mode::Pair).then_some(&self.psk),
            prologue: &pro,
        })
        .unwrap();
        let (mut hs, payload) = hs.read(&msg1).expect("message 1 authenticates");
        let msg2 = serde_json::to_vec(&Msg2 {
            pc_name: (mode == Mode::Pair).then(|| "Fake PC".to_owned()),
            proto: PROTO_MAX_SUPPORTED,
            server_time_ms: 1_700_000_000_000,
        })
        .unwrap();
        let out = hs.write(&msg2).unwrap();
        let mut bytes = vec![0x00];
        bytes.extend(encode_frame(&out).unwrap());
        raw.write(&bytes).await;
        let finished = hs.finish().unwrap();
        (
            RawSession {
                raw,
                transport: finished.transport,
                handshake_hash: finished.handshake_hash,
                msg2: Vec::new(),
            },
            payload,
        )
    }
}

/// A well-formed PC HELLO body.
pub fn pc_hello_body(beacon: Option<[u8; 32]>) -> Vec<u8> {
    serde_json::to_vec(&HelloBody {
        app: APP_NAME.into(),
        platform: "windows".into(),
        proto: 1,
        now_ms: 1_700_000_000_000,
        last_epoch: 0,
        last_rev: 0,
        beacon_key: beacon.map(|k| cooldown_sync::codec::b64_encode(&k)),
    })
    .unwrap()
}
