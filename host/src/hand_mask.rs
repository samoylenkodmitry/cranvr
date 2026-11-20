//! The hands that the headset's app holds back from the game.
//!
//! A hand that points at a panel of the app presses the panel with its trigger and works it with
//! its thumbstick. The game must not see those inputs, or a click on a panel would fire in the
//! game. The client says which hands it holds (`InputHold::held`) in the datagram that carries the
//! input. The host gives the game rest values for a held hand: no button, no touch, no trigger, no
//! grip and a centered thumbstick. The poses stay, so the game still draws the controller.
//!
//! An input that is down when the hold ends stays up for the game until the user lets go of it.
//! Otherwise a trigger that the user still holds after the ray left the panel would press in the
//! game, and the user never meant that press for the game.

use cranvr_proto::message::{HandInput, Input};

/// A trigger or a grip below this counts as let go.
const ANALOG_REST: f32 = 0.05;
/// A thumbstick within this distance of its middle counts as let go.
const STICK_REST: f32 = 0.15;

/// What a hand held down while it was held, and has not let go of since.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Latch {
    buttons: u32,
    touches: u32,
    trigger: bool,
    grip: bool,
    stick: bool,
}

impl Latch {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The mask of both hands.
#[derive(Debug, Default)]
pub struct HandMask {
    latch: [Latch; 2],
}

fn leans(stick: [f32; 2]) -> bool {
    stick[0].abs().max(stick[1].abs()) > STICK_REST
}

impl HandMask {
    /// The input that the game gets. `held` says which hands the app holds back now.
    pub fn apply(&mut self, input: &Input, held: [bool; 2]) -> Input {
        let mut out = *input;
        for hand in 0..2 {
            out.hands[hand] = self.apply_hand(hand, &input.hands[hand], held[hand]);
        }
        out
    }

    fn apply_hand(&mut self, hand: usize, now: &HandInput, held: bool) -> HandInput {
        let latch = &mut self.latch[hand];
        if held {
            // Whatever is down now stays up for the game until the user lets go of it
            latch.buttons |= now.buttons;
            latch.touches |= now.touches;
            latch.trigger |= now.trigger > ANALOG_REST;
            latch.grip |= now.grip > ANALOG_REST;
            latch.stick |= leans(now.stick);
            return HandInput::default();
        }
        if latch.is_empty() {
            return *now;
        }
        latch.buttons &= now.buttons;
        latch.touches &= now.touches;
        latch.trigger &= now.trigger > ANALOG_REST;
        latch.grip &= now.grip > ANALOG_REST;
        latch.stick &= leans(now.stick);
        HandInput {
            buttons: now.buttons & !latch.buttons,
            touches: now.touches & !latch.touches,
            trigger: if latch.trigger { 0.0 } else { now.trigger },
            grip: if latch.grip { 0.0 } else { now.grip },
            stick: if latch.stick { [0.0; 2] } else { now.stick },
        }
    }

    /// Forgets what the hands held down: the app lost the input focus and the host let go of every
    /// button, or the session ended.
    pub fn reset(&mut self) {
        self.latch = [Latch::default(); 2];
    }

    /// Whether a hand has an input to wait for.
    #[cfg(test)]
    fn is_active(&self) -> bool {
        self.latch.iter().any(|latch| !latch.is_empty())
    }
}

/// The same rule for a hand that makes the game's controller from a pinch (`hand_controller.rs`):
/// the pinch is the trigger and the closed fist is the squeeze. A pinch that is closed when the
/// hold ends must open before it counts for the game.
#[derive(Debug, Default)]
pub struct PinchMask {
    latched: [bool; 2],
}

impl PinchMask {
    /// The trigger and the squeeze of `hand` that the game gets.
    pub fn apply(&mut self, hand: usize, held: bool, trigger: f32, squeeze: f32) -> (f32, f32) {
        let down = trigger > ANALOG_REST || squeeze > ANALOG_REST;
        if held {
            self.latched[hand] |= down;
            return (0.0, 0.0);
        }
        self.latched[hand] &= down;
        if self.latched[hand] {
            (0.0, 0.0)
        } else {
            (trigger, squeeze)
        }
    }

