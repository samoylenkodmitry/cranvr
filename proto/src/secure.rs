//! Secrecy and authenticity of a session.
//!
//! The handshake runs in three flights (see [`crate::link`]):
//! 1. The client sends a fresh X25519 key.
//! 2. The host answers with a fresh X25519 key, its Ed25519 key and its signature over the
//!    [`transcript_hash`].
//! 3. The client sends its Ed25519 key and its signature over the same transcript. By now both
//!    sides hold the shared secret, so this flight is already sealed.
//!
//! The transcript covers both preambles (so a downgrade of the version breaks the signatures),
//! both fresh keys and the host's key. A signature of the client for one session cannot serve
//! another session: the host's fresh key is part of what the client signs.
//!
//! [`derive`] splits the shared secret into four keys with HKDF-SHA256, one for each direction of
//! each path. The TCP path has one counter for each direction. It carries the control messages
//! and, with TCP media, the media and the datagrams as well. The UDP path carries the media and
//! the datagrams. A UDP packet has its own counter in front, so a receiver can drop a replayed
//! packet and tolerate loss and reordering (see [`ReplayWindow`]).
//!
//! A sealed packet is a counter (eight bytes, little-endian), the content encrypted with
//! ChaCha20-Poly1305 and a tag of 16 bytes. The nonce is four zero bytes and the counter. A key
//! never seals two packets with one counter: each direction has its own key and its own counter,
//! and both start at 0 with the session's fresh key. The packet's content is the kind byte and the
//! body. FEC runs before sealing, so each packet is sealed on its own and a lost packet costs
//! nothing else.

use crate::{
    identity::{self, KEY_LEN, SIGNATURE_LEN},
    wire::{Error, PacketBuf},
};
use chacha20poly1305::{AeadInOut, ChaCha20Poly1305, Key, KeyInit, Nonce, Tag};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use x25519_dalek::{x25519, X25519_BASEPOINT_BYTES};
use zeroize::Zeroizing;

/// The bytes of the counter in front of a sealed packet
pub const COUNTER_LEN: usize = 8;
/// The bytes of the tag behind a sealed packet
pub const TAG_LEN: usize = 16;
/// What sealing adds to a packet's content
pub const OVERHEAD: usize = COUNTER_LEN + TAG_LEN;

/// A key sealed this many packets in one direction. No session gets near it: at 100,000 packets a
/// second it would take a million years. The limit keeps the counter's arithmetic safe.
const MAX_COUNTER: u64 = 1 << 62;

/// What the host's signature starts with
const HOST_LABEL: &[u8] = b"cranvr host signature v3";
/// What the client's signature starts with. The two labels differ, so a signature of one side can
/// not stand for the other side.
const CLIENT_LABEL: &[u8] = b"cranvr client signature v3";
const TRANSCRIPT_LABEL: &[u8] = b"cranvr handshake transcript v3";

/// One key for one direction of one path.
pub struct Cipher(ChaCha20Poly1305);

impl Cipher {
    pub fn new(key: &[u8; KEY_LEN]) -> Self {
        Self(ChaCha20Poly1305::new(&Key::from(*key)))
    }

    fn nonce(counter: u64) -> Nonce {
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&counter.to_le_bytes());
        Nonce::from(nonce)
    }

    /// Encrypts `content` in place and returns the tag.
    pub(crate) fn seal_in_place(&self, counter: u64, content: &mut [u8]) -> Result<[u8; TAG_LEN], Error> {
        let tag = self.0.encrypt_inout_detached(&Self::nonce(counter), &[], content.into()).map_err(|_| Error::Crypto("seal"))?;
        Ok(tag.into())
    }

    /// Opens `packet` (the counter, the encrypted content, the tag) in place. It returns the
    /// counter and the content. The counter is not checked here.
    fn open_in_place<'a>(&self, packet: &'a mut [u8]) -> Result<(u64, &'a mut [u8]), Error> {
        if packet.len() < OVERHEAD {
            return Err(Error::Crypto("short packet"));
        }
        let (counter, rest) = packet.split_at_mut(COUNTER_LEN);
        let counter = u64::from_le_bytes(counter.try_into().expect("eight bytes"));
        let (content, tag) = rest.split_at_mut(rest.len() - TAG_LEN);
        let tag = Tag::from(<[u8; TAG_LEN]>::try_from(&*tag).expect("sixteen bytes"));
        self.0
            .decrypt_inout_detached(&Self::nonce(counter), &[], (&mut *content).into(), &tag)
            .map_err(|_| Error::Crypto("tag"))?;
        Ok((counter, content))
    }
}

