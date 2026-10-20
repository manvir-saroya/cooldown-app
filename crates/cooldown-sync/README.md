# cooldown-sync

Pairing and sync between the Cooldown PC app and a paired Android phone: QR pairing payload, Noise handshakes (`snow`), framing and messages, the trust and identity stores, the two endpoints (`PcServer` and `PhoneClient`), and the LAN discovery beacon that lets the phone find the PC again after a DHCP change.

It implements `docs/pairing-protocol.md` (the authoritative specification, written by a security designer). Read that first; this README says how the crate is put together, where it interprets or departs from the document, what it does not do yet, and what could not be verified.

The same crate is compiled for Windows (the PC) and Android (the phone, through Tauri mobile). It performs no UI work, reads no credentials of its own and never touches the operating system's keychain except through [`KeyringIdentityStore`](#identity-keys-and-the-keyring-finding), which tests do not call.

## What crosses the wire

Only `cooldown_core::SyncPayload` (rebuilt field by field and scrubbed first, see below), plus control messages. Never API keys, tokens or cookies; this crate never sees them.

```text
phone                                              PC
  |  ClientHello (24 B, cleartext, bound into prologue)
  |  Frame(msg 1)  ---------------------------------->  window / pair_id / rate gate (no DH yet)
  |                                                     read msg 1 (PSK checked here, then reply)
  |  <----------------------- status 0x00, Frame(msg 2)
  |  HELLO (first transport message = key confirmation)
  |                                    [pairing only]   show 6-digit code, user confirms (60 s)
  |  <----------------------------------------------  PAIR_OK, then HELLO (with beacon_key)
  |  <-- SYNC {epoch, rev, payload}  /  --> ACK  /  --> ALERT_SET  /  PING, PONG, REVOKE, ERROR
```

First pairing is `Noise_IKpsk1_25519_ChaChaPoly_BLAKE2s`, reconnects are `Noise_IK_25519_ChaChaPoly_BLAKE2s`. The Noise prologue is `"cooldown/noise/v1" || ClientHello || pc_device_id`, so the offered versions, the mode and the `pair_id` cannot be altered in transit.

## Modules

