//! End to end over real 127.0.0.1 sockets with fake clocks: pair, sync, reconnect, revoke.

mod common;

use common::*;
use cooldown_sync::server::ServerEvent;
use cooldown_sync::{ClientError, CloseReason, TrustStore};

#[tokio::test]
async fn pair_then_reconnect_then_revoke() {
    let clock = fake_clock();
    let pc = start_pc(&clock, |_| {}).await;
    let phone = phone(&clock, "Pixel 8");
    let mut events = pc.server.subscribe();

    // Pair.
    let connection = pair(&pc, &phone, &mut events).await;
    let pc_id = connection.pc_device_id();
    assert_eq!(pc_id, pc.server.device_id());
    assert_eq!(pc.server.peers().unwrap().len(), 1);
    assert_eq!(phone.client.paired_pcs().unwrap().len(), 1);
    next_event(&mut events, |e| {
        matches!(e, ServerEvent::DeviceConnected { .. }).then_some(())
    })
    .await;

    // The PC pushes; the phone exposes the latest payload.
    let stamp = pc.server.push_sync(&sample_payload(12.0)).unwrap();
    eventually(|| phone.client.latest_sync().is_some()).await;
    let got = phone.client.latest_sync().unwrap();
    assert_eq!((got.epoch, got.rev), (stamp.epoch, stamp.rev));
    assert_eq!(got.payload.snapshots[0].percent_used, Some(12.0));
    assert_eq!(got.payload.device_id, pc_id.to_hex());

    // A newer push replaces it.
    pc.server.push_sync(&sample_payload(55.0)).unwrap();
    eventually(|| {
        phone
            .client
            .latest_sync()
            .is_some_and(|s| s.payload.snapshots[0].percent_used == Some(55.0))
    })
    .await;

    // Drop the connection, then reconnect with Noise_IK.
    connection.close();
    assert_eq!(within(connection.closed()).await, CloseReason::Local);
    next_event(&mut events, |e| {
        matches!(e, ServerEvent::DeviceDisconnected { .. }).then_some(())
    })
    .await;
    let again = within(phone.client.connect(&pc_id, &[pc.addr()]))
        .await
        .unwrap();
    within(again.wait_ready()).await.unwrap();
    assert!(!again.clock_skew_warning());
    next_event(&mut events, |e| {
        matches!(e, ServerEvent::DeviceConnected { .. }).then_some(())
    })
    .await;
    // The phone said what it already holds, so the PC did not resend it: state is still the
    // 55 % revision and a fresh push still arrives.
    pc.server.push_sync(&sample_payload(70.0)).unwrap();
    eventually(|| {
        phone
            .client
            .latest_sync()
            .is_some_and(|s| s.payload.snapshots[0].percent_used == Some(70.0))
    })
    .await;

    // Phone toggles an alert; the PC is told exactly once.
    let mut alerts = pc.server.take_alert_requests().unwrap();
    again
        .set_alert("reset:claude:work:session_5h", false)
        .unwrap();
    let request = within(alerts.recv()).await.unwrap();
    assert_eq!(request.id, "reset:claude:work:session_5h");
    assert!(!request.enabled);
    assert_eq!(request.device_id, phone.id);

    // Revoke: the phone is told, deletes the PC, and cannot reconnect.
    pc.server.revoke(&phone.id).unwrap();
    assert_eq!(within(again.closed()).await, CloseReason::Revoked);
    assert!(phone.client.paired_pcs().unwrap().is_empty());
    assert!(pc.server.peers().unwrap().is_empty());
    assert!(pc.trust.beacon_key().unwrap().is_none());
    // The record is gone on the phone, so reconnecting needs a new pairing.
    assert!(matches!(
        phone.client.connect(&pc_id, &[pc.addr()]).await,
        Err(ClientError::UnknownPc)
    ));
}

#[tokio::test]
async fn a_restarted_pc_continues_or_restarts_the_revision_sequence_safely() {
    use cooldown_sync::server::SyncStamp;
    let clock = fake_clock();
    // Resuming continues the persisted sequence...
    let pc = start_pc_with(&clock, |c| {
        c.resume = Some(SyncStamp { epoch: 77, rev: 10 });
    })
    .await;
    assert_eq!(pc.server.sync_stamp(), SyncStamp { epoch: 77, rev: 10 });
    let stamp = pc.server.push_sync(&sample_payload(1.0)).unwrap();
    assert_eq!(stamp, SyncStamp { epoch: 77, rev: 11 });
    // ...and the default is a fresh random epoch at revision 0, so a phone holding newer
    // revisions of an older epoch still applies what the PC sends.
    let a = start_pc(&clock, |_| {}).await;
    let b = start_pc(&clock, |_| {}).await;
    assert_ne!(a.server.sync_stamp().epoch, b.server.sync_stamp().epoch);
    assert_eq!(a.server.sync_stamp().rev, 0);
}
