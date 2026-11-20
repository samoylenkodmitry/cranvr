//! Which hands the app holds back from the game.
//!
//! A hand that points at a panel of the app presses the panel with its trigger and works it with
//! its thumbstick. The game must not see those inputs (see `host/src/hand_mask.rs`). This file
//! decides, for each frame of the headset, which hands the app holds. The sender puts the answer
//! into the datagram that carries the same frame's input (`InputHold`).
//!
//! Cranpose gives the app no signal for "this hand's ray is on a panel". Its pointer events carry
//! no hand, and `XrFrame` tells only which input drives each hand's pointer. The app knows two
//! things instead:
//! - whether some panel of the app has the pointer over it (`hover`), from the pointer events of its
//!   panels (`hover.rs`);
//! - each hand's trigger and whether something points for the hand.
//!
//! Cranpose drives one pointer at a time (`cranpose-xr/src/pointer.rs`): the hand whose trigger
//! went down last, and the other hand only while the first points at no panel and the second does.
//! The app follows the same rule to learn which hand the hover belongs to. A pull on a panel keeps
//! the hand held until its trigger lets go, also when the ray leaves the panel during the press, as
//! a drag does. The host keeps a press that is still down away from the game after the hold ends.
//!
//! What Cranpose would have to give for an exact answer: `XrFrame` with the panel or handle that each
//! hand's ray meets (`left_hit` and `right_hit`, or the pointer event with the hand that made it).

use std::time::Duration;

/// A trigger counts as down above `PRESS` and as up below `RELEASE`, as in Cranpose's pointer.
const PRESS: f32 = 0.6;
const RELEASE: f32 = 0.4;
/// A hand stays held this long after the ray left the last panel, so a ray that crosses a gap
/// between two panels, or a release that turns the controller, does not leak a press to the game.
pub const RELEASE_DELAY: Duration = Duration::from_millis(150);
/// A pull counts as a pull on a panel when the ray was on a panel this long before it. The pull of a
/// trigger turns the controller, and Cranpose takes the point from just before the pull.
const PULL_LOOKBACK: Duration = Duration::from_millis(250);
/// While the hand menu is open, the hand that does not carry it stays held this long after the ray
/// left the menu: the user works the menu with it, and the pause between two presses is not a game.
pub const MENU_GRACE: Duration = Duration::from_millis(1500);

/// What the app knows in one frame of the headset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HoldFrame {
    /// The frame's time, on any clock that rises
    pub time: Duration,
    /// A panel of the app has the pointer over it
    pub hover: bool,
    /// How far each hand's trigger is pulled, or how far its index finger pinches, from 0 to 1
    pub trigger: [f32; 2],
    /// Whether something points for each hand: a tracked controller, or a tracked hand
    pub points: [bool; 2],
    /// The hand menu is open. It rides on the left hand, so the right hand works it.
    pub menu_open: bool,
}

/// The state of the rule across frames. Before any trigger goes down, the right hand drives the
/// pointer.
#[derive(Clone, Copy, Debug)]
pub struct PanelHold {
    /// The hand that drives the pointer: 0 for the left hand and 1 for the right hand
    driver: usize,
    down: [bool; 2],
    /// The hand pulled its trigger over a panel and has not let go
    pressing: [bool; 2],
    /// When each hand's ray was last on a panel
    hovered_at: [Option<Duration>; 2],
    /// When any hand's ray was last on a panel
    any_hover_at: Option<Duration>,
}

impl Default for PanelHold {
    fn default() -> Self {
        Self { driver: 1, down: [false; 2], pressing: [false; 2], hovered_at: [None; 2], any_hover_at: None }
    }
}

impl PanelHold {
    /// The hands that the app holds back from the game in this frame, left then right.
    pub fn update(&mut self, frame: &HoldFrame) -> [bool; 2] {
        let was_down = self.down;
        for hand in 0..2 {
            let trigger = frame.trigger[hand];
            self.down[hand] = if was_down[hand] { trigger > RELEASE } else { trigger >= PRESS };
        }
        let pulled = |hand: usize| self.down[hand] && !was_down[hand];
        // A trigger that goes down takes the pointer to its hand, the right hand when both do
        if pulled(1) {
            self.driver = 1;
        } else if pulled(0) {
            self.driver = 0;
        } else if !self.pressing[self.driver] && !frame.points[self.driver] && frame.points[1 - self.driver] {
            self.driver = 1 - self.driver;
        }
        if frame.hover {
            self.hovered_at[self.driver] = Some(frame.time);
            self.any_hover_at = Some(frame.time);
        }
        let hover_within = |hand: usize, within: Duration| self.hovered_at[hand].is_some_and(|at| frame.time.saturating_sub(at) <= within);
        for hand in 0..2 {
            if pulled(hand) && hover_within(hand, PULL_LOOKBACK) {
                self.pressing[hand] = true;
            }
            if !self.down[hand] {
                self.pressing[hand] = false;
            }
        }
        let mut held = [false; 2];
        for hand in 0..2 {
            let delay = if frame.menu_open && hand == 1 { MENU_GRACE } else { RELEASE_DELAY };
            held[hand] = self.pressing[hand] || hover_within(hand, delay);
        }
        held
    }

    /// How long no hand's ray has been on a panel, or `None` before the first time.
    pub fn idle(&self, now: Duration) -> Option<Duration> {
        self.any_hover_at.map(|at| now.saturating_sub(at))
    }