    pub fn reset(&mut self) {
        self.latched = [false; 2];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranvr_proto::message::buttons;

    fn pull(trigger: f32, buttons: u32) -> HandInput {
        HandInput { trigger, buttons, ..Default::default() }
    }

    fn input(left: HandInput, right: HandInput) -> Input {
        Input { sample_time: 1, hands: [left, right] }
    }

    #[test]
    fn a_hand_that_is_not_held_passes_through() {
        let mut mask = HandMask::default();
        let now = input(pull(0.4, buttons::PRIMARY), pull(1.0, buttons::TRIGGER_CLICK));
        assert_eq!(mask.apply(&now, [false, false]), now);
        assert!(!mask.is_active());
    }

    #[test]
    fn a_held_hand_gives_the_game_rest_values_and_keeps_the_other_hand() {
        let mut mask = HandMask::default();
        let left = HandInput { trigger: 0.7, grip: 0.9, stick: [0.5, -0.5], buttons: buttons::PRIMARY | buttons::TRIGGER_CLICK, touches: buttons::PRIMARY, ..Default::default() };
        let right = pull(1.0, buttons::TRIGGER_CLICK);
        let out = mask.apply(&input(left, right), [false, true]);
        assert_eq!(out.hands[0], left);
        assert_eq!(out.hands[1], HandInput::default());
        // The sample's time stays
        assert_eq!(out.sample_time, 1);
    }

    #[test]
    fn a_press_that_is_still_down_when_the_hold_ends_stays_up_for_the_game_until_it_is_let_go() {
        let mut mask = HandMask::default();
        let down = pull(1.0, buttons::TRIGGER_CLICK);
        // The press began on a panel
        assert_eq!(mask.apply(&input(HandInput::default(), down), [false, true]).hands[1], HandInput::default());
        // The ray left the panel and the trigger is still down: the game gets nothing
        assert_eq!(mask.apply(&input(HandInput::default(), down), [false, false]).hands[1], HandInput::default());
        assert!(mask.is_active());
        // A second button pressed after the hold ended is the game's
        let more = pull(1.0, buttons::TRIGGER_CLICK | buttons::PRIMARY);
        assert_eq!(mask.apply(&input(HandInput::default(), more), [false, false]).hands[1], HandInput { buttons: buttons::PRIMARY, ..Default::default() });
        // The user lets go: the next press goes through
        assert_eq!(mask.apply(&input(HandInput::default(), HandInput::default()), [false, false]).hands[1], HandInput::default());
        assert!(!mask.is_active());
        let again = pull(0.9, buttons::TRIGGER_CLICK);
        assert_eq!(mask.apply(&input(HandInput::default(), again), [false, false]).hands[1], again);
    }

    #[test]
    fn a_thumbstick_that_leans_through_the_hold_waits_for_the_middle() {
        let mut mask = HandMask::default();
        let leaning = HandInput { stick: [0.0, 0.9], ..Default::default() };
        mask.apply(&input(HandInput::default(), leaning), [false, true]);
        assert_eq!(mask.apply(&input(HandInput::default(), leaning), [false, false]).hands[1].stick, [0.0, 0.0]);
        mask.apply(&input(HandInput::default(), HandInput { stick: [0.05, 0.0], ..Default::default() }), [false, false]);
        assert_eq!(mask.apply(&input(HandInput::default(), leaning), [false, false]).hands[1].stick, [0.0, 0.9]);
    }

    #[test]
    fn a_trigger_that_is_not_pulled_at_the_start_of_the_hold_is_the_games_at_its_end() {
        let mut mask = HandMask::default();
        mask.apply(&input(HandInput::default(), HandInput::default()), [false, true]);
        let pulled = pull(0.8, buttons::TRIGGER_CLICK);
        assert_eq!(mask.apply(&input(HandInput::default(), pulled), [false, false]).hands[1], pulled);
    }

    #[test]
    fn a_reset_forgets_what_was_down() {
        let mut mask = HandMask::default();
        mask.apply(&input(HandInput::default(), pull(1.0, buttons::TRIGGER_CLICK)), [false, true]);
        mask.reset();
        let pulled = pull(1.0, buttons::TRIGGER_CLICK);
        assert_eq!(mask.apply(&input(HandInput::default(), pulled), [false, false]).hands[1], pulled);
    }

    #[test]
    fn a_pinch_that_is_closed_when_the_hold_ends_must_open_first() {
        let mut mask = PinchMask::default();
        assert_eq!(mask.apply(1, false, 0.9, 0.0), (0.9, 0.0));
        assert_eq!(mask.apply(1, true, 0.9, 0.0), (0.0, 0.0));
        assert_eq!(mask.apply(1, false, 0.9, 0.0), (0.0, 0.0));
        assert_eq!(mask.apply(1, false, 0.0, 0.0), (0.0, 0.0));
        assert_eq!(mask.apply(1, false, 0.8, 0.0), (0.8, 0.0));
        // The other hand never stood in the way
        assert_eq!(mask.apply(0, false, 0.5, 0.5), (0.5, 0.5));
    }
}
