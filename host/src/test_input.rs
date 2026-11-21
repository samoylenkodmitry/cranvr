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
//!
//! The hand follows the head: the angles are those of the direction that the headset's real pose faces. With `turn`
//! the game sees the angles from the direction that its own head faces, which is the one that the headset's picture
//! shows.

use crate::pose::{DeviceMotion, Pose as RoomPose};
use cranvr_proto::{
    message::{buttons, Input, Tracking},
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
}

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
    /// held straight has them.
    pub fn apply_tracking(&self, tracking: &mut Tracking) {
        let motion = self.motion(&tracking.head.pose);
        tracking.right_grip = Some(motion);
        tracking.right_aim = Some(motion);
    }

    /// The rotation of the game's room.
    fn room(&self) -> Quat {
        Quat::from_rotation_y(self.turn.to_radians())
    }

    /// Replaces the right hand of an input sample.
    pub fn apply_input(&self, input: &mut Input) {
        let hand = &mut input.hands[1];
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
}
