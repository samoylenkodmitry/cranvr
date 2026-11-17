//! Where the desktop panel stands in the room, how the user carries and resizes it, and where a
//! controller points on it.
//!
//! Cranpose carries a `movable()` panel but does not tell the app where the user put it, and a ray
//! that meets a surface panel reports no point. The app needs both: it saves the place for the next
//! start and it sends the point to the host as the mouse position. So the app keeps the panel's pose
//! itself, moves it with its own grab and finds the point with Cranpose's public ray math
//! (`Ray::hit_panel`).
//!
//! The pose lives in the room of the headset (OpenXR's LOCAL space, whose origin lies where the head
//! was when the app started), as the controllers' poses do. A saved place therefore stays in front of
//! the user at each start, and a recenter of the headset moves the panel with the room.
//!
//! The panel stays upright and faces the head: a place is a center, a yaw and a pitch, and a width.
//! The height follows the picture's aspect ratio.
//!
//! The controls:
//! - Point at the panel and squeeze the grip to carry it. The panel follows the controller and turns
//!   to face the head. The thumbstick forward and back moves it away and near. Left and right resize
//!   it.
//! - Point at the panel with the trigger to click. The thumbstick scrolls.

use cranpose_xr::{Pose, Ray, SubspaceModifier};

/// Where Cranpose puts the origin of a subspace in the room, in meters: a meter in front of the
/// head's place at the start. A panel's modifier offset counts from there.
const SUBSPACE_ORIGIN: [f32; 3] = [0.0, 0.0, -1.0];

/// The smallest and the widest panel in meters, and the nearest and the farthest it stands.
const WIDTH_RANGE: [f32; 2] = [0.4, 8.0];
const DISTANCE_RANGE: [f32; 2] = [0.5, 6.0];
/// A grip squeezed past this grabs the panel and one released below `LET_GO` drops it.
const GRAB: f32 = 0.8;
const LET_GO: f32 = 0.5;
/// A trigger pulled past `PRESS` presses the mouse button and one released below `RELEASE` lifts it.
/// The host applies the same limits.
const PRESS: f32 = 0.6;
const RELEASE: f32 = 0.4;
/// Carrying: how fast the thumbstick moves the panel away in meters a second, and how fast it
/// resizes the panel as a share of the width a second, at full lean
const PUSH_SPEED: f32 = 1.2;
const RESIZE_SPEED: f32 = 0.7;
/// A thumbstick leaning less than this counts as at rest
const DEAD_ZONE: f32 = 0.12;

/// The panel's place.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    /// The center in the room, in meters
    pub center: [f32; 3],
    /// The turn about the vertical axis in degrees: positive turns the panel's face to the right
    pub yaw: f32,
    /// The tilt about the horizontal axis in degrees
    pub pitch: f32,
    /// The width in meters
    pub width: f32,
}

impl Default for Place {
    /// In front of the head, 1.5 m away and a little below eye level. 1.6 m wide fills 56 degrees.
    fn default() -> Self {
        Self { center: [0.0, -0.1, -1.5], yaw: 0.0, pitch: 0.0, width: 1.6 }
    }
}

impl Place {
    /// The pose of the panel's center, which faces its +z.
    pub fn pose(&self) -> Pose {
        Pose { position: self.center, orientation: turn(self.pitch, self.yaw) }
    }

    /// The panel's width and height in meters for a picture of `aspect` (width over height).
    pub fn size(&self, aspect: f32) -> [f32; 2] {
        [self.width, self.width / aspect.max(0.1)]
    }

    /// The modifier that puts a spatial composable here. The offset is in dp, a millimeter each.
    pub fn modifier(&self, aspect: f32) -> SubspaceModifier {
        let [width, height] = self.size(aspect).map(|meters| meters * 1000.0);
        let [x, y, z] = [0, 1, 2].map(|axis| (self.center[axis] - SUBSPACE_ORIGIN[axis]) * 1000.0);
        SubspaceModifier::new().size(width, height).offset(x, y, z).rotate(self.pitch, self.yaw, 0.0)
    }

