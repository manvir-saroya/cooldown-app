//! Discovery beacons end to end over `127.0.0.1` UDP sockets with a fake clock: a locator finds an
//! announcer that has its key and nothing else, the announcer answers only valid probes and at
//! most once per source IP every 2 s, and announcements and probes go out on the documented
//! schedule. No real multicast is used (see `tests/multicast.rs` for that).

mod common;

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use common::{eventually, fake_clock, settle, within};
use cooldown_core::FakeClock;
use cooldown_sync::beacon::{Beacon, BeaconKind};
use cooldown_sync::discovery::MAX_CANDIDATES;
use cooldown_sync::{
    AnnouncerNet, BeaconAnnouncer, LocatorNet, MulticastNet, PcLocator, SyncClock,
    UdpBeaconAnnouncer, UdpPcLocator,
};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

const KEY: [u8; 32] = [0x11; 32];
const OTHER_KEY: [u8; 32] = [0x22; 32];
const PORT: u16 = 53777;
const LO: Ipv4Addr = Ipv4Addr::LOCALHOST;

type Located = JoinHandle<io::Result<Vec<SocketAddr>>>;

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(LO), port)
}

fn dyn_clock(clock: &Arc<FakeClock>) -> Arc<dyn SyncClock> {
    clock.clone()
}

/// An announcer on an OS-assigned loopback port that sends its announcements to `announce_to`.
fn announcer(clock: &Arc<FakeClock>, announce_to: Vec<SocketAddr>) -> UdpBeaconAnnouncer {
    UdpBeaconAnnouncer::new(
        AnnouncerNet::Unicast {
            bind: loopback(0),
            announce_to,
        },
        dyn_clock(clock),
    )
}

fn locator(clock: &Arc<FakeClock>, probe_to: Vec<SocketAddr>) -> Arc<UdpPcLocator> {
    Arc::new(UdpPcLocator::new(
        LocatorNet::Unicast {
            bind: IpAddr::V4(LO),
            probe_to,
        },
        dyn_clock(clock),
    ))
}

fn spawn_locate(locator: &Arc<UdpPcLocator>, key: [u8; 32], timeout: Duration) -> Located {
    let locator = Arc::clone(locator);
    tokio::spawn(async move { locator.locate(key, timeout).await })
}

/// A plain UDP socket on a loopback port: a hand-driven phone or PC.
async fn raw() -> UdpSocket {
    UdpSocket::bind(loopback(0)).await.unwrap()
}

/// A loopback address nothing is listening on.
fn unused_addr() -> SocketAddr {
    let socket = std::net::UdpSocket::bind(loopback(0)).unwrap();
    socket.local_addr().unwrap()
}

/// The next datagram, or `None` after `wait` of real time. A `ConnectionReset` (Windows reports an
/// earlier ICMP "port unreachable" on the next receive) is not a datagram and is skipped.
async fn recv_for(socket: &UdpSocket, wait: Duration) -> Option<(Vec<u8>, SocketAddr)> {
    let next = async {
        let mut buf = vec![0u8; 4096];
        loop {
            match socket.recv_from(&mut buf).await {
                Ok((len, from)) => return (buf[..len].to_vec(), from),
                Err(error) if error.kind() == io::ErrorKind::ConnectionReset => continue,
                Err(error) => panic!("receive failed: {error}"),
            }
        }
    };
    tokio::time::timeout(wait, next).await.ok()
}

async fn recv(socket: &UdpSocket) -> (Vec<u8>, SocketAddr) {
    within(async { recv_for(socket, Duration::from_secs(10)).await })
        .await
        .expect("a datagram")
}

async fn expect_silence(socket: &UdpSocket) {
    if let Some((datagram, from)) = recv_for(socket, Duration::from_millis(120)).await {
        panic!("unexpected {}-byte datagram from {from}", datagram.len());
    }
}

async fn drain(socket: &UdpSocket) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some((datagram, _)) = recv_for(socket, Duration::from_millis(120)).await {
        out.push(datagram);
    }
    out
}

fn probe(key: &[u8; 32], nonce: u8) -> [u8; 32] {
    Beacon::probe([nonce; 8]).encode(key)
}

fn announce(key: &[u8; 32], port: u16, nonce: u8) -> [u8; 32] {
    Beacon::announce(port, [nonce; 8]).unwrap().encode(key)
}

