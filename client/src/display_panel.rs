//! The panels that show the Mac's displays: one in the room for each display that is shown, or one on
//! the left hand for the display that the user pinned there (see `hand_menu.rs`).
//!
//! A panel is a `SpatialExternalSurface` that the display's decoder writes into (see `displays.rs`).
//! Cranpose finds where a controller's ray meets the surface and reports it as pointer events in dp
//! from the surface's top left corner. The pointer events carry no scroll, so the thumbstick's lean
//! comes from the headset's frame while the ray is on the surface.
//!
//! Cranpose carries a panel in the room. Its move events give the pose that `place.rs` keeps for the
//! next start. The first display keeps the name `desktop` there, so the places that users saved
//! before several displays still hold. The user resizes a panel with the sphere at its bottom right
//! corner, or with the thumbstick during a carry. This file keeps the width.
//!
//! A panel composes nothing while the host sends no pictures of its display. The display that is not
//! shown has no panel, no surface and no decoder.

use crate::{
    displays::{displays, PointerNow},
    hand_menu::{self, HandFrame},
    hover,
    panel_layout::layer_rank,
    place,
};
use crate::launch;
use cranpose::prelude::*;
use cranpose_xr::{
    Controller, Pose, ResizeLimits, ResizePolicy, SpatialExternalSurface, SpatialFrameEffect, SpatialMoveEvent, SpatialMoveEventType, SpatialPanel, SpatialResizeEventType,
    StereoMode, SubspaceModifier, XrFrame, METERS_PER_DP,
};
use cranvr_common::controls::{mouse, Hand as ControlHand};
use cranvr_proto::message::DESKTOP_STREAM;
use std::time::Duration;

/// How fast the thumbstick resizes a carried panel, as a share of the width a second at full lean
const RESIZE_SPEED: f32 = 0.7;
/// A thumbstick leaning less than this counts as at rest
const DEAD_ZONE: f32 = 0.12;
/// How often the panel looks at the display's state
const POLL: Duration = Duration::from_millis(100);
/// The cursor: a ring that is lifted this far off the surface, in meters, with this diameter in dp
const CURSOR_LIFT: f32 = 0.004;
const CURSOR_SIZE: f32 = 26.0;
/// The widths that the user may give a panel in the room, in meters
const WIDTH_RANGE: [f32; 2] = [0.4, 8.0];
/// The width of the hand's display at the start, and the widths that the user may give it, in meters
const HAND_START_WIDTH: f32 = 0.42;
const HAND_WIDTH: [f32; 2] = [0.28, 0.70];

/// The width in meters of the hand's display: the one that the user gave it last, or the width that
/// it starts with.
pub fn hand_display_width() -> f32 {
    place::saved_width(hand_menu::frame().display).map_or(HAND_START_WIDTH, |width| width.clamp(HAND_WIDTH[0], HAND_WIDTH[1]))
}

/// The name under which `place.rs` keeps the pose and width of a display's panel
pub fn panel_name(stream: u8) -> &'static str {
    match stream - DESKTOP_STREAM {
        0 => "desktop",
        1 => "display2",
        2 => "display3",
        _ => "display4",
    }
}

/// Where a display's panel starts when the user never carried it, relative to the origin of the
/// subspace, which lies 1 m ahead of the head, and how wide it is in meters. The first display stands
/// 1.5 m ahead of the head and a little below eye level, 1.6 m wide, which fills 56 degrees. The
/// others stand around it, 1.2 m wide, turned toward the user.
fn start_of(stream: u8) -> (Pose, f32) {
    let pose = |x: f32, y: f32, z: f32, pitch: f32, yaw: f32| SubspaceModifier::new().offset(x, y, z).rotate(pitch, yaw, 0.0).pose();
    match stream - DESKTOP_STREAM {
        0 => (Pose::at([0.0, -0.1, -0.5]), 1.6),
        1 => (pose(1300.0, 450.0, 100.0, 0.0, -40.0), 1.2),
        2 => (pose(-1300.0, 450.0, 100.0, 0.0, 40.0), 1.2),
        _ => (pose(0.0, 750.0, 0.0, -30.0, 0.0), 1.2),
    }
}

