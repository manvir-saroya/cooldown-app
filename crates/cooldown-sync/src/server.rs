//! The PC side: [`PcServer`] accepts pairing and reconnect handshakes and pushes `SyncPayload`s to
//! authenticated phones.
//!
//! One task per connection runs: hello, rate limit, version, window gate (before any DH), message
//! 1, key and PSK checks, message 2, the phone's first transport message, then (for pairing) the
//! user's confirmation, and finally the session loop. Nothing is trusted before the phone's first
//! transport message authenticates; nothing is stored before the user confirms.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cooldown_core::SyncPayload;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, WriteHalf};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use zeroize::Zeroizing;

use crate::clock::{with_deadline, SyncClock};
use crate::codec::b64_encode;
use crate::error::ServerError;
use crate::frame::{encode_frame, FrameReader, MsgType, MAX_BODY_LEN};
use crate::hello::{ClientHello, Mode, Status, CLIENT_HELLO_LEN, PROTO_MAX_SUPPORTED};
use crate::keys::{is_small_order, random_bytes, random_secret, DeviceId, StaticKeypair};
use crate::limits::{Limits, MAX_PEERS, MAX_SESSION_MESSAGES};
use crate::lock;
use crate::message::{
    valid_name, AckBody, AlertSetBody, EmptyBody, ErrorCode, HelloBody, Msg2, PairMsg1, PingBody,
    ReconnectMsg1, RevokeBody, SyncBody, APP_NAME,
};
use crate::noise::{
    confirmation_code, format_confirmation_code, prologue, Handshake, HandshakeParams,
    MAX_HANDSHAKE_MESSAGE,
};
use crate::pairing::{Admit, LockResult, PairingWindow};
use crate::qr::QrPayload;
use crate::ratelimit::{AdmitGuard, ConnGate, HandshakeLimiter, LogGate};
use crate::sanitize::sanitize_payload;
use crate::session::{Channel, ChannelError, Incoming};
use crate::trust::{PeerRecord, SessionUpdate, TrustStore};

pub use crate::pairing::WindowClosed;

/// Settings for [`PcServer`].
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// The PC's display name (at most 32 characters), shown on the phone.
    pub pc_name: String,
    /// `windows` in production; defaults to the build target's OS name.
    pub platform: String,
    /// Timeouts and abuse limits. Defaults are the protocol document's numbers.
    pub limits: Limits,
    /// Resume an earlier `(epoch, rev)` sequence (persist the stamp [`PcServer::push_sync`]
    /// returns). `None`, the default, starts a new random epoch at revision 0, which phones always
    /// accept; resuming matters only to avoid resending after a restart. After a database restore,
    /// pass `None` (or a fresh epoch) so a restored PC cannot be ignored by phones that hold
    /// newer revisions of the old epoch.
    pub resume: Option<SyncStamp>,
}

impl ServerConfig {
    /// Defaults with the given PC name.
    pub fn new(pc_name: impl Into<String>) -> Self {
        ServerConfig {
            pc_name: pc_name.into(),
            platform: std::env::consts::OS.to_owned(),
            limits: Limits::default(),
            resume: None,
        }
    }
}

/// Position of a pushed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncStamp {
    /// The epoch.
    pub epoch: u64,
    /// The revision within the epoch.
    pub rev: u64,
}

/// Things the app may want to show or react to. Carries names and the confirmation code (it must
/// reach the UI) but no keys; the server never logs any of it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServerEvent {
    /// A pairing window was opened (the QR is on screen).
    PairingWindowOpened,
    /// The window closed.
    PairingWindowClosed(WindowClosed),
    /// A phone completed the handshake. Show the prompt "Pair this phone? Only confirm if this code
    /// is on YOUR phone" with the code, then call `confirm_pairing`.
    PairingConfirmationRequested {
        /// The phone's chosen name.
        name: String,
        /// The phone's platform tag.
        platform: String,
        /// The six-digit code as shown (`123 456`).
        code: String,
    },
    /// A phone was paired and stored.
    DevicePaired {
        /// Its id.
        device_id: DeviceId,
        /// Its name.
        name: String,
    },
    /// A pairing that reached the confirmation stage did not complete.
    PairingFailed {
        /// A short reason code (`rejected`, `timeout`, `disconnected`, ...).
        reason: &'static str,
    },
    /// An authenticated session became active.
    DeviceConnected {
        /// The phone.
        device_id: DeviceId,
    },
    /// A session ended.
    DeviceDisconnected {
        /// The phone.
        device_id: DeviceId,
        /// A short reason code.
        reason: &'static str,
    },
    /// A device was revoked (by the PC, or by the phone unpairing itself).
    DeviceRevoked {
        /// The device.
        device_id: DeviceId,
        /// True when the phone asked to be unpaired.
        by_phone: bool,
    },
    /// A second session from one static key replaced a live one. Detected, not prevented: it may
    /// be a clone or just a phone that reconnected before the old socket timed out.
    PossibleClone {
        /// The phone.
        device_id: DeviceId,
    },
}

/// A phone asked to toggle a reset alert. The PC is authoritative: apply it, then push a new SYNC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertRequest {
    /// The requesting phone.
    pub device_id: DeviceId,
    /// `reset:<provider>:<account>:<window>`.
    pub id: String,
    /// The requested switch position.
    pub enabled: bool,
}

/// What `open_pairing` returns.
pub struct PairingOffer {
    /// The QR text. It contains the PSK: show it in the native window only, never log it or put it
    /// on the clipboard.
    pub qr_text: Zeroizing<String>,
    /// How long the window stays open.
    pub expires_in: Duration,
}

impl std::fmt::Debug for PairingOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingOffer")
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    /// The user confirmed.
    Accept,
    /// The user rejected (or the typed code was wrong).
    Reject,
    /// The user did not answer in time.
    Timeout,
    /// The phone went away while the prompt was up.
    Gone,
}

