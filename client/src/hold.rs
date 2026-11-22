//! Which hands the app holds back from the game.
//!
//! A hand that points at a panel of the app presses the panel with its trigger and works it with
//! its thumbstick. The game must not see those inputs (see `host/src/hand_mask.rs`). This file
//! decides, for each frame of the headset, which hands the app holds. The sender puts the answer
//! into the datagram that carries the same frame's input (`InputHold`).
//!
//! Cranpose gives each hand a pointer of its own, and a pointer event carries the id of its pointer:
//! the left hand's or the right hand's. The app therefore knows which hand's ray is on a panel
//! (`hover.rs`: the pointer events of each panel's root and of each display's surface). A pull on a
//! panel keeps the hand held until its trigger lets go, also when the ray leaves the panel during the
//! press, as a drag does. A hand stays held for a moment after the ray left the last panel. The host
//! keeps a press that is still down away from the game after the hold ends.
//!
//! What the app still cannot know: that a hand's ray is on a handle, the sphere under a panel or at its
//! corner. A handle gives the app no hover and no hand, only the start and the end of a grab. The trigger's
//! pull before the grab's start therefore reaches the game, and the hold covers the grab from its start
//! to its end, for the hands whose triggers are down. `XrFrame` or the pointer's events would have to name
//! the handle under each hand's ray for an exact answer.

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
/// While the hand menu is open, the hand that works it stays held this long after the ray left the
/// menu: the pause between two presses is not a game.
pub const MENU_GRACE: Duration = Duration::from_millis(1500);

/// What the app knows in one frame of the headset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HoldFrame {
    /// The frame's time, on any clock that rises
    pub time: Duration,
    /// Whether each hand's pointer is over a panel of the app, left then right
    pub hover: [bool; 2],
    /// A controller holds a panel by its handle, to carry it or to resize it
    pub handle: bool,
    /// How far each hand's trigger is pulled, or how far its index finger pinches, from 0 to 1
    pub trigger: [f32; 2],
    /// The hands that work the open hand menu, left then right: the other hand of a menu on a hand,
    /// or the hands that point at a menu in front of the user (`menu_place::workers`). None while the
    /// menu is closed.
    pub menu_workers: [bool; 2],
}

/// The state of the rule across frames.
#[derive(Clone, Copy, Debug, Default)]
pub struct PanelHold {
    down: [bool; 2],
    /// The hand pulled its trigger over a panel and has not let go
    pressing: [bool; 2],
    /// When each hand's ray was last on a panel
    hovered_at: [Option<Duration>; 2],
}

impl PanelHold {
    /// The hands that the app holds back from the game in this frame, left then right.
    pub fn update(&mut self, frame: &HoldFrame) -> [bool; 2] {
        let was_down = self.down;
        for hand in 0..2 {
            let trigger = frame.trigger[hand];
            self.down[hand] = if was_down[hand] { trigger > RELEASE } else { trigger >= PRESS };
            if frame.hover[hand] {
                self.hovered_at[hand] = Some(frame.time);
            }
        }
        let hover_within = |hand: usize, within: Duration| self.hovered_at[hand].is_some_and(|at| frame.time.saturating_sub(at) <= within);
        let mut held = [false; 2];
        for hand in 0..2 {
            // A pull over a panel is a press on it, which lasts until the trigger lets go
            if self.down[hand] && !was_down[hand] && hover_within(hand, PULL_LOOKBACK) {
                self.pressing[hand] = true;
            }
            if !self.down[hand] {
                self.pressing[hand] = false;
            }
            let delay = if frame.menu_workers[hand] { MENU_GRACE } else { RELEASE_DELAY };
            // A grab of a handle holds the hands whose triggers are down: Cranpose names no hand for it
            let carrying = frame.handle && self.down[hand];
            held[hand] = self.pressing[hand] || carrying || hover_within(hand, delay);
        }
        held
    }