/// Where the first display's panel stands and how wide it is before the user carries it, for the
/// scripted controllers of a test build (`scripted.rs`)
#[cfg(all(feature = "scripted-hand", target_os = "android"))]
pub(crate) fn start_panel() -> (Pose, f32) {
    let (pose, width) = start_of(DESKTOP_STREAM);
    let name = panel_name(DESKTOP_STREAM);
    (place::start_pose(name, pose), place::saved_width(name).map_or(width, |width| width.clamp(WIDTH_RANGE[0], WIDTH_RANGE[1])))
}

/// A test with no controller in the room: `--es desktop_pointer_test circle` sends the pointer of a
/// controller that circles over the picture of each shown display, pulls its trigger for 0.3 s every 3 s
/// and leans its thumbstick forward for 2 s every 6 s. The packets go to the host as the surface's events
/// do. Cranpose's ray is not part of the test.
fn pointer_test() -> bool {
    launch::args().string("desktop_pointer_test") == Some("circle")
}

/// The pointer of the pointer test at the headset time `time`: a point that circles the middle of
/// the picture once in 4 seconds.
fn synthetic_pointer(time: Duration) -> PointerNow {
    let t = time.as_secs_f32();
    let angle = t * std::f32::consts::TAU / 4.0;
    PointerNow {
        hit: Some([0.5 + 0.3 * angle.cos(), 0.5 + 0.3 * angle.sin()]),
        // The left button every 3 s, the right one in the second 1.0 to 1.3 of each 3 s and the
        // middle one in the second 2.0 to 2.3
        buttons: [(0.0, mouse::LEFT), (1.0, mouse::RIGHT), (2.0, mouse::MIDDLE)]
            .into_iter()
            .filter(|(from, _)| (t % 3.0 - from).abs() < 0.3 && t % 3.0 >= *from)
            .fold(0, |bits, (_, bit)| bits | bit),
        scroll: if t % 6.0 > 4.0 { [0.6, 0.6] } else { [0.0, 0.0] },
    }
}

/// How far the thumbstick that leans furthest along `axis` (0 across, 1 forward) leans, from -1 to
/// 1, with the dead zone left out. A pointer event of the surface does not say which controller
/// points, so the stronger of the two sticks counts.
fn stick_lean(frame: &XrFrame, axis: usize) -> f32 {
    [&frame.left_controller, &frame.right_controller]
        .map(|controller| [controller.thumbstick.x, controller.thumbstick.y][axis])
        .into_iter()
        .filter(|lean| lean.abs() >= DEAD_ZONE)
        .max_by(|a, b| a.abs().total_cmp(&b.abs()))
        .unwrap_or(0.0)
}

/// How far a controller's thumbstick leans across and forward, each from -1 to 1, with the dead zone
/// left out.
fn lean_of(controller: &Controller) -> [f32; 2] {
    [controller.thumbstick.x, controller.thumbstick.y].map(|lean| if lean.abs() >= DEAD_ZONE { lean } else { 0.0 })
}

/// The surface's width and height in dp (a millimeter each) for a panel `width` meters wide that
/// shows a picture of `aspect` (width over height).
fn surface_size(width: f32, aspect: f32) -> [f32; 2] {
    let width = width / METERS_PER_DP;
    [width, width / aspect.max(0.1)]
}

/// The view of a display's stream, kept fresh in a state so the composition follows it.
#[composable]
fn watch(stream: u8) -> MutableState<crate::displays::View> {
    let view = rememberMutableStateOf(|| displays().view(stream));
    LaunchedEffect(stream, async move |_| loop {
        let now = displays().view(stream);
        if view.get_non_reactive() != now {
            view.set(now);
        }
        delay(POLL).await;
    });
    view
}

