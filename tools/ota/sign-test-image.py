#!/usr/bin/env python3
"""Minisign-compatible signer for OTA bench testing.

TEST KEYS ONLY. Keys made here exist so a bench Hopspot built with
HOPSPOT_OTA_PUBKEY can verify uploads end to end. They carry no custody, they
must never sign a release, and the real chain stays with minisign and
release/keys/minisign.pub. Nothing here reads or writes that key.

The output is a standard prehashed minisign signature: Ed25519 over the
BLAKE2b-512 digest of the file, plus the global signature over
signature || trusted comment, so `minisign -Vm <image> -p ota-test.pub`
agrees with the on-device verifier.

Requires PyNaCl (pip install pynacl).

Commands:
  keygen <keydir>          create ota-test.key and ota-test.pub in <keydir>,
                           print the HOPSPOT_OTA_PUBKEY value
  sign <keyfile> <image>   write <image>.minisig next to the image
  pubkey-env <keyfile>     print the HOPSPOT_OTA_PUBKEY value for a key
"""

import base64
import hashlib
import os
import sys
import time

from nacl.signing import SigningKey

KEY_FILE = "ota-test.key"
PUB_FILE = "ota-test.pub"
ED25519_ALGORITHM = b"Ed"
PREHASHED_ALGORITHM = b"ED"


def load_key(path):
    with open(path, "r", encoding="ascii") as handle:
        seed_hex, key_id_hex = handle.read().split()
    return SigningKey(bytes.fromhex(seed_hex)), bytes.fromhex(key_id_hex)


def pubkey_base64(signing_key, key_id):
    raw = ED25519_ALGORITHM + key_id + signing_key.verify_key.encode()
    return base64.b64encode(raw).decode("ascii")


def display_key_id(key_id):
    return key_id[::-1].hex().upper()


def keygen(keydir):
    os.makedirs(keydir, exist_ok=True)
    key_path = os.path.join(keydir, KEY_FILE)
    if os.path.exists(key_path):
        sys.exit(f"refusing to overwrite {key_path}")
    signing_key = SigningKey(os.urandom(32))
    key_id = os.urandom(8)
    with open(key_path, "w", encoding="ascii") as handle:
        handle.write(f"{signing_key.encode().hex()}\n{key_id.hex()}\n")
    encoded = pubkey_base64(signing_key, key_id)
    pub_path = os.path.join(keydir, PUB_FILE)
    with open(pub_path, "w", encoding="ascii") as handle:
        handle.write(
            f"untrusted comment: minisign public key {display_key_id(key_id)} (PRNS OTA TEST ONLY)\n"
            f"{encoded}\n"
        )
    print(f"wrote {key_path} and {pub_path}")
    print(f"HOPSPOT_OTA_PUBKEY={encoded}")


def sign(keyfile, image_path):
    signing_key, key_id = load_key(keyfile)
    with open(image_path, "rb") as handle:
        digest = hashlib.blake2b(handle.read(), digest_size=64).digest()
    signature = signing_key.sign(digest).signature
    trusted_comment = (
        f"timestamp:{int(time.time())}\tfile:{os.path.basename(image_path)}\tprehashed"
    )
    global_signature = signing_key.sign(
        signature + trusted_comment.encode("ascii")
    ).signature
    document = (
        "untrusted comment: signature from PRNS OTA TEST key\n"
        f"{base64.b64encode(PREHASHED_ALGORITHM + key_id + signature).decode('ascii')}\n"
        f"trusted comment: {trusted_comment}\n"
        f"{base64.b64encode(global_signature).decode('ascii')}\n"
    )
    signature_path = f"{image_path}.minisig"
    with open(signature_path, "w", encoding="ascii", newline="\n") as handle:
        handle.write(document)
    print(f"wrote {signature_path}")


def pubkey_env(keyfile):
    signing_key, key_id = load_key(keyfile)
    print(f"HOPSPOT_OTA_PUBKEY={pubkey_base64(signing_key, key_id)}")


def main():
    if len(sys.argv) >= 3 and sys.argv[1] == "keygen":
        keygen(sys.argv[2])
    elif len(sys.argv) >= 4 and sys.argv[1] == "sign":
        sign(sys.argv[2], sys.argv[3])
    elif len(sys.argv) >= 3 and sys.argv[1] == "pubkey-env":
        pubkey_env(sys.argv[2])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
