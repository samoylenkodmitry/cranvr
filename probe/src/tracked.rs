//! The hands, body, face and eyes that the probe's tracking script plays (`cranvr-probe --tracking`)
//! and that `cranvr-fake-game` and the Windows test program `tracking_smoke.exe` look for at the
//! game. The three take their numbers from here and from the formulas in the comments, so the script
//! and the checks cannot drift apart. All of it stands still: the check compares values, and a
//! moving hand would make the compared time matter.
//!
//! The poses are in the probe's room. The host lifts each by the height of the probe's floor, and
//! the game's runtime puts the room's origin under the head. The probe's head stands still while it
//! plays this script, at x 0 and z 0 and with no turn, so the game's LOCAL space is the probe's room
//! lifted by the floor.
//!
//! The formulas, with `i` the joint's index:
//! - a hand's wrist lies at [`HAND_AT`] for its side and turns about y by [`HAND_YAW`] degrees. Joint
//!   `i` lies at (0.005 i, -0.003 i, -0.006 i) from the wrist, in the wrist's frame (x is negated
//!   for the left hand), and turns about x by 2 i degrees relative to the wrist.
//! - the body's root lies at [`BODY_AT`] and turns about y by [`BODY_YAW`] degrees. Joint `i` lies at
//!   (0.01 i, 0.02 i, 0.005 i) from the root in the root's frame and turns about z by i degrees.
//! - face weight `i` is ((7 i) mod 71) / 70. The confidences are 0.25 and 0.75.
//! - the gaze lies at [`GAZE_AT`] and turns about y by 15 degrees, then about x by -5 degrees.

use cranvr_proto::{
    tracking::{Body, BodySet, Eyes, Face, Hand, Hands, Joint, FACE_WEIGHTS, HAND_JOINTS},
    wire::{Motion, Pose},
};
use glam::{Quat, Vec3};

/// Where each hand's wrist lies, left then right
pub const HAND_AT: [[f32; 3]; 2] = [[-0.20, 1.20, -0.40], [0.25, 1.10, -0.40]];
/// The turn of each hand's wrist about y in degrees
pub const HAND_YAW: [f32; 2] = [20.0, -15.0];
pub const BODY_AT: [f32; 3] = [0.10, 0.0, -0.50];
pub const BODY_YAW: f32 = 10.0;
pub const GAZE_AT: [f32; 3] = [0.0, 1.6, 0.0];

fn pose(rotation: Quat, position: Vec3) -> Pose {
    Pose { orientation: rotation.to_array(), position: position.to_array() }
}

/// The pose of `relative` in the frame of `root`
fn compose(root: &Pose, relative: &Pose) -> Pose {
    let (q, p) = (Quat::from_array(root.orientation), Vec3::from_array(root.position));
    pose(q * Quat::from_array(relative.orientation), p + q * Vec3::from_array(relative.position))
}

/// A hand's joints: the wrist's motion with no velocity, and the joints in the room
pub fn hand_joints(side: usize) -> (Motion, [Joint; HAND_JOINTS]) {
    let root = Motion {
        pose: pose(Quat::from_rotation_y(HAND_YAW[side].to_radians()), Vec3::from_array(HAND_AT[side])),
        ..Default::default()
    };
    let mirror = if side == 0 { -1.0 } else { 1.0 };
    let joints = std::array::from_fn(|i| {
        let i = i as f32;
        let relative = pose(Quat::from_rotation_x((2.0 * i).to_radians()), Vec3::new(mirror * 0.005 * i, -0.003 * i, -0.006 * i));
        Joint::tracked(compose(&root.pose, &relative))
    });
    (root, joints)
}

/// Both hands at a sample
pub fn hands(sample_time: u64, display_time: u64) -> Hands {
    let hand = |side| {
        let (root, joints) = hand_joints(side);
        Hand::new(root, &joints)
    };
    Hands { sample_time, display_time, left: Some(hand(0)), right: Some(hand(1)) }
}

/// The body's root with no velocity and the joints of the set in the room
pub fn body_joints(set: BodySet) -> (Motion, Vec<Joint>) {
    let root = Motion { pose: pose(Quat::from_rotation_y(BODY_YAW.to_radians()), Vec3::from_array(BODY_AT)), ..Default::default() };
    let joints = (0..set.joints())
        .map(|i| {
            let i = i as f32;
            let relative = pose(Quat::from_rotation_z(i.to_radians()), Vec3::new(0.01 * i, 0.02 * i, 0.005 * i));
            Joint::tracked(compose(&root.pose, &relative))
        })
        .collect();
    (root, joints)
}

pub fn body(sample_time: u64, display_time: u64, set: BodySet) -> Body {
    let (root, joints) = body_joints(set);
    Body::new(sample_time, display_time, set, 0.9, root, &joints).expect("as many joints as the set has")
}

pub fn face_weights() -> [f32; FACE_WEIGHTS] {
    std::array::from_fn(|i| ((7 * i) % 71) as f32 / 70.0)
}

pub const FACE_CONFIDENCES: [f32; 2] = [0.25, 0.75];

pub fn face(sample_time: u64, display_time: u64) -> Face {
    Face::new(sample_time, display_time, cranvr_proto::tracking::face::VALID, FACE_CONFIDENCES, &face_weights())
}

pub fn gaze() -> Pose {
    pose(Quat::from_rotation_y(15f32.to_radians()) * Quat::from_rotation_x((-5f32).to_radians()), Vec3::from_array(GAZE_AT))
}

pub fn eyes(sample_time: u64, display_time: u64) -> Eyes {
    Eyes { sample_time, display_time, gaze: Some(gaze()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_survives_the_wire() {
        let hands = hands(1, 2);
        for (side, hand) in [hands.left.as_ref().unwrap(), hands.right.as_ref().unwrap()].into_iter().enumerate() {
            let (_, expected) = hand_joints(side);
            for (a, b) in expected.iter().zip(hand.joints().unwrap()) {
                assert!((0..3).all(|k| (a.pose.position[k] - b.pose.position[k]).abs() < 2e-4));
            }
        }
        let body = body(1, 2, BodySet::Full);
        assert_eq!(body.joints().unwrap().len(), cranvr_proto::tracking::FULL_BODY_JOINTS);
        assert!(face_weights().iter().all(|w| (0.0..=1.0).contains(w)));
        // The joints of the two hands differ in every position
        assert_ne!(hand_joints(0).1[5].pose.position, hand_joints(1).1[5].pose.position);
    }
}