/// A display's panel in the room, and the Mac's cursor over it. It composes nothing while the host has
/// no video of the display for this client.
#[composable]
pub fn RoomDisplay(stream: u8) {
    let view = watch(stream);
    let name = panel_name(stream);
    let (start, start_width) = start_of(stream);
    // Where the user left the panel and how wide it is, as `place.rs` kept it
    let pose = rememberMutableStateOf(move || place::start_pose(name, start));
    let width = rememberMutableStateOf(move || place::saved_width(name).map_or(start_width, |width| width.clamp(WIDTH_RANGE[0], WIDTH_RANGE[1])));
    // Whether the user carries the panel now
    let carrying = rememberMutableStateOf(|| false);
    let cursor = rememberMutableStateOf(|| None::<[f32; 2]>);
    SpatialFrameEffect(move |frame: &XrFrame| {
        if !displays().view(stream).visible {
            displays().reset_pointers(stream);
            // A surface that leaves during a carry sends no `End`
            if carrying.get_non_reactive() {
                carrying.set(false);
            }
            return;
        }
        if carrying.get_non_reactive() {
            // The thumbstick's forward lean moves a carried panel away in Cranpose. Its lean across
            // resizes the panel.
            let lean = stick_lean(frame, 0);
            let before = width.get_non_reactive();
            let after = (before * (lean * RESIZE_SPEED * frame.period.as_secs_f32().clamp(0.0, 0.1)).exp()).clamp(WIDTH_RANGE[0], WIDTH_RANGE[1]);
            if after != before {
                width.set(after);
            }
            // The stick that carries a panel scrolls nothing
            displays().update_pointers(stream, |pointers| ControlHand::BOTH.into_iter().for_each(|hand| pointers.lean(hand, [0.0; 2])));
        } else if pointer_test() {
            displays().set_pointer(stream, synthetic_pointer(frame.predicted_display_time));
        } else {
            let leans = [lean_of(&frame.left_controller), lean_of(&frame.right_controller)];
            displays().update_pointers(stream, |pointers| ControlHand::BOTH.into_iter().for_each(|hand| pointers.lean(hand, leans[hand.index()])));
        }
        let at = displays().cursor(stream);
        if cursor.get_non_reactive() != at {
            cursor.set(at);
        }
    });
    let view = view.get();
    if !view.visible {
        return;
    }
    RoomSurface(stream, name, pose, width, carrying, view.aspect);
    if !displays().cursor_in_video(stream) && launch::args().string("cursor") != Some("none") {
        CursorRing(pose, width, cursor, view.aspect);
    }
}

/// The surface that the decoder writes into. The headset makes its swapchain at the size of the
/// panel's first composition (1.5 pixels to a dp) and keeps it when the user resizes the panel. The
/// decoder's pictures are larger. A test with the swapchain at 1600 by 900 dp (2400 by 1350 pixels) and
/// at 2560 by 1440 dp (3840 by 2160 pixels) showed the whole picture both times.
///
/// The surface starts at the pose that `pose` holds at its composition. Cranpose keeps the pose of
/// the user's last carry until the surface leaves the composition. The move events update `pose` for
/// the cursor ring. The last event goes to `place.rs`.
#[composable]
fn RoomSurface(stream: u8, name: &'static str, pose: MutableState<Pose>, width: MutableState<f32>, carrying: MutableState<bool>, aspect: f32) {
    let [surface_width, surface_height] = surface_size(width.get(), aspect);
    // The sphere at the bottom right corner resizes the panel. The picture keeps its shape.
    let [smallest, largest] = WIDTH_RANGE.map(|width| surface_size(width, aspect));
    let limits = ResizeLimits::new().minimum_size(smallest[0], smallest[1]).maximum_size(largest[0], largest[1]).maintain_aspect_ratio(true);
    let resize = ResizePolicy::custom(move |event| {
        hover::set_handle(event.event_type != SpatialResizeEventType::End);
        width.set((event.size[0] * METERS_PER_DP).clamp(WIDTH_RANGE[0], WIDTH_RANGE[1]));
        if event.event_type == SpatialResizeEventType::End {
            place::save_width(name, width.get_non_reactive());
        }
    });
    let modifier = place::movable(
        name,
        SubspaceModifier::new().size(surface_width, surface_height).resizable_with(limits, resize),
        pose.get_non_reactive(),
        move |event: &SpatialMoveEvent| {
            let ended = event.event_type == SpatialMoveEventType::End;
            carrying.set(!ended);
            pose.set(event.pose);
            if ended {
                place::save_width(name, width.get_non_reactive());
            }
        },
    );
    Surface(stream, modifier, width);
}

