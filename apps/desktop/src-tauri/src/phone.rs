//! The link to paired phones: the `cooldown-sync` PC server, started only when it is needed.
//!
//! The server does not run until the user opens "Pair a phone" or a phone is already paired, so a
//! fresh install never opens a listening port (and Windows Firewall never asks) for nothing.
//!
//! It listens only on private-range IPv4 addresses (10/8, 172.16/12, 192.168/16) and, on Windows,
//! only on networks Windows classifies as Private or Domain (protocol section 4.3), so a café
//! network marked Public is never listened on even when it uses a private range. When Windows
//! cannot say, nothing is listened on (fail closed).

use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::sync::{Arc, Mutex};

use cooldown_core::SyncPayload;
use cooldown_sync::message::valid_name;
use cooldown_sync::{
    load_or_create_pc_identity, AlertRequest, BeaconAnnouncer, DeviceId, FileTrustStore,
    IdentityOutcome, IdentityStore, PcServer, ServerConfig, ServerEvent, SyncClock, SyncStamp,
    SystemSyncClock, TrustStore, UdpBeaconAnnouncer, WindowClosed, DEFAULT_PORT,
};
use time::OffsetDateTime;
use tokio::sync::{broadcast, mpsc};

use crate::backend::lock;
use crate::settings::SyncResume;
use crate::views::{
    iso, paired_device, short_fingerprint, PairedDevice, PairingPayload, PairingState,
};

/// The QR payload holds at most this many addresses.
const MAX_ADVERTISED: usize = 4;

/// Private-range IPv4 addresses from `addrs`, sorted, without duplicates, at most four.
pub fn private_ipv4(addrs: impl IntoIterator<Item = IpAddr>) -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = addrs
        .into_iter()
        .filter(|ip| matches!(ip, IpAddr::V4(v4) if v4.is_private() && !v4.is_loopback()))
        .collect();
    out.sort();
    out.dedup();
    out.truncate(MAX_ADVERTISED);
    out
}

/// Where Windows' network profile list comes from. The full path, so a `powershell.exe` planted
/// earlier on PATH is never run.
#[cfg(windows)]
const POWERSHELL: &str = r"WindowsPowerShell\v1.0\powershell.exe";

/// IPv4 addresses of interfaces whose network category is not Public.
#[cfg(windows)]
const TRUSTED_NETWORK_SCRIPT: &str = "Get-NetConnectionProfile | Where-Object { $_.NetworkCategory -ne 'Public' } | ForEach-Object { Get-NetIPAddress -InterfaceIndex $_.InterfaceIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue } | ForEach-Object { $_.IPAddress }";

/// One IP address per line (PowerShell output); anything else is ignored.
pub fn parse_ip_lines(text: &str) -> Vec<IpAddr> {
    text.lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

/// Windows: addresses on Private or Domain networks. `None` when Windows could not be asked.
#[cfg(windows)]
fn trusted_network_ips() -> Option<Vec<IpAddr>> {
    use cooldown_adapters::codex_launcher::CREATE_NO_WINDOW;
    use std::os::windows::process::CommandExt;
    let system = std::env::var_os("SystemRoot")?;
    let exe = std::path::Path::new(&system)
        .join("System32")
        .join(POWERSHELL);
    let output = std::process::Command::new(exe)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            TRUSTED_NETWORK_SCRIPT,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| parse_ip_lines(&String::from_utf8_lossy(&output.stdout)))
}

/// True when the server is bound to exactly the allowed addresses: nothing missing, and nothing
/// extra (an address whose network turned Public must not keep listening).
pub fn same_addresses(bound: &[IpAddr], allowed: &[IpAddr]) -> bool {
    !allowed.is_empty()
        && bound.iter().all(|ip| allowed.contains(ip))
        && allowed.iter().all(|ip| bound.contains(ip))
}

/// Keep the candidates that are on a trusted network. `None` (unknown) keeps nothing.
pub fn on_trusted_networks(candidates: Vec<IpAddr>, trusted: Option<&[IpAddr]>) -> Vec<IpAddr> {
    match trusted {
        Some(trusted) => candidates
            .into_iter()
            .filter(|ip| trusted.contains(ip))
            .collect(),
        None => Vec::new(),
    }
}

