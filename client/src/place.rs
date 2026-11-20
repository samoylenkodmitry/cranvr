//! Where the panels stand, as the user left them.
//!
//! The headset carries a movable panel and reports each step as a `SpatialMoveEvent`. This file
//! keeps the pose that the last step of a carry reports in Cranpose's preferences service, one for
//! each panel. The next start puts the panel there with `SubspaceModifier::with_pose`.
//!
//! A pose is relative to the origin of the panel's subspace. All panels stand in a
//! `FollowingSubspace`, whose origin lies a meter ahead of the head at the app's first tracked
//! frame. The origin moves there again after a recenter. A saved pose therefore means the same at
//! each start: the place relative to where the user looked then. A panel with no saved pose takes its default pose
//! in the same frame, so the first start opens in front of the user.

use cranpose::preferences;
use cranpose::prelude::MutableState;
use cranpose_xr::{MovePolicy, Pose, ResizeLimits, ResizePolicy, SpatialMoveEvent, SpatialMoveEventType, SpatialResizeEventType, SubspaceModifier};

/// The farthest that a saved panel may stand from its subspace's origin, in meters. A pose from
/// a damaged preference must not put a panel out of reach.
const FAR: f32 = 12.0;

/// How many times its own size the user may make a resizable panel
const GROWTH: f32 = 2.5;

fn pose_key(name: &str) -> String {
    format!("pose:{name}")
}

/// The pose of the panel `name` where the user left it, if the user carried it. The next start
/// places the panel there.
pub fn saved_pose(name: &str) -> Option<Pose> {
    preferences().get(&pose_key(name)).and_then(|text| pose_from_text(&text))
}

/// Keeps the pose of the panel `name` for the next start.
pub fn save_pose(name: &str, pose: &Pose) {
    if let Err(e) = preferences().set(&pose_key(name), &pose_to_text(pose)) {
        log::warn!("the pose of the {name} panel was not saved: {e}");
    }
}

/// The width in meters that the user gave the panel `name`, if they did. A move event carries no
/// size, so a panel that the user resizes keeps its width apart from its pose.
pub fn saved_width(name: &str) -> Option<f32> {
    preferences().get(&format!("width:{name}")).and_then(|text| text.trim().parse().ok()).filter(|width: &f32| width.is_finite())
}

/// Keeps the width in meters of the panel `name` for the next start.
pub fn save_width(name: &str, width: f32) {
    if let Err(e) = preferences().set(&format!("width:{name}"), &format!("{width:.4}")) {
        log::warn!("the width of the {name} panel was not saved: {e}");
    }
}

/// The size in dp that the user gave the panel `name`, if they resized it.
fn saved_size(name: &str) -> Option<[f32; 2]> {
    let text = preferences().get(&format!("size:{name}"))?;
    let (width, height) = text.split_once(',')?;
    let size: [f32; 2] = [width.trim().parse().ok()?, height.trim().parse().ok()?];
    size.iter().all(|side| side.is_finite()).then_some(size)
}

fn save_size(name: &str, size: [f32; 2]) {
    if let Err(e) = preferences().set(&format!("size:{name}"), &format!("{:.0},{:.0}", size[0], size[1])) {
        log::warn!("the size of the {name} panel was not saved: {e}");
    }
}

/// The size in dp that the panel `name` starts at: the saved one within the panel's limits, else
/// `smallest`, the size that its content was laid out for.
pub fn start_size(name: &str, smallest: [f32; 2]) -> [f32; 2] {
    saved_size(name).map_or(smallest, |size| [0, 1].map(|axis| size[axis].clamp(smallest[axis], smallest[axis] * GROWTH)))
}

/// The modifier of the panel `name` at the size that `size` holds. The sphere at the panel's bottom
/// right corner resizes it from `smallest` up to `GROWTH` times that. The content lays out again at
/// each size. The size of the last step of each resize goes to the preferences.
pub fn resizable(name: &'static str, size: MutableState<[f32; 2]>, smallest: [f32; 2]) -> SubspaceModifier {
    let [width, height] = size.get();
    let limits = ResizeLimits::new().minimum_size(smallest[0], smallest[1]).maximum_size(smallest[0] * GROWTH, smallest[1] * GROWTH);
    SubspaceModifier::new().size(width, height).resizable_with(
        limits,
        ResizePolicy::custom(move |event| {
            crate::hover::set_handle(event.event_type != SpatialResizeEventType::End);
            size.set(event.size);
            if event.event_type == SpatialResizeEventType::End {
                save_size(name, event.size);
            }
        }),
    )
}