    /// Forgets everything: the app lost the input focus, or the link ended.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ms: u64, hover: [bool; 2], trigger: [f32; 2]) -> HoldFrame {
        HoldFrame { time: Duration::from_millis(ms), hover, handle: false, trigger, menu_workers: [false; 2] }
    }

    const NONE: [bool; 2] = [false, false];
    const LEFT: [bool; 2] = [true, false];
    const RIGHT: [bool; 2] = [false, true];

    #[test]
    fn a_ray_on_no_panel_holds_nothing() {
        let mut hold = PanelHold::default();
        for ms in 0..20 {
            assert_eq!(hold.update(&frame(ms * 10, NONE, [0.0, 0.0])), NONE);
        }
        // A press in the game, away from every panel, stays the game's
        assert_eq!(hold.update(&frame(300, NONE, [0.0, 1.0])), NONE);
    }

    #[test]
    fn the_hand_whose_pointer_is_on_a_panel_is_held_and_the_other_is_not() {
        let mut hold = PanelHold::default();
        assert_eq!(hold.update(&frame(0, RIGHT, [0.0, 0.0])), RIGHT);
        // The left hand pulls its trigger in the game, away from every panel: the game's pull
        assert_eq!(hold.update(&frame(20, RIGHT, [1.0, 0.0])), RIGHT);
        assert_eq!(hold.update(&frame(40, LEFT, [1.0, 0.0])), [true, true], "the right hand's hover lingers for the release delay");
        assert_eq!(hold.update(&frame(300, NONE, [1.0, 0.0])), NONE);
        // Each hand has its own hover: the left hand's pointer is on a panel and the right hand's is not
        assert_eq!(hold.update(&frame(310, LEFT, [0.0, 0.0])), LEFT);
    }

    #[test]
    fn a_hold_ends_after_the_release_delay() {
        let mut hold = PanelHold::default();
        assert_eq!(hold.update(&frame(0, RIGHT, [0.0, 0.0])), RIGHT);
        assert_eq!(hold.update(&frame(100, NONE, [0.0, 0.0])), RIGHT);
        assert_eq!(hold.update(&frame(151, NONE, [0.0, 0.0])), NONE);
    }

    #[test]
    fn a_press_that_began_on_a_panel_holds_until_the_trigger_lets_go_also_off_the_panel() {
        let mut hold = PanelHold::default();
        assert_eq!(hold.update(&frame(0, RIGHT, [0.0, 0.0])), RIGHT);
        assert_eq!(hold.update(&frame(50, RIGHT, [0.0, 0.8])), RIGHT);
        // The ray drags off the panel and stays away for a second with the trigger down
        assert_eq!(hold.update(&frame(300, NONE, [0.0, 0.9])), RIGHT);
        assert_eq!(hold.update(&frame(1300, NONE, [0.0, 0.9])), RIGHT);
        // The trigger lets go: the hold ends with the press. What the trigger still shows while it
        // falls stays away from the game by the host's latch (`hand_mask.rs`).
        assert_eq!(hold.update(&frame(1310, NONE, [0.0, 0.2])), NONE);
    }

    #[test]
    fn a_trigger_that_was_down_before_the_ray_reached_a_panel_is_the_games() {
        let mut hold = PanelHold::default();
        assert_eq!(hold.update(&frame(0, NONE, [0.0, 1.0])), NONE);
        // The ray comes onto a panel with the trigger still down: Cranpose does not press, and the
        // hover holds the hand only for as long as the ray is there
        assert_eq!(hold.update(&frame(500, RIGHT, [0.0, 1.0])), RIGHT);
        assert_eq!(hold.update(&frame(900, NONE, [0.0, 1.0])), NONE);
    }

    #[test]
    fn each_hand_presses_its_own_panel() {
        let mut hold = PanelHold::default();
        // Both pointers are over panels and both triggers go down in turn
        assert_eq!(hold.update(&frame(0, [true, true], [0.0, 0.0])), [true, true]);
        assert_eq!(hold.update(&frame(50, [true, true], [1.0, 0.0])), [true, true]);
        // The right ray leaves its panel and the left press goes on
        assert_eq!(hold.update(&frame(1000, LEFT, [1.0, 0.0])), LEFT);
        assert_eq!(hold.update(&frame(1200, NONE, [0.0, 0.0])), NONE);
    }

    #[test]
    fn a_grab_of_a_handle_holds_the_hands_whose_triggers_are_down() {
        let mut hold = PanelHold::default();
        let mut f = frame(0, NONE, [0.0, 0.9]);
        // A carry begins: the right trigger is down
        f.handle = true;
        assert_eq!(hold.update(&f), RIGHT);
        // The left hand's trigger is down too, in the game: it is held while the carry lasts
        f.time = Duration::from_millis(500);
        f.trigger = [0.9, 0.9];
        assert_eq!(hold.update(&f), [true, true]);
        // The carry ends and the triggers are still down: the hands are free, and the host's latch keeps
        // their presses from the game
        f.handle = false;
        f.time = Duration::from_millis(900);
        assert_eq!(hold.update(&f), NONE);
    }

    #[test]
    fn the_hand_that_works_the_open_menu_stays_held_a_moment() {
        let mut hold = PanelHold::default();
        let mut f = frame(0, RIGHT, [0.0, 0.0]);
        f.menu_workers = RIGHT;
        assert_eq!(hold.update(&f), RIGHT);
        f.hover = NONE;
        f.time = Duration::from_millis(1000);
        assert_eq!(hold.update(&f), RIGHT, "the pause between two presses on the menu");
        f.time = Duration::from_millis(1700);
        assert_eq!(hold.update(&f), NONE);
        // With the menu shut, only the short delay holds
        let mut hold = PanelHold::default();
        hold.update(&frame(0, RIGHT, [0.0, 0.0]));
        assert_eq!(hold.update(&frame(1000, NONE, [0.0, 0.0])), NONE);
        // A menu on the right hand: the left hand works it
        let mut hold = PanelHold::default();
        let mut f = frame(0, LEFT, [0.0, 0.0]);
        f.menu_workers = LEFT;
        hold.update(&f);
        f.hover = NONE;
        f.time = Duration::from_millis(1000);
        assert_eq!(hold.update(&f), LEFT);
    }
}
