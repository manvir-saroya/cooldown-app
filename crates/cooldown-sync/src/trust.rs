//! The trust store: which devices are paired, with their static public keys.
//!
//! Everything stored here is public data (keys, names, timestamps) plus the beacon key, a
//! low-sensitivity symmetric key for discovery beacons. Private identity keys never come near it;
//! they go through [`IdentityStore`](crate::IdentityStore).
//!
//! The same trait serves both ends. On the **PC** a record describes a phone and `last_ts` is the
//! highest reconnect timestamp accepted from it. On the **phone** a record describes a PC and
//! `last_ts` is the last timestamp sent to it. Two implementations ship: in memory (tests, and
//! apps that persist elsewhere) and a JSON file written atomically.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::codec::{b64_decode_fixed, b64_encode};
use crate::error::StoreError;
use crate::keys::{DeviceId, PublicKey};

/// One paired device.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerRecord {
    /// `BLAKE2s-256(static_pub)[..16]`; always consistent with `static_pub`.
    pub device_id: DeviceId,
    /// The peer's static X25519 public key.
    #[serde(with = "key32")]
    pub static_pub: PublicKey,
    /// Display name (at most 32 characters).
    pub name: String,
    /// `android`, `windows`, ...
    pub platform: String,
    /// When pairing completed, in Unix milliseconds on the local wall clock.
    pub paired_at_ms: i64,
    /// Last authenticated session start, Unix milliseconds. `None` if never connected.
    pub last_seen_ms: Option<i64>,
    /// PC: highest reconnect `ts` accepted. Phone: last `ts` sent. Persisted before use.
    pub last_ts: u64,
    /// Highest protocol version completed with this peer; the phone never offers lower.
    pub proto_floor: u8,
    /// Phone side: the PC's beacon key, learned from its HELLO.
    #[serde(default, with = "key32_opt")]
    pub beacon_key: Option<[u8; 32]>,
}

impl PeerRecord {
    /// A record for a freshly paired device.
    pub fn new(
        static_pub: PublicKey,
        name: String,
        platform: String,
        paired_at_ms: i64,
        last_ts: u64,
        proto: u8,
    ) -> Self {
        PeerRecord {
            device_id: DeviceId::from_public(&static_pub),
            static_pub,
            name,
            platform,
            paired_at_ms,
            last_seen_ms: None,
            last_ts,
            proto_floor: proto,
            beacon_key: None,
        }
    }
}

impl std::fmt::Debug for PeerRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerRecord")
            .field("device_id", &self.device_id)
            .field("platform", &self.platform)
            .field("paired_at_ms", &self.paired_at_ms)
            .field("last_seen_ms", &self.last_seen_ms)
            .field("proto_floor", &self.proto_floor)
            .finish_non_exhaustive()
    }
}

mod key32 {
    use super::{b64_decode_fixed, b64_encode};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(key: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&b64_encode(key))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(deserializer)?;
        b64_decode_fixed::<32>(&text).ok_or_else(|| serde::de::Error::custom("invalid key"))
    }
}

mod key32_opt {
    use super::{b64_decode_fixed, b64_encode};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        key: &Option<[u8; 32]>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match key {
            Some(key) => serializer.serialize_some(&b64_encode(key)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<[u8; 32]>, D::Error> {
        match Option::<String>::deserialize(deserializer)? {
            None => Ok(None),
            Some(text) => b64_decode_fixed::<32>(&text)
                .map(Some)
                .ok_or_else(|| serde::de::Error::custom("invalid key")),
        }
    }
}

/// What a session start updates on a record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionUpdate {
    /// New `last_seen_ms`.
    pub last_seen_ms: Option<i64>,
    /// New protocol floor. Only ever raises the stored value.
    pub proto_floor: Option<u8>,
    /// New beacon key (phone side).
    pub beacon_key: Option<[u8; 32]>,
}

/// Persistent knowledge of paired devices. All methods are synchronous and short; a store that
/// blocks for long must hide that itself.
pub trait TrustStore: Send + Sync + 'static {
    /// Every paired device.
    fn list(&self) -> Result<Vec<PeerRecord>, StoreError>;

    /// One device by id.
    fn get(&self, id: &DeviceId) -> Result<Option<PeerRecord>, StoreError>;

    /// Insert a record, or replace the one with the same `device_id`.
    fn upsert(&self, record: PeerRecord) -> Result<(), StoreError>;