/// This machine's addresses the phone server may listen on right now. Blocking on Windows (it asks
/// PowerShell for the network profiles, about a second).
pub fn lan_ips() -> Vec<IpAddr> {
    let interfaces = if_addrs::get_if_addrs().unwrap_or_default();
    let candidates = private_ipv4(
        interfaces
            .into_iter()
            .filter(|interface| !interface.is_loopback())
            .map(|interface| interface.ip()),
    );
    #[cfg(windows)]
    {
        let trusted = trusted_network_ips();
        on_trusted_networks(candidates, trusted.as_deref())
    }
    #[cfg(not(windows))]
    {
        candidates
    }
}

#[cfg(windows)]
const NO_NETWORK: &str = "This PC is not on a network Windows treats as private. In Windows Settings > Network & internet, set your home Wi-Fi to Private, then try again.";
#[cfg(not(windows))]
const NO_NETWORK: &str =
    "This PC is not on a private network (such as home Wi-Fi). Connect to one to pair or sync a phone.";

/// The subnets (`(address, prefix length)`) of `addresses`, from the machine's interfaces: the only
/// sources the discovery beacon answers. An address with no matching interface gets a /32, which
/// still lets nobody else in.
pub fn subnets_of(
    addresses: &[Ipv4Addr],
    interfaces: &[if_addrs::Interface],
) -> Vec<(Ipv4Addr, u8)> {
    addresses
        .iter()
        .map(|addr| {
            let prefix = interfaces
                .iter()
                .find_map(|i| match &i.addr {
                    if_addrs::IfAddr::V4(v4) if v4.ip == *addr => {
                        Some(u32::from(v4.netmask).count_ones() as u8)
                    }
                    _ => None,
                })
                .unwrap_or(32);
            (*addr, prefix)
        })
        .collect()
}

/// The PC name phones show: the computer name when it is a legal name, otherwise a fixed one.
pub fn pc_name_from(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
        .take(32)
        .collect();
    let cleaned = cleaned.trim().to_owned();
    if valid_name(&cleaned) {
        cleaned
    } else {
        "Cooldown PC".to_owned()
    }
}

fn pc_name() -> String {
    let raw = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default();
    pc_name_from(&raw)
}

/// Receivers handed to the caller when the server has just started, so it can run the loops that
/// react to them.
pub struct Started {
    /// Server events (pairing progress, connections).
    pub events: broadcast::Receiver<ServerEvent>,
    /// Alert switches flipped on a phone.
    pub alert_requests: Option<mpsc::Receiver<AlertRequest>>,
}

/// What the phone link asks of the machine it runs on: the addresses it may listen on right now
/// and the store that holds the PC's identity key. [`HostEnv::system`] is the real thing; tests
/// pass fakes, because a test must never reach the OS keychain or the real network.
#[derive(Clone)]
pub struct HostEnv {
    networks: Arc<dyn Fn() -> Vec<IpAddr> + Send + Sync>,
    identity: Arc<dyn Fn() -> Result<Box<dyn IdentityStore>, String> + Send + Sync>,
}

impl HostEnv {
    /// This machine: [`lan_ips`] and the OS credential store.
    pub fn system() -> Self {
        HostEnv {
            networks: Arc::new(lan_ips),
            identity: Arc::new(identity_store),
        }
    }

    /// A custom environment (tests). `networks` may block; it is run off the async runtime.
    #[cfg(test)]
    pub fn new(
        networks: impl Fn() -> Vec<IpAddr> + Send + Sync + 'static,
        identity: impl Fn() -> Result<Box<dyn IdentityStore>, String> + Send + Sync + 'static,
    ) -> Self {
        HostEnv {
            networks: Arc::new(networks),
            identity: Arc::new(identity),
        }
    }
}

/// The phone side of the desktop app.
pub struct PhoneLink {
    env: HostEnv,
    trust: Result<Arc<FileTrustStore>, String>,
    server: Mutex<Option<Arc<PcServer>>>,
    /// The discovery beacon for the running server, so paired phones find this PC after its
    /// address or port changed. One per server start (an announcer stays stopped once stopped).
    announcer: Mutex<Option<Arc<UdpBeaconAnnouncer>>>,
    starting: tokio::sync::Mutex<()>,
    state: Mutex<PairingState>,
}

impl PhoneLink {
    /// Tests only: [`with_env`](Self::with_env) on this machine.
    #[cfg(test)]
    pub fn new(data_dir: &Path) -> Self {
        Self::with_env(data_dir, HostEnv::system())
    }