struct Pending {
    tx: oneshot::Sender<Decision>,
    digits: String,
}

#[derive(Debug)]
enum SessionCmd {
    Revoke,
    Close(&'static str),
    SendHello,
}

struct SessionHandle {
    id: u64,
    cmd: mpsc::UnboundedSender<SessionCmd>,
}

struct SyncState {
    epoch: u64,
    rev: u64,
    body: Vec<u8>,
}

struct Shared {
    pc_name: String,
    platform: String,
    limits: Limits,
    clock: Arc<dyn SyncClock>,
    identity: StaticKeypair,
    pc_id: DeviceId,
    trust: Arc<dyn TrustStore>,
    window: Mutex<PairingWindow>,
    pending: Mutex<Option<Pending>>,
    gate: Arc<ConnGate>,
    limiter: Mutex<HandshakeLimiter>,
    sessions: Mutex<HashMap<DeviceId, SessionHandle>>,
    stamp: Mutex<SyncStamp>,
    sync_tx: watch::Sender<Option<Arc<SyncState>>>,
    events: broadcast::Sender<ServerEvent>,
    alerts_tx: mpsc::Sender<AlertRequest>,
    shutdown_tx: watch::Sender<bool>,
    next_session: AtomicU64,
    logs: LogGate,
}

impl Shared {
    fn emit(&self, event: ServerEvent) {
        let _ = self.events.send(event);
    }

    /// One log line per event code, rate limited. Codes, a 4-byte device prefix and the IP only.
    fn note(&self, code: &'static str, warn: bool, ip: Option<IpAddr>, device: Option<&DeviceId>) {
        if !self.logs.allow(code, self.clock.mono()) {
            return;
        }
        let ip = ip.map(|ip| ip.to_string());
        let device = device.map(DeviceId::short);
        if warn {
            tracing::warn!(
                code,
                ip = ip.as_deref(),
                device = device.as_deref(),
                "sync event"
            );
        } else {
            tracing::debug!(
                code,
                ip = ip.as_deref(),
                device = device.as_deref(),
                "sync event"
            );
        }
    }

    fn pc_hello(&self, proto: u8) -> HelloBody {
        let stamp = *lock(&self.stamp);
        HelloBody {
            app: APP_NAME.to_owned(),
            platform: self.platform.clone(),
            proto,
            now_ms: self.clock.wall_ms(),
            last_epoch: stamp.epoch,
            last_rev: stamp.rev,
            beacon_key: self
                .trust
                .beacon_key()
                .ok()
                .flatten()
                .map(|key| b64_encode(&key)),
        }
    }

    /// Replace the beacon key after a revocation (or clear it when no peers remain) and tell the
    /// phones that are still connected.
    fn rotate_beacon_key(&self) -> Result<(), ServerError> {
        let remaining = self.trust.list()?;
        if remaining.is_empty() {
            self.trust.set_beacon_key(None)?;
            return Ok(());
        }
        let key = random_secret()?;
        self.trust.set_beacon_key(Some(*key))?;
        let sessions = lock(&self.sessions);
        for peer in &remaining {
            if let Some(handle) = sessions.get(&peer.device_id) {
                let _ = handle.cmd.send(SessionCmd::SendHello);
            }
        }
        Ok(())
    }

