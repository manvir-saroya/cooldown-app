//! Cooldown pairing and sync: the protocol in `docs/pairing-protocol.md`, as a library.
//!
//! * [`PcServer`] (PC) accepts first pairings and reconnects and pushes
//!   [`cooldown_core::SyncPayload`]s to authenticated phones.
//! * [`PhoneClient`] (phone) pairs from a QR payload, reconnects, and keeps the latest payload.
//! * [`TrustStore`] and [`IdentityStore`] keep paired devices and private keys, with in-memory and
//!   persistent implementations.
//! * [`UdpBeaconAnnouncer`] (PC) and [`UdpPcLocator`] (phone) are the LAN discovery beacon of
//!   document section 4: the PC announces itself with a keyed UDP datagram, the phone probes for
//!   it, and the addresses found are only candidates for [`PhoneClient::connect`].
//! * The protocol pieces ([`qr`], [`hello`], [`beacon`], [`noise`], [`frame`], [`message`],
//!   [`sanitize`]) are public so tests, fuzz targets and interoperability checks can drive the
//!   wire format by hand.
//!
//! Everything time-dependent goes through [`SyncClock`], so tests run over real `127.0.0.1`
//! sockets with fake clocks. The README explains the architecture, how a Tauri app uses the two
//! endpoints, every place this implementation interprets the protocol document, what is not
//! implemented yet, and the security caveats (notably that `snow` is not formally audited).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod beacon;
pub mod client;
pub mod clock;
pub mod codec;
pub mod discovery;
pub mod error;
pub mod frame;
pub mod hello;
pub mod identity;
pub mod keys;
pub mod limits;
pub mod message;
pub mod noise;
mod pairing;
pub mod qr;
mod ratelimit;
pub mod sanitize;
pub mod server;
mod session;
pub mod trust;

pub use beacon::{Beacon, BeaconKind, BEACON_GROUP, BEACON_PORT};
pub use client::should_apply;
pub use client::{
    ClientConfig, CloseReason, ConnState, Connection, DeviceInfo, PendingPairing, PhoneClient,
    ReceivedSync,
};
pub use clock::{SyncClock, SystemSyncClock};
pub use discovery::{
    source_allowed, AnnouncerNet, BeaconAnnouncer, LocatorNet, MulticastNet, PcLocator,
    UdpBeaconAnnouncer, UdpPcLocator, PROBE_NONCE_MEMORY,
};
pub use error::*;
pub use hello::{ClientHello, Mode, Status};
pub use identity::*;
pub use keys::{DeviceId, PublicKey, StaticKeypair};
pub use limits::{Limits, DEFAULT_PORT, MAX_PEERS};
pub use qr::QrPayload;
pub use server::*;
pub use trust::{FileTrustStore, MemoryTrustStore, PeerRecord, SessionUpdate, TrustStore};

/// Lock a mutex and carry on if another thread panicked while holding it: a poisoned lock is not
/// treated as fatal anywhere in this crate.
pub(crate) fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
