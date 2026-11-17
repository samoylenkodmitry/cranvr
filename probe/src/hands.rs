//! The controllers that the probe's tracking script plays (`cranvr-probe --protocol native`, see
//! `native.rs`) and that `cranvr-fake-game` looks for at the game. Both files take the numbers from
//! here, so the script and the check cannot drift apart.
//!
//! Each controller has a grip pose and an aim pose. The two hands differ in every number, so a swap
//! of the hands or of grip and aim changes what the game sees. The poses are in the probe's room.

use alvr_common::glam::{Quat, Vec3};
use cranvr_proto::wire::{Motion, Pose};

/// Where the left grip pose stays
pub const LEFT_AT: [f32; 3] = [-0.2, 1.2, -0.4];
/// The middle of the right grip pose's swing. The pose swings by `RIGHT_SWING` meters to each side
/// along the x axis.
pub const RIGHT_AT: [f32; 3] = [0.25, 1.1, -0.4];
pub const RIGHT_SWING: f32 = 0.1;

/// The turn of each grip pose about the y axis in degrees, left then right
pub const GRIP_YAW: [f32; 2] = [30.0, -20.0];
/// The angle at which each aim pose points below the horizon, in degrees: a rotation about the x
/// axis
pub const AIM_PITCH: [f32; 2] = [-40.0, -50.0];
/// Where each aim pose lies relative to its grip pose, in the room's axes
pub const AIM_OFFSET: [[f32; 3]; 2] = [[0.0, -0.03, -0.10], [0.01, -0.04, -0.12]];
/// What each aim pose's linear velocity has beyond its grip pose's
pub const AIM_LINEAR_EXTRA: [[f32; 3]; 2] = [[0.0, 0.1, 0.0], [0.0, 0.0, 0.3]];
/// Each aim pose's angular velocity. The grip poses turn at no rate.
pub const AIM_ANGULAR: [[f32; 3]; 2] = [[0.25, 0.0, 0.0], [0.0, 0.5, 0.0]];

pub fn grip_orientation(hand: usize) -> [f32; 4] {
    Quat::from_rotation_y(GRIP_YAW[hand].to_radians()).to_array()
}

pub fn aim_orientation(hand: usize) -> [f32; 4] {
    Quat::from_rotation_x(AIM_PITCH[hand].to_radians()).to_array()
}

/// The grip motion of a hand at `position`, which moves at `linear_velocity`
pub fn grip(hand: usize, position: [f32; 3], linear_velocity: [f32; 3]) -> Motion {
    Motion { pose: Pose { orientation: grip_orientation(hand), position }, linear_velocity, angular_velocity: [0.0; 3] }
}

/// The aim motion that belongs to a grip motion
pub fn aim_with(hand: usize, grip: &Motion) -> Motion {
    let (at, offset) = (Vec3::from_array(grip.pose.position), Vec3::from_array(AIM_OFFSET[hand]));
    Motion {
        pose: Pose { orientation: aim_orientation(hand), position: (at + offset).to_array() },
        linear_velocity: (Vec3::from_array(grip.linear_velocity) + Vec3::from_array(AIM_LINEAR_EXTRA[hand])).to_array(),
        angular_velocity: AIM_ANGULAR[hand],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hands_differ_in_every_pose() {
        let motions: Vec<Motion> = (0..2)
            .flat_map(|hand| {
                let grip = grip(hand, if hand == 0 { LEFT_AT } else { RIGHT_AT }, [0.0; 3]);
                [grip, aim_with(hand, &grip)]
            })
            .collect();
        for (i, a) in motions.iter().enumerate() {
            for b in &motions[i + 1..] {
                assert_ne!(a.pose.orientation, b.pose.orientation);
                assert_ne!(a.pose.position, b.pose.position);
            }
        }
    }

    #[test]
    fn an_aim_pose_points_down_by_its_pitch() {
        for (hand, pitch) in AIM_PITCH.iter().enumerate() {
            let forward = Quat::from_array(aim_orientation(hand)) * Vec3::NEG_Z;
            assert!((forward.y - pitch.to_radians().sin()).abs() < 1e-5, "{forward:?}");
        }
    }
}
