# Cooldown pairing and sync protocol (v1)

Status: draft for security review, 2026-10-10. This implements spec §6 (`docs/superpowers/specs/2026-10-10-cooldown-design.md`), and §9 lists where it departs from the spec. MUST, MUST NOT and SHOULD are normative.

## 1. Threat model

**Assets:** (1) API keys and tokens on the PC. They never enter this protocol, and a break of the protocol must not reach them. (2) Static private keys and the pairing PSK. (3) Integrity of snapshots and alert times, since a forged reset time means a wrong alarm. (4) Confidentiality of snapshots (low sensitivity). (5) Availability.

**Trust assumptions:** the user opens the QR on the PC's own screen, in the native window; the screen-to-camera path is an authentic out-of-band channel; OS sandboxes and credential stores work as documented; the phone only ever initiates connections and never listens.

| Attacker | Defence |
|---|---|
| Passive LAN observer (Wi-Fi, malicious guest) | Everything travels inside Noise. Beacons carry no names or keys (§4). Sizes and timing still leak; padding reduces this. |
| Active LAN MITM (ARP or DHCP spoofing, rogue AP) | Noise IK authenticates both sides, and the PC's static key comes from the QR. We accept that dropping packets is a DoS. |
| Captive, shared or public networks | No listener on networks with the Windows Public profile, re-checked every 120 s (§4.3, including the known 120 s window). |
| Web page scanning the LAN or 127.0.0.1, DNS rebinding | The sync port speaks raw TCP, not HTTP or WebSocket, so a browser cannot talk to it. The app has no HTTP listener at all today; a future local dashboard would need the controls in §5 (not implemented). |
| Rogue app on the phone | Only two components are exported, both required by Android: the launcher activity and the home-screen widget receiver `CooldownWidgetReceiver` (the widget host must reach it; it only draws state the app already stores). The alarm and boot receivers, the QR scanner and the file provider are not exported. The phone app never puts the pairing code on the clipboard; confirm buttons set `filterTouchesWhenObscured`; keys are wrapped by the Keystore. |
| Other-user or low-integrity process on the PC | The same defence as the LAN attacker: the sync port needs the Noise handshake (a paired key, or the live pairing secret). Nothing else is exposed, because the app has no local HTTP listener. A per-launch token for a possible future local dashboard is described in §5 (not implemented). |
| Stolen phone | Can read snapshots on the same LAN until the user revokes it on the PC (§2.8). |
| QR photographer or shoulder-surfer | 120 s window, single use, and a SAS the user confirms on the PC (§2.5). The copyable QR text (§2.5) is the same secret, so the same limits apply. |
| Replay | Noise nonces, a monotonic `ts` in reconnect message 1, a single-use PSK. |
| Downgrade | ClientHello bound into the prologue, plus a stored version floor (§3.4). |
| Flooding | Limits in §6. |

**Out of scope:** malware running as the same Windows user (it can read Credential Manager and the API keys anyway), root or Keystore extraction on the phone, physical access to an unlocked PC, jamming, side channels in X25519 or ChaChaPoly, and traffic analysis beyond padding.

## 2. Cryptographic design

### 2.1 Pattern choice

| Option | Verdict |
|---|---|
| `Noise_XXpsk3` (in the spec) | Rejected. The PC would send its static key to anyone who connects, which lets a scanner track it. The PSK is only checked at message 3. XX is for an initiator that does not know the responder's key, and the QR already supplies it. |
| `Noise_IKpsk2` (WireGuard) | Sound, but the PC replies before it has verified the PSK. |
| **`Noise_IKpsk1`** | **First pairing.** The phone knows `rs` from the QR. The PSK is mixed at the end of message 1, so the PC rejects a wrong PSK before replying. The phone's static key and payload are encrypted, and the PC never transmits its static key. |
| **`Noise_IK`** | **Reconnects**, as the spec says. |
| PAKE (CPace) | Unnecessary while the QR carries a 256-bit secret. It would only be needed for a typed *short* code, which v1 does not have: the fallback for a camera that cannot scan is the full QR text, copied on the PC and pasted on the phone (§2.5). |