    /// The modifier for a small panel of `size` dp that shows the cursor at `[u, v]` on the picture
    /// (from its top left corner), `lift` meters in front of it. The panel's center lies on the
    /// point.
    pub fn cursor_modifier(&self, aspect: f32, [u, v]: [f32; 2], size: f32, lift: f32) -> SubspaceModifier {
        let [width, height] = self.size(aspect);
        let local = Pose::at([(u - 0.5) * width, (0.5 - v) * height, lift]);
        let room = self.pose().then(&local).position;
        let [x, y, z] = [0, 1, 2].map(|axis| (room[axis] - SUBSPACE_ORIGIN[axis]) * 1000.0);
        SubspaceModifier::new().size(size, size).offset(x, y, z).rotate(self.pitch, self.yaw, 0.0)
    }

    /// The place with its face turned to `head`.
    pub fn facing(mut self, head: [f32; 3]) -> Self {
        let d = [head[0] - self.center[0], head[1] - self.center[1], head[2] - self.center[2]];
        let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if length > 1e-3 {
            self.yaw = d[0].atan2(d[2]).to_degrees();
            self.pitch = -(d[1] / length).clamp(-1.0, 1.0).asin().to_degrees();
        }
        self
    }

    /// The text that the preferences keep.
    pub fn to_text(self) -> String {
        let [x, y, z] = self.center;
        format!("{x:.4},{y:.4},{z:.4},{:.2},{:.2},{:.4}", self.yaw, self.pitch, self.width)
    }

    /// The place from the preferences' text. Nothing for a text that is not one, or that puts the
    /// panel out of reach.
    pub fn from_text(text: &str) -> Option<Self> {
        let numbers: Vec<f32> = text.split(',').map(|part| part.trim().parse().ok()).collect::<Option<_>>()?;
        let [x, y, z, yaw, pitch, width] = numbers.try_into().ok()?;
        let place = Self { center: [x, y, z], yaw, pitch, width };
        let distance = (x * x + y * y + z * z).sqrt();
        let sane = [x, y, z, yaw, pitch, width].iter().all(|v| v.is_finite())
            && (WIDTH_RANGE[0]..=WIDTH_RANGE[1]).contains(&width)
            && distance <= DISTANCE_RANGE[1] * 2.0;
        sane.then_some(place)
    }
}

/// The turn by `pitch` about x, then `yaw` about y, in degrees, as Cranpose's `rotate` applies them
/// (with no roll): a quaternion x, y, z, w.
fn turn(pitch: f32, yaw: f32) -> [f32; 4] {
    let about = |axis: [f32; 3], degrees: f32| {
        let (sin, cos) = (degrees.to_radians() / 2.0).sin_cos();
        [axis[0] * sin, axis[1] * sin, axis[2] * sin, cos]
    };
    let (a, b) = (about([0.0, 1.0, 0.0], yaw), about([1.0, 0.0, 0.0], pitch));
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

/// One controller in a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Hand {
    /// The pose that the controller points with, or `None` while it is not tracked
    pub aim: Option<Pose>,
    pub trigger: f32,
    pub squeeze: f32,
    /// The thumbstick: x to the right, y forward
    pub stick: [f32; 2],
}

/// What the engine reads in one headset frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    /// Left, then right
    pub hands: [Hand; 2],
    /// The head's place, or `None` while the runtime does not track it
    pub head: Option<[f32; 3]>,
    /// The time since the last frame in seconds
    pub dt: f32,
}

/// What the app sends the host about the pointer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PointerNow {
    /// Where the controller points on the panel as u and v from its top left corner
    pub hit: Option<[f32; 2]>,
    pub trigger: f32,
    pub scroll: f32,
}

impl PointerNow {
    /// Whether the host has something to do with it: a controller points at the panel or holds a
    /// press.
    pub fn busy(&self) -> bool {
        self.hit.is_some() || self.trigger > RELEASE
    }

    /// Whether `other` differs enough to send. A move of a fifth of a pixel of a 4K picture does not
    /// count.
    pub fn differs(&self, other: &Self) -> bool {
        let moved = match (self.hit, other.hit) {
            (Some(a), Some(b)) => (a[0] - b[0]).abs() > 5e-5 || (a[1] - b[1]).abs() > 5e-5,
            (None, None) => false,
            _ => true,
        };
        moved || (self.trigger - other.trigger).abs() > 0.02 || (self.scroll - other.scroll).abs() > 0.02
    }
}