    fn close_window(&self, generation: u64, reason: WindowClosed) {
        if lock(&self.window).close(generation) {
            self.emit(ServerEvent::PairingWindowClosed(reason));
        }
    }
}

/// Counts a failed PAIR handshake against the window unless disarmed. Dropping it armed covers
/// every early exit: bad message 1, timeouts, a client that simply hangs up.
struct AttemptGuard {
    shared: Arc<Shared>,
    generation: u64,
    armed: bool,
}

impl AttemptGuard {
    fn disarm(mut self) -> u64 {
        self.armed = false;
        self.generation
    }
}

impl Drop for AttemptGuard {
    fn drop(&mut self) {
        if self.armed && lock(&self.shared.window).record_failure(self.generation) {
            self.shared.note("window_burned", true, None, None);
            self.shared
                .emit(ServerEvent::PairingWindowClosed(WindowClosed::Burned));
        }
    }
}

/// Held from the moment message 1 locks the window. Unless [`LockedGuard::complete`] is called,
/// dropping it burns the window: the PSK was consumed by a handshake that did not end in a
/// confirmed pairing.
struct LockedGuard {
    shared: Arc<Shared>,
    generation: u64,
    done: bool,
    reason: &'static str,
}

impl LockedGuard {
    fn complete(mut self) {
        self.done = true;
        self.shared
            .close_window(self.generation, WindowClosed::Completed);
    }
}

impl Drop for LockedGuard {
    fn drop(&mut self) {
        if !self.done {
            self.shared.note("pairing_failed", true, None, None);
            self.shared
                .close_window(self.generation, WindowClosed::Rejected);
            self.shared.emit(ServerEvent::PairingFailed {
                reason: self.reason,
            });
        }
    }
}

/// Why a connection task stopped. Only the code is ever logged.
struct Stop(&'static str);

impl From<ChannelError> for Stop {
    fn from(error: ChannelError) -> Self {
        Stop(error.code())
    }
}

/// The PC's sync server.
pub struct PcServer {
    shared: Arc<Shared>,
    addrs: Vec<SocketAddr>,
    alerts_rx: Mutex<Option<mpsc::Receiver<AlertRequest>>>,
}

impl Drop for PcServer {
    fn drop(&mut self) {
        let _ = self.shared.shutdown_tx.send(true);
    }
}

impl PcServer {
    /// Bind one listener per address in `bind_ips` on `port` (0 picks a free port, shared by all
    /// listeners) and start accepting. Per the protocol document, pass only addresses on Private
    /// or Domain networks; discovering those is the app's job.
    pub async fn start(
        config: ServerConfig,
        identity: StaticKeypair,
        trust: Arc<dyn TrustStore>,
        clock: Arc<dyn SyncClock>,
        bind_ips: &[IpAddr],
        port: u16,
    ) -> Result<PcServer, ServerError> {
        if !valid_name(&config.pc_name) {
            return Err(ServerError::Payload("invalid PC name"));
        }
        if bind_ips.is_empty() {
            return Err(ServerError::NoAddress);
        }
        let mut listeners = Vec::new();
        let mut port = port;
        for ip in bind_ips {
            let listener = TcpListener::bind(SocketAddr::new(*ip, port)).await?;
            port = listener.local_addr()?.port();
            listeners.push(listener);
        }
        let addrs: Vec<SocketAddr> = listeners
            .iter()
            .map(TcpListener::local_addr)
            .collect::<Result<_, _>>()?;

        let now = clock.mono();
        let start_stamp = match config.resume {
            Some(stamp) => stamp,
            None => {
                let mut bytes = [0u8; 8];
                random_bytes(&mut bytes)?;
                SyncStamp {
                    epoch: u64::from_be_bytes(bytes),
                    rev: 0,
                }
            }
        };
        let limits = config.limits.normalized();
        let (events, _) = broadcast::channel(64);
        let (alerts_tx, alerts_rx) = mpsc::channel(64);
        let (sync_tx, _) = watch::channel(None);
        let (shutdown_tx, _) = watch::channel(false);
        let pc_id = identity.device_id();
        let shared = Arc::new(Shared {
            pc_name: config.pc_name,
            platform: config.platform,
            window: Mutex::new(PairingWindow::new(
                limits.pairing_window,
                limits.pair_failures_per_window,
            )),
            pending: Mutex::new(None),
            gate: ConnGate::new(
                limits.max_unauthenticated,
                limits.max_unauthenticated_per_ip,
            ),
            limiter: Mutex::new(HandshakeLimiter::new(
                limits.handshakes_per_minute_per_ip,
                limits.handshakes_per_minute_total,
                now,
            )),
            limits,
            clock,
            identity,
            pc_id,
            trust,
            sessions: Mutex::new(HashMap::new()),
            stamp: Mutex::new(start_stamp),
            sync_tx,
            events,
            alerts_tx,
            shutdown_tx,
            next_session: AtomicU64::new(1),
            logs: LogGate::new(20),
        });
        for listener in listeners {
            tokio::spawn(accept_loop(Arc::clone(&shared), listener));
        }
        Ok(PcServer {
            shared,
            addrs,
            alerts_rx: Mutex::new(Some(alerts_rx)),
        })
    }

    /// The addresses actually bound.
    pub fn local_addrs(&self) -> &[SocketAddr] {
        &self.addrs
    }

    /// The TCP port (the same on every listener).
    pub fn port(&self) -> u16 {
        self.addrs.first().map_or(0, SocketAddr::port)
    }

    /// This PC's device id.
    pub fn device_id(&self) -> DeviceId {
        self.shared.pc_id
    }

    /// The `(epoch, rev)` of the newest state pushed so far.
    pub fn sync_stamp(&self) -> SyncStamp {
        *lock(&self.shared.stamp)
    }

    /// Subscribe to events. Subscribe before opening a pairing window so no event is missed.
    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.shared.events.subscribe()
    }

    /// Take the receiver of phone-initiated alert toggles. Only the first call returns it.
    pub fn take_alert_requests(&self) -> Option<mpsc::Receiver<AlertRequest>> {
        lock(&self.alerts_rx).take()
    }

    /// Open the pairing window: new single-use PSK and `pair_id`, 120 s, one at a time. Returns
    /// the QR text. `advertise` lists the LAN addresses to put in it (1 to 4); pass an empty slice
    /// to advertise the bound addresses.
    pub fn open_pairing(&self, advertise: &[IpAddr]) -> Result<PairingOffer, ServerError> {
        let shared = &self.shared;
        if shared.trust.list()?.len() >= MAX_PEERS {
            return Err(ServerError::PeerLimit);
        }
        let addrs: Vec<IpAddr> = if advertise.is_empty() {
            self.addrs
                .iter()
                .map(SocketAddr::ip)
                .filter(|ip| !ip.is_unspecified())
                .collect()
        } else {
            advertise.to_vec()
        };
        if addrs.is_empty() {
            return Err(ServerError::NoAddress);
        }
        let psk = random_secret()?;
        let mut pair_id = [0u8; 16];
        loop {
            random_bytes(&mut pair_id)?;
            if pair_id.iter().any(|&b| b != 0) {
                break;
            }
        }
        let payload = QrPayload {
            pc_static_pub: *shared.identity.public(),
            psk: psk.clone(),
            pair_id,
            tcp_port: self.port(),
            addrs,
        };
        let qr_text = payload
            .encode()
            .map_err(|_| ServerError::Payload("cannot encode the QR payload"))?;
        let generation = lock(&shared.window)
            .open(pair_id, psk, shared.clock.mono())
            .ok_or(ServerError::PairingAlreadyOpen)?;
        shared.emit(ServerEvent::PairingWindowOpened);
        let ttl = shared.limits.pairing_window;
        let task_shared = Arc::clone(shared);
        tokio::spawn(async move {
            let mut shutdown = task_shared.shutdown_tx.subscribe();
            tokio::select! {
                () = task_shared.clock.sleep(ttl) => {}
                _ = shutdown.changed() => return,
            }
            let expired =
                lock(&task_shared.window).expire_generation(generation, task_shared.clock.mono());
            if expired {
                task_shared.emit(ServerEvent::PairingWindowClosed(WindowClosed::Expired));
            }
        });
        Ok(PairingOffer {
            qr_text,
            expires_in: ttl,
        })
    }

    /// Hide the QR: close an open window, or reject a pairing that is waiting for confirmation.
    pub fn close_pairing(&self) {
        let hidden = lock(&self.shared.window).hide();
        if let Some(_generation) = hidden {
            self.shared
                .emit(ServerEvent::PairingWindowClosed(WindowClosed::Hidden));
            return;
        }
        let _ = self.decide(Decision::Reject);
    }

    fn decide(&self, decision: Decision) -> Result<(), ServerError> {
        let pending = lock(&self.shared.pending)
            .take()
            .ok_or(ServerError::NoPendingPairing)?;
        let _ = pending.tx.send(decision);
        Ok(())
    }

    /// The user clicked Confirm (`true`) or Reject (`false`) on the pairing prompt.
    pub fn confirm_pairing(&self, accept: bool) -> Result<(), ServerError> {
        self.decide(if accept {
            Decision::Accept
        } else {
            Decision::Reject
        })
    }

    /// The user typed the code shown on the phone. A mismatch rejects the pairing (and burns the
    /// window): someone who cannot read the right code off the right phone does not get in.
    pub fn confirm_pairing_with_code(&self, typed: &str) -> Result<(), ServerError> {
        let typed: String = typed.chars().filter(char::is_ascii_digit).collect();
        let expected = lock(&self.shared.pending)
            .as_ref()
            .map(|p| p.digits.clone())
            .ok_or(ServerError::NoPendingPairing)?;
        if bool::from(typed.as_bytes().ct_eq(expected.as_bytes())) {
            self.decide(Decision::Accept)
        } else {
            let _ = self.decide(Decision::Reject);
            Err(ServerError::WrongCode)
        }
    }

    /// Push the PC's current state to every authenticated phone. The payload is rebuilt through
    /// the allowlist and scrubber first. Returns its `(epoch, rev)`. Phones receive at most one
    /// SYNC per 2 s each, newest first.
    pub fn push_sync(&self, payload: &SyncPayload) -> Result<SyncStamp, ServerError> {
        // The sender id is always this PC's device id; the phone checks it matches the PC it
        // paired with.
        let mut stamped = payload.clone();
        stamped.device_id = self.shared.pc_id.to_hex();
        let clean = sanitize_payload(&stamped).map_err(|e| {
            ServerError::Payload(match e {
                crate::sanitize::SanitizeError::TooMany => "too many entries",
                crate::sanitize::SanitizeError::Identifier => "an identifier looks like a secret",
                crate::sanitize::SanitizeError::Invalid => "payload failed validation",
            })
        })?;
        let mut stamp = lock(&self.shared.stamp);
        let next = SyncStamp {
            epoch: stamp.epoch,
            rev: stamp
                .rev
                .checked_add(1)
                .ok_or(ServerError::Payload("revision counter exhausted"))?,
        };
        let body = serde_json::to_vec(&SyncBody {
            epoch: next.epoch,
            rev: next.rev,
            payload: clean,
        })
        .map_err(|_| ServerError::Payload("payload does not serialize"))?;
        if body.len() > MAX_BODY_LEN {
            return Err(ServerError::Payload("payload exceeds 60 KiB"));
        }
        *stamp = next;
        self.shared.sync_tx.send_replace(Some(Arc::new(SyncState {
            epoch: next.epoch,
            rev: next.rev,
            body,
        })));
        Ok(next)
    }

    /// Every paired phone.
    pub fn peers(&self) -> Result<Vec<PeerRecord>, ServerError> {
        Ok(self.shared.trust.list()?)
    }

    /// Phones with a live authenticated session.
    pub fn connected_peers(&self) -> Vec<DeviceId> {
        lock(&self.shared.sessions).keys().copied().collect()
    }

    /// Remove a phone: delete its record (so later attempts get a generic `FAIL`), tell it with
    /// `REVOKE` if it is connected, close the session, and rotate the beacon key.
    pub fn revoke(&self, id: &DeviceId) -> Result<(), ServerError> {
        let shared = &self.shared;
        if !shared.trust.revoke(id)? {
            return Err(ServerError::UnknownDevice);
        }
        if let Some(handle) = lock(&shared.sessions).remove(id) {
            let _ = handle.cmd.send(SessionCmd::Revoke);
        }
        shared.rotate_beacon_key()?;
        shared.emit(ServerEvent::DeviceRevoked {
            device_id: *id,
            by_phone: false,
        });
        shared.note("device_revoked", false, None, Some(id));
        Ok(())
    }

    /// Stop accepting and close every session.
    pub fn shutdown(&self) {
        let _ = self.shared.shutdown_tx.send(true);
    }

    /// Unauthenticated connections currently held (for tests and diagnostics).
    pub fn unauthenticated_connections(&self) -> usize {
        self.shared.gate.unauthenticated()
    }
}

fn spawn_connection<S>(shared: Arc<Shared>, stream: S, ip: IpAddr)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let Some(guard) = shared.gate.try_admit(ip) else {
        shared.note("conn_cap", true, Some(ip), None);
        return;
    };
    tokio::spawn(async move {
        let mut shutdown = shared.shutdown_tx.subscribe();
        let result = tokio::select! {
            result = run_connection(&shared, stream, ip, guard) => result,
            _ = shutdown.changed() => Err(Stop("shutdown")),
        };
        if let Err(Stop(code)) = result {
            shared.note(code, code_is_suspicious(code), Some(ip), None);
        }
    });
}