    /// Open the trust store in `data_dir`. A trust store that cannot be opened disables pairing
    /// (and says why) instead of stopping the app. `env` supplies the machine's networks and
    /// identity store (the real ones, or fakes in tests).
    pub fn with_env(data_dir: &Path, env: HostEnv) -> Self {
        let trust = FileTrustStore::open(data_dir.join("trust.json"))
            .map(Arc::new)
            .map_err(|e| format!("Paired phones cannot be loaded: {e}"));
        PhoneLink {
            env,
            trust,
            server: Mutex::new(None),
            announcer: Mutex::new(None),
            starting: tokio::sync::Mutex::new(()),
            state: Mutex::new(PairingState::Idle),
        }
    }

    fn trust(&self) -> Result<&Arc<FileTrustStore>, String> {
        self.trust.as_ref().map_err(Clone::clone)
    }

    /// The running server, if any.
    pub fn server(&self) -> Option<Arc<PcServer>> {
        lock(&self.server).clone()
    }

    /// True when at least one phone is paired.
    pub fn has_peers(&self) -> bool {
        self.trust()
            .ok()
            .and_then(|t| t.list().ok())
            .is_some_and(|peers| !peers.is_empty())
    }

    /// Paired phones, newest first.
    pub fn devices(&self) -> Result<Vec<PairedDevice>, String> {
        let mut peers = self.trust()?.list().map_err(|e| e.to_string())?;
        peers.sort_by_key(|p| std::cmp::Reverse(p.paired_at_ms));
        Ok(peers.iter().map(paired_device).collect())
    }

    /// Start the server if it is not running (or restart it when the LAN addresses changed).
    /// Returns the receivers to watch when a new server was started.
    pub async fn ensure_server(
        &self,
        resume: Option<SyncResume>,
    ) -> Result<(Arc<PcServer>, Option<Started>), String> {
        let _guard = self.starting.lock().await;
        let networks = Arc::clone(&self.env.networks);
        let ips = tokio::task::spawn_blocking(move || networks())
            .await
            .unwrap_or_default();
        if let Some(server) = self.server() {
            let bound: Vec<IpAddr> = server.local_addrs().iter().map(|a| a.ip()).collect();
            if same_addresses(&bound, &ips) {
                return Ok((server, None));
            }
            // The network changed (or one became Public): stop listening on every old address.
            server.shutdown();
            *lock(&self.server) = None;
            let announcer = lock(&self.announcer).take();
            if let Some(announcer) = announcer {
                announcer.stop().await;
            }
        }
        if ips.is_empty() {
            return Err(NO_NETWORK.to_owned());
        }

        let trust = self.trust()?.clone();
        let vault = (self.env.identity)()?;
        // The keypair is secret and deliberately not Clone: load it once per start attempt.
        let load_identity = || -> Result<_, String> {
            let (identity, outcome) =
                load_or_create_pc_identity(&*vault, &*trust).map_err(|e| e.to_string())?;
            if let IdentityOutcome::Regenerated(reason) = outcome {
                log::warn!("the PC identity was replaced ({reason:?}); phones must pair again");
            }
            Ok(identity)
        };

        let mut config = ServerConfig::new(pc_name());
        config.resume = resume.map(|r| SyncStamp {
            epoch: r.epoch,
            rev: r.rev,
        });
        let clock: Arc<dyn SyncClock> = Arc::new(SystemSyncClock::new());
        let trust_dyn: Arc<dyn TrustStore> = trust.clone();
        let server = match PcServer::start(
            config.clone(),
            load_identity()?,
            trust_dyn.clone(),
            clock.clone(),
            &ips,
            DEFAULT_PORT,
        )
        .await
        {
            Ok(server) => server,
            // The usual port is taken (another copy, another app): let the OS pick one. The QR
            // carries the real port; already-paired phones find it through the discovery beacon.
            Err(_) => PcServer::start(config, load_identity()?, trust_dyn, clock, &ips, 0)
                .await
                .map_err(|e| format!("Could not start listening for phones: {e}"))?,
        };
        let server = Arc::new(server);
        let started = Started {
            events: server.subscribe(),
            alert_requests: server.take_alert_requests(),
        };
        *lock(&self.server) = Some(server.clone());

        let interfaces: Vec<Ipv4Addr> = ips
            .iter()
            .filter_map(|ip| match ip {
                IpAddr::V4(v4) => Some(*v4),
                IpAddr::V6(_) => None,
            })
            .collect();
        let subnets = subnets_of(&interfaces, &if_addrs::get_if_addrs().unwrap_or_default());
        let announcer = Arc::new(
            UdpBeaconAnnouncer::multicast(interfaces, Arc::new(SystemSyncClock::new()))
                .with_allowed_sources(subnets),
        );
        *lock(&self.announcer) = Some(announcer);
        self.reannounce().await;
        Ok((server, Some(started)))
    }

