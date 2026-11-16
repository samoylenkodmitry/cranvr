// cranvr: an OpenXR eye as a camera of a Cranpose scene, and OpenXR places as places in that
// scene. OpenXR has y up and looks along -z, in meters. Cranpose's scene has y down and looks
// along +z, in the units of `UNITS`.

use alvr_common::glam::{Mat4 as GMat4, UVec2};
use cranpose::space::{Camera, Mat4, Vec3};
use openxr as xr;

/// Scene units per meter: a Cranpose camera cuts away what lies nearer than a 64th of its focal
/// length in pixels, so the scene is in millimeters
const UNITS: f32 = 1000.0;

/// An eye as OpenXR gives it: where it is, where it looks and the tangents of its view's edges
/// (left, right, up, down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Eye {
    position: [f32; 3],
    orientation: [f32; 4],
    tangents: [f32; 4],
}

/// The tangents of a view's edges: left, right, up, down.
pub(crate) fn tangents(fov: xr::Fovf) -> [f32; 4] {
    [fov.angle_left.tan(), fov.angle_right.tan(), fov.angle_up.tan(), fov.angle_down.tan()]
}

/// The box that a view fills inside a cell of the swapchain: as large as fits, with the view's own
/// aspect. The compositor stretches the box over the view's edges. A box of another aspect would
/// stretch across by another amount than up, which moves each eye's picture toward its own
/// center and so changes how far away the scene looks.
pub(crate) fn fitted(fov: xr::Fovf, cell: UVec2) -> UVec2 {
    let [l, r, u, d] = tangents(fov);
    let scale = (cell.x as f32 / (r - l)).min(cell.y as f32 / (u - d));
    UVec2::new(((scale * (r - l)).round() as u32).clamp(1, cell.x), ((scale * (u - d)).round() as u32).clamp(1, cell.y))
}

impl Eye {
    pub(crate) fn of(view: &xr::View) -> Self {
        let (p, q) = (view.pose.position, view.pose.orientation);
        Self { position: [p.x, p.y, p.z], orientation: [q.x, q.y, q.z, q.w], tangents: tangents(view.fov) }
    }

    /// The camera that sees the scene as this eye does, in a box of `width` by `height` pixels.
    /// `flip` turns the picture upside down, for a target whose rows go up.
    pub(crate) fn camera(&self, width: f32, height: f32, flip: bool) -> Camera {
        let [l, r, u, d] = self.tangents;
        let focal_length = (width / (r - l)).min(height / (u - d)).max(1.0);
        // How far the view's center lies from the box's middle, in focal lengths
        let (sx, sy) = (-(l + r) * 0.5, (u + d) * 0.5);
        let q = self.orientation;
        let right = scene_direction(rotate(q, [1.0, 0.0, 0.0]));
        let down = scene_direction(rotate(q, [0.0, -1.0, 0.0]));
        let forward = scene_direction(rotate(q, [0.0, 0.0, -1.0]));
        let eye = scene_point(self.position);
        let row = |a: Vec3| [a.x, a.y, a.z, -a.dot(eye)];
        let z = row(forward);
        let sheared = |a: Vec3, s: f32| {
            let r = row(a);
            [r[0] + s * z[0], r[1] + s * z[1], r[2] + s * z[2], r[3] + s * z[3]]
        };
        let (down, sy) = if flip { (down * -1.0, -sy) } else { (down, sy) };
        Camera {
            view: Mat4::from_rows([sheared(right, sx), sheared(down, sy), z, [0.0, 0.0, 0.0, 1.0]]),
            focal_length,
        }
    }
}

/// An OpenXR point in meters as a point of the scene: Cranpose's scene has y down and looks
/// along +z.
pub(crate) fn scene_point([x, y, z]: [f32; 3]) -> Vec3 {
    Vec3::new(x, -y, -z) * UNITS
}

