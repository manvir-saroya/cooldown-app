//! Discovery: finding a paired PC on the local network after its address changed.
//!
//! The design is in `docs/pairing-protocol.md` section 4: a private UDP beacon (group
//! `224.0.0.167`, its own port) carrying only a magic, a version, a kind, the TCP port, a random
//! nonce and a 16-byte keyed tag over those fields, keyed with the `beacon_key` that this crate
//! already generates, stores, rotates on revocation and delivers in HELLO. The wire format lives
//! in [`beacon`](crate::beacon); this module has the two traits the app is written against and
//! their tokio UDP implementations.
//!
//! * [`UdpBeaconAnnouncer`] (PC, [`BeaconAnnouncer`]) announces the TCP port on every eligible
//!   interface at 100, 500 and 2000 ms after each [`announce`](BeaconAnnouncer::announce), and
//!   answers a valid PROBE with a unicast ANNOUNCE, at most once per source IP every 2 s. It holds
//!   a UDP socket **only while it has a key**: with no peers there is no `beacon_key`, so nothing
//!   is bound, sent or answered. Anything that is not a valid PROBE for the current key (wrong
//!   length, magic, version, kind, tag; an ANNOUNCE; junk) is dropped without a reply, a counter or
//!   a log line, and does not use up the sender's reply slot.
//! * [`UdpPcLocator`] (phone, [`PcLocator`]) sends a PROBE burst from one socket per interface,
//!   collects the sources of valid ANNOUNCEs until the timeout, and returns them, each paired with
//!   the TCP port the beacon named. They are **candidates only**: an address learned from a beacon
//!   is never trusted, and the Noise handshake in
//!   [`PhoneClient::connect`](crate::PhoneClient::connect) is what proves the PC's identity.
//!
//! # Where packets go
//!
//! In production ([`AnnouncerNet::Multicast`], [`LocatorNet::Multicast`]) every beacon is a
//! TTL-1 multicast datagram to the group, sent from a socket bound to one eligible interface
//! address with `IP_MULTICAST_IF` set, so it leaves on that interface and no other. The PC's
//! listener is one socket on `0.0.0.0:<port>` that joins the group once per eligible interface.
//! Which interfaces are eligible is the caller's decision: the document's rule that the PC binds
//! only to Private or Domain networks (`INetworkListManager`) is applied by passing only those
//! interfaces' addresses, exactly as [`PcServer::start`](crate::PcServer::start) takes its
//! addresses. After a network change, [`stop`](BeaconAnnouncer::stop) the announcer and build a
//! new one with the new list.
//!
//! [`AnnouncerNet::Unicast`] and [`LocatorNet::Unicast`] replace multicast with plain UDP to
//! explicit addresses. They exist so tests can run over `127.0.0.1` and so an app can probe a
//! known address; nothing about the wire format or the checks differs.
//!
//! IPv4 only, as the document specifies. Nothing here logs a key, a tag or a peer's datagram.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::task::{JoinHandle, JoinSet};
use zeroize::Zeroizing;

use crate::beacon::{Beacon, BeaconKind, BEACON_GROUP, BEACON_LEN, BEACON_NONCE_LEN, BEACON_PORT};
use crate::clock::{BoxFuture, SyncClock};
use crate::keys::random_bytes;
use crate::lock;
use crate::ratelimit::{LogGate, ReplyLimiter};

/// Shortest gap between two replies to one source IP (the document's "once per IP every 2 s").
pub const REPLY_INTERVAL: Duration = Duration::from_secs(2);
/// When an announcement is sent, measured from the call to [`BeaconAnnouncer::announce`].
pub const ANNOUNCE_OFFSETS: [Duration; 3] = [
    Duration::from_millis(100),
    Duration::from_millis(500),
    Duration::from_millis(2000),
];
/// When the locator sends a PROBE, measured from the start of [`PcLocator::locate`]. Probes the
/// PC answers within [`REPLY_INTERVAL`] of an earlier one are not answered again; the later ones
/// only matter if the first datagram or its reply was lost.
pub const PROBE_OFFSETS: [Duration; 4] = [
    Duration::ZERO,
    Duration::from_millis(100),
    Duration::from_millis(500),
    Duration::from_millis(2000),
];
/// Most candidate addresses a locate call returns.
pub const MAX_CANDIDATES: usize = 16;