    /// Delete a device (its static key goes with the record). Returns whether it existed.
    fn revoke(&self, id: &DeviceId) -> Result<bool, StoreError>;

    /// PC side: atomically raise `last_ts` to `ts` if `ts > last_ts` and persist. Returns `false`
    /// (and changes nothing) for a replayed or lower timestamp and for an unknown device.
    fn advance_ts(&self, id: &DeviceId, ts: u64) -> Result<bool, StoreError>;

    /// Phone side: pick `max(now_ms, last_ts + 1)`, persist it as `last_ts`, and return it. The
    /// value is on disk before the caller sends it. `None` for an unknown device.
    fn reserve_ts(&self, id: &DeviceId, now_ms: i64) -> Result<Option<u64>, StoreError>;

    /// Record a session start. Returns `false` for an unknown device.
    fn record_session(&self, id: &DeviceId, update: SessionUpdate) -> Result<bool, StoreError>;

    /// PC side: the discovery beacon key. `None` while there are no peers.
    fn beacon_key(&self) -> Result<Option<[u8; 32]>, StoreError>;

    /// PC side: replace or clear the beacon key.
    fn set_beacon_key(&self, key: Option<[u8; 32]>) -> Result<(), StoreError>;

    /// PC side: the public half of this device's own identity, recorded when it was created, to
    /// detect a lost, replaced or cloned credential-store entry.
    fn own_public(&self) -> Result<Option<PublicKey>, StoreError>;

    /// PC side: record (or clear) the own public key.
    fn set_own_public(&self, key: Option<PublicKey>) -> Result<(), StoreError>;

    /// Drop every peer and the beacon key (identity reset).
    fn clear_peers(&self) -> Result<(), StoreError>;
}

const FORMAT_VERSION: u32 = 1;
const MAX_STORED_PEERS: usize = 64;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreState {
    version: u32,
    #[serde(default, with = "key32_opt")]
    own_public: Option<[u8; 32]>,
    #[serde(default, with = "key32_opt")]
    beacon_key: Option<[u8; 32]>,
    peers: Vec<PeerRecord>,
}

impl StoreState {
    fn empty() -> Self {
        StoreState {
            version: FORMAT_VERSION,
            ..StoreState::default()
        }
    }

    fn validate(&self) -> Result<(), StoreError> {
        if self.version != FORMAT_VERSION {
            return Err(StoreError::Corrupt("unsupported format version"));
        }
        if self.peers.len() > MAX_STORED_PEERS {
            return Err(StoreError::Corrupt("too many peers"));
        }
        for (index, peer) in self.peers.iter().enumerate() {
            if peer.device_id != DeviceId::from_public(&peer.static_pub) {
                return Err(StoreError::Corrupt("device id does not match its key"));
            }
            if self.peers[..index]
                .iter()
                .any(|other| other.device_id == peer.device_id)
            {
                return Err(StoreError::Corrupt("duplicate device"));
            }
            if peer.name.chars().count() > 64 || peer.platform.len() > 32 {
                return Err(StoreError::Corrupt("field too long"));
            }
        }
        Ok(())
    }

    fn peer_mut(&mut self, id: &DeviceId) -> Option<&mut PeerRecord> {
        self.peers.iter_mut().find(|p| p.device_id == *id)
    }
}

/// Where a store keeps its state.
trait Persist: Send + Sync + 'static {
    fn save(&self, state: &StoreState) -> Result<(), StoreError>;
}

struct NoPersist;

impl Persist for NoPersist {
    fn save(&self, _state: &StoreState) -> Result<(), StoreError> {
        Ok(())
    }
}

struct FilePersist {
    path: PathBuf,
}

impl Persist for FilePersist {
    fn save(&self, state: &StoreState) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|_| StoreError::Corrupt("state does not serialize"))?;
        atomic_write(&self.path, &bytes)?;
        Ok(())
    }
}

/// State behind a mutex, persisted by `P`. A mutation runs on a copy, is saved, and only then
/// replaces the live state, so memory never claims more than the disk holds.
struct Backed<P: Persist> {
    state: Mutex<StoreState>,
    persist: P,
}

impl<P: Persist> Backed<P> {
    fn read<R>(&self, f: impl FnOnce(&StoreState) -> R) -> R {
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        f(&guard)
    }

    /// `f` returns the result and whether it changed the state.
    fn write<R>(
        &self,
        f: impl FnOnce(&mut StoreState) -> Result<(R, bool), StoreError>,
    ) -> Result<R, StoreError> {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = guard.clone();
        let (result, changed) = f(&mut next)?;
        if changed {
            self.persist.save(&next)?;
            *guard = next;
        }
        Ok(result)
    }