pub(crate) fn scene_direction([x, y, z]: [f32; 3]) -> Vec3 {
    Vec3::new(x, -y, -z)
}

pub(crate) fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let (axis, w) = (Vec3::new(q[0], q[1], q[2]), q[3]);
    let v = Vec3::new(v[0], v[1], v[2]);
    let t = axis.cross(v) * 2.0;
    let r = v + t * w + axis.cross(t);
    [r.x, r.y, r.z]
}

/// A model matrix of the overlay, in meters, as a transform of the scene.
pub(crate) fn scene_transform(model: &GMat4) -> [[f32; 4]; 4] {
    let column = |i: usize| {
        let c = model.col(i);
        [c.x * UNITS, -c.y * UNITS, -c.z * UNITS]
    };
    let [x, y, z, o] = [column(0), column(1), column(2), column(3)];
    let a = |v: [f32; 3]| Vec3::new(v[0], v[1], v[2]);
    // The origin column scales like a point, the axes like directions; both by UNITS
    Mat4::from_axes(a(x), a(y), a(z), a(o)).rows()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranpose::prelude::Size;

    /// The Quest 3's left eye, from the host's log: left, right, up and down in degrees
    fn quest_left_eye() -> xr::View {
        let angle = |degrees: f32| degrees.to_radians();
        xr::View {
            pose: xr::Posef {
                orientation: xr::Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
                position: xr::Vector3f { x: -0.032, y: 1.6, z: 0.0 },
            },
            fov: xr::Fovf { angle_left: angle(-54.0), angle_right: angle(40.0), angle_up: angle(44.0), angle_down: angle(-55.0) },
        }
    }

    /// Where the compositor shows a direction of the eye's view in the eye's box: the box spans
    /// the view's edges, from the up edge at the top row
    fn shown_at(view: &xr::View, box_size: UVec2, (across, up): (f32, f32)) -> (f32, f32) {
        let [l, r, u, d] = tangents(view.fov);
        ((across - l) / (r - l) * box_size.x as f32, (u - up) / (u - d) * box_size.y as f32)
    }

    #[test]
    fn a_point_lands_where_the_compositor_shows_its_direction() {
        let view = quest_left_eye();
        let cell = UVec2::new(1680, 1760);
        let size = fitted(view.fov, cell);
        let camera = Eye::of(&view).camera(size.x as f32, size.y as f32, false);
        let eye = [view.pose.position.x, view.pose.position.y, view.pose.position.z];
        // Points 2 m ahead, straight on and toward each edge of the view
        for (across, up) in [(0.0, 0.0), (-1.0, 0.0), (0.7, 0.0), (0.0, 0.8), (0.0, -1.2), (-0.9, 0.6)] {
            let point = [eye[0] + across * 2.0, eye[1] + up * 2.0, eye[2] - 2.0];
            let projected = camera
                .project(scene_point(point), Size { width: size.x as f32, height: size.y as f32 })
                .expect("the point lies ahead of the eye");
            let (x, y) = shown_at(&view, size, (across, up));
            assert!(
                (projected.x - x).abs() < 1.0 && (projected.y - y).abs() < 1.0,
                "direction ({across}, {up}) drawn at ({}, {}), shown at ({x}, {y})",
                projected.x,
                projected.y
            );
        }
    }

    #[test]
    fn the_box_keeps_the_aspect_of_the_view() {
        let view = quest_left_eye();
        let [l, r, u, d] = tangents(view.fov);
        for cell in [UVec2::new(1680, 1760), UVec2::new(1629, 1760), UVec2::new(2000, 1000)] {
            let size = fitted(view.fov, cell);
            assert!(size.x <= cell.x && size.y <= cell.y, "{size} leaves the cell {cell}");
            let aspect = size.x as f32 / size.y as f32;
            assert!((aspect - (r - l) / (u - d)).abs() < 2.0 / size.y as f32, "{size} has another aspect than the view");
        }
    }
}
