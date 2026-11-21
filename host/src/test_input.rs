//! A hand for a test run with nobody in the headset. `CRANVR_TEST_INPUT=FILE` makes the host replace the right
//! controller that the client reports with the one that the file describes, so that a test can press the buttons
//! of a game's menu while a headset (or the probe) only shows the picture. The host reads the file about every
//! 30 ms. A host without the variable, or with no file, passes every sample as it came.
//!
//! The file holds `NAME=VALUE` words separated by spaces, for example `yaw=-10 pitch=-5 trigger=1`:
//!
//! - `yaw`, `pitch`: where the controller points, in degrees from the direction that the head faces. A positive
//!   yaw turns to the left and a positive pitch turns up. The default is 0.
//! - `trigger`: the trigger's pull from 0 to 1 (a pull of 0.7 or more clicks it). The default is 0.
//! - `primary`, `secondary`: 1 presses A or B. `stick_x`, `stick_y`: the stick from -1 to 1. `stick_click`: 1 presses
//!   the stick down, the click that a game written for the Vive wand's trackpad reads.
//! - `grip`: the grip's pull from 0 to 1 (a pull of 0.7 or more clicks it). The default is 0.
//! - `turn`: the game's room turns by this many degrees about the vertical axis through the head (a positive turn
//!   to the left): the game sees the head and both controllers turned, and the picture that the headset shows is the
//!   one for the headset's real pose. A game that puts its panels at the side of a headset that lies on a desk then
//!   shows them in front of it. The turn changes only the poses that go to the game, not the poses of the frames.
//! - `x`, `y`, `z`: where the controller's grip sits, in meters right, up and forward of the head, in the head's
//!   axes. The default is 0.2 right, 0.35 below and 0.3 in front.
//! - `hand`: 1 makes the right hand a hand that the headset tracks, with its 26 joints, in place of the controller.
//!   The right controller is then gone, and the client's own hands are left out. The hand points its fingers along
//!   the direction of `yaw` and `pitch`, with the palm down. `x`, `y` and `z` place its palm joint. A game reads it
//!   through `XR_EXT_hand_tracking` or the hand interaction profile of `XR_EXT_hand_interaction`.
//! - `pinch`: with `hand=1`, how far thumb and index finger close, from 0 (6 cm between the tips) to 1 (1.8 cm, which
//!   is touching tips). The distance is linear in the value, as the host's pinch is (`hand_controller.rs`).
//! - `grip`: with `hand=1`, how far middle, ring and little finger close into a fist, from 0 to 1.
//! - `roll`: with `hand=1`, the hand turns by this many degrees about the direction of its fingers. 90 turns the
//!   palm of the right hand to the left, with the thumb up, as when both palms face each other.
//! - `lost`: with `hand=1`, 1 makes the cameras lose the hand: the headset tracks neither the hand nor a controller.
//!
//! The hand follows the head: the angles are those of the direction that the headset's real pose faces. With `turn`
//! the game sees the angles from the direction that its own head faces, which is the one that the headset's picture
//! shows.

use crate::{
    hand_controller::{PINCH_CLOSED, PINCH_OPEN},
    pose::{DeviceMotion, Pose as RoomPose},
};
use cranvr_proto::{
    message::{buttons, Input, Tracking},
    tracking::{Hand as TrackedHand, Hands, Joint, HAND_JOINTS},
    wire::{Motion, Pose},
};
use glam::{Quat, Vec3};
use std::{
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const REREAD: Duration = Duration::from_millis(30);
/// The pull at which the trigger clicks.
const CLICK: f32 = 0.7;

/// What the file says.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hand {
    turn: f32,
    yaw: f32,
    pitch: f32,
    trigger: f32,
    grip: f32,
    primary: bool,
    secondary: bool,
    stick_click: bool,
    stick: [f32; 2],
    at: [f32; 3],
    /// The right hand is a tracked hand, not a controller
    tracked_hand: bool,
    pinch: f32,
    roll: f32,
    lost: bool,
}