/// How long a PROBE nonce is remembered: a probe seen again within this time is a replay and gets
/// no answer.
pub const PROBE_NONCE_MEMORY: Duration = Duration::from_secs(10 * 60);

/// Most PROBE nonces remembered at once. When full of live entries, new probes go unanswered
/// (a flood, like the reply limiter's table).
const MAX_SEEN_NONCES: usize = 4096;

/// Large enough that no datagram is ever truncated (a truncated datagram is an error on Windows).
const RECV_BUF: usize = 65_536;
/// Log lines per event code per minute.
const LOG_PER_MINUTE: u32 = 6;

/// PC side: announce this PC on the local networks.
pub trait BeaconAnnouncer: Send + Sync {
    /// Announce the TCP port on every eligible interface, and keep answering valid probes. Call it
    /// at startup, on a network change and after a revocation (when the key changed). `key` is the
    /// current beacon key; `None` means the PC has no peers, and then nothing is sent and probes
    /// are ignored.
    fn announce(&self, tcp_port: u16, key: Option<[u8; 32]>) -> BoxFuture<'_, io::Result<()>>;

    /// Stop announcing and release the sockets.
    fn stop(&self) -> BoxFuture<'_, ()>;
}

/// Phone side: find a paired PC after its address changed.
pub trait PcLocator: Send + Sync {
    /// Send a PROBE burst and collect the sources of valid ANNOUNCEs for up to `timeout`. `key` is
    /// the PC's beacon key from its HELLO. The returned addresses are candidates only.
    fn locate(
        &self,
        key: [u8; 32],
        timeout: Duration,
    ) -> BoxFuture<'_, io::Result<Vec<SocketAddr>>>;
}

/// Multicast settings shared by both sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MulticastNet {
    /// The multicast group ([`BEACON_GROUP`], `224.0.0.167`, by default).
    pub group: Ipv4Addr,
    /// The UDP port ([`BEACON_PORT`], 53319, by default).
    pub port: u16,
    /// The IPv4 address of every interface to use, one socket each. The caller filters these (on
    /// Windows, to addresses on Private or Domain networks). Empty means no network: nothing is
    /// sent or answered.
    pub interfaces: Vec<Ipv4Addr>,
    /// Deliver our own multicast datagrams back to this machine (`IP_MULTICAST_LOOP`). **Off** in
    /// production, where the other end is a different device. Turn it on only to run both ends on
    /// one host, for example in a test or a development machine that is also the "phone".
    pub loopback: bool,
}

impl MulticastNet {
    /// The document's group and port on the given interfaces.
    pub fn new(interfaces: Vec<Ipv4Addr>) -> Self {
        MulticastNet {
            group: BEACON_GROUP,
            port: BEACON_PORT,
            interfaces,
            loopback: false,
        }
    }

    /// The group and port as a socket address.
    fn destination(&self) -> SocketAddr {
        SocketAddr::from((self.group, self.port))
    }

    /// Check the settings and return the interfaces without duplicates.
    fn checked_interfaces(&self) -> io::Result<Vec<Ipv4Addr>> {
        if !self.group.is_multicast() || self.port == 0 {
            return Err(invalid("not a multicast group and port"));
        }
        let mut out: Vec<Ipv4Addr> = Vec::new();
        for &iface in &self.interfaces {
            if iface.is_unspecified() || iface.is_multicast() || iface.is_broadcast() {
                return Err(invalid("not a unicast interface address"));
            }
            if !out.contains(&iface) {
                out.push(iface);
            }
        }
        Ok(out)
    }
}

/// How the PC's beacons travel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnnouncerNet {
    /// Production: listen on the group and announce to it, on each eligible interface.
    Multicast(MulticastNet),
    /// Plain UDP: listen on `bind` and send announcements to `announce_to` (which may be empty).
    /// Probes may come from anywhere that can reach `bind`. Use port 0 to get an OS-assigned one
    /// (see [`UdpBeaconAnnouncer::local_addr`]).
    Unicast {
        /// Where to listen.
        bind: SocketAddr,
        /// Where to send each announcement.
        announce_to: Vec<SocketAddr>,
    },
}

