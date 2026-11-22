//! The hand menu's ball on an open palm, for a user with hands and no controllers.
//!
//! A tracked hand that turns its open palm to the eyes gets the ball 7 cm in front of the palm. The
//! other hand presses it: its pinch through Cranpose's hand pointer, or its controller's trigger. The
//! palm's own hand never presses it: Cranpose's pointer ignores the panels that ride on the same
//! hand. The right palm's pinch belongs to the system, which opens its own menu with it, so the
//! design never asks for it. The left palm's pinch reaches the app as the menu gesture
//! (`HandAim::menu_pressed`), and it opens the hand menu as the menu button does.
//!
//! The palm counts as turned to the eyes when its normal points within [`FACING_DEGREES`] of the
//! eyes and its index finger does not pinch. It must hold that for [`SHOW_AFTER`], and the ball goes
//! when the palm turns away for [`HIDE_AFTER`], so a flicker of the tracking does not blink it.
//!
//! The file has no Cranpose state, so a test drives it with plain values.

use cranpose_xr::Pose;
use cranvr_common::controls::Hand;
use std::time::Duration;

/// How far the palm's normal may point from the eyes
pub const FACING_DEGREES: f32 = 50.0;
/// How long a palm faces the eyes before the ball shows on it
pub const SHOW_AFTER: Duration = Duration::from_millis(200);
/// How long a palm faces away before the ball goes
pub const HIDE_AFTER: Duration = Duration::from_millis(300);
/// A pinch stronger than this closes the hand: the palm is no open palm
const PINCHING: f32 = 0.5;

/// Whether the palm joint at `palm` faces the eyes at `eyes`. A hand joint's +y points out of the
/// back of the hand, so the palm faces along -y.
pub fn faces(palm: &Pose, eyes: [f32; 3]) -> bool {
    let normal = palm.rotate([0.0, -1.0, 0.0]);
    let to_eyes = [0, 1, 2].map(|axis| eyes[axis] - palm.position[axis]);
    let length = to_eyes.iter().map(|v| v * v).sum::<f32>().sqrt();
    if length < f32::EPSILON {
        return false;
    }
    let cos: f32 = normal.iter().zip(to_eyes).map(|(n, t)| n * t / length).sum();
    cos >= FACING_DEGREES.to_radians().cos()
}

/// What the headset knows about one hand in a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PalmFrame {
    /// The palm joint's pose, while the cameras track the hand and the joint is valid
    pub palm: Option<Pose>,
    /// How far the index finger pinches, from 0 to 1
    pub pinch: f32,
    /// The hand makes the menu gesture: the left palm's pinch
    pub menu_pressed: bool,
}

/// Follows both palms from frame to frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct PalmWatch {
    /// Since when each palm differs from what the ball shows
    differs_since: [Option<Duration>; 2],
    shown: Option<Hand>,
    menu_down: bool,
}

/// What a frame changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PalmChange {
    /// The palm that carries the ball now, if any
    pub shown: Option<Hand>,
    /// The left palm's pinch went down in this frame: the menu opens or closes
    pub menu: bool,
}

