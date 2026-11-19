//! The pose types of the host's scenes and of the game link. The vectors and rotations are glam's.

pub use glam::{Quat, Vec3};

/// A place and a turn in the headset's room.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub orientation: Quat,
    pub position: Vec3,
}

impl Default for Pose {
    fn default() -> Self {
        Self { orientation: Quat::IDENTITY, position: Vec3::ZERO }
    }
}

/// A pose with the velocities that a device had at that moment.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DeviceMotion {
    pub pose: Pose,
    pub linear_velocity: Vec3,
    pub angular_velocity: Vec3,
}

/// The angles of one eye's field of view in radians: left and down are negative.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fov {
    pub left: f32,
    pub right: f32,
    pub up: f32,
    pub down: f32,
}

/// What the headset's eyes look like: each eye's pose in the head's space and its field of view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewsConfig {
    pub local_view_transforms: [Pose; 2],
    pub fov: [Fov; 2],
}
