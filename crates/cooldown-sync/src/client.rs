//! The phone side: [`PhoneClient`] pairs from a QR payload, reconnects to a paired PC, and keeps
//! the latest `SyncPayload` it received.
//!
//! The phone only ever dials out; it never listens. It stores nothing about a PC until the PC's
//! `PAIR_OK` arrives, never treats an unauthenticated byte (`FAIL`, `VERSION`, ...) as a reason to
//! unpair or to lower its protocol floor, and closes the connection on any decrypt error.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cooldown_core::SyncPayload;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};

use crate::clock::{with_deadline, SyncClock};
use crate::codec::b64_decode_fixed;
use crate::error::ClientError;
use crate::frame::{encode_frame, FrameReader, MsgType};
use crate::hello::{ClientHello, Mode, Status, PROTO_MAX_SUPPORTED};
use crate::keys::{random_bytes, DeviceId, PublicKey, StaticKeypair};
use crate::limits::{Limits, MAX_SESSION_MESSAGES};
use crate::lock;
use crate::message::{
    valid_name, valid_platform, AckBody, AlertSetBody, ErrorCode, HelloBody, Msg2, PairMsg1,
    PingBody, ReconnectMsg1, RevokeBody, SyncBody, APP_NAME,
};
use crate::noise::{
    confirmation_code, format_confirmation_code, prologue, Handshake, HandshakeParams,
    MAX_HANDSHAKE_MESSAGE,
};
use crate::qr::QrPayload;
use crate::sanitize::check_incoming;
use crate::session::{Channel, ChannelError, Incoming};
use crate::trust::{PeerRecord, SessionUpdate, TrustStore};

/// What this phone calls itself.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Display name, 1 to 32 characters, shown on the PC's confirmation prompt.
    pub name: String,
    /// Platform tag, for example `android`.
    pub platform: String,
}

/// Settings for [`PhoneClient`].
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// This phone's name and platform.
    pub device: DeviceInfo,
    /// Timeouts. Defaults are the protocol document's numbers.
    pub limits: Limits,
}

impl ClientConfig {
    /// Defaults with the given device name and platform.
    pub fn new(name: impl Into<String>, platform: impl Into<String>) -> Self {
        ClientConfig {
            device: DeviceInfo {
                name: name.into(),
                platform: platform.into(),
            },
            limits: Limits::default(),
        }
    }
}

/// The newest state received from one PC.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceivedSync {
    /// The PC it came from.
    pub pc: DeviceId,
    /// Its epoch.
    pub epoch: u64,
    /// Its revision.
    pub rev: u64,
    /// The payload (already validated against the shared limits).
    pub payload: SyncPayload,
    /// When it arrived, in Unix milliseconds on the phone's wall clock (show as "as of").
    pub received_at_ms: i64,
}

/// Why a connection is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// An authenticated `REVOKE`: the PC unpaired this phone and the record was deleted.
    Revoked,
    /// This phone unpaired from the PC.
    Unpaired,
    /// `close()` was called or the handle was dropped.
    Local,
    /// The peer, the network or a limit ended it; a short code.
    Remote(&'static str),
}

/// Where a connection is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    /// Handshake done; waiting for the PC's HELLO.
    Connecting,
    /// The PC's HELLO arrived; syncing.
    Ready,
    /// Over.
    Closed(CloseReason),
}

#[derive(Debug)]
enum ClientCmd {
    SetAlert(AlertSetBody),
    Close,
    Unpair,
}

struct ClientShared {
    limits: Limits,
    device: DeviceInfo,
    clock: Arc<dyn SyncClock>,
    keypair: StaticKeypair,
    trust: Arc<dyn TrustStore>,
    latest: Mutex<HashMap<DeviceId, ReceivedSync>>,
    latest_tx: watch::Sender<Option<Arc<ReceivedSync>>>,
}