    fn list(&self) -> Result<Vec<PeerRecord>, StoreError> {
        Ok(self.read(|s| s.peers.clone()))
    }

    fn get(&self, id: &DeviceId) -> Result<Option<PeerRecord>, StoreError> {
        Ok(self.read(|s| s.peers.iter().find(|p| p.device_id == *id).cloned()))
    }

    fn upsert(&self, record: PeerRecord) -> Result<(), StoreError> {
        if record.device_id != DeviceId::from_public(&record.static_pub) {
            return Err(StoreError::Corrupt("device id does not match its key"));
        }
        self.write(|s| {
            match s.peers.iter_mut().find(|p| p.device_id == record.device_id) {
                Some(existing) => *existing = record,
                None => s.peers.push(record),
            }
            if s.peers.len() > MAX_STORED_PEERS {
                return Err(StoreError::Corrupt("too many peers"));
            }
            Ok(((), true))
        })
    }

    fn revoke(&self, id: &DeviceId) -> Result<bool, StoreError> {
        self.write(|s| {
            let before = s.peers.len();
            s.peers.retain(|p| p.device_id != *id);
            let removed = s.peers.len() != before;
            Ok((removed, removed))
        })
    }

    fn advance_ts(&self, id: &DeviceId, ts: u64) -> Result<bool, StoreError> {
        self.write(|s| match s.peer_mut(id) {
            Some(peer) if ts > peer.last_ts => {
                peer.last_ts = ts;
                Ok((true, true))
            }
            _ => Ok((false, false)),
        })
    }

    fn reserve_ts(&self, id: &DeviceId, now_ms: i64) -> Result<Option<u64>, StoreError> {
        self.write(|s| match s.peer_mut(id) {
            None => Ok((None, false)),
            Some(peer) => {
                if peer.last_ts == u64::MAX {
                    return Err(StoreError::Corrupt("timestamp counter exhausted"));
                }
                let ts = u64::try_from(now_ms).unwrap_or(0).max(peer.last_ts + 1);
                peer.last_ts = ts;
                Ok((Some(ts), true))
            }
        })
    }

    fn record_session(&self, id: &DeviceId, update: SessionUpdate) -> Result<bool, StoreError> {
        self.write(|s| match s.peer_mut(id) {
            None => Ok((false, false)),
            Some(peer) => {
                let mut changed = false;
                if let Some(seen) = update.last_seen_ms {
                    changed |= peer.last_seen_ms != Some(seen);
                    peer.last_seen_ms = Some(seen);
                }
                if let Some(floor) = update.proto_floor {
                    if floor > peer.proto_floor {
                        peer.proto_floor = floor;
                        changed = true;
                    }
                }
                if let Some(key) = update.beacon_key {
                    changed |= peer.beacon_key != Some(key);
                    peer.beacon_key = Some(key);
                }
                Ok((true, changed))
            }
        })
    }

    fn beacon_key(&self) -> Result<Option<[u8; 32]>, StoreError> {
        Ok(self.read(|s| s.beacon_key))
    }

    fn set_beacon_key(&self, key: Option<[u8; 32]>) -> Result<(), StoreError> {
        self.write(|s| {
            let changed = s.beacon_key != key;
            s.beacon_key = key;
            Ok(((), changed))
        })
    }

    fn own_public(&self) -> Result<Option<PublicKey>, StoreError> {
        Ok(self.read(|s| s.own_public))
    }

    fn set_own_public(&self, key: Option<PublicKey>) -> Result<(), StoreError> {
        self.write(|s| {
            let changed = s.own_public != key;
            s.own_public = key;
            Ok(((), changed))
        })
    }

    fn clear_peers(&self) -> Result<(), StoreError> {
        self.write(|s| {
            let changed = !s.peers.is_empty() || s.beacon_key.is_some();
            s.peers.clear();
            s.beacon_key = None;
            Ok(((), changed))
        })
    }
}