/// How the phone's probes travel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocatorNet {
    /// Production: send each PROBE to the group from one socket per eligible interface, and read
    /// the unicast replies on those same sockets.
    Multicast(MulticastNet),
    /// Plain UDP: one socket bound to `bind` (any port) sends each PROBE to every `probe_to`.
    Unicast {
        /// The local address to send from.
        bind: IpAddr,
        /// Where to send each PROBE.
        probe_to: Vec<SocketAddr>,
    },
}

/// Sockets and tasks need a tokio runtime; say so as an error instead of panicking inside tokio.
fn ensure_runtime() -> io::Result<()> {
    tokio::runtime::Handle::try_current()
        .map(|_| ())
        .map_err(|_| io::Error::other("discovery needs a tokio runtime"))
}

fn invalid(what: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what)
}

fn random_nonce() -> io::Result<[u8; 8]> {
    let mut nonce = [0u8; 8];
    random_bytes(&mut nonce).map_err(|_| io::Error::other("random number generator failed"))?;
    Ok(nonce)
}

/// A peer address worth replying to or returning: a real unicast source.
fn usable_peer(addr: SocketAddr) -> bool {
    let ip = addr.ip();
    addr.port() != 0
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && ip != IpAddr::V4(Ipv4Addr::BROADCAST)
}

/// Receive errors that say nothing about this socket: Windows reports an earlier ICMP "port
/// unreachable" for a datagram we sent as a `ConnectionReset` on the next receive.
fn is_transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::TimedOut
    )
}

/// Log an i/o failure by kind only, bounded per event code. Never anything from a datagram.
fn log_io(gate: &LogGate, clock: &dyn SyncClock, code: &'static str, error: &io::Error) {
    if gate.allow(code, clock.mono()) {
        tracing::debug!(event = code, kind = ?error.kind(), "beacon i/o");
    }
}

// ---------------------------------------------------------------------------------------------
// Sockets
// ---------------------------------------------------------------------------------------------

fn into_tokio(socket: Socket) -> io::Result<UdpSocket> {
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket.into())
}

fn new_udp_socket() -> io::Result<Socket> {
    Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
}

/// A socket bound to `iface` on an OS-assigned port whose multicast datagrams leave on that
/// interface only (`IP_MULTICAST_IF`), with TTL 1 and, unless `loopback` is set, no loop-back to
/// the local machine. Used for the PC's announcements and the phone's probes (and for the phone to
/// read the replies).
fn interface_socket(iface: Ipv4Addr, loopback: bool) -> io::Result<UdpSocket> {
    let socket = new_udp_socket()?;
    socket.bind(&SocketAddr::from((iface, 0)).into())?;
    socket.set_multicast_if_v4(&iface)?;
    socket.set_multicast_ttl_v4(1)?;
    socket.set_multicast_loop_v4(loopback)?;
    into_tokio(socket)
}

/// The PC's listener: `0.0.0.0:<port>`, joined to the group on every eligible interface. Fails if
/// the port is taken (no address reuse is requested, so another process cannot share it) or no
/// interface could join. Unicast replies leave with TTL 1, so they never cross a router.
fn listen_socket(net: &MulticastNet, interfaces: &[Ipv4Addr]) -> io::Result<UdpSocket> {
    let socket = new_udp_socket()?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, net.port)).into())?;
    let mut joined = 0usize;
    let mut last_error = None;
    for iface in interfaces {
        match socket.join_multicast_v4(&net.group, iface) {
            Ok(()) => joined += 1,
            Err(error) => last_error = Some(error),
        }
    }
    if joined == 0 {
        if let Some(error) = last_error {
            return Err(error);
        }
    }
    socket.set_ttl_v4(1)?;
    into_tokio(socket)
}

fn unicast_socket(bind: SocketAddr) -> io::Result<UdpSocket> {
    let socket = std::net::UdpSocket::bind(bind)?;
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket)
}

