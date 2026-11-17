//! Pairing: a client has a key that it generated. The host remembers the keys that the user
//! approved.
//!
//! The client keeps a 32-byte seed. Its public key goes to the host in the Hello. A client that the
//! host does not know shows a short code that comes from its public key. The user enters the code
//! at the host (`cranvr-host --pair CODE`). The host then remembers the public key.
//!
//! Each later connection proves that the client holds the key. The host sends a random challenge.
//! The client signs a fixed label and the challenge with its seed. The host checks the signature
//! with the public key that it remembers. A listener of the network sees the public key and the
//! code, but it cannot sign a challenge without the seed.
//!
//! The proof comes before the code check. A client that knows only another client's public key
//! gets no further than the proof.
//!
//! This protects against a device that wants a stream without the user's approval. The session
//! itself stays plain: the media and the tracking are not encrypted.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read};

/// The length of a seed, a public key and a challenge in bytes
pub const KEY_LEN: usize = 32;
/// The length of a signature in bytes
pub const SIGNATURE_LEN: usize = 64;
/// What the client signs in front of the challenge. It keeps the signature from serving another
/// purpose.
const LABEL: &[u8] = b"cranvr pairing challenge v1";
/// What the pairing code hashes in front of the public key
const CODE_LABEL: &[u8] = b"cranvr pairing code v1";
/// The pairing code has this many digits.
pub const CODE_DIGITS: u32 = 6;

/// The client's key.
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

    /// Answers the host's challenge.
    pub fn sign(&self, challenge: &[u8; KEY_LEN]) -> Vec<u8> {
        self.key.sign(&message(challenge)).to_bytes().to_vec()
    }
}

fn message(challenge: &[u8; KEY_LEN]) -> Vec<u8> {
    [LABEL, challenge].concat()
}

/// Whether `signature` answers `challenge` for `public_key`.
pub fn verify(public_key: &[u8; KEY_LEN], challenge: &[u8; KEY_LEN], signature: &[u8]) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(public_key) else { return false };
    let Ok(signature) = <[u8; SIGNATURE_LEN]>::try_from(signature) else { return false };
    key.verify(&message(challenge), &Signature::from_bytes(&signature)).is_ok()
}

/// The code of a public key, from 0 to 999999.
pub fn pairing_code(public_key: &[u8; KEY_LEN]) -> u32 {
    let hash = Sha256::new().chain_update(CODE_LABEL).chain_update(public_key).finalize();
    u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]]) % 10u32.pow(CODE_DIGITS)
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
    fn a_signature_proves_the_key_for_one_challenge() {
        let identity = Identity::from_seed([7; KEY_LEN]);
        let challenge = [1; KEY_LEN];
        let signature = identity.sign(&challenge);
        assert!(verify(&identity.public_key(), &challenge, &signature));
        // Another challenge, another key, a cut signature and a flipped bit fail
        assert!(!verify(&identity.public_key(), &[2; KEY_LEN], &signature));
        assert!(!verify(&Identity::from_seed([8; KEY_LEN]).public_key(), &challenge, &signature));
        assert!(!verify(&identity.public_key(), &challenge, &signature[..63]));
        let mut flipped = signature.clone();
        flipped[10] ^= 1;
        assert!(!verify(&identity.public_key(), &challenge, &flipped));
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