fn code_is_suspicious(code: &str) -> bool {
    matches!(
        code,
        "msg1_auth" | "replay" | "unknown_peer" | "rate_limited" | "decrypt" | "window_burned"
    )
}

async fn accept_loop(shared: Arc<Shared>, listener: TcpListener) {
    let mut shutdown = shared.shutdown_tx.subscribe();
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, addr)) => {
                    let _ = stream.set_nodelay(true);
                    spawn_connection(Arc::clone(&shared), stream, addr.ip().to_canonical());
                }
                Err(_) => {
                    // Typically descriptor exhaustion; back off instead of spinning.
                    shared.clock.sleep(Duration::from_millis(100)).await;
                }
            },
        }
    }
}

/// Send a status byte (and the VERSION extra byte) and ignore failures: the peer may be gone.
/// How long a rejected connection is drained before it is dropped (see [`reply_status`]).
const LINGER: Duration = Duration::from_secs(1);
/// Most bytes drained from a rejected connection.
const LINGER_MAX_BYTES: usize = 8 * 1024;

/// Send a status byte (and an optional extra byte), then close gracefully.
///
/// The PC often answers before it has read everything the phone sent (message 1 is still in the
/// receive buffer when the window gate says no). Closing a socket with unread data makes Windows
/// send a TCP reset instead of a FIN, and the reset discards the status byte before the phone can
/// read it. So: write, half-close our side, then read and discard what the phone already sent, until
/// it closes or `LINGER` / `LINGER_MAX_BYTES` runs out, and only then drop the socket.
async fn reply_status<R, W>(
    shared: &Shared,
    reader: &mut FrameReader<R>,
    writer: &mut WriteHalf<W>,
    status: Status,
    extra: Option<u8>,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut bytes = vec![status.to_byte()];
    bytes.extend(extra);
    let sent = with_deadline(&*shared.clock, shared.limits.write_timeout, async {
        writer.write_all(&bytes).await?;
        writer.flush().await
    })
    .await;
    if !matches!(sent, Ok(Ok(()))) {
        return;
    }
    let _ = with_deadline(&*shared.clock, LINGER, async {
        let _ = writer.shutdown().await;
        // A short read at the end is an error too, which also ends the drain.
        for _ in 0..LINGER_MAX_BYTES / 256 {
            if reader.read_raw(256).await.is_err() {
                break;
            }
        }
    })
    .await;
}