/// Where the joints of the test hand lie from its palm joint, in the hand's axes (+x to the little finger of the
/// right hand, +y out of the back of the hand, +z toward the wrist), in the order of XR_EXT_hand_tracking. The thumb
/// tip follows `pinch` and the tips of middle, ring and little finger follow `grip`.
const HAND_SHAPE: [[f32; 3]; HAND_JOINTS] = [
    [0.0, 0.0, 0.0],
    [0.0, 0.0, 0.04],
    [-0.025, -0.01, 0.02],
    [-0.04, -0.015, -0.005],
    [-0.045, -0.015, -0.035],
    [-0.02, -0.06, -0.115],
    [-0.02, 0.0, 0.03],
    [-0.02, 0.0, -0.03],
    [-0.02, 0.0, -0.07],
    [-0.02, 0.0, -0.095],
    [-0.02, 0.0, -0.115],
    [0.0, 0.0, 0.03],
    [0.0, 0.0, -0.035],
    [0.0, 0.0, -0.08],
    [0.0, 0.0, -0.105],
    [0.0, 0.0, -0.127],
    [0.018, 0.0, 0.03],
    [0.018, 0.0, -0.03],
    [0.018, 0.0, -0.07],
    [0.018, 0.0, -0.095],
    [0.018, 0.0, -0.115],
    [0.034, 0.0, 0.025],
    [0.034, 0.0, -0.025],
    [0.034, 0.0, -0.06],
    [0.034, 0.0, -0.083],
    [0.034, 0.0, -0.102],
];
/// Where a fingertip of a closed fist lies from the palm joint, below the palm: the tip keeps its own x
const FIST_TIP: [f32; 2] = [-0.03, -0.01];

impl Default for Hand {
    fn default() -> Self {
        Self {
            turn: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            trigger: 0.0,
            grip: 0.0,
            primary: false,
            secondary: false,
            stick_click: false,
            stick: [0.0; 2],
            at: [0.2, -0.35, -0.3],
            tracked_hand: false,
            pinch: 0.0,
            roll: 0.0,
            lost: false,
        }
    }
}

impl Hand {
    /// The words of one file. A word that is no `NAME=VALUE` of a known name is left out.
    pub fn parse(text: &str) -> Self {
        let mut hand = Self::default();
        for word in text.split_whitespace() {
            let Some((name, value)) = word.split_once('=') else { continue };
            let Ok(value) = value.parse::<f32>() else { continue };
            match name {
                "turn" => hand.turn = value,
                "yaw" => hand.yaw = value,
                "pitch" => hand.pitch = value,
                "trigger" => hand.trigger = value.clamp(0.0, 1.0),
                "grip" => hand.grip = value.clamp(0.0, 1.0),
                "primary" => hand.primary = value != 0.0,
                "secondary" => hand.secondary = value != 0.0,
                "stick_click" => hand.stick_click = value != 0.0,
                "stick_x" => hand.stick[0] = value.clamp(-1.0, 1.0),
                "stick_y" => hand.stick[1] = value.clamp(-1.0, 1.0),
                // The right of the head is +x, up is +y and forward is -z, as in the poses of the protocol
                "x" => hand.at[0] = value,
                "y" => hand.at[1] = value,
                "z" => hand.at[2] = -value,
                "hand" => hand.tracked_hand = value != 0.0,
                "pinch" => hand.pinch = value.clamp(0.0, 1.0),
                "roll" => hand.roll = value,
                "lost" => hand.lost = value != 0.0,
                _ => {}
            }
        }
        hand
    }

    /// The right controller's motion for a head at `head`: the grip at the offset in the head's axes, the aim along the
    /// direction that the angles name. The velocities are zero.
    fn motion(&self, head: &Pose) -> Motion {
        let turn = Quat::from_array(head.orientation);
        let aim = turn * Quat::from_rotation_y(self.yaw.to_radians()) * Quat::from_rotation_x(self.pitch.to_radians());
        let at = Vec3::from_array(head.position) + turn * Vec3::from_array(self.at);
        Motion { pose: Pose { orientation: aim.to_array(), position: at.to_array() }, linear_velocity: [0.0; 3], angular_velocity: [0.0; 3] }
    }

    /// Replaces the right hand of a tracking sample. The grip and the aim are the same pose, as a controller that is
    /// held straight has them. A tracked hand leaves no controller.
    pub fn apply_tracking(&self, tracking: &mut Tracking) {
        if self.tracked_hand {
            tracking.right_grip = None;
            tracking.right_aim = None;
            return;
        }
        let motion = self.motion(&tracking.head.pose);
        tracking.right_grip = Some(motion);
        tracking.right_aim = Some(motion);
    }