/// How far behind the newest counter a UDP packet may still arrive. A key frame is a burst of a
/// few hundred packets. Threads of the sender take their counters and send in no fixed order.
const WINDOW: u64 = 4096;

/// The counters of the packets that a receiver took. A counter is new when it is ahead of all
/// others, or inside the last [`WINDOW`] counters and not taken yet. Everything else is a replay
/// or too old. The window moves only when [`ReplayWindow::mark`] runs, which the caller does after
/// the tag held, so a forged packet cannot move it.
pub struct ReplayWindow {
    /// One more than the highest counter that was taken
    next: u64,
    /// A bit for each counter of the window, at the counter modulo [`WINDOW`]
    seen: [u64; (WINDOW / 64) as usize],
}

impl Default for ReplayWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplayWindow {
    pub fn new() -> Self {
        Self { next: 0, seen: [0; (WINDOW / 64) as usize] }
    }

    fn slot(counter: u64) -> (usize, u64) {
        let slot = counter % WINDOW;
        ((slot / 64) as usize, 1 << (slot % 64))
    }

    pub fn accepts(&self, counter: u64) -> bool {
        if counter >= MAX_COUNTER {
            return false;
        }
        if counter >= self.next {
            return true;
        }
        if self.next - 1 - counter >= WINDOW {
            return false;
        }
        let (word, bit) = Self::slot(counter);
        self.seen[word] & bit == 0
    }

    /// Records a counter whose packet passed the tag check.
    pub fn mark(&mut self, counter: u64) {
        if counter >= self.next {
            // The slots of the new counters belonged to counters that fell out of the window
            let first = if counter - self.next >= WINDOW { counter + 1 - WINDOW } else { self.next };
            for fresh in first..=counter {
                let (word, bit) = Self::slot(fresh);
                self.seen[word] &= !bit;
            }
            self.next = counter + 1;
        }
        let (word, bit) = Self::slot(counter);
        self.seen[word] |= bit;
    }
}

/// The sending end of the UDP path. Any thread may seal: the counter comes from an atomic.
pub struct UdpTx {
    cipher: Cipher,
    next: AtomicU64,
}

impl UdpTx {
    /// Seals the packet in `buf` with the next counter. The caller sends [`PacketBuf::datagram`].
    pub fn seal(&self, buf: &mut PacketBuf) -> Result<(), Error> {
        let counter = self.next.fetch_add(1, Ordering::Relaxed);
        if counter >= MAX_COUNTER {
            return Err(Error::Crypto("the counter ran out"));
        }
        buf.seal(&self.cipher, counter)
    }
}

/// The receiving end of the UDP path.
pub struct UdpRx {
    cipher: Cipher,
    window: ReplayWindow,
}

impl UdpRx {
    /// Opens a datagram in place and returns its content (the kind byte and the body). It fails
    /// for a packet that was changed, that came from another key, that was replayed or that is
    /// too old.
    pub fn open<'a>(&mut self, datagram: &'a mut [u8]) -> Result<&'a mut [u8], Error> {
        let Some(head) = datagram.get(..COUNTER_LEN) else { return Err(Error::Crypto("short packet")) };
        let counter = u64::from_le_bytes(head.try_into().expect("eight bytes"));
        if !self.window.accepts(counter) {
            return Err(Error::Crypto("replay"));
        }
        let (counter, content) = self.cipher.open_in_place(datagram)?;
        self.window.mark(counter);
        Ok(content)
    }
}

/// The sending end of the TCP path. TCP delivers in order, so the counter counts up by one for
/// each record and the caller seals and writes under one lock.
pub struct TcpTx {
    cipher: Cipher,
    next: u64,
}

impl TcpTx {
    /// Seals the packet in `buf` and returns it with its length prefix, ready to write.
    pub fn seal<'a>(&mut self, buf: &'a mut PacketBuf) -> Result<&'a [u8], Error> {
        if self.next >= MAX_COUNTER {
            return Err(Error::Crypto("the counter ran out"));
        }
        buf.seal(&self.cipher, self.next)?;
        self.next += 1;
        Ok(buf.framed())
    }
}

/// The receiving end of the TCP path.
pub struct TcpRx {
    cipher: Cipher,
    next: u64,
}