async fn run_connection<S>(
    sh: &Arc<Shared>,
    stream: S,
    ip: IpAddr,
    guard: AdmitGuard,
) -> Result<(), Stop>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let limits = &sh.limits;
    let clock: &dyn SyncClock = &*sh.clock;
    let accepted = clock.mono();
    let handshake_left = || (accepted + limits.handshake_deadline).saturating_sub(clock.mono());
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = FrameReader::new(read_half, MAX_HANDSHAKE_MESSAGE);

    // 1. The cleartext hello. Anything wrong here (including a bad magic, e.g. an HTTP request)
    //    closes silently: no reply, no oracle.
    let raw = with_deadline(
        clock,
        limits.hello_deadline,
        reader.read_raw(CLIENT_HELLO_LEN),
    )
    .await
    .map_err(|_| Stop("hello_timeout"))?
    .map_err(|_| Stop("hello_read"))?;
    let hello = ClientHello::decode(&raw).map_err(|_| Stop("hello_invalid"))?;
    let hello_bytes: [u8; CLIENT_HELLO_LEN] = raw.try_into().map_err(|_| Stop("hello_invalid"))?;

    // 2. Rate limit, before any state or DH.
    if !lock(&sh.limiter).admit(ip, clock.mono()) {
        reply_status(sh, &mut reader, &mut writer, Status::RateLimited, None).await;
        return Err(Stop("rate_limited"));
    }

    // 3. Strict version negotiation. The offer is also bound into the prologue below.
    let Some(proto) = hello.negotiate() else {
        reply_status(
            sh,
            &mut reader,
            &mut writer,
            Status::Version,
            Some(PROTO_MAX_SUPPORTED),
        )
        .await;
        return Err(Stop("version"));
    };

    // 4. The window gate, before any DH.
    let mut attempt: Option<AttemptGuard> = None;
    if hello.mode == Mode::Pair {
        let admitted = lock(&sh.window).admit(&hello.pair_id, clock.mono());
        match admitted {
            Admit::Proceed(generation) => {
                attempt = Some(AttemptGuard {
                    shared: Arc::clone(sh),
                    generation,
                    armed: true,
                });
            }
            Admit::NotPairing => {
                reply_status(sh, &mut reader, &mut writer, Status::NotPairing, None).await;
                return Err(Stop("not_pairing"));
            }
            Admit::Busy => {
                reply_status(sh, &mut reader, &mut writer, Status::Busy, None).await;
                return Err(Stop("busy"));
            }
        }
    }

    // 5. Message 1 (at most 512 bytes), within the overall handshake deadline.
    let msg1 = match with_deadline(clock, handshake_left(), reader.read_frame()).await {
        Ok(Ok(frame)) => frame,
        _ => {
            reply_status(sh, &mut reader, &mut writer, Status::Fail, None).await;
            return Err(Stop("msg1_read"));
        }
    };

    // 6. Noise. For PAIR the PSK comes from the window; it is checked inside message 1, so a
    //    wrong PSK fails here, before the PC says anything.
    let psk = match &attempt {
        Some(a) => {
            let psk = lock(&sh.window).psk(a.generation);
            if psk.is_none() {
                reply_status(sh, &mut reader, &mut writer, Status::NotPairing, None).await;
                return Err(Stop("not_pairing"));
            }
            psk
        }
        None => None,
    };
    let pro = prologue(&hello_bytes, &sh.pc_id);
    let responder = Handshake::responder(&HandshakeParams {
        mode: hello.mode,
        local: &sh.identity,
        remote_public: None,
        psk: psk.as_deref(),
        prologue: &pro,
    })
    .map_err(|_| Stop("setup"))?;
    drop(psk);
    // A failed read consumes the handshake state: it cannot be retried.
    let Ok((mut hs, payload)) = responder.read(&msg1) else {
        reply_status(sh, &mut reader, &mut writer, Status::Fail, None).await;
        return Err(Stop("msg1_auth"));
    };
    let Some(remote) = hs.remote_static().filter(|key| !is_small_order(key)) else {
        reply_status(sh, &mut reader, &mut writer, Status::Fail, None).await;
        return Err(Stop("msg1_auth"));
    };
    let peer_id = DeviceId::from_public(&remote);

    let mut locked: Option<LockedGuard> = None;
    let mut pair_request: Option<PairMsg1> = None;
    match hello.mode {
        Mode::Reconnect => {
            let known = serde_json::from_slice::<ReconnectMsg1>(&payload)
                .ok()
                .and_then(|m| {
                    let record = sh.trust.get(&peer_id).ok().flatten()?;
                    bool::from(record.static_pub.ct_eq(&remote)).then_some(m.ts)
                });
            let Some(ts) = known else {
                reply_status(sh, &mut reader, &mut writer, Status::Fail, None).await;
                return Err(Stop("unknown_peer"));
            };
            // Persisted before replying: replaying this message 1 now fails.
            if !sh.trust.advance_ts(&peer_id, ts).unwrap_or(false) {
                reply_status(sh, &mut reader, &mut writer, Status::Fail, None).await;
                return Err(Stop("replay"));
            }
        }
        Mode::Pair => {
            let Some(request) = serde_json::from_slice::<PairMsg1>(&payload)
                .ok()
                .filter(PairMsg1::is_valid)
            else {
                reply_status(sh, &mut reader, &mut writer, Status::Fail, None).await;
                return Err(Stop("msg1_payload"));
            };
            let has_room = match sh.trust.get(&peer_id) {
                Ok(Some(_)) => true,
                Ok(None) => sh.trust.list().is_ok_and(|l| l.len() < MAX_PEERS),
                Err(_) => false,
            };
            if !has_room {
                reply_status(sh, &mut reader, &mut writer, Status::Fail, None).await;
                return Err(Stop("peer_limit"));
            }
            let Some(attempt) = attempt.take() else {
                return Err(Stop("setup"));
            };
            let generation = attempt.generation;
            let lock_result = lock(&sh.window).try_lock(generation, clock.mono());
            match lock_result {
                LockResult::Locked => {
                    // Message 1 verified: the window is now single-use and locked.
                    let _ = attempt.disarm();
                    locked = Some(LockedGuard {
                        shared: Arc::clone(sh),
                        generation,
                        done: false,
                        reason: "disconnected",
                    });
                }
                LockResult::Busy => {
                    // A valid PSK that lost a race is not a failed attempt.
                    let _ = attempt.disarm();
                    reply_status(sh, &mut reader, &mut writer, Status::Busy, None).await;
                    return Err(Stop("busy"));
                }
                LockResult::NotPairing => {
                    let _ = attempt.disarm();
                    reply_status(sh, &mut reader, &mut writer, Status::NotPairing, None).await;
                    return Err(Stop("not_pairing"));
                }
            }
            pair_request = Some(request);
        }
    }

    // 7. Message 2. Pairing withholds the beacon key until the user confirms.
    let msg2 = Msg2 {
        pc_name: (hello.mode == Mode::Pair).then(|| sh.pc_name.clone()),
        proto,
        server_time_ms: clock.wall_ms(),
    };
    let msg2_json = serde_json::to_vec(&msg2).map_err(|_| Stop("setup"))?;
    let msg2_bytes = hs.write(&msg2_json).map_err(|_| Stop("setup"))?;
    let mut out = vec![Status::Ok.to_byte()];
    out.extend(encode_frame(&msg2_bytes).map_err(|_| Stop("setup"))?);
    with_deadline(clock, handshake_left(), async {
        writer.write_all(&out).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| Stop("msg2_timeout"))?
    .map_err(|_| Stop("msg2_write"))?;
    let established = hs.finish().map_err(|_| Stop("setup"))?;
    let handshake_hash = established.handshake_hash;
    let mut channel = Channel::new(
        reader,
        writer,
        established.transport,
        Arc::clone(&sh.clock),
        limits.write_timeout,
    );

    // 8. The phone's first transport message must be HELLO: key confirmation.
    let first = with_deadline(clock, limits.first_message_deadline, channel.recv())
        .await
        .map_err(|_| Stop("first_timeout"))?;
    let first = match first {
        Ok(message) => message,
        Err(error) => {
            if let Some(code) = error.reply() {
                channel.send_error(code).await;
            }
            return Err(error.into());
        }
    };
    let phone_hello = match check_hello(&first, proto) {
        Ok(hello) => hello,
        Err(code) => {
            channel.send_error(code).await;
            return Err(Stop("first_invalid"));
        }
    };

    // 9. Pairing needs the user; reconnecting is already authenticated.
    if let (Some(request), Some(mut locked_guard)) = (pair_request, locked.take()) {
        let code = confirmation_code(&handshake_hash);
        let decision = ask_user(sh, &mut channel, &request, code).await;
        match decision {
            Decision::Accept => {}
            other => {
                locked_guard.reason = match other {
                    Decision::Timeout => "timeout",
                    Decision::Gone => "disconnected",
                    _ => "rejected",
                };
                let _ = channel
                    .send_json(MsgType::Revoke, &RevokeBody::new(locked_guard.reason))
                    .await;
                channel.shutdown().await;
                return Err(Stop("pair_not_confirmed"));
            }
        }
        let mut record = PeerRecord::new(
            remote,
            request.name.clone(),
            request.platform.clone(),
            clock.wall_ms(),
            request.ts,
            proto,
        );
        if let Ok(Some(existing)) = sh.trust.get(&peer_id) {
            record.paired_at_ms = existing.paired_at_ms;
        }
        sh.trust.upsert(record).map_err(|_| Stop("store"))?;
        if sh.trust.beacon_key().map_err(|_| Stop("store"))?.is_none() {
            let key = random_secret().map_err(|_| Stop("rng"))?;
            sh.trust
                .set_beacon_key(Some(*key))
                .map_err(|_| Stop("store"))?;
        }
        locked_guard.complete();
        if channel
            .send_json(MsgType::PairOk, &EmptyBody {})
            .await
            .is_err()
        {
            // The phone never learned it is paired, so do not keep a record for it.
            let _ = sh.trust.revoke(&peer_id);
            let _ = sh.rotate_beacon_key();
            return Err(Stop("pair_ok_write"));
        }
        sh.emit(ServerEvent::DevicePaired {
            device_id: peer_id,
            name: request.name,
        });
        sh.note("device_paired", false, Some(ip), Some(&peer_id));
    }

    run_authenticated(sh, channel, peer_id, proto, &phone_hello, guard).await;
    Ok(())
}