impl ClientShared {
    /// Forget everything held for a PC that was revoked, unpaired or forgotten. The `latest_tx`
    /// watch is what [`PhoneClient::latest_sync`] and the app's subscription read, so if it holds
    /// this PC's state it is replaced with the newest state of a remaining PC (by received time),
    /// or `None`. Otherwise the app would keep showing, and re-scheduling alarms from, a PC the
    /// user no longer trusts.
    fn drop_pc_state(&self, pc: &DeviceId) {
        let mut latest = lock(&self.latest);
        latest.remove(pc);
        let held = self
            .latest_tx
            .borrow()
            .as_ref()
            .is_some_and(|have| have.pc == *pc);
        if held {
            let newest = newest_state(&latest).cloned().map(Arc::new);
            self.latest_tx.send_replace(newest);
        }
    }
}

/// The most recently received state of any PC (ties broken by device id so the choice is stable).
fn newest_state(latest: &HashMap<DeviceId, ReceivedSync>) -> Option<&ReceivedSync> {
    latest
        .values()
        .max_by_key(|state| (state.received_at_ms, state.pc))
}

/// The phone's sync client.
#[derive(Clone)]
pub struct PhoneClient {
    shared: Arc<ClientShared>,
}

struct Established<S> {
    channel: Channel<S>,
    handshake_hash: [u8; 32],
    msg2: Msg2,
    hello_sent_at: Duration,
}

impl PhoneClient {
    /// A client with this phone's identity and trust store.
    pub fn new(
        config: ClientConfig,
        keypair: StaticKeypair,
        trust: Arc<dyn TrustStore>,
        clock: Arc<dyn SyncClock>,
    ) -> Result<Self, ClientError> {
        if !valid_name(&config.device.name) || !valid_platform(&config.device.platform) {
            return Err(ClientError::Protocol("invalid device info"));
        }
        let (latest_tx, _) = watch::channel(None);
        Ok(PhoneClient {
            shared: Arc::new(ClientShared {
                limits: config.limits.normalized(),
                device: config.device,
                clock,
                keypair,
                trust,
                latest: Mutex::new(HashMap::new()),
                latest_tx,
            }),
        })
    }

    /// This phone's device id.
    pub fn device_id(&self) -> DeviceId {
        self.shared.keypair.device_id()
    }

    /// Paired PCs.
    pub fn paired_pcs(&self) -> Result<Vec<PeerRecord>, ClientError> {
        Ok(self.shared.trust.list()?)
    }

    /// Delete a PC's record while not connected to it (the user removed it on the phone). The app
    /// must also cancel that PC's alarms.
    pub fn forget(&self, pc: &DeviceId) -> Result<bool, ClientError> {
        self.shared.drop_pc_state(pc);
        Ok(self.shared.trust.revoke(pc)?)
    }

    /// The newest state received from any PC.
    pub fn latest_sync(&self) -> Option<ReceivedSync> {
        self.shared
            .latest_tx
            .borrow()
            .as_ref()
            .map(|latest| (**latest).clone())
    }

    /// The newest state received from one PC.
    pub fn latest_sync_for(&self, pc: &DeviceId) -> Option<ReceivedSync> {
        lock(&self.shared.latest).get(pc).cloned()
    }

    /// Watch for new states.
    pub fn subscribe(&self) -> watch::Receiver<Option<Arc<ReceivedSync>>> {
        self.shared.latest_tx.subscribe()
    }

    /// Restore a state the app persisted, so the app can show it before the first reconnect and the
    /// next HELLO reports it (the PC can skip a redundant push). It also becomes
    /// [`latest_sync`](Self::latest_sync) unless something newer is already there. Ignored if the
    /// PC is unknown.
    pub fn restore_latest(&self, state: ReceivedSync) {
        let known = self.shared.trust.get(&state.pc).ok().flatten().is_some();
        if !known {
            return;
        }
        lock(&self.shared.latest).insert(state.pc, state.clone());
        let newer = self
            .shared
            .latest_tx
            .borrow()
            .as_ref()
            .is_none_or(|have| have.received_at_ms <= state.received_at_ms);
        if newer {
            self.shared.latest_tx.send_replace(Some(Arc::new(state)));
        }
    }

