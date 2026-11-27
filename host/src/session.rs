//! What the host starts with and what it agreed on with the headset.

use cranvr_common::foveation::{FoveationConfig, FoveationLevel};
use cranvr_proto::wire::Codec;

pub struct Options {
    /// The TCP port of the headset protocol
    pub native_port: u16,
    pub fps: f32,
    pub eye_width: u32,
    /// The eye width for a client without foveation, when the user gave no width. 0 means none.
    pub eye_width_unfoveated: u32,
    /// Whether the user gave `--eye-width`. `eye_width` holds the host's own choice otherwise.
    pub eye_width_given: bool,
    pub mbps: u64,
    /// Accept every client without a pairing (for tests)
    pub trust_all: bool,
    /// Listen on 127.0.0.1 alone, with no USB network listener and no discovery, for a test host
    /// that never asks macOS for Local Network access
    pub loopback: bool,
    /// Take a link over a USB cable. `--no-cable` turns it off for the Connection choice Wi-Fi.
    pub cable: bool,
    /// The UDP port that answers the headsets' discovery queries. `None` turns discovery off.
    pub discovery_port: Option<u16>,
    /// Plan the host's own scene's frames for the headset's display
    pub display_lock: bool,
    /// Send the media over TCP for every client, not only for a client that needs it
    pub tcp: bool,
    /// How strongly each eye is foveated: `--foveation`
    pub foveation: FoveationLevel,
    /// A game renders each eye at this share of the stream's eye size. Foveated encoding sends
    /// about a third of the eye's pixels, and a base M5 drew Beat Saber at 37 to 41 frames per
    /// second at the full size.
    pub game_scale: f32,
    /// VideoToolbox's low-latency rate control; see `cranvr_vt::EncoderConfig::strict_rate`
    pub strict_rate: bool,
    /// The Mac's sound plays in the headset (see game_audio.rs)
    pub game_audio: bool,
    /// Keep the ring of the headset's microphone for games (`headset/native/microphone.rs`)
    pub microphone: bool,
    /// Scaled displays get their panels' own pixels while a game runs (see display_modes.rs)
    pub unscale_displays: bool,
    /// A tracked hand makes the controller of a game that has no hand code, while the headset
    /// tracks no controller (see hand_controller.rs)
    pub hand_controllers: bool,
}

/// What the host agreed on with the headset for the current connection.
#[derive(Clone, Debug, PartialEq)]
pub struct Negotiated {
    pub eye_width: u32,
    pub eye_height: u32,
    pub fps: f32,
    pub full_range: bool,
    pub codec: Codec,
    pub foveated: bool,
    /// The foveation parameters, which count when `foveated` is set
    pub foveation: FoveationConfig,
}