/// What one frame changed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Step {
    pub pointer: PointerNow,
    /// The panel moved or changed its size
    pub moved: bool,
    /// The user let go of a carried panel: the place is worth saving
    pub dropped: bool,
    /// Whether a carry is on
    pub carrying: bool,
}

/// A grab: the panel's pose in the controller's frame.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Carry {
    hand: usize,
    relative: Pose,
}

/// The state that connects the frames: the place, the carry and the press.
#[derive(Debug)]
pub struct Engine {
    pub place: Place,
    /// The picture's width over its height
    pub aspect: f32,
    carry: Option<Carry>,
    /// The hand that points, 0 left and 1 right
    active: usize,
    /// A press that began on the panel, and the last point on it
    pressed: Option<[f32; 2]>,
    /// A trigger that was down before it met the panel does not click
    ignoring: bool,
}

impl Engine {
    pub fn new(place: Place, aspect: f32) -> Self {
        Self { place, aspect, carry: None, active: 1, pressed: None, ignoring: false }
    }

    /// Where `hand` points on the panel.
    fn hit(&self, hand: &Hand) -> Option<[f32; 2]> {
        let aim = hand.aim?;
        let hit = Ray::of_aim(&aim).hit_panel(&self.place.pose(), self.place.size(self.aspect))?;
        Some([hit.u, hit.v])
    }

    pub fn step(&mut self, frame: &Frame) -> Step {
        let before = self.place;
        let mut step = Step::default();
        let hits = [self.hit(&frame.hands[0]), self.hit(&frame.hands[1])];

        match self.carry {
            None => {
                // A grip squeezed over the panel grabs it
                let grab = (0..2).rev().find(|&h| frame.hands[h].squeeze >= GRAB && hits[h].is_some());
                if let (Some(hand), true) = (grab, self.pressed.is_none()) {
                    if let Some(aim) = frame.hands[hand].aim {
                        self.carry = Some(Carry { hand, relative: aim.inverse().then(&self.place.pose()) });
                    }
                }
            }
            Some(carry) => {
                let hand = &frame.hands[carry.hand];
                match hand.aim {
                    Some(aim) if hand.squeeze >= LET_GO => self.carry_to(carry, &aim, hand.stick, frame),
                    _ => {
                        self.carry = None;
                        step.dropped = true;
                    }
                }
            }
        }
        step.carrying = self.carry.is_some();
        step.moved = self.place != before;
        if step.carrying || step.dropped {
            // The mouse stays still while the user carries the panel
            self.pressed = None;
            self.ignoring = frame.hands.iter().any(|h| h.trigger > PRESS);
            return step;
        }

        // The hand that points: the active one while it hits, else the right one, else the left
        if hits[self.active].is_none() {
            if let Some(other) = [1, 0].into_iter().find(|&h| hits[h].is_some()) {
                self.active = other;
            }
        }
        // A trigger pulled on the panel by the other hand takes over
        let other = 1 - self.active;
        if hits[other].is_some() && frame.hands[other].trigger > PRESS && frame.hands[self.active].trigger <= PRESS {
            self.active = other;
        }
        let hand = &frame.hands[self.active];
        let hit = hits[self.active];
        match (self.pressed, hit) {
            // Held outside the panel's edge: the press follows the edge
            (Some(last), None) if hand.trigger >= RELEASE => {
                step.pointer = PointerNow { hit: Some(last), trigger: hand.trigger, scroll: 0.0 };
            }
            (Some(_), _) if hand.trigger < RELEASE => {
                self.pressed = None;
                step.pointer = PointerNow { hit, trigger: hand.trigger, scroll: 0.0 };
            }
            (Some(_), Some(at)) => {
                self.pressed = Some(at);
                step.pointer = PointerNow { hit, trigger: hand.trigger, scroll: stick_y(hand) };
            }
            (None, Some(at)) => {
                if hand.trigger < RELEASE {
                    self.ignoring = false;
                }
                if hand.trigger >= PRESS && !self.ignoring {
                    self.pressed = Some(at);
                } else if hand.trigger >= PRESS {
                    self.ignoring = true;
                }
                let trigger = if self.ignoring { 0.0 } else { hand.trigger };
                step.pointer = PointerNow { hit, trigger, scroll: stick_y(hand) };
            }
            (Some(_), None) => self.pressed = None,
            (None, None) => {
                self.ignoring = hand.trigger >= PRESS;
            }
        }
        step
    }