fn assert_announce(datagram: &[u8], key: &[u8; 32], port: u16) -> Beacon {
    let beacon = Beacon::decode(datagram, key).expect("a valid beacon for this key");
    assert_eq!(beacon.kind(), BeaconKind::Announce);
    assert_eq!(beacon.tcp_port(), port);
    beacon
}

// ---------------------------------------------------------------------------------------------
// Locator and announcer together
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_locator_finds_an_announcer_that_has_its_key() {
    let clock = fake_clock();
    let pc = announcer(&clock, vec![]);
    pc.announce(PORT, Some(KEY)).await.unwrap();
    let phone = locator(
        &clock,
        vec![pc.local_addr().expect("listening once it has a key")],
    );

    let search = spawn_locate(&phone, KEY, Duration::from_secs(3));
    eventually(|| pc.replies_sent() >= 1).await;
    settle().await;
    clock.advance(Duration::from_secs(3)).await;

    // The candidate is the datagram's source IP with the TCP port the beacon named.
    let found = within(search).await.unwrap().unwrap();
    assert_eq!(found, vec![loopback(PORT)]);
}

#[tokio::test]
async fn the_wrong_key_finds_nothing_and_the_right_key_still_works_afterwards() {
    let clock = fake_clock();
    let pc = announcer(&clock, vec![]);
    pc.announce(PORT, Some(KEY)).await.unwrap();
    let phone = locator(&clock, vec![pc.local_addr().unwrap()]);

    let search = spawn_locate(&phone, OTHER_KEY, Duration::from_secs(3));
    settle().await;
    clock.advance(Duration::from_secs(3)).await;
    assert_eq!(within(search).await.unwrap().unwrap(), vec![]);
    assert_eq!(
        pc.replies_sent(),
        0,
        "a probe for another key is not answered"
    );

    // The failed search did not use up the phone's reply slot, and the announcer is still alive.
    let search = spawn_locate(&phone, KEY, Duration::from_secs(3));
    eventually(|| pc.replies_sent() >= 1).await;
    settle().await;
    clock.advance(Duration::from_secs(3)).await;
    assert_eq!(within(search).await.unwrap().unwrap(), vec![loopback(PORT)]);
}

#[tokio::test]
async fn a_pc_with_no_key_listens_on_nothing_and_answers_nothing() {
    let clock = fake_clock();
    let pc = announcer(&clock, vec![]);
    pc.announce(PORT, None).await.unwrap();
    assert_eq!(pc.local_addr(), None, "no peers, so no socket");

    let phone = locator(&clock, vec![unused_addr()]);
    let search = spawn_locate(&phone, KEY, Duration::from_secs(3));
    settle().await;
    clock.advance(Duration::from_secs(3)).await;
    assert_eq!(within(search).await.unwrap().unwrap(), vec![]);
    assert_eq!(pc.replies_sent(), 0);
}

#[tokio::test]
async fn losing_the_key_closes_the_socket_and_stops_the_announcements() {
    let clock = fake_clock();
    let phone = raw().await;
    let pc = announcer(&clock, vec![phone.local_addr().unwrap()]);
    pc.announce(PORT, Some(KEY)).await.unwrap();
    let listening = pc.local_addr().unwrap();
    clock.advance(Duration::from_millis(100)).await;
    recv(&phone).await; // the first announcement of the burst

    // Revoking the last phone clears the key: the rest of the burst is cancelled, the port is
    // released, and a probe with the old key gets no answer.
    pc.announce(PORT, None).await.unwrap();
    assert_eq!(pc.local_addr(), None);
    clock.advance(Duration::from_secs(10)).await;
    expect_silence(&phone).await;
    phone.send_to(&probe(&KEY, 1), listening).await.unwrap();
    expect_silence(&phone).await;
    assert_eq!(pc.replies_sent(), 0);
    std::net::UdpSocket::bind(listening).expect("the port was released");

    // A key later brings it back.
    pc.announce(PORT, Some(KEY)).await.unwrap();
    assert!(pc.local_addr().is_some());
}

