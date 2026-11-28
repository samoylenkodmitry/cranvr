//! A look that rests on a ball opens the hand menu, for a user with no controller and no tracked
//! hand.
//!
//! While no input is tracked, a small ball stands low in front of the user: 45 cm ahead of the eyes
//! and [`PITCH_DEGREES`] below them. It follows the head's place and its turn about the vertical axis
//! in every frame (`FollowMode::Tight(TrackedDimensions::TranslationAndYaw)`), so it stays in front
//! of the user, and a look down does not move it. A Quest 3 has no eye tracking, so the look is the
//! head's ray: the user tips the head down until the ball is in the middle of the view. When the ray
//! passes within [`AIM_DEGREES`] of the ball's middle for [`DWELL`], the menu opens or closes. The
//! next look counts after the ray left the ball.
//!
//! The app knows where the ball stands, because the subspace follows the head with no lag: its
//! origin is the head's place, turned as the head is turned about the vertical axis, and a meter
//! ahead. The launch extra `gaze_pitch` sets the ball's angle below the eyes in degrees, for a
//! headset on a desk that cannot look down.
//!
//! The file has no Cranpose state, so a test drives it with plain values.

use crate::launch;
use cranpose_xr::Pose;
use std::time::Duration;

/// How far below the eyes the ball stands, in degrees
pub const PITCH_DEGREES: f32 = 34.0;
/// The ball's distance from the eyes, in meters
pub const DISTANCE: f32 = 0.45;
/// How close to the ball's middle the head's ray must pass
pub const AIM_DEGREES: f32 = 4.0;
/// How long the look rests on the ball before the menu opens
pub const DWELL: Duration = Duration::from_secs(1);

/// The ball's angle below the eyes in this launch, in degrees
pub fn pitch() -> f32 {
    launch::args().string("gaze_pitch").and_then(|text| text.parse::<f32>().ok()).filter(|p| p.is_finite()).unwrap_or(PITCH_DEGREES)
}

/// The ball's offset from the subspace's origin, which lies a meter ahead of the eyes, in dp
pub fn offset(pitch_degrees: f32) -> [f32; 3] {
    let (down, ahead) = pitch_degrees.to_radians().sin_cos();
    [0.0, -DISTANCE * down * 1000.0, (1.0 - DISTANCE * ahead) * 1000.0]
}

/// The heading of the head about the vertical axis, as a turn: Cranpose's upright follow takes the
/// heading of the head's forward axis, seen from above.
fn heading(head: &Pose) -> Option<[f32; 4]> {
    let ahead = head.rotate([0.0, 0.0, -1.0]);
    let level = ahead[0].hypot(ahead[2]);
    (level >= 0.1).then(|| {
        let half = (-ahead[0]).atan2(-ahead[2]) / 2.0;
        [0.0, half.sin(), 0.0, half.cos()]
    })
}

/// Whether no input is tracked, so the look's ball shows: no controller is present and no hand is
/// tracked (left, right). A hand counts while Cranpose's hand pointer points with it, since that
/// pointer presses the ball in the view, or while its palm joint is located.
pub fn no_input(controllers: [bool; 2], hands: [bool; 2]) -> bool {
    controllers == [false; 2] && hands == [false; 2]
}

/// Whether the head at `head` looks at the ball, which stands `pitch_degrees` below its eyes.
pub fn looks_at(head: &Pose, pitch_degrees: f32) -> bool {
    let Some(turn) = heading(head) else { return false };
    let upright = Pose { position: head.position, orientation: turn };
    let [x, y, z] = offset(pitch_degrees).map(|dp| dp / 1000.0);
    let ball = upright.then(&Pose::at([x, y, z - 1.0])).position;
    let to_ball = [0, 1, 2].map(|axis| ball[axis] - head.position[axis]);
    let ahead = head.rotate([0.0, 0.0, -1.0]);
    let length = to_ball.iter().map(|v| v * v).sum::<f32>().sqrt();
    let cos: f32 = to_ball.iter().zip(ahead).map(|(b, a)| b * a / length).sum();
    cos >= AIM_DEGREES.to_radians().cos()
}