impl PalmWatch {
    /// Reads one frame of both hands (left, right) with the eyes at `eyes`.
    pub fn update(&mut self, time: Duration, eyes: [f32; 3], hands: [PalmFrame; 2]) -> PalmChange {
        let facing = hands.map(|hand| hand.pinch < PINCHING && hand.palm.is_some_and(|palm| faces(&palm, eyes)));
        for hand in Hand::BOTH {
            let i = hand.index();
            let shows = self.shown == Some(hand);
            if facing[i] == shows || (facing[i] && self.shown.is_some()) {
                self.differs_since[i] = None;
                continue;
            }
            let since = *self.differs_since[i].get_or_insert(time);
            let wait = if facing[i] { SHOW_AFTER } else { HIDE_AFTER };
            if time.saturating_sub(since) >= wait {
                self.shown = facing[i].then_some(hand);
                self.differs_since = [None; 2];
            }
        }
        let menu_down = hands[Hand::Left.index()].menu_pressed;
        let menu = menu_down && !self.menu_down;
        self.menu_down = menu_down;
        PalmChange { shown: self.shown, menu }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EYES: [f32; 3] = [0.0; 3];
    /// A left palm 35 cm ahead of the eyes and 15 cm below them, its normal toward the viewer
    const TOWARD: Pose = Pose { position: [-0.1, -0.15, -0.35], orientation: [-std::f32::consts::FRAC_1_SQRT_2, 0.0, 0.0, std::f32::consts::FRAC_1_SQRT_2] };
    /// The same palm turned down, as a hand at rest
    const DOWN: Pose = Pose { position: [-0.1, -0.15, -0.35], orientation: [0.0, 0.0, 0.0, 1.0] };

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn open(palm: Pose) -> PalmFrame {
        PalmFrame { palm: Some(palm), ..PalmFrame::default() }
    }

    #[test]
    fn a_palm_faces_the_eyes_when_its_normal_points_at_them() {
        assert!(faces(&TOWARD, EYES));
        assert!(!faces(&DOWN, EYES));
        // The back of the hand toward the eyes is no palm
        let back = Pose { orientation: [std::f32::consts::FRAC_1_SQRT_2, 0.0, 0.0, std::f32::consts::FRAC_1_SQRT_2], ..TOWARD };
        assert!(!faces(&back, EYES));
    }

    #[test]
    fn the_ball_shows_on_a_palm_that_faces_the_eyes_for_a_moment_and_goes_when_it_turns_away() {
        let mut watch = PalmWatch::default();
        let none = PalmFrame::default();
        assert_eq!(watch.update(ms(0), EYES, [open(TOWARD), none]).shown, None);
        assert_eq!(watch.update(ms(199), EYES, [open(TOWARD), none]).shown, None);
        assert_eq!(watch.update(ms(200), EYES, [open(TOWARD), none]).shown, Some(Hand::Left));
        // A short turn away keeps it
        assert_eq!(watch.update(ms(300), EYES, [open(DOWN), none]).shown, Some(Hand::Left));
        assert_eq!(watch.update(ms(400), EYES, [open(TOWARD), none]).shown, Some(Hand::Left));
        // The right palm turns too: the ball stays on the left one
        let right = PalmFrame { palm: Some(Pose { position: [0.1, -0.15, -0.35], ..TOWARD }), ..PalmFrame::default() };
        assert_eq!(watch.update(ms(800), EYES, [open(TOWARD), right]).shown, Some(Hand::Left));
        // A pinch of the left hand closes it: it is no open palm, and the ball goes after the wait
        let fist = PalmFrame { pinch: 1.0, ..open(TOWARD) };
        assert_eq!(watch.update(ms(900), EYES, [fist, none]).shown, Some(Hand::Left));
        assert_eq!(watch.update(ms(1200), EYES, [fist, none]).shown, None);
        // A lost hand hides it the same way
        let mut watch = PalmWatch::default();
        watch.update(ms(0), EYES, [none, right]);
        assert_eq!(watch.update(ms(200), EYES, [none, right]).shown, Some(Hand::Right));
        assert_eq!(watch.update(ms(500), EYES, [none, none]).shown, Some(Hand::Right));
        assert_eq!(watch.update(ms(800), EYES, [none, none]).shown, None);
    }

    #[test]
    fn the_left_palms_pinch_opens_the_menu_once_for_each_press() {
        let mut watch = PalmWatch::default();
        let pressed = PalmFrame { menu_pressed: true, ..PalmFrame::default() };
        let none = PalmFrame::default();
        assert!(watch.update(ms(0), EYES, [pressed, none]).menu);
        assert!(!watch.update(ms(10), EYES, [pressed, none]).menu);
        assert!(!watch.update(ms(20), EYES, [none, none]).menu);
        assert!(watch.update(ms(30), EYES, [pressed, none]).menu);
        // The right palm's gesture belongs to the system
        assert!(!watch.update(ms(40), EYES, [none, pressed]).menu);
    }
}