#[tokio::test]
async fn stop_releases_the_socket_for_good() {
    let clock = fake_clock();
    let phone = raw().await;
    let pc = announcer(&clock, vec![phone.local_addr().unwrap()]);
    pc.announce(PORT, Some(KEY)).await.unwrap();
    let listening = pc.local_addr().unwrap();

    pc.stop().await;
    assert_eq!(pc.local_addr(), None);
    clock.advance(Duration::from_secs(10)).await;
    expect_silence(&phone).await;
    std::net::UdpSocket::bind(listening).expect("the port was released");
    pc.stop().await; // idempotent

    let error = pc.announce(PORT, Some(KEY)).await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotConnected);
    assert_eq!(pc.local_addr(), None);
}

#[tokio::test]
async fn the_listener_is_exclusive() {
    let clock = fake_clock();
    let first = announcer(&clock, vec![]);
    first.announce(PORT, Some(KEY)).await.unwrap();
    let taken = first.local_addr().unwrap();

    let second = UdpBeaconAnnouncer::new(
        AnnouncerNet::Unicast {
            bind: taken,
            announce_to: vec![],
        },
        dyn_clock(&clock),
    );
    let error = second.announce(PORT, Some(KEY)).await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
}

// ---------------------------------------------------------------------------------------------
// The announcer, driven by a hand-made phone
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn only_a_valid_probe_for_the_current_key_is_answered_and_junk_costs_nothing() {
    let clock = fake_clock();
    let pc = announcer(&clock, vec![]);
    pc.announce(PORT, Some(KEY)).await.unwrap();
    let pc_addr = pc.local_addr().unwrap();
    let phone = raw().await;

    let valid = probe(&KEY, 1);
    let mut junk: Vec<Vec<u8>> = vec![
        vec![],
        vec![0],
        valid[..31].to_vec(),
        [valid.as_slice(), &[0]].concat(),
        vec![0; 32],
        vec![0xFF; 1400],
        probe(&OTHER_KEY, 2).to_vec(),
        announce(&KEY, PORT, 3).to_vec(), // a PC never answers an ANNOUNCE
    ];
    let mut tampered = valid;
    tampered[6..8].copy_from_slice(&7u16.to_be_bytes());
    junk.push(tampered.to_vec());
    for index in [0usize, 4, 5] {
        let mut bad = valid;
        bad[index] ^= 0xFF;
        junk.push(bad.to_vec());
    }
    for datagram in &junk {
        phone.send_to(datagram, pc_addr).await.unwrap();
    }
    settle().await;
    expect_silence(&phone).await;
    assert_eq!(pc.replies_sent(), 0);

    // The same source is still answered at once: none of that used up its reply slot.
    phone.send_to(&valid, pc_addr).await.unwrap();
    let (reply, from) = recv(&phone).await;
    assert_eq!(from, pc_addr);
    assert_announce(&reply, &KEY, PORT);
    assert_eq!(pc.replies_sent(), 1);
    expect_silence(&phone).await;
}

#[tokio::test]
async fn replies_are_limited_to_one_per_source_ip_every_two_seconds() {
    let clock = fake_clock();
    let pc = announcer(&clock, vec![]);
    pc.announce(PORT, Some(KEY)).await.unwrap();
    let pc_addr = pc.local_addr().unwrap();
    let phone = raw().await;

    phone.send_to(&probe(&KEY, 1), pc_addr).await.unwrap();
    let (first, _) = recv(&phone).await;

    // A burst of valid probes right behind it gets nothing.
    for nonce in 2..6 {
        phone.send_to(&probe(&KEY, nonce), pc_addr).await.unwrap();
    }
    expect_silence(&phone).await;

    clock.advance(Duration::from_millis(1999)).await;
    phone.send_to(&probe(&KEY, 7), pc_addr).await.unwrap();
    expect_silence(&phone).await;

    clock.advance(Duration::from_millis(1)).await; // exactly two seconds after the reply
    phone.send_to(&probe(&KEY, 8), pc_addr).await.unwrap();
    let (second, _) = recv(&phone).await;
    assert_eq!(pc.replies_sent(), 2);

    // Each reply carries its own nonce, so replies cannot be linked by their bytes.
    let a = assert_announce(&first, &KEY, PORT);
    let b = assert_announce(&second, &KEY, PORT);
    assert_ne!(a.nonce(), b.nonce());
    assert_ne!(first, second);

    // And the limit restarts from the second reply.
    phone.send_to(&probe(&KEY, 9), pc_addr).await.unwrap();
    expect_silence(&phone).await;
}

