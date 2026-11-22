//! The messages of a session. Each direction has two enums. Control messages go over TCP and
//! arrive in order. Datagram messages may get lost and may arrive late.
//!
//! The handshake in order:
//! 1. Both sides swap a preamble (see [`client_preamble`]) that holds the protocol version. A side
//!    that sees another version than its own ends the exchange with an update message.
//! 2. The key exchange of [`crate::secure`]: three flights. They prove the keys of both sides and
//!    give both the keys of the session. Everything below is sealed.
//! 3. The client sends [`ClientMsg::Hello`] with its capabilities.
//! 4. The host answers [`HostMsg::Welcome`] with its choices, or [`HostMsg::Reject`]. A host that
//!    does not know the client first sends [`HostMsg::AwaitApproval`]. The client
//!    answers [`ClientMsg::PairNonce`], the host sends [`HostMsg::PairReveal`], and both show the
//!    approval code. The host's user compares the codes and allows or denies the client. Then the
//!    host sends the `Welcome` or `Reject(Denied)`.
//! 5. With UDP media, the client repeats [`ClientDatagram::Hello`] until the host answers
//!    [`HostMsg::MediaReady`] over TCP. The datagram tells the host the client's UDP address.
//!    With TCP media the host sends `MediaReady` at once.
//! 6. The host sends [`HostMsg::DecoderConfig`] and then frames. The first frame is a key frame.

use crate::tracking::{Body, Eyes, Face, Features, Hands};
use crate::wire::{Codec, Fov, Motion, Nanos, Pose};
use crate::Error;
use cranvr_common::foveation::FoveationConfig;
use serde::{Deserialize, Serialize};

/// The first four bytes of both preambles.
pub const MAGIC: [u8; 4] = *b"CRNV";

/// The client's first bytes on the TCP connection: the magic and its protocol version.
pub const CLIENT_PREAMBLE_LEN: usize = 6;
/// The host's answer: the magic, the host's protocol version and whether the host takes a link
/// over a USB cable.
pub const HOST_PREAMBLE_LEN: usize = 7;
/// The start of the host's answer: the magic and the version. A host of another version may answer
/// with another length, so a client reads this much first. It reads the rest only from a host of
/// its own version.
pub const HOST_VERSION_LEN: usize = 6;

pub fn client_preamble(version: u16) -> [u8; CLIENT_PREAMBLE_LEN] {
    let mut out = [0; CLIENT_PREAMBLE_LEN];
    out[..4].copy_from_slice(&MAGIC);
    out[4..6].copy_from_slice(&version.to_le_bytes());
    out
}

/// The client's protocol version
pub fn parse_client_preamble(bytes: &[u8; CLIENT_PREAMBLE_LEN]) -> Result<u16, Error> {
    if bytes[..4] != MAGIC {
        return Err(Error::Malformed("not a cranvr client"));
    }
    Ok(u16::from_le_bytes([bytes[4], bytes[5]]))
}

/// The host's answer to a client's preamble. `cable` is false when the host's user chose Wi-Fi:
/// a headset on the Mac's USB cable then connects over Wi-Fi.
pub fn host_preamble(version: u16, cable: bool) -> [u8; HOST_PREAMBLE_LEN] {
    let mut out = [0; HOST_PREAMBLE_LEN];
    out[..4].copy_from_slice(&MAGIC);
    out[4..6].copy_from_slice(&version.to_le_bytes());
    out[6] = u8::from(cable);
    out
}

/// The host's protocol version, from the first [`HOST_VERSION_LEN`] bytes of its preamble
pub fn parse_host_preamble(bytes: &[u8]) -> Result<u16, Error> {
    if bytes.len() < HOST_VERSION_LEN || bytes[..4] != MAGIC {
        return Err(Error::Malformed("not a cranvr host"));
    }
    Ok(u16::from_le_bytes([bytes[4], bytes[5]]))
}

