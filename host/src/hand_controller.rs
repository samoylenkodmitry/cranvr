//! A controller made of a tracked hand, for a game that has no hand code.
//!
//! A game that uses only the Oculus Touch controller profile knows nothing of hands. When the
//! headset tracks a hand and no controller, the host turns the hand into what a controller of that
//! hand would report:
//! - the grip pose sits at the palm joint with the axes of OpenXR's grip pose, which the game puts its
//!   controller model at (see `grip`);
//! - the aim pose points from the shoulder through the pinching fingers, which is how a hand points
//!   at a menu on headsets that offer hand tracking;
//! - pinching thumb and index finger pulls the trigger;
//! - closing the other three fingers presses the squeeze (grip) button.
//!
//! A game that creates a hand tracker has hand code of its own. It gets no controller made of a hand,
//! or its hand would count twice. The choice follows the game's use of the tracker (see
//! `tracking.rs`). It never follows the game's name.

use crate::pose::{DeviceMotion, Pose, Quat, Vec3};
use cranvr_proto::tracking::{Joint, HAND_JOINTS};

/// The joints of `XR_EXT_hand_tracking` that the conversion reads
pub const PALM: usize = 0;
pub const THUMB_TIP: usize = 5;
pub const INDEX_TIP: usize = 10;
pub const MIDDLE_TIP: usize = 15;
pub const RING_TIP: usize = 20;
pub const LITTLE_TIP: usize = 25;

/// The distance between the thumb tip and the index tip at which the trigger is fully pulled, and
/// the one at which it is released, in meters. Touching tips lie about 18 mm apart because each
/// joint sits in the middle of its fingertip.
pub const PINCH_CLOSED: f32 = 0.018;
pub const PINCH_OPEN: f32 = 0.060;
/// The distance between a fingertip and the palm's center at which the finger is curled into a fist,
/// and the one at which it is stretched, in meters.
const CURLED: f32 = 0.050;
const STRETCHED: f32 = 0.100;

/// Where the shoulder lies in the head's frame turned to face the way the head looks level: the
/// hand's side and below the head, a little behind it. The headset's own pointer pose uses the same
/// idea, because the arm swings about the shoulder.
const SHOULDER: Vec3 = Vec3::new(0.17, -0.25, 0.05);

/// What a hand gives a game in place of a controller.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandController {
    pub grip: DeviceMotion,
    pub aim: DeviceMotion,
    /// From 0 to 1
    pub trigger: f32,
    /// From 0 to 1
    pub squeeze: f32,
}

fn position(joint: &Joint) -> Vec3 {
    Vec3::from_array(joint.pose.position)
}

fn ramp(value: f32, at_one: f32, at_zero: f32) -> f32 {
    ((at_zero - value) / (at_zero - at_one)).clamp(0.0, 1.0)
}

/// How far thumb and index finger are pinched, from 0 for an open hand to 1 for touching tips
pub fn pinch(joints: &[Joint; HAND_JOINTS]) -> f32 {
    ramp(position(&joints[THUMB_TIP]).distance(position(&joints[INDEX_TIP])), PINCH_CLOSED, PINCH_OPEN)
}

/// How far middle, ring and little finger are curled toward the palm, from 0 to 1
pub fn squeeze(joints: &[Joint; HAND_JOINTS]) -> f32 {
    let palm = position(&joints[PALM]);
    let curl = [MIDDLE_TIP, RING_TIP, LITTLE_TIP].map(|tip| ramp(position(&joints[tip]).distance(palm), CURLED, STRETCHED));
    curl.iter().sum::<f32>() / curl.len() as f32
}

/// The orientation whose -Z axis points along `forward` and whose Y axis leans up as far as it can
fn looking_along(forward: Vec3, fallback: Quat) -> Quat {
    let z = -forward;
    let x = Vec3::Y.cross(z);
    if x.length_squared() < 1e-6 {
        return fallback;
    }
    let x = x.normalize();
    Quat::from_mat3(&glam::Mat3::from_cols(x, z.cross(x), z)).normalize()
}

