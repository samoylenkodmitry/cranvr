//! cranvr's own protocol between the Mac host and the Quest client ("native mode"). The crate
//! has no dependency on ALVR. It builds for macOS and for `aarch64-linux-android`.
//!
//! A session has three paths:
//!
//! - **Control** runs over TCP. The client dials the host, because `adb reverse` forwards only in
//!   that direction. Both sides first swap a fixed preamble that names the protocol versions they
//!   speak. Every later message is a [`postcard`] value. The messages are in [`message`].
//! - **Media** goes from the host to the client. Each encoded slice of a frame is a "unit". The
//!   unit is cut into packets that fit a 1400-byte datagram. Reed-Solomon parity packets follow
//!   (see [`media`] and [`fec`]). Over a USB link the same packets go through the TCP connection
//!   with no parity.
//! - **Datagrams** are small messages that may get lost. Tracking, input, frame timing, loss
//!   reports, clock pings and haptics use them. They share the UDP socket with the media, or the
//!   TCP connection when UDP is off.
//!
//! Every packet on either path starts with a kind byte (see [`wire`]). A TCP packet has a four
//! byte length before it.
//!
//! Times are `u64` nanoseconds on the clock of the side that wrote them. The host and the client
//! each have their own monotonic clock. [`clock::ClockSync`] estimates the offset between them
//! from ping exchanges.

pub mod clock;
pub mod fec;
pub mod identity;
pub mod link;
pub mod media;
pub mod message;
pub mod motion;
pub mod wire;

pub use wire::{Error, Nanos};

/// The newest protocol version this crate speaks. A change of the wire format, or of a message
/// that older code cannot read, raises it. New enum variants at the end of a message enum do not
/// need a new version for receivers that ignore what they cannot decode.
///
/// Version 2 added the key and the challenge of pairing, the display time of a tracking sample
/// and the moment at which the compositor took a frame.
pub const PROTOCOL_VERSION: u16 = 2;

/// The oldest protocol version this crate still speaks.
pub const MIN_PROTOCOL_VERSION: u16 = 2;

/// The largest datagram, in bytes. A media packet and any datagram message fit in it. A typical
/// path MTU of 1500 bytes minus the IP and UDP headers leaves 1472 bytes. The 1400 bytes leave
/// room for a tunnel header.
pub const MTU: usize = 1400;