/// Whether the host takes a link over a USB cable, from the whole preamble of a host of this version
pub fn host_takes_cable(bytes: &[u8; HOST_PREAMBLE_LEN]) -> Result<bool, Error> {
    match bytes[HOST_VERSION_LEN] {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(Error::Malformed("the host's answer about the cable is neither yes nor no")),
    }
}

/// Whether the other side must update when two sides of different versions cannot talk: the side
/// with the lower version.
pub fn peer_must_update(ours: u16, theirs: u16) -> bool {
    theirs < ours
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
    /// The link runs over a USB cable: `adb reverse` or the headset's USB network to the Mac. The
    /// host then sends at the cable's bitrate.
    pub cable: bool,
    /// The client can record its microphone for the host's games (see [`HostMsg::Microphone`])
    pub microphone: bool,
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
    /// A number that the client repeats in its UDP hello. The hello is sealed, so only a client
    /// that finished the handshake can send it.
    pub session: u64,
    pub stream: StreamParams,
    pub media: MediaPlan,
    /// The payload bytes in each media packet. All data and parity packets of a unit have it.
    pub shard_len: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RejectReason {
    /// Another headset is connected, or another headset waits for the host's user
    Busy,
    NoCommonCodec,
    NoCommonRefreshRate,
    Other(String),
    /// The host's user pressed Deny
    Denied,
    /// The host's user did not answer in time
    NoAnswer,
    /// The client came over a USB cable, and the host's user chose Wi-Fi. The client connects
    /// over Wi-Fi.
    WiFiOnly,
}

/// How long a host holds a connection for its user's answer. A client waits a little longer.
pub const APPROVAL_WAIT: std::time::Duration = std::time::Duration::from_secs(180);

/// What a client tells its user when the host's cranvr has another version than its own and is the
/// older one: the user updates the Mac. The words are the same in every place that says it.
pub const UPDATE_MAC: &str = "Update cranvr on the Mac.";
/// What a host tells its user when the headset's cranvr app is the older one.
pub const UPDATE_HEADSET: &str = "Update the cranvr app on the headset.";

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
    /// recentering.
    Recenter(Recenter),
    /// The height of the floor in the headset's room in meters, from the headset's stage. It is
    /// negative when the floor lies below the room's origin, which is where the head was when the
    /// app started. The client sends it again when it changes. The host puts its scenes' floor
    /// there.
    Floor(f32),
    /// A press on a button of the console, as a JSON object (`{"action": "play", "game": "Beat
    /// Saber"}`, `{"action": "stop"}`, `{"action": "set", "fps": 90}`). The host hands it to the
    /// cranvr app as a `vr_request` status line.
    Console(String),
    /// The client's half of the approval code (see [`crate::secure::approval_code`]): a random
    /// number that it sends after it saw the host's [`HostMsg::AwaitApproval`].
    PairNonce([u8; 32]),
    /// The tracking features that the headset offers: hands, body, face and eyes (see
    /// [`crate::tracking`]). The client sends it once after the welcome and again when the set
    /// changes, for example when the user allows the headset's face tracking.
    TrackingOffer(Features),
    /// How much of the headset the app has (see [`Focus`]). The client sends it when the state
    /// changes, and once after the welcome when the app starts without the input focus. A client
    /// that never sends it is focused.
    Focus(Focus),
    /// The client adds a display to the Mac or removes one it added (see [`DisplayRequest`]). The
    /// host answers with [`HostMsg::Displays`].
    Display(DisplayRequest),
    /// What the client asks of the video of one display: the stream of its panel, how wide a picture
    /// and how many a second. A display that the client does not wish costs the Mac and the headset
    /// nothing: the host runs no capture and no encoder and sends nothing for it.
    DisplayWish { stream: u8, wish: DesktopWish },
}

/// How much of the headset the app has. The values follow the states of an OpenXR session and
/// Meta's rules for them: an app that loses the input focus keeps drawing and takes no input, and an
/// app that the system pauses stops its interactive parts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Focus {
    /// The app has the input focus. It sends tracking and input, and the host streams.
    #[default]
    Focused,
    /// The app is on display and has no input focus, as under the system's menu or beside another
    /// app's panel. The client sends no tracking and no input and keeps drawing the last picture.
    /// The host pauses the stream.
    Visible,
    /// The headset shows none of the app, as with its display off or while another immersive app
    /// runs. The client sends no tracking and no input. The host pauses the stream.
    Hidden,
}