// ---------------------------------------------------------------------------------------------
// PC: the announcer
// ---------------------------------------------------------------------------------------------

/// The key and port the announcer currently serves.
struct Current {
    key: Zeroizing<[u8; 32]>,
    tcp_port: u16,
}

/// A socket and the address it announces to.
struct Sender {
    socket: Arc<UdpSocket>,
    dest: SocketAddr,
}

/// PROBE nonces seen recently (replay protection without a clock shared by both devices).
#[derive(Debug, Default)]
struct SeenNonces {
    seen: HashMap<[u8; BEACON_NONCE_LEN], Duration>,
}

impl SeenNonces {
    /// Record `nonce` at `now`. `false` when it was seen within [`PROBE_NONCE_MEMORY`] (a replay)
    /// or the table is full of live entries.
    fn first_sighting(&mut self, nonce: [u8; BEACON_NONCE_LEN], now: Duration) -> bool {
        if let Some(&at) = self.seen.get(&nonce) {
            if now.saturating_sub(at) < PROBE_NONCE_MEMORY {
                return false;
            }
        }
        if self.seen.len() >= MAX_SEEN_NONCES {
            self.seen
                .retain(|_, &mut at| now.saturating_sub(at) < PROBE_NONCE_MEMORY);
            if self.seen.len() >= MAX_SEEN_NONCES {
                return false;
            }
        }
        self.seen.insert(nonce, now);
        true
    }
}

/// True when `ip` is inside one of `allowed` (`(network address, prefix length)`), or when
/// `allowed` is empty (no restriction).
pub fn source_allowed(ip: IpAddr, allowed: &[(Ipv4Addr, u8)]) -> bool {
    if allowed.is_empty() {
        return true;
    }
    let IpAddr::V4(v4) = ip else { return false };
    allowed.iter().any(|&(net, prefix)| {
        let prefix = u32::from(prefix.min(32));
        let mask = if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - prefix)
        };
        u32::from(v4) & mask == u32::from(net) & mask
    })
}

/// State shared by the receive task and the announce bursts.
struct Shared {
    current: Mutex<Current>,
    limiter: Mutex<ReplyLimiter>,
    seen: Mutex<SeenNonces>,
    allowed_sources: Vec<(Ipv4Addr, u8)>,
    clock: Arc<dyn SyncClock>,
    replies: Arc<AtomicU64>,
    log: LogGate,
}

impl Shared {
    fn snapshot(&self) -> (Zeroizing<[u8; 32]>, u16) {
        let current = lock(&self.current);
        (current.key.clone(), current.tcp_port)
    }

