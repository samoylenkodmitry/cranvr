//! Keys: every client and every host has an Ed25519 key that it generated. A side that is paired
//! with another one remembers that side's public key.
//!
//! The client keeps a 32-byte seed. Its public key goes to the host in the handshake. A host
//! that does not know the key asks its user (protocol version 5): both screens show a code that
//! comes from the session (see [`crate::secure::approval_code`]), and the user allows the headset
//! on the Mac. The host then remembers the public key. A version 4 client shows a short code that
//! comes from its public key. The user enters it at the host (`cranvr-host --pair CODE`). The host
//! has a key of its own. A client remembers the key of the host that it paired with, and refuses a
//! host with another key.
//!
//! Each connection proves the keys. Both sides sign a hash of the handshake with their seeds (see
//! [`crate::secure`]). The hash covers the fresh keys of that connection, so a signature serves for
//! no other connection. A listener of the network sees the public keys and the code. It cannot
//! sign without a seed.
//!
//! The proof comes before the code check. A client that knows only another client's public key
//! gets no further than the proof.
//!
//! The first connection of a client to a host is trusted on first use: the client has no earlier
//! key of the host to compare with. It remembers the key once the host accepted it. Someone who
//! controls the network at that moment can pose as the host to the client. The host stays safe:
//! the attacker cannot sign for the client.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read};

/// The length of a seed and of a public key in bytes
pub const KEY_LEN: usize = 32;
/// The length of a signature in bytes
pub const SIGNATURE_LEN: usize = 64;
/// What the pairing code hashes in front of the public key
const CODE_LABEL: &[u8] = b"cranvr pairing code v1";
/// What the fingerprint hashes in front of the public key
const FINGERPRINT_LABEL: &[u8] = b"cranvr key fingerprint v1";
/// The bytes of a fingerprint
pub const FINGERPRINT_LEN: usize = 8;
/// The pairing code has this many digits.
pub const CODE_DIGITS: u32 = 6;

/// A key of a client or of a host.
#[derive(Clone)]
pub struct Identity {
    key: SigningKey,
}

impl Identity {
    pub fn from_seed(seed: [u8; KEY_LEN]) -> Self {
        Self { key: SigningKey::from_bytes(&seed) }
    }

    /// A new identity from the system's random numbers
    pub fn generate() -> std::io::Result<Self> {
        Ok(Self::from_seed(random_bytes()?))
    }

    pub fn seed(&self) -> [u8; KEY_LEN] {
        self.key.to_bytes()
    }

    pub fn public_key(&self) -> [u8; KEY_LEN] {
        self.key.verifying_key().to_bytes()
    }

    /// The code that the user enters at the host
    pub fn code(&self) -> u32 {
        pairing_code(&self.public_key())
    }

    /// Signs a message with the key. [`crate::secure`] builds the message from a label and the
    /// hash of the handshake.
    pub fn sign_message(&self, message: &[u8]) -> [u8; SIGNATURE_LEN] {
        self.key.sign(message).to_bytes()
    }
}

/// Whether `signature` is the signature of `message` by `public_key`.
pub fn verify_message(public_key: &[u8; KEY_LEN], message: &[u8], signature: &[u8; SIGNATURE_LEN]) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(public_key) else { return false };
    // The strict check also refuses a public key of small order, which signs for every message
    key.verify_strict(message, &Signature::from_bytes(signature)).is_ok()
}

/// The code of a public key, from 0 to 999999.
pub fn pairing_code(public_key: &[u8; KEY_LEN]) -> u32 {
    let hash = Sha256::new().chain_update(CODE_LABEL).chain_update(public_key).finalize();
    u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]]) % 10u32.pow(CODE_DIGITS)
}

/// The start of a key's SHA-256 hash. A host names itself with it in the discovery answer, and a
/// client that paired with a host finds the key again by it. It is no proof of the key: only the
/// handshake proves a key.
pub fn fingerprint(public_key: &[u8; KEY_LEN]) -> [u8; FINGERPRINT_LEN] {
    let hash = Sha256::new().chain_update(FINGERPRINT_LABEL).chain_update(public_key).finalize();
    hash[..FINGERPRINT_LEN].try_into().expect("a hash is longer than a fingerprint")
}

/// The code as the user reads and types it: six digits, with a space in the middle.
pub fn format_code(code: u32) -> String {
    let digits = format!("{code:0width$}", width = CODE_DIGITS as usize);
    let (first, second) = digits.split_at(digits.len() / 2);
    format!("{first} {second}")
}

/// Reads a code that the user typed. Spaces are allowed. It gives `None` for anything but six
/// digits.
pub fn parse_code(text: &str) -> Option<u32> {
    let digits: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    (digits.len() == CODE_DIGITS as usize && digits.chars().all(|c| c.is_ascii_digit())).then(|| digits.parse().ok()).flatten()
}

/// Random bytes from the system, for seeds and challenges. macOS, Linux and Android all have
/// `/dev/urandom`.
pub fn random_bytes() -> std::io::Result<[u8; KEY_LEN]> {
    let mut bytes = [0u8; KEY_LEN];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// A public key as hex, for the host's file of paired clients
pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(text: &str) -> Option<[u8; KEY_LEN]> {
    let text = text.trim();
    if text.len() != KEY_LEN * 2 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; KEY_LEN];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_proves_the_key_for_one_message() {
        let identity = Identity::from_seed([7; KEY_LEN]);
        let signature = identity.sign_message(b"message");
        assert!(verify_message(&identity.public_key(), b"message", &signature));
        // Another message, another key and a flipped bit fail
        assert!(!verify_message(&identity.public_key(), b"messagf", &signature));
        assert!(!verify_message(&Identity::from_seed([8; KEY_LEN]).public_key(), b"message", &signature));
        let mut flipped = signature;
        flipped[10] ^= 1;
        assert!(!verify_message(&identity.public_key(), b"message", &flipped));
    }

    #[test]
    fn the_code_comes_from_the_public_key_and_has_six_digits() {
        let a = Identity::from_seed([1; KEY_LEN]);
        assert_eq!(a.code(), Identity::from_seed([1; KEY_LEN]).code());
        assert!(a.code() < 1_000_000);
        assert_ne!(a.code(), Identity::from_seed([2; KEY_LEN]).code());
        assert_eq!(format_code(4829), "004 829");
        assert_eq!(parse_code("004 829"), Some(4829));
        assert_eq!(parse_code("004829"), Some(4829));
        assert_eq!(parse_code("12345"), None);
        assert_eq!(parse_code("12a456"), None);
        assert_eq!(parse_code("1234567"), None);
    }

    #[test]
    fn a_fingerprint_is_the_start_of_a_hash_of_the_key() {
        let (a, b) = (Identity::from_seed([1; KEY_LEN]).public_key(), Identity::from_seed([2; KEY_LEN]).public_key());
        assert_eq!(fingerprint(&a), fingerprint(&a));
        assert_ne!(fingerprint(&a), fingerprint(&b));
        assert_eq!(fingerprint(&a).len(), FINGERPRINT_LEN);
    }

    #[test]
    fn hex_round_trips() {
        let key = Identity::from_seed([9; KEY_LEN]).public_key();
        assert_eq!(from_hex(&to_hex(&key)), Some(key));
        assert_eq!(from_hex("zz"), None);
        assert_eq!(from_hex(&"g".repeat(64)), None);
    }

    #[test]
    fn two_random_seeds_differ() {
        assert_ne!(random_bytes().unwrap(), random_bytes().unwrap());
    }
}