/// The pose `pose` of a point of the hand, with the velocities that it has as a part of the hand: the wrist's
/// velocities plus the turn about the wrist.
pub fn on_hand(root: &DeviceMotion, pose: Pose) -> DeviceMotion {
    DeviceMotion {
        pose,
        linear_velocity: root.linear_velocity + root.angular_velocity.cross(pose.position - root.pose.position),
        angular_velocity: root.angular_velocity,
    }
}

/// The palm joint's pose
pub fn palm(joints: &[Joint; HAND_JOINTS]) -> Pose {
    Pose { orientation: Quat::from_array(joints[PALM].pose.orientation).normalize(), position: position(&joints[PALM]) }
}

/// The grip pose's turn from the palm joint's. XR_EXT_hand_tracking gives the palm joint a +Y out
/// of the back of the hand and a +Z along the middle finger's metacarpal toward the wrist. Its +X
/// then points to the little finger of the right hand and to the thumb of the left hand. OpenXR's
/// grip pose has its +X out of the back of the left hand and into the palm of the right hand, and
/// its -Z from the little finger to the index finger. So the grip's +X is the palm's +Y for the
/// right hand and the palm's -Y for the left hand. The grip's -Z is the palm's -X for the right hand
/// and its +X for the left hand.
fn grip_turn(left: bool) -> Quat {
    let side = if left { -1.0 } else { 1.0 };
    let x = Vec3::new(0.0, side, 0.0);
    let z = Vec3::new(side, 0.0, 0.0);
    Quat::from_mat3(&glam::Mat3::from_cols(x, z.cross(x), z)).normalize()
}

/// The grip pose of a tracked hand: at the palm joint, turned to the axes of OpenXR's grip pose.
/// A game puts the model of a controller there. With the palm joint's own axes the model's front
/// would point along the fingers, not across the hand. The controller made of a hand and the hand
/// interaction profile (`hand_interaction.rs`) both give a game this pose.
pub fn grip(left: bool, joints: &[Joint; HAND_JOINTS]) -> Pose {
    let palm = palm(joints);
    Pose { orientation: (palm.orientation * grip_turn(left)).normalize(), position: palm.position }
}

/// The controller that `hand` stands for. `root` is the wrist's pose with its velocities, `joints`
/// are in the same room, `left` says which hand it is, and `head` is the head's pose when known. The
/// aim pose needs the head for the shoulder. Without it the aim pose follows the palm's direction.
pub fn controller(left: bool, root: &DeviceMotion, joints: &[Joint; HAND_JOINTS], head: Option<&Pose>) -> HandController {
    let grip = on_hand(root, grip(left, joints));
    HandController { grip, aim: aim(left, root, joints, head), trigger: pinch(joints), squeeze: squeeze(joints) }
}

