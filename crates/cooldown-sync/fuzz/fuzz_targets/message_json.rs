#![no_main]
//! Every message body type, from arbitrary bytes.

use libfuzzer_sys::fuzz_target;
use cooldown_sync::message::*;

fuzz_target!(|data: &[u8]| {
    if let Ok(body) = serde_json::from_slice::<HelloBody>(data) {
        let _ = body.is_valid();
    }
    if let Ok(body) = serde_json::from_slice::<AlertSetBody>(data) {
        let _ = body.is_valid();
    }
    if let Ok(body) = serde_json::from_slice::<PairMsg1>(data) {
        let _ = body.is_valid();
    }
    if let Ok(body) = serde_json::from_slice::<Msg2>(data) {
        let _ = body.is_valid();
    }
    let _ = serde_json::from_slice::<SyncBody>(data);
    let _ = serde_json::from_slice::<AckBody>(data);
    let _ = serde_json::from_slice::<PingBody>(data);
    let _ = serde_json::from_slice::<RevokeBody>(data);
    let _ = serde_json::from_slice::<ErrorBody>(data);
    let _ = serde_json::from_slice::<ReconnectMsg1>(data);
    let _ = serde_json::from_slice::<EmptyBody>(data);
});