impl TcpRx {
    /// Opens a record that [`crate::wire::read_tcp_packet`] read and returns its content. The
    /// record is opened in place. A record that is not the next one, or that was changed, ends the
    /// session.
    pub fn open<'a>(&mut self, record: &'a mut [u8]) -> Result<&'a mut [u8], Error> {
        let (counter, content) = self.cipher.open_in_place(record)?;
        if counter != self.next {
            return Err(Error::Crypto("a record is missing, repeated or out of order"));
        }
        self.next += 1;
        Ok(content)
    }
}

/// Everything that a side holds after the handshake. The counters live in these values, so the
/// packets that the handshake sealed after the key exchange (the Hello, the Welcome) and the packets of
/// the session never reuse a counter.
pub struct Channels {
    pub tcp_tx: TcpTx,
    pub tcp_rx: TcpRx,
    pub udp_tx: UdpTx,
    pub udp_rx: UdpRx,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Client,
    Host,
}

/// A fresh X25519 key for one handshake. Its secret is wiped when the value goes.
pub struct Ephemeral {
    secret: Zeroizing<[u8; KEY_LEN]>,
    public: [u8; KEY_LEN],
}

impl Ephemeral {
    pub fn new() -> Result<Self, Error> {
        let secret = Zeroizing::new(identity::random_bytes()?);
        let public = x25519(*secret, X25519_BASEPOINT_BYTES);
        Ok(Self { secret, public })
    }

    pub fn public(&self) -> [u8; KEY_LEN] {
        self.public
    }

    /// The secret that both sides share. A peer key of low order gives an all-zero secret for
    /// every secret key, so the exchange refuses it.
    pub fn agree(&self, peer: &[u8; KEY_LEN]) -> Result<Zeroizing<[u8; KEY_LEN]>, Error> {
        let shared = Zeroizing::new(x25519(*self.secret, *peer));
        if shared.iter().fold(0u8, |all, byte| all | byte) == 0 {
            return Err(Error::Crypto("the peer's key is a point of low order"));
        }
        Ok(shared)
    }
}

/// The hash that both sides sign. The preambles are the exact bytes of the version exchange.
pub fn transcript_hash(
    client_preamble: &[u8],
    host_preamble: &[u8],
    client_ephemeral: &[u8; KEY_LEN],
    host_ephemeral: &[u8; KEY_LEN],
    host_key: &[u8; KEY_LEN],
) -> [u8; 32] {
    Sha256::new()
        .chain_update(TRANSCRIPT_LABEL)
        .chain_update(client_preamble)
        .chain_update(host_preamble)
        .chain_update(client_ephemeral)
        .chain_update(host_ephemeral)
        .chain_update(host_key)
        .finalize()
        .into()
}

fn signed_message(role: Role, transcript: &[u8; 32], client_key: &[u8; KEY_LEN]) -> Vec<u8> {
    match role {
        Role::Host => [HOST_LABEL, transcript].concat(),
        Role::Client => [CLIENT_LABEL, transcript, client_key].concat(),
    }
}

/// The host's signature over the transcript
pub fn sign_as_host(identity: &identity::Identity, transcript: &[u8; 32]) -> [u8; SIGNATURE_LEN] {
    identity.sign_message(&signed_message(Role::Host, transcript, &[0; KEY_LEN]))
}

/// The client's signature over the transcript
pub fn sign_as_client(identity: &identity::Identity, transcript: &[u8; 32]) -> [u8; SIGNATURE_LEN] {
    identity.sign_message(&signed_message(Role::Client, transcript, &identity.public_key()))
}

pub fn verify_host(host_key: &[u8; KEY_LEN], transcript: &[u8; 32], signature: &[u8; SIGNATURE_LEN]) -> bool {
    identity::verify_message(host_key, &signed_message(Role::Host, transcript, &[0; KEY_LEN]), signature)
}

pub fn verify_client(client_key: &[u8; KEY_LEN], transcript: &[u8; 32], signature: &[u8; SIGNATURE_LEN]) -> bool {
    identity::verify_message(client_key, &signed_message(Role::Client, transcript, client_key), signature)
}

