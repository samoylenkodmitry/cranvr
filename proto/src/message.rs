//! The messages of a session. Each direction has two enums. Control messages go over TCP and
//! arrive in order. Datagram messages may get lost and may arrive late.
//!
//! The handshake in order:
//! 1. Both sides swap a preamble (see [`client_preamble`]) that picks the protocol version.
//! 2. The client sends [`ClientMsg::Hello`] with its capabilities.
//! 3. The host answers [`HostMsg::Welcome`] with its choices, or [`HostMsg::Reject`].
//! 4. With UDP media, the client repeats [`ClientDatagram::Hello`] until the host answers
//!    [`HostMsg::MediaReady`] over TCP. The datagram tells the host the client's UDP address.
//!    With TCP media the host sends `MediaReady` at once.
//! 5. The host sends [`HostMsg::DecoderConfig`] and then frames. The first frame is a key frame.

use crate::wire::{Codec, Fov, Motion, Nanos, Pose};
use crate::{Error, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION};
use cranvr_common::foveation::FoveationConfig;
use serde::{Deserialize, Serialize};

/// The first four bytes of both preambles.
pub const MAGIC: [u8; 4] = *b"CRNV";

/// A range of protocol versions, ends included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VersionRange {
    pub min: u16,
    pub max: u16,
}

impl VersionRange {
    /// The versions this crate speaks.
    pub const OURS: VersionRange = VersionRange { min: MIN_PROTOCOL_VERSION, max: PROTOCOL_VERSION };

    /// The highest version in both ranges.
    pub fn common(self, other: VersionRange) -> Option<u16> {
        let high = self.max.min(other.max);
        (high >= self.min.max(other.min)).then_some(high)
    }
}

/// The client's first bytes on the TCP connection: the magic and its version range.
pub const CLIENT_PREAMBLE_LEN: usize = 8;
/// The host's answer: the magic, the chosen version (0 for none) and the host's version range.
pub const HOST_PREAMBLE_LEN: usize = 10;

pub fn client_preamble(range: VersionRange) -> [u8; CLIENT_PREAMBLE_LEN] {
    let mut out = [0; CLIENT_PREAMBLE_LEN];
    out[..4].copy_from_slice(&MAGIC);
    out[4..6].copy_from_slice(&range.min.to_le_bytes());
    out[6..8].copy_from_slice(&range.max.to_le_bytes());
    out
}

pub fn parse_client_preamble(bytes: &[u8; CLIENT_PREAMBLE_LEN]) -> Result<VersionRange, Error> {
    if bytes[..4] != MAGIC {
        return Err(Error::Malformed("not a cranvr client"));
    }
    Ok(VersionRange { min: u16::from_le_bytes([bytes[4], bytes[5]]), max: u16::from_le_bytes([bytes[6], bytes[7]]) })
}

pub fn host_preamble(chosen: Option<u16>, range: VersionRange) -> [u8; HOST_PREAMBLE_LEN] {
    let mut out = [0; HOST_PREAMBLE_LEN];
    out[..4].copy_from_slice(&MAGIC);
    out[4..6].copy_from_slice(&chosen.unwrap_or(0).to_le_bytes());
    out[6..8].copy_from_slice(&range.min.to_le_bytes());
    out[8..10].copy_from_slice(&range.max.to_le_bytes());
    out
}

/// The chosen version (`None` when the ranges do not meet) and the host's range.
pub fn parse_host_preamble(bytes: &[u8; HOST_PREAMBLE_LEN]) -> Result<(Option<u16>, VersionRange), Error> {
    if bytes[..4] != MAGIC {
        return Err(Error::Malformed("not a cranvr host"));
    }
    let chosen = u16::from_le_bytes([bytes[4], bytes[5]]);
    let range = VersionRange { min: u16::from_le_bytes([bytes[6], bytes[7]]), max: u16::from_le_bytes([bytes[8], bytes[9]]) };
    Ok(((chosen != 0).then_some(chosen), range))
}

/// What a headset can do.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClientCaps {
    /// The device name for logs, such as "Quest 3"
    pub name: String,
    /// The size in pixels of one eye's image at the headset's native density
    pub eye_size: [u32; 2],
    pub views: Views,
    /// The display's refresh rates in Hz
    pub refresh_rates: Vec<f32>,
    /// The codecs its hardware decodes, the preferred one first
    pub codecs: Vec<Codec>,
    /// How many decoder instances run at the same time
    pub decoders: u8,
    /// The client can unwarp a foveated frame
    pub foveation: bool,
    /// The client reaches the host through a TCP-only path, such as `adb reverse` over USB. The
    /// host then sends the media over TCP.
    pub tcp_only: bool,
}

/// The two eyes' field of view and their place relative to the head.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Views {
    pub fov: [Fov; 2],
    /// Each eye's pose in the head's space
    pub eye_pose: [Pose; 2],
}

/// How the host sends foveation to the client. It has the same fields as
/// `cranvr_common::foveation::FoveationConfig`: both sides derive the encoded layout from them.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FoveationParams {
    pub center_size: [f32; 2],
    pub center_shift: [f32; 2],
    pub edge_ratio: [f32; 2],
}