    /// Step 1 of pairing: connect to the PC named in the QR, run `Noise_IKpsk1`, and return the
    /// confirmation code to show. Nothing is stored yet.
    pub async fn begin_pairing(&self, qr_text: &str) -> Result<PendingPairing, ClientError> {
        let qr = QrPayload::decode(qr_text)?;
        if qr.pc_static_pub == *self.shared.keypair.public() {
            return Err(ClientError::Protocol("QR names this device"));
        }
        let ts = u64::try_from(self.shared.clock.wall_ms())
            .unwrap_or(1)
            .max(1);
        let request = PairMsg1 {
            name: self.shared.device.name.clone(),
            platform: self.shared.device.platform.clone(),
            ts,
        };
        let payload = serde_json::to_vec(&request).map_err(|_| ClientError::Protocol("encode"))?;
        let stream = self.dial(&qr.socket_addrs()).await?;
        let established = self
            .handshake(
                stream,
                Mode::Pair,
                &qr.pc_static_pub,
                Some(&qr.psk),
                qr.pair_id,
                1,
                &payload,
                (0, 0),
            )
            .await?;
        let code = format_confirmation_code(confirmation_code(&established.handshake_hash));
        Ok(PendingPairing {
            shared: Arc::clone(&self.shared),
            established,
            code,
            pc_pub: qr.pc_static_pub,
            ts,
        })
    }

    /// Reconnect to a paired PC with `Noise_IK`. `addrs` are candidate addresses (cached ones, or
    /// the source of a verified beacon); the first to answer is used, and the handshake, not the
    /// address, proves it is the right PC.
    pub async fn connect(
        &self,
        pc: &DeviceId,
        addrs: &[SocketAddr],
    ) -> Result<Connection, ClientError> {
        let shared = &self.shared;
        let record = shared.trust.get(pc)?.ok_or(ClientError::UnknownPc)?;
        // Never offer less than the highest version completed with this PC.
        let proto_min = record.proto_floor.max(1);
        if proto_min > PROTO_MAX_SUPPORTED {
            return Err(ClientError::VersionMismatch {
                server_max: PROTO_MAX_SUPPORTED,
            });
        }
        // Persisted before it is sent, so a crash cannot lead to reusing a timestamp.
        let ts = shared
            .trust
            .reserve_ts(pc, shared.clock.wall_ms())?
            .ok_or(ClientError::UnknownPc)?;
        let payload = serde_json::to_vec(&ReconnectMsg1 { ts })
            .map_err(|_| ClientError::Protocol("encode"))?;
        let applied = lock(&shared.latest)
            .get(pc)
            .map_or((0, 0), |s| (s.epoch, s.rev));
        let stream = self.dial(addrs).await?;
        let established = self
            .handshake(
                stream,
                Mode::Reconnect,
                &record.static_pub,
                None,
                [0; 16],
                proto_min,
                &payload,
                applied,
            )
            .await?;
        let proto = established.msg2.proto;
        Ok(start_connection(shared, established, *pc, proto))
    }

    /// Dial every address with a stagger, keep the first TCP connection that opens.
    async fn dial(&self, addrs: &[SocketAddr]) -> Result<TcpStream, ClientError> {
        if addrs.is_empty() {
            return Err(ClientError::Connect);
        }
        let limits = &self.shared.limits;
        let (tx, mut rx) = mpsc::channel(addrs.len());
        let mut tasks = Vec::with_capacity(addrs.len());
        for (index, addr) in addrs.iter().copied().enumerate() {
            let tx = tx.clone();
            let clock = Arc::clone(&self.shared.clock);
            let stagger = limits.connect_stagger * u32::try_from(index).unwrap_or(u32::MAX);
            let timeout = limits.connect_timeout;
            tasks.push(tokio::spawn(async move {
                if !stagger.is_zero() {
                    clock.sleep(stagger).await;
                }
                let stream = with_deadline(&*clock, timeout, TcpStream::connect(addr)).await;
                let _ = tx.send(stream.ok().and_then(Result::ok)).await;
            }));
        }
        drop(tx);
        let mut winner = None;
        while let Some(attempt) = rx.recv().await {
            if let Some(stream) = attempt {
                winner = Some(stream);
                break;
            }
        }
        for task in tasks {
            task.abort();
        }
        let stream = winner.ok_or(ClientError::Connect)?;
        let _ = stream.set_nodelay(true);
        Ok(stream)
    }