/// Splits the shared secret into the four keys of a session. `transcript` is the HKDF salt, so the
/// keys depend on everything that the signatures cover.
pub fn derive(role: Role, shared: &[u8; KEY_LEN], transcript: &[u8; 32]) -> Channels {
    let hkdf = Hkdf::<Sha256>::new(Some(transcript), shared);
    let key = |info: &[u8]| {
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        hkdf.expand(info, &mut *key).expect("32 bytes are a valid length");
        Cipher::new(&key)
    };
    let (tcp_up, tcp_down) = (key(b"cranvr v3 tcp client to host"), key(b"cranvr v3 tcp host to client"));
    let (udp_up, udp_down) = (key(b"cranvr v3 udp client to host"), key(b"cranvr v3 udp host to client"));
    let (tcp_send, tcp_receive, udp_send, udp_receive) = match role {
        Role::Client => (tcp_up, tcp_down, udp_up, udp_down),
        Role::Host => (tcp_down, tcp_up, udp_down, udp_up),
    };
    Channels {
        tcp_tx: TcpTx { cipher: tcp_send, next: 0 },
        tcp_rx: TcpRx { cipher: tcp_receive, next: 0 },
        udp_tx: UdpTx { cipher: udp_send, next: AtomicU64::new(0) },
        udp_rx: UdpRx { cipher: udp_receive, window: ReplayWindow::new() },
    }
}

/// What the three flights carry. A flight is a TCP packet that starts with the handshake kind.
pub const CLIENT_HELLO_LEN: usize = KEY_LEN;
pub const HOST_HELLO_LEN: usize = KEY_LEN + KEY_LEN + SIGNATURE_LEN;
pub const CLIENT_AUTH_LEN: usize = KEY_LEN + SIGNATURE_LEN;

/// The host's flight: its fresh key, its Ed25519 key and its signature.
pub struct HostHello {
    pub ephemeral: [u8; KEY_LEN],
    pub key: [u8; KEY_LEN],
    pub signature: [u8; SIGNATURE_LEN],
}

impl HostHello {
    pub fn to_bytes(&self) -> Vec<u8> {
        [&self.ephemeral[..], &self.key, &self.signature].concat()
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != HOST_HELLO_LEN {
            return Err(Error::Malformed("host hello length"));
        }
        let (ephemeral, rest) = bytes.split_at(KEY_LEN);
        let (key, signature) = rest.split_at(KEY_LEN);
        Ok(Self {
            ephemeral: ephemeral.try_into().expect("checked length"),
            key: key.try_into().expect("checked length"),
            signature: signature.try_into().expect("checked length"),
        })
    }
}

/// The client's third flight: its Ed25519 key and its signature.
pub struct ClientAuth {
    pub key: [u8; KEY_LEN],
    pub signature: [u8; SIGNATURE_LEN],
}