    /// Announce this PC (port and current beacon key) on the trusted networks. Call after a start,
    /// a pairing and a revocation, when the key may have changed. Without paired phones there is
    /// no key and nothing is sent.
    pub async fn reannounce(&self) {
        let server = self.server();
        let announcer = lock(&self.announcer).clone();
        let (Some(server), Some(announcer)) = (server, announcer) else {
            return;
        };
        let key = self
            .trust()
            .ok()
            .and_then(|trust| trust.beacon_key().ok().flatten());
        if let Err(e) = announcer.announce(server.port(), key).await {
            log::warn!("paired phones may not find this PC after a network change: {e}");
        }
    }

    /// Open a pairing window on `server` and describe it for the Pairing screen.
    pub fn open_pairing(&self, server: &PcServer) -> Result<PairingPayload, String> {
        server.close_pairing();
        // An empty list advertises exactly the addresses the server is bound to, which
        // `ensure_server` just checked against the current trusted networks.
        let offer = server.open_pairing(&[]).map_err(|e| e.to_string())?;
        let expires_at = OffsetDateTime::now_utc()
            + time::Duration::try_from(offer.expires_in).unwrap_or(time::Duration::minutes(2));
        let host = server
            .local_addrs()
            .first()
            .map(|a| a.ip().to_string())
            .unwrap_or_default();
        *lock(&self.state) = PairingState::Waiting;
        // The typed code is the QR text itself, so the phone's "Paste code instead" takes exactly
        // what the camera would have read. It is a secret until pairing ends: never log it.
        let qr_text = offer.qr_text.to_string();
        Ok(PairingPayload {
            manual_code: qr_text.clone(),
            uri: qr_text,
            host,
            port: server.port(),
            expires_at: iso(expires_at),
            fingerprint: short_fingerprint(&server.device_id().to_hex()),
            placeholder: false,
        })
    }

    /// Hide the QR code (and reject a pairing waiting for confirmation).
    pub fn cancel_pairing(&self) {
        if let Some(server) = self.server() {
            server.close_pairing();
        }
        *lock(&self.state) = PairingState::Idle;
    }

    /// The user's answer to "Is this code on your phone?".
    pub fn confirm_pairing(&self, accept: bool) -> Result<(), String> {
        let server = self.server().ok_or("No pairing is in progress.")?;
        server.confirm_pairing(accept).map_err(|e| e.to_string())
    }

    /// Where pairing stands.
    pub fn state(&self) -> PairingState {
        lock(&self.state).clone()
    }

    /// Unpair a phone: through the server when it runs (the phone is told), else in the store.
    pub fn revoke(&self, hex: &str) -> Result<(), String> {
        let id = DeviceId::from_hex(hex).ok_or("That is not a device id.")?;
        match self.server() {
            Some(server) => server.revoke(&id).map_err(|e| e.to_string()),
            None => self
                .trust()?
                .revoke(&id)
                .map(|_| ())
                .map_err(|e| e.to_string()),
        }
    }

    /// Send the latest state to every phone. Returns the stamp to persist, if one was pushed.
    pub fn push(&self, payload: &SyncPayload) -> Option<SyncStamp> {
        let server = self.server()?;
        match server.push_sync(payload) {
            Ok(stamp) => Some(stamp),
            Err(e) => {
                log::warn!("could not sync phones: {e}");
                None
            }
        }
    }

    /// Update the pairing state from a server event. Returns true when phones should get a fresh
    /// sync (a phone just paired or connected).
    pub fn on_event(&self, event: &ServerEvent) -> bool {
        let mut state = lock(&self.state);
        match event {
            ServerEvent::PairingWindowOpened => {
                *state = PairingState::Waiting;
                false
            }
            ServerEvent::PairingConfirmationRequested {
                name,
                platform,
                code,
            } => {
                *state = PairingState::Confirm {
                    name: name.clone(),
                    platform: platform.clone(),
                    code: code.clone(),
                };
                false
            }
            ServerEvent::DevicePaired { name, .. } => {
                *state = PairingState::Paired { name: name.clone() };
                true
            }
            ServerEvent::PairingFailed { reason } => {
                *state = PairingState::Failed {
                    reason: describe_failure(reason),
                };
                false
            }
            ServerEvent::PairingWindowClosed(closed) => {
                let waiting =
                    matches!(*state, PairingState::Waiting | PairingState::Confirm { .. });
                if waiting {
                    *state = match closed {
                        WindowClosed::Expired => PairingState::Failed {
                            reason: "The code expired before a phone connected. Make a new one.".into(),
                        },
                        WindowClosed::Burned => PairingState::Failed {
                            reason: "Too many failed attempts, so that code no longer works. Make a new one.".into(),
                        },
                        WindowClosed::Rejected => PairingState::Failed {
                            reason: "The phone was not paired.".into(),
                        },
                        WindowClosed::Hidden | WindowClosed::Completed => PairingState::Idle,
                    };
                }
                false
            }
            ServerEvent::DeviceConnected { .. } => true,
            _ => false,
        }
    }

