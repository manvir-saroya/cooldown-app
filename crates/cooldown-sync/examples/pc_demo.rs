//! A headless PC for testing a phone (the Android spike) without the desktop app.
//!
//!     cargo run -p cooldown-sync --example pc_demo -- <LAN IPv4> <file for the QR text> [advertise IPv4]
//!
//! The optional third address goes into the QR instead of the bound one: for an Android emulator,
//! bind 127.0.0.1 and advertise 10.0.2.2 (the emulator's alias for the host's loopback).
//!
//! It listens on the given address, opens a pairing window, writes the QR text (which holds the
//! one-time secret) to the file, **auto-confirms** the first phone (demo only: the real app asks
//! the user to compare the code), then pushes a sample payload with one window and one alert set
//! to ring two minutes later, and keeps the session open. In-memory identity and trust: nothing is
//! stored, nothing touches the keychain.

use std::net::IpAddr;
use std::sync::Arc;

use cooldown_core::model::{SnapshotSource, SourceKind, Unit, WindowKind};
use cooldown_core::{AlertIntent, Snapshot, SyncPayload, SCHEMA_VERSION};
use cooldown_sync::{
    MemoryTrustStore, PcServer, ServerConfig, ServerEvent, StaticKeypair, SystemSyncClock,
    DEFAULT_PORT,
};
use time::OffsetDateTime;

fn payload() -> SyncPayload {
    let now = OffsetDateTime::now_utc();
    let resets = now + time::Duration::minutes(2);
    let mut window = Snapshot::new(
        "claude",
        "default",
        WindowKind::Session5h,
        Unit::Percent,
        SnapshotSource {
            kind: SourceKind::Official,
            adapter: "claude-statusline".into(),
            adapter_version: "0.1.0".into(),
        },
        now,
    );
    window.percent_used = Some(100.0);
    window.resets_at = Some(resets);
    SyncPayload {
        schema_version: SCHEMA_VERSION,
        device_id: "pc".into(),
        generated_at: now,
        alerts: vec![AlertIntent {
            id: window.alert_id(),
            fire_at: resets,
            enabled: true,
        }],
        snapshots: vec![window],
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();
    let mut args = std::env::args().skip(1);
    let ip: IpAddr = args
        .next()
        .ok_or("usage: pc_demo <LAN IPv4> <qr file>")?
        .parse()?;
    let qr_file = args.next().ok_or("usage: pc_demo <LAN IPv4> <qr file>")?;
    let advertise: Vec<IpAddr> = args
        .next()
        .map(|a| a.parse())
        .transpose()?
        .into_iter()
        .collect();

    let server = PcServer::start(
        ServerConfig::new("Spike PC"),
        StaticKeypair::generate()?,
        Arc::new(MemoryTrustStore::new()),
        Arc::new(SystemSyncClock::new()),
        &[ip],
        DEFAULT_PORT,
    )
    .await?;
    let mut events = server.subscribe();
    let offer = server.open_pairing(&advertise)?;
    std::fs::write(&qr_file, offer.qr_text.as_bytes())?;
    println!(
        "listening on {ip}:{}; QR text written to {qr_file}",
        server.port()
    );

    loop {
        match events.recv().await? {
            ServerEvent::PairingConfirmationRequested { name, code, .. } => {
                println!("phone '{name}' shows code {code}: auto-confirming (demo only)");
                server.confirm_pairing(true)?;
            }
            ServerEvent::DevicePaired { name, .. } => println!("paired with '{name}'"),
            ServerEvent::DeviceConnected { device_id } => {
                let stamp = server.push_sync(&payload())?;
                println!("phone {device_id} connected; pushed sync rev {}", stamp.rev);
            }
            other => println!("event: {other:?}"),
        }
    }
}
