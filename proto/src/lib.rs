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
//!   reports, clock pings, haptics, the pointer on the desktop panel, the Mac's cursor and the
//!   Mac's sound use them. They share the UDP socket with the media, or the TCP connection when
//!   UDP is off.
//!
//! The eye stream has the stream id 0. The Mac's desktop is a second video stream with the id
//! [`message::DESKTOP_STREAM`]. Its units share the media path with the eye stream and have their own
//! frame numbers, key frames and decoder configuration.
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

/// The newest protocol version this crate speaks. A change of the wire format, or of a message
/// that older code cannot read, raises it. New enum variants at the end of a message enum do not
/// need a new version for receivers that ignore what they cannot decode.
///
/// Version 2 added the key and the challenge of pairing, the display time of a tracking sample
/// and the moment at which the compositor took a frame.
///
/// Version 3 added the aim pose of each controller to [`message::Tracking`]. Postcard has no
/// optional trailing fields, so a version 2 sample does not decode and the two versions cannot
/// talk.
///
/// Version 4 seals the session. Both sides sign a key exchange. Every packet after it is encrypted
/// and authenticated. The host also has a key. Versions 2 and 3 ran in clear text, so this crate
/// speaks no older version. A peer of version 3 or lower is refused.
///
/// Version 5 adds the approval of a new headset on the Mac. A host that does not know a client
/// holds the connection and tells the client to wait ([`message::HostMsg::AwaitApproval`]). Both
/// sides show a code that comes from the session (see [`secure::approval_code`]). The user compares
/// the two codes and allows the headset on the Mac. A version 4 client cannot wait: the host refuses
/// it at once, as before. A headset that is paired already connects with either version.
///
/// Version 6 adds the tracking of hands, body, face and eyes (see [`tracking`]): the offer of the
/// client, the wish of the host and one datagram for each feature. They are new variants at the end
/// of the message enums, so a peer of version 4 or 5 skips them and the two still talk. The host
/// sends a wish only to a client of version 6, and a client of version 6 offers features only to a
/// host that answered with version 6.
///
/// Version 8 adds displays: the client adds and removes displays of the Mac and asks for the video
/// of each one (`ClientMsg::Display`, `ClientMsg::DisplayWish`, `HostMsg::Displays`,
/// `HostMsg::DisplayStream`), the host says whether a game runs (`HostMsg::GameRuns`), and the
/// client sends the controllers' input together with the hands that it holds back from the game
/// (`ClientDatagram::InputHold`). All of them are new variants at the end of the enums, so a peer
/// of an older version skips them. A host of version 8 keeps the video of a display that the
/// client wishes while a game runs. It hides the first display for a game only from a client of
/// an older version, which has no other way to learn that a game runs.
pub const PROTOCOL_VERSION: u16 = 8;

/// The first protocol version with displays, the game's state and held hands.
pub const DISPLAYS_VERSION: u16 = 8;

/// The oldest protocol version this crate still speaks.
pub const MIN_PROTOCOL_VERSION: u16 = 4;

/// The first protocol version with the approval of a new headset on the Mac.
pub const APPROVAL_VERSION: u16 = 5;

/// The first protocol version with the tracking messages of [`tracking`].
pub const TRACKING_VERSION: u16 = tracking::TRACKING_VERSION;

/// The largest datagram, in bytes, sealed: the counter and the tag are in it. A media packet and
/// any datagram message fit in it. A typical path MTU of 1500 bytes minus the IP and UDP headers
/// leaves 1472 bytes. The 1400 bytes leave room for a tunnel header.
pub const MTU: usize = 1400;