/// The surface itself: the decoder's target and the pointer's events. `width` is the panel's width in
/// meters now, for the point on the picture.
#[composable]
fn Surface(stream: u8, modifier: SubspaceModifier, width: MutableState<f32>) {
    let id = hover::DISPLAY + u32::from(stream);
    SpatialExternalSurface(StereoMode::Mono, modifier, move |scope| {
        scope.on_surface_created(move |surface| displays().set_surface(stream, Some(surface)));
        // The decoder stops before the callback returns, as the surface's owner requires. A surface
        // that leaves sends no `Exit`: a press that was on would stay down.
        scope.on_surface_destroyed(move |_| {
            displays().reset_pointers(stream);
            hover::clear_panel(id);
            displays().set_surface(stream, None);
        });
        scope.on_pointer_event(move |event| {
            if let Some(hovered) = hover::hover_of(event.kind) {
                hover::set(id, event.id.0, hovered);
            }
            displays().on_pointer_event(stream, &event, surface_size(width.get_non_reactive(), displays().view(stream).aspect));
        });
    });
}

/// The display that rides with the hand menu, in the menu's `frame` (see `hand_menu.rs`). The panel is
/// small, so it asks for a smaller picture (see `display_wish.rs`). The user can resize it with the
/// corner sphere. The sphere under it carries it: the carry sets its offset from the controller,
/// which `place.rs` keeps under the frame's name for the display.
#[composable]
pub fn HandDisplay(stream: u8, frame: HandFrame) {
    let view = watch(stream);
    let name = frame.display;
    let width = rememberMutableStateOf(move || place::saved_width(name).map_or(HAND_START_WIDTH, |width| width.clamp(HAND_WIDTH[0], HAND_WIDTH[1])));
    SpatialFrameEffect(move |frame: &XrFrame| {
        if !displays().view(stream).visible {
            displays().reset_pointers(stream);
            return;
        }
        if pointer_test() {
            displays().set_pointer(stream, synthetic_pointer(frame.predicted_display_time));
        } else {
            let leans = [lean_of(&frame.left_controller), lean_of(&frame.right_controller)];
            displays().update_pointers(stream, |pointers| ControlHand::BOTH.into_iter().for_each(|hand| pointers.lean(hand, leans[hand.index()])));
        }
    });
    let view = view.get();
    if !view.visible {
        return;
    }
    let [surface_width, surface_height] = surface_size(width.get(), view.aspect);
    let [smallest, largest] = HAND_WIDTH.map(|width| surface_size(width, view.aspect));
    let limits = ResizeLimits::new().minimum_size(smallest[0], smallest[1]).maximum_size(largest[0], largest[1]).maintain_aspect_ratio(true);
    let resize = ResizePolicy::custom(move |event| {
        hover::set_handle(event.event_type != SpatialResizeEventType::End);
        width.set((event.size[0] * METERS_PER_DP).clamp(HAND_WIDTH[0], HAND_WIDTH[1]));
        if event.event_type == SpatialResizeEventType::End {
            place::save_width(name, width.get_non_reactive());
        }
    });
    let saved = remember(move || place::saved_pose(name)).with(|pose: &Option<Pose>| *pose);
    let pose = saved.unwrap_or_else(|| hand_menu::hand_display_start(frame, [surface_width, surface_height]));
    let modifier = place::movable(name, SubspaceModifier::new().size(surface_width, surface_height).resizable_with(limits, resize), pose, |_| {});
    Surface(stream, modifier, width);
}

/// The pose of a ring that lies on the point `[u, v]` of a picture (from its top left corner) on
/// a panel at `panel`, `CURSOR_LIFT` meters in front of it. `size` is the panel's width and height
/// in meters.
fn ring_pose(panel: &Pose, [width, height]: [f32; 2], [u, v]: [f32; 2]) -> Pose {
    panel.then(&Pose::at([(u - 0.5) * width, (0.5 - v) * height, CURSOR_LIFT]))
}