    /// Write the hello and message 1, read the status, read message 2, switch to transport mode
    /// and send our HELLO.
    #[allow(clippy::too_many_arguments)]
    async fn handshake<S>(
        &self,
        stream: S,
        mode: Mode,
        pc_pub: &PublicKey,
        psk: Option<&[u8; 32]>,
        pair_id: [u8; 16],
        proto_min: u8,
        msg1_payload: &[u8],
        applied: (u64, u64),
    ) -> Result<Established<S>, ClientError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let shared = &self.shared;
        let limits = &shared.limits;
        let clock: &dyn SyncClock = &*shared.clock;
        let started = clock.mono();
        let left = || (started + limits.handshake_deadline).saturating_sub(clock.mono());
        let hello = ClientHello {
            proto_min,
            proto_max: PROTO_MAX_SUPPORTED,
            mode,
            pair_id,
        };
        let hello_bytes = hello.encode();
        let pc_id = DeviceId::from_public(pc_pub);
        let pro = prologue(&hello_bytes, &pc_id);
        let mut hs = Handshake::initiator(&HandshakeParams {
            mode,
            local: &shared.keypair,
            remote_public: Some(pc_pub),
            psk,
            prologue: &pro,
        })
        .map_err(|_| ClientError::Protocol("handshake setup"))?;
        let msg1 = hs
            .write(msg1_payload)
            .map_err(|_| ClientError::Protocol("message 1"))?;
        let mut out = hello_bytes.to_vec();
        out.extend(encode_frame(&msg1).map_err(|_| ClientError::Protocol("message 1"))?);

        let (read_half, mut writer) = tokio::io::split(stream);
        let mut reader = FrameReader::new(read_half, MAX_HANDSHAKE_MESSAGE);
        with_deadline(clock, left(), async {
            writer.write_all(&out).await?;
            writer.flush().await
        })
        .await
        .map_err(|_| ClientError::Timeout)?
        .map_err(|_| ClientError::Closed)?;

        let status_byte = with_deadline(clock, left(), reader.read_raw(1))
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(|_| ClientError::Closed)?[0];
        match Status::from_byte(status_byte) {
            Some(Status::Ok) => {}
            Some(Status::Version) => {
                // Unauthenticated: report it, but it changes nothing we store or offer.
                let extra = with_deadline(clock, left(), reader.read_raw(1)).await;
                let server_max = extra.ok().and_then(Result::ok).map_or(0, |b| b[0]);
                return Err(ClientError::VersionMismatch { server_max });
            }
            Some(Status::NotPairing) => return Err(ClientError::NotPairing),
            Some(Status::Busy) => return Err(ClientError::Busy),
            Some(Status::RateLimited) => return Err(ClientError::RateLimited),
            Some(Status::Fail) => return Err(ClientError::Rejected),
            None => return Err(ClientError::Protocol("unknown status byte")),
        }
        let msg2_bytes = with_deadline(clock, left(), reader.read_frame())
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(|_| ClientError::Protocol("message 2 framing"))?;
        // A failed read consumes the handshake: no retry on the same state.
        let (hs, msg2_payload) = hs
            .read(&msg2_bytes)
            .map_err(|_| ClientError::Protocol("message 2 authentication"))?;
        let msg2: Msg2 = serde_json::from_slice(&msg2_payload)
            .ok()
            .filter(Msg2::is_valid)
            .ok_or(ClientError::Protocol("message 2 payload"))?;
        if msg2.proto < proto_min || msg2.proto > PROTO_MAX_SUPPORTED {
            return Err(ClientError::Protocol("negotiated version out of range"));
        }
        let finished = hs
            .finish()
            .map_err(|_| ClientError::Protocol("handshake state"))?;
        if !bool::from(finished.remote_static.ct_eq(pc_pub)) {
            return Err(ClientError::Protocol("unexpected PC key"));
        }
        let mut channel = Channel::new(
            reader,
            writer,
            finished.transport,
            Arc::clone(&shared.clock),
            limits.write_timeout,
        );
        let hello_body = HelloBody {
            app: APP_NAME.to_owned(),
            platform: shared.device.platform.clone(),
            proto: msg2.proto,
            now_ms: shared.clock.wall_ms(),
            last_epoch: applied.0,
            last_rev: applied.1,
            beacon_key: None,
        };
        let hello_sent_at = clock.mono();
        channel
            .send_json(MsgType::Hello, &hello_body)
            .await
            .map_err(|_| ClientError::Closed)?;
        Ok(Established {
            channel,
            handshake_hash: finished.handshake_hash,
            msg2,
            hello_sent_at,
        })
    }
}