    /// The joints of the tracked right hand for a head at `head`, with the wrist as the root. The hand stands still.
    fn joints(&self, head: &Pose) -> (Motion, [Joint; HAND_JOINTS]) {
        let motion = self.motion(head);
        // The palm faces down with no roll; a positive roll turns it to the left about the fingers' direction (-z)
        let turn = Quat::from_array(motion.pose.orientation) * Quat::from_rotation_z(-self.roll.to_radians());
        let palm = Vec3::from_array(motion.pose.position);
        let gap = PINCH_OPEN - self.pinch * (PINCH_OPEN - PINCH_CLOSED);
        let mut shape = HAND_SHAPE.map(Vec3::from_array);
        shape[5] = shape[10] + Vec3::new(0.0, -gap, 0.0);
        for tip in [15, 20, 25] {
            let fist = Vec3::new(shape[tip].x, FIST_TIP[0], FIST_TIP[1]);
            shape[tip] = shape[tip].lerp(fist, self.grip);
        }
        let joints = shape.map(|at| {
            Joint::tracked(Pose { orientation: turn.to_array(), position: (palm + turn * at).to_array() })
        });
        let root = Motion { pose: joints[1].pose, linear_velocity: [0.0; 3], angular_velocity: [0.0; 3] };
        (root, joints)
    }

    /// The rotation of the game's room.
    fn room(&self) -> Quat {
        Quat::from_rotation_y(self.turn.to_radians())
    }

    /// The hands of a tracking sample for a tracked test hand
    fn hands(&self, tracking: &Tracking) -> Option<Hands> {
        if !self.tracked_hand {
            return None;
        }
        let right = (!self.lost).then(|| {
            let (root, joints) = self.joints(&tracking.head.pose);
            TrackedHand::new(root, &joints)
        });
        Some(Hands { sample_time: tracking.sample_time, display_time: tracking.display_time, left: None, right })
    }

    /// Replaces the right hand of an input sample. A tracked hand has no buttons.
    pub fn apply_input(&self, input: &mut Input) {
        let hand = &mut input.hands[1];
        if self.tracked_hand {
            *hand = Default::default();
            return;
        }
        hand.trigger = self.trigger;
        hand.grip = self.grip;
        hand.stick = self.stick;
        hand.buttons = (buttons::TRIGGER_CLICK * u32::from(self.trigger >= CLICK))
            | (buttons::GRIP_CLICK * u32::from(self.grip >= CLICK))
            | (buttons::PRIMARY * u32::from(self.primary))
            | (buttons::SECONDARY * u32::from(self.secondary))
            | (buttons::STICK_CLICK * u32::from(self.stick_click));
        hand.touches = (buttons::TRIGGER_CLICK * u32::from(self.trigger > 0.0))
            | (buttons::PRIMARY * u32::from(self.primary))
            | (buttons::STICK_CLICK * u32::from(self.stick_click));
    }
}

/// Turns a pose that goes to the game about the head's place `about`, by the room's turn `room`.
fn turned(motion: &DeviceMotion, room: Quat, about: Vec3) -> DeviceMotion {
    DeviceMotion {
        pose: RoomPose { orientation: room * motion.pose.orientation, position: about + room * (motion.pose.position - about) },
        linear_velocity: room * motion.linear_velocity,
        angular_velocity: room * motion.angular_velocity,
    }
}

/// Where the head was in the last pose that went to the game: the controllers of the same sample turn about it.
fn head_place() -> &'static Mutex<Vec3> {
    static PLACE: OnceLock<Mutex<Vec3>> = OnceLock::new();
    PLACE.get_or_init(|| Mutex::new(Vec3::ZERO))
}

/// The head pose that goes to the game, turned when a test turns the room.
pub fn game_head(head: &DeviceMotion) -> DeviceMotion {
    *head_place().lock().unwrap() = head.pose.position;
    match current() {
        Some(hand) if hand.turn != 0.0 => turned(head, hand.room(), head.pose.position),
        _ => *head,
    }
}

/// A controller pose that goes to the game, turned with the room about the head of the same sample.
pub fn game_controller(motion: &DeviceMotion) -> DeviceMotion {
    match current() {
        Some(hand) if hand.turn != 0.0 => turned(motion, hand.room(), *head_place().lock().unwrap()),
        _ => *motion,
    }
}

/// The file that the variable names, and the last words read from it.
struct Source {
    path: Option<PathBuf>,
    read_at: Option<Instant>,
    hand: Option<Hand>,
}

fn source() -> &'static Mutex<Source> {
    static SOURCE: OnceLock<Mutex<Source>> = OnceLock::new();
    SOURCE.get_or_init(|| {
        let path = std::env::var_os("CRANVR_TEST_INPUT").filter(|path| !path.is_empty()).map(PathBuf::from);
        Mutex::new(Source { path, read_at: None, hand: None })
    })
}

/// The hand that the file describes now, if the host has the variable and the file exists.
fn current() -> Option<Hand> {
    let mut source = source().lock().unwrap();
    let path = source.path.clone()?;
    if source.read_at.is_none_or(|at| at.elapsed() >= REREAD) {
        source.read_at = Some(Instant::now());
        source.hand = std::fs::read_to_string(path).ok().filter(|text| !text.trim().is_empty()).map(|text| Hand::parse(&text));
    }
    source.hand
}