/// Validate the phone's first message as a HELLO for the negotiated version.
fn check_hello(first: &Incoming, proto: u8) -> Result<HelloBody, ErrorCode> {
    if first.msg_type != MsgType::Hello {
        return Err(ErrorCode::Unexpected);
    }
    let hello: HelloBody = first.json().map_err(|_| ErrorCode::Malformed)?;
    if !hello.is_valid() || hello.beacon_key.is_some() {
        return Err(ErrorCode::Malformed);
    }
    if hello.proto != proto {
        return Err(ErrorCode::Version);
    }
    Ok(hello)
}

/// Show the code and wait for the user (up to the confirm timeout).
async fn ask_user<S: AsyncRead + AsyncWrite + Unpin>(
    sh: &Arc<Shared>,
    channel: &mut Channel<S>,
    request: &PairMsg1,
    code: u32,
) -> Decision {
    let (tx, mut rx) = oneshot::channel();
    *lock(&sh.pending) = Some(Pending {
        tx,
        digits: format!("{code:06}"),
    });
    sh.emit(ServerEvent::PairingConfirmationRequested {
        name: request.name.clone(),
        platform: request.platform.clone(),
        code: format_confirmation_code(code),
    });
    let timeout = sh.clock.sleep(sh.limits.confirm_timeout);
    tokio::pin!(timeout);
    let decision = loop {
        tokio::select! {
            decided = &mut rx => break decided.unwrap_or(Decision::Reject),
            () = &mut timeout => break Decision::Timeout,
            incoming = channel.recv() => match incoming {
                // The phone may ping while the user decides; anything else is a violation.
                Ok(message) if message.msg_type == MsgType::Ping => {
                    if let Ok(ping) = message.json::<PingBody>() {
                        let _ = channel.send_json(MsgType::Pong, &ping).await;
                    }
                }
                Ok(_) => {
                    channel.send_error(ErrorCode::Unexpected).await;
                    break Decision::Reject;
                }
                Err(_) => break Decision::Gone,
            },
        }
    };
    *lock(&sh.pending) = None;
    decision
}

