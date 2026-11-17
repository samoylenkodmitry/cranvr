//! ALVR keeps its settings in session.json. The host writes the few settings it depends on before
//! the server core starts, and reads back what the server agreed on with the headset.

use alvr_filesystem::Layout;
use alvr_packets::ClientListAction;
use alvr_server_io::ServerSessionManager;
use alvr_session::{
    BitrateModeDefaultVariant, CodecTypeDefaultVariant, FrameSizeDefaultVariant, MediacodecPropTypeDefault,
    MediacodecPropTypeDefaultVariant, MediacodecPropertyDefault, SocketProtocolDefaultVariant,
};
use crate::headset::Protocol;
use cranvr_common::foveation::FoveationConfig;
use cranvr_proto::wire::Codec;
use std::{fs, net::IpAddr, path::Path};

pub struct Options {
    /// How the host talks to the headset's app
    pub protocol: Protocol,
    /// The TCP port of the native protocol
    pub native_port: u16,
    pub fps: f32,
    pub eye_width: u32,
    /// The eye width for a native client without foveation, when the user gave no width. 0
    /// means none.
    pub eye_width_unfoveated: u32,
    pub mbps: u64,
    pub trust: Vec<String>,
    pub trust_all: bool,
    /// With the native protocol: the pairing code of the headset to pair (see `pairing.rs`)
    pub pair_code: Option<u32>,
    /// With the native protocol: plan the host's own scene's frames for the headset's display
    pub display_lock: bool,
    /// Clients the host connects to at a fixed address, such as a phone through `adb forward`
    pub client_ips: Vec<IpAddr>,
    pub tcp: bool,
    pub foveation: bool,
    /// Frames the client may hold before it decodes; ALVR's default is 2
    pub buffering_frames: f32,
    /// A game renders each eye at this share of the stream's eye size. Foveated encoding sends
    /// about a third of the eye's pixels, and a base M5 drew Beat Saber at 37 to 41 frames per
    /// second at the full size.
    pub game_scale: f32,
    /// Asks the headset's decoder for its low-latency mode (Android 11 and newer: Quest 3)
    pub decoder_low_latency: bool,
    /// VideoToolbox's low-latency rate control; see `cranvr_vt::EncoderConfig::strict_rate`
    pub strict_rate: bool,
    /// The Mac's sound plays in the headset (see game_audio.rs)
    pub game_audio: bool,
    /// Scaled displays get their panels' own pixels while a game runs (see display_modes.rs)
    pub unscale_displays: bool,
}

pub fn prepare(layout: &Layout, options: &Options) {
    let mut manager = ServerSessionManager::new(Some(layout.session()));
    {
        let mut session = manager.session_mut();
        let settings = &mut session.session_settings;

        let video = &mut settings.video;
        video.preferred_fps = options.fps;
        video.preferred_codec.variant = CodecTypeDefaultVariant::Hevc;
        // ALVR's default foveation parameters; the client unwarps with the same ones
        video.foveated_encoding.enabled = options.foveation;
        for size in [&mut video.transcoding_view_resolution, &mut video.emulated_headset_view_resolution] {
            size.variant = FrameSizeDefaultVariant::Absolute;
            size.Absolute.width = options.eye_width;
            size.Absolute.height.set = false;
        }
        video.max_buffering_frames = options.buffering_frames;
        // ALVR leaves this key out: Android 10 phones crash with it. A Quest 3 runs Android 12.
        let options_list = &mut video.mediacodec_extra_options.content;
        options_list.retain(|(key, _)| key != "low-latency");
        if options.decoder_low_latency {
            options_list.push((
                "low-latency".into(),
                MediacodecPropertyDefault {
                    ty: MediacodecPropTypeDefault { variant: MediacodecPropTypeDefaultVariant::Int32 },
                    value: "1".into(),
                },
            ));
        }
        video.bitrate.mode.variant = BitrateModeDefaultVariant::ConstantMbps;
        video.bitrate.mode.ConstantMbps = options.mbps;

        settings.audio.game_audio.enabled = options.game_audio;
        // The headset's microphone needs a virtual input device on this Mac, which the host does
        // not set up yet
        settings.audio.microphone.enabled = false;

        settings.connection.stream_protocol.variant =
            if options.tcp { SocketProtocolDefaultVariant::Tcp } else { SocketProtocolDefaultVariant::Udp };
        settings.connection.client_discovery.enabled = true;
        settings.connection.client_discovery.content.auto_trust_clients = options.trust_all;
    }
    for ip in &options.client_ips {
        let hostname = format!("{ip}.manual");
        manager.update_client_list(hostname.clone(), ClientListAction::AddIfMissing { trusted: true, manual_ips: vec![*ip] });
        manager.update_client_list(hostname.clone(), ClientListAction::SetManualIps(vec![*ip]));
        manager.update_client_list(hostname, ClientListAction::Trust);
    }
    for hostname in &options.trust {
        manager.update_client_list(hostname.clone(), ClientListAction::AddIfMissing { trusted: true, manual_ips: vec![] });
        manager.update_client_list(hostname.clone(), ClientListAction::Trust);
    }
}

/// What the server agreed on with the headset for the current connection.
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

pub fn read_negotiated(session_path: &Path) -> Option<Negotiated> {
    let text = fs::read_to_string(session_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let c = json.get("openvr_config")?;
    Some(Negotiated {
        eye_width: c.get("eye_resolution_width")?.as_u64()? as u32,
        eye_height: c.get("eye_resolution_height")?.as_u64()? as u32,
        fps: c.get("refresh_rate")?.as_u64()? as f32,
        full_range: c.get("use_full_range_encoding")?.as_bool()?,
        // ALVR's numbering: 0 H.264, 1 HEVC, 2 AV1
        codec: match c.get("codec")?.as_u64()? {
            0 => Codec::H264,
            1 => Codec::Hevc,
            _ => Codec::Av1,
        },
        foveated: c.get("enable_foveated_encoding")?.as_bool()?,
        foveation: foveation_config(),
    })
}

/// The foveation parameters ALVR sends to the client with the stream config.
pub fn foveation_config() -> FoveationConfig {
    match alvr_server_core::settings().video.foveated_encoding.as_option() {
        Some(c) => FoveationConfig {
            center_size: [c.center_size_x, c.center_size_y],
            center_shift: [c.center_shift_x, c.center_shift_y],
            edge_ratio: [c.edge_ratio_x, c.edge_ratio_y],
        },
        None => FoveationConfig::default(),
    }
}