impl Focus {
    /// Whether the app takes part: it sends its tracking and input and gets the stream.
    pub fn is_focused(self) -> bool {
        self == Focus::Focused
    }
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
    /// The console's menu as a JSON object: the games, each with its VR system (`vr`), and the settings
    /// that the cranvr app offers (its `vr-menu.json`) and the stream's state as `status`. The host sends it
    /// when the client connects and again when it changes, at most once a second.
    Menu(String),
    /// The host does not know this client. It holds the connection until its user allows or denies
    /// the client. `commit` is the hash of a random number that the host keeps secret until the
    /// client answered with [`ClientMsg::PairNonce`]. The host then sends the number in
    /// [`HostMsg::PairReveal`], and both sides compute the approval code from both numbers. A
    /// client that is in the middle of two connections cannot make the two codes equal: it has
    /// to fix its number on one connection before it sees the number of the other.
    AwaitApproval { host_name: String, commit: [u8; 32] },
    /// The number that `commit` hides
    PairReveal([u8; 32]),
    /// The tracking features that a game uses now. The client sends the datagrams of these features
    /// and no others. The set is empty when no game asks. The host sends it when the headset
    /// connects and when it changes.
    TrackingWish(Features),
    /// The displays that the client can show, and whether it can add one. The host sends the list
    /// when the client connects and after each change.
    Displays(DisplayList),
    /// The state of the video of a display: its parameters, or `None` while the host runs none for
    /// this client. The host sends it after a wish and again when the picture's size changes. The
    /// decoder's parameter sets follow in a [`HostMsg::DecoderConfig`] of the same stream.
    DisplayStream { stream: u8, params: Option<DesktopParams> },
    /// Whether a game runs on the host: it sends frames or it loads. The host sends it when the
    /// client connects and when it changes. The host hides no display for a game: a display that
    /// the client wishes goes on.
    GameRuns(bool),
    /// Whether a game records from the client's microphone. The client records while it is true
    /// and sends [`ClientDatagram::Microphone`]. A client that does not record costs nothing. The
    /// host sends it when the client connects and when it changes.
    Microphone(bool),
    /// The host's user asks the headset to open its hand menu: a button of the cranvr app on the Mac.
    /// A user whose controllers do not reach the menu opens it this way.
    OpenMenu,
}

/// Datagram messages from the client to the host.
// A tracking sample is the large variant. A value lives for one datagram, and a box would allocate
// on the path that sends a sample for every headset frame.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClientDatagram {
    /// The first datagram on the UDP path
    Hello { session: u64 },
    Tracking(Tracking),
    FrameTiming(FrameTiming),
    Loss(LossReport),
    Ping(Ping),
    Pong(Pong),
    /// The hands' joints, for every headset frame while the host wishes [`Features::HANDS`]
    Hands(Hands),
    /// The body's joints, while the host wishes [`Features::BODY`] or [`Features::FULL_BODY`]
    Body(Body),
    /// The face's blend shape weights, while the host wishes [`Features::FACE`]
    Face(Face),
    /// The gaze, while the host wishes [`Features::EYES`]
    Eyes(Eyes),
    /// The mouse on the panel of a display: where the controller that owns the Mac's mouse points,
    /// with the mouse buttons that it holds and its thumbstick's lean. The stream names the display.
    DisplayMouse { stream: u8, mouse: Mouse },
    /// The controllers' input with the hands that the app holds back from the game (see
    /// [`InputHold`]). The client sends it for every headset frame that has input.
    InputHold(InputHold),
    /// Sound of the client's microphone, while the host asks for it with [`HostMsg::Microphone`]
    Microphone(Microphone),
}