/// The authenticated phase: register the session, send the PC's HELLO and run the loop.
async fn run_authenticated<S>(
    sh: &Arc<Shared>,
    mut channel: Channel<S>,
    id: DeviceId,
    proto: u8,
    phone_hello: &HelloBody,
    guard: AdmitGuard,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // Authenticated now: this connection no longer counts against the unauthenticated caps.
    drop(guard);
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let session_id = sh.next_session.fetch_add(1, Ordering::Relaxed);
    let replaced = lock(&sh.sessions).insert(
        id,
        SessionHandle {
            id: session_id,
            cmd: cmd_tx,
        },
    );
    if let Some(old) = replaced {
        let _ = old.cmd.send(SessionCmd::Close("replaced"));
        sh.emit(ServerEvent::PossibleClone { device_id: id });
        sh.note("possible_clone", true, None, Some(&id));
    }
    // Re-check after registering: a revoke that raced with activation either removed this handle
    // (and queued Revoke) or has already deleted the record, which is caught here.
    let still_paired = sh.trust.get(&id).ok().flatten().is_some();
    let reason = if still_paired {
        let _ = sh.trust.record_session(
            &id,
            SessionUpdate {
                last_seen_ms: Some(sh.clock.wall_ms()),
                proto_floor: Some(proto),
                beacon_key: None,
            },
        );
        sh.emit(ServerEvent::DeviceConnected { device_id: id });
        sh.note("device_connected", false, None, Some(&id));
        let hello = sh.pc_hello(proto);
        if channel.send_json(MsgType::Hello, &hello).await.is_ok() {
            let applied = (phone_hello.last_epoch, phone_hello.last_rev);
            run_session(sh, &mut channel, id, proto, applied, &mut cmd_rx).await
        } else {
            "write"
        }
    } else {
        "revoked"
    };
    {
        let mut sessions = lock(&sh.sessions);
        if sessions.get(&id).is_some_and(|h| h.id == session_id) {
            sessions.remove(&id);
        }
    }
    channel.shutdown().await;
    sh.emit(ServerEvent::DeviceDisconnected {
        device_id: id,
        reason,
    });
    sh.note(reason, false, None, Some(&id));
}

/// Per-session state for the PC's loop.
struct PcSession<'a, S> {
    sh: &'a Arc<Shared>,
    channel: &'a mut Channel<S>,
    id: DeviceId,
    proto: u8,
    /// The SYNC awaiting its ACK: `(epoch, rev, sent_at)`.
    inflight: Option<(u64, u64, Duration)>,
    /// The newest `(epoch, rev)` pushed on this session (or already held by the phone).
    last_key: (u64, u64),
    /// The last 64 `op_id`s, for idempotent `ALERT_SET`.
    recent_ops: VecDeque<u64>,
}