| Module | What it is |
|---|---|
| `qr` | `QrPayload` encode/decode: `CD1:` + base32 (upper case, no padding), strict and canonical (one spelling per payload), rejects unusable addresses and small-order keys, never panics. |
| `hello` | The 24-byte `ClientHello`, the status bytes, version negotiation (highest common version, never below the phone's stored floor). |
| `noise` | A thin `snow` wrapper: protocol names, prologue, the confirmation code, `Handshake` (a failed `read` consumes the state) and `Transport`. No rekey, no raw split, no stateless transport, no manual nonces. |
| `frame` | `len u16` frames (cancel-safe `FrameReader`, length checked from the header before the body is read) and the padded envelope `type | flags | body_len | body | zero padding`. |
| `message` | JSON bodies for every message, all `deny_unknown_fields`. |
| `sanitize` | The allowlist copy of `SyncPayload` and the secret scrubber, plus the phone's incoming checks. |
| `trust` | `TrustStore` trait, `MemoryTrustStore`, `FileTrustStore` (atomic JSON file: temp file in the same directory, fsync, rename; 0600 on Unix). Public data only, plus the beacon key. |
| `identity` | `IdentityStore` trait, `MemoryIdentityStore`, `KeyringIdentityStore`, `load_or_create_pc_identity`, `reset_pc_identity`. |
| `server` | `PcServer`, `ServerConfig`, `ServerEvent`, `AlertRequest`. |
| `client` | `PhoneClient`, `PendingPairing`, `Connection`, `ReceivedSync`, `should_apply`. |
| `limits` | Every timeout and abuse limit, defaults taken from the document. |
| `clock` | `SyncClock` (monotonic time, wall time, sleep). `SystemSyncClock` in production; `cooldown_core::FakeClock` implements it for tests. |
| `beacon` | The discovery beacon wire format (document section 4.1) as pure functions: `Beacon::announce` / `probe` / `encode` / `decode`. 32 bytes, native keyed BLAKE2s-128 tag compared in constant time, strict and canonical, never panics. |
| `discovery` | `BeaconAnnouncer` and `PcLocator` (the traits), `UdpBeaconAnnouncer` (PC) and `UdpPcLocator` (phone): tokio UDP over multicast `224.0.0.167` with one socket per interface, or plain unicast for tests. See "Discovery" below. |

## Using it from a Tauri app

PC side (desktop backend):

```rust
let trust = Arc::new(FileTrustStore::open(app_data.join("trust.json"))?);
let vault = KeyringIdentityStore::native()?;                 // Windows Credential Manager, persistence = local
let (identity, outcome) = load_or_create_pc_identity(&vault, &*trust)?;
if let IdentityOutcome::Regenerated(_) = outcome { /* tell the user: every phone must pair again */ }

let server = PcServer::start(
    ServerConfig::new("Office PC"),
    identity,
    trust.clone(),
    Arc::new(SystemSyncClock::new()),
    &private_network_ips,        // only addresses on Private/Domain networks (your job, see below)
    DEFAULT_PORT,                // or 0 for an OS-assigned port; the real port goes into the QR
).await?;

let mut events = server.subscribe();                          // subscribe BEFORE opening a window
let mut toggles = server.take_alert_requests().unwrap();      // phone-initiated alert toggles

// "Pair a phone": show offer.qr_text as a QR code (native window only; never log or copy it).
let offer = server.open_pairing(&lan_ips)?;                   // 120 s, single use, one at a time
// On ServerEvent::PairingConfirmationRequested { name, code, .. } show
//   "Pair '<name>'? Only confirm if this code is on YOUR phone: <code>"
// and call server.confirm_pairing(true | false) (or confirm_pairing_with_code("123456")) within 60 s.

server.push_sync(&sync_payload)?;       // sanitized, stamped, delivered to every phone, newest wins
server.revoke(&device_id)?;             // REVOKE if connected, record deleted, beacon key rotated
```

Persist the `SyncStamp` that `push_sync` returns and pass it as `ServerConfig::resume` to continue a revision sequence after a restart. The default (a new random epoch, revision 0) is always safe: phones accept a new epoch whatever revision they hold.

Phone side (the Kotlin shell supplies an `IdentityStore` that wraps the raw key with a Keystore key; storage lives in app-private, backup-excluded files):

```rust
let client = PhoneClient::new(ClientConfig::new("Pixel 8", "android"), keypair, trust, clock)?;

let pending = client.begin_pairing(&scanned_qr_text).await?;  // handshake only; nothing stored yet
show(pending.code());                                         // "123 456", the same code the PC shows
let connection = pending.wait_for_confirmation().await?;      // stores the PC only after PAIR_OK

// Later, whenever the app opens, the network changes, or WorkManager fires:
let connection = client.connect(&pc_device_id, &candidate_addrs).await?;
connection.wait_ready().await?;
let latest = client.latest_sync();                            // keep showing it, "as of" received_at_ms
connection.set_alert("reset:claude:work:session_5h", false)?; // the PC is authoritative
match connection.closed().await { CloseReason::Revoked => /* "Unpaired by PC", cancel that PC's alarms */ , _ => {} }
```

An unauthenticated `ClientError::Rejected` (a bare `FAIL`) must never unpair; the app may offer "Re-pair?" after repeated failures. Only `CloseReason::Revoked` (an authenticated `REVOKE`) deletes the record.

## Discovery

The phone's last-known address goes stale when the PC's DHCP lease changes. The PC announces itself with a keyed UDP beacon and the phone probes for it; neither side trusts what it learns, and the Noise handshake still decides who is who. The key is the `beacon_key` that `PcServer` generates when the first phone pairs, rotates on every revocation and delivers to each phone in its HELLO (`TrustStore::beacon_key` on the PC, `PeerRecord::beacon_key` on the phone).

PC side (call `announce` at startup, on `ServerEvent::DevicePaired`, on `ServerEvent::DeviceRevoked`, and after a network change; rebuild the announcer with the new interface list when the set of eligible networks changes):

```rust
// Only the addresses of Private/Domain networks (your job, same rule as PcServer::start).
let announcer = UdpBeaconAnnouncer::multicast(private_ipv4s, clock.clone());
announcer.announce(server.port(), trust.beacon_key()?).await?;   // None = no peers = no socket at all
```

Phone side (when the cached addresses fail; hold `WifiManager.MulticastLock` from the Kotlin shell while it runs):

```rust
let locator = UdpPcLocator::multicast(wifi_ipv4s, clock);
let candidates = locator.locate(record.beacon_key.unwrap(), Duration::from_millis(2500)).await?;
let connection = client.connect(&pc_device_id, &candidates).await?;   // Noise decides, not the beacon
```

| | PC (`UdpBeaconAnnouncer`) | Phone (`UdpPcLocator`) |
|---|---|---|
| Sockets | None without a key. With a key: one listener on `0.0.0.0:53319` joined to the group on each eligible interface, one send socket per interface (bound to the interface address, `IP_MULTICAST_IF`, TTL 1, no loop-back) | Per call, one socket per eligible interface (same options), closed when the call returns |
| Sends | ANNOUNCE at 100, 500 and 2000 ms after each `announce`, each with a fresh nonce; unicast ANNOUNCE to a valid PROBE's source | PROBE at 0, 100, 500 and 2000 ms (those before the timeout), each with a fresh nonce |
| Accepts | Only a valid PROBE for the current key. Everything else is dropped silently, uncounted and unlogged | Only a valid ANNOUNCE for the given key; returns `source IP : announced TCP port`, deduplicated, at most 16 |
| Limit | One reply per source IP per 2 s on the monotonic clock; only a valid PROBE spends the slot | The full timeout is always waited |

`AnnouncerNet::Unicast` / `LocatorNet::Unicast` replace multicast with plain UDP to explicit addresses; the wire format and every check are identical. The tests use them over `127.0.0.1`.

## Where this implementation interprets or departs from the document

These are decisions a security reviewer should check. Each picks the safer reading.

1. **`beacon_key` is not in message 2.** The document lists it in the message-2 payload; it is delivered in the PC's first HELLO, after `PAIR_OK`. A racing attacker who photographed the QR and wins the handshake would otherwise learn it before the user could reject.
2. **A wrong `pair_id` is `NOT_PAIRING` and is not counted** toward the five failures (it cannot be told apart from "no window"). Counting it would let anyone on the LAN burn the window with junk. A failure is anything after the window gate that does not lead to a verified message 1: wrong PSK, malformed or truncated message 1, a client that hangs up, a handshake timeout.
3. **The PSK is single use as soon as message 1 verifies.** Once the window locks, any outcome other than a confirmed pairing (reject, timeout, disconnect, write failure) burns the window, not only an explicit reject. The 120 s limit stops applying once locked; the 60 s confirmation deadline takes over.
4. **The reconnect timestamp is persisted when message 1 verifies**, before the PC replies (the document persists `last_ts` per peer but does not say when). A replay is rejected from that moment; session activation still waits for the phone's first transport message, so a replayed message 1 cannot evict a live session.
5. **A session from a key that already has one replaces it** (as specified) and emits `ServerEvent::PossibleClone`. A phone that reconnects before its old socket timed out triggers the same event, so it is a signal, not proof.
6. **The user-facing code is displayed on both devices**, and a typed-code confirmation (`confirm_pairing_with_code`) is offered in addition to the Confirm button. A wrong typed code rejects the pairing and burns the window.
7. **A phone-sent `REVOKE` unpairs the phone on the PC** (record deleted, beacon key rotated). The document specifies only the phone deleting its own record; honouring it here is the symmetric and harmless reading, since an authenticated phone can only affect itself.
8. **Padding.** SYNC is padded to the next 1 KiB as specified; every other message is padded to a multiple of 64 bytes so control messages look alike. Receivers accept any zero padding.
9. **Additions the document did not specify**: an ACK timeout (30 s unacknowledged SYNC closes the session with `ERROR timeout`), an `ERROR` is sent (then close) for envelope or JSON violations but not for decrypt failures, the PC stamps `SyncPayload.deviceId` with its own id and the phone rejects a SYNC whose id differs, names reject invisible and bidirectional-override characters, an ACK for a revision never sent is a violation, and the scrubber may replace an account label with `[redacted]` (rewriting the snapshot and alert ids to match).
10. **Payload limits can conflict.** 200 snapshots at about 330 bytes each exceed the 61,440-byte body cap. `push_sync` returns an error rather than truncating. The phone enforces 200 entries, 128-character labels and 256-character notes on top of `cooldown-core`'s own validation (core allows 256 entries).
11. **The keyring entry is stored as base64url text** (no padding) rather than standard base64; it is the same 32 bytes in the same entry (`cooldown` / `noise-static-v1`).
12. **A credential with no recorded public key is treated as foreign** (see the keyring finding): the PC makes a fresh identity and drops its peers.
13. **"Lockout"** is implemented as the burned window plus per-IP and global handshake token buckets (`RATE_LIMITED`); there is no per-IP ban list.

Discovery (document section 4):

14. **The tag is native keyed BLAKE2s with a 16-byte digest**, not the first 16 bytes of the 256-bit digest ("BLAKE2s-128(key = beacon_key, ...)" names the former; the two differ). The input is the ASCII string `cd-beacon-v1` followed by bytes 0..16. A golden vector is cross-checked against Python's `hashlib.blake2s(..., key=..., digest_size=16)`.
15. **The decoder is stricter than the layout alone.** A datagram must be exactly 32 bytes (shorter, truncated and padded datagrams are rejected, not ignored). An ANNOUNCE with port 0 and a PROBE with a non-zero port are rejected ("0 in PROBE" is read as a rule, and port 0 is not dialable), so each beacon has exactly one valid spelling. Structural checks run before the tag and depend on no secret; the tag is compared in constant time.
16. **A PC with no peers binds nothing.** The document says it "sends nothing and ignores probes"; not holding a UDP socket at all is the stricter reading. `announce(port, None)` closes the socket and cancels any announcement burst in flight. `announce(0, Some(_))` is an error. After `stop()` the announcer stays stopped (`announce` returns `NotConnected`).
17. **Announcements start at 100 ms, not at 0**: the document adopts LocalSend's bursts "at 100, 500 and 2000 ms" and says there is no heartbeat. A new `announce` call replaces a burst still in flight (a rotated key must not keep announcing under the old one). Key and port changes between calls take effect on the running sockets.
18. **Reply limiting** is per source IP on the monotonic clock. A cheap check runs before the MAC, but only a *valid* PROBE spends the slot, so a flood of junk from a phone's address cannot lock that phone out. The table holds at most 1024 sources; if all of them are still inside their 2 s interval, new sources are refused rather than evicting old ones (that many sources in 2 s is a flood). Replies go to the PROBE's source address and port, from the listener, with TTL 1 in multicast mode.
19. **"One socket per interface" is applied to sending and probing, not to the PC's listener.** Every announcement and every PROBE leaves from a socket bound to one interface address with `IP_MULTICAST_IF` set, so it goes out on exactly that interface (without it the routing table picks, which on a multi-homed Windows PC can mean a Public network or a VPN adapter). The PC's receiving side is one socket on `0.0.0.0:<port>` that joins the group once per eligible interface, because a socket bound to a unicast address does not receive multicast on Linux or macOS and the per-OS alternative could not be tested. The consequence is that a valid PROBE arriving by unicast on any local address is answered, including one on a non-eligible network; the installer adds no firewall rule (document section 4.3), so the second layer is the source-subnet filter of item 25, and only a holder of the beacon key gets an answer.
20. **No address reuse.** The listener sets neither `SO_REUSEADDR` nor `SO_REUSEPORT`: on Windows reuse lets another process bind the same port and share or steal datagrams. The cost is that a second Cooldown instance, or LocalSend if the port were ever the same, fails with `AddrInUse`.
21. **The locator waits the full timeout** ("collect ... until the timeout"). The schedule ends at 2000 ms, so the app should pass about 2.5 s. It does not listen for multicast ANNOUNCEs; it uses the unicast reply to its own PROBE (the document's phone flow). Candidates are `source IP : announced TCP port`; sources that are unspecified, multicast or broadcast, and port 0, are dropped. No candidate is trusted: they are inputs to `PhoneClient::connect`.
22. **The PROBE schedule (0, 100, 500, 2000 ms) is this crate's choice**; the document only says "PROBE burst". The immediate first probe is the one that usually succeeds; the rest cover a lost datagram or reply, and the PC's 2 s limit means later probes are answered only if no reply went out in the last 2 s.
23. **IPv4 only**, as the document's group is IPv4. `MulticastNet::loopback` (default off) is the one extra knob: it turns on `IP_MULTICAST_LOOP` so both ends can run on one host; it is meant for tests.
24. **Rejections close gracefully.** After a status byte the PC half-closes and drains what the phone already sent (at most 1 s and 8 KiB) before dropping the socket. Closing with unread data makes Windows send a TCP reset, which discarded the status byte before the phone could read it (found by the first Windows CI run).
25. **Replayed probes and foreign sources go unanswered.** The announcer remembers PROBE nonces for 10 minutes (`PROBE_NONCE_MEMORY`) and ignores a repeat, so a captured probe cannot be replayed for an ANNOUNCE; no timestamp is added to the beacon, because that would make discovery depend on the phone's and PC's clocks agreeing. And with `with_allowed_sources` it answers only sources inside the eligible interfaces' subnets: the listener is bound to `0.0.0.0` (item 19), so this is what keeps a probe from a non-eligible network unanswered. A spoofed private source gains nothing, the reply goes to that address.

## Identity keys and the keyring finding

The designer flagged: could the PC identity key roam to other machines through the `keyring` crate on Windows?

**What was verified, by reading the source in `~/.cargo/registry` and Microsoft's documentation:**

* `keyring` 4.2.0's `v1` wrapper builds entries with `keyring_core::Entry::new(service, user)` and no modifiers (`src/v1.rs`). On Windows it installs `windows-native-keyring-store` 1.1.0.
* That store creates credentials with **`CRED_PERSIST_ENTERPRISE` by default**: `src/store.rs`, `Store::build`, `.unwrap_or("Enterprise")`, then `CredWriteW` with `Persist: persist` (`src/utils.rs`). It accepts a `persistence` entry modifier (`Session`, `Local`, `Enterprise`) that the `v1` wrapper cannot pass. The level is applied on every write, so a later write through a default-built entry resets it to Enterprise.
* Microsoft's `CREDENTIALW` page: `CRED_PERSIST_ENTERPRISE` is "visible to other logon sessions of this same user on this same computer and to logon sessions for this user on other computers" (it behaves as local if the user has no roaming state), while `CRED_PERSIST_LOCAL_MACHINE` is "not visible to logon sessions for this user on other computers".

So the concern is real for the default configuration. **What this crate does about it:** `KeyringIdentityStore::native()` uses `keyring-core` plus `windows-native-keyring-store` directly (the route the keyring documentation recommends for applications), builds every entry with `persistence=local`, and rewrites a credential found at any other level when it loads. `load_or_create_pc_identity` also records the public key in the machine-local trust store, so a credential that appears on a machine whose trust store does not know it (the roaming or clone case) is treated as foreign: a new identity is created and no peers are trusted.

**What could not be verified:** this machine is a Mac, so the Windows branch of `native()` was neither compiled nor run (only the `aarch64-apple-darwin` target is installed). The tests use `keyring-core`'s mock store and a fake store that reproduces the Windows behaviour read from the source (default Enterprise, `persistence` modifier, level readable as an attribute). I did not observe Credential Manager itself, did not confirm that `CRED_PERSIST_LOCAL_MACHINE` is honoured for generic credentials exactly as documented, and did not exercise an actual roaming profile. The macOS Keychain branch compiles but, by rule, no test calls it. Linux has no `native()` store (apps supply their own `IdentityStore`), and neither does Android.

## Testing

`cargo test -p cooldown-sync` runs everything over real sockets on `127.0.0.1` with fake clocks and in-memory stores.

| What | Where |
|---|---|
| Cacophony vectors for both protocol names, run through this crate's own builder (PSK position, prologue, message order) | `src/noise.rs`, `vectors`, data in `tests/vectors/` |
| Golden vectors for the pairing handshake (prologue, messages 1 and 2, handshake hash, code, QR text), **cross-checked once against Python `noiseprotocol` 0.3.1**, an independent implementation | `src/noise.rs`; reproduce with `tests/vectors/crosscheck_noiseprotocol.py` |
| Every negative case of the document's list (wrong PSK and `pair_id`, expired, busy and burned windows, the sixth failure, unknown phone, wrong PC key, replayed message 1 with equal and lower `ts`, replayed transport frame, tampered ciphertext, truncated, zero-length and oversized frames, bodies over 61,440 bytes, wrong magic and version, a lowered `proto_max`, non-zero flags and padding, forged `VERSION` and `FAIL`, every missed deadline, a first message that is not HELLO, SYNC before `PAIR_OK`, wrong confirmation code, revoked-device reconnect) | `tests/negative_handshake.rs`, `tests/negative_transport.rs`, `tests/client_side.rs`, `tests/limits.rs` |
| Property tests (proptest): frame parser and streaming reader, envelope, `ClientHello`, QR decoder (never panics, one canonical spelling), base32, `(epoch, rev)` and `op_id` idempotency, timestamp monotonicity, scrubber output | `tests/properties.rs` |
| End to end: pair, sync, reconnect, alert toggle, revoke | `tests/e2e.rs` |
| Discovery wire format: golden vectors (cross-checked against Python `hashlib.blake2s`), round trip, every rejection (length, magic, version, kind, port, tag, another key, a replay with a changed port, every single-bit flip) | `src/beacon.rs` |
| Discovery property tests: decoder never panics and is canonical, round trip, mutation and foreign-key rejection, length rejection | `tests/properties.rs` |
| Discovery end to end over `127.0.0.1` with a fake clock: locator finds an announcer with the right key and nothing with the wrong key or with no key on the PC, junk costs no reply slot, replies once per source IP per 2 s, the 100/500/2000 ms and 0/100/500/2000 ms schedules, key rotation and cancellation, `stop`, exclusive port | `tests/discovery.rs`, `src/ratelimit.rs` |
| Real multicast on one host (group join, `IP_MULTICAST_IF`, send to the group, unicast reply), with a skip where the host does not deliver multicast | `tests/multicast.rs` |
| Log hygiene: a capturing subscriber over a full run (pairing, hostile traffic, revocation) must not see keys, the PSK, the QR, the code, names, notes or full device ids | `tests/logging.rs` (`COOLDOWN_SHOW_LOG=1` prints the captured lines) |
| Log hygiene for discovery: a capturing subscriber over probes, junk, replies and a failed send must not see the beacon key, a tag, a nonce or a datagram | `tests/discovery_logging.rs` |

**Fuzzing.** `fuzz/` is a cargo-fuzz layout with eight targets (QR, frames, envelope, hello, message JSON, responder `read`, payload scrubber, beacon decoder). `cargo-fuzz` is **not installed** (no global tools were added). The crate type-checks with `cargo check --manifest-path fuzz/Cargo.toml`, and each target was built and run for 100,000 random inputs with the uninstrumented libFuzzer driver. That is a smoke test of the harnesses, not coverage-guided fuzzing: run `cargo fuzz run <target>` (nightly) for the real thing.

## Not implemented

* **Discovery gaps.** The wire format, the PC announcer, the phone locator and the reply limit are implemented and tested over loopback, and the real multicast socket path was run once on a Mac's loopback interface. Not done: enumerating interfaces and deciding which are eligible (the caller passes their IPv4 addresses), noticing network changes (rebuild the announcer), `WifiManager.MulticastLock` (Kotlin), the phone listening passively for multicast ANNOUNCEs, and IPv6. **Unverified**: multicast between two devices, on Windows, on Android, and on a real LAN interface (the Mac this was built on does not loop multicast back on its Wi-Fi interface even with raw sockets, so only `127.0.0.1` was exercised).
* **Platform gates** (document section 4.3): this crate binds to the addresses it is given (`PcServer::start`, the beacon types) and has no rebind call; deciding which networks are Private or Domain is the app's job. The desktop app asks Windows (`Get-NetConnectionProfile`) and restarts the server and announcer when the allowed set changes, checking every 120 s, so there is a window of up to 120 s after a move to a Public network (a known limitation in the document). The installer adds no firewall rules, and the beacon's UDP listener is wildcard-bound (interpretation 19), so the source-subnet filter (item 25) is what keeps a probe from a non-eligible network unanswered. On Android 17, the `ACCESS_LOCAL_NETWORK` permission flow.
* **The Windows and Android secret stores.** `KeyringIdentityStore` covers Windows (unverified, above) and macOS. Android needs a Kotlin `IdentityStore` that wraps the raw key with a non-exportable Keystore key and excludes it from backup.
* **A SQLite `TrustStore`.** The document keeps peers in SQLite; this crate ships memory and JSON-file implementations behind the trait.
* **The local browser dashboard** (document section 5) is a separate listener and not part of this crate.
* **`KEY_ROTATE`** (an authenticated static-key rotation) is future work in the document too; `reset_pc_identity` is the supported way to change the PC key.
* **App-level behaviour:** merging snapshots from two PCs (newer `observedAt` wins), the pending mark on an alert toggle until a SYNC reflects it, retry and backoff policy for `connect`, and persisting the latest payload for offline display (`PhoneClient::restore_latest` re-seeds it).
* **Cross-implementation check of the reconnect pattern.** The golden cross-check covers `IKpsk1`; `IK` is covered by the Cacophony vector only.

## Security caveats

* **`snow` is not formally audited.** It is pinned exactly (`=0.10.0`) with only the primitives this protocol uses (X25519, ChaChaPoly, BLAKE2s). RUSTSEC-2024-0011 (unauthenticated payloads advancing the nonce in stateful transports) is fixed in 0.9.5, so 0.10.0 is past it; I confirmed that on rustsec.org. I did **not** run `cargo audit` over the whole dependency tree (it is not installed here), so RustSec status of the other crates is unchecked. Commit `Cargo.lock` and re-check before publishing.
* **Snow does not wipe its internal key copies** (it does not depend on `zeroize`). This crate wipes the secrets it owns (private keys, PSKs, QR text, beacon key buffers where practical), but copies inside snow's `HandshakeState` and `TransportState`, and temporaries inside the X25519 code, are outside its control.
* **Snow does not reject an all-zero Diffie-Hellman result.** The QR key and every static key learned in a handshake are screened for small-order points here; ephemeral keys are not, which is safe for IK and IKpsk1 because every later key also mixes in a static-key result the attacker cannot compute.
* **The six-digit code is about 20 bits.** It exists to catch someone who photographed the QR and raced the user, not to defend against a network attacker (the QR already carries a 256-bit secret). The document asks a cryptographer to review that choice, along with `IKpsk1` against `IKpsk2` and a link PSK for reconnects.
* **Sizes and timing leak.** Padding reduces it; it does not remove it.
* **Dropping packets is a denial of service** that the protocol accepts.
* **Windows firewall behaviour, Android permission behaviour, and multicast reliability** are untested here (the document lists them as unverified).
* **Beacons are keyed, not secret.** Every paired phone holds the same `beacon_key` (until a revocation rotates it), so any of them can forge a PROBE or an ANNOUNCE. A sniffed valid ANNOUNCE can be replayed to a phone (the phone keeps no nonce memory), and a sniffed valid PROBE can be replayed once the PC's 10-minute nonce memory (`PROBE_NONCE_MEMORY`, item 25) has forgotten it; within those 10 minutes a repeated PROBE gets no answer. The worst outcomes are a candidate address where Noise then fails, and one 32-byte reply per source IP per 2 s to a spoofed address (no amplification). A beacon reveals that a Cooldown device is at an IP and its TCP port, and nothing else.
* **The beacon key is wiped where this crate owns the copy** (`Zeroizing` in the announcer and locator), but the by-value `[u8; 32]` arguments of the two traits, and the padded key block inside `blake2`'s MAC state (the `zeroize` feature is not enabled), are not.
* The cryptographic test vectors that ship in `tests/vectors/cacophony_ik_subset.json` come from `snow`'s copy of the Cacophony suite; keep their attribution in `THIRD_PARTY_NOTICES.md`.

## Cargo features

`keyring-store` (default) enables `KeyringIdentityStore`. Android builds use `default-features = false`.
