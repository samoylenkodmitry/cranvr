//! The eye size that a game renders, which the host writes into the runtime's control file.
//!
//! A game reads its view size once, when it starts. A headset links later or earlier. At the link
//! the host writes the stream's eye at the game render size. Before any link the host writes the
//! eye that a Quest 3 would get from the host's wishes, through the same negotiation, so a game
//! that starts before a Quest 3 links renders at the size of the stream that comes (#170). A
//! headset with another eye changes the size at its link. A game that runs then keeps the size of
//! its start, and the host tells the app so.

use cranvr_proto::{
    message::{negotiate, ClientCaps, HostWishes, Views},
    wire::{Codec, Fov, Pose},
};

/// What the cranvr client on a Quest 3 tells the host in its Hello. Its runtime recommends an eye
/// of 1680x1760, and its decoder takes the frame of the weakest foveation level at that size at
/// 120 Hz, so the client asks for it (client/src/sizing.rs). Its refresh rates and its field of
/// view are those that Horizon OS reports. Only the eye, the rates, the codecs, the foveation and
/// the field of view count in the negotiation.
pub fn quest_3_caps() -> ClientCaps {
    let degrees = |d: f32| d.to_radians();
    let left = Fov { left: degrees(-54.0), right: degrees(40.0), up: degrees(44.0), down: degrees(-55.0) };
    let eye = |x: f32| Pose { position: [x, 0.0, 0.0], ..Pose::IDENTITY };
    ClientCaps {
        name: "Meta Quest 3".into(),
        eye_size: [1680, 1760],
        views: Views { fov: [left, Fov { left: -left.right, right: -left.left, ..left }], eye_pose: [eye(-0.0315), eye(0.0315)] },
        refresh_rates: vec![72.0, 80.0, 90.0, 120.0],
        codecs: vec![Codec::Hevc],
        decoders: 1,
        foveation: true,
        tcp_only: false,
        cable: true,
        microphone: true,
    }
}

/// The eye that a game renders for a stream's eye: the game render size's share of it, as the
/// runtime takes it (even, and within its limits).
pub fn scaled(eye: [u32; 2], game_scale: f32) -> (u32, u32) {
    let scale = |v: u32| (v as f32 * game_scale).round() as u32;
    crate::runtime_link::view_size((scale(eye[0]), scale(eye[1])))
}

/// The eye and the refresh rate for a game that starts before any headset links: those of the
/// stream that a Quest 3 would get from `wishes`, at the game render size. `None` when a Quest 3
/// would get no stream, such as from a host that encodes no codec of the Quest's.
pub fn before_link(wishes: &HostWishes, game_scale: f32) -> Option<((u32, u32), u32)> {
    let stream = negotiate(wishes, &quest_3_caps()).ok()?;
    Some((scaled(stream.eye_size, game_scale), stream.refresh_hz.round() as u32))
}

/// The words for a game that keeps the eye of its start while a new link gives another one.
pub fn kept_message(kept: (u32, u32), now: (u32, u32)) -> String {
    format!(
        "The running game keeps the eye size of its start, {}x{}. The stream's eye at the game render size is {}x{} now. Start the game again for the new size.",
        kept.0, kept.1, now.0, now.1
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranvr_common::foveation::FoveationLevel;

    /// The host's wishes for these settings, as `cranvr-host --fps FPS --foveation LEVEL` makes them
    fn wishes(fps: f32, foveation: FoveationLevel) -> HostWishes {
        crate::wishes(&crate::options_for(fps, foveation, 0))
    }

    #[test]
    fn a_game_that_starts_before_the_link_gets_a_quest_3_stream_at_the_game_render_size() {
        // (foveation, rate, game render size) and the eye that the game gets
        let cases = [
            (FoveationLevel::Off, 120.0, 1.0, (1248, 1306)),
            (FoveationLevel::Off, 120.0, 0.7, (874, 914)),
            (FoveationLevel::Off, 90.0, 0.7, (1052, 1102)),
            (FoveationLevel::Off, 72.0, 0.5, (864, 904)),
            (FoveationLevel::Low, 120.0, 1.0, (1680, 1760)),
            (FoveationLevel::Medium, 120.0, 0.7, (1176, 1232)),
            (FoveationLevel::High, 120.0, 0.3, (504, 528)),
        ];
        for (level, fps, scale, eye) in cases {
            let (got, rate) = before_link(&wishes(fps, level), scale).unwrap();
            assert_eq!((got, rate), (eye, fps as u32), "{level:?} at {fps} Hz and {scale}");
        }
    }

    #[test]
    fn the_start_size_is_no_longer_a_whole_quest_3_eye() {
        // The owner's settings on 2026-11-27: 120 Hz, foveation Off, game render size 100%. The
        // stream's eye was 1248x1306 and the control file held 2064x2208 before the link.
        let (eye, _) = before_link(&wishes(120.0, FoveationLevel::Off), 1.0).unwrap();
        assert!(eye.0 * eye.1 < 2064 * 2208 / 2, "{eye:?}");
    }

    #[test]
    fn a_rate_between_the_quests_rates_takes_the_next_lower_one() {
        let (_, rate) = before_link(&wishes(100.0, FoveationLevel::Medium), 0.7).unwrap();
        assert_eq!(rate, 90);
    }

    #[test]
    fn a_width_on_the_command_line_counts_before_the_link_too() {
        let options = crate::options_for(120.0, FoveationLevel::Medium, 1600);
        let (eye, _) = before_link(&crate::wishes(&options), 1.0).unwrap();
        assert_eq!(eye.0, 1600);
    }

    #[test]
    fn the_message_names_both_sizes() {
        let text = kept_message((1176, 1232), (874, 914));
        assert!(text.contains("1176x1232") && text.contains("874x914"), "{text}");
    }
}
