//! Basic types, the error type and the packet framing that all paths share.
//!
//! Serialization: [`postcard`]. Reasons for the choice:
//! - Its wire format is documented and stable, so a Quest app and a Mac host of different builds
//!   can agree on a protocol version by number.
//! - Integers are variable-length, so frame indexes, counters and small enums take one or two
//!   bytes. Floats stay four bytes, so poses have a fixed size.
//! - It writes into a caller's buffer (`to_slice`, `to_extend`), which keeps the packet path free
//!   of allocation.
//! - It is maintained. It needs no code generation beyond serde's derive.
//!
//! The media packet header is not a postcard value. It has fixed offsets, so a receiver can read
//! it without a parser and the sender can write it into a reused buffer.
//!
//! Every packet after the handshake is sealed (see [`crate::secure`]). A sealed packet is a counter,
//! the content encrypted in place and a tag. The content is the kind byte and the body that the
//! rest of this crate builds. [`PacketBuf`] reserves room for the counter in front of the content,
//! so sealing needs no copy and no allocation.

use crate::secure::{Cipher, COUNTER_LEN, OVERHEAD};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{fmt, io};

/// A time in nanoseconds on one side's monotonic clock.
pub type Nanos = u64;

/// The first byte of a TCP control packet.
pub const KIND_CONTROL: u8 = 1;
/// The first byte of a media packet.
pub const KIND_MEDIA: u8 = 2;
/// The first byte of a datagram message.
pub const KIND_DATAGRAM: u8 = 3;
/// The first byte of a handshake packet. The handshake's own packets carry it. The sealed answer of
/// the client carries it as the first byte of its content.
pub const KIND_HANDSHAKE: u8 = 4;

/// The longest TCP packet this crate reads, in bytes. A longer length prefix means a broken stream.
pub const MAX_TCP_PACKET: usize = 16 << 20;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// A message that postcard could not write or read
    Codec(postcard::Error),
    /// Bytes that do not form a valid packet
    Malformed(&'static str),
    /// A packet or message above its size limit
    TooLarge(usize),
    /// The peer speaks no protocol version that this side speaks
    Version { ours: (u16, u16), theirs: (u16, u16) },
    /// The host refused the client
    Rejected(crate::message::RejectReason),
    /// The peer sent something else than the handshake expects
    Unexpected(&'static str),
    /// A signature, a tag or a key that does not hold. The peer is not who it claims to be, or the
    /// packet was changed on the way.
    Crypto(&'static str),
    /// The host's key differs from the one that this client paired with
    HostKeyChanged,
    /// A wait ran out
    Timeout,
    /// The connection ended
    Closed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Codec(e) => write!(f, "message codec: {e}"),
            Error::Malformed(what) => write!(f, "malformed packet: {what}"),
            Error::TooLarge(n) => write!(f, "{n} bytes is more than a packet may hold"),
            Error::Version { ours, theirs } => write!(
                f,
                "no common protocol version: this side speaks {}..={}, the peer {}..={}",
                ours.0, ours.1, theirs.0, theirs.1
            ),
            Error::Rejected(reason) => write!(f, "the host refused the connection: {reason:?}"),
            Error::Unexpected(what) => write!(f, "unexpected message: {what}"),
            Error::Crypto(what) => write!(f, "authentication failed: {what}"),
            Error::HostKeyChanged => write!(f, "the host's key is not the one that this client paired with"),
            Error::Timeout => write!(f, "timed out"),
            Error::Closed => write!(f, "connection closed"),
        }
    }
}

impl Error {
    /// For a version mismatch: whether the peer is the side that must update. `None` for any other
    /// error.
    pub fn peer_must_update(&self) -> Option<bool> {
        match self {
            Error::Version { ours, theirs } => Some(theirs.1 < ours.1),
            _ => None,
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Error::Timeout,
            io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe => Error::Closed,
            _ => Error::Io(e),
        }
    }
}

impl From<postcard::Error> for Error {
    fn from(e: postcard::Error) -> Self {
        Error::Codec(e)
    }
}

/// A position and an orientation in the tracking space. Meters, right-handed, y up, as OpenXR.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pose {
    /// x, y, z, w
    pub orientation: [f32; 4],
    pub position: [f32; 3],
}

impl Pose {
    pub const IDENTITY: Pose = Pose { orientation: [0.0, 0.0, 0.0, 1.0], position: [0.0; 3] };
}

