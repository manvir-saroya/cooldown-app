#![no_main]
//! QR parser: arbitrary text and arbitrary binary payloads must never panic, and anything that
//! decodes must re-encode to exactly the same text (one spelling per payload).

use libfuzzer_sys::fuzz_target;
use cooldown_sync::QrPayload;

fuzz_target!(|data: &[u8]| {
    let _ = QrPayload::decode_bytes(data);
    if let Ok(text) = std::str::from_utf8(data) {
        if let Ok(payload) = QrPayload::decode(text) {
            let again = payload.encode().expect("a decoded payload re-encodes");
            assert_eq!(again.as_str(), text, "non-canonical QR text accepted");
        }
    }
});