The full names are `Noise_IKpsk1_25519_ChaChaPoly_BLAKE2s` and `Noise_IK_25519_ChaChaPoly_BLAKE2s`. ChaChaPoly is fast on phones without AES hardware. Both names are in snow's bundled `tests/vectors/cacophony.txt`, and `noise-java` supports them, which keeps a native Kotlin fallback possible.

### 2.2 Identities and key storage

- Static X25519 key pairs come from `snow::Builder::generate_keypair()` on first launch. `device_id` = hex of the first 16 bytes of `BLAKE2s-256(static_pub)`, and it fills `SyncPayload.deviceId`.
- **PC.** The private key goes in `keyring` (service `cooldown`, user `noise-static-v1`), base64. The public key is also kept in SQLite to detect a lost or mismatched entry. Peers (public key, `device_id`, name, `paired_at`, `last_seen`, `last_ts`) live in SQLite; all of that is public data. **Check:** if `keyring`'s Windows backend uses `CRED_PERSIST_ENTERPRISE`, the credential roams with domain profiles and the identity gets cloned. Prefer `LOCAL_MACHINE`, through raw Win32 if necessary. (Implemented as `persistence=local` in `KeyringIdentityStore`, which also rewrites an Enterprise-level entry it finds; untested on real Windows hardware.)
- **Android.** The Keystore cannot perform the X25519 DH that snow needs, so the raw key must exist in process memory. Generate it in Rust, wrap it with a non-exportable AES-256-GCM Keystore key (StrongBox if available), and store the blob, the `beacon_key` and the PC record in app-private storage. Exclude all of it from Auto Backup and device transfer with `dataExtractionRules`.
- Hold secrets in `zeroize::Zeroizing`. Snow does not appear to depend on `zeroize`, so assume its internal copies are not wiped.

### 2.3 QR payload (v1)

The payload is `CD1:` followed by RFC 4648 base32 (upper case, no padding), in QR alphanumeric mode at ECC level M. It is opaque text, not a URL, so camera apps don't act on it.

```
off  len  field
0    1    qr_version = 0x01
1    32   pc_static_pub (X25519)
33   32   psk                (OsRng, single use)
65   16   pair_id            (OsRng)
81   2    tcp_port           (u16 BE)
83   1    n_addr             (1..4)
84   …    addr: family u8 (0x04|0x06) + 4 or 16 bytes
```

It is at most about 152 bytes. The phone tries the addresses in parallel with a 250 ms stagger. Link-local IPv6 is excluded. A Tailscale address can be added on request.

### 2.4 Prologue

`prologue = "cooldown/noise/v1" (17 B) || ClientHello (24 B, §3.1) || pc_device_id (16 B)`. Tampering with the cleartext hello (versions, mode, `pair_id`) makes the handshake fail.

### 2.5 First pairing