/// A pairing that has completed the handshake and is waiting for the user at the PC.
///
/// Show [`code`](Self::code) on the phone. Nothing is stored until
/// [`wait_for_confirmation`](Self::wait_for_confirmation) sees the PC's `PAIR_OK`. Dropping this
/// value abandons the pairing and stores nothing.
pub struct PendingPairing {
    shared: Arc<ClientShared>,
    established: Established<TcpStream>,
    code: String,
    pc_pub: PublicKey,
    ts: u64,
}

impl std::fmt::Debug for PendingPairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingPairing").finish_non_exhaustive()
    }
}

impl PendingPairing {
    /// The six-digit confirmation code (`123 456`) to display. The PC shows the same code and
    /// the user confirms there only if the two match.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The PC's name, as the PC announced it inside the authenticated handshake.
    pub fn pc_name(&self) -> &str {
        self.established.msg2.pc_name.as_deref().unwrap_or("PC")
    }

    /// The PC's device id (derived from the QR key, which the handshake confirmed).
    pub fn pc_device_id(&self) -> DeviceId {
        DeviceId::from_public(&self.pc_pub)
    }

    /// Wait for the user's decision on the PC. On `PAIR_OK` the PC is stored and the live
    /// connection is returned; on anything else nothing is stored.
    pub async fn wait_for_confirmation(self) -> Result<Connection, ClientError> {
        let PendingPairing {
            shared,
            established,
            pc_pub,
            ts,
            ..
        } = self;
        let Established {
            mut channel,
            msg2,
            hello_sent_at,
            ..
        } = established;
        let limits = &shared.limits;
        let clock: &dyn SyncClock = &*shared.clock;
        let started = clock.mono();
        loop {
            let left = (started + limits.pair_ok_timeout).saturating_sub(clock.mono());
            let message = with_deadline(clock, left, channel.recv())
                .await
                .map_err(|_| ClientError::Timeout)?;
            let message = match message {
                Ok(message) => message,
                Err(ChannelError::Closed | ChannelError::Io) => return Err(ClientError::Closed),
                Err(error) => {
                    if let Some(code) = error.reply() {
                        channel.send_error(code).await;
                    }
                    return Err(ClientError::Protocol(error.code()));
                }
            };
            match message.msg_type {
                MsgType::PairOk => {
                    if message.json::<crate::message::EmptyBody>().is_err() {
                        channel.send_error(ErrorCode::Malformed).await;
                        return Err(ClientError::Protocol("malformed PAIR_OK"));
                    }
                    break;
                }
                MsgType::Revoke | MsgType::Error => return Err(ClientError::PairingRefused),
                MsgType::Ping => {
                    if let Ok(ping) = message.json::<PingBody>() {
                        let _ = channel.send_json(MsgType::Pong, &ping).await;
                    }
                }
                // SYNC (or anything else) before PAIR_OK: the PC must not send data before the
                // user has confirmed, so this is a protocol violation and nothing is stored.
                _ => {
                    channel.send_error(ErrorCode::Unexpected).await;
                    return Err(ClientError::Protocol("message before PAIR_OK"));
                }
            }
        }
        let pc_id = DeviceId::from_public(&pc_pub);
        let record = PeerRecord::new(
            pc_pub,
            msg2.pc_name.clone().unwrap_or_else(|| "PC".to_owned()),
            "pc".to_owned(),
            shared.clock.wall_ms(),
            ts,
            msg2.proto,
        );
        shared.trust.upsert(record)?;
        let proto = msg2.proto;
        Ok(start_connection(
            &shared,
            Established {
                channel,
                handshake_hash: [0; 32],
                msg2,
                hello_sent_at,
            },
            pc_id,
            proto,
        ))
    }
}