    /// Puts the panel where the grab holds it, with the thumbstick's push and resize.
    fn carry_to(&mut self, mut carry: Carry, aim: &Pose, stick: [f32; 2], frame: &Frame) {
        let dt = frame.dt.clamp(0.0, 0.1);
        let [x, y] = stick.map(|v| if v.abs() < DEAD_ZONE { 0.0 } else { v });
        carry.relative.position[2] =
            (carry.relative.position[2] - y * PUSH_SPEED * dt).clamp(-DISTANCE_RANGE[1], -DISTANCE_RANGE[0]);
        self.carry = Some(carry);
        self.place.width = (self.place.width * (x * RESIZE_SPEED * dt).exp()).clamp(WIDTH_RANGE[0], WIDTH_RANGE[1]);
        let held = aim.then(&carry.relative);
        self.place.center = held.position;
        self.place = match frame.head {
            Some(head) => self.place.facing(head),
            None => self.place,
        };
    }
}

/// The thumbstick's forward lean outside the dead zone, with the zone's width taken off.
fn stick_y(hand: &Hand) -> f32 {
    let y = hand.stick[1];
    if y.abs() < DEAD_ZONE {
        0.0
    } else {
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: [f32; 3] = [0.0, 0.0, 0.0];

    /// A controller at `position` that points straight ahead along -z.
    fn hand(position: [f32; 3]) -> Hand {
        Hand { aim: Some(Pose::at(position)), ..Hand::default() }
    }

    fn frame(left: Hand, right: Hand) -> Frame {
        Frame { hands: [left, right], head: Some(HEAD), dt: 1.0 / 90.0 }
    }

    fn engine() -> Engine {
        // 1.6 m wide, 16:9, 1.5 m ahead of the head
        Engine::new(Place { center: [0.0, 0.0, -1.5], yaw: 0.0, pitch: 0.0, width: 1.6 }, 16.0 / 9.0)
    }

    #[test]
    fn a_controller_pointing_at_the_middle_hits_the_middle() {
        let engine = engine();
        let hit = engine.hit(&hand([0.0, 0.0, 0.0])).unwrap();
        assert!((hit[0] - 0.5).abs() < 1e-5 && (hit[1] - 0.5).abs() < 1e-5);
        // 40 cm to the right of the middle on a panel 1.6 m wide is at u 0.75. 22.5 cm up is at v 0.25.
        let hit = engine.hit(&hand([0.4, 0.225, 0.0])).unwrap();
        assert!((hit[0] - 0.75).abs() < 1e-4 && (hit[1] - 0.25).abs() < 1e-4, "{hit:?}");
        // Beside the panel and behind it: no hit
        assert!(engine.hit(&hand([1.0, 0.0, 0.0])).is_none());
        assert!(engine.hit(&hand([0.0, 0.0, -3.0])).is_none());
    }

    #[test]
    fn the_modifier_puts_the_panel_where_the_place_says() {
        // The subspace starts a meter ahead: a panel 1.5 m ahead is 0.5 m behind that, 500 dp
        let place = Place { center: [0.2, 0.1, -1.5], yaw: 10.0, pitch: -5.0, width: 1.6 };
        let modifier = place.modifier(16.0 / 9.0);
        let size = modifier.size_or([0.0, 0.0]);
        assert!((size[0] - 1600.0).abs() < 1e-2 && (size[1] - 900.0).abs() < 1e-2, "{size:?}");
        let pose = modifier.pose();
        assert!((pose.position[0] - 0.2).abs() < 1e-6 && (pose.position[1] - 0.1).abs() < 1e-6 && (pose.position[2] + 0.5).abs() < 1e-6);
        // The orientation is the same quaternion as the one that the hit test uses
        let ours = place.pose().orientation;
        for i in 0..4 {
            assert!((pose.orientation[i] - ours[i]).abs() < 1e-5, "{:?} {:?}", pose.orientation, ours);
        }
    }

    #[test]
    fn the_cursor_panel_lies_on_the_point_in_front_of_the_picture() {
        // A panel 1.6 m by 0.9 m, 1.5 m ahead. The cursor at one quarter across and one quarter
        // down is 40 cm left of the middle and 22.5 cm above it, 4 mm in front
        let place = Place { center: [0.0, 0.0, -1.5], yaw: 0.0, pitch: 0.0, width: 1.6 };
        let modifier = place.cursor_modifier(16.0 / 9.0, [0.25, 0.25], 26.0, 0.004);
        let at = modifier.pose().position;
        assert!((at[0] + 0.4).abs() < 1e-5 && (at[1] - 0.225).abs() < 1e-5 && (at[2] - (-1.5 + 0.004 + 1.0)).abs() < 1e-5, "{at:?}");
        assert_eq!(modifier.size_or([0.0, 0.0]), [26.0, 26.0]);
    }

    #[test]
    fn facing_the_head_turns_the_panel_toward_it() {
        let place = Place { center: [1.0, 0.0, -1.0], ..Place::default() }.facing(HEAD);
        // A panel to the right and ahead faces back to the left: a negative yaw turns its face left
        assert!((place.yaw + 45.0).abs() < 1e-3 && place.pitch.abs() < 1e-3, "{place:?}");
        // The normal then points at the head
        let normal = place.pose().rotate([0.0, 0.0, 1.0]);
        let to_head = [-1.0 / 2f32.sqrt(), 0.0, 1.0 / 2f32.sqrt()];
        for i in 0..3 {
            assert!((normal[i] - to_head[i]).abs() < 1e-4, "{normal:?}");
        }
        // A panel above the head tilts its face down
        let high = Place { center: [0.0, 1.0, -1.0], ..Place::default() }.facing(HEAD);
        let normal = high.pose().rotate([0.0, 0.0, 1.0]);
        assert!(normal[1] < -0.5, "{normal:?}");
    }

    #[test]
    fn the_place_survives_the_preferences() {
        let place = Place { center: [0.3, -0.2, -1.1], yaw: 12.5, pitch: -3.0, width: 2.2 };
        assert_eq!(Place::from_text(&place.to_text()), Some(place));
        assert_eq!(Place::from_text("garbage"), None);
        assert_eq!(Place::from_text("0,0,-1,0,0,100"), None);
        assert_eq!(Place::from_text("0,0,-50,0,0,1.6"), None);
        assert_eq!(Place::from_text("NaN,0,-1,0,0,1.6"), None);
    }

    #[test]
    fn a_trigger_pulled_on_the_panel_clicks_and_a_trigger_pulled_beside_it_does_not() {
        let mut engine = engine();
        let mut right = hand([0.0, 0.0, 0.0]);
        let step = engine.step(&frame(Hand::default(), right));
        assert!(step.pointer.hit.is_some() && step.pointer.trigger == 0.0);
        right.trigger = 1.0;
        let step = engine.step(&frame(Hand::default(), right));
        assert_eq!(step.pointer.trigger, 1.0);
        // The controller leaves the panel's edge with the trigger held: the press stays at the edge
        let mut away = hand([2.0, 0.0, 0.0]);
        away.trigger = 1.0;
        let step = engine.step(&frame(Hand::default(), away));
        assert!(step.pointer.hit.is_some() && step.pointer.trigger == 1.0);
        // The trigger lets go outside the panel: the host gets the release
        away.trigger = 0.0;
        let step = engine.step(&frame(Hand::default(), away));
        assert_eq!(step.pointer.trigger, 0.0);

        // A trigger that was down before the controller met the panel does not click
        let mut engine = self::engine();
        let mut beside = hand([2.0, 0.0, 0.0]);
        beside.trigger = 1.0;
        engine.step(&frame(Hand::default(), beside));
        let mut onto = hand([0.0, 0.0, 0.0]);
        onto.trigger = 1.0;
        let step = engine.step(&frame(Hand::default(), onto));
        assert!(step.pointer.hit.is_some());
        assert_eq!(step.pointer.trigger, 0.0);
        onto.trigger = 0.0;
        engine.step(&frame(Hand::default(), onto));
        onto.trigger = 1.0;
        assert_eq!(engine.step(&frame(Hand::default(), onto)).pointer.trigger, 1.0);
    }

    #[test]
    fn the_thumbstick_scrolls_while_the_controller_points_at_the_panel() {
        let mut engine = engine();
        let mut right = hand([0.0, 0.0, 0.0]);
        right.stick = [0.0, 0.7];
        assert_eq!(engine.step(&frame(Hand::default(), right)).pointer.scroll, 0.7);
        right.stick = [0.0, 0.05];
        assert_eq!(engine.step(&frame(Hand::default(), right)).pointer.scroll, 0.0);
        // Off the panel nothing goes to the host
        let mut away = hand([3.0, 0.0, 0.0]);
        away.stick = [0.0, 1.0];
        let step = engine.step(&frame(Hand::default(), away));
        assert_eq!(step.pointer, PointerNow::default());
        assert!(!step.pointer.busy());
    }

    #[test]
    fn the_left_hand_points_when_the_right_one_does_not() {
        let mut engine = engine();
        let step = engine.step(&frame(hand([0.4, 0.0, 0.0]), hand([5.0, 0.0, 0.0])));
        assert!(step.pointer.hit.is_some_and(|h| (h[0] - 0.75).abs() < 1e-4));
    }

    #[test]
    fn a_squeezed_grip_carries_the_panel_with_the_controller_and_a_release_drops_it() {
        let mut engine = engine();
        let mut right = hand([0.0, 0.0, 0.0]);
        right.squeeze = 1.0;
        let step = engine.step(&frame(Hand::default(), right));
        assert!(step.carrying && !step.moved);
        // The controller moves 0.5 m to the right and 0.3 m up: the panel goes along
        let mut moved = hand([0.5, 0.3, 0.0]);
        moved.squeeze = 1.0;
        let step = engine.step(&frame(Hand::default(), moved));
        assert!(step.carrying && step.moved);
        assert!((engine.place.center[0] - 0.5).abs() < 1e-4 && (engine.place.center[1] - 0.3).abs() < 1e-4);
        assert!((engine.place.center[2] + 1.5).abs() < 1e-4);
        // The panel faces the head: it is to the right of the head, so its yaw is negative
        assert!(engine.place.yaw < -5.0);
        // The mouse stays still during the carry
        assert_eq!(step.pointer, PointerNow::default());
        // Letting go drops it where it is
        moved.squeeze = 0.0;
        let step = engine.step(&frame(Hand::default(), moved));
        assert!(step.dropped && !step.carrying);
        assert!((engine.place.center[0] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn a_grip_squeezed_beside_the_panel_grabs_nothing() {
        let mut engine = engine();
        let mut right = hand([4.0, 0.0, 0.0]);
        right.squeeze = 1.0;
        let step = engine.step(&frame(Hand::default(), right));
        assert!(!step.carrying && !step.moved);
    }

    #[test]
    fn the_thumbstick_pushes_and_resizes_a_carried_panel_within_limits() {
        let mut engine = engine();
        let mut right = hand([0.0, 0.0, 0.0]);
        right.squeeze = 1.0;
        engine.step(&frame(Hand::default(), right));
        right.stick = [0.0, 1.0];
        for _ in 0..90 {
            engine.step(&frame(Hand::default(), right));
        }
        // A second at full lean moves the panel 1.2 m farther
        assert!((engine.place.center[2] + 2.7).abs() < 0.05, "{:?}", engine.place);
        right.stick = [0.0, -1.0];
        for _ in 0..900 {
            engine.step(&frame(Hand::default(), right));
        }
        assert!((engine.place.center[2] + DISTANCE_RANGE[0]).abs() < 1e-3);
        right.stick = [1.0, 0.0];
        let width = engine.place.width;
        for _ in 0..90 {
            engine.step(&frame(Hand::default(), right));
        }
        assert!(engine.place.width > width * 1.9 && engine.place.width < width * 2.2, "{}", engine.place.width);
        for _ in 0..2000 {
            engine.step(&frame(Hand::default(), right));
        }
        assert_eq!(engine.place.width, WIDTH_RANGE[1]);
    }

    #[test]
    fn a_pointer_that_barely_moved_is_not_worth_sending() {
        let a = PointerNow { hit: Some([0.5, 0.5]), trigger: 0.0, scroll: 0.0 };
        assert!(!a.differs(&PointerNow { hit: Some([0.500_02, 0.5]), ..a }));
        assert!(a.differs(&PointerNow { hit: Some([0.501, 0.5]), ..a }));
        assert!(a.differs(&PointerNow { hit: None, ..a }));
        assert!(a.differs(&PointerNow { trigger: 0.5, ..a }));
    }
}