/// Replaces the right hand of a tracking sample when a test hand is on.
pub fn tracking(tracking: &mut Tracking) {
    if let Some(hand) = current() {
        hand.apply_tracking(tracking);
    }
}

/// Replaces the right hand of an input sample when a test hand is on.
pub fn input(input: &mut Input) {
    if let Some(hand) = current() {
        hand.apply_input(input);
    }
}

/// Whether the test hand is a tracked hand: the client's own hands are then left out, so that the runtime hears one
/// right hand.
pub fn replaces_hands() -> bool {
    current().is_some_and(|hand| hand.tracked_hand)
}

/// The hands of a tracking sample when the test hand is a tracked hand: the right hand from the words, no left hand.
/// The poses lie in the headset's room, as the sample's do.
pub fn hands(tracking: &Tracking) -> Option<Hands> {
    current()?.hands(tracking)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_of_a_file_set_the_hand_and_unknown_words_are_left_out() {
        let hand = Hand::parse("yaw=-10 pitch=5.5 trigger=2 primary=1 junk nothing=3 stick_x=-4 z=0.5 grip=0.8 stick_click=1\n");
        assert_eq!((hand.yaw, hand.pitch), (-10.0, 5.5));
        // The pull stays between 0 and 1, the stick between -1 and 1
        assert_eq!(hand.trigger, 1.0);
        assert_eq!(hand.grip, 0.8);
        assert_eq!(hand.stick, [-1.0, 0.0]);
        assert!(hand.primary && !hand.secondary && hand.stick_click);
        assert_eq!(hand.at, [0.2, -0.35, -0.5]);
        assert_eq!(Hand::parse(""), Hand::default());
        assert_eq!(Hand::parse("turn=40").turn, 40.0);
    }

    #[test]
    fn the_hand_points_where_the_angles_say_from_the_heads_direction() {
        // A head one meter and a half up, turned a quarter turn to the left
        let head = Pose { orientation: Quat::from_rotation_y(90f32.to_radians()).to_array(), position: [0.0, 1.5, 0.0] };
        let hand = Hand::parse("yaw=0 pitch=0");
        let motion = hand.motion(&head);
        let forward = Quat::from_array(motion.pose.orientation) * Vec3::NEG_Z;
        // The head faces -x, so the controller points at -x
        assert!((forward - Vec3::NEG_X).length() < 1e-5, "{forward:?}");
        // The grip sits at the offset in the head's axes: 0.2 to the head's right (-z here) and 0.3 ahead (-x)
        let at = Vec3::from_array(motion.pose.position);
        assert!((at - Vec3::new(-0.3, 1.15, -0.2)).length() < 1e-5, "{at:?}");
        // A positive pitch turns up and a positive yaw turns to the left, relative to the head
        let up = Quat::from_array(Hand::parse("pitch=30").motion(&Pose { orientation: [0.0, 0.0, 0.0, 1.0], position: [0.0; 3] }).pose.orientation) * Vec3::NEG_Z;
        assert!(up.y > 0.49 && up.y < 0.51, "{up:?}");
        let left = Quat::from_array(Hand::parse("yaw=30").motion(&Pose { orientation: [0.0, 0.0, 0.0, 1.0], position: [0.0; 3] }).pose.orientation) * Vec3::NEG_Z;
        assert!(left.x < -0.49 && left.x > -0.51, "{left:?}");
    }

    #[test]
    fn the_rooms_turn_moves_the_head_and_the_hands_about_the_head() {
        let head = DeviceMotion {
            pose: RoomPose { orientation: Quat::IDENTITY, position: Vec3::new(0.0, 1.6, 0.0) },
            linear_velocity: Vec3::ZERO,
            angular_velocity: Vec3::ZERO,
        };
        let hand = DeviceMotion {
            pose: RoomPose { orientation: Quat::IDENTITY, position: Vec3::new(0.0, 1.4, -1.0) },
            linear_velocity: Vec3::new(0.0, 0.0, -1.0),
            angular_velocity: Vec3::ZERO,
        };
        // A turn of 90 degrees to the left: ahead (-z) becomes -x
        let room = Hand::parse("turn=90").room();
        let turned_head = turned(&head, room, head.pose.position);
        assert!((turned_head.pose.orientation * Vec3::NEG_Z - Vec3::NEG_X).length() < 1e-5);
        assert_eq!(turned_head.pose.position, head.pose.position);
        let turned_hand = turned(&hand, room, head.pose.position);
        assert!((turned_hand.pose.position - Vec3::new(-1.0, 1.4, 0.0)).length() < 1e-5, "{:?}", turned_hand.pose.position);
        assert!((turned_hand.linear_velocity - Vec3::NEG_X).length() < 1e-5);
    }

    #[test]
    fn the_trigger_clicks_at_seven_tenths_and_only_the_right_hand_changes() {
        let mut input = Input::default();
        input.hands[0].trigger = 0.25;
        Hand::parse("trigger=0.6").apply_input(&mut input);
        assert_eq!(input.hands[0].trigger, 0.25);
        assert_eq!(input.hands[1].trigger, 0.6);
        assert_eq!(input.hands[1].buttons & buttons::TRIGGER_CLICK, 0);
        Hand::parse("trigger=0.8 secondary=1").apply_input(&mut input);
        assert_eq!(input.hands[1].buttons, buttons::TRIGGER_CLICK | buttons::SECONDARY);
    }

    #[test]
    fn a_tracking_sample_gets_a_right_hand_at_the_head() {
        let mut tracking = Tracking::default();
        tracking.head.pose = Pose { orientation: [0.0, 0.0, 0.0, 1.0], position: [1.0, 1.6, 2.0] };
        tracking.left_grip = Some(Motion::default());
        Hand::parse("").apply_tracking(&mut tracking);
        let grip = tracking.right_grip.unwrap();
        assert_eq!(grip, tracking.right_aim.unwrap());
        assert_eq!(grip.pose.position, [1.2, 1.25, 1.7]);
        // The left hand is the client's
        assert_eq!(tracking.left_grip, Some(Motion::default()));
    }

    #[test]
    fn a_tracked_test_hand_replaces_the_right_controller_with_joints() {
        let mut tracking = Tracking::default();
        tracking.head.pose = Pose { orientation: [0.0, 0.0, 0.0, 1.0], position: [0.0, 1.6, 0.0] };
        tracking.right_grip = Some(Motion::default());
        tracking.right_aim = Some(Motion::default());
        let hand = Hand::parse("hand=1 pinch=0.5 grip=1 x=0.2 y=-0.35 z=0.3");
        hand.apply_tracking(&mut tracking);
        assert_eq!((tracking.right_grip, tracking.right_aim), (None, None));
        let mut input = Input::default();
        input.hands[1].trigger = 1.0;
        hand.apply_input(&mut input);
        assert_eq!(input.hands[1], Default::default());

        let (root, joints) = hand.joints(&tracking.head.pose);
        // The palm joint lies where the words say, the wrist 4 cm behind it
        assert!((Vec3::from_array(joints[0].pose.position) - Vec3::new(0.2, 1.25, -0.3)).length() < 1e-6);
        assert_eq!(root.pose, joints[1].pose);
        // The host reads back the pinch of the words and a closed fist
        assert!((crate::hand_controller::pinch(&joints) - 0.5).abs() < 1e-4, "{}", crate::hand_controller::pinch(&joints));
        assert_eq!(crate::hand_controller::squeeze(&joints), 1.0);
        let open = Hand::parse("hand=1").joints(&tracking.head.pose).1;
        assert!(crate::hand_controller::pinch(&open) < 1e-4 && crate::hand_controller::squeeze(&open) == 0.0);
        let touching = Hand::parse("hand=1 pinch=1").joints(&tracking.head.pose).1;
        assert!(crate::hand_controller::pinch(&touching) > 0.9999);
        // A roll of 90 degrees turns the thumb of the right hand up
        let rolled = Hand::parse("hand=1 roll=90").joints(&tracking.head.pose).1;
        let thumb = Vec3::from_array(rolled[3].pose.position) - Vec3::from_array(rolled[0].pose.position);
        assert!(thumb.y > 0.03 && thumb.x.abs() < 0.02, "{thumb:?}");
    }

    #[test]
    fn a_lost_test_hand_is_a_sample_without_a_right_hand() {
        let mut tracking = Tracking::default();
        tracking.head.pose = Pose { orientation: [0.0, 0.0, 0.0, 1.0], position: [0.0, 1.6, 0.0] };
        tracking.sample_time = 7;
        let hands = Hand::parse("hand=1").hands(&tracking).unwrap();
        assert_eq!((hands.sample_time, hands.left.is_none()), (7, true));
        assert!(hands.right.unwrap().joints().is_ok());
        let lost = Hand::parse("hand=1 lost=1").hands(&tracking).unwrap();
        assert!(lost.left.is_none() && lost.right.is_none());
        // A controller makes no hands
        assert!(Hand::parse("pinch=1").hands(&tracking).is_none());
    }
}
