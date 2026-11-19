//! Finding a host on the local network, so the user types no address.
//!
//! The client sends a [`Query`] by UDP broadcast. Each host that runs the native protocol answers
//! the sender with an [`Answer`]: its name, its TCP port, the start of its key's fingerprint and
//! the protocol versions that it speaks. The client lists the answers. The user presses one, and
//! the client connects to the address that the answer came from, at the port in it.
//!
//! The answer proves nothing. Anyone on the network can send one. The handshake proves the host's
//! key, and the approval of a new headset on the Mac (see [`crate::secure::approval_code`]) makes
//! the user check that the headset talks to the Mac that it means.
//!
//! The bytes are fixed, not postcard, and they stay readable across protocol versions. A client
//! needs the host's versions to say which side to update before it connects. A receiver ignores
//! bytes after the fields that it knows. A new kind of message gets a new kind byte.
//!
//! A query is longer than any answer, so a host cannot serve to amplify a spoofed query. The client
//! listens for answers to the queries that it sent, never for broadcasts, so it needs neither a
//! multicast lock on Android nor a permission beyond the network. Several hosts on one Mac share
//! the port ([`Responder::bind`] sets `SO_REUSEPORT`), so each of them hears a broadcast.

use crate::{
    identity::{random_bytes, FINGERPRINT_LEN},
    message::VersionRange,
};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// The UDP port on which hosts listen for queries. It is the host's default TCP port as well, and
/// the two ports do not clash.
pub const DEFAULT_PORT: u16 = 9950;
const MAGIC: [u8; 4] = *b"CRVD";
const KIND_QUERY: u8 = 1;
const KIND_ANSWER: u8 = 2;
/// The longest name that an answer carries, in bytes
pub const MAX_NAME: usize = 40;
/// The length of a query. An answer is never longer.
pub const QUERY_LEN: usize = 80;
/// The bytes of an answer before its name
const ANSWER_HEAD: usize = 4 + 1 + 2 + 2 + 2 + FINGERPRINT_LEN + 8 + 1;
/// How often a search repeats its query
const ASK_EVERY: Duration = Duration::from_millis(250);

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn read_u64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
}

/// A name that fits an answer: no control characters, and at most [`MAX_NAME`] bytes cut at a
/// character boundary.
pub fn clean_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > MAX_NAME {
            break;
        }
        out.push(c);
    }
    out.trim().to_owned()
}

/// What a client asks: who is here?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Query {
    /// The protocol versions of the client
    pub versions: VersionRange,
    /// A number that the answer repeats, so a client takes only answers to its own queries
    pub nonce: u64,
}

impl Query {
    pub fn to_bytes(&self) -> [u8; QUERY_LEN] {
        let mut out = [0u8; QUERY_LEN];
        out[..4].copy_from_slice(&MAGIC);
        out[4] = KIND_QUERY;
        out[5..7].copy_from_slice(&self.versions.min.to_le_bytes());
        out[7..9].copy_from_slice(&self.versions.max.to_le_bytes());
        out[9..17].copy_from_slice(&self.nonce.to_le_bytes());
        out
    }

    /// `None` for anything that is not a full query
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < QUERY_LEN || bytes[..4] != MAGIC || bytes[4] != KIND_QUERY {
            return None;
        }
        Some(Self { versions: VersionRange { min: read_u16(bytes, 5), max: read_u16(bytes, 7) }, nonce: read_u64(bytes, 9) })
    }
}

/// What a host answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// The protocol versions of the host
    pub versions: VersionRange,
    /// The host's TCP port for the native protocol
    pub port: u16,
    /// The start of the hash of the host's key (see [`crate::identity::fingerprint`])
    pub fingerprint: [u8; FINGERPRINT_LEN],
    /// The nonce of the query
    pub nonce: u64,
    /// The name of the Mac, for the user
    pub name: String,
}

impl Answer {
    pub fn to_bytes(&self) -> Vec<u8> {
        let name = clean_name(&self.name);
        let mut out = Vec::with_capacity(ANSWER_HEAD + name.len());
        out.extend_from_slice(&MAGIC);
        out.push(KIND_ANSWER);
        out.extend_from_slice(&self.versions.min.to_le_bytes());
        out.extend_from_slice(&self.versions.max.to_le_bytes());
        out.extend_from_slice(&self.port.to_le_bytes());
        out.extend_from_slice(&self.fingerprint);
        out.extend_from_slice(&self.nonce.to_le_bytes());
        out.push(name.len() as u8);
        out.extend_from_slice(name.as_bytes());
        out
    }