macro_rules! forward_trust_store {
    ($ty:ty) => {
        impl TrustStore for $ty {
            fn list(&self) -> Result<Vec<PeerRecord>, StoreError> {
                self.0.list()
            }
            fn get(&self, id: &DeviceId) -> Result<Option<PeerRecord>, StoreError> {
                self.0.get(id)
            }
            fn upsert(&self, record: PeerRecord) -> Result<(), StoreError> {
                self.0.upsert(record)
            }
            fn revoke(&self, id: &DeviceId) -> Result<bool, StoreError> {
                self.0.revoke(id)
            }
            fn advance_ts(&self, id: &DeviceId, ts: u64) -> Result<bool, StoreError> {
                self.0.advance_ts(id, ts)
            }
            fn reserve_ts(&self, id: &DeviceId, now_ms: i64) -> Result<Option<u64>, StoreError> {
                self.0.reserve_ts(id, now_ms)
            }
            fn record_session(
                &self,
                id: &DeviceId,
                update: SessionUpdate,
            ) -> Result<bool, StoreError> {
                self.0.record_session(id, update)
            }
            fn beacon_key(&self) -> Result<Option<[u8; 32]>, StoreError> {
                self.0.beacon_key()
            }
            fn set_beacon_key(&self, key: Option<[u8; 32]>) -> Result<(), StoreError> {
                self.0.set_beacon_key(key)
            }
            fn own_public(&self) -> Result<Option<PublicKey>, StoreError> {
                self.0.own_public()
            }
            fn set_own_public(&self, key: Option<PublicKey>) -> Result<(), StoreError> {
                self.0.set_own_public(key)
            }
            fn clear_peers(&self) -> Result<(), StoreError> {
                self.0.clear_peers()
            }
        }
    };
}

/// A trust store that lives only in memory.
pub struct MemoryTrustStore(Backed<NoPersist>);

impl MemoryTrustStore {
    /// An empty store.
    pub fn new() -> Self {
        MemoryTrustStore(Backed {
            state: Mutex::new(StoreState::empty()),
            persist: NoPersist,
        })
    }
}

impl Default for MemoryTrustStore {
    fn default() -> Self {
        Self::new()
    }
}

forward_trust_store!(MemoryTrustStore);

/// A trust store backed by one JSON file, replaced atomically on every change (write a temporary
/// file in the same directory, flush it to disk, rename over the target).
///
/// A missing file is an empty store. A file that fails validation is an error, never silently
/// reset: dropping trust data without telling the user would hide a problem.
pub struct FileTrustStore(Backed<FilePersist>);

impl FileTrustStore {
    /// Open (or prepare to create) the store at `path`.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        let state = match fs::read(&path) {
            Ok(bytes) => {
                let state: StoreState = serde_json::from_slice(&bytes)
                    .map_err(|_| StoreError::Corrupt("not valid trust store JSON"))?;
                state.validate()?;
                state
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => StoreState::empty(),
            Err(e) => return Err(e.into()),
        };
        Ok(FileTrustStore(Backed {
            state: Mutex::new(state),
            persist: FilePersist { path },
        }))
    }

    /// The backing file.
    pub fn path(&self) -> &Path {
        &self.0.persist.path
    }
}

forward_trust_store!(FileTrustStore);

