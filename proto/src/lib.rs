//! cranvr's protocol between the Mac host and the headset client. The crate builds for macOS and
//! for `aarch64-linux-android`.
//!
//! A session has three paths:
//!
//! - **Control** runs over TCP. The client dials the host, because `adb reverse` forwards only in
//!   that direction. Both sides first swap a fixed preamble that names the protocol versions they
//!   speak. Then they run the handshake that proves their keys and agrees on the session's keys
//!   (see [`secure`]). Every later message is a [`postcard`] value, sealed. The messages are in
//!   [`message`].
//! - **Media** goes from the host to the client. Each encoded slice of a frame is a "unit". The
//!   unit is cut into packets that fit a 1400-byte datagram. Reed-Solomon parity packets follow
//!   (see [`media`] and [`fec`]). Over a USB link the same packets go through the TCP connection
//!   with no parity.
//! - **Datagrams** are small messages that may get lost. Tracking, input, frame timing, loss
//!   reports, clock pings, haptics, the mouse on the panel of a display, the Mac's cursors and the
//!   Mac's sound use them. They share the UDP socket with the media, or the TCP connection when
//!   UDP is off.
//!
//! The eye stream has the stream id 0. Each display of the Mac is a further video stream, from the id
//! [`message::DESKTOP_STREAM`] on (see [`message::display_stream`]). Their units share the media path
//! with the eye stream and have their own frame numbers, key frames and decoder configuration.
//!
//! Every packet on either path is sealed after the handshake: ChaCha20-Poly1305 encrypts it and
//! authenticates it with a key that only the two sides hold. Its content starts with a kind byte
//! (see [`wire`]). A TCP packet has a four byte length before it. A sealed UDP packet starts with
//! the counter that makes its nonce, so the receiver can drop a replayed packet.
//!
//! Times are `u64` nanoseconds on the clock of the side that wrote them. The host and the client
//! each have their own monotonic clock. [`clock::ClockSync`] estimates the offset between them
//! from ping exchanges.

pub mod clock;
pub mod discovery;
pub mod fec;
pub mod identity;
pub mod link;
pub mod media;
pub mod message;
pub mod motion;
pub mod secure;
pub mod tracking;
pub mod wire;

pub use wire::{Error, Nanos};

/// The protocol version. The crate speaks this one and no other. A peer of another version is
/// refused with a message that says which side to update (see [`message::peer_must_update`]). Any
/// change of the wire format or of a message raises it. Both ends are one project and ship from one
/// commit, so the version tells that they do.
///
/// Version 10 says in the client's capabilities whether the link runs over a USB cable
/// (`ClientCaps::cable`). The headset's USB network carries UDP, so a TCP-only path no longer
/// stands for the cable.
///
/// Version 11 says in the host's preamble whether the host takes a link over a USB cable. The
/// host's user chooses Wi-Fi to stop the cable on every way over it: the headset's USB network and
/// `adb reverse`.
///
/// Version 12 carries the client's microphone for the host's games: `ClientCaps::microphone`,
/// `HostMsg::Microphone` and `ClientDatagram::Microphone`.
pub const PROTOCOL_VERSION: u16 = 12;

/// The largest datagram, in bytes, sealed: the counter and the tag are in it. A media packet and
/// any datagram message fit in it. A typical path MTU of 1500 bytes minus the IP and UDP headers
/// leaves 1472 bytes. The 1400 bytes leave room for a tunnel header.
pub const MTU: usize = 1400;