#[tokio::test]
async fn announcements_go_out_at_100_500_and_2000_ms() {
    let clock = fake_clock();
    let phone = raw().await;
    let pc = announcer(&clock, vec![phone.local_addr().unwrap()]);
    pc.announce(PORT, Some(KEY)).await.unwrap();
    expect_silence(&phone).await; // nothing is sent before the first offset

    clock.advance(Duration::from_millis(99)).await;
    expect_silence(&phone).await;
    clock.advance(Duration::from_millis(1)).await;
    let (first, _) = recv(&phone).await;

    clock.advance(Duration::from_millis(399)).await;
    expect_silence(&phone).await;
    clock.advance(Duration::from_millis(1)).await;
    let (second, _) = recv(&phone).await;

    clock.advance(Duration::from_millis(1499)).await;
    expect_silence(&phone).await;
    clock.advance(Duration::from_millis(1)).await;
    let (third, _) = recv(&phone).await;

    clock.advance(Duration::from_secs(60)).await;
    expect_silence(&phone).await; // no heartbeat

    let nonces: Vec<_> = [&first, &second, &third]
        .into_iter()
        .map(|datagram| assert_announce(datagram, &KEY, PORT).nonce())
        .collect();
    assert!(nonces[0] != nonces[1] && nonces[1] != nonces[2] && nonces[0] != nonces[2]);
}

#[tokio::test]
async fn a_new_key_replaces_the_old_one_and_the_burst_in_flight() {
    let (first_key, second_key) = ([0x31; 32], [0x32; 32]);
    let clock = fake_clock();
    let phone = raw().await;
    let pc = announcer(&clock, vec![phone.local_addr().unwrap()]);

    pc.announce(1111, Some(first_key)).await.unwrap();
    let listening = pc.local_addr().unwrap();
    clock.advance(Duration::from_millis(100)).await;
    let (before, _) = recv(&phone).await;
    assert_announce(&before, &first_key, 1111);

    // Rotation after a revocation: same sockets, new key and port, a fresh burst.
    pc.announce(2222, Some(second_key)).await.unwrap();
    assert_eq!(pc.local_addr(), Some(listening));
    clock.advance(Duration::from_secs(10)).await;
    let after = drain(&phone).await;
    assert_eq!(after.len(), 3, "exactly the new burst, none of the old one");
    for datagram in &after {
        assert_announce(datagram, &second_key, 2222);
        assert!(Beacon::decode(datagram, &first_key).is_err());
    }

    // Probes: the old key is dead, the new one is answered.
    phone
        .send_to(&probe(&first_key, 1), listening)
        .await
        .unwrap();
    expect_silence(&phone).await;
    phone
        .send_to(&probe(&second_key, 2), listening)
        .await
        .unwrap();
    let (reply, _) = recv(&phone).await;
    assert_announce(&reply, &second_key, 2222);
}

