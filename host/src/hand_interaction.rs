//! The hand interaction profile of `XR_EXT_hand_interaction`, made of a tracked hand's joints.
//!
//! A game that binds `/interaction_profiles/ext/hand_interaction_ext` reads each hand through four
//! poses and three gestures. The game's runtime asks for them (the hand interaction bit of
//! `TRACKING_REQUEST`), and the host makes them from the 26 joints of `XR_EXT_hand_tracking` that
//! the headset sends. The axes follow the extension's text:
//! - The grip pose is the one of a controller made of a hand (`hand_controller::grip`): it sits at
//!   the palm joint. Its -Z runs from the little finger to the index finger, and its +X leaves the
//!   back of the left hand and enters the palm of the right hand. The palm joint has other axes (its
//!   +Z points to the wrist), so the grip pose turns it.
//! - The aim pose is the one of a controller made of a hand (`hand_controller.rs`): it points from
//!   the shoulder through the pinching fingertips.
//! - The pinch pose sits between the tips of thumb and index finger. Its +Z points back to the
//!   middle of their proximal joints, and its +Y points up while the palms face each other.
//! - The poke pose sits on the skin of the index fingertip and turns with the fingertip.
//!
//! The pinch goes from 0 for an open hand to 1 for touching tips, linear in the distance of the tips.
//! The extension leaves the aim activation gesture to the runtime: here it is the same pinch, which
//! keeps the aim ray still. The grasp says how far middle, ring and little finger close into a
//! fist. A gesture is ready while the headset tracks the joints that it reads, and its value is 0
//! while it is not ready.

use crate::hand_controller::{self, INDEX_TIP, LITTLE_TIP, MIDDLE_TIP, PALM, RING_TIP, THUMB_TIP};
use crate::pose::{DeviceMotion, Pose, Quat, Vec3};
use cranvr_proto::tracking::{joint, Joint, HAND_JOINTS};

pub const THUMB_PROXIMAL: usize = 3;
pub const INDEX_PROXIMAL: usize = 7;
/// The radius of the index fingertip in the runtime's table of joint radii (`kHandJointRadius` in
/// its tracking.cpp). The poke pose lies this far ahead of the fingertip joint.
pub const INDEX_TIP_RADIUS: f32 = 0.006;

/// The poses in the order of the runtime's message
pub const GRIP: usize = 0;
pub const AIM: usize = 1;
pub const PINCH_POSE: usize = 2;
pub const POKE: usize = 3;
/// The gestures in the order of the runtime's message
pub const PINCH: usize = 0;
pub const AIM_ACTIVATE: usize = 1;
pub const GRASP: usize = 2;

/// What one tracked hand gives the hand interaction profile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandInteraction {
    /// Grip, aim, pinch and poke, with the velocities that the hand gives them
    pub poses: [DeviceMotion; 4],
    /// Pinch, aim activation and grasp, from 0 to 1
    pub values: [f32; 3],
    pub ready: [bool; 3],
}

fn position(joint: &Joint) -> Vec3 {
    Vec3::from_array(joint.pose.position)
}

fn orientation(joint: &Joint) -> Quat {
    Quat::from_array(joint.pose.orientation).normalize()
}

/// The headset measured the joint's place, so a gesture may read it
fn placed(joint: &Joint) -> bool {
    let wanted = joint::POSITION_VALID | joint::POSITION_TRACKED;
    joint.flags & wanted == wanted
}

/// The orientation with this +Z and the +Y closest to `up`, or `fallback` when they are parallel
fn with_z_and_up(z: Vec3, up: Vec3, fallback: Quat) -> Quat {
    let Some(z) = z.try_normalize() else { return fallback };
    let Some(y) = (up - z * up.dot(z)).try_normalize() else { return fallback };
    Quat::from_mat3(&glam::Mat3::from_cols(y.cross(z), y, z)).normalize()
}

