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
/// Version 9 has the displays of the Mac: the client adds and removes displays and asks for the
/// video of each (`ClientMsg::Display`, `ClientMsg::DisplayWish`), the host lists them and sends
/// each one's stream parameters (`HostMsg::Displays`, `HostMsg::DisplayStream`) and the cursor of
/// each (`HostDatagram::DisplayCursor`), the client sends the mouse of each display
/// (`ClientDatagram::DisplayMouse`), the host says whether a game runs (`HostMsg::GameRuns`), and the
/// client sends the controllers' input together with the hands that it holds back from the game
/// (`ClientDatagram::InputHold`). The first display has no messages of its own.
pub const PROTOCOL_VERSION: u16 = 9;

/// The largest datagram, in bytes, sealed: the counter and the tag are in it. A media packet and
/// any datagram message fit in it. A typical path MTU of 1500 bytes minus the IP and UDP headers
/// leaves 1472 bytes. The 1400 bytes leave room for a tunnel header.
pub const MTU: usize = 1400;