/// The aim pose of a hand: it points from the shoulder through the pinching fingers, and it starts
/// between them. Without the head it points along the palm.
pub fn aim(left: bool, root: &DeviceMotion, joints: &[Joint; HAND_JOINTS], head: Option<&Pose>) -> DeviceMotion {
    let palm = palm(joints);
    // The pointing ray starts between the pinching fingertips
    let target = (position(&joints[THUMB_TIP]) + position(&joints[INDEX_TIP])) * 0.5;
    let forward = match head {
        Some(head) => {
            // The shoulder follows the head's turn about the vertical axis, not its nod
            let ahead = head.orientation * Vec3::NEG_Z;
            let yaw = (-ahead.x).atan2(-ahead.z);
            let side = if left { -1.0 } else { 1.0 };
            let shoulder = head.position + Quat::from_rotation_y(yaw) * (SHOULDER * Vec3::new(side, 1.0, 1.0));
            (target - shoulder).try_normalize()
        }
        None => None,
    }
    .unwrap_or_else(|| palm.orientation * Vec3::NEG_Z);
    on_hand(root, Pose { orientation: looking_along(forward, palm.orientation), position: target })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joint_at(position: Vec3) -> Joint {
        Joint::tracked(cranvr_proto::wire::Pose { orientation: [0.0, 0.0, 0.0, 1.0], position: position.to_array() })
    }

    /// A hand at `wrist` with its fingers along -Z: the palm 4 cm in front of the wrist, the
    /// fingertips `reach` meters further, the thumb and index tips `gap` meters apart.
    fn hand(wrist: Vec3, reach: f32, gap: f32) -> [Joint; HAND_JOINTS] {
        let mut joints = [joint_at(wrist); HAND_JOINTS];
        let tip = |side: f32, below: f32| joint_at(wrist + Vec3::new(side, -below, -0.04 - reach));
        joints[PALM] = joint_at(wrist + Vec3::new(0.0, 0.0, -0.04));
        // The index tip lies on the palm's line, the thumb tip is `gap` below it
        joints[INDEX_TIP] = tip(0.0, 0.0);
        joints[THUMB_TIP] = tip(0.0, gap);
        joints[MIDDLE_TIP] = tip(0.01, 0.0);
        joints[RING_TIP] = tip(0.02, 0.0);
        joints[LITTLE_TIP] = tip(0.03, 0.0);
        joints
    }

    fn at(position: Vec3) -> DeviceMotion {
        DeviceMotion { pose: Pose { orientation: Quat::IDENTITY, position }, ..Default::default() }
    }

    #[test]
    fn an_open_hand_pulls_nothing_and_a_pinch_pulls_the_trigger() {
        let wrist = Vec3::new(0.2, 1.2, -0.4);
        let open = hand(wrist, 0.10, 0.08);
        assert_eq!(pinch(&open), 0.0);
        assert_eq!(squeeze(&open), 0.0);
        let pinched = hand(wrist, 0.10, 0.018);
        assert!((pinch(&pinched) - 1.0).abs() < 1e-5);
        // Half way between the two distances
        let half = hand(wrist, 0.10, 0.039);
        assert!((pinch(&half) - 0.5).abs() < 0.01, "{}", pinch(&half));
        // The pinch moves the trigger and leaves the squeeze alone
        let c = controller(false, &at(wrist), &pinched, None);
        assert!((c.trigger - 1.0).abs() < 1e-5 && c.squeeze == 0.0);
    }

    #[test]
    fn a_fist_presses_the_squeeze() {
        let wrist = Vec3::new(-0.2, 1.2, -0.4);
        // Fingertips 4 cm from the palm: curled
        let fist = hand(wrist, 0.0, 0.08);
        assert_eq!(squeeze(&fist), 1.0);
        let c = controller(true, &at(wrist), &fist, None);
        assert_eq!(c.squeeze, 1.0);
        // Fingers half curled give about half
        let half = hand(wrist, 0.075 - 0.0, 0.08);
        assert!(squeeze(&half) > 0.4 && squeeze(&half) < 0.6, "{}", squeeze(&half));
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-5
    }

    #[test]
    fn the_grip_pose_has_the_axes_of_openxr_for_a_palm_facing_down() {
        // The test hand's joints have no turn: the palm faces down and the fingers point along -Z,
        // so the palm joint's +X points to the right. The ray that enters the right palm and the
        // ray that leaves the left palm both point up. The little finger of the right hand lies on
        // the right, so its grip's -Z points to the left. The left hand's grip's -Z points to the
        // right.
        let wrist = Vec3::new(0.2, 1.2, -0.4);
        let joints = hand(wrist, 0.10, 0.08);
        let right = controller(false, &at(wrist), &joints, None).grip.pose.orientation;
        assert!(close(right * Vec3::X, Vec3::Y), "{:?}", right * Vec3::X);
        assert!(close(right * Vec3::NEG_Z, Vec3::NEG_X), "{:?}", right * Vec3::NEG_Z);
        let left = controller(true, &at(wrist), &joints, None).grip.pose.orientation;
        assert!(close(left * Vec3::X, Vec3::NEG_Y), "{:?}", left * Vec3::X);
        assert!(close(left * Vec3::NEG_Z, Vec3::X), "{:?}", left * Vec3::NEG_Z);
        // +Y follows from the right hand rule: toward the wrist for both hands
        assert!(close(right * Vec3::Y, Vec3::Z) && close(left * Vec3::Y, Vec3::Z));
    }

    #[test]
    fn the_grip_pose_turns_with_the_palm() {
        // A right palm turned to face left, as it holds a stick that points up: the grip's +X
        // points to the right and its -Z, from the little finger to the index finger, points up
        let turn = Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2);
        let mut joints = hand(Vec3::ZERO, 0.10, 0.08);
        joints[PALM].pose.orientation = turn.to_array();
        let grip = grip(false, &joints).orientation;
        assert!(close(grip * Vec3::X, Vec3::X), "{:?}", grip * Vec3::X);
        assert!(close(grip * Vec3::NEG_Z, Vec3::Y), "{:?}", grip * Vec3::NEG_Z);
    }

    #[test]
    fn the_grip_pose_is_at_the_palm_and_it_moves_with_the_wrist() {
        let wrist = Vec3::new(0.2, 1.2, -0.4);
        let joints = hand(wrist, 0.10, 0.08);
        let root = DeviceMotion {
            pose: Pose { orientation: Quat::IDENTITY, position: wrist },
            linear_velocity: Vec3::new(0.5, 0.0, 0.0),
            angular_velocity: Vec3::new(0.0, 2.0, 0.0),
        };
        let c = controller(false, &root, &joints, None);
        assert_eq!(c.grip.pose.position, wrist + Vec3::new(0.0, 0.0, -0.04));
        // A turn of 2 rad/s about y moves a point 4 cm in front of the wrist (along -Z) along -X
        // at 0.08 m/s on top of the wrist's own 0.5 m/s: omega x r = (0,2,0) x (0,0,-0.04) = (-0.08,0,0)
        assert!((c.grip.linear_velocity - Vec3::new(0.42, 0.0, 0.0)).length() < 1e-5, "{:?}", c.grip.linear_velocity);
        assert_eq!(c.grip.angular_velocity, root.angular_velocity);
    }

    #[test]
    fn the_aim_pose_points_from_the_shoulder_through_the_fingers() {
        let head = Pose { orientation: Quat::IDENTITY, position: Vec3::new(0.0, 1.6, 0.0) };
        for left in [false, true] {
            let side = if left { -1.0 } else { 1.0 };
            let wrist = Vec3::new(0.25 * side, 1.25, -0.35);
            let joints = hand(wrist, 0.10, 0.018);
            let c = controller(left, &at(wrist), &joints, Some(&head));
            let target = (position(&joints[THUMB_TIP]) + position(&joints[INDEX_TIP])) * 0.5;
            let shoulder = head.position + Vec3::new(0.17 * side, -0.25, 0.05);
            let expected = (target - shoulder).normalize();
            let pointing = c.aim.pose.orientation * Vec3::NEG_Z;
            assert!((pointing - expected).length() < 1e-4, "{pointing:?} {expected:?}");
            assert!((c.aim.pose.position - target).length() < 1e-6);
            // The aim pose does not roll: its X axis stays level
            let x = c.aim.pose.orientation * Vec3::X;
            assert!(x.y.abs() < 1e-4);
            assert!((c.aim.pose.orientation.length() - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn the_shoulder_follows_the_heads_turn_but_not_its_nod() {
        let wrist = Vec3::new(0.25, 1.25, -0.35);
        let joints = hand(wrist, 0.10, 0.018);
        let level = Pose { orientation: Quat::IDENTITY, position: Vec3::new(0.0, 1.6, 0.0) };
        let nodding = Pose { orientation: Quat::from_rotation_x(-0.6), ..level };
        let a = controller(false, &at(wrist), &joints, Some(&level)).aim.pose.orientation;
        let b = controller(false, &at(wrist), &joints, Some(&nodding)).aim.pose.orientation;
        assert!(a.angle_between(b) < 1e-4);
        // A head turned by 90 degrees to the left moves the shoulder to where the hand's side is
        let turned = Pose { orientation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2), ..level };
        let c = controller(false, &at(wrist), &joints, Some(&turned)).aim.pose.orientation;
        assert!(a.angle_between(c) > 0.05);
    }

    #[test]
    fn without_a_head_the_aim_pose_points_along_the_palm() {
        let wrist = Vec3::new(0.0, 1.2, -0.3);
        let joints = hand(wrist, 0.10, 0.08);
        let c = controller(false, &at(wrist), &joints, None);
        // The palm's orientation is the identity in the test hand: forward is -Z
        let pointing = c.aim.pose.orientation * Vec3::NEG_Z;
        assert!((pointing - Vec3::NEG_Z).length() < 1e-5, "{pointing:?}");
    }
}