impl Default for Pose {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// A pose with its velocities, in meters per second and radians per second.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Motion {
    pub pose: Pose,
    pub linear_velocity: [f32; 3],
    pub angular_velocity: [f32; 3],
}

/// The angles of a view frustum in radians from the view axis. `left` and `down` are negative for
/// a symmetric view, as in OpenXR's `XrFovf`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fov {
    pub left: f32,
    pub right: f32,
    pub up: f32,
    pub down: f32,
}

/// A rectangle of pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

/// Appends the postcard bytes of `value` to `out`. `out` keeps its capacity between calls.
pub fn encode<T: Serialize>(value: &T, out: &mut Vec<u8>) -> Result<(), Error> {
    let bytes = std::mem::take(out);
    *out = postcard::to_extend(value, bytes)?;
    Ok(())
}

/// Reads a value that fills `bytes`.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    Ok(postcard::from_bytes(bytes)?)
}

/// Bytes in front of a packet's content: the four byte TCP length and the eight byte counter.
const PREFIX: usize = 4 + COUNTER_LEN;

/// A packet under construction. The first twelve bytes stay free for the TCP length and the
/// counter, so one buffer serves both transports and sealing happens in place: UDP sends
/// [`PacketBuf::datagram`], TCP writes [`PacketBuf::framed`].
pub struct PacketBuf {
    bytes: Vec<u8>,
}

impl Default for PacketBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl PacketBuf {
    pub fn new() -> Self {
        Self { bytes: Vec::with_capacity(2048) }
    }

    /// Starts a packet of `kind` and returns the buffer to append the body to.
    pub fn begin(&mut self, kind: u8) -> &mut Vec<u8> {
        self.bytes.clear();
        self.bytes.extend_from_slice(&[0; PREFIX]);
        self.bytes.push(kind);
        &mut self.bytes
    }

    /// The kind byte and the body, as long as the packet is not sealed.
    pub fn content(&self) -> &[u8] {
        &self.bytes[PREFIX.min(self.bytes.len())..]
    }

    /// The bytes that a sealed copy of this packet adds to its content.
    pub const OVERHEAD: usize = OVERHEAD;

    /// Encrypts the content in place with `cipher` and puts `counter` in front of it. The tag goes
    /// behind it. The packet is then ready for [`PacketBuf::datagram`] and [`PacketBuf::framed`].
    /// The counter must be new for the cipher's key: a key that seals two packets with one counter
    /// loses its secrecy.
    pub fn seal(&mut self, cipher: &Cipher, counter: u64) -> Result<(), Error> {
        let content = self.bytes.len() - PREFIX;
        if content + OVERHEAD > MAX_TCP_PACKET {
            return Err(Error::TooLarge(content + OVERHEAD));
        }
        self.bytes[4..PREFIX].copy_from_slice(&counter.to_le_bytes());
        let tag = cipher.seal_in_place(counter, &mut self.bytes[PREFIX..])?;
        self.bytes.extend_from_slice(&tag);
        let len = (self.bytes.len() - 4) as u32;
        self.bytes[..4].copy_from_slice(&len.to_le_bytes());
        Ok(())
    }

    /// The sealed packet as a datagram: the counter, the encrypted content and the tag.
    pub fn datagram(&self) -> &[u8] {
        &self.bytes[4..]
    }

    /// The sealed packet with its four byte length, as TCP carries it.
    pub fn framed(&self) -> &[u8] {
        &self.bytes
    }
}

/// Reads one TCP packet into `buf`: the kind byte and the body, without the length.
pub fn read_tcp_packet(reader: &mut impl io::Read, buf: &mut Vec<u8>) -> Result<(), Error> {
    read_tcp_packet_limited(reader, buf, MAX_TCP_PACKET)
}

/// Like [`read_tcp_packet`], with a lower limit on the length. The handshake reads from a peer that
/// has proved nothing yet, so it must not let that peer size a buffer of megabytes.
pub fn read_tcp_packet_limited(reader: &mut impl io::Read, buf: &mut Vec<u8>, limit: usize) -> Result<(), Error> {
    let mut prefix = [0u8; 4];
    reader.read_exact(&mut prefix)?;
    let len = u32::from_le_bytes(prefix) as usize;
    if len == 0 || len > limit.min(MAX_TCP_PACKET) {
        return Err(Error::Malformed("TCP packet length out of range"));
    }
    buf.resize(len, 0);
    reader.read_exact(buf)?;
    Ok(())
}