/// Follows the look from frame to frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Dwell {
    /// Since when the look rests on the ball, while it does
    since: Option<Duration>,
    /// The look opened or closed the menu and has not left the ball since
    fired: bool,
}

impl Dwell {
    /// Reads one frame. Returns whether the look opens or closes the menu now.
    pub fn update(&mut self, time: Duration, on_ball: bool) -> bool {
        if !on_ball {
            *self = Self::default();
            return false;
        }
        let since = *self.since.get_or_insert(time);
        if self.fired || time.saturating_sub(since) < DWELL {
            return false;
        }
        self.fired = true;
        true
    }

    /// Whether the look rests on the ball now and counts toward a press
    pub fn resting(&self) -> bool {
        self.since.is_some() && !self.fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A head at the eyes' place `position`, turned left by `yaw` and tipped down by `down` degrees
    fn head(position: [f32; 3], yaw: f32, down: f32) -> Pose {
        let (y, p) = ((yaw / 2.0).to_radians(), (-down / 2.0).to_radians());
        let yaw_turn = [0.0, y.sin(), 0.0, y.cos()];
        let pitch_turn = [p.sin(), 0.0, 0.0, p.cos()];
        Pose { position, orientation: Pose { position: [0.0; 3], orientation: yaw_turn }.then(&Pose { position: [0.0; 3], orientation: pitch_turn }).orientation }
    }

    #[test]
    fn the_ball_stands_low_in_front_and_a_head_that_tips_down_looks_at_it() {
        let [x, y, z] = offset(PITCH_DEGREES);
        assert!(x == 0.0 && (-260.0..-240.0).contains(&y) && (620.0..640.0).contains(&z), "{x} {y} {z}");
        // A level head does not look at it, and a head tipped down by the ball's angle does, whatever
        // its place and its heading
        for (position, yaw) in [([0.0, 1.6, 0.0], 0.0), ([1.0, 1.2, -2.0], 120.0), ([0.0, 0.0, 0.0], -75.0)] {
            assert!(!looks_at(&head(position, yaw, 0.0), PITCH_DEGREES));
            assert!(looks_at(&head(position, yaw, PITCH_DEGREES), PITCH_DEGREES), "{position:?} {yaw}");
            assert!(looks_at(&head(position, yaw, PITCH_DEGREES + 3.0), PITCH_DEGREES));
            assert!(!looks_at(&head(position, yaw, PITCH_DEGREES + 6.0), PITCH_DEGREES));
        }
        // A test on a desk puts the ball straight ahead
        assert!(looks_at(&head([0.0; 3], 0.0, 0.0), 0.0));
    }

    #[test]
    fn only_a_headset_with_no_controller_and_no_tracked_hand_has_no_input() {
        assert!(no_input([false; 2], [false; 2]));
        assert!(!no_input([true, false], [false; 2]));
        assert!(!no_input([false, true], [false; 2]));
        // A hand that the cameras track points with its pinch, palm joint or not
        assert!(!no_input([false; 2], [false, true]));
        assert!(!no_input([false; 2], [true, false]));
    }

    #[test]
    fn a_look_of_a_second_fires_once_and_the_next_needs_a_look_away() {
        let ms = Duration::from_millis;
        let mut dwell = Dwell::default();
        assert!(!dwell.update(ms(0), true));
        assert!(dwell.resting());
        assert!(!dwell.update(ms(999), true));
        assert!(dwell.update(ms(1000), true));
        assert!(!dwell.update(ms(3000), true), "one press for one look");
        assert!(!dwell.resting());
        // A look away and back again
        assert!(!dwell.update(ms(3100), false));
        assert!(!dwell.update(ms(3200), true));
        assert!(!dwell.update(ms(3700), false));
        assert!(!dwell.update(ms(3800), true));
        assert!(dwell.update(ms(4800), true));
    }
}