/// The Mac's cursor as a small panel in front of the picture.
#[composable]
fn CursorRing(pose: MutableState<Pose>, width: MutableState<f32>, cursor: MutableState<Option<[f32; 2]>>, aspect: f32) {
    let Some(at) = cursor.get() else { return };
    let size = surface_size(width.get(), aspect).map(|dp| dp * METERS_PER_DP);
    let modifier = SubspaceModifier::new().size(CURSOR_SIZE, CURSOR_SIZE).with_pose(ring_pose(&pose.get(), size, at)).layer_priority(layer_rank::DISPLAY);
    SpatialPanel(modifier, || {
        Box(
            Modifier::empty()
                .fill_max_size()
                .background(Color(1.0, 1.0, 1.0, 0.9))
                .rounded_corners(CURSOR_SIZE / 2.0)
                .border(3.0, Color(0.05, 0.05, 0.08, 1.0), RoundedCornerShape::uniform(CURSOR_SIZE / 2.0)),
            BoxSpec::default(),
            || {},
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn the_panel_is_as_wide_as_the_user_made_it_and_as_high_as_the_picture_is() {
        let [w, h] = surface_size(1.6, 16.0 / 9.0);
        assert!(close(w, 1600.0) && close(h, 900.0), "{w} {h}");
        let [w, h] = surface_size(1.0, 2.0);
        assert!((w - 1000.0).abs() < 1e-2 && (h - 500.0).abs() < 1e-2, "{w} {h}");
    }

    #[test]
    fn the_first_display_keeps_the_name_that_the_saved_places_have() {
        assert_eq!(panel_name(DESKTOP_STREAM), "desktop");
        assert_eq!(panel_name(DESKTOP_STREAM + 1), "display2");
        let names: Vec<&str> = (0..4).map(|i| panel_name(DESKTOP_STREAM + i)).collect();
        for (i, name) in names.iter().enumerate() {
            assert!(!names[..i].contains(name));
        }
    }

    #[test]
    fn the_added_displays_start_around_the_first_one() {
        let (first, first_width) = start_of(DESKTOP_STREAM);
        assert_eq!((first.position, first_width), ([0.0, -0.1, -0.5], 1.6));
        let (right, _) = start_of(DESKTOP_STREAM + 1);
        let (left, _) = start_of(DESKTOP_STREAM + 2);
        assert!(right.position[0] > 1.0 && left.position[0] < -1.0);
        let (above, _) = start_of(DESKTOP_STREAM + 3);
        assert!(above.position[1] > 0.5);
    }

    #[test]
    fn the_cursor_ring_lies_on_the_point_in_front_of_the_picture() {
        // A panel 1.6 m by 0.9 m, 1.5 m ahead of the head, which is 0.5 m behind the subspace's
        // origin. The cursor at one quarter across and one quarter down is 40 cm left of the
        // middle and 22.5 cm above it, 4 mm in front.
        let panel = Pose::at([0.0, 0.0, -0.5]);
        let at = ring_pose(&panel, [1.6, 0.9], [0.25, 0.25]).position;
        assert!(close(at[0], -0.4) && close(at[1], 0.225) && close(at[2], -0.5 + CURSOR_LIFT), "{at:?}");
        let turned = Pose { position: [1.0, 0.0, 0.0], ..SubspaceModifier::new().rotate(0.0, 90.0, 0.0).pose() };
        let at = ring_pose(&turned, [1.6, 0.9], [0.0, 0.5]).position;
        assert!(close(at[0], 1.0 + CURSOR_LIFT) && close(at[2], 0.8), "{at:?}");
    }

    #[test]
    fn the_pointer_test_presses_each_button_in_its_turn() {
        let seen: Vec<u8> = (0..60).map(|i| synthetic_pointer(Duration::from_secs_f32(i as f32 * 0.1)).buttons).collect();
        for bit in [mouse::LEFT, mouse::RIGHT, mouse::MIDDLE] {
            assert!(seen.contains(&bit), "{bit}: {seen:?}");
        }
        assert!(synthetic_pointer(Duration::from_secs_f32(4.5)).scroll[0] > 0.0);
    }
}
