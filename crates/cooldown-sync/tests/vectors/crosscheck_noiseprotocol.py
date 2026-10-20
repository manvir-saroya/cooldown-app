# Independent check of the golden values pinned in src/noise.rs (module `vectors`).
#
# It reproduces the pairing handshake with a *different* Noise implementation (Python
# `noiseprotocol`, pure Python, ChaChaPoly/BLAKE2s/X25519 from `cryptography`) from the same fixed
# keys, ephemerals, PSK and prologue, and prints the prologue, both handshake messages, the
# handshake hash, the confirmation code and the QR text. They must equal the constants in
# src/noise.rs. To run it without installing anything globally:
#
#   python3 -m venv /tmp/nv && /tmp/nv/bin/pip install noiseprotocol && /tmp/nv/bin/python crosscheck_noiseprotocol.py
#
# Independent check of the Rust golden values with Python noiseprotocol (a separate implementation).
import binascii, hashlib
from noise.connection import NoiseConnection, Keypair
from noise.backends.default.keypairs import KeyPair25519

H = binascii.hexlify
def unhex(s): return binascii.unhexlify(s)

phone_priv = bytes([0x42])*32
pc_priv = bytes([0x41])*32
phone_e = bytes([0x43])*32
pc_e = bytes([0x44])*32
psk = bytes([0x45])*32
pair_id = bytes([0x46])*16

pc_kp = KeyPair25519.from_private_bytes(pc_priv)
phone_kp = KeyPair25519.from_private_bytes(phone_priv)
pc_pub = pc_kp.public_bytes
phone_pub = phone_kp.public_bytes
device_id = hashlib.blake2s(pc_pub).digest()[:16]
hello = b"CDWN" + bytes([1,1,1,0]) + pair_id
prologue = b"cooldown/noise/v1" + hello + device_id

def fixed(conn, priv):
    kp = KeyPair25519.from_private_bytes(priv)
    conn.noise_protocol.dh_fn.generate_keypair = lambda: kp

init = NoiseConnection.from_name(b"Noise_IKpsk1_25519_ChaChaPoly_BLAKE2s")
init.set_as_initiator()
init.set_psks(psk)
init.set_keypair_from_private_bytes(Keypair.STATIC, phone_priv)
init.set_keypair_from_public_bytes(Keypair.REMOTE_STATIC, pc_pub)
init.set_prologue(prologue)
fixed(init, phone_e)
init.start_handshake()

resp = NoiseConnection.from_name(b"Noise_IKpsk1_25519_ChaChaPoly_BLAKE2s")
resp.set_as_responder()
resp.set_psks(psk)
resp.set_keypair_from_private_bytes(Keypair.STATIC, pc_priv)
resp.set_prologue(prologue)
fixed(resp, pc_e)
resp.start_handshake()

m1 = init.write_message(b'{"name":"Pixel 8","platform":"android","ts":1700000000000}')
resp.read_message(m1)
m2 = resp.write_message(b'{"pc_name":"Test PC","proto":1,"server_time_ms":1700000000000}')
init.read_message(m2)
h = init.get_handshake_hash()
assert h == resp.get_handshake_hash()
sas = int.from_bytes(hashlib.blake2s(b"cooldown-sas-v1" + h).digest()[:8], "big") % 10**6

import base64
qr_bin = bytes([1]) + pc_pub + psk + pair_id + (53320).to_bytes(2,"big") + bytes([1, 4, 192,168,1,20])
qr = "CD1:" + base64.b32encode(qr_bin).decode().rstrip("=")

print("pc_pub   ", H(pc_pub).decode())
print("phone_pub", H(phone_pub).decode())
print("device_id", H(device_id).decode())
print("prologue ", H(prologue).decode())
print("msg1     ", H(m1).decode())
print("msg2     ", H(m2).decode())
print("h        ", H(h).decode())
print("sas      ", "%03d %03d" % (sas // 1000, sas % 1000))
print("qr       ", qr)