/// Whether a SYNC with `(epoch, rev)` should replace what the phone holds (`have`).
///
/// The rule is idempotent and only moves forward: a new epoch always applies (the PC's database
/// was replaced or restored, so its revisions restarted), and within one epoch only a higher
/// revision does. A duplicate or older SYNC is acknowledged but not applied.
pub fn should_apply(have: Option<(u64, u64)>, epoch: u64, rev: u64) -> bool {
    have.is_none_or(|(have_epoch, have_rev)| epoch != have_epoch || rev > have_rev)
}

/// A live, authenticated connection to a PC. Dropping it closes the connection.
pub struct Connection {
    pc: DeviceId,
    cmd_tx: mpsc::UnboundedSender<ClientCmd>,
    state_rx: watch::Receiver<ConnState>,
    offset_ms: Arc<Mutex<Option<i64>>>,
    skew_limit_ms: i64,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("pc", &self.pc)
            .field("state", &self.state())
            .finish()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(ClientCmd::Close);
    }
}

impl Connection {
    /// The PC at the other end.
    pub fn pc_device_id(&self) -> DeviceId {
        self.pc
    }

    /// The current state.
    pub fn state(&self) -> ConnState {
        *self.state_rx.borrow()
    }

    /// Wait until the PC's HELLO has been processed (or the connection ends first).
    pub async fn wait_ready(&self) -> Result<(), CloseReason> {
        let mut rx = self.state_rx.clone();
        loop {
            match *rx.borrow_and_update() {
                ConnState::Ready => return Ok(()),
                ConnState::Closed(reason) => return Err(reason),
                ConnState::Connecting => {}
            }
            if rx.changed().await.is_err() {
                return Err(CloseReason::Remote("dropped"));
            }
        }
    }

    /// Wait until the connection is over and say why.
    pub async fn closed(&self) -> CloseReason {
        let mut rx = self.state_rx.clone();
        loop {
            if let ConnState::Closed(reason) = *rx.borrow_and_update() {
                return reason;
            }
            if rx.changed().await.is_err() {
                return CloseReason::Remote("dropped");
            }
        }
    }

    /// The PC's clock minus ours, in milliseconds, estimated from HELLO and the round trip.
    /// Display only: it never moves an alarm.
    pub fn clock_offset_ms(&self) -> Option<i64> {
        *lock(&self.offset_ms)
    }

    /// True if the clocks differ by more than the warning threshold (120 s by default).
    pub fn clock_skew_warning(&self) -> bool {
        self.clock_offset_ms()
            .is_some_and(|offset| offset.abs() > self.skew_limit_ms)
    }

    /// Ask the PC to toggle a reset alert. The PC is authoritative: the phone marks the toggle
    /// pending and clears the mark when a SYNC reflects it.
    pub fn set_alert(&self, id: &str, enabled: bool) -> Result<(), ClientError> {
        let mut op = [0u8; 8];
        random_bytes(&mut op).map_err(|_| ClientError::Rng)?;
        let body = AlertSetBody {
            op_id: u64::from_be_bytes(op),
            id: id.to_owned(),
            enabled,
        };
        if !body.is_valid() {
            return Err(ClientError::Protocol("invalid alert id"));
        }
        self.cmd_tx
            .send(ClientCmd::SetAlert(body))
            .map_err(|_| ClientError::Closed)
    }

    /// Close this connection (the pairing stays).
    pub fn close(&self) {
        let _ = self.cmd_tx.send(ClientCmd::Close);
    }

    /// Unpair from this PC: tell it, delete the record, and close.
    pub fn unpair(&self) {
        let _ = self.cmd_tx.send(ClientCmd::Unpair);
    }
}