1. **Pair a phone** exists in the native window only. It creates `psk` and `pair_id` and opens a **120 s window** (one at a time, measured on a monotonic clock). The QR is shown with a countdown and a Hide button. Under it, **Cannot scan? Copy the code** shows the QR text itself (`CD1:…`) with a **Copy code** button, for a phone whose camera cannot read the QR; the phone's **Paste code instead** takes it. It is the same one-time secret: it is never logged, it is shown only while the QR is shown, and the window drops both (and closes the pairing window) when the user leaves the screen or closes the window to the tray.
2. The phone obtains `ACCESS_LOCAL_NETWORK`, scans, and sends `ClientHello{PAIR, pair_id}` plus message 1, with payload `{name ≤32, platform, ts}`.
3. Before doing any DH, the PC checks mode, `pair_id`, that the window is open and that it is not busy. It then reads message 1. On success it **locks** the window (later attempts get `BUSY`) and replies with message 2, payload `{pc_name, proto, beacon_key, server_time_ms}`.
4. Within 10 s the phone's first transport message MUST be `HELLO`; this is key confirmation.
5. **SAS:** `u64_be(BLAKE2s-256("cooldown-sas-v1" || h)[0..8]) mod 10^6`, shown as `123 456`, where `h` is `get_handshake_hash()` read before `into_transport_mode()`. The phone shows it. The PC shows *"Pair ‘Pixel 8’? Only confirm if this code is on YOUR phone"*, and the user MUST click Confirm within 60 s.
6. On Confirm the PC stores the peer, sends `PAIR_OK`, closes the window and zeroizes `psk`. The phone stores nothing until it receives `PAIR_OK`. On Reject or timeout the PC sends `REVOKE` and burns the window.

Because the QR is already an authentic channel, the SAS is not there to stop a MITM. It catches **a race by someone who photographed the QR**. If they finish first, the user's phone gets `BUSY` and shows no matching code, so the user rejects. **Five failed PAIR handshakes burn the window**; with a 256-bit PSK, a failure means a bug or an attack.

### 2.6 Reconnect

