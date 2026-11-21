//! What the client asks of the host for each display it shows, and what the menu says of its cost.
//!
//! A display that is not shown is not asked for: the host runs no capture and no encoder for it, and
//! the headset runs no decoder. A display that is shown costs the Mac's encoder and the headset's
//! decoder. The decoder also decodes the game's picture, and the pictures of all displays share the
//! media path with it, so a display asks for less while a game runs, and for less again when it
//! rides on the hand, where it is small.

use cranvr_proto::message::DesktopWish;

/// Where a shown display stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// A panel in the room
    Room,
    /// The small panel on the left hand, `width_mm` wide
    Hand { width_mm: u32 },
}

/// The headset's pixels a degree in the middle of the view: the Quest 3 shows about 25
const PIXELS_PER_DEGREE: f32 = 25.0;
/// The nearest distance from the eyes at which a user holds the hand's display to read it, in meters
const READING_DISTANCE: f32 = 0.35;
/// The widths that the hand's display asks for. A resize starts a new capture only when it crosses a
/// step.
const HAND_STEPS: [u32; 3] = [1920, 2560, 3840];

/// The picture's width that the hand's display needs when it is `width_mm` wide: the headset's pixels
/// across the panel at the reading distance, rounded up to a step. The panel of 42 cm that the hand
/// starts with spans 62 degrees there, about 1550 of the headset's pixels, and gets 1920. A panel wider
/// than 55 cm gets 2560.
pub fn hand_width(width_mm: u32) -> u32 {
    let half = width_mm as f32 / 2000.0;
    let pixels = (2.0 * (half / READING_DISTANCE).atan().to_degrees() * PIXELS_PER_DEGREE).ceil() as u32;
    HAND_STEPS.into_iter().find(|&step| step >= pixels).unwrap_or(HAND_STEPS[HAND_STEPS.len() - 1])
}

/// The widest picture and the most pictures a second for a display.
///
/// - One display in the room, no game: the display's own size at up to 60 a second, as before.
/// - Two in the room, no game: the same size at 30 a second each: the decoder takes a 4K picture at
///   60 a second once (see `desktop.rs`), and a still desktop needs few pictures.
/// - Three or four in the room, no game: 2560 pixels across at 30.
/// - During a game: 2560 pixels across at 30 for a panel in the room. More than two displays in a game
///   take 1920 at 30 each.
/// - The hand's panel: the width that its size needs (see [`hand_width`]) at 30, in a game or not.
///
/// `decoder_width` is the widest picture that the headset's decoder takes, when it says.
pub fn wish_for(place: Place, in_game: bool, shown: usize, decoder_width: Option<u32>) -> DesktopWish {
    let (width, fps) = match (in_game, place, shown) {
        (_, Place::Hand { width_mm }, _) => (hand_width(width_mm), 30),
        (true, Place::Room, 0..=2) => (2560, 30),
        (true, Place::Room, _) => (1920, 30),
        (false, Place::Room, 0..=1) => (3840, 60),
        (false, Place::Room, 2) => (3840, 30),
        (false, Place::Room, _) => (2560, 30),
    };
    DesktopWish { enabled: true, max_width: decoder_width.map_or(width, |limit| width.min(limit)), max_fps: fps }
}

/// What the menu knows of a display's stream, from the client's own counts.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Measured {
    /// The picture's size in pixels, once the host sent pictures
    pub size: Option<[u32; 2]>,
    /// Pictures that arrived and that the decoder gave, per second
    pub received_fps: f32,
    pub decoded_fps: f32,
    pub mbps: f32,
    pub decode_ms: f32,
}