/// Datagram messages from the host to the client.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HostDatagram {
    Haptics(Haptics),
    Audio(Audio),
    Ping(Ping),
    Pong(Pong),
    /// Where the Mac's cursor is on a display. The stream names it.
    DisplayCursor { stream: u8, cursor: Cursor },
}

/// The head and the controllers at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tracking {
    /// When the headset sampled the poses, on the client's clock. Frames come back with this
    /// time, so it names the sample.
    pub sample_time: Nanos,
    /// When the compositor shows the frame that the poses are for, on the client's clock. The
    /// poses are predictions for this time. The times of successive samples make the host's model
    /// of the client's display grid.
    pub display_time: Nanos,
    pub head: Motion,
    /// The left hand's grip pose: the place of the hand that holds the controller. A game puts the
    /// controller's model there. `None` while the headset does not track the controller.
    pub left_grip: Option<Motion>,
    /// The right hand's grip pose
    pub right_grip: Option<Motion>,
    /// The left controller's aim pose: where it points. A game casts its pointing ray from it. The
    /// headset reports it next to the grip pose, so a hand with a grip pose has an aim pose too. A
    /// client that has none leaves it `None`, and the host then gives a game the grip pose for both.
    /// Both poses are the headset's own, with no offset.
    pub left_aim: Option<Motion>,
    /// The right controller's aim pose
    pub right_aim: Option<Motion>,
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
    /// The compositor took the frame for its display. A frame that came later waited for the next
    /// display time.
    pub picked: Nanos,
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

/// The format of the client's microphone in [`Microphone`]: one channel at 48 kHz, 16 bits.
pub const MICROPHONE_FORMAT: AudioFormat = AudioFormat { sample_rate: 48_000, channels: 1, sample: SampleFormat::S16Le };

/// The frames in one [`Microphone`] datagram: 10 ms, 960 bytes.
pub const MICROPHONE_FRAMES: usize = 480;

const _: () = assert!(MICROPHONE_FRAMES * 2 <= MAX_AUDIO_BYTES);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Microphone {
    /// Counts up by one per datagram, so the host sees gaps
    pub sequence: u32,
    /// When the microphone took the first frame, on the client's clock. A client whose platform
    /// gives no capture time sends the time at which the frame reached the app.
    pub captured: Nanos,
    /// [`MICROPHONE_FRAMES`] frames of [`MICROPHONE_FORMAT`]
    pub pcm: Vec<u8>,
}

/// The id of the stream that carries the first display of the Mac. The eye stream is 0.
pub const DESKTOP_STREAM: u8 = 1;

/// What the client asks of the desktop stream.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DesktopWish {
    /// Whether the client shows the desktop. `false` stops the capture.
    pub enabled: bool,
    /// The widest picture in pixels that the client's decoder takes and the panel can use. The host
    /// takes the display's own pixel width when that is smaller.
    pub max_width: u32,
    /// The most pictures a second that the client wants
    pub max_fps: u8,
}

/// The video of a display as the host runs it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DesktopParams {
    /// The display's stream (see [`display_stream`])
    pub stream: u8,
    pub codec: Codec,
    /// The size of the pictures in pixels: the display's full pixel size, or the client's
    /// `max_width` with the display's shape
    pub size: [u32; 2],
    /// The display's size in points, which the cursor's position and the pointer refer to
    pub points: [f32; 2],
    pub max_fps: f32,
    pub bitrate_bps: u64,
    /// The encoder uses the full 0 to 255 range
    pub full_range: bool,
    /// The host's pictures are mirrored top to bottom, because the client's compositor reads a
    /// decoder's surface bottom row first. Cranpose's compositor does not: the host sends the
    /// pictures upright and this is `false`.
    pub flipped: bool,
    /// The Mac's cursor is part of the pictures. The client then draws none of its own. A client's
    /// panel cannot show a cursor over a surface panel today: the compositor draws surfaces over
    /// panels.
    pub cursor_in_video: bool,
}

/// The most displays that a client shows at once: the first display, which the host captures from
/// the start, and the ones that the client adds.
pub const MAX_DISPLAYS: u8 = 4;

