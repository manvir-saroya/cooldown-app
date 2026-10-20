#![no_main]
//! A responder reading arbitrary bytes as message 1, in both modes. It must fail without
//! panicking (the PSK and the prologue make a successful read practically impossible).

use libfuzzer_sys::fuzz_target;
use cooldown_sync::hello::{ClientHello, Mode};
use cooldown_sync::noise::{prologue, Handshake, HandshakeParams};
use cooldown_sync::{DeviceId, StaticKeypair};
use std::sync::OnceLock;

fn server_key() -> &'static StaticKeypair {
    static KEY: OnceLock<StaticKeypair> = OnceLock::new();
    KEY.get_or_init(|| StaticKeypair::generate().expect("keygen"))
}

fuzz_target!(|data: &[u8]| {
    let key = server_key();
    for (mode, psk) in [(Mode::Pair, Some([7u8; 32])), (Mode::Reconnect, None)] {
        let hello = ClientHello {
            proto_min: 1,
            proto_max: 1,
            mode,
            pair_id: if mode == Mode::Pair { [1; 16] } else { [0; 16] },
        };
        let pro = prologue(&hello.encode(), &DeviceId::from_public(key.public()));
        let responder = Handshake::responder(&HandshakeParams {
            mode,
            local: key,
            remote_public: None,
            psk: psk.as_ref(),
            prologue: &pro,
        })
        .expect("builds");
        assert!(responder.read(data).is_err());
    }
});