/// The lines under a display in the menu: what the display costs now. Each line is short enough for the
/// menu's width. A hidden display costs nothing. A shown display costs the Mac's encoder and the
/// headset's decoder, which the second line names with the numbers.
pub fn cost_lines(shown: bool, measured: &Measured, wish: Option<&DesktopWish>) -> Vec<String> {
    if !shown {
        return vec!["Hidden: it costs nothing".to_string()];
    }
    let Some([width, height]) = measured.size else {
        return vec!["Starting: the Mac sends the first picture".to_string()];
    };
    let limit = wish.filter(|w| w.max_fps < 60).map_or(String::new(), |w| format!(", at most {}", w.max_fps));
    vec![
        format!("{width} x {height} at {:.0} a second{limit}", measured.decoded_fps),
        format!("{:.1} Mbps, decode {:.1} ms; the Mac encodes it", measured.mbps, measured.decode_ms),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_display_in_the_room_gets_the_full_size_and_rate() {
        let w = wish_for(Place::Room, false, 1, None);
        assert_eq!((w.enabled, w.max_width, w.max_fps), (true, 3840, 60));
    }

    #[test]
    fn more_displays_in_the_room_ask_for_less_each() {
        assert_eq!(wish_for(Place::Room, false, 2, None).max_fps, 30);
        assert_eq!(wish_for(Place::Room, false, 3, None).max_width, 2560);
        // The width and the rate that all the displays ask for together never grow with the count
        let rate = |n: usize| {
            let w = wish_for(Place::Room, false, n, None);
            u64::from(w.max_width) * u64::from(w.max_fps) * n as u64
        };
        assert!(rate(2) <= rate(1) && rate(3) <= rate(1) && rate(4) <= rate(1) + 3 * 1920 * 30 / 2);
    }

    #[test]
    fn a_game_gets_a_smaller_and_slower_display_and_the_hand_a_smaller_one_still() {
        let room = wish_for(Place::Room, true, 1, None);
        let hand = wish_for(Place::Hand { width_mm: 420 }, true, 1, None);
        let free = wish_for(Place::Room, false, 1, None);
        assert!(room.max_width < free.max_width && room.max_fps < free.max_fps);
        assert!(hand.max_width < room.max_width);
        assert_eq!((hand.max_width, hand.max_fps), (1920, 30));
        // The hand's display asks the same in the room: it is small there as well
        assert_eq!(wish_for(Place::Hand { width_mm: 420 }, false, 1, None), hand);
    }

    #[test]
    fn a_decoder_that_takes_less_limits_the_width() {
        assert_eq!(wish_for(Place::Room, false, 1, Some(2560)).max_width, 2560);
        assert_eq!(wish_for(Place::Hand { width_mm: 420 }, false, 1, Some(4096)).max_width, 1920);
        assert_eq!(wish_for(Place::Hand { width_mm: 700 }, false, 1, Some(2048)).max_width, 2048);
    }

    #[test]
    fn the_hand_asks_for_the_pixels_that_its_size_needs() {
        // The hand's display is 28 to 70 cm wide (`display_panel.rs`)
        assert_eq!(hand_width(280), 1920);
        assert_eq!(hand_width(420), 1920);
        assert_eq!(hand_width(550), 1920);
        assert_eq!(hand_width(560), 2560);
        assert_eq!(hand_width(700), 2560);
        // A wider panel never asks for less
        let widths: Vec<u32> = (280..=700).step_by(10).map(hand_width).collect();
        assert!(widths.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn the_cost_lines_say_what_a_shown_display_costs_and_that_a_hidden_one_costs_nothing() {
        assert_eq!(cost_lines(false, &Measured::default(), None), vec!["Hidden: it costs nothing"]);
        assert!(cost_lines(true, &Measured::default(), None)[0].starts_with("Starting"));
        let measured = Measured { size: Some([3840, 2160]), received_fps: 31.0, decoded_fps: 30.0, mbps: 6.2, decode_ms: 2.04 };
        let lines = cost_lines(true, &measured, Some(&DesktopWish { enabled: true, max_width: 2560, max_fps: 30 }));
        assert_eq!(lines, vec!["3840 x 2160 at 30 a second, at most 30", "6.2 Mbps, decode 2.0 ms; the Mac encodes it"]);
        let lines = cost_lines(true, &measured, Some(&DesktopWish { enabled: true, max_width: 3840, max_fps: 60 }));
        assert!(!lines[0].contains("at most"));
        // Each line fits the menu: it holds about 40 characters at 20 mm
        assert!(lines.iter().chain(cost_lines(true, &Measured::default(), None).iter()).all(|line| line.len() <= 44), "{lines:?}");
    }
}