`Noise_IK` with stored statics, `mode=RECONNECT` and `pair_id` all zeros. The message-1 payload is `{ts}` with `ts = max(now_ms, last_sent+1)`, persisted on the phone. The PC rejects `ts ≤ last_ts` for that peer, which stops replay of message 1 (WireGuard's TAI64N idea). A session becomes **active**, and replaces an older one from the same peer, only once the phone's first transport message arrives. A replayed message 1 therefore cannot evict a live session.

### 2.7 Lifetime and rotation

- **No in-session rekey.** The connection closes and re-handshakes after 24 h or 2^32 messages, so there is no rekey coordination to get wrong.
- The PC's static key does not rotate in v1. **Reset pairing identity** makes a new one and drops every peer, so all phones re-pair. An authenticated `KEY_ROTATE` is future work.
- `beacon_key` rotates on every revocation and is pushed to the remaining phones in `HELLO`.

### 2.8 Revocation

| Event | Effect |
|---|---|
| PC removes a phone | Sends `REVOKE` if the phone is connected, closes the session, deletes the static key and the record, and rotates `beacon_key`. Later attempts get a generic `FAIL`. |
| Phone removes the PC, or receives an authenticated `REVOKE` | Deletes the record, the wrapped keys and `beacon_key`, and cancels that PC's alarms. If it receives `REVOKE` it shows "Unpaired by PC". The PC lists stale peers with `last_seen`. |
| Phone receives a cleartext `FAIL` | MUST NOT unpair, because the message can be forged. After repeated failures it offers "Re-pair?" and lets the user choose. |

### 2.9 Handling `snow` (not formally audited)

Pin the version (the local clone is 0.10.0), commit `Cargo.lock`, and check RustSec. I recall RUSTSEC-2024-0011, an unauthenticated nonce increment fixed in 0.9.5; verify it.

- After **any** `HandshakeState::read_message` error, drop the state. Snow restores a checkpoint on failure, so keeping the state would let an attacker retry.
- After any transport decrypt error, close the connection. Snow does not advance the nonce on failure; never resync.
- Use only the stateful `TransportState`. Never use `StatelessTransportState`, `set_receiving_nonce`, `dangerously_get_raw_split` or `risky-raw-split`. Put `fixed_ephemeral_key_for_testing_only` behind `#[cfg(test)]`.
- A Noise message is at most 65535 bytes including the 16-byte tag. Reject anything longer before snow sees it, and size the buffers explicitly.
- The 0.10.0 builder methods (`local_private_key`, `remote_public_key`, `psk(1, &[u8;32])`, `prologue`) return `Result`; 0.9 may differ. Check docs.rs for the pinned version.

## 3. Wire format (TCP)

Plain TCP, so browsers can't speak it. The default port is 53320 (placeholder) with an OS-assigned fallback; the actual port travels in the QR and in beacons. Integers are big-endian.

### 3.1 Handshake

```
C→S ClientHello (24 B, unframed)
    0 4  magic "CDWN"     4 1 proto_min   5 1 proto_max   6 1 mode 0x01 PAIR | 0x02 RECONNECT
    7 1  flags (must be 0)                8 16 pair_id (zeros if RECONNECT)
C→S Frame(msg1)
S→C status u8: 0x00 OK, then Frame(msg2)
               0x01 VERSION (+ server_max u8) | 0x02 NOT_PAIRING | 0x03 BUSY
               0x04 RATE_LIMITED | 0x10 FAIL (every crypto or unknown-peer error), then close
Frame = len u16 (1..65535) || bytes
```

A bad magic (for example `GET `) is closed silently. Using one `FAIL` code for all crypto errors removes the oracle.

### 3.2 Transport

Each frame is `Frame(noise_ciphertext)`, with this plaintext:

```
0 1 type | 1 1 flags (0) | 2 2 body_len | 4 body (UTF-8 JSON) | zero padding
```

`body_len` is at most 61,440. A larger body, an unknown type, non-zero padding or bad JSON produces `ERROR` and a close. SYNC is padded to the next 1 KiB. Each connection buffers at most 128 KiB.

| Type | Dir | Body |
|---|---|---|
| 0x01 HELLO | both | `{app, platform, proto, now_ms, last_epoch, last_rev}`; the PC adds `beacon_key` |
| 0x02 PAIR_OK | PC→P | `{}` |
| 0x03 SYNC | PC→P | `{epoch, rev, payload: SyncPayload}` |
| 0x04 ACK | P→PC | `{epoch, rev}` |
| 0x05 ALERT_SET | P→PC | `{op_id, id, enabled}`; the PC remembers the last 64 `op_id`s |
| 0x06/0x07 PING/PONG | both | `{n}` |
| 0x08 REVOKE | both | `{reason}`; the sender then closes |
| 0x7F ERROR | both | `{code}`; then close |

### 3.3 Sync semantics

- **Only `SyncPayload` (spec §4) crosses the wire.** It is built by an **allowlist serializer**, never by serializing internal structs. A scrubber replaces with `"[redacted]"` any `account`, `note` or label that matches a secret pattern (`sk-`, `Bearer`, a JWT shape, base64 of 32+ characters, `@`). Limits: 200 snapshots, 200 alerts, 128-character strings, 256-character `note`. The phone validates against the JSON Schema with the same limits.
- **Idempotency.** `(epoch, rev)` only moves forward. `epoch` is a random u64 made with the database and again when a restore is detected, so a restored PC cannot freeze its phones. The phone applies a SYNC only if the `epoch` is new or the `rev` is higher, and ACKs every SYNC.
- **Flow control.** At most one unacked SYNC, at most one push every 2 s, newest state wins. PING after 30 s idle; close after 90 s of silence. Android holds no background socket. It connects when the app opens, on a network change, and from a periodic WorkManager job.
- **Alert intents.** The PC is authoritative. The phone applies a toggle locally, marks it pending and sends `ALERT_SET`; the pending mark clears when a SYNC reflects the change.

### 3.4 Versioning

The PC MUST pick the highest version in `[proto_min, proto_max] ∩ supported` and echo it inside the encrypted HELLO. The offer is in the prologue, so it cannot be altered. A `VERSION` reject is unauthenticated, so the phone keeps a per-PC `proto_floor` (the highest version it has completed) and never offers lower. A new major version means a new prologue string.

### 3.5 Clocks

`resetsAt` is absolute UTC from the provider. The phone sets RTC alarms at `resetsAt` on its own clock. It estimates `offset ≈ pc_now − (phone_now − rtt/2)` from `HELLO`. If |offset| > 120 s it shows a warning. The offset is used only to display ages, never to move alarms. No security decision uses wall-clock time.

## 4. Discovery

### 4.1 Beacon

This adapts LocalSend's announce pattern (`packages/core/src/multicast`): group **224.0.0.167**, chosen because some Android devices only receive 224.0.0.0/24; TTL 1; one socket per interface; bursts at 100, 500 and 2000 ms. It uses **its own UDP port (53319, placeholder)**, because 53317 is LocalSend's and each app would parse the other's datagrams.

```
0 4 magic "CDBC" | 4 1 ver=1 | 5 1 kind 0x01 ANNOUNCE (PC) / 0x02 PROBE (phone)
6 2 tcp_port (0 in PROBE) | 8 8 nonce (random) | 16 16 tag
tag = BLAKE2s-128(key = beacon_key, "cd-beacon-v1" || bytes[0..16])
```

- **What it reveals:** that some Cooldown device is at this IP, and its port. It carries no name, ID or key, and the random nonce keeps tags unlinkable. A replayed beacon only points the phone at an address where Noise fails.
- A PC with no peers has no `beacon_key`, so it sends nothing and ignores probes.
- The PC announces at startup, on a network change and after a revocation, with no heartbeat. It answers a valid PROBE with a unicast ANNOUNCE, at most once per IP every 2 s.
- **After a DHCP change** the phone tries its cached addresses, then sends a PROBE burst (holding `WifiManager.MulticastLock` only during discovery), then connects to the source IP of a valid ANNOUNCE. The address is never trusted; Noise proves identity.

### 4.2 mDNS compared with the beacon

mDNS (`_cooldown._tcp`) is standard and Android supports it through `NsdManager`. It broadcasts an instance name, shares port 5353 with Windows' built-in responder, and Android support has been uneven. The beacon leaks less and LocalSend has used the same pattern in the field. **v1 uses the beacon only.**

### 4.3 Platform gates

- **Windows.** The app listens (TCP and the UDP beacon) **only on addresses of networks Windows does not classify as Public**, so Private and DomainAuthenticated networks. It asks Windows for the profiles with `Get-NetConnectionProfile` (PowerShell started by its full `System32` path, no window) and intersects the answer with the machine's private-range IPv4 addresses. If Windows cannot be asked, or no network qualifies, it listens on nothing (fail closed). The server is not started at all until a phone is paired or the user opens **Pair a device**.
  - **Re-check.** While the server runs, the app repeats the check **every 120 s**. If the set of allowed addresses differs from the bound one (a new network, or one that turned Public), it stops and restarts the server on the allowed addresses, or leaves it stopped when none are allowed. While paired phones exist and the server is stopped, the same 120 s check tries to start it again, so it comes back after a Wi-Fi switch or a wake from sleep. There is no change-event subscription (`INetworkListManager` is not used).
  - **Firewall.** The installer adds **no** firewall rules (the NSIS installer has no such hook). The first time the app listens, Windows Defender Firewall prompts; the docs tell users to tick **Private networks only**. Whatever the user answers is the only firewall rule there is, so it is not a reliable second layer behind the profile check (a user who also ticks Public leaves nothing behind it). The bind-address filtering above is therefore the control that keeps the listener off Public networks. The beacon listener is bound to `0.0.0.0` (crate README, item 19), so the beacon additionally answers only sources inside the subnets of the allowed interfaces (crate README, item 25).
  - **Known limitation: a window of up to 120 s.** The check is periodic. After the PC moves to a Public network (for example a café Wi-Fi after waking from sleep), the server can keep listening on the old address until the next check, for up to 120 s. Over that time the exposure is the pre-authentication surface: the hello and handshake limits of §6 and, for the beacon, a keyed PROBE that only a paired phone can produce. Nothing is exposed without the Noise handshake. Closing the gap needs a network-change event or a shorter interval, which v1 does not do.
  - **What the user sees.** Starting **Pair a device** with no qualifying network fails with "This PC is not on a network Windows treats as private. In Windows Settings > Network & internet, set your home Wi-Fi to Private, then try again." A server stopped by the background check is only written to the log (`not listening for phones: ...`); v1 has no persistent banner.
- **Android 17+.** Request `ACCESS_LOCAL_NETWORK` with a rationale when pairing starts; if it is denied, deep-link to Settings. Without it the phone keeps its last snapshot and alarms. Verify the semantics against the Android 17 docs.

## 5. Local browser dashboard (not implemented; future design)

> **Status: not implemented.** v1 has no local browser dashboard and no listener on `127.0.0.1`. The text below is the design a future version would have to meet before it ships; it is kept so that the decision is not lost. Nothing in the current app, the guides or the threat model relies on it.

- This is a **separate listener** on `127.0.0.1` (never `0.0.0.0`) on an OS-assigned port that changes each launch. Keeping it apart means a bind or routing bug cannot put an HTTP API on the LAN. It also has different authentication (a bearer token instead of Noise) and different attackers (browsers instead of the LAN).
- **Token.** 256 random bits per launch. The app opens `http://127.0.0.1:P/#t=<b64url>`. The page moves the token to `sessionStorage`, strips it with `history.replaceState`, and sends `Authorization: Bearer` on every `/api/*` request. Cookies are not used, because they are shared across ports on 127.0.0.1. The custom header also forces a preflight on any cross-origin request, and preflights are refused.
- **Every request:** `Host` must be exactly `127.0.0.1:P` or `localhost:P`, otherwise 421 (this blocks DNS rebinding). `Origin`, when present, must be `http://127.0.0.1:P` or `http://localhost:P`, and it is required on non-GET requests. `Sec-Fetch-Site`, when present, must be `same-origin` or `none`. No `Access-Control-Allow-*` headers, ever; `OPTIONS` returns 405. The token is compared in constant time. Headers: CSP `default-src 'self'; frame-ancestors 'none'`, `nosniff`, `no-referrer`, `no-store`. Provider strings are rendered as text.
- **Exposed:** sanitized snapshots, alerts and alert toggles. **Never exposed:** keys, adapter settings, pairing or the QR, the device list or revocation, logs and file paths. Those stay in the native window's IPC.

## 6. Abuse limits and logging

| Limit | Value |
|---|---|
| Unauthenticated connections | 32 in total, 4 per IP |
| ClientHello deadline / handshake deadline | 2 s / 5 s after accept |
| Handshake frame size | ≤ 512 B |
| Handshake rate | 10/min per IP and 60/min overall (token bucket), then `RATE_LIMITED` |
| PAIR with no window open | `NOT_PAIRING`, before any DH |
| PAIR failures per window | 5, then the window is burned |
| Peers per PC | 8 |

A reconnect from an unknown phone costs two X25519 operations, which is acceptable under these limits. **Logging:** never log keys, PSKs, QR text, `h`, the SAS, payloads, device names or tags. Log event codes, a 4-byte `device_id` prefix, the IP, counts and durations. Rate-limit the logs. Logs stay local; there is no telemetry.

## 7. Failure and edge cases

| Case | Behaviour |
|---|---|
| Phone away from the network | Keeps the last payload and its alarms, and shows "as of". Reconnects through cached addresses or a PROBE. |
| Wrong clocks | §3.5. Windows and rate limits use monotonic clocks. |
| Two PCs | Separate records keyed by `pc_device_id`. If both report the same snapshot `id`, the newer `observedAt` wins for display and for the alarm. |
| One PC, two phones | Independent peers with a shared `beacon_key`. |
| PC reinstalled or key-store entry lost | The SQLite public key no longer matches, so the PC makes a new identity and clears its peers. Phones get `FAIL` and offer to re-pair. The uninstaller does **not** delete the credential (the NSIS installer has no uninstall hooks, so the pairing key stays in Credential Manager until the user removes the `cooldown` entries by hand). |
| Phone reinstalled | New static key, so it re-pairs. The old record stays until it is removed. |
| Backup and restore | Phone keys are excluded, and a restored blob is useless without the Keystore key. A restored PC database gets a new `epoch`. |
| Cloned device | Two simultaneous sessions from one static key are flagged as a possible clone. This is detected, not prevented. |
| Pairing interrupted | Nothing is stored before `PAIR_OK`. Pair again with a new QR. |

## 8. Test plan

1. **Vectors.** In CI, run snow's cacophony vectors for both protocol names. Keep golden vectors of our own (fixed statics, ephemerals and PSK, then the prologue, messages 1 and 2, `h`, SAS, QR bytes and beacon tag), and cross-check them once against a second implementation (`noise-java` or Python `noiseprotocol`).
2. **Property tests (proptest).** Encode/decode round trips for the QR, ClientHello, frames, envelope and beacons. Applying the same `(epoch, rev)` or `op_id` twice has the same effect as once. Serializer output never matches a scrubber pattern.
3. **Fuzzing (cargo-fuzz).** Frame reader, ClientHello, envelope plus JSON, QR parser, beacon parser, and, if §5 is ever built, the dashboard's Host/Origin/token check.
4. **Negative tests.** Wrong PSK or `pair_id`; an expired, busy or burned window; the 6th failure; an unknown phone; the wrong PC key; message 1 replayed with equal and with lower `ts`; a replayed transport frame; a truncated hello or frame; length 0, length 65536 and a body over 61,440 bytes; wrong magic or version; non-zero flags or padding; a lowered `proto_max`; a forged `VERSION` or `FAIL`; missed deadlines; a first message that is not HELLO; SYNC before `PAIR_OK`. If §5 is ever built, add dashboard requests with a bad Host, a foreign Origin, no token, cross-site `Sec-Fetch-Site`, or `OPTIONS`.
5. **MUST be re-reviewed against the real code before publishing:** (a) every snow call, error path and buffer size; (b) single use and zeroization of the PSK; (c) the SAS gate (nothing stored before Confirm); (d) the `ts` replay check and the order of session activation; (e) the allowlist serializer and scrubber; (f) bind addresses and Public-profile behaviour (the installer adds no firewall rules, §4.3); (g) if §5 is ever built, the dashboard's Host, Origin and token checks and its CSP; (h) `keyring` persistence and Android backup exclusion; (i) every log statement; (j) `Cargo.lock` and RustSec status.

## 9. Decisions and open questions

- **Departures from the spec:** first pairing uses `IKpsk1` instead of `XXpsk3`; discovery uses its own UDP port instead of LocalSend's 53317.
- **For a cryptographer:** IKpsk1 compared with IKpsk2 here (both appear among the Noise spec's examples); whether a 20-bit SAS is acceptable as defence in depth; whether reconnects should use `IKpsk2` with a per-pair link PSK as a hedge against future attacks on X25519.
- **Unverified:** the `keyring` persistence flag on Windows, and whether to store bytes through `set_secret`; how Android 17's `ACCESS_LOCAL_NETWORK` actually behaves; how reliable 224.0.0.167 reception is on the target phones; snow's RustSec IDs and the API differences between versions; whether placeholder ports 53319 and 53320 clash with anything.
- **Accepted risks:** the X25519 private key in phone memory; the copied pairing text lingering on the PC clipboard (and in Windows clipboard history) and in whatever channel carries it to the phone, which matters only for the 120 s before it expires or is used; a race by someone who photographed the QR, which the SAS should catch; users answering the Windows firewall prompt wrongly; up to 120 s of listening after the PC joins a Public network (§4.3).