async fn run_session<S>(
    sh: &Arc<Shared>,
    channel: &mut Channel<S>,
    id: DeviceId,
    proto: u8,
    applied: (u64, u64),
    cmd_rx: &mut mpsc::UnboundedReceiver<SessionCmd>,
) -> &'static str
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let limits = &sh.limits;
    let clock = &*sh.clock;
    let started = clock.mono();
    let mut last_rx = started;
    let mut last_ping = started;
    let mut last_push: Option<Duration> = None;
    let mut ping_n = 0u64;
    let mut sync_rx = sh.sync_tx.subscribe();
    let mut shutdown = sh.shutdown_tx.subscribe();
    let mut session = PcSession {
        sh,
        channel,
        id,
        proto,
        inflight: None,
        last_key: applied,
        recent_ops: VecDeque::with_capacity(64),
    };
    loop {
        let now = clock.mono();
        let age = now.saturating_sub(started);
        if age >= limits.session_lifetime
            || session.channel.total_messages() >= MAX_SESSION_MESSAGES
        {
            return "lifetime";
        }
        let silent_for = now.saturating_sub(last_rx);
        if silent_for >= limits.silence_timeout {
            return "silence";
        }
        let mut wake = (limits.silence_timeout - silent_for).min(limits.session_lifetime - age);
        if let Some((_, _, since)) = session.inflight {
            let waited = now.saturating_sub(since);
            if waited >= limits.ack_timeout {
                session.channel.send_error(ErrorCode::Timeout).await;
                return "ack_timeout";
            }
            wake = wake.min(limits.ack_timeout - waited);
        } else {
            let current = sync_rx.borrow_and_update().clone();
            if let Some(state) = current {
                if (state.epoch, state.rev) != session.last_key {
                    let due = last_push.map_or(Duration::ZERO, |t| {
                        (t + limits.min_push_interval).saturating_sub(now)
                    });
                    if due.is_zero() {
                        if session
                            .channel
                            .send_bytes(MsgType::Sync, &state.body)
                            .await
                            .is_err()
                        {
                            return "write";
                        }
                        session.inflight = Some((state.epoch, state.rev, now));
                        session.last_key = (state.epoch, state.rev);
                        last_push = Some(now);
                        continue;
                    }
                    wake = wake.min(due);
                }
            }
        }
        let ping_due = (last_rx.max(last_ping) + limits.ping_after).saturating_sub(now);
        if ping_due.is_zero() {
            ping_n += 1;
            if session
                .channel
                .send_json(MsgType::Ping, &PingBody { n: ping_n })
                .await
                .is_err()
            {
                return "write";
            }
            last_ping = now;
            continue;
        }
        wake = wake.min(ping_due);

        tokio::select! {
            biased;
            _ = shutdown.changed() => return "shutdown",
            command = cmd_rx.recv() => match command {
                None => return "dropped",
                Some(SessionCmd::Close(why)) => return why,
                Some(SessionCmd::Revoke) => {
                    let _ = session
                        .channel
                        .send_json(MsgType::Revoke, &RevokeBody::new("removed"))
                        .await;
                    return "revoked";
                }
                Some(SessionCmd::SendHello) => {
                    let hello = sh.pc_hello(session.proto);
                    if session.channel.send_json(MsgType::Hello, &hello).await.is_err() {
                        return "write";
                    }
                }
            },
            incoming = session.channel.recv() => match incoming {
                Err(error) => {
                    if let Some(code) = error.reply() {
                        session.channel.send_error(code).await;
                    }
                    return error.code();
                }
                Ok(message) => {
                    last_rx = clock.mono();
                    if let Some(why) = session.handle(message).await {
                        return why;
                    }
                }
            },
            _ = sync_rx.changed() => {},
            () = clock.sleep(wake) => {},
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> PcSession<'_, S> {
    /// Handle one authenticated message. `Some(reason)` ends the session.
    async fn handle(&mut self, message: Incoming) -> Option<&'static str> {
        match message.msg_type {
            MsgType::Ack => {
                let Ok(ack) = message.json::<AckBody>() else {
                    return Some(self.fail(ErrorCode::Malformed, "malformed").await);
                };
                match self.inflight {
                    Some((epoch, rev, _)) if (epoch, rev) == (ack.epoch, ack.rev) => {
                        self.inflight = None;
                    }
                    _ => {
                        // Stale or duplicate acknowledgements are harmless; one for a revision
                        // we have not sent is not.
                        if ack.epoch == self.last_key.0 && ack.rev > self.last_key.1 {
                            return Some(self.fail(ErrorCode::Unexpected, "bad_ack").await);
                        }
                    }
                }
                None
            }
            MsgType::AlertSet => {
                let Ok(body) = message.json::<AlertSetBody>() else {
                    return Some(self.fail(ErrorCode::Malformed, "malformed").await);
                };
                if !body.is_valid() {
                    return Some(self.fail(ErrorCode::Malformed, "malformed").await);
                }
                if self.recent_ops.contains(&body.op_id) {
                    return None;
                }
                if self.recent_ops.len() == 64 {
                    self.recent_ops.pop_front();
                }
                self.recent_ops.push_back(body.op_id);
                let _ = self.sh.alerts_tx.try_send(AlertRequest {
                    device_id: self.id,
                    id: body.id,
                    enabled: body.enabled,
                });
                None
            }
            MsgType::Ping => {
                let Ok(ping) = message.json::<PingBody>() else {
                    return Some(self.fail(ErrorCode::Malformed, "malformed").await);
                };
                if self.channel.send_json(MsgType::Pong, &ping).await.is_err() {
                    return Some("write");
                }
                None
            }
            MsgType::Pong => {
                if message.json::<PingBody>().is_err() {
                    return Some(self.fail(ErrorCode::Malformed, "malformed").await);
                }
                None
            }
            MsgType::Revoke => {
                // The phone is unpairing itself. Honour it: it can only affect its own record.
                let _ = self.sh.trust.revoke(&self.id);
                let _ = self.sh.rotate_beacon_key();
                self.sh.emit(ServerEvent::DeviceRevoked {
                    device_id: self.id,
                    by_phone: true,
                });
                Some("unpaired_by_phone")
            }
            MsgType::Error => Some("peer_error"),
            // A second HELLO, PAIR_OK, or SYNC from the phone is not part of the protocol.
            MsgType::Hello | MsgType::PairOk | MsgType::Sync => {
                Some(self.fail(ErrorCode::Unexpected, "unexpected").await)
            }
        }
    }

    async fn fail(&mut self, code: ErrorCode, reason: &'static str) -> &'static str {
        self.channel.send_error(code).await;
        reason
    }
}
