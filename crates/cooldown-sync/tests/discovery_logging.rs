//! "No logging of keys": run discovery (announcer, locator, junk, a failed send) under a capturing
//! `tracing` subscriber and check that no log line shows the beacon key, a tag, a nonce or a
//! datagram. This is its own test binary, with a single test, because a thread-scoped subscriber
//! can miss events from callsites that other concurrently running tests registered first.

mod common;

use std::fmt::Write as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{fake_clock, settle, within};
use cooldown_sync::beacon::Beacon;
use cooldown_sync::codec::{b64_encode, hex_encode};
use cooldown_sync::{
    AnnouncerNet, BeaconAnnouncer, LocatorNet, PcLocator, SyncClock, UdpBeaconAnnouncer,
    UdpPcLocator,
};
use tokio::net::UdpSocket;
use tracing::field::{Field, Visit};
use tracing::{span, Event, Metadata, Subscriber};

const KEY: [u8; 32] = [0x11; 32];
const OTHER_KEY: [u8; 32] = [0x22; 32];
const PORT: u16 = 53777;

#[derive(Clone, Default)]
struct Capture {
    lines: Arc<Mutex<Vec<String>>>,
}

struct Line(String);

impl Visit for Line {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, "{}={:?} ", field.name(), value);
    }
}

impl Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }
    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut line = Line(format!("{} ", event.metadata().target()));
        event.record(&mut line);
        self.lines.lock().unwrap().push(line.0);
    }
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

#[tokio::test]
async fn logs_never_show_keys_tags_nonces_or_datagrams() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());

    let clock = fake_clock();
    let clock_dyn: Arc<dyn SyncClock> = clock.clone();
    let phone = UdpSocket::bind(loopback(0)).await.unwrap();
    // One destination that cannot be sent to (an IPv6 address from an IPv4 socket), so the
    // failure path logs as well.
    let unreachable = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 9);
    let pc = UdpBeaconAnnouncer::new(
        AnnouncerNet::Unicast {
            bind: loopback(0),
            announce_to: vec![phone.local_addr().unwrap(), unreachable],
        },
        clock_dyn.clone(),
    );
    pc.announce(PORT, Some(KEY)).await.unwrap();
    let pc_addr = pc.local_addr().unwrap();

    // A valid probe (answered), a probe for another key, and junk.
    let valid = Beacon::probe([0xA7; 8]).encode(&KEY);
    phone.send_to(&valid, pc_addr).await.unwrap();
    phone
        .send_to(&Beacon::probe([0xB8; 8]).encode(&OTHER_KEY), pc_addr)
        .await
        .unwrap();
    phone.send_to(&[0xEE; 40], pc_addr).await.unwrap();
    let mut buf = [0u8; 64];
    let (len, _) = within(phone.recv_from(&mut buf)).await.unwrap();
    let reply = buf[..len].to_vec();

    // The announcement burst (including the failing destination), then a locator run.
    clock.advance(Duration::from_secs(3)).await;
    let locator = Arc::new(UdpPcLocator::new(
        LocatorNet::Unicast {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            probe_to: vec![pc_addr],
        },
        clock_dyn,
    ));
    let search = {
        let locator = Arc::clone(&locator);
        tokio::spawn(async move { locator.locate(KEY, Duration::from_secs(3)).await })
    };
    settle().await;
    clock.advance(Duration::from_secs(3)).await;
    within(search).await.unwrap().unwrap();
    pc.announce(PORT, None).await.unwrap();

    let log = capture.lines.lock().unwrap().join("\n");
    assert!(
        log.contains("beacon_listening"),
        "expected some log output:\n{log}"
    );
    assert!(
        log.contains("beacon_send_failed"),
        "the failure path ran:\n{log}"
    );
    let needles = [
        ("key hex", hex_encode(&KEY)),
        ("key base64", b64_encode(&KEY)),
        ("other key hex", hex_encode(&OTHER_KEY)),
        ("probe datagram hex", hex_encode(&valid)),
        ("probe tag hex", hex_encode(&valid[16..])),
        ("probe nonce hex", hex_encode(&valid[8..16])),
        ("reply datagram hex", hex_encode(&reply)),
        ("reply tag hex", hex_encode(&reply[16..])),
        ("reply nonce hex", hex_encode(&reply[8..16])),
        ("junk hex", hex_encode(&[0xEE; 8])),
    ];
    for (what, needle) in needles {
        assert!(!log.contains(&needle), "log leaks the {what}:\n{log}");
    }
}