    /// Stop the server (app exit).
    pub fn shutdown(&self) {
        if let Some(server) = lock(&self.server).take() {
            server.shutdown();
        }
        // Dropping the announcer closes its sockets.
        lock(&self.announcer).take();
    }
}

fn describe_failure(reason: &str) -> String {
    match reason {
        "rejected" => "You did not confirm the code, so the phone was not paired.".into(),
        "timeout" => {
            "No one confirmed the code within a minute, so the phone was not paired.".into()
        }
        "disconnected" => "The phone disconnected before pairing finished.".into(),
        other => format!("Pairing did not finish ({other})."),
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn identity_store() -> Result<Box<dyn cooldown_sync::IdentityStore>, String> {
    cooldown_sync::KeyringIdentityStore::native()
        .map(|store| Box::new(store) as Box<dyn cooldown_sync::IdentityStore>)
        .map_err(|e| format!("The system credential store is not available: {e}"))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn identity_store() -> Result<Box<dyn cooldown_sync::IdentityStore>, String> {
    Err("Phone pairing is supported on Windows (and macOS for development) only.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[test]
    fn only_private_ipv4_is_kept() {
        let picked = private_ipv4([
            v4(127, 0, 0, 1),
            v4(8, 8, 8, 8),
            v4(192, 168, 1, 20),
            v4(10, 0, 0, 5),
            v4(172, 16, 4, 2),
            v4(172, 32, 0, 1),
            v4(169, 254, 3, 3),
            v4(192, 168, 1, 20),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ]);
        assert_eq!(
            picked,
            vec![v4(10, 0, 0, 5), v4(172, 16, 4, 2), v4(192, 168, 1, 20)]
        );
    }

    #[test]
    fn at_most_four_addresses() {
        let many = (1..=9).map(|n| v4(10, 0, 0, n));
        assert_eq!(private_ipv4(many).len(), 4);
    }

    #[test]
    fn powershell_output_is_parsed_line_by_line() {
        let text = "192.168.1.20\r\n\r\n  10.0.0.5  \r\nWARNING: something\r\n";
        assert_eq!(
            parse_ip_lines(text),
            vec![v4(192, 168, 1, 20), v4(10, 0, 0, 5)]
        );
        assert!(parse_ip_lines("").is_empty());
    }

    #[test]
    fn only_trusted_networks_are_kept_and_unknown_means_none() {
        let candidates = vec![v4(192, 168, 1, 20), v4(10, 0, 0, 5)];
        assert_eq!(
            on_trusted_networks(candidates.clone(), Some(&[v4(10, 0, 0, 5)])),
            vec![v4(10, 0, 0, 5)]
        );
        assert!(
            on_trusted_networks(candidates.clone(), Some(&[])).is_empty(),
            "all Public"
        );
        assert!(
            on_trusted_networks(candidates, None).is_empty(),
            "fail closed"
        );
    }

    #[test]
    fn a_bound_address_that_is_no_longer_allowed_forces_a_restart() {
        let home = v4(192, 168, 1, 20);
        let cafe = v4(10, 0, 0, 5);
        assert!(same_addresses(&[home], &[home]));
        assert!(same_addresses(&[cafe, home], &[home, cafe]));
        assert!(
            !same_addresses(&[home, cafe], &[home]),
            "cafe turned Public"
        );
        assert!(
            !same_addresses(&[home], &[home, cafe]),
            "a new trusted network"
        );
        assert!(!same_addresses(&[home], &[]), "nothing trusted any more");
        assert!(!same_addresses(&[], &[]));
    }

    #[test]
    fn subnets_come_from_the_interface_netmasks() {
        let iface = |ip: [u8; 4], mask: [u8; 4]| if_addrs::Interface {
            name: "en0".into(),
            addr: if_addrs::IfAddr::V4(if_addrs::Ifv4Addr {
                ip: Ipv4Addr::from(ip),
                netmask: Ipv4Addr::from(mask),
                prefixlen: 0,
                broadcast: None,
            }),
            index: None,
            oper_status: if_addrs::IfOperStatus::Up,
            is_p2p: false,
            #[cfg(windows)]
            adapter_name: String::new(),
        };
        let ifaces = vec![
            iface([192, 168, 1, 20], [255, 255, 255, 0]),
            iface([10, 4, 0, 9], [255, 255, 0, 0]),
        ];
        let got = subnets_of(
            &[
                Ipv4Addr::new(192, 168, 1, 20),
                Ipv4Addr::new(10, 4, 0, 9),
                Ipv4Addr::new(172, 16, 0, 1),
            ],
            &ifaces,
        );
        assert_eq!(
            got,
            vec![
                (Ipv4Addr::new(192, 168, 1, 20), 24),
                (Ipv4Addr::new(10, 4, 0, 9), 16),
                (Ipv4Addr::new(172, 16, 0, 1), 32),
            ]
        );
    }

    #[test]
    fn pc_names_are_cleaned() {
        assert_eq!(pc_name_from("DESKTOP-7Q2"), "DESKTOP-7Q2");
        assert_eq!(pc_name_from(""), "Cooldown PC");
        assert_eq!(pc_name_from("\u{202e}evil"), "evil");
        assert_eq!(pc_name_from(&"x".repeat(80)).chars().count(), 32);
    }

    fn link() -> (tempfile::TempDir, PhoneLink) {
        let dir = tempfile::tempdir().unwrap();
        let link = PhoneLink::new(dir.path());
        (dir, link)
    }

    #[test]
    fn pairing_state_follows_server_events() {
        let (_dir, link) = link();
        assert_eq!(link.state(), PairingState::Idle);
        assert!(!link.has_peers());
        assert!(link.devices().unwrap().is_empty());

        assert!(!link.on_event(&ServerEvent::PairingWindowOpened));
        assert_eq!(link.state(), PairingState::Waiting);

        link.on_event(&ServerEvent::PairingConfirmationRequested {
            name: "Pixel".into(),
            platform: "android".into(),
            code: "123 456".into(),
        });
        assert!(
            matches!(link.state(), PairingState::Confirm { ref code, .. } if code == "123 456")
        );

        let push = link.on_event(&ServerEvent::DevicePaired {
            device_id: DeviceId::from_hex("00112233445566778899aabbccddeeff").unwrap(),
            name: "Pixel".into(),
        });
        assert!(push, "a new phone gets the current state");
        assert_eq!(
            link.state(),
            PairingState::Paired {
                name: "Pixel".into()
            }
        );

        // A late "window closed (completed)" does not wipe the success message.
        link.on_event(&ServerEvent::PairingWindowClosed(WindowClosed::Completed));
        assert_eq!(
            link.state(),
            PairingState::Paired {
                name: "Pixel".into()
            }
        );
    }

    #[test]
    fn expiry_and_failures_explain_themselves() {
        let (_dir, link) = link();
        link.on_event(&ServerEvent::PairingWindowOpened);
        link.on_event(&ServerEvent::PairingWindowClosed(WindowClosed::Expired));
        assert!(
            matches!(link.state(), PairingState::Failed { ref reason } if reason.contains("expired"))
        );

        link.on_event(&ServerEvent::PairingFailed { reason: "timeout" });
        assert!(
            matches!(link.state(), PairingState::Failed { ref reason } if reason.contains("minute"))
        );

        link.on_event(&ServerEvent::PairingWindowOpened);
        link.on_event(&ServerEvent::PairingWindowClosed(WindowClosed::Hidden));
        assert_eq!(link.state(), PairingState::Idle);
    }

    #[test]
    fn revoking_without_a_server_uses_the_store() {
        let (_dir, link) = link();
        assert!(link.revoke("not hex").is_err());
        link.revoke("00112233445566778899aabbccddeeff").unwrap();
        assert!(
            link.push(&SyncPayload {
                schema_version: cooldown_core::SCHEMA_VERSION,
                device_id: "pc".into(),
                generated_at: OffsetDateTime::now_utc(),
                snapshots: vec![],
                alerts: vec![],
            })
            .is_none(),
            "no server, nothing pushed"
        );
    }
}