fn start_connection<S>(
    shared: &Arc<ClientShared>,
    established: Established<S>,
    pc: DeviceId,
    proto: u8,
) -> Connection
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (state_tx, state_rx) = watch::channel(ConnState::Connecting);
    let offset_ms = Arc::new(Mutex::new(None));
    let skew_limit_ms = i64::try_from(shared.limits.clock_skew_warn_secs)
        .unwrap_or(i64::MAX)
        .saturating_mul(1000);
    let task_shared = Arc::clone(shared);
    let task_offset = Arc::clone(&offset_ms);
    tokio::spawn(async move {
        let reason = run_client_loop(
            &task_shared,
            established.channel,
            pc,
            proto,
            established.hello_sent_at,
            cmd_rx,
            &state_tx,
            &task_offset,
        )
        .await;
        let _ = state_tx.send(ConnState::Closed(reason));
    });
    Connection {
        pc,
        cmd_tx,
        state_rx,
        offset_ms,
        skew_limit_ms,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_client_loop<S>(
    sh: &Arc<ClientShared>,
    mut channel: Channel<S>,
    pc: DeviceId,
    proto: u8,
    hello_sent_at: Duration,
    mut cmd_rx: mpsc::UnboundedReceiver<ClientCmd>,
    state_tx: &watch::Sender<ConnState>,
    offset_ms: &Mutex<Option<i64>>,
) -> CloseReason
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let limits = &sh.limits;
    let clock = &*sh.clock;
    let started = clock.mono();
    let mut last_rx = started;
    let mut last_ping = started;
    let mut ping_n = 0u64;
    let mut ready = false;
    let reason = loop {
        let now = clock.mono();
        let age = now.saturating_sub(started);
        if age >= limits.session_lifetime || channel.total_messages() >= MAX_SESSION_MESSAGES {
            break CloseReason::Remote("lifetime");
        }
        let silent_for = now.saturating_sub(last_rx);
        if silent_for >= limits.silence_timeout {
            break CloseReason::Remote("silence");
        }
        let mut wake = (limits.silence_timeout - silent_for).min(limits.session_lifetime - age);
        if !ready {
            if age >= limits.first_message_deadline {
                channel.send_error(ErrorCode::Timeout).await;
                break CloseReason::Remote("hello_timeout");
            }
            wake = wake.min(limits.first_message_deadline - age);
        }
        let ping_due = (last_rx.max(last_ping) + limits.ping_after).saturating_sub(now);
        if ping_due.is_zero() {
            ping_n += 1;
            if channel
                .send_json(MsgType::Ping, &PingBody { n: ping_n })
                .await
                .is_err()
            {
                break CloseReason::Remote("write");
            }
            last_ping = now;
            continue;
        }
        wake = wake.min(ping_due);

        tokio::select! {
            biased;
            command = cmd_rx.recv() => match command {
                None | Some(ClientCmd::Close) => break CloseReason::Local,
                Some(ClientCmd::SetAlert(body)) => {
                    if channel.send_json(MsgType::AlertSet, &body).await.is_err() {
                        break CloseReason::Remote("write");
                    }
                }
                Some(ClientCmd::Unpair) => {
                    let _ = channel
                        .send_json(MsgType::Revoke, &RevokeBody::new("unpaired_on_phone"))
                        .await;
                    let _ = sh.trust.revoke(&pc);
                    sh.drop_pc_state(&pc);
                    break CloseReason::Unpaired;
                }
            },
            incoming = channel.recv() => match incoming {
                Err(error) => {
                    if let Some(code) = error.reply() {
                        channel.send_error(code).await;
                    }
                    break CloseReason::Remote(error.code());
                }
                Ok(message) => {
                    last_rx = clock.mono();
                    let outcome = handle_message(
                        sh, &mut channel, pc, proto, message, &mut ready,
                        hello_sent_at, state_tx, offset_ms,
                    )
                    .await;
                    if let Some(reason) = outcome {
                        break reason;
                    }
                }
            },
            () = clock.sleep(wake) => {},
        }
    };
    channel.shutdown().await;
    reason
}

