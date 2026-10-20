#![no_main]
//! Length-prefixed frame parser.

use libfuzzer_sys::fuzz_target;
use cooldown_sync::frame::decode_frame;

fuzz_target!(|data: &[u8]| {
    // The handshake cap, the transport cap, and an arbitrary one derived from the input.
    for max in [512usize, 65535, usize::from(data.first().copied().unwrap_or(1)).max(1)] {
        if let Ok(Some((body, used))) = decode_frame(data, max) {
            assert!(!body.is_empty() && body.len() <= max);
            assert_eq!(used, 2 + body.len());
        }
    }
});
