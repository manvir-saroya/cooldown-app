#![no_main]
//! The 24-byte cleartext ClientHello.

use libfuzzer_sys::fuzz_target;
use cooldown_sync::hello::ClientHello;

fuzz_target!(|data: &[u8]| {
    if let Ok(hello) = ClientHello::decode(data) {
        assert_eq!(hello.encode().as_slice(), data, "non-canonical hello accepted");
        let _ = hello.negotiate();
    }
});
