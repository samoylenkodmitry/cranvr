use super::eye_poses;
use crate::pose::{Pose, Quat, Vec3};
use std::f32::consts::FRAC_PI_2;

fn close(a: Vec3, b: Vec3) -> bool {
    (a - b).length() < 1e-5
}

#[test]
fn eyes_sit_beside_a_head_that_stands_straight() {
    let head = Pose { orientation: Quat::IDENTITY, position: Vec3::new(1.0, 1.6, -2.0) };
    let eyes = eye_poses(&head, [[0.0, 0.0, 0.0, 1.0]; 2], [[-0.032, 0.0, 0.0, 0.0], [0.032, 0.0, 0.0, 0.0]]);
    assert!(close(eyes[0].position, Vec3::new(0.968, 1.6, -2.0)));
    assert!(close(eyes[1].position, Vec3::new(1.032, 1.6, -2.0)));
    assert!(eyes.iter().all(|e| e.orientation.abs_diff_eq(Quat::IDENTITY, 1e-6)));
}

#[test]
fn a_turned_head_carries_its_eyes_around() {
    // A quarter turn about the vertical axis takes the head's right side from +x to -z
    let head = Pose { orientation: Quat::from_rotation_y(FRAC_PI_2), position: Vec3::new(0.0, 1.6, 0.0) };
    let eyes = eye_poses(&head, [[0.0, 0.0, 0.0, 1.0]; 2], [[-0.032, 0.0, 0.0, 0.0], [0.032, 0.0, 0.0, 0.0]]);
    assert!(close(eyes[0].position, Vec3::new(0.0, 1.6, 0.032)));
    assert!(close(eyes[1].position, Vec3::new(0.0, 1.6, -0.032)));
    assert!(eyes.iter().all(|e| e.orientation.abs_diff_eq(head.orientation, 1e-6)));
}

#[test]
fn an_eye_that_looks_aside_turns_after_the_head() {
    // The eye's own rotation applies first, in the head's frame: the order head times eye
    let head = Pose { orientation: Quat::from_rotation_y(FRAC_PI_2), position: Vec3::ZERO };
    let eye = Quat::from_rotation_x(0.2);
    let eyes = eye_poses(&head, [eye.to_array(), eye.to_array()], [[0.0; 4]; 2]);
    assert!(eyes[0].orientation.abs_diff_eq(head.orientation * eye, 1e-6));
    assert!(!eyes[0].orientation.abs_diff_eq(eye * head.orientation, 1e-3));
}