impl ClientAuth {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != CLIENT_AUTH_LEN {
            return Err(Error::Malformed("client auth length"));
        }
        let (key, signature) = bytes.split_at(KEY_LEN);
        Ok(Self { key: key.try_into().expect("checked length"), signature: signature.try_into().expect("checked length") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::KIND_DATAGRAM;

    fn pair() -> (Channels, Channels) {
        let shared = [7u8; 32];
        let transcript = [9u8; 32];
        (derive(Role::Client, &shared, &transcript), derive(Role::Host, &shared, &transcript))
    }

    fn sealed(tx: &UdpTx, text: &[u8]) -> Vec<u8> {
        let mut buf = PacketBuf::new();
        buf.begin(KIND_DATAGRAM).extend_from_slice(text);
        tx.seal(&mut buf).unwrap();
        buf.datagram().to_vec()
    }

    #[test]
    fn the_window_takes_each_counter_once_and_tolerates_reordering() {
        let mut window = ReplayWindow::new();
        for counter in [0, 1, 2, 5, 3, 4] {
            assert!(window.accepts(counter), "{counter}");
            window.mark(counter);
        }
        for counter in 0..=5 {
            assert!(!window.accepts(counter), "{counter} again");
        }
        assert!(window.accepts(6));
    }

    #[test]
    fn the_window_refuses_counters_behind_its_span() {
        let mut window = ReplayWindow::new();
        window.mark(10_000);
        assert!(!window.accepts(10_000));
        assert!(window.accepts(10_000 - WINDOW + 1));
        assert!(!window.accepts(10_000 - WINDOW));
        assert!(!window.accepts(5));
        // A counter that the jump skipped is new while it lies inside the span
        assert!(window.accepts(9_999));
        window.mark(9_999);
        assert!(!window.accepts(9_999));
        // The slot of an old counter does not make a new one look taken
        window.mark(10_000 + WINDOW - 1);
        assert!(window.accepts(10_000 + WINDOW - 2));
        window.mark(10_000 + WINDOW);
        assert!(window.accepts(10_000 + WINDOW - 3));
    }

    #[test]
    fn a_huge_counter_is_refused_before_it_can_overflow() {
        let window = ReplayWindow::new();
        assert!(!window.accepts(u64::MAX));
        assert!(!window.accepts(MAX_COUNTER));
        assert!(window.accepts(MAX_COUNTER - 1));
    }

    #[test]
    fn a_sealed_datagram_opens_with_the_other_side_and_only_once() {
        let (client, mut host) = pair();
        let mut packet = sealed(&client.udp_tx, b"tracking");
        assert_eq!(packet.len(), 1 + 8 + OVERHEAD);
        let copy = packet.clone();
        assert_eq!(host.udp_rx.open(&mut packet).unwrap(), &[KIND_DATAGRAM, b't', b'r', b'a', b'c', b'k', b'i', b'n', b'g'][..]);
        let mut again = copy;
        assert!(host.udp_rx.open(&mut again).is_err());
    }

    #[test]
    fn each_direction_has_its_own_key() {
        let (mut client, host) = pair();
        // A packet that the client sealed does not open with the client's own receiving key
        let mut packet = sealed(&client.udp_tx, b"x");
        assert!(client.udp_rx.open(&mut packet).is_err());
        let mut packet = sealed(&host.udp_tx, b"x");
        assert!(client.udp_rx.open(&mut packet).is_ok());
    }

    #[test]
    fn a_changed_packet_does_not_open_and_does_not_move_the_window() {
        let (client, mut host) = pair();
        let good = sealed(&client.udp_tx, b"abcdefgh");
        // One bit in the counter, in the content, in the tag
        for at in [0, COUNTER_LEN + 2, good.len() - 1] {
            let mut bad = good.clone();
            bad[at] ^= 1;
            assert!(host.udp_rx.open(&mut bad).is_err(), "bit in byte {at}");
        }
        // A forged packet with a far counter must not shut out the real ones
        let mut far = good.clone();
        far[..COUNTER_LEN].copy_from_slice(&(1u64 << 40).to_le_bytes());
        assert!(host.udp_rx.open(&mut far).is_err());
        let mut real = good;
        assert!(host.udp_rx.open(&mut real).is_ok());
    }

    #[test]
    fn a_short_packet_is_refused() {
        let (_, mut host) = pair();
        for len in 0..OVERHEAD {
            assert!(host.udp_rx.open(&mut vec![0u8; len]).is_err());
        }
    }

    #[test]
    fn tcp_records_must_arrive_in_order() {
        let (mut client, mut host) = pair();
        let mut record = |text: &[u8]| {
            let mut buf = PacketBuf::new();
            buf.begin(KIND_DATAGRAM).extend_from_slice(text);
            client.tcp_tx.seal(&mut buf).unwrap()[4..].to_vec()
        };
        let (first, second, third) = (record(b"1"), record(b"2"), record(b"3"));
        assert!(host.tcp_rx.open(&mut first.clone()).is_ok());
        // The second record is dropped on the way. The third is then the wrong one.
        assert!(host.tcp_rx.open(&mut third.clone()).is_err());
        assert!(host.tcp_rx.open(&mut first.clone()).is_err(), "a repeat");
        assert!(host.tcp_rx.open(&mut second.clone()).is_ok());
        assert!(host.tcp_rx.open(&mut third.clone()).is_ok());
    }

    #[test]
    fn a_weak_peer_key_is_refused() {
        let ours = Ephemeral::new().unwrap();
        assert!(ours.agree(&[0; 32]).is_err());
        let mut one = [0u8; 32];
        one[0] = 1;
        assert!(ours.agree(&one).is_err());
        let theirs = Ephemeral::new().unwrap();
        assert_eq!(*ours.agree(&theirs.public()).unwrap(), *theirs.agree(&ours.public()).unwrap());
    }

    #[test]
    fn the_signatures_of_the_two_roles_are_not_interchangeable() {
        let id = identity::Identity::from_seed([1; 32]);
        let transcript = [3u8; 32];
        let as_host = sign_as_host(&id, &transcript);
        let as_client = sign_as_client(&id, &transcript);
        assert!(verify_host(&id.public_key(), &transcript, &as_host));
        assert!(verify_client(&id.public_key(), &transcript, &as_client));
        assert!(!verify_client(&id.public_key(), &transcript, &as_host));
        assert!(!verify_host(&id.public_key(), &transcript, &as_client));
        assert!(!verify_host(&id.public_key(), &[4u8; 32], &as_host));
    }
}
