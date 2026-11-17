//! Motion of a pose: velocities from two poses. The pose that velocities lead to.
//!
//! The headset reports a pose that is valid at one time. The host renders for a later time, so it
//! moves the pose forward by its velocities. The angular velocity is about the axes of the room,
//! so a turn applies on the left of the orientation.

use crate::wire::{Motion, Pose};

fn multiply(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

/// The linear and the angular velocity that take a pose from `before` to `after` in `seconds`.
pub fn velocities(before: &Pose, after: &Pose, seconds: f32) -> ([f32; 3], [f32; 3]) {
    let linear = [0, 1, 2].map(|i| (after.position[i] - before.position[i]) / seconds);
    // The turn from `before` to `after` in the room's axes: after * conjugate(before)
    let [bx, by, bz, bw] = before.orientation;
    let mut turn = multiply(after.orientation, [-bx, -by, -bz, bw]);
    if turn[3] < 0.0 {
        turn = turn.map(|v| -v);
    }
    let sine = (turn[0] * turn[0] + turn[1] * turn[1] + turn[2] * turn[2]).sqrt();
    let angle = 2.0 * sine.atan2(turn[3]);
    // For a tiny turn the axis is not defined. The angle is then about twice the sine
    let scale = if sine > 1e-6 { angle / sine } else { 2.0 };
    let angular = [0, 1, 2].map(|i| turn[i] * scale / seconds);
    (linear, angular)
}

/// The pose that `motion` leads to after `seconds`, for a constant velocity. A negative time goes
/// back.
pub fn extrapolate(motion: &Motion, seconds: f32) -> Pose {
    let [vx, vy, vz] = motion.linear_velocity;
    let position = [
        motion.pose.position[0] + vx * seconds,
        motion.pose.position[1] + vy * seconds,
        motion.pose.position[2] + vz * seconds,
    ];
    let [wx, wy, wz] = motion.angular_velocity;
    let rotation = [wx * seconds, wy * seconds, wz * seconds];
    let angle = (rotation[0] * rotation[0] + rotation[1] * rotation[1] + rotation[2] * rotation[2]).sqrt();
    let orientation = if angle < 1e-9 {
        motion.pose.orientation
    } else {
        let (sine, cosine) = (angle / 2.0).sin_cos();
        let turn = [rotation[0] / angle * sine, rotation[1] / angle * sine, rotation[2] / angle * sine, cosine];
        let q = multiply(turn, motion.pose.orientation);
        let length = q.iter().map(|v| v * v).sum::<f32>().sqrt();
        q.map(|v| v / length)
    };
    Pose { orientation, position }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose(position: [f32; 3], yaw: f32, pitch: f32) -> Pose {
        // A turn by `yaw` about y after a turn by `pitch` about x, in the room's axes
        let about = |axis: [f32; 3], angle: f32| {
            let (s, c) = (angle / 2.0).sin_cos();
            [axis[0] * s, axis[1] * s, axis[2] * s, c]
        };
        Pose { orientation: multiply(about([0.0, 1.0, 0.0], yaw), about([1.0, 0.0, 0.0], pitch)), position }
    }

    fn close(a: &Pose, b: &Pose) -> bool {
        // The same turn up to the sign of the quaternion
        let dot: f32 = a.orientation.iter().zip(b.orientation).map(|(x, y)| x * y).sum();
        dot.abs() > 0.999_999 && (0..3).all(|i| (a.position[i] - b.position[i]).abs() < 1e-5)
    }

    #[test]
    fn a_turn_about_y_gives_an_angular_velocity_about_y() {
        let (_, angular) = velocities(&Pose::IDENTITY, &pose([0.0; 3], 0.2, 0.0), 0.1);
        assert!(angular[0].abs() < 1e-5 && angular[2].abs() < 1e-5);
        assert!((angular[1] - 2.0).abs() < 1e-3, "{angular:?}");
        // The opposite sign of the quaternion is the same turn
        let mut flipped = pose([0.0; 3], 0.2, 0.0);
        flipped.orientation = flipped.orientation.map(|v| -v);
        assert!((velocities(&Pose::IDENTITY, &flipped, 0.1).1[1] - 2.0).abs() < 1e-3);
    }

    #[test]
    fn velocities_and_extrapolation_round_trip() {
        // From a pose with an orientation of its own, so that the order of the turns counts
        let before = pose([0.1, 1.5, -0.2], 0.7, -0.3);
        let after = Pose { position: [0.13, 1.49, -0.25], ..pose([0.0; 3], 0.7, -0.3) };
        let mut after = after;
        // A turn in the room's axes: yaw 0.03 about y on the left of the first orientation
        let (s, c) = (0.03_f32 / 2.0).sin_cos();
        after.orientation = multiply([0.0, s, 0.0, c], before.orientation);
        let seconds = 0.0083;
        let (linear_velocity, angular_velocity) = velocities(&before, &after, seconds);
        let motion = Motion { pose: before, linear_velocity, angular_velocity };
        assert!(close(&extrapolate(&motion, seconds), &after));
        // Two steps of the same length reach twice as far
        let twice = extrapolate(&motion, 2.0 * seconds);
        let step = extrapolate(&Motion { pose: extrapolate(&motion, seconds), ..motion }, seconds);
        assert!(close(&twice, &step));
        // No time, no change
        assert!(close(&extrapolate(&motion, 0.0), &before));
        // The turn is about the room's y axis, not the head's own
        assert!((angular_velocity[1] - 0.03 / seconds).abs() < 1e-2 && angular_velocity[0].abs() < 1e-3 && angular_velocity[2].abs() < 1e-3);
    }

    #[test]
    fn a_still_head_stays_where_it_is() {
        let motion = Motion { pose: pose([0.0, 1.6, 0.0], 0.4, 0.1), ..Default::default() };
        assert!(close(&extrapolate(&motion, 0.05), &motion.pose));
    }
}