    /// Handle one datagram from the listener. Everything but a valid PROBE for the current key,
    /// from a source whose reply slot is free, is dropped without a trace.
    async fn answer(&self, socket: &UdpSocket, datagram: &[u8], src: SocketAddr) {
        if datagram.len() != BEACON_LEN
            || !usable_peer(src)
            || !source_allowed(src.ip(), &self.allowed_sources)
        {
            return;
        }
        let now = self.clock.mono();
        // Cheap check before the MAC; it does not spend the slot.
        if !lock(&self.limiter).would_allow(src.ip(), now) {
            return;
        }
        let (key, tcp_port) = self.snapshot();
        let probe = match Beacon::decode(datagram, &key) {
            Ok(beacon) if beacon.kind() == BeaconKind::Probe => beacon,
            _ => return,
        };
        // A captured probe sent again (from anywhere) is a replay: no answer, no slot spent.
        if !lock(&self.seen).first_sighting(probe.nonce(), now) {
            return;
        }
        // Only a valid probe spends the slot, so junk cannot lock a real phone out.
        if !lock(&self.limiter).try_acquire(src.ip(), now) {
            return;
        }
        let Ok(nonce) = random_nonce() else { return };
        let Ok(reply) = Beacon::announce(tcp_port, nonce) else {
            return;
        };
        match socket.send_to(&reply.encode(&key), src).await {
            Ok(_) => {
                self.replies.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => log_io(&self.log, &*self.clock, "beacon_reply_failed", &error),
        }
    }

    /// One ANNOUNCE per sender, each with its own nonce.
    async fn announce_once(&self, senders: &[Sender]) {
        let (key, tcp_port) = self.snapshot();
        for sender in senders {
            let Ok(nonce) = random_nonce() else { return };
            let Ok(beacon) = Beacon::announce(tcp_port, nonce) else {
                return;
            };
            if let Err(error) = sender
                .socket
                .send_to(&beacon.encode(&key), sender.dest)
                .await
            {
                log_io(&self.log, &*self.clock, "beacon_send_failed", &error);
            }
        }
    }
}

async fn serve_probes(socket: Arc<UdpSocket>, shared: Arc<Shared>) {
    let mut buf = vec![0u8; RECV_BUF];
    loop {
        let (len, src) = match socket.recv_from(&mut buf).await {
            Ok(received) => received,
            Err(error) if is_transient(&error) => continue,
            Err(error) => {
                log_io(
                    &shared.log,
                    &*shared.clock,
                    "beacon_listener_failed",
                    &error,
                );
                return;
            }
        };
        shared.answer(&socket, &buf[..len], src).await;
    }
}

async fn announce_burst(shared: Arc<Shared>, senders: Arc<Vec<Sender>>) {
    let mut elapsed = Duration::ZERO;
    for offset in ANNOUNCE_OFFSETS {
        shared.clock.sleep(offset.saturating_sub(elapsed)).await;
        elapsed = offset;
        shared.announce_once(&senders).await;
    }
}

/// Everything that exists while the announcer holds a key.
struct Running {
    shared: Arc<Shared>,
    local_addr: SocketAddr,
    senders: Arc<Vec<Sender>>,
    listener: JoinHandle<()>,
    burst: Option<JoinHandle<()>>,
}

impl Running {
    /// Abort every task and hand back the handles so the caller can wait for the sockets to close.
    fn abort(self) -> Vec<JoinHandle<()>> {
        self.listener.abort();
        let mut handles = vec![self.listener];
        if let Some(burst) = self.burst {
            burst.abort();
            handles.push(burst);
        }
        handles
    }
}

#[derive(Default)]
struct AnnouncerState {
    stopped: bool,
    running: Option<Running>,
}

/// The PC's UDP [`BeaconAnnouncer`].
///
/// Construction does no I/O. [`announce`](BeaconAnnouncer::announce) with a key binds the sockets
/// (once; later calls reuse them), updates the key and port, and schedules an announcement burst
/// at 100, 500 and 2000 ms (a new call replaces a burst still in flight). With `None`, or after
/// [`stop`](BeaconAnnouncer::stop), the sockets are closed and nothing is heard or sent. A stopped
/// announcer stays stopped: `announce` then fails with `NotConnected`.
///
/// Must be used from inside a tokio runtime.
pub struct UdpBeaconAnnouncer {
    net: AnnouncerNet,
    clock: Arc<dyn SyncClock>,
    reply_interval: Duration,
    allowed_sources: Vec<(Ipv4Addr, u8)>,
    replies: Arc<AtomicU64>,
    state: Mutex<AnnouncerState>,
}

impl UdpBeaconAnnouncer {
    /// An announcer for `net`. Nothing is bound until a key is announced.
    pub fn new(net: AnnouncerNet, clock: Arc<dyn SyncClock>) -> Self {
        UdpBeaconAnnouncer {
            net,
            clock,
            reply_interval: REPLY_INTERVAL,
            allowed_sources: Vec::new(),
            replies: Arc::new(AtomicU64::new(0)),
            state: Mutex::new(AnnouncerState::default()),
        }
    }

    /// The production announcer: the document's group and port on `interfaces`.
    pub fn multicast(interfaces: Vec<Ipv4Addr>, clock: Arc<dyn SyncClock>) -> Self {
        Self::new(
            AnnouncerNet::Multicast(MulticastNet::new(interfaces)),
            clock,
        )
    }