#[tokio::test]
async fn an_announcer_refuses_what_it_cannot_announce() {
    let clock = fake_clock();

    // Port 0 names nothing a phone could dial.
    let pc = announcer(&clock, vec![]);
    let error = pc.announce(0, Some(KEY)).await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(pc.local_addr(), None);
    pc.announce(0, None).await.unwrap(); // no key: the port does not matter

    // A group that is not multicast, or an interface that is not a unicast address.
    for net in [
        MulticastNet {
            group: Ipv4Addr::new(10, 0, 0, 1),
            port: 53319,
            interfaces: vec![LO],
            loopback: false,
        },
        MulticastNet {
            group: Ipv4Addr::new(224, 0, 0, 167),
            port: 53319,
            interfaces: vec![Ipv4Addr::UNSPECIFIED],
            loopback: false,
        },
    ] {
        let pc = UdpBeaconAnnouncer::new(AnnouncerNet::Multicast(net.clone()), dyn_clock(&clock));
        let error = pc.announce(PORT, Some(KEY)).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let phone = UdpPcLocator::new(LocatorNet::Multicast(net), dyn_clock(&clock));
        let error = phone.locate(KEY, Duration::from_secs(1)).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    // No eligible interface (for example only Public networks): quietly nothing to do.
    let pc = UdpBeaconAnnouncer::multicast(vec![], dyn_clock(&clock));
    pc.announce(PORT, Some(KEY)).await.unwrap();
    assert_eq!(pc.local_addr(), None);
    let phone = UdpPcLocator::multicast(vec![], dyn_clock(&clock));
    assert_eq!(
        phone.locate(KEY, Duration::from_secs(1)).await.unwrap(),
        vec![]
    );
}

// ---------------------------------------------------------------------------------------------
// The locator, driven by a hand-made PC
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_locator_keeps_only_valid_announces_and_returns_them_as_candidates() {
    let clock = fake_clock();
    let pc = raw().await;
    let phone = locator(&clock, vec![pc.local_addr().unwrap()]);
    let search = spawn_locate(&phone, KEY, Duration::from_secs(3));

    let (first_probe, phone_addr) = recv(&pc).await;
    let probe_beacon = Beacon::decode(&first_probe, &KEY).expect("a probe tagged with the key");
    assert_eq!(probe_beacon.kind(), BeaconKind::Probe);
    assert_eq!(probe_beacon.tcp_port(), 0);

    let mut tampered = announce(&KEY, 7002, 7);
    tampered[6..8].copy_from_slice(&7003u16.to_be_bytes());
    let replies: Vec<Vec<u8>> = vec![
        vec![],
        vec![1, 2, 3],
        vec![0; 32],
        vec![0xFF; 2000],
        announce(&OTHER_KEY, 7001, 1).to_vec(), // another PC's key
        probe(&KEY, 2).to_vec(),                // right key, wrong kind
        tampered.to_vec(),                      // a replay with a changed port
        announce(&KEY, 4242, 4).to_vec(),       // the real one
        announce(&KEY, 4242, 5).to_vec(),       // same address again: one candidate
        announce(&KEY, 4243, 6).to_vec(),       // same host, another port
    ];
    for reply in &replies {
        pc.send_to(reply, phone_addr).await.unwrap();
    }
    settle().await;
    clock.advance(Duration::from_secs(3)).await;
    assert_eq!(
        within(search).await.unwrap().unwrap(),
        vec![loopback(4242), loopback(4243)]
    );
}

#[tokio::test]
async fn the_locator_never_returns_more_than_the_candidate_cap() {
    let clock = fake_clock();
    let pc = raw().await;
    let phone = locator(&clock, vec![pc.local_addr().unwrap()]);
    let search = spawn_locate(&phone, KEY, Duration::from_secs(3));
    let (_, phone_addr) = recv(&pc).await;

    for port in 1000..1000 + 2 * MAX_CANDIDATES as u16 {
        pc.send_to(&announce(&KEY, port, 1), phone_addr)
            .await
            .unwrap();
    }
    settle().await;
    clock.advance(Duration::from_secs(3)).await;
    let found = within(search).await.unwrap().unwrap();
    let expected: Vec<SocketAddr> = (1000..1000 + MAX_CANDIDATES as u16).map(loopback).collect();
    assert_eq!(found, expected);
}

#[tokio::test]
async fn the_probe_burst_is_0_100_500_and_2000_ms_and_stops_at_the_timeout() {
    let clock = fake_clock();
    let pc = raw().await;
    let phone = locator(&clock, vec![pc.local_addr().unwrap()]);
    let search = spawn_locate(&phone, KEY, Duration::from_secs(1));

    let (first, _) = recv(&pc).await;
    clock.advance(Duration::from_millis(99)).await;
    expect_silence(&pc).await;
    clock.advance(Duration::from_millis(1)).await;
    let (second, _) = recv(&pc).await;
    clock.advance(Duration::from_millis(399)).await;
    expect_silence(&pc).await;
    clock.advance(Duration::from_millis(1)).await;
    let (third, _) = recv(&pc).await;

    // The 2000 ms probe falls after the 1 s timeout, so it is never sent.
    clock.advance(Duration::from_secs(30)).await;
    expect_silence(&pc).await;
    assert_eq!(within(search).await.unwrap().unwrap(), vec![]);

    let nonces: Vec<_> = [&first, &second, &third]
        .into_iter()
        .map(|datagram| {
            let beacon = Beacon::decode(datagram, &KEY).unwrap();
            assert_eq!(beacon.kind(), BeaconKind::Probe);
            beacon.nonce()
        })
        .collect();
    assert!(nonces[0] != nonces[1] && nonces[1] != nonces[2] && nonces[0] != nonces[2]);
}

#[tokio::test]
async fn a_cancelled_search_closes_its_sockets() {
    let clock = fake_clock();
    let pc = raw().await;
    let phone = locator(&clock, vec![pc.local_addr().unwrap()]);
    let search = spawn_locate(&phone, KEY, Duration::from_secs(60));
    let (_, phone_addr) = recv(&pc).await;

    search.abort();
    let _ = search.await;
    settle().await;
    // The probing socket is gone: its port can be taken again.
    std::net::UdpSocket::bind(phone_addr).expect("the locator released its socket");
}

// ---------------------------------------------------------------------------------------------
// Replays and sources (fixes for the automated review findings)
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_replayed_probe_is_never_answered_again() {
    let clock = fake_clock();
    let pc = announcer(&clock, vec![]);
    pc.announce(PORT, Some(KEY)).await.unwrap();
    let pc_addr = pc.local_addr().unwrap();
    let phone = raw().await;

    let captured = probe(&KEY, 1);
    phone.send_to(&captured, pc_addr).await.unwrap();
    let (reply, _) = recv(&phone).await;
    assert_announce(&reply, &KEY, PORT);

    // Well past the reply limit, the same probe again: a replay, so silence.
    clock.advance(Duration::from_secs(5)).await;
    phone.send_to(&captured, pc_addr).await.unwrap();
    expect_silence(&phone).await;
    assert_eq!(pc.replies_sent(), 1);

    // A fresh probe from the same source is answered: the replay did not spend its slot.
    phone.send_to(&probe(&KEY, 2), pc_addr).await.unwrap();
    let (reply, _) = recv(&phone).await;
    assert_announce(&reply, &KEY, PORT);

    // After the memory window a nonce may be used again (a phone's random 8 bytes never repeat
    // in practice; the window bounds the table).
    clock
        .advance(cooldown_sync::PROBE_NONCE_MEMORY + Duration::from_secs(1))
        .await;
    phone.send_to(&captured, pc_addr).await.unwrap();
    let (reply, _) = recv(&phone).await;
    assert_announce(&reply, &KEY, PORT);
}

#[tokio::test]
async fn probes_from_outside_the_allowed_subnets_are_ignored() {
    let clock = fake_clock();
    let elsewhere =
        announcer(&clock, vec![]).with_allowed_sources(vec![(Ipv4Addr::new(10, 0, 0, 0), 8)]);
    elsewhere.announce(PORT, Some(KEY)).await.unwrap();
    let phone = raw().await;
    phone
        .send_to(&probe(&KEY, 1), elsewhere.local_addr().unwrap())
        .await
        .unwrap();
    expect_silence(&phone).await;
    assert_eq!(elsewhere.replies_sent(), 0);

    let here = announcer(&clock, vec![]).with_allowed_sources(vec![(LO, 8)]);
    here.announce(PORT, Some(KEY)).await.unwrap();
    phone
        .send_to(&probe(&KEY, 2), here.local_addr().unwrap())
        .await
        .unwrap();
    let (reply, _) = recv(&phone).await;
    assert_announce(&reply, &KEY, PORT);
}

#[test]
fn subnet_matching() {
    use cooldown_sync::source_allowed;
    let ip = |a, b, c, d| IpAddr::V4(Ipv4Addr::new(a, b, c, d));
    let home = vec![(Ipv4Addr::new(192, 168, 1, 0), 24)];
    assert!(source_allowed(ip(192, 168, 1, 77), &home));
    assert!(!source_allowed(ip(192, 168, 2, 77), &home));
    assert!(!source_allowed(ip(10, 0, 0, 1), &home));
    assert!(!source_allowed(
        IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
        &home
    ));
    assert!(
        source_allowed(ip(8, 8, 8, 8), &[]),
        "empty means no restriction"
    );
    // The network address need not be canonical; /0 accepts everything IPv4.
    assert!(source_allowed(
        ip(172, 16, 9, 9),
        &[(Ipv4Addr::new(172, 16, 200, 1), 12)]
    ));
    assert!(source_allowed(
        ip(1, 2, 3, 4),
        &[(Ipv4Addr::UNSPECIFIED, 0)]
    ));
}
