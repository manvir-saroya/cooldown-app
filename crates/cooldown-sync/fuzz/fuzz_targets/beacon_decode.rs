#![no_main]
//! Discovery beacons: arbitrary bytes under an arbitrary key must never panic, anything that
//! decodes must re-encode to exactly the same bytes, and a beacon built from the input must
//! round-trip and must stop decoding after any single-bit change.

use libfuzzer_sys::fuzz_target;
use cooldown_sync::beacon::Beacon;

fuzz_target!(|data: &[u8]| {
    if data.len() < 32 {
        return;
    }
    let (key, rest) = data.split_at(32);
    let key: [u8; 32] = key.try_into().expect("32 bytes");

    if let Ok(beacon) = Beacon::decode(rest, &key) {
        assert_eq!(
            beacon.encode(&key).as_slice(),
            rest,
            "non-canonical beacon accepted"
        );
    }

    // A random tag essentially never verifies, so also build a valid beacon from the input.
    if rest.len() >= 11 {
        let port = u16::from_be_bytes([rest[0], rest[1]]);
        let nonce: [u8; 8] = rest[2..10].try_into().expect("8 bytes");
        let beacon = if rest[10] & 1 == 0 {
            Beacon::probe(nonce)
        } else {
            match Beacon::announce(port, nonce) {
                Ok(beacon) => beacon,
                Err(_) => return,
            }
        };
        let datagram = beacon.encode(&key);
        assert_eq!(Beacon::decode(&datagram, &key), Ok(beacon));
        let bit = usize::from(rest[10] >> 1) % (datagram.len() * 8);
        let mut mutated = datagram;
        mutated[bit / 8] ^= 1 << (bit % 8);
        assert!(Beacon::decode(&mutated, &key).is_err(), "bit change accepted");
    }
});