/// The pose that the panel `name` starts at: the saved one, else `default`.
pub fn start_pose(name: &str, default: Pose) -> Pose {
    saved_pose(name).unwrap_or(default)
}

/// The modifier of the panel `name`. The panel stands at `pose`. The user can carry it. The pose of
/// the last step of each carry goes to the preferences. `on_move` hears of each step first.
pub fn movable(name: &'static str, modifier: SubspaceModifier, pose: Pose, on_move: impl Fn(&SpatialMoveEvent) + 'static) -> SubspaceModifier {
    modifier.with_pose(pose).movable_with(MovePolicy::system(move |event| {
        // A carry holds the carrying hand back from the game until it ends
        crate::hover::set_handle(event.event_type != SpatialMoveEventType::End);
        on_move(&event);
        if event.event_type == SpatialMoveEventType::End {
            save_pose(name, &event.pose);
        }
    }))
}

/// The text that the preferences keep: x, y, z and the quaternion x, y, z, w.
fn pose_to_text(pose: &Pose) -> String {
    let [x, y, z] = pose.position;
    let [qx, qy, qz, qw] = pose.orientation;
    format!("{x:.4},{y:.4},{z:.4},{qx:.5},{qy:.5},{qz:.5},{qw:.5}")
}

/// The pose from the preferences' text. Nothing for a text that is not one, or that puts the
/// panel out of reach. The turn is made a unit quaternion again, as the rounding of the text moved
/// its length a little.
fn pose_from_text(text: &str) -> Option<Pose> {
    let numbers: Vec<f32> = text.split(',').map(|part| part.trim().parse().ok()).collect::<Option<_>>()?;
    let [x, y, z, qx, qy, qz, qw] = <[f32; 7]>::try_from(numbers).ok()?;
    let length = (qx * qx + qy * qy + qz * qz + qw * qw).sqrt();
    let sane = [x, y, z, length].iter().all(|v| v.is_finite()) && (0.9..=1.1).contains(&length) && (x * x + y * y + z * z).sqrt() <= FAR;
    sane.then(|| Pose { position: [x, y, z], orientation: [qx / length, qy / length, qz / length, qw / length] })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pose_survives_the_preferences() {
        let turned = SubspaceModifier::new().offset(300.0, -200.0, -1100.0).rotate(-34.0, 12.5, 0.0).pose();
        let back = pose_from_text(&pose_to_text(&turned)).unwrap();
        for i in 0..3 {
            assert!((back.position[i] - turned.position[i]).abs() < 1e-4, "{back:?}");
        }
        for i in 0..4 {
            assert!((back.orientation[i] - turned.orientation[i]).abs() < 1e-4, "{back:?}");
        }
        assert_eq!(pose_from_text(&pose_to_text(&Pose::IDENTITY)), Some(Pose::IDENTITY));
    }

    #[test]
    fn a_text_that_is_no_pose_or_puts_the_panel_out_of_reach_is_refused() {
        assert_eq!(pose_from_text("garbage"), None);
        assert_eq!(pose_from_text("0,0,-1,0,0,0"), None);
        assert_eq!(pose_from_text("0,0,-50,0,0,0,1"), None);
        assert_eq!(pose_from_text("NaN,0,-1,0,0,0,1"), None);
        // A turn that is no unit quaternion
        assert_eq!(pose_from_text("0,0,-1,0,0,0,0"), None);
        assert_eq!(pose_from_text("0,0,-1,0,0,0,3"), None);
    }

    #[test]
    fn the_modifier_places_the_panel_at_the_pose() {
        let pose = Pose { position: [0.2, 0.1, -0.5], orientation: [0.0, 0.0, 0.0, 1.0] };
        let modifier = movable("test", SubspaceModifier::new().size(760.0, 840.0), pose, |_| {});
        assert_eq!(modifier.pose(), pose);
        assert_eq!(modifier.size_or([0.0, 0.0]), [760.0, 840.0]);
    }
}
