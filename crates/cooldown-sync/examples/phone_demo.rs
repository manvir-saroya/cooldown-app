//! A headless phone for testing a PC (pairs with `pc_demo` or the desktop app).
//!
//!     cargo run -p cooldown-sync --example phone_demo -- <file with the QR text>
//!
//! In-memory identity and trust: nothing is stored.

use std::sync::Arc;
use std::time::Instant;

use cooldown_sync::{ClientConfig, MemoryTrustStore, PhoneClient, StaticKeypair, SystemSyncClock};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: phone_demo <qr file>")?;
    let qr = std::fs::read_to_string(path)?;
    let client = PhoneClient::new(
        ClientConfig::new("Demo phone", "android"),
        StaticKeypair::generate()?,
        Arc::new(MemoryTrustStore::new()),
        Arc::new(SystemSyncClock::new()),
    )?;
    let started = Instant::now();
    let pending = client.begin_pairing(qr.trim()).await?;
    println!(
        "handshake done in {:?}; code {} from '{}'",
        started.elapsed(),
        pending.code(),
        pending.pc_name()
    );
    let connection = pending.wait_for_confirmation().await?;
    println!("paired; waiting for a sync");
    let mut syncs = client.subscribe();
    syncs.changed().await?;
    let latest = client.latest_sync().ok_or("no sync")?;
    println!(
        "sync: {} snapshots, {} alerts",
        latest.payload.snapshots.len(),
        latest.payload.alerts.len()
    );
    drop(connection);
    Ok(())
}