/// The stream of the display with this index. Index 0 is the first display, [`DESKTOP_STREAM`].
pub const fn display_stream(index: u8) -> u8 {
    DESKTOP_STREAM + index
}

/// The index of the display that a stream carries, or `None` for a stream that is no display's.
pub const fn display_index(stream: u8) -> Option<u8> {
    if stream >= DESKTOP_STREAM && stream < DESKTOP_STREAM + MAX_DISPLAYS {
        Some(stream - DESKTOP_STREAM)
    } else {
        None
    }
}

/// What the client asks of the Mac's displays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisplayRequest {
    /// The Mac makes one more display, which only the headset shows. The first one that the host
    /// captures from the start stays.
    Add,
    /// The Mac removes the display that carries this stream, if the client added it.
    Remove { stream: u8 },
}

/// A display that the client can show as a panel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayInfo {
    /// The stream that carries its video. It names the display in every message.
    pub stream: u8,
    /// The words for the headset's menu, such as "Mac display" or "Added display 2"
    pub name: String,
    /// The display's size in points
    pub points: [u32; 2],
    /// The client can remove it with [`DisplayRequest::Remove`]: the host made it for the client.
    pub removable: bool,
}

/// The displays of the host, in the order of their streams.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DisplayList {
    pub displays: Vec<DisplayInfo>,
    /// The host can add one more display now
    pub can_add: bool,
    /// One line of plain words about a request that failed, such as an add that macOS refused. It
    /// stays until the next list.
    pub notice: Option<String>,
}

/// The controllers' input and the hands that the app holds back from the game. A hand points at a
/// panel of the app, or holds a press that began on one: its buttons belong to the panel, so the
/// game must not see them. The input and the mask travel in one datagram, so no input of a held
/// hand reaches the game before the mask does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InputHold {
    pub input: Input,
    /// Left, then right. The host gives the game rest values in place of the held hand's buttons,
    /// touches, trigger, grip and thumbstick. The hand's poses stay.
    pub held: [bool; 2],
}

/// Where the controller that owns the Mac's mouse points on the desktop panel. The Mac has one
/// mouse, so the client names one controller at a time: the one that holds a button, else the one
/// that pressed last. The client sends the datagram for every headset frame while it changes and
/// every 50 ms otherwise, so a lost datagram heals.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mouse {
    /// Counts up by one per datagram. The host drops a datagram that is older than the newest.
    pub seq: u32,
    /// When the client sampled the controller, on the client's clock
    pub sent: Nanos,
    /// The point on the panel as u and v from its top left corner (0, 0) to its bottom right corner
    /// (1, 1), or `None` while no controller points at the panel
    pub hit: Option<[f32; 2]>,
    /// The mouse buttons that are down, as the bits of `cranvr_common::controls::mouse`: 1 for the
    /// left, 2 for the right and 4 for the middle button. The client maps the controller's buttons
    /// to them (see `cranvr_common::controls`), so the host needs no map. A bit stays set for as
    /// long as the button is down: the host presses on the first datagram with the bit and lets go
    /// on the first datagram without it.
    pub buttons: u8,
    /// How far the thumbstick leans, each axis from -1 to 1: across to the right, then forward. The
    /// host scrolls at a speed that follows the lean. An axis that the user mapped to nothing is 0.
    pub scroll: [f32; 2],
}

/// Where the Mac's cursor lies on the desktop. The newest datagram wins.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cursor {
    pub seq: u32,
    /// The position as u and v from the display's top left corner (0, 0) to its bottom right
    /// corner (1, 1)
    pub u: f32,
    pub v: f32,
    /// Whether the cursor lies on the desktop's display
    pub on: bool,
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
    // The full-density region goes where the headset's eye looks straight ahead
    let fov = caps.views.fov[0];
    let foveation = wishes
        .foveation
        .filter(|_| caps.foveation)
        .map(|f| FoveationConfig::from(f).centered([fov.left.tan(), fov.right.tan(), fov.up.tan(), fov.down.tan()]).into());
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
