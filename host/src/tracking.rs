//! Hands, body, face and eyes between the headset and the game (see `cranvr_proto::tracking`).
//!
//! The hub sits in the host's event loop and does four jobs:
//! - It tells the game's runtime which features the headset offers (a file, see `runtime_link.rs`).
//! - It decides which features the host asks the headset for: the ones that the game's runtime reports
//!   as in use. A feature that no game uses costs no traffic.
//! - It forwards the data that comes back to the runtime.
//! - It makes controllers from hands (see `hand_controller.rs`) for a game without hand code, while the
//!   headset tracks a hand and no controller.
//! - It makes the hand interaction profile of `XR_EXT_hand_interaction` from the hands (see
//!   `hand_interaction.rs`) for a game that binds that profile. Such a game has hand code too.

use crate::{
    hand_controller, hand_interaction,
    hand_mask::PinchMask,
    headset::{Device, Headset},
    pose::{DeviceMotion, Pose, Quat, Vec3},
    runtime_link::RuntimeLink,
};
use cranvr_common::inputs::{LEFT_SQUEEZE_VALUE_ID, LEFT_TRIGGER_TOUCH_ID, LEFT_TRIGGER_VALUE_ID, RIGHT_SQUEEZE_VALUE_ID, RIGHT_TRIGGER_TOUCH_ID, RIGHT_TRIGGER_VALUE_ID};
use cranvr_proto::{
    tracking::{Features, Hand, Joint, Tracked, HAND_JOINTS},
    wire::{Motion, Nanos},
};
use log::{info, warn};
use std::sync::Arc;

/// A controller that is missing this long makes the host ask for hands.
const MISSING_FOR: Nanos = 300_000_000;
/// Both controllers must be back this long before the host stops asking for hands. The user puts a
/// controller down and picks it up again, and a wish that flaps would restart the headset's tracker.
const BACK_FOR: Nanos = 3_000_000_000;
/// A hand sample that is older than this no longer makes a controller.
const HAND_FRESH: Nanos = 100_000_000;
/// A game that has hand code creates its tracker soon after it starts. A hand makes no controller
/// until the game has run this long without one, so that such a game never sees a controller that
/// comes and goes.
const GRACE: Nanos = 3_000_000_000;

/// Decides when the host asks the headset for hands to make controllers from. It asks while a
/// controller is missing, and keeps asking for a while after both are back.
#[derive(Debug, Default)]
pub struct HandNeed {
    asking: bool,
    /// Since when a controller has been missing, or both have been there
    since: Option<Nanos>,
}

impl HandNeed {
    /// `both` says whether the sample at `now` has both controllers. It returns whether to ask.
    pub fn update(&mut self, now: Nanos, both: bool) -> bool {
        // The state that this sample points to starts at the first sample that shows it
        let pointing_to_ask = !both;
        if pointing_to_ask == self.asking {
            self.since = None;
            return self.asking;
        }
        let since = *self.since.get_or_insert(now);
        let needed = if pointing_to_ask { MISSING_FOR } else { BACK_FOR };
        if now.saturating_sub(since) >= needed {
            self.asking = pointing_to_ask;
            self.since = None;
        }
        self.asking
    }
}

/// The ids of the buttons that a hand's pinch and fist stand for, left then right: the trigger's
/// value, the squeeze's value and the trigger's touch.
const IDS: [[u64; 3]; 2] =
    [[LEFT_TRIGGER_VALUE_ID, LEFT_SQUEEZE_VALUE_ID, LEFT_TRIGGER_TOUCH_ID], [RIGHT_TRIGGER_VALUE_ID, RIGHT_SQUEEZE_VALUE_ID, RIGHT_TRIGGER_TOUCH_ID]];

/// The trigger value from which the finger counts as touching the trigger
const TOUCHING: f32 = 0.1;

pub struct Hub {
    link: Arc<RuntimeLink>,
    headset: Arc<dyn Headset>,
    /// The user did not turn off controllers made of hands
    hand_controllers: bool,
    offered: Features,
    /// The wish that the headset link has now
    wished: Features,
    need: HandNeed,
    /// The sample time at which the game was first seen connected, until it leaves
    connected_since: Option<Nanos>,
    /// The sample times of the newest hand samples that could make a controller
    seen: [Option<Nanos>; 2],
    /// A controller of that hand is tracked in the newest tracking sample
    real: [bool; 2],
    /// The hand makes the game's controller now, and the last button values that it sent
    made: [Option<[f32; 3]>; 2],
    /// The hands that the headset's app holds back from the game: a pinch of such a hand is the
    /// panel's press, not the game's trigger
    held: [bool; 2],
    pinch_mask: PinchMask,
    /// The same mask for the pinch and the grasp of the hand interaction profile
    interaction_mask: PinchMask,
    /// The sample time of the newest hand interaction sent to the runtime: a pause sends the hands'
    /// loss with it, as the runtime takes no older sample
    interaction_ns: Nanos,
}