impl From<FoveationConfig> for FoveationParams {
    fn from(c: FoveationConfig) -> Self {
        Self { center_size: c.center_size, center_shift: c.center_shift, edge_ratio: c.edge_ratio }
    }
}

impl From<FoveationParams> for FoveationConfig {
    fn from(p: FoveationParams) -> Self {
        Self { center_size: p.center_size, center_shift: p.center_shift, edge_ratio: p.edge_ratio }
    }
}

/// What the host chose for one video stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamParams {
    /// 0 is the eye stream. Other ids are for further streams, such as a desktop layer.
    pub stream: u8,
    /// The size in pixels of one eye's image before foveation
    pub eye_size: [u32; 2],
    pub codec: Codec,
    pub refresh_hz: f32,
    pub bitrate_bps: u64,
    pub foveation: Option<FoveationParams>,
    /// The encoder uses the full 0 to 255 range
    pub full_range: bool,
    /// The slices in each frame. 1 means that a frame is one unit.
    pub slices: u8,
    pub audio: Option<AudioFormat>,
}

/// How the media reaches the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaPlan {
    /// UDP to this port on the host's address, with parity packets
    Udp { port: u16 },
    /// The TCP connection of the control messages, without parity
    Tcp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    /// A number that the client repeats in its UDP hello, so the host knows whose datagram it is
    pub session: u64,
    pub version: u16,
    pub stream: StreamParams,
    pub media: MediaPlan,
    /// The payload bytes in each media packet. All data and parity packets of a unit have it.
    pub shard_len: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RejectReason {
    NotTrusted,
    /// Another headset is connected
    Busy,
    NoCommonCodec,
    NoCommonRefreshRate,
    Other(String),
}

/// Control messages from the client to the host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClientMsg {
    Hello(ClientCaps),
    /// The client lost a unit that FEC could not recover, or its decoder broke. The host sends a
    /// key frame next. `frame` is the frame that failed.
    RequestKeyFrame { stream: u8, frame: u32 },
    /// The headset's views changed
    Views(Views),
    Bye,
    /// The headset's runtime moved the origin of its tracking space, which the user asked for by
    /// recentering. A receiver that does not know this message skips it.
    Recenter(Recenter),
}

/// A change of the origin of the poses that the client sends. Every pose with a display time at or
/// after `change_time` is relative to the new origin. Poses that came before it are relative to the
/// old one.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recenter {
    /// When the new origin takes effect, on the client's clock. It may lie after the time at which
    /// the client sends the message.
    pub change_time: Nanos,
    /// The new origin as a pose in the old space, when the runtime gives it
    pub origin: Option<Pose>,
}

/// Control messages from the host to the client.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HostMsg {
    Welcome(Welcome),
    Reject(RejectReason),
    /// The media path works
    MediaReady,
    /// The decoder's parameter sets (for HEVC: VPS, SPS and PPS in Annex B). The client needs it
    /// before the first frame, so it travels over the reliable path. The host sends it again when
    /// it changes.
    DecoderConfig { stream: u8, codec: Codec, config: Vec<u8> },
    /// The host changed the stream, for example the foveation or the eye size
    StreamUpdate(StreamParams),
    Bye,
}

/// Datagram messages from the client to the host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClientDatagram {
    /// The first datagram on the UDP path
    Hello { session: u64 },
    Tracking(Tracking),
    Input(Input),
    FrameTiming(FrameTiming),
    Loss(LossReport),
    Ping(Ping),
    Pong(Pong),
}

/// Datagram messages from the host to the client.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HostDatagram {
    Haptics(Haptics),
    Audio(Audio),
    Ping(Ping),
    Pong(Pong),
}

/// The head and the controllers at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tracking {
    /// When the headset sampled the poses, on the client's clock. Frames come back with this
    /// time, so it names the sample.
    pub sample_time: Nanos,
    pub head: Motion,
    pub left: Option<Motion>,
    pub right: Option<Motion>,
}

/// Bits of [`HandInput::buttons`] and [`HandInput::touches`].
pub mod buttons {
    /// A on the right controller, X on the left
    pub const PRIMARY: u32 = 1 << 0;
    /// B on the right controller, Y on the left
    pub const SECONDARY: u32 = 1 << 1;
    pub const MENU: u32 = 1 << 2;
    pub const STICK_CLICK: u32 = 1 << 3;
    pub const TRIGGER_CLICK: u32 = 1 << 4;
    pub const GRIP_CLICK: u32 = 1 << 5;
    /// For touches: the thumb rests on the stick, the rest, the trigger
    pub const THUMB_REST: u32 = 1 << 6;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HandInput {
    pub buttons: u32,
    pub touches: u32,
    pub trigger: f32,
    pub grip: f32,
    /// x then y, each from -1 to 1
    pub stick: [f32; 2],
}

/// The buttons, triggers and sticks of both controllers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Input {
    pub sample_time: Nanos,
    /// Left, then right
    pub hands: [HandInput; 2],
}