/// Replace `path` with `bytes` without ever exposing a partial file.
fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    fs::create_dir_all(dir)?;
    let mut suffix = [0u8; 6];
    getrandom::fill(&mut suffix).map_err(|_| io::Error::other("random number generator failed"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?
        .to_string_lossy();
    let tmp = dir.join(format!(".{name}.{}.tmp", crate::codec::hex_encode(&suffix)));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
        return result;
    }
    #[cfg(unix)]
    {
        // Make the rename itself durable. Best effort: not every filesystem allows it.
        if let Ok(dir_handle) = fs::File::open(dir) {
            let _ = dir_handle.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::StaticKeypair;

    fn record(name: &str) -> PeerRecord {
        let pair = StaticKeypair::generate().unwrap();
        PeerRecord::new(*pair.public(), name.into(), "android".into(), 1_000, 5, 1)
    }

    fn exercise(store: &dyn TrustStore) {
        let a = record("Pixel 8");
        let b = record("Galaxy");
        store.upsert(a.clone()).unwrap();
        store.upsert(b.clone()).unwrap();
        assert_eq!(store.list().unwrap().len(), 2);
        assert_eq!(store.get(&a.device_id).unwrap().unwrap().name, "Pixel 8");

        // advance_ts is strictly increasing and persists.
        assert!(!store.advance_ts(&a.device_id, 5).unwrap());
        assert!(!store.advance_ts(&a.device_id, 4).unwrap());
        assert!(store.advance_ts(&a.device_id, 6).unwrap());
        assert!(!store.advance_ts(&a.device_id, 6).unwrap());
        assert!(!store.advance_ts(&DeviceId([9; 16]), 100).unwrap());

        // reserve_ts is max(now, last + 1).
        assert_eq!(store.reserve_ts(&b.device_id, 2_000).unwrap(), Some(2_000));
        assert_eq!(store.reserve_ts(&b.device_id, 1_000).unwrap(), Some(2_001));
        assert_eq!(store.reserve_ts(&b.device_id, -5).unwrap(), Some(2_002));
        assert_eq!(store.reserve_ts(&DeviceId([9; 16]), 1).unwrap(), None);

        // Session updates raise the floor but never lower it.
        let update = SessionUpdate {
            last_seen_ms: Some(77),
            proto_floor: Some(3),
            beacon_key: Some([4; 32]),
        };
        assert!(store.record_session(&a.device_id, update).unwrap());
        let lower = SessionUpdate {
            proto_floor: Some(1),
            ..SessionUpdate::default()
        };
        assert!(store.record_session(&a.device_id, lower).unwrap());
        let got = store.get(&a.device_id).unwrap().unwrap();
        assert_eq!(
            (got.last_seen_ms, got.proto_floor, got.beacon_key),
            (Some(77), 3, Some([4; 32]))
        );

        store.set_beacon_key(Some([1; 32])).unwrap();
        assert_eq!(store.beacon_key().unwrap(), Some([1; 32]));
        store.set_own_public(Some([2; 32])).unwrap();
        assert_eq!(store.own_public().unwrap(), Some([2; 32]));

        assert!(store.revoke(&a.device_id).unwrap());
        assert!(!store.revoke(&a.device_id).unwrap());
        assert!(store.get(&a.device_id).unwrap().is_none());
        store.clear_peers().unwrap();
        assert!(store.list().unwrap().is_empty());
        assert_eq!(store.beacon_key().unwrap(), None);
    }

    #[test]
    fn memory_store_behaves() {
        exercise(&MemoryTrustStore::new());
    }

    #[test]
    fn file_store_behaves_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("trust.json");
        exercise(&FileTrustStore::open(&path).unwrap());

        let a = record("Pixel 8");
        let store = FileTrustStore::open(&path).unwrap();
        store.upsert(a.clone()).unwrap();
        store.advance_ts(&a.device_id, 99).unwrap();
        drop(store);
        let reopened = FileTrustStore::open(&path).unwrap();
        let got = reopened.get(&a.device_id).unwrap().unwrap();
        assert_eq!((got.static_pub, got.last_ts), (a.static_pub, 99));
    }

    #[test]
    fn file_writes_leave_no_temporary_files_and_are_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let store = FileTrustStore::open(&path).unwrap();
        store.upsert(record("x")).unwrap();
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["trust.json".to_string()]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o077,
                0,
                "trust file must not be group/world readable"
            );
        }
    }

    #[test]
    fn corrupt_files_are_errors_not_resets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        fs::write(&path, b"{ not json").unwrap();
        assert!(matches!(
            FileTrustStore::open(&path),
            Err(StoreError::Corrupt(_))
        ));
        // A record whose device id does not match its key is rejected on load.
        let good = FileTrustStore::open(dir.path().join("ok.json")).unwrap();
        good.upsert(record("a")).unwrap();
        let text = fs::read_to_string(good.path()).unwrap();
        let bad = text.replace("\"device_id\": \"", "\"device_id\": \"ff");
        fs::write(&path, bad).unwrap();
        assert!(FileTrustStore::open(&path).is_err());
        // Unknown fields are rejected too.
        fs::write(&path, br#"{"version":1,"peers":[],"surprise":true}"#).unwrap();
        assert!(FileTrustStore::open(&path).is_err());
    }

    #[test]
    fn a_failed_save_does_not_change_memory() {
        let dir = tempfile::tempdir().unwrap();
        // The "file" is a directory, so every save fails.
        let path = dir.path().join("trust.json");
        let store = FileTrustStore::open(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(store.upsert(record("a")).is_err());
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn upsert_rejects_inconsistent_device_ids() {
        let store = MemoryTrustStore::new();
        let mut bad = record("a");
        bad.device_id = DeviceId([1; 16]);
        assert!(store.upsert(bad).is_err());
    }

    #[test]
    fn debug_output_hides_keys() {
        let mut r = record("a");
        r.beacon_key = Some([0xAA; 32]);
        let shown = format!("{r:?}");
        assert!(!shown.contains("170"));
        assert!(!shown.to_lowercase().contains("beacon"));
    }
}