impl Hub {
    pub fn new(link: Arc<RuntimeLink>, headset: Arc<dyn Headset>, hand_controllers: bool) -> Self {
        Self {
            link,
            headset,
            hand_controllers,
            offered: Features::NONE,
            wished: Features::NONE,
            need: HandNeed::default(),
            connected_since: None,
            seen: [None, None],
            real: [false; 2],
            made: [None, None],
            held: [false; 2],
            pinch_mask: PinchMask::default(),
            interaction_mask: PinchMask::default(),
            interaction_ns: 0,
        }
    }

    /// The headset's app says which hands it holds back from the game now.
    pub fn set_held(&mut self, held: [bool; 2]) {
        self.held = held;
    }

    /// The headset's app lost the input focus: no hand makes a controller, and the buttons that a
    /// hand made go back to 0 (Meta's rule: an app without the focus shows no controllers).
    pub fn pause(&mut self) {
        self.pinch_mask.reset();
        self.interaction_mask.reset();
        self.seen = [None, None];
        for hand in 0..2 {
            self.release(hand);
        }
        if self.link.wants_hand_interaction() {
            for hand in 0..2 {
                self.link.hand_interaction(self.interaction_ns, hand, None);
            }
        }
    }

    /// The game has hand code: it creates a hand tracker or binds the hand interaction profile
    fn game_has_hand_code(&self) -> bool {
        self.link.requested_tracking().contains(Features::HANDS) || self.link.wants_hand_interaction()
    }

    /// The headset says which features it offers. An empty set means that it is gone.
    pub fn on_offer(&mut self, offered: Features) {
        self.offered = offered;
        self.link.set_offered_tracking(offered);
        if offered.is_empty() {
            self.seen = [None, None];
            self.made = [None, None];
            // The next headset starts with no wish, whatever the last one was asked for
            self.wished = Features::NONE;
            self.headset.set_tracking_wish(Features::NONE);
        }
    }

    /// Whether a hand makes a controller at the sample time `ns`: a game runs, it has run for the
    /// grace time without hand code, and the user did not turn it off.
    fn making_controllers(&self, ns: Nanos) -> bool {
        self.hand_controllers
            && !self.game_has_hand_code()
            && self.connected_since.is_some_and(|since| ns.saturating_sub(since) >= GRACE)
    }

    /// One tracking sample came. `real_mask` has the controllers that the headset tracks: bit 0 for the
    /// left one and bit 1 for the right one. It returns the mask that the game gets: the hands that make a
    /// controller count too. The wish to the headset follows the game's use of tracking.
    pub fn on_sample(&mut self, ns: Nanos, real_mask: u32) -> u32 {
        self.real = [real_mask & 1 != 0, real_mask & 2 != 0];
        if !self.link.is_connected() {
            self.connected_since = None;
        } else if self.connected_since.is_none() {
            self.connected_since = Some(ns);
        }
        let making = self.making_controllers(ns);
        let need = making && self.offered.contains(Features::HANDS) && self.need.update(ns, real_mask == 3);
        if !making {
            self.need = HandNeed::default();
        }
        // The hand interaction profile is made of the hands
        let hands = need || self.link.wants_hand_interaction();
        let wish = self.link.requested_tracking().union(if hands { Features::HANDS } else { Features::NONE });
        if wish != self.wished {
            self.wished = wish;
            self.headset.set_tracking_wish(wish);
        }
        let mut mask = real_mask;
        for hand in 0..2 {
            let fresh = self.seen[hand].is_some_and(|seen| ns.saturating_sub(seen) <= HAND_FRESH);
            if making && !self.real[hand] && fresh {
                mask |= 1 << hand;
            } else {
                self.release(hand);
            }
        }
        mask
    }

