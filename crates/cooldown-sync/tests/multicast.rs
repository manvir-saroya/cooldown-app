//! A best-effort check of the real multicast path (group join, `IP_MULTICAST_IF`, TTL 1, the
//! production socket setup) on one host, with real time and the loopback flag on so a locator and
//! an announcer on the same machine can hear each other.
//!
//! Hosts differ in whether they deliver multicast on the loopback interface (the CI images and a
//! Windows runner may not), so when the sockets cannot be set up or nothing is delivered this test
//! prints `SKIPPED` and passes instead of failing. Where it does run, the assertions are strict.
//! Set `COOLDOWN_MULTICAST_IFACE` to a LAN interface address (for example `192.168.1.20`) to
//! exercise a real interface instead of 127.0.0.1; run with `--nocapture` to see which one ran.
//! The cross-device case (a phone and a PC on a Wi-Fi network) is not covered by any test here.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use cooldown_sync::beacon::BEACON_GROUP;
use cooldown_sync::{
    AnnouncerNet, BeaconAnnouncer, LocatorNet, MulticastNet, PcLocator, SystemSyncClock,
    UdpBeaconAnnouncer, UdpPcLocator,
};

const KEY: [u8; 32] = [0x44; 32];
const TCP_PORT: u16 = 53888;

fn interface() -> Ipv4Addr {
    std::env::var("COOLDOWN_MULTICAST_IFACE")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(Ipv4Addr::LOCALHOST)
}

fn free_udp_port() -> u16 {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
    socket.local_addr().unwrap().port()
}

fn skip(why: impl std::fmt::Display) {
    eprintln!("SKIPPED: multicast is not usable here: {why}");
}

#[tokio::test]
async fn multicast_probe_and_reply_work_on_this_host_when_it_allows_multicast() {
    let iface = interface();
    let net = MulticastNet {
        group: BEACON_GROUP,
        port: free_udp_port(),
        interfaces: vec![iface],
        loopback: true,
    };
    let clock = Arc::new(SystemSyncClock::new());

    let pc = UdpBeaconAnnouncer::new(AnnouncerNet::Multicast(net.clone()), clock.clone());
    if let Err(error) = pc.announce(TCP_PORT, Some(KEY)).await {
        return skip(format!("the announcer could not start on {iface}: {error}"));
    }
    assert_eq!(
        pc.local_addr().map(|addr| addr.port()),
        Some(net.port),
        "the listener binds the beacon port on every address"
    );

    // The listener is exclusive: a second one on the same port cannot start.
    let second = UdpBeaconAnnouncer::new(AnnouncerNet::Multicast(net.clone()), clock.clone());
    assert_eq!(
        second
            .announce(TCP_PORT, Some(KEY))
            .await
            .expect_err("a second listener on the same port")
            .kind(),
        std::io::ErrorKind::AddrInUse
    );

    let phone = UdpPcLocator::new(LocatorNet::Multicast(net), clock);
    let found = match phone.locate(KEY, Duration::from_millis(1500)).await {
        Ok(found) => found,
        Err(error) => return skip(format!("the locator could not probe on {iface}: {error}")),
    };
    if found.is_empty() {
        return skip(format!("no datagram was delivered on {iface}"));
    }
    eprintln!("multicast delivered on {iface}: {found:?}");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].port(), TCP_PORT);
    // The reply's source is an address of this host.
    assert!(matches!(found[0], SocketAddr::V4(_)));
    assert_ne!(found[0].ip(), IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    assert!(pc.replies_sent() >= 1);

    // A phone that holds a different key hears nothing.
    let other = UdpPcLocator::new(
        LocatorNet::Multicast(MulticastNet {
            group: BEACON_GROUP,
            port: pc.local_addr().unwrap().port(),
            interfaces: vec![iface],
            loopback: true,
        }),
        Arc::new(SystemSyncClock::new()),
    );
    assert_eq!(
        other
            .locate([0x55; 32], Duration::from_millis(600))
            .await
            .unwrap(),
        vec![]
    );
    pc.stop().await;
}
