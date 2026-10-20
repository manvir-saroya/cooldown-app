#![no_main]
//! Decrypted envelope parser (`type | flags | body_len | body | padding`).

use libfuzzer_sys::fuzz_target;
use cooldown_sync::frame::{decode_envelope, encode_envelope, MAX_BODY_LEN};

fuzz_target!(|data: &[u8]| {
    if let Ok((msg_type, body)) = decode_envelope(data) {
        assert!(body.len() <= MAX_BODY_LEN);
        // A valid envelope is canonical up to padding length.
        let again = encode_envelope(msg_type, body).expect("re-encodes");
        let (t2, b2) = decode_envelope(&again).expect("decodes again");
        assert_eq!((t2, b2), (msg_type, body));
    }
});