    /// Answer PROBEs only from these subnets (`(network address, prefix length)`), normally the
    /// subnets of the eligible interfaces. The listener is bound to `0.0.0.0` (a socket bound to
    /// one address does not receive multicast on every OS), so this is what keeps a probe that
    /// arrived from a non-eligible network unanswered. A spoofed source gains nothing: the reply
    /// goes to that source. Empty, the default, means no restriction.
    pub fn with_allowed_sources(mut self, subnets: Vec<(Ipv4Addr, u8)>) -> Self {
        self.allowed_sources = subnets;
        self
    }

    /// The address the listener is bound to, while it holds a key.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        lock(&self.state).running.as_ref().map(|r| r.local_addr)
    }

    /// How many probe replies have been sent since this announcer was created.
    pub fn replies_sent(&self) -> u64 {
        self.replies.load(Ordering::Relaxed)
    }

    /// Bind the sockets and start the listener. `None` when there is nothing to bind to.
    fn start(&self, key: Zeroizing<[u8; 32]>, tcp_port: u16) -> io::Result<Option<Running>> {
        ensure_runtime()?;
        let log = LogGate::new(LOG_PER_MINUTE);
        let (listener, senders) = match &self.net {
            AnnouncerNet::Multicast(net) => {
                let interfaces = net.checked_interfaces()?;
                if interfaces.is_empty() {
                    return Ok(None);
                }
                let listener = Arc::new(listen_socket(net, &interfaces)?);
                let mut senders = Vec::new();
                for iface in interfaces {
                    match interface_socket(iface, net.loopback) {
                        Ok(socket) => senders.push(Sender {
                            socket: Arc::new(socket),
                            dest: net.destination(),
                        }),
                        Err(error) => log_io(&log, &*self.clock, "beacon_interface_failed", &error),
                    }
                }
                (listener, senders)
            }
            AnnouncerNet::Unicast { bind, announce_to } => {
                let listener = Arc::new(unicast_socket(*bind)?);
                let senders = announce_to
                    .iter()
                    .map(|&dest| Sender {
                        socket: Arc::clone(&listener),
                        dest,
                    })
                    .collect();
                (listener, senders)
            }
        };
        let local_addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            current: Mutex::new(Current { key, tcp_port }),
            limiter: Mutex::new(ReplyLimiter::new(self.reply_interval)),
            seen: Mutex::new(SeenNonces::default()),
            allowed_sources: self.allowed_sources.clone(),
            clock: Arc::clone(&self.clock),
            replies: Arc::clone(&self.replies),
            log,
        });
        let listener = tokio::spawn(serve_probes(listener, Arc::clone(&shared)));
        tracing::debug!(
            event = "beacon_listening",
            port = local_addr.port(),
            "beacon"
        );
        Ok(Some(Running {
            shared,
            local_addr,
            senders: Arc::new(senders),
            listener,
            burst: None,
        }))
    }

    /// The synchronous part of `announce`. Returns the handles of tasks that were shut down, so
    /// the caller can wait until their sockets are really closed.
    fn apply(&self, tcp_port: u16, key: Option<[u8; 32]>) -> io::Result<Vec<JoinHandle<()>>> {
        let mut state = lock(&self.state);
        if state.stopped {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "beacon announcer stopped",
            ));
        }
        let Some(key) = key.map(Zeroizing::new) else {
            return Ok(state.running.take().map(Running::abort).unwrap_or_default());
        };
        if tcp_port == 0 {
            return Err(invalid("cannot announce port 0"));
        }
        if let Some(running) = state.running.as_mut() {
            *lock(&running.shared.current) = Current { key, tcp_port };
            if let Some(old) = running.burst.take() {
                old.abort();
            }
        } else {
            match self.start(key, tcp_port)? {
                Some(running) => state.running = Some(running),
                None => return Ok(Vec::new()),
            }
        }
        if let Some(running) = state.running.as_mut() {
            running.burst = Some(tokio::spawn(announce_burst(
                Arc::clone(&running.shared),
                Arc::clone(&running.senders),
            )));
        }
        Ok(Vec::new())
    }
}