/// Handle one authenticated message from the PC. `Some(reason)` ends the connection.
#[allow(clippy::too_many_arguments)]
async fn handle_message<S>(
    sh: &Arc<ClientShared>,
    channel: &mut Channel<S>,
    pc: DeviceId,
    proto: u8,
    message: Incoming,
    ready: &mut bool,
    hello_sent_at: Duration,
    state_tx: &watch::Sender<ConnState>,
    offset_ms: &Mutex<Option<i64>>,
) -> Option<CloseReason>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    macro_rules! bail {
        ($code:expr, $why:expr) => {{
            channel.send_error($code).await;
            return Some(CloseReason::Remote($why));
        }};
    }
    match message.msg_type {
        MsgType::Hello => {
            let Ok(hello) = message.json::<HelloBody>() else {
                bail!(ErrorCode::Malformed, "malformed");
            };
            if !hello.is_valid() || hello.proto != proto {
                bail!(ErrorCode::Version, "bad_hello");
            }
            let beacon_key = match hello.beacon_key.as_deref() {
                None => None,
                Some(text) => match b64_decode_fixed::<32>(text) {
                    Some(key) => Some(key),
                    None => bail!(ErrorCode::Malformed, "bad_beacon_key"),
                },
            };
            if !*ready {
                // offset = pc_now - (phone_now - rtt / 2), all in milliseconds.
                let now = sh.clock.mono();
                let rtt_ms = i64::try_from(now.saturating_sub(hello_sent_at).as_millis())
                    .unwrap_or(i64::MAX);
                let offset = hello
                    .now_ms
                    .saturating_sub(sh.clock.wall_ms().saturating_sub(rtt_ms / 2));
                *lock(offset_ms) = Some(offset);
                *ready = true;
                let _ = state_tx.send(ConnState::Ready);
            }
            let _ = sh.trust.record_session(
                &pc,
                SessionUpdate {
                    last_seen_ms: Some(sh.clock.wall_ms()),
                    proto_floor: Some(proto),
                    beacon_key,
                },
            );
            None
        }
        MsgType::Sync => {
            if !*ready {
                bail!(ErrorCode::Unexpected, "sync_before_hello");
            }
            let Ok(body) = message.json::<SyncBody>() else {
                bail!(ErrorCode::Malformed, "malformed");
            };
            if check_incoming(&body.payload).is_err() || body.payload.device_id != pc.to_hex() {
                bail!(ErrorCode::Malformed, "bad_payload");
            }
            let apply = {
                let latest = lock(&sh.latest);
                should_apply(
                    latest.get(&pc).map(|have| (have.epoch, have.rev)),
                    body.epoch,
                    body.rev,
                )
            };
            if apply {
                let received = ReceivedSync {
                    pc,
                    epoch: body.epoch,
                    rev: body.rev,
                    payload: body.payload,
                    received_at_ms: sh.clock.wall_ms(),
                };
                // Under one lock, so a concurrent forget cannot interleave between the two.
                let mut latest = lock(&sh.latest);
                latest.insert(pc, received.clone());
                sh.latest_tx.send_replace(Some(Arc::new(received)));
                drop(latest);
            }
            // Every SYNC is acknowledged, applied or not, so the PC can send the next one.
            let ack = AckBody {
                epoch: body.epoch,
                rev: body.rev,
            };
            if channel.send_json(MsgType::Ack, &ack).await.is_err() {
                return Some(CloseReason::Remote("write"));
            }
            None
        }
        MsgType::Ping => {
            let Ok(ping) = message.json::<PingBody>() else {
                bail!(ErrorCode::Malformed, "malformed");
            };
            if channel.send_json(MsgType::Pong, &ping).await.is_err() {
                return Some(CloseReason::Remote("write"));
            }
            None
        }
        MsgType::Pong => {
            if message.json::<PingBody>().is_err() {
                bail!(ErrorCode::Malformed, "malformed");
            }
            None
        }
        MsgType::Revoke => {
            // Authenticated, so honoured: delete the record. (An unauthenticated FAIL never gets
            // here; it is a status byte, not a message.)
            let _ = sh.trust.revoke(&pc);
            sh.drop_pc_state(&pc);
            Some(CloseReason::Revoked)
        }
        MsgType::Error => Some(CloseReason::Remote("peer_error")),
        MsgType::PairOk | MsgType::Ack | MsgType::AlertSet => {
            bail!(ErrorCode::Unexpected, "unexpected")
        }
    }
}