/// What the client's compositor did with one frame. All times are on the client's clock. The host
/// uses them to start its frames so that they arrive just before the display needs them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FrameTiming {
    pub stream: u8,
    pub frame: u32,
    /// The tracking sample of the frame, from its metadata
    pub tracking_sample: Nanos,
    /// The last packet of the frame arrived
    pub received: Nanos,
    /// The decoder finished the frame
    pub decode_end: Nanos,
    /// The display time that the compositor reported for this frame
    pub display_time: Nanos,
    /// The next display time that the compositor predicts after this one
    pub next_display_time: Nanos,
}

/// What the client's receiver counted since its last report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LossReport {
    /// The packets that the finished units had in total, data and parity
    pub packets_expected: u32,
    /// The packets of those units that arrived
    pub packets_received: u32,
    /// Units that arrived complete
    pub units_complete: u32,
    /// Units that needed parity to complete
    pub units_recovered: u32,
    /// Units that FEC could not complete
    pub units_lost: u32,
    /// Data packets that FEC restored
    pub packets_recovered: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ping {
    pub id: u32,
    /// The sender's clock when it sent the ping
    pub sent: Nanos,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pong {
    pub id: u32,
    /// The pinger's clock from the ping
    pub ping_sent: Nanos,
    /// The answerer's clock when the ping arrived
    pub received: Nanos,
    /// The answerer's clock when it sent this pong
    pub sent: Nanos,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Haptics {
    /// 0 left, 1 right
    pub hand: u8,
    pub duration_ns: u64,
    pub frequency: f32,
    pub amplitude: f32,
}

/// The format of the Mac's captured sound, as the host's capture produces it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u8,
    pub sample: SampleFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SampleFormat {
    /// Signed 16-bit little-endian, interleaved
    S16Le,
}

impl AudioFormat {
    /// What the host captures: stereo at 48 kHz, 16 bits.
    pub const HOST_CAPTURE: AudioFormat = AudioFormat { sample_rate: 48_000, channels: 2, sample: SampleFormat::S16Le };
}

/// The most PCM bytes in one [`Audio`] message that still fits a datagram.
pub const MAX_AUDIO_BYTES: usize = 1200;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Audio {
    /// Counts up by one per message, so the client sees gaps
    pub sequence: u32,
    /// When the host captured the first sample, on the host's clock
    pub host_time: Nanos,
    pub pcm: Vec<u8>,
}

/// What the host wants from a stream, before it knows the client.
#[derive(Clone, Debug)]
pub struct HostWishes {
    /// The eye width in pixels; 0 takes the client's own width
    pub eye_width: u32,
    /// The eye width for a client that takes no foveation, when `foveation` is wished. The width
    /// of a foveated eye counts pixels that the foveation then leaves out, so a client that takes
    /// no foveation would get a picture that no encoder or decoder holds. 0 keeps `eye_width`.
    pub eye_width_unfoveated: u32,
    pub refresh_hz: f32,
    pub bitrate_bps: u64,
    /// The codecs the host encodes, the preferred one first
    pub codecs: Vec<Codec>,
    pub foveation: Option<FoveationParams>,
    pub full_range: bool,
    pub slices: u8,
}

/// Picks the stream for a client. The codec is the first one that both sides know. The refresh
/// rate is the highest one of the client that does not exceed the wish, or the lowest one above
/// it when none is below.
pub fn negotiate(wishes: &HostWishes, caps: &ClientCaps) -> Result<StreamParams, RejectReason> {
    let codec = wishes.codecs.iter().copied().find(|c| caps.codecs.contains(c)).ok_or(RejectReason::NoCommonCodec)?;
    let mut rates: Vec<f32> = caps.refresh_rates.iter().copied().filter(|r| *r > 0.0).collect();
    rates.sort_by(f32::total_cmp);
    let refresh_hz = rates
        .iter()
        .rev()
        .copied()
        .find(|r| *r <= wishes.refresh_hz + 0.5)
        .or_else(|| rates.first().copied())
        .ok_or(RejectReason::NoCommonRefreshRate)?;
    let [native_w, native_h] = caps.eye_size;
    let foveation = wishes.foveation.filter(|_| caps.foveation);
    let wished = if wishes.foveation.is_some() && foveation.is_none() && wishes.eye_width_unfoveated != 0 {
        wishes.eye_width_unfoveated
    } else {
        wishes.eye_width
    };
    let width = if wished == 0 { native_w } else { wished };
    // Keep the client's aspect ratio. Hardware encoders want even sizes.
    let height = ((width as u64 * native_h as u64 + native_w as u64 / 2) / native_w.max(1) as u64) as u32 & !1;
    Ok(StreamParams {
        stream: 0,
        eye_size: [width, height],
        codec,
        refresh_hz,
        bitrate_bps: wishes.bitrate_bps,
        foveation,
        full_range: wishes.full_range,
        slices: wishes.slices.max(1),
        audio: None,
    })
}