impl BeaconAnnouncer for UdpBeaconAnnouncer {
    fn announce(&self, tcp_port: u16, key: Option<[u8; 32]>) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            for handle in self.apply(tcp_port, key)? {
                let _ = handle.await;
            }
            Ok(())
        })
    }

    fn stop(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let handles = {
                let mut state = lock(&self.state);
                state.stopped = true;
                state.running.take().map(Running::abort).unwrap_or_default()
            };
            for handle in handles {
                let _ = handle.await;
            }
        })
    }
}

impl Drop for UdpBeaconAnnouncer {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        if let Some(running) = state.running.take() {
            let _ = running.abort();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Phone: the locator
// ---------------------------------------------------------------------------------------------

/// A socket that sends PROBEs and reads the replies.
struct Prober {
    socket: Arc<UdpSocket>,
    dests: Vec<SocketAddr>,
}

/// Read datagrams until aborted, keeping the sources of valid ANNOUNCEs for `key`.
async fn collect_announces(
    socket: Arc<UdpSocket>,
    key: Arc<Zeroizing<[u8; 32]>>,
    found: Arc<Mutex<Vec<SocketAddr>>>,
) {
    let mut buf = vec![0u8; RECV_BUF];
    loop {
        let (len, src) = match socket.recv_from(&mut buf).await {
            Ok(received) => received,
            Err(error) if is_transient(&error) => continue,
            Err(_) => return,
        };
        if len != BEACON_LEN || !usable_peer(src) {
            continue;
        }
        let Ok(beacon) = Beacon::decode(&buf[..len], &key) else {
            continue;
        };
        if beacon.kind() != BeaconKind::Announce {
            continue;
        }
        let candidate = SocketAddr::new(src.ip(), beacon.tcp_port());
        let mut found = lock(&found);
        if found.len() < MAX_CANDIDATES && !found.contains(&candidate) {
            found.push(candidate);
        }
    }
}

/// The phone's UDP [`PcLocator`].
///
/// Each [`locate`](PcLocator::locate) opens its sockets, sends PROBEs at 0, 100, 500 and 2000 ms
/// (those that fall before the timeout), reads replies until the timeout, and closes everything.
/// Nothing is held between calls, so the Android `MulticastLock` is needed only while one runs.
/// An empty result means nobody answered; an error means no probe could be sent at all.
pub struct UdpPcLocator {
    net: LocatorNet,
    clock: Arc<dyn SyncClock>,
}

impl UdpPcLocator {
    /// A locator for `net`.
    pub fn new(net: LocatorNet, clock: Arc<dyn SyncClock>) -> Self {
        UdpPcLocator { net, clock }
    }

    /// The production locator: the document's group and port on `interfaces`.
    pub fn multicast(interfaces: Vec<Ipv4Addr>, clock: Arc<dyn SyncClock>) -> Self {
        Self::new(LocatorNet::Multicast(MulticastNet::new(interfaces)), clock)
    }

    fn open(&self, log: &LogGate) -> io::Result<Vec<Prober>> {
        ensure_runtime()?;
        match &self.net {
            LocatorNet::Multicast(net) => {
                let interfaces = net.checked_interfaces()?;
                let mut probers = Vec::new();
                let mut last_error = None;
                for iface in interfaces {
                    match interface_socket(iface, net.loopback) {
                        Ok(socket) => probers.push(Prober {
                            socket: Arc::new(socket),
                            dests: vec![net.destination()],
                        }),
                        Err(error) => {
                            log_io(log, &*self.clock, "beacon_interface_failed", &error);
                            last_error = Some(error);
                        }
                    }
                }
                match (probers.is_empty(), last_error) {
                    (true, Some(error)) => Err(error),
                    _ => Ok(probers),
                }
            }
            LocatorNet::Unicast { bind, probe_to } => {
                if probe_to.is_empty() {
                    return Ok(Vec::new());
                }
                let socket = unicast_socket(SocketAddr::new(*bind, 0))?;
                Ok(vec![Prober {
                    socket: Arc::new(socket),
                    dests: probe_to.clone(),
                }])
            }
        }
    }