    /// Data of a feature came from the headset.
    pub fn on_tracked(&mut self, tracked: Tracked) {
        let requested = self.link.requested_tracking();
        match tracked {
            Tracked::Hands(hands) => {
                let ns = hands.sample_time;
                let interaction = self.link.wants_hand_interaction();
                for (index, hand) in [hands.left, hands.right].into_iter().enumerate() {
                    let hand = hand.and_then(|hand| hand.joints().ok().map(|joints| (hand, joints)));
                    if requested.contains(Features::HANDS) {
                        let root = hand.as_ref().map(|(hand, _)| motion(&hand.root));
                        self.link.hand(ns, index as u32, root.as_ref().zip(hand.as_ref().map(|(_, joints)| joints)));
                    }
                    if interaction {
                        self.on_hand_interaction(ns, index, hand.as_ref());
                    }
                    self.on_hand(ns, index, hand.as_ref());
                }
            }
            Tracked::Body(body) => {
                if requested.contains(Features::BODY) || requested.contains(Features::FULL_BODY) {
                    match body.joints() {
                        Ok(joints) => self.link.body(body.sample_time, &motion(&body.root), &body, &joints),
                        Err(e) => warn!("Tracking: a body that does not unpack: {e}"),
                    }
                }
            }
            Tracked::Face(face) => {
                if requested.contains(Features::FACE) {
                    match face.weights() {
                        Ok(weights) => self.link.face(face.sample_time, &face, &weights),
                        Err(e) => warn!("Tracking: a face that does not unpack: {e}"),
                    }
                }
            }
            Tracked::Eyes(eyes) => {
                if requested.contains(Features::EYES) {
                    self.link.eyes(eyes.sample_time, &eyes);
                }
            }
        }
    }

    /// A hand sample for a game that binds the hand interaction profile. A pinch or a grasp at a
    /// panel of the headset's app reads 0 for the game, as the trigger of a controller made of a
    /// hand does.
    fn on_hand_interaction(&mut self, ns: Nanos, index: usize, hand: Option<&(Hand, [Joint; HAND_JOINTS])>) {
        self.interaction_ns = self.interaction_ns.max(ns);
        let Some((hand, joints)) = hand else {
            self.link.hand_interaction(ns, index as u32, None);
            return;
        };
        let head = self.headset.motion(Device::Head, ns).map(|m| m.pose);
        let mut interaction = hand_interaction::interaction(index == 0, &motion(&hand.root), joints, head.as_ref());
        let (pinch, grasp) = self.interaction_mask.apply(
            index,
            self.held[index],
            interaction.values[hand_interaction::PINCH],
            interaction.values[hand_interaction::GRASP],
        );
        interaction.values[hand_interaction::PINCH] = pinch;
        interaction.values[hand_interaction::AIM_ACTIVATE] = pinch;
        interaction.values[hand_interaction::GRASP] = grasp;
        self.link.hand_interaction(ns, index as u32, Some(&interaction));
    }

    /// A hand sample: it may make the controller of that hand.
    fn on_hand(&mut self, ns: Nanos, index: usize, hand: Option<&(Hand, [Joint; HAND_JOINTS])>) {
        let Some((hand, joints)) = hand else {
            self.seen[index] = None;
            self.release(index);
            return;
        };
        let head = self.headset.motion(Device::Head, ns).map(|m| m.pose);
        let controller = hand_controller::controller(index == 0, &motion(&hand.root), joints, head.as_ref());
        self.seen[index] = Some(ns);
        if !self.making_controllers(ns) || self.real[index] {
            return;
        }
        self.link.controller(ns, index as u32, &controller.grip, Some(&controller.aim));
        let (trigger, squeeze) = self.pinch_mask.apply(index, self.held[index], controller.trigger, controller.squeeze);
        let values = [trigger, squeeze, f32::from(trigger > TOUCHING)];
        let last = self.made[index].get_or_insert_with(|| {
            info!("Tracking: the {} hand makes the game's controller", if index == 0 { "left" } else { "right" });
            [f32::NAN; 3]
        });
        for (k, value) in values.into_iter().enumerate() {
            // The runtime keeps a value until a new one comes, so only a change goes out
            if last[k].is_nan() || (last[k] - value).abs() >= 0.01 || (value == 0.0) != (last[k] == 0.0) {
                last[k] = value;
                self.link.button(IDS[index][k], k < 2, value);
            }
        }
    }

    /// The hand stops making the controller. The values that it left in the runtime go back to 0,
    /// because a real controller reports a change only when it moves.
    fn release(&mut self, index: usize) {
        if self.made[index].take().is_some() {
            info!("Tracking: the {} hand no longer makes the game's controller", if index == 0 { "left" } else { "right" });
            for (k, id) in IDS[index].into_iter().enumerate() {
                self.link.button(id, k < 2, 0.0);
            }
        }
    }
}

/// The host's motion of a proto motion
pub fn motion(m: &Motion) -> DeviceMotion {
    DeviceMotion {
        pose: Pose { orientation: Quat::from_array(m.pose.orientation), position: Vec3::from_array(m.pose.position) },
        linear_velocity: Vec3::from_array(m.linear_velocity),
        angular_velocity: Vec3::from_array(m.angular_velocity),
    }
}

#[cfg(test)]
#[path = "tests/tracking.rs"]
mod tests;