/// The hand interaction profile of one hand. `root` is the wrist's pose with its velocities,
/// `joints` lie in the same room, `left` says which hand it is and `head` is the head's pose when
/// known (the aim pose needs it for the shoulder).
pub fn interaction(left: bool, root: &DeviceMotion, joints: &[Joint; HAND_JOINTS], head: Option<&Pose>) -> HandInteraction {
    let palm = hand_controller::palm(joints);
    let grip = hand_controller::grip(left, joints);

    let thumb = position(&joints[THUMB_TIP]);
    let index = position(&joints[INDEX_TIP]);
    let between = (thumb + index) * 0.5;
    let knuckles = (position(&joints[THUMB_PROXIMAL]) + position(&joints[INDEX_PROXIMAL])) * 0.5;
    // The palm's +X points to the little finger of the right hand and to the thumb of the left
    // hand, which is up while the palms face each other with the thumbs on top
    let up = palm.orientation * Vec3::X * if left { 1.0 } else { -1.0 };
    let pinch = Pose { orientation: with_z_and_up(knuckles - between, up, palm.orientation), position: between };

    let tip = orientation(&joints[INDEX_TIP]);
    let poke = Pose { orientation: tip, position: index + tip * Vec3::new(0.0, 0.0, -INDEX_TIP_RADIUS) };

    let pinch_ready = placed(&joints[THUMB_TIP]) && placed(&joints[INDEX_TIP]);
    let grasp_ready = [PALM, MIDDLE_TIP, RING_TIP, LITTLE_TIP].iter().all(|&j| placed(&joints[j]));
    let pinched = if pinch_ready { hand_controller::pinch(joints) } else { 0.0 };
    let grasp = if grasp_ready { hand_controller::squeeze(joints) } else { 0.0 };
    let mut out = HandInteraction { poses: [DeviceMotion::default(); 4], values: [0.0; 3], ready: [false; 3] };
    out.poses[GRIP] = hand_controller::on_hand(root, grip);
    out.poses[AIM] = hand_controller::aim(left, root, joints, head);
    out.poses[PINCH_POSE] = hand_controller::on_hand(root, pinch);
    out.poses[POKE] = hand_controller::on_hand(root, poke);
    for (gesture, value, ready) in [(PINCH, pinched, pinch_ready), (AIM_ACTIVATE, pinched, pinch_ready), (GRASP, grasp, grasp_ready)] {
        out.values[gesture] = value;
        out.ready[gesture] = ready;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joint(at: Vec3, turn: Quat) -> Joint {
        Joint::tracked(cranvr_proto::wire::Pose { orientation: turn.to_array(), position: at.to_array() })
    }

    /// A hand with its fingers forward (-z) and turned by `turn` about its palm: the palm joint at
    /// `palm`, the joints of XR_EXT_hand_tracking turned with the hand. With no turn the palm faces
    /// down, so the palm joint's +Y (the back of the hand) points up. The thumb lies on the side of
    /// the body's middle: -x for the right hand, +x for the left hand. `gap` is the distance of the
    /// thumb tip from the index tip.
    fn hand(left: bool, palm: Vec3, turn: Quat, gap: f32) -> [Joint; HAND_JOINTS] {
        let side = if left { -1.0 } else { 1.0 };
        let at = |x: f32, y: f32, z: f32| joint(palm + turn * Vec3::new(x * side, y, z), turn);
        let mut joints = [at(0.0, 0.0, 0.0); HAND_JOINTS];
        joints[1] = at(0.0, 0.0, 0.04);
        joints[THUMB_PROXIMAL] = at(-0.04, -0.015, -0.005);
        joints[INDEX_PROXIMAL] = at(-0.02, 0.0, -0.03);
        joints[INDEX_TIP] = at(-0.02, 0.0, -0.115);
        joints[THUMB_TIP] = at(-0.02, -gap, -0.115);
        joints[MIDDLE_TIP] = at(0.0, 0.0, -0.127);
        joints[RING_TIP] = at(0.018, 0.0, -0.115);
        joints[LITTLE_TIP] = at(0.034, 0.0, -0.102);
        joints
    }

    fn still(at: Vec3) -> DeviceMotion {
        DeviceMotion { pose: Pose { orientation: Quat::IDENTITY, position: at }, ..Default::default() }
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-4
    }

    #[test]
    fn the_grip_axes_follow_the_extension_for_both_hands() {
        // A fist of each hand in front of the body with a stick pointing up: the palms face each
        // other, the knuckles point forward. The extension: +X points to the user's right for both
        // hands and -Z goes from the little finger to the index finger, which is up.
        for left in [false, true] {
            let side = if left { -1.0 } else { 1.0 };
            // The right palm faces left: the hand rolls a quarter turn about its forward axis
            let turn = Quat::from_rotation_z(-side * std::f32::consts::FRAC_PI_2);
            let joints = hand(left, Vec3::new(0.2 * side, 1.2, -0.3), turn, 0.08);
            let grip = interaction(left, &still(Vec3::ZERO), &joints, None).poses[GRIP].pose;
            assert!(close(grip.orientation * Vec3::X, Vec3::X), "left {left}: {:?}", grip.orientation * Vec3::X);
            assert!(close(grip.orientation * Vec3::NEG_Z, Vec3::Y), "left {left}: {:?}", grip.orientation * Vec3::NEG_Z);
            assert_eq!(grip.position, position(&joints[PALM]));
        }
    }

    #[test]
    fn the_profile_and_a_controller_made_of_the_hand_give_one_grip_pose() {
        for left in [false, true] {
            let turn = Quat::from_rotation_y(0.4) * Quat::from_rotation_z(0.7);
            let joints = hand(left, Vec3::new(0.2, 1.2, -0.3), turn, 0.08);
            let profile = interaction(left, &still(Vec3::ZERO), &joints, None).poses[GRIP];
            let controller = hand_controller::controller(left, &still(Vec3::ZERO), &joints, None).grip;
            assert_eq!(profile, controller, "left {left}");
        }
    }

    #[test]
    fn a_right_palm_facing_down_has_its_grip_x_up_and_its_grip_z_to_the_right() {
        // The index finger is on the left of the little finger, and the palm faces down: the ray
        // that enters the right palm points up
        let joints = hand(false, Vec3::new(0.2, 1.2, -0.3), Quat::IDENTITY, 0.08);
        let grip = interaction(false, &still(Vec3::ZERO), &joints, None).poses[GRIP].pose;
        assert!(close(grip.orientation * Vec3::X, Vec3::Y));
        assert!(close(grip.orientation * Vec3::NEG_Z, Vec3::NEG_X));
    }

    #[test]
    fn the_pinch_pose_sits_between_the_tips_and_points_up_while_the_palms_face_each_other() {
        for left in [false, true] {
            let side = if left { -1.0 } else { 1.0 };
            let turn = Quat::from_rotation_z(-side * std::f32::consts::FRAC_PI_2);
            let joints = hand(left, Vec3::new(0.2 * side, 1.2, -0.3), turn, 0.02);
            let pinch = interaction(left, &still(Vec3::ZERO), &joints, None).poses[PINCH_POSE].pose;
            assert!(close(pinch.position, (position(&joints[THUMB_TIP]) + position(&joints[INDEX_TIP])) * 0.5));
            let y = pinch.orientation * Vec3::Y;
            assert!(y.dot(Vec3::Y) > 0.95, "left {left}: {y:?}");
            // +Z points back, toward the middle of the proximal joints
            let z = pinch.orientation * Vec3::Z;
            assert!(z.dot(Vec3::Z) > 0.95, "left {left}: {z:?}");
        }
    }

    #[test]
    fn the_poke_pose_lies_on_the_skin_ahead_of_the_index_tip() {
        let joints = hand(false, Vec3::new(0.2, 1.2, -0.3), Quat::IDENTITY, 0.08);
        let poke = interaction(false, &still(Vec3::ZERO), &joints, None).poses[POKE].pose;
        assert!(close(poke.position, position(&joints[INDEX_TIP]) + Vec3::new(0.0, 0.0, -INDEX_TIP_RADIUS)));
        // +Y up with the palm down and +Z from the fingertip to the knuckle
        assert!(close(poke.orientation * Vec3::Y, Vec3::Y));
        assert!(close(poke.orientation * Vec3::Z, Vec3::Z));
    }

    #[test]
    fn the_pinch_and_the_grasp_follow_the_fingers_and_need_tracked_joints() {
        let open = interaction(false, &still(Vec3::ZERO), &hand(false, Vec3::ZERO, Quat::IDENTITY, 0.08), None);
        assert_eq!(open.values, [0.0; 3]);
        assert_eq!(open.ready, [true; 3]);
        let touching = interaction(false, &still(Vec3::ZERO), &hand(false, Vec3::ZERO, Quat::IDENTITY, 0.018), None);
        assert!(touching.values[PINCH] > 0.999 && touching.values[AIM_ACTIVATE] == touching.values[PINCH]);
        let half = interaction(false, &still(Vec3::ZERO), &hand(false, Vec3::ZERO, Quat::IDENTITY, 0.039), None);
        assert!((half.values[PINCH] - 0.5).abs() < 0.01, "{}", half.values[PINCH]);

        let mut fist = hand(false, Vec3::ZERO, Quat::IDENTITY, 0.08);
        for tip in [MIDDLE_TIP, RING_TIP, LITTLE_TIP] {
            fist[tip] = joint(Vec3::new(0.0, -0.03, -0.01), Quat::IDENTITY);
        }
        assert_eq!(interaction(false, &still(Vec3::ZERO), &fist, None).values[GRASP], 1.0);

        // A fingertip that the cameras lost: the pinch is not ready and reads 0
        let mut lost = hand(false, Vec3::ZERO, Quat::IDENTITY, 0.018);
        lost[THUMB_TIP].flags = joint::POSITION_VALID | joint::ORIENTATION_VALID;
        let lost = interaction(false, &still(Vec3::ZERO), &lost, None);
        assert_eq!((lost.ready[PINCH], lost.values[PINCH]), (false, 0.0));
        assert!(lost.ready[GRASP]);
    }

    #[test]
    fn every_pose_moves_with_the_wrist() {
        let joints = hand(false, Vec3::new(0.2, 1.2, -0.3), Quat::IDENTITY, 0.08);
        let root = DeviceMotion {
            pose: Pose { orientation: Quat::IDENTITY, position: position(&joints[1]) },
            linear_velocity: Vec3::new(0.5, 0.0, 0.0),
            angular_velocity: Vec3::ZERO,
        };
        for pose in interaction(false, &root, &joints, None).poses {
            assert_eq!(pose.linear_velocity, Vec3::new(0.5, 0.0, 0.0));
        }
    }
}