    async fn run(&self, key: [u8; 32], timeout: Duration) -> io::Result<Vec<SocketAddr>> {
        let key = Arc::new(Zeroizing::new(key));
        let log = LogGate::new(LOG_PER_MINUTE);
        let probers = self.open(&log)?;
        if probers.is_empty() {
            return Ok(Vec::new());
        }
        let found = Arc::new(Mutex::new(Vec::new()));
        // Dropping the set (including when this future is cancelled) aborts the readers.
        let mut readers = JoinSet::new();
        for prober in &probers {
            readers.spawn(collect_announces(
                Arc::clone(&prober.socket),
                Arc::clone(&key),
                Arc::clone(&found),
            ));
        }

        let mut sent_any = false;
        let mut last_error = None;
        let mut elapsed = Duration::ZERO;
        for offset in PROBE_OFFSETS {
            if offset > timeout {
                break;
            }
            self.clock.sleep(offset - elapsed).await;
            elapsed = offset;
            for prober in &probers {
                for dest in &prober.dests {
                    let beacon = Beacon::probe(random_nonce()?);
                    match prober.socket.send_to(&beacon.encode(&key), dest).await {
                        Ok(_) => sent_any = true,
                        Err(error) => {
                            log_io(&log, &*self.clock, "beacon_probe_failed", &error);
                            last_error = Some(error);
                        }
                    }
                }
            }
        }
        self.clock.sleep(timeout - elapsed).await;
        readers.shutdown().await;

        let candidates = lock(&found).clone();
        match (sent_any, last_error) {
            (false, Some(error)) => Err(error),
            _ => Ok(candidates),
        }
    }
}

impl PcLocator for UdpPcLocator {
    fn locate(
        &self,
        key: [u8; 32],
        timeout: Duration,
    ) -> BoxFuture<'_, io::Result<Vec<SocketAddr>>> {
        Box::pin(self.run(key, timeout))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(group: [u8; 4], port: u16, interfaces: Vec<Ipv4Addr>) -> MulticastNet {
        MulticastNet {
            group: Ipv4Addr::from(group),
            port,
            interfaces,
            loopback: false,
        }
    }

    #[test]
    fn the_default_multicast_settings_are_the_documents() {
        let net = MulticastNet::new(vec![Ipv4Addr::LOCALHOST]);
        assert_eq!(net.group, Ipv4Addr::new(224, 0, 0, 167));
        assert_eq!(net.port, 53319);
        assert_eq!(net.destination(), "224.0.0.167:53319".parse().unwrap());
    }

    #[test]
    fn multicast_settings_are_checked_and_deduplicated() {
        let lan = Ipv4Addr::new(192, 168, 1, 7);
        let ok = net([224, 0, 0, 167], 53319, vec![lan, lan, Ipv4Addr::LOCALHOST]);
        assert_eq!(
            ok.checked_interfaces().unwrap(),
            vec![lan, Ipv4Addr::LOCALHOST]
        );
        assert!(net([10, 0, 0, 1], 53319, vec![lan])
            .checked_interfaces()
            .is_err());
        assert!(net([224, 0, 0, 167], 0, vec![lan])
            .checked_interfaces()
            .is_err());
        for bad in [
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::BROADCAST,
            Ipv4Addr::new(224, 0, 0, 1),
        ] {
            assert!(
                net([224, 0, 0, 167], 53319, vec![lan, bad])
                    .checked_interfaces()
                    .is_err(),
                "{bad} accepted as an interface"
            );
        }
    }

    #[test]
    fn only_real_unicast_sources_are_usable_peers() {
        let ok: SocketAddr = "192.168.1.9:40000".parse().unwrap();
        assert!(usable_peer(ok));
        for bad in [
            "0.0.0.0:40000",
            "224.0.0.167:40000",
            "255.255.255.255:40000",
            "192.168.1.9:0",
        ] {
            assert!(!usable_peer(bad.parse().unwrap()), "{bad}");
        }
    }

    #[test]
    fn windows_style_connection_resets_are_not_fatal() {
        assert!(is_transient(&io::Error::from(
            io::ErrorKind::ConnectionReset
        )));
        assert!(!is_transient(&io::Error::from(io::ErrorKind::BrokenPipe)));
        assert!(!is_transient(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
    }
}
