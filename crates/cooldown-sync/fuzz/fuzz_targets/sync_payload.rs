#![no_main]
//! The allowlist copy and scrubber on arbitrary JSON payloads, and the phone's incoming checks.

use libfuzzer_sys::fuzz_target;
use cooldown_core::SyncPayload;
use cooldown_sync::sanitize::{check_incoming, sanitize_payload};

fuzz_target!(|data: &[u8]| {
    if let Ok(payload) = serde_json::from_slice::<SyncPayload>(data) {
        let _ = check_incoming(&payload);
        if let Ok(clean) = sanitize_payload(&payload) {
            // Idempotent, and the result is acceptable to the receiving side.
            assert_eq!(sanitize_payload(&clean).as_ref(), Ok(&clean));
            assert!(check_incoming(&clean).is_ok());
        }
    }
});