    /// Forgets everything: the app lost the input focus, or the link ended.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ms: u64, hover: bool, trigger: [f32; 2]) -> HoldFrame {
        HoldFrame { time: Duration::from_millis(ms), hover, trigger, points: [true, true], menu_open: false }
    }

    #[test]
    fn a_ray_on_no_panel_holds_nothing() {
        let mut hold = PanelHold::default();
        for ms in 0..20 {
            assert_eq!(hold.update(&frame(ms * 10, false, [0.0, 0.0])), [false, false]);
        }
        // A press in the game, away from every panel, stays the game's
        assert_eq!(hold.update(&frame(300, false, [0.0, 1.0])), [false, false]);
    }

    #[test]
    fn the_hand_that_points_at_a_panel_is_held_and_the_other_is_not() {
        let mut hold = PanelHold::default();
        // The right hand drives by default
        assert_eq!(hold.update(&frame(0, true, [0.0, 0.0])), [false, true]);
        // The left hand pulls its trigger in the game: it takes the pointer, and no panel is under its ray
        assert_eq!(hold.update(&frame(20, false, [1.0, 0.0])), [false, true], "the right hand's hover lingers for the release delay");
        assert_eq!(hold.update(&frame(300, false, [1.0, 0.0])), [false, false]);
    }

    #[test]
    fn a_hold_ends_after_the_release_delay() {
        let mut hold = PanelHold::default();
        assert_eq!(hold.update(&frame(0, true, [0.0, 0.0])), [false, true]);
        assert_eq!(hold.update(&frame(100, false, [0.0, 0.0])), [false, true]);
        assert_eq!(hold.update(&frame(151, false, [0.0, 0.0])), [false, false]);
    }

    #[test]
    fn a_press_that_began_on_a_panel_holds_until_the_trigger_lets_go_also_off_the_panel() {
        let mut hold = PanelHold::default();
        assert_eq!(hold.update(&frame(0, true, [0.0, 0.0])), [false, true]);
        assert_eq!(hold.update(&frame(50, true, [0.0, 0.8])), [false, true]);
        // The ray drags off the panel and stays away for a second with the trigger down
        assert_eq!(hold.update(&frame(300, false, [0.0, 0.9])), [false, true]);
        assert_eq!(hold.update(&frame(1300, false, [0.0, 0.9])), [false, true]);
        // The trigger lets go: the hold ends with the press. What the trigger still shows while it
        // falls stays away from the game by the host's latch (`hand_mask.rs`).
        assert_eq!(hold.update(&frame(1310, false, [0.0, 0.2])), [false, false]);
    }

    #[test]
    fn a_trigger_that_was_down_before_the_ray_reached_a_panel_is_the_games() {
        let mut hold = PanelHold::default();
        assert_eq!(hold.update(&frame(0, false, [0.0, 1.0])), [false, false]);
        // The ray comes onto a panel with the trigger still down: Cranpose does not press, and the
        // hover holds the hand only for as long as the ray is there
        assert_eq!(hold.update(&frame(500, true, [0.0, 1.0])), [false, true]);
        assert_eq!(hold.update(&frame(900, false, [0.0, 1.0])), [false, false]);
    }

    #[test]
    fn the_hover_belongs_to_the_hand_that_pulled_last() {
        let mut hold = PanelHold::default();
        // The left hand pulls on a panel
        assert_eq!(hold.update(&frame(0, true, [1.0, 0.0])), [true, false]);
        assert_eq!(hold.update(&frame(50, true, [0.0, 0.0])), [true, false]);
        // The right hand pulls on a panel
        assert_eq!(hold.update(&frame(1000, true, [0.0, 1.0])), [false, true]);
    }

    #[test]
    fn both_triggers_in_one_frame_go_to_the_right_hand() {
        let mut hold = PanelHold::default();
        assert_eq!(hold.update(&frame(0, true, [1.0, 1.0])), [false, true]);
    }

    #[test]
    fn a_driver_that_points_at_nothing_hands_the_pointer_to_the_hand_that_does() {
        let mut hold = PanelHold::default();
        let mut f = frame(0, true, [0.0, 0.0]);
        // Nothing tracks the right hand: the left hand points
        f.points = [true, false];
        assert_eq!(hold.update(&f), [true, false]);
    }

    #[test]
    fn the_right_hand_stays_held_a_moment_while_the_menu_is_open() {
        let mut hold = PanelHold::default();
        let mut f = frame(0, true, [0.0, 0.0]);
        f.menu_open = true;
        assert_eq!(hold.update(&f), [false, true]);
        f.hover = false;
        f.time = Duration::from_millis(1000);
        assert_eq!(hold.update(&f), [false, true], "the pause between two presses on the menu");
        f.time = Duration::from_millis(1700);
        assert_eq!(hold.update(&f), [false, false]);
        // With the menu shut, only the short delay holds
        let mut hold = PanelHold::default();
        hold.update(&frame(0, true, [0.0, 0.0]));
        assert_eq!(hold.update(&frame(1000, false, [0.0, 0.0])), [false, false]);
    }

    #[test]
    fn the_idle_time_counts_from_the_last_hover() {
        let mut hold = PanelHold::default();
        assert_eq!(hold.idle(Duration::from_secs(5)), None);
        hold.update(&frame(1000, true, [0.0, 0.0]));
        assert_eq!(hold.idle(Duration::from_millis(4000)), Some(Duration::from_secs(3)));
        hold.reset();
        assert_eq!(hold.idle(Duration::from_secs(9)), None);
    }
}