    /// `None` for anything that is not a complete answer. Bytes after the name are ignored.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < ANSWER_HEAD || bytes[..4] != MAGIC || bytes[4] != KIND_ANSWER {
            return None;
        }
        let name_len = bytes[ANSWER_HEAD - 1] as usize;
        let name = bytes.get(ANSWER_HEAD..ANSWER_HEAD + name_len).filter(|_| name_len <= MAX_NAME)?;
        let fingerprint_at = 4 + 1 + 2 + 2 + 2;
        Some(Self {
            versions: VersionRange { min: read_u16(bytes, 5), max: read_u16(bytes, 7) },
            port: read_u16(bytes, 9),
            fingerprint: bytes[fingerprint_at..fingerprint_at + FINGERPRINT_LEN].try_into().expect("eight bytes"),
            nonce: read_u64(bytes, fingerprint_at + FINGERPRINT_LEN),
            name: clean_name(std::str::from_utf8(name).ok()?),
        })
    }
}

/// The host's end: a socket that answers queries.
pub struct Responder {
    socket: UdpSocket,
}

impl Responder {
    /// Listens for queries on `port` on every address. Another responder on the same port, such as
    /// a second host on this Mac, may share it. Port 0 picks a free one, for tests.
    pub fn bind(port: u16) -> io::Result<Self> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        #[cfg(unix)]
        socket.set_reuse_port(true)?;
        socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;
        let socket: UdpSocket = socket.into();
        socket.set_read_timeout(Some(Duration::from_millis(200)))?;
        Ok(Self { socket })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Answers each query with `answer`, with the query's nonce, until `stop` is set. It takes the
    /// calling thread.
    pub fn run(&self, stop: &AtomicBool, answer: &Answer) {
        let mut buf = [0u8; 256];
        while !stop.load(Ordering::Relaxed) {
            let Ok((n, from)) = self.socket.recv_from(&mut buf) else { continue };
            if let Some(query) = Query::parse(&buf[..n]) {
                let reply = Answer { nonce: query.nonce, ..answer.clone() }.to_bytes();
                // A client that left, or a network that drops the answer, is nobody's error
                self.socket.send_to(&reply, from).ok();
            }
        }
    }
}

/// A host that answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The address that the answer came from. The client connects to it.
    pub address: IpAddr,
    pub answer: Answer,
}

impl Found {
    /// The address and port to connect to
    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.address, self.answer.port)
    }
}

/// The client's end: a socket that sends queries and collects the answers.
pub struct Finder {
    socket: UdpSocket,
    nonce: u64,
    targets: Vec<SocketAddr>,
}

impl Finder {
    /// A finder that asks each of `targets`. [`broadcast_targets`] gives the usual ones.
    pub fn new(targets: Vec<SocketAddr>) -> io::Result<Self> {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        socket.set_broadcast(true)?;
        let nonce = u64::from_le_bytes(random_bytes()?[..8].try_into().expect("eight bytes"));
        Ok(Self { socket, nonce, targets })
    }

    /// The port of the finder's own socket, where the answers arrive
    pub fn local_port(&self) -> u16 {
        self.socket.local_addr().map_or(0, |address| address.port())
    }

    /// Sends the query to every target. A target that the network refuses is skipped: a network
    /// without broadcast still has the other targets.
    pub fn ask(&self) {
        let query = Query { versions: VersionRange::OURS, nonce: self.nonce }.to_bytes();
        for target in &self.targets {
            self.socket.send_to(&query, target).ok();
        }
    }

    /// Waits up to `timeout` for one answer to this finder's query. It skips other datagrams.
    pub fn receive(&self, timeout: Duration) -> Option<Found> {
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 512];
        loop {
            let left = deadline.checked_duration_since(Instant::now()).filter(|left| !left.is_zero())?;
            self.socket.set_read_timeout(Some(left)).ok()?;
            let Ok((n, from)) = self.socket.recv_from(&mut buf) else { return None };
            if let Some(answer) = Answer::parse(&buf[..n]).filter(|answer| answer.nonce == self.nonce) {
                return Some(Found { address: from.ip(), answer });
            }
        }
    }
}

/// Where a client asks: every host on the network by the limited broadcast address, the hosts on
/// the client's own /24 network by its directed broadcast address (some networks drop the first),
/// and this machine, for a flat window beside the host.
pub fn broadcast_targets(port: u16) -> Vec<SocketAddr> {
    let mut targets = vec![SocketAddr::from((Ipv4Addr::BROADCAST, port))];
    if let Some(IpAddr::V4(own)) = own_address() {
        let [a, b, c, _] = own.octets();
        if !own.is_loopback() {
            targets.push(SocketAddr::from((Ipv4Addr::new(a, b, c, 255), port)));
        }
    }
    targets.push(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
    targets
}

/// The address that this machine uses for the default route. A UDP socket that connects sends
/// nothing: the system only picks the route.
fn own_address() -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    socket.local_addr().ok().map(|address| address.ip())
}

/// Asks `targets` for `wait`, repeating the query, and returns the hosts that answered, each once
/// by address and port.
pub fn find(targets: Vec<SocketAddr>, wait: Duration) -> io::Result<Vec<Found>> {
    let finder = Finder::new(targets)?;
    let deadline = Instant::now() + wait;
    let mut found: Vec<Found> = Vec::new();
    while Instant::now() < deadline {
        finder.ask();
        let round = Instant::now() + ASK_EVERY;
        while let Some(host) = finder.receive(round.saturating_duration_since(Instant::now())) {
            if !found.iter().any(|known| known.address == host.address && known.answer.port == host.answer.port) {
                found.push(host);
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer() -> Answer {
        Answer {
            versions: VersionRange { min: 4, max: 5 },
            port: 9950,
            fingerprint: [1, 2, 3, 4, 5, 6, 7, 8],
            nonce: 0xdead_beef_0102_0304,
            name: "Dmitry's MacBook Pro".into(),
        }
    }

    #[test]
    fn a_query_round_trips_and_has_the_length_of_the_longest_answer() {
        let query = Query { versions: VersionRange { min: 4, max: 5 }, nonce: 77 };
        let bytes = query.to_bytes();
        assert_eq!(Query::parse(&bytes), Some(query));
        // The longest answer is not longer than a query: no amplification
        let longest = Answer { name: "x".repeat(MAX_NAME), ..answer() }.to_bytes();
        assert!(longest.len() <= QUERY_LEN, "{} bytes", longest.len());
        // A short or foreign packet is no query
        assert_eq!(Query::parse(&bytes[..QUERY_LEN - 1]), None);
        let mut other = bytes;
        other[0] = b'X';
        assert_eq!(Query::parse(&other), None);
        // An answer is no query, and a query is no answer
        assert_eq!(Query::parse(&answer().to_bytes()), None);
        assert_eq!(Answer::parse(&bytes), None);
    }

    #[test]
    fn an_answer_round_trips() {
        let bytes = answer().to_bytes();
        assert_eq!(Answer::parse(&bytes), Some(answer()));
        // Later versions may add fields: bytes after the name do not matter
        let mut longer = bytes.clone();
        longer.extend_from_slice(b"more fields");
        assert_eq!(Answer::parse(&longer), Some(answer()));
    }

    #[test]
    fn a_broken_answer_is_ignored() {
        let bytes = answer().to_bytes();
        for cut in 0..bytes.len() {
            assert_eq!(Answer::parse(&bytes[..cut]), None, "cut at {cut}");
        }
        // A name length above the limit
        let mut big = bytes.clone();
        big[ANSWER_HEAD - 1] = (MAX_NAME + 1) as u8;
        big.extend_from_slice(&[b'x'; 64]);
        assert_eq!(Answer::parse(&big), None);
        // A name that is not UTF-8
        let mut bad = bytes.clone();
        bad[ANSWER_HEAD] = 0xff;
        assert_eq!(Answer::parse(&bad), None);
        assert_eq!(Answer::parse(b""), None);
        assert_eq!(Answer::parse(&[0u8; 100]), None);
    }

    #[test]
    fn a_long_name_is_cut_at_a_character_boundary_and_control_characters_go() {
        // Each of these characters takes three bytes
        let long = Answer { name: "\u{2019}".repeat(30), ..answer() };
        let parsed = Answer::parse(&long.to_bytes()).unwrap();
        assert!(parsed.name.len() <= MAX_NAME);
        assert_eq!(parsed.name.chars().count(), MAX_NAME / 3);
        assert_eq!(clean_name("a\nb\u{7}c"), "abc");
        assert_eq!(clean_name("  Mac  "), "Mac");
    }
}
