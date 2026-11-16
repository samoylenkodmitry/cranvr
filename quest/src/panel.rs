// cranvr: the desktop panels' places in the headset's own stage space, and the controllers that
// point at them, move them, resize them and press their buttons.
//
// The app owns the panels' places. The host works in ALVR's recentered space, which differs from
// the headset's stage space, so a place from the host would put a panel elsewhere than the host
// thinks. The app also points with the headset's own aim poses at the refresh it draws, so the
// ray, the hit and the panel always agree.
//
// Each panel shows one Mac display. Under it lie its handle and its buttons:
// - The handle is a bar, as in the Quest's own apps: point at it and hold the trigger to carry
//   the panel along the ray. The thumbstick of that hand pushes it away or pulls it near. The
//   panel keeps its angle to the ray: a turn of the wrist turns, tilts or rolls the panel by as
//   much. Within LEVEL_SNAP degrees of level its top edge snaps level.
//   Pointing at the handle without the trigger, the thumbstick moves the panel away or near.
// - The buttons right of the handle pause and resume the panel's picture, change its quality
//   (automatic, then 1920, 2560 and 3840 pixels across), add a panel with a new Mac display, and
//   close an added panel. The handle and the buttons form a bar that the compositor shows as its
//   own layer, sharp like the panel (see `bars`).
// A trigger pull turns the controller a little, and the Mac would see a drag instead of a click.
// A press on the picture therefore holds the point where the ray pointed just before the pull,
// until the ray turns more than CLICK_ANGLE degrees in the press's first CLICK_TIME seconds, or
// more than DRAG_ANGLE degrees later: then the press drags. A shaky hand still clicks.
// The pointing rays go through a one-euro filter: a resting hand's tremor barely moves them, and a
// fast turn follows with little lag. The hits, the drawn rays and the cursor use the same rays.
// Each press on a picture adds a line to panels-presses.txt beside the panels' file: its length in
// seconds, the ray's largest turn in degrees and whether it dragged, for tuning these numbers.
// A corner of a panel, just outside the picture, resizes it: point there, hold the trigger and
// pull. The panel keeps its center and its shape. A click on the right thumbstick brings the
// panel under the ray, or the first panel, in front of the user.
//
// The app keeps the panels' places in a file, so they stay where the user left them.
//
// For each panel the app tells the host how the head sees it and how many pixels across the
// headset can show of it. The host sends fewer frames to a panel at the edge of the view, none to
// a panel out of view, and in automatic quality no more pixels than the headset can show.

use alvr_common::{
    glam::{Mat4, Quat, Vec3},
    Pose,
};
use crate::console::{self, Kind, Menu, CONSOLE_ID};
use alvr_graphics::{OverlayDraw, OverlayShape};
use std::{collections::HashMap, path::PathBuf};

/// How far in front of the user a new panel goes, in meters
const DISTANCE: f32 = 1.4;
/// The width of the first panel, in meters. At 1.4 m this covers about 75 degrees.
const WIDTH: f32 = 2.2;
/// The width of an added panel
const EXTRA_WIDTH: f32 = 1.6;
const MIN_WIDTH: f32 = 0.3;
const MAX_WIDTH: f32 = 6.0;
/// The handle bar: its gap to the panel's bottom edge and its height
const HANDLE_GAP: f32 = 0.04;
const HANDLE_HEIGHT: f32 = 0.018;
const HANDLE_WIDTH: f32 = 0.2;
/// The handle catches a ray within this height, more than the bar itself
const HANDLE_REACH: f32 = 0.07;
/// The buttons' height and the gap between them
const BUTTON_HEIGHT: f32 = 0.04;
const BUTTON_GAP: f32 = 0.012;
/// The bar's margin around the handle and the buttons
const BAR_MARGIN: f32 = 0.006;
/// A press on the picture holds its point until the ray turns this far, in degrees
const DRAG_ANGLE: f32 = 4.0;
/// In its first CLICK_TIME seconds a press holds its point until the ray turns CLICK_ANGLE degrees
const CLICK_TIME: f32 = 0.35;
const CLICK_ANGLE: f32 = 10.0;
/// The pointing rays' one-euro filter: its cutoff in hertz while the hand rests, and how far the
/// cutoff rises for each radian per second that the ray turns
const STEADY_CUTOFF: f32 = 1.0;
const STEADY_BETA: f32 = 2.0;
/// The cutoff in hertz of the filter's own estimate of the turning speed
const SPEED_CUTOFF: f32 = 1.0;
/// A carried panel follows the hand with this time constant, in seconds
const CARRY_SMOOTH: f32 = 0.05;
/// A carried panel whose top edge lies within this many degrees of level snaps level
const LEVEL_SNAP: f32 = 4.0;
/// The press log stops growing at this size
const PRESS_LOG_LIMIT: u64 = 200_000;
/// A press takes the point where the ray pointed this long before, in seconds: the pull of the
/// trigger turns the controller
const PRESS_LOOKBACK: f32 = 0.08;
/// A corner catches a ray this far out from the picture
const CORNER_REACH: f32 = 0.07;
/// A ray that hits nothing has this length
const RAY_LENGTH: f32 = 1.5;
/// A trigger counts as pressed above PRESS and as released below RELEASE
const PRESS: f32 = 0.6;
const RELEASE: f32 = 0.4;
/// How fast the thumbstick moves a carried panel, as a share of its distance per second
const PUSH_SPEED: f32 = 1.2;
/// A panel within this angle of the view direction gets the full frame rate
const CENTER_ANGLE: f32 = 25.0;
/// The pixels per degree that the host should send: twice what a Quest 3 shows in the middle of
/// its lenses, for the compositor's supersampling
const PIXELS_PER_DEGREE: f32 = 50.0;
/// The picture widths of the fixed qualities
pub const QUALITY_WIDTHS: [u32; 3] = [1920, 2560, 3840];
pub const MAX_PANELS: usize = 4;

/// A flat panel in the stage space. Its front faces +Z of `orientation`; +X is its right.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Panel {
    pub center: Vec3,
    pub orientation: Quat,
    pub width: f32,
    pub height: f32,
}

/// How the head sees a panel. The numbers go to the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gaze {
    Out = 0,
    Side = 1,
    Center = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Pause,
    Quality,
    Add,
    Close,
}

impl Button {
    fn width(self) -> f32 {
        match self {
            Button::Pause => 0.13,
            Button::Quality => 0.17,
            Button::Add => 0.19,
            Button::Close => 0.12,
        }
    }
}

/// The part of a panel that a ray points at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Part {
    /// The picture, at (u, v) from its top left corner (0, 0) to its bottom right corner (1, 1)
    Screen { u: f32, v: f32 },
    Handle,
    Corner,
    Button(Button),
    /// An item on the console's face, by its index in `console::layout`; NONE for the face around
    /// the items
    Item(u8),
}

/// The console's face around its items
const NONE: u8 = u8::MAX;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub panel: u8,
    pub distance: f32,
    pub part: Part,
}

/// A request for the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Pause(u8, bool),
    /// 0 for automatic, else 1 + an index into QUALITY_WIDTHS
    Quality(u8, u8),
    Add,
    Close(u8),
}

/// How a part of the bar looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Look {
    Idle,
    Hover,
    Active,
    /// Text without a button under it
    Label,
}

/// A button of a bar: its center and size in meters from the bar's center, its text, its look.
#[derive(Clone, Debug, PartialEq)]
pub struct BarButton {
    pub center: (f32, f32),
    pub size: (f32, f32),
    pub label: String,
    pub look: Look,
}

/// A panel's handle and buttons, as one flat picture under the panel.
#[derive(Clone, Debug, PartialEq)]
pub struct Bar {
    pub panel: u8,
    pub center: Vec3,
    pub orientation: Quat,
    /// Width and height in meters
    pub size: (f32, f32),
    /// The handle's center and size in meters from the bar's center, and its look
    pub handle: ((f32, f32), (f32, f32), Look),
    pub buttons: Vec<BarButton>,
    /// A plate behind everything, for the console
    pub background: Option<[f32; 4]>,
}

/// What the host reported about a panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PanelState {
    pub paused: bool,
    pub quality: u8,
    /// The picture's width in pixels
    pub width: u32,
}

/// The rotation that turns a panel's front to `normal` and keeps the panel upright.
fn facing(normal: Vec3) -> Quat {
    let flat = Vec3::new(normal.x, 0.0, normal.z).try_normalize().unwrap_or(Vec3::Z);
    Quat::from_rotation_y(flat.x.atan2(flat.z))
}

/// `q` with its top edge level when the edge lies within LEVEL_SNAP degrees of level.
fn level_snap(q: Quat) -> Quat {
    let (right, normal) = (q * Vec3::X, q * Vec3::Z);
    let Some(level) = Vec3::Y.cross(normal).try_normalize() else { return q };
    let roll = normal.dot(right.cross(level)).atan2(right.dot(level));
    if roll.abs().to_degrees() < LEVEL_SNAP {
        (Quat::from_axis_angle(normal, roll) * q).normalize()
    } else {
        q
    }
}

/// A one-euro filter on a controller's pointing direction.
#[derive(Clone, Copy, Debug, Default)]
struct AimFilter {
    dir: Option<Vec3>,
    /// The turning speed in radians per second, itself filtered
    speed: f32,
}

/// The share of the way to the new value that a low-pass filter at `cutoff` hertz goes in `dt`
fn low_pass(dt: f32, cutoff: f32) -> f32 {
    let tau = 1.0 / (2.0 * std::f32::consts::PI * cutoff);
    1.0 / (1.0 + tau / dt)
}

impl AimFilter {
    fn apply(&mut self, aim: Pose, dt: f32) -> Pose {
        let raw = aim.orientation * Vec3::NEG_Z;
        let Some(prev) = self.dir.filter(|_| dt > 0.0) else {
            self.dir = Some(raw);
            return aim;
        };
        self.speed += low_pass(dt, SPEED_CUTOFF) * (prev.angle_between(raw) / dt - self.speed);
        let cutoff = STEADY_CUTOFF + STEADY_BETA * self.speed;
        let dir = prev.lerp(raw, low_pass(dt, cutoff)).try_normalize().unwrap_or(raw);
        self.dir = Some(dir);
        Pose { orientation: (Quat::from_rotation_arc(raw, dir) * aim.orientation).normalize(), position: aim.position }
    }
}

/// One press on a picture, for the press log
#[derive(Clone, Copy, Debug)]
struct PressRecord {
    start: f32,
    turn: f32,
    dragged: bool,
}

impl Panel {
    /// Upright, at eye height, in the direction the head looks, facing the user.
    pub fn in_front_of(head: &Pose, aspect: f32, width: f32) -> Self {
        let forward = head.orientation * Vec3::NEG_Z;
        let flat = Vec3::new(forward.x, 0.0, forward.z).try_normalize().unwrap_or(Vec3::NEG_Z);
        Self { center: head.position + flat * DISTANCE, orientation: facing(-flat), width, height: width / aspect }
    }

    pub fn right(&self) -> Vec3 {
        self.orientation * Vec3::X
    }

    pub fn up(&self) -> Vec3 {
        self.orientation * Vec3::Y
    }

    /// The front's direction
    pub fn normal(&self) -> Vec3 {
        self.orientation * Vec3::Z
    }

    /// The point at (x, y) meters from the center, along the panel's right and up
    pub fn at(&self, x: f32, y: f32) -> Vec3 {
        self.center + self.right() * x + self.up() * y
    }

    fn handle_offset(&self) -> f32 {
        -(self.height / 2.0 + HANDLE_GAP + HANDLE_HEIGHT / 2.0)
    }

    fn handle_width(&self) -> f32 {
        HANDLE_WIDTH
    }

    /// Where a ray from `origin` along `dir` meets the panel's plane from the front: the distance
    /// and the point in meters from the center, along the panel's right and up.
    fn plane_hit(&self, origin: Vec3, dir: Vec3) -> Option<(f32, f32, f32)> {
        let n = self.normal();
        let facing = dir.dot(n);
        if facing > -1e-4 {
            return None;
        }
        let distance = (self.center - origin).dot(n) / facing;
        if distance <= 0.0 {
            return None;
        }
        let p = origin + dir * distance - self.center;
        Some((distance, p.dot(self.right()), p.dot(self.up())))
    }

    /// Whether the head looks toward the panel: its center or a corner lies within about 66
    /// degrees of the view direction.
    pub fn in_view(&self, head: &Pose) -> bool {
        let forward = head.orientation * Vec3::NEG_Z;
        let (w, h) = (self.width / 2.0, self.height / 2.0);
        [self.center, self.at(w, h), self.at(w, -h), self.at(-w, h), self.at(-w, -h)]
            .into_iter()
            .any(|p| (p - head.position).try_normalize().is_some_and(|d| d.dot(forward) > 0.4))
    }

    /// How the head sees the panel, and the pixels across that the host should send for it.
    pub fn gaze(&self, head: &Pose) -> (Gaze, u16) {
        if !self.in_view(head) {
            return (Gaze::Out, 0);
        }
        let forward = head.orientation * Vec3::NEG_Z;
        // The panel's point nearest the view direction
        let nearest = match self.plane_hit(head.position, forward) {
            Some((_, x, y)) => self.at(x.clamp(-self.width / 2.0, self.width / 2.0), y.clamp(-self.height / 2.0, self.height / 2.0)),
            None => self.center,
        };
        let angle = forward.angle_between(nearest - head.position).to_degrees();
        let distance = (self.center - head.position).length().max(0.1);
        let degrees = 2.0 * (self.width / 2.0 / distance).atan().to_degrees();
        let wanted = (degrees * PIXELS_PER_DEGREE).min(u16::MAX as f32) as u16;
        (if angle < CENTER_ANGLE { Gaze::Center } else { Gaze::Side }, wanted)
    }

    fn save(&self, id: u8) -> String {
        let (c, q) = (self.center, self.orientation);
        format!("{id} {} {} {} {} {} {} {} {}\n", c.x, c.y, c.z, q.x, q.y, q.z, q.w, self.width)
    }

    fn load(line: &str) -> Option<(u8, Self)> {
        let mut words = line.split_whitespace();
        let id: u8 = words.next()?.parse().ok()?;
        let v: Vec<f32> = words.map(str::parse).collect::<Result<_, _>>().ok()?;
        let [cx, cy, cz, qx, qy, qz, qw, width] = v[..] else { return None };
        let q = Quat::from_xyzw(qx, qy, qz, qw);
        let orientation = (q.length() > 0.5).then(|| q.normalize())?;
        (width > 0.1 && v.iter().all(|x| x.is_finite())).then_some((id, Self { center: Vec3::new(cx, cy, cz), orientation, width, height: width }))
    }
}

/// One panel on show: its place and what the host reported about it.
struct Slot {
    id: u8,
    panel: Panel,
    state: PanelState,
}

impl Slot {
    /// The buttons and their centers in meters from the panel's center: in the handle's row,
    /// right of the handle.
    fn buttons(&self, can_add: bool) -> Vec<(Button, f32, f32)> {
        let mut list = vec![Button::Pause, Button::Quality];
        if can_add {
            list.push(Button::Add);
        }
        if self.id != 0 {
            list.push(Button::Close);
        }
        let y = self.panel.handle_offset();
        let mut left = HANDLE_WIDTH / 2.0 + 0.05;
        list.into_iter()
            .map(|b| {
                let x = left + b.width() / 2.0;
                left += b.width() + BUTTON_GAP;
                (b, x, y)
            })
            .collect()
    }

    fn hit(&self, origin: Vec3, dir: Vec3, can_add: bool) -> Option<Hit> {
        let p = &self.panel;
        let (distance, x, y) = p.plane_hit(origin, dir)?;
        let (w, h) = (p.width / 2.0, p.height / 2.0);
        let near = |v: f32, half: f32| v.abs() > half - 0.03 && v.abs() < half + CORNER_REACH;
        let part = if near(x, w) && near(y, h) && (x.abs() > w || y.abs() > h) {
            Part::Corner
        } else if x.abs() <= w && y.abs() <= h {
            Part::Screen { u: x / p.width + 0.5, v: 0.5 - y / p.height }
        } else if x.abs() <= p.handle_width() / 2.0 + 0.02 && (y - p.handle_offset()).abs() <= HANDLE_REACH / 2.0 {
            Part::Handle
        } else {
            let (button, ..) = self
                .buttons(can_add)
                .into_iter()
                .find(|&(b, bx, by)| (x - bx).abs() <= b.width() / 2.0 && (y - by).abs() <= BUTTON_HEIGHT / 2.0)?;
            Part::Button(button)
        };
        Some(Hit { panel: self.id, distance, part })
    }
}

/// One controller as the headset tracks it at the coming refresh.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hand {
    /// The pointing pose: -Z points where the controller points
    pub aim: Option<Pose>,
    /// The handle's pose: -Z runs along the handle
    pub grip: Option<Pose>,
    pub trigger: f32,
    pub stick_y: f32,
}

#[derive(Clone)]
pub struct Inputs {
    /// Left, then right
    pub hands: [Hand; 2],
    pub head: Pose,
    /// The right thumbstick is pressed down
    pub stick_click: bool,
    /// Seconds since the last update
    pub dt: f32,
}

/// What goes to the host.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Output {
    /// The panel and the position on it that the active controller points at
    pub pointer: Option<(u8, f32, f32)>,
    pub trigger: f32,
    pub scroll: f32,
    /// For each panel: how the head sees it and the pixels across that it needs
    pub views: Vec<(u8, Gaze, u16)>,
    /// The panel picture that the head points at, where on it, and its pixels across
    pub look: Option<(u8, f32, f32, u16)>,
    pub commands: Vec<Command>,
    /// The console's requests for the host, as JSON
    pub console: Vec<String>,
}

enum Grab {
    /// Carrying a panel by its handle
    Move {
        distance: f32,
        /// The panel's center minus the grabbed point, in the panel's own axes
        offset: Vec3,
        /// The panel's orientation in the controller's axes
        relative: Quat,
    },
    Resize,
}

pub struct PanelControl {
    slots: Vec<Slot>,
    /// Places from the file, and of panels that left
    saved: HashMap<u8, Panel>,
    /// The controller that points for the mouse: the one whose trigger was pressed last
    active: usize,
    pressed: [bool; 2],
    clicked: bool,
    /// The active trigger went down on a picture: the host gets it until it goes up, also when
    /// the ray leaves the picture meanwhile
    screen_press: bool,
    /// The hand, the panel and what the hand does with it
    grab: Option<(usize, u8, Grab)>,
    /// Seconds since the start, from the updates' steps
    clock: f32,
    /// Each hand's recent rays: the time, the point on a picture, the ray's direction
    history: [std::collections::VecDeque<(f32, Option<(u8, f32, f32)>, Vec3)>; 2],
    /// A press on the picture holds this point while the ray stays near this direction; the
    /// press's start
    hold: Option<((u8, f32, f32), Vec3, f32)>,
    /// The press on a picture under way
    press: Option<PressRecord>,
    /// Each hand's ray filter
    steady: [AimFilter; 2],
    /// Whether the rays go through the filter
    pub steady_rays: bool,
    /// The console's place and what it shows, once the host sent a menu
    console: Option<(Panel, Menu)>,
    /// Whether the console waits for the head's pose to take its default place
    place_console: bool,
    /// The thumbstick moves a panel by its handle; its place is saved when the stick rests
    pushing: bool,
    hands: [Hand; 2],
    head: Pose,
    targets: [Option<Hit>; 2],
    store: PathBuf,
}

impl PanelControl {
    /// Takes the panels' places from `store` if the file holds them.
    pub fn new(store: PathBuf) -> Self {
        let text = std::fs::read_to_string(&store).unwrap_or_default();
        Self {
            slots: Vec::new(),
            saved: text.lines().filter_map(Panel::load).collect(),
            active: 1,
            pressed: [false; 2],
            clicked: false,
            screen_press: false,
            grab: None,
            clock: 0.0,
            history: Default::default(),
            hold: None,
            press: None,
            steady: [AimFilter::default(); 2],
            steady_rays: true,
            console: None,
            place_console: false,
            pushing: false,
            hands: [Hand::default(); 2],
            head: Pose::default(),
            targets: [None; 2],
            store,
        }
    }

    /// The hands with their pointing rays through the filters.
    fn steadied(&mut self, inputs: &Inputs) -> Inputs {
        let mut out = inputs.clone();
        let on = self.steady_rays;
        for (hand, filter) in out.hands.iter_mut().zip(self.steady.iter_mut()) {
            match hand.aim {
                Some(aim) if on => hand.aim = Some(filter.apply(aim, inputs.dt)),
                _ => *filter = AimFilter::default(),
            }
        }
        out
    }

    /// The press log beside the panels' file: panels.txt gets panels-presses.txt.
    fn press_log(&self) -> PathBuf {
        let stem = self.store.file_stem().map_or("panels".into(), |s| s.to_string_lossy());
        self.store.with_file_name(format!("{stem}-presses.txt"))
    }

    /// Adds a press to the press log.
    fn log_press(&self, press: PressRecord) {
        use std::io::Write;
        let path = self.press_log();
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > PRESS_LOG_LIMIT) {
            return;
        }
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            writeln!(file, "{:.3} {:.2} {}", self.clock - press.start, press.turn, press.dragged as u8).ok();
        }
    }

    /// Takes the host's menu for the console. The console shows from the first menu on, at its
    /// saved place or below and ahead of the head.
    pub fn set_menu(&mut self, menu: Menu) {
        let (height, _) = console::layout(&menu);
        match &mut self.console {
            Some((panel, shown)) => {
                panel.height = height;
                *shown = menu;
            }
            None => {
                // The first menu can come before the first head pose. The console then takes its
                // default place in the next update, not at the stage's origin under the floor.
                let saved = self.saved.get(&CONSOLE_ID).copied();
                self.place_console = saved.is_none();
                let mut panel = saved.unwrap_or_else(|| console::default_place(self.head.position, self.head.orientation, height));
                (panel.width, panel.height) = (console::WIDTH, height);
                self.console = Some((panel, menu));
            }
        }
    }

    /// Whether the console is nearer to the head than every screen. The compositor stacks layers
    /// in the order of the list, not by depth: a nearer console must come after the screens.
    pub fn console_nearer(&self) -> bool {
        let Some((console, _)) = &self.console else { return false };
        let distance = |p: &Panel| (p.center - self.head.position).length();
        self.slots.iter().all(|s| distance(console) < distance(&s.panel))
    }

    /// Puts the console upright straight ahead of the head, for tests with a headset on a desk.
    pub fn console_in_front(&mut self) {
        let head = self.head;
        if let Some((panel, _)) = self.console.as_mut() {
            let forward = head.orientation * Vec3::NEG_Z;
            panel.center = head.position + forward * 0.8;
            panel.orientation = head.orientation;
        }
    }

    /// What a ray hits on the console.
    fn console_hit(&self, origin: Vec3, dir: Vec3) -> Option<Hit> {
        let (panel, menu) = self.console.as_ref()?;
        let (distance, x, y) = panel.plane_hit(origin, dir)?;
        if x.abs() > panel.width / 2.0 || y.abs() > panel.height / 2.0 {
            return None;
        }
        let (_, items) = console::layout(menu);
        let part = if console::on_handle(panel.height, (x, y)) {
            Part::Handle
        } else {
            let index = items.iter().position(|i| {
                matches!(i.kind, Kind::Button { .. })
                    && (x - i.center.0).abs() <= i.size.0 / 2.0
                    && (y - i.center.1).abs() <= i.size.1 / 2.0
            });
            Part::Item(index.map_or(NONE, |i| i as u8))
        };
        Some(Hit { panel: CONSOLE_ID, distance, part })
    }

    fn save(&mut self) {
        for slot in &self.slots {
            self.saved.insert(slot.id, slot.panel);
        }
        if let Some((panel, _)) = &self.console {
            self.saved.insert(CONSOLE_ID, *panel);
        }
        let mut ids: Vec<&u8> = self.saved.keys().collect();
        ids.sort();
        let text: String = ids.into_iter().map(|id| self.saved[id].save(*id)).collect();
        if let Some(dir) = self.store.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        std::fs::write(&self.store, text).ok();
    }

    /// The panels on show, with their numbers.
    pub fn panels(&self) -> impl Iterator<Item = (u8, Panel)> + '_ {
        self.slots.iter().map(|s| (s.id, s.panel))
    }

    /// Takes what the host reported about panel `id`.
    pub fn set_state(&mut self, id: u8, paused: bool, quality: u8) {
        if let Some(slot) = self.slots.iter_mut().find(|s| s.id == id) {
            (slot.state.paused, slot.state.quality) = (paused, quality);
        }
    }

    fn can_add(&self) -> bool {
        self.slots.len() < MAX_PANELS
    }

    /// A place for a new panel: the first one in front of the user, an added one to the right
    /// of the last one, facing the user.
    fn place(&self, id: u8, aspect: f32, head: &Pose) -> Panel {
        if let Some(mut panel) = self.saved.get(&id).copied() {
            panel.height = panel.width / aspect;
            return panel;
        }
        match self.slots.last() {
            Some(last) if id != 0 => {
                let p = &last.panel;
                let center = p.at(p.width / 2.0 + 0.1 + EXTRA_WIDTH / 2.0, 0.0);
                Panel { center, orientation: facing(head.position - center), width: EXTRA_WIDTH, height: EXTRA_WIDTH / aspect }
            }
            _ => Panel::in_front_of(head, aspect, if id == 0 { WIDTH } else { EXTRA_WIDTH }),
        }
    }

    /// Moves the panels by the controllers and finds what each controller points at. `streams`
    /// holds the panels on show: number, width over height, picture width in pixels.
    pub fn update(&mut self, inputs: &Inputs, streams: &[(u8, f32, u32)]) -> Output {
        let steadied = self.steadied(inputs);
        let inputs = &steadied;
        let head = inputs.head;
        self.head = head;
        if std::mem::take(&mut self.place_console) {
            if let Some((panel, _)) = self.console.as_mut() {
                *panel = console::default_place(head.position, head.orientation, panel.height);
            }
        }
        self.hands = inputs.hands;

        // The panels follow the host's streams
        let gone: Vec<u8> = self.slots.iter().map(|s| s.id).filter(|id| !streams.iter().any(|s| s.0 == *id)).collect();
        if !gone.is_empty() {
            self.save();
            self.slots.retain(|s| !gone.contains(&s.id));
        }
        for &(id, aspect, width) in streams {
            match self.slots.iter_mut().find(|s| s.id == id) {
                Some(slot) => {
                    slot.panel.height = slot.panel.width / aspect;
                    slot.state.width = width;
                }
                None => {
                    let panel = self.place(id, aspect, &head);
                    self.slots.push(Slot { id, panel, state: PanelState { width, ..Default::default() } });
                    self.slots.sort_by_key(|s| s.id);
                }
            }
        }
        if self.slots.is_empty() {
            self.grab = None;
            return Output::default();
        }

        let can_add = self.can_add();
        let ray = |hand: &Hand| hand.aim.map(|a| (a.position, a.orientation * Vec3::NEG_Z));
        let targets = inputs.hands.map(|hand| {
            ray(&hand).and_then(|(o, d)| {
                self.slots
                    .iter()
                    .filter_map(|s| s.hit(o, d, can_add))
                    .chain(self.console_hit(o, d))
                    .min_by(|a, b| a.distance.total_cmp(&b.distance))
            })
        });
        self.targets = targets;
        let mut press = [false; 2];
        for (i, hand) in inputs.hands.iter().enumerate() {
            let down = hand.aim.is_some() && hand.trigger > if self.pressed[i] { RELEASE } else { PRESS };
            press[i] = down && !self.pressed[i];
            self.pressed[i] = down;
            if press[i] {
                self.active = i;
            }
        }

        let mut out = Output::default();
        let mut moved = false;
        // The right thumbstick brings the panel that the ray points at in front of the user. A
        // click that points at nothing brings the first screen and the console.
        if inputs.stick_click && !self.clicked && self.grab.is_none() {
            let target = self.targets[self.active].map(|t| t.panel);
            let id = target.unwrap_or(self.slots[0].id);
            if let Some(slot) = self.slots.iter_mut().find(|s| s.id == id) {
                let aspect = slot.panel.width / slot.panel.height;
                slot.panel = Panel::in_front_of(&head, aspect, slot.panel.width);
                moved = true;
            }
            if let (None | Some(CONSOLE_ID), Some((panel, _))) = (target, self.console.as_mut()) {
                *panel = console::default_place(head.position, head.orientation, panel.height);
                moved = true;
            }
        }
        self.clicked = inputs.stick_click;

        if let Some((hand, id, grab)) = &mut self.grab {
            let held = ray(&inputs.hands[*hand]).filter(|_| self.pressed[*hand]);
            let console = self.console.as_mut().filter(|_| *id == CONSOLE_ID).map(|c| &mut c.0);
            match (held, self.slots.iter_mut().find(|s| s.id == *id).map(|s| &mut s.panel).or(console)) {
                (Some((origin, dir)), Some(panel)) => {
                    match grab {
                        Grab::Move { distance, offset, relative } => {
                            let push = inputs.hands[*hand].stick_y * PUSH_SPEED * inputs.dt * distance.max(0.5);
                            *distance = (*distance + push).clamp(0.3, 8.0);
                            let point = origin + dir * *distance;
                            // The panel keeps its angle to the ray: the wrist turns it
                            let aim = inputs.hands[*hand].aim.map_or(Quat::IDENTITY, |a| a.orientation);
                            let orientation = level_snap(aim * *relative);
                            // Smoothing keeps a far panel from shaking with the hand
                            let a = 1.0 - (-inputs.dt / CARRY_SMOOTH).exp();
                            panel.orientation = panel.orientation.slerp(orientation, a).normalize();
                            panel.center = panel.center.lerp(point + orientation * *offset, a);
                        }
                        Grab::Resize => {
                            // The corner follows the ray in the panel's plane; the center stays
                            let n = panel.normal();
                            let facing = dir.dot(n);
                            if facing.abs() > 1e-3 {
                                let distance = (panel.center - origin).dot(n) / facing;
                                let p = origin + dir * distance - panel.center;
                                let aspect = panel.width / panel.height;
                                let width = (2.0 * p.dot(panel.right()).abs()).max(2.0 * p.dot(panel.up()).abs() * aspect);
                                panel.width = width.clamp(MIN_WIDTH, MAX_WIDTH);
                                panel.height = panel.width / aspect;
                            }
                        }
                    }
                }
                _ => {
                    self.grab = None;
                    moved = true;
                }
            }
        } else if let (true, Some(hit), Some((origin, dir))) =
            (press[self.active], self.targets[self.active], ray(&inputs.hands[self.active]))
        {
            let slot = self.slots.iter().find(|s| s.id == hit.panel);
            let console = self.console.as_ref().filter(|_| hit.panel == CONSOLE_ID);
            match (hit.part, slot) {
                (Part::Handle, _) => {
                    if let Some(panel) = slot.map(|s| s.panel).or(console.map(|c| c.0)) {
                        let point = origin + dir * hit.distance;
                        let offset = panel.orientation.inverse() * (panel.center - point);
                        let aim = inputs.hands[self.active].aim.map_or(Quat::IDENTITY, |a| a.orientation);
                        let relative = aim.inverse() * panel.orientation;
                        self.grab = Some((self.active, hit.panel, Grab::Move { distance: hit.distance, offset, relative }));
                    }
                }
                (Part::Item(index), _) => {
                    let items = console.map(|(_, menu)| console::layout(menu).1).unwrap_or_default();
                    if let Some(Kind::Button { action, .. }) = items.get(index as usize).map(|i| &i.kind) {
                        out.console.push(action.request());
                    }
                }
                (Part::Corner, Some(_)) => self.grab = Some((self.active, hit.panel, Grab::Resize)),
                (Part::Button(button), Some(slot)) => out.commands.push(match button {
                    Button::Pause => Command::Pause(slot.id, !slot.state.paused),
                    Button::Quality => Command::Quality(slot.id, (slot.state.quality + 1) % (QUALITY_WIDTHS.len() as u8 + 1)),
                    Button::Add => Command::Add,
                    Button::Close => Command::Close(slot.id),
                }),
                _ => {}
            }
        }
        if moved {
            self.save();
        }

        // The thumbstick moves a panel away or near while the ray points at its handle
        let hand = inputs.hands[self.active];
        let target = self.targets[self.active];
        let push = hand.stick_y;
        match (target, hand.aim, &self.grab) {
            (Some(Hit { panel, part: Part::Handle, .. }), Some(aim), None) if push.abs() > 0.25 => {
                let console = self.console.as_mut().filter(|_| panel == CONSOLE_ID).map(|c| &mut c.0);
                if let Some(p) = self.slots.iter_mut().find(|s| s.id == panel).map(|s| &mut s.panel).or(console) {
                    // Along the ray, so the handle stays under it
                    let dir = aim.orientation * Vec3::NEG_Z;
                    let distance = (p.center - aim.position).length();
                    let step = push * PUSH_SPEED * inputs.dt * distance.max(0.5);
                    if (0.4..8.0).contains(&(distance + step)) {
                        p.center += dir * step;
                    }
                    self.pushing = true;
                }
            }
            _ if self.pushing && push.abs() <= 0.25 => {
                self.pushing = false;
                self.save();
            }
            _ => {}
        }

        // Each hand's recent rays, for the press's point
        self.clock += inputs.dt;
        for (i, hand) in inputs.hands.iter().enumerate() {
            let point = match self.targets[i] {
                Some(Hit { panel, part: Part::Screen { u, v }, .. }) => Some((panel, u, v)),
                _ => None,
            };
            let clock = self.clock;
            let history = &mut self.history[i];
            if let Some(aim) = hand.aim {
                history.push_back((clock, point, aim.orientation * Vec3::NEG_Z));
            }
            while history.front().is_some_and(|e| clock - e.0 > 0.5) {
                history.pop_front();
            }
        }
        let dir = hand.aim.map(|a| a.orientation * Vec3::NEG_Z);
        if press[self.active] {
            self.screen_press = self.grab.is_none() && matches!(target, Some(Hit { part: Part::Screen { .. }, .. }));
            // The point from just before the pull, on the same picture
            let clock = self.clock;
            let history = &self.history[self.active];
            self.hold = match (self.screen_press, target) {
                (true, Some(Hit { panel, .. })) => history
                    .iter()
                    .rev()
                    .find(|e| clock - e.0 >= PRESS_LOOKBACK)
                    .or(history.front())
                    .and_then(|&(_, point, dir)| point.filter(|p| p.0 == panel).map(|p| (p, dir, clock))),
                _ => None,
            };
            self.press = self.screen_press.then_some(PressRecord { start: clock, turn: 0.0, dragged: false });
        } else if !self.pressed[self.active] {
            if let Some(press) = self.press.take() {
                self.log_press(press);
            }
            self.screen_press = false;
            self.hold = None;
        }
        if let (Some((_, held, start)), Some(dir)) = (self.hold, dir) {
            let turn = held.angle_between(dir).to_degrees();
            let limit = if self.clock - start < CLICK_TIME { CLICK_ANGLE } else { DRAG_ANGLE };
            if let Some(press) = self.press.as_mut() {
                press.turn = press.turn.max(turn);
                press.dragged |= turn > limit;
            }
            if turn > limit {
                self.hold = None;
            }
        }
        let on_controls = matches!(target, Some(Hit { part: Part::Handle | Part::Corner | Part::Button(_) | Part::Item(_), .. }));
        if self.grab.is_none() && (!on_controls || self.screen_press) {
            out.trigger = if self.screen_press { hand.trigger } else { 0.0 };
            out.scroll = if on_controls { 0.0 } else { hand.stick_y };
            out.pointer = match (self.hold, target) {
                (Some((point, _, _)), _) => Some(point),
                (None, Some(Hit { panel, part: Part::Screen { u, v }, .. })) => Some((panel, u, v)),
                _ => None,
            };
        }
        out.views = self.slots.iter().map(|s| {
            let (gaze, wanted) = s.panel.gaze(&head);
            (s.id, gaze, wanted)
        }).collect();
        // The nearest picture along the head's direction
        let forward = head.orientation * Vec3::NEG_Z;
        out.look = self
            .slots
            .iter()
            .filter_map(|s| match s.hit(head.position, forward, false) {
                Some(Hit { distance, part: Part::Screen { u, v }, .. }) => Some((distance, s.id, u, v, s.panel.gaze(&head).1)),
                _ => None,
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id, u, v, wanted)| (id, u, v, wanted));
        out
    }

    /// Each panel's bar: the handle and the buttons, for a layer of its own.
    pub fn bars(&self) -> Vec<Bar> {
        let can_add = self.can_add();
        let mut bars = self
            .slots
            .iter()
            .map(|slot| {
                let p = &slot.panel;
                let hovered = |part: Part| self.targets.iter().flatten().any(|t| t.panel == slot.id && t.part == part);
                let buttons = slot.buttons(can_add);
                let right = buttons.last().map_or(HANDLE_WIDTH / 2.0, |(b, x, _)| x + b.width() / 2.0) + BAR_MARGIN;
                let left = -HANDLE_WIDTH / 2.0 - BAR_MARGIN;
                let (cx, height) = ((left + right) / 2.0, BUTTON_HEIGHT + 2.0 * BAR_MARGIN);
                let handle = if matches!(self.grab, Some((_, id, Grab::Move { .. })) if id == slot.id) {
                    Look::Active
                } else if hovered(Part::Handle) {
                    Look::Hover
                } else {
                    Look::Idle
                };
                let buttons = buttons
                    .into_iter()
                    .map(|(button, x, _)| {
                        let label = match button {
                            Button::Pause if slot.state.paused => "Resume".to_string(),
                            Button::Pause => "Pause".to_string(),
                            Button::Quality => match slot.state.quality {
                                0 => format!("Auto {}", slot.state.width),
                                q => format!("Fixed {}", QUALITY_WIDTHS[(q as usize - 1).min(2)]),
                            },
                            Button::Add => "Add display".to_string(),
                            Button::Close => "Close".to_string(),
                        };
                        let look = if hovered(Part::Button(button)) { Look::Hover } else { Look::Idle };
                        BarButton { center: (x - cx, 0.0), size: (button.width(), BUTTON_HEIGHT), label, look }
                    })
                    .collect();
                Bar {
                    panel: slot.id,
                    center: p.at(cx, p.handle_offset()) + p.normal() * 0.001,
                    orientation: p.orientation,
                    size: (right - left, height),
                    handle: ((-cx, 0.0), (HANDLE_WIDTH, HANDLE_HEIGHT), handle),
                    buttons,
                    background: None,
                }
            })
            .collect::<Vec<_>>();
        if let Some((panel, menu)) = &self.console {
            let (height, items) = console::layout(menu);
            let hovered = |part: Part| self.targets.iter().flatten().any(|t| t.panel == CONSOLE_ID && t.part == part);
            let handle = if matches!(self.grab, Some((_, CONSOLE_ID, Grab::Move { .. }))) {
                Look::Active
            } else if hovered(Part::Handle) {
                Look::Hover
            } else {
                Look::Idle
            };
            let buttons = items
                .into_iter()
                .enumerate()
                .map(|(i, item)| {
                    let look = match item.kind {
                        Kind::Button { selected: true, .. } => Look::Active,
                        Kind::Button { .. } if hovered(Part::Item(i as u8)) => Look::Hover,
                        Kind::Button { .. } => Look::Idle,
                        Kind::Title | Kind::Text => Look::Label,
                    };
                    BarButton { center: item.center, size: item.size, label: item.label, look }
                })
                .collect();
            bars.push(Bar {
                panel: CONSOLE_ID,
                center: panel.center,
                orientation: panel.orientation,
                size: (panel.width, height),
                handle: (console::handle_center(height), (console::HANDLE_WIDTH, console::HANDLE_HEIGHT), handle),
                buttons,
                background: Some([0.09, 0.1, 0.12, 1.0]),
            });
        }
        bars
    }

    /// The controllers, their rays, the panels' corners, and the point that the active ray hits.
    pub fn draws(&self) -> Vec<OverlayDraw> {
        let mut draws = Vec::new();
        let at = |pose: &Pose| Mat4::from_rotation_translation(pose.orientation, pose.position);
        let solid = |shape, model, color| OverlayDraw { shape, model, color };

        // A controller: a body around the pointing tip and a handle with round ends below it
        for hand in &self.hands {
            if let Some(grip) = &hand.grip {
                let color = [0.17, 0.18, 0.2, 1.0];
                let (radius, length) = (0.016, 0.085);
                let part = |shape, scale: Vec3, z: f32| {
                    solid(shape, at(grip) * Mat4::from_scale_rotation_translation(scale, Quat::IDENTITY, Vec3::new(0.0, 0.0, z)), color)
                };
                draws.push(part(OverlayShape::Cylinder, Vec3::new(radius * 2.0, radius * 2.0, length), 0.015));
                draws.push(part(OverlayShape::Sphere, Vec3::splat(radius * 2.0), 0.015 + length / 2.0));
            }
            if let Some(aim) = &hand.aim {
                draws.push(solid(
                    OverlayShape::Sphere,
                    at(aim) * Mat4::from_scale_rotation_translation(Vec3::new(0.06, 0.032, 0.07), Quat::IDENTITY, Vec3::new(0.0, -0.004, 0.03)),
                    [0.24, 0.25, 0.28, 1.0],
                ));
            }
        }

        for slot in &self.slots {
            let p = &slot.panel;
            // A flat shape in the panel's plane, a little in front of it
            let on_panel = |x: f32, y: f32, lift: f32| Mat4::from_rotation_translation(p.orientation, p.at(x, y) + p.normal() * lift);
            let flat = |width, height, radius| OverlayShape::RoundRect { width, height, radius, fade: 0.0 };
            let hovered = |part: Part| self.targets.iter().flatten().any(|t| t.panel == slot.id && t.part == part);
            let grabbed = self.grab.as_ref().is_some_and(|g| g.1 == slot.id);

            // The corners show while a ray points at the panel
            let resizing = grabbed && matches!(self.grab, Some((_, _, Grab::Resize)));
            if resizing || self.targets.iter().flatten().any(|t| t.panel == slot.id) {
                let color = if resizing {
                    [0.45, 0.7, 1.0, 1.0]
                } else if hovered(Part::Corner) {
                    [1.0, 1.0, 1.0, 0.95]
                } else {
                    [0.75, 0.77, 0.8, 0.45]
                };
                let (w, h, arm, thick, out) = (p.width / 2.0, p.height / 2.0, 0.06, 0.008, 0.015);
                for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                    let (cx, cy) = (sx * (w + out), sy * (h + out));
                    draws.push(OverlayDraw { shape: flat(arm, thick, thick / 2.0), model: on_panel(cx - sx * arm / 2.0, cy, 0.002), color });
                    draws.push(OverlayDraw { shape: flat(thick, arm, thick / 2.0), model: on_panel(cx, cy - sy * arm / 2.0, 0.002), color });
                }
            }
        }

        // The console's frame: boxes around its face give it a depth
        if let Some((p, _)) = &self.console {
            let (depth, rim) = (0.03, 0.014);
            let (w, h) = (p.width / 2.0, p.height / 2.0);
            let color = [0.2, 0.21, 0.24, 1.0];
            for (x, y, sx, sy) in [
                (0.0, h + rim / 2.0, 2.0 * (w + rim), rim),
                (0.0, -h - rim / 2.0, 2.0 * (w + rim), rim),
                (-w - rim / 2.0, 0.0, rim, 2.0 * h),
                (w + rim / 2.0, 0.0, rim, 2.0 * h),
            ] {
                let center = p.at(x, y) - p.normal() * (depth / 2.0 - 0.003);
                let model = Mat4::from_scale_rotation_translation(Vec3::new(sx, sy, depth), p.orientation, center);
                draws.push(OverlayDraw { shape: OverlayShape::Box, model, color });
            }
        }

        // The rays: bright for the active controller, faint for the other. Each is a flat strip
        // along the ray that turns about the ray to face the head, and fades toward its end.
        for (i, (hand, target)) in self.hands.iter().zip(self.targets).enumerate() {
            let Some(aim) = &hand.aim else { continue };
            let length = match &self.grab {
                Some((h, _, Grab::Move { distance, .. })) if *h == i => *distance,
                _ => target.map_or(RAY_LENGTH, |t| t.distance),
            };
            let active = i == self.active;
            let dir = aim.orientation * Vec3::NEG_Z;
            let to_head = self.head.position - aim.position;
            let Some(face) = (to_head - dir * to_head.dot(dir)).try_normalize() else { continue };
            let model = Mat4::from_cols(
                dir.cross(face).extend(0.0),
                dir.extend(0.0),
                face.extend(0.0),
                (aim.position + dir * (length / 2.0)).extend(1.0),
            );
            draws.push(OverlayDraw {
                shape: OverlayShape::RoundRect { width: 0.004, height: length, radius: 0.002, fade: 0.6 },
                model,
                color: if active { [1.0, 1.0, 1.0, 0.8] } else { [1.0, 1.0, 1.0, 0.3] },
            });
            let screen = matches!(target, Some(Hit { part: Part::Screen { .. }, .. }));
            if let (true, None, true, Some(hit)) = (active, &self.grab, screen, target) {
                // A white dot with a dark ring, where the ray meets the picture
                let Some(slot) = self.slots.iter().find(|s| s.id == hit.panel) else { continue };
                let point = aim.position + dir * hit.distance;
                for (size, color, lift) in [(0.018, [0.0, 0.0, 0.0, 0.5], 0.002), (0.012, [1.0, 1.0, 1.0, 0.95], 0.003)] {
                    draws.push(OverlayDraw {
                        shape: OverlayShape::RoundRect { width: size, height: size, radius: size / 2.0, fade: 0.0 },
                        model: Mat4::from_rotation_translation(slot.panel.orientation, point + slot.panel.normal() * lift),
                        color,
                    });
                }
            }
        }
        draws
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASPECT: f32 = 16.0 / 9.0;

    fn head() -> Pose {
        Pose { orientation: Quat::IDENTITY, position: Vec3::new(0.0, 1.6, 0.0) }
    }

    fn panel() -> Panel {
        Panel::in_front_of(&head(), ASPECT, WIDTH)
    }

    fn store(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("cranvr-panel-{name}-{}.txt", std::process::id()));
        std::fs::remove_file(&path).ok();
        path
    }

    /// A control with panel 0 in front of the head. Its rays are not filtered, so a step points
    /// exactly where it says.
    fn control(name: &str) -> PanelControl {
        let mut control = PanelControl::new(store(name));
        control.steady_rays = false;
        control.update(&Inputs { hands: [Hand::default(); 2], head: head(), stick_click: false, dt: 0.01 }, &[(0, ASPECT, 3840)]);
        control
    }

    const ORIGIN: Vec3 = Vec3::new(0.0, 1.2, -0.3);

    /// The right controller at ORIGIN pointing along `dir`
    fn step(control: &mut PanelControl, streams: &[(u8, f32, u32)], dir: Vec3, trigger: f32) -> Output {
        step_aim(control, streams, Quat::from_rotation_arc(Vec3::NEG_Z, dir.normalize()), trigger)
    }

    /// The right controller at ORIGIN turned by `orientation`, wrist roll included
    fn step_aim(control: &mut PanelControl, streams: &[(u8, f32, u32)], orientation: Quat, trigger: f32) -> Output {
        let aim = Pose { orientation, position: ORIGIN };
        let hands = [Hand::default(), Hand { aim: Some(aim), grip: None, trigger, stick_y: 0.0 }];
        control.update(&Inputs { hands, head: head(), stick_click: false, dt: 0.01 }, streams)
    }

    #[test]
    fn a_new_panel_faces_the_user() {
        let p = panel();
        assert!((p.center - Vec3::new(0.0, 1.6, -DISTANCE)).length() < 1e-5);
        assert!((p.normal() - Vec3::Z).length() < 1e-5);
        assert!((p.right() - Vec3::X).length() < 1e-5);
    }

    #[test]
    fn rays_hit_the_picture_the_handle_a_button_a_corner_or_nothing() {
        let slot = Slot { id: 0, panel: panel(), state: PanelState::default() };
        let p = slot.panel;
        let eye = head().position;
        let part = |target: Vec3| slot.hit(eye, (target - eye).normalize(), true).map(|h| h.part);
        let Some(Part::Screen { u, v }) = part(p.center) else { panic!() };
        assert!((u - 0.5).abs() < 1e-4 && (v - 0.5).abs() < 1e-4);
        let Some(Part::Screen { u, v }) = part(p.at(-0.5, 0.3)) else { panic!() };
        assert!(u < 0.5 && v < 0.5);
        assert_eq!(part(p.at(0.0, p.handle_offset())), Some(Part::Handle));
        let (button, x, y) = slot.buttons(true)[0];
        assert_eq!(part(p.at(x, y)), Some(Part::Button(button)));
        assert_eq!(part(p.at(p.width / 2.0 + 0.02, p.height / 2.0 + 0.02)), Some(Part::Corner));
        assert_eq!(slot.hit(eye, Vec3::Z, true), None);
    }

    #[test]
    fn the_places_survive_the_file() {
        let p = panel();
        let (id, mut back) = Panel::load(&p.save(3)).unwrap();
        back.height = p.height;
        assert_eq!(id, 3);
        assert!((back.center - p.center).length() < 1e-5 && back.orientation.dot(p.orientation).abs() > 0.9999);
    }

    #[test]
    fn the_handle_carries_the_panel_at_its_angle_to_the_ray() {
        let mut control = control("carry");
        let streams = [(0, ASPECT, 3840)];
        let p = control.slots[0].panel;
        // a trigger pulled off the picture does not click
        let out = step(&mut control, &streams, Vec3::Y, 1.0);
        assert_eq!((out.pointer, out.trigger), (None, 0.0));
        step(&mut control, &streams, Vec3::Y, 0.0);
        // a trigger pulled on the picture clicks, also after the ray leaves the picture
        assert_eq!(step(&mut control, &streams, p.center - ORIGIN, 1.0).trigger, 1.0);
        assert_eq!(step(&mut control, &streams, Vec3::Y, 1.0).trigger, 1.0);
        step(&mut control, &streams, Vec3::Y, 0.0);
        // pointing at the handle neither moves the mouse nor clicks; the press grabs the handle
        let to_handle = Quat::from_rotation_arc(Vec3::NEG_Z, (p.at(0.0, p.handle_offset()) - ORIGIN).normalize());
        let out = step_aim(&mut control, &streams, to_handle, 1.0);
        assert_eq!((out.pointer, out.trigger), (None, 0.0));
        let angle = |c: &PanelControl, aim: Quat| (aim * Vec3::NEG_Z).angle_between(c.slots[0].panel.normal());
        let at_grab = angle(&control, to_handle);
        // turning the controller to the left carries the panel and turns it by as much
        let left = Quat::from_rotation_y(0.6) * to_handle;
        for _ in 0..40 {
            step_aim(&mut control, &streams, left, 1.0);
        }
        let moved = control.slots[0].panel;
        assert!(moved.center.x < -0.5, "{:?}", moved.center);
        assert!((angle(&control, left) - at_grab).abs() < 0.01);
        assert!(moved.right().y.abs() < 1e-3);
        // tilting the wrist up tilts the panel up by as much
        let up = Quat::from_axis_angle(left * Vec3::X, 0.5) * left;
        for _ in 0..40 {
            step_aim(&mut control, &streams, up, 1.0);
        }
        assert!((angle(&control, up) - at_grab).abs() < 0.01);
        assert!(control.slots[0].panel.normal().y < -0.3, "{:?}", control.slots[0].panel);
        // a roll of the wrist rolls the panel; a slight roll snaps back level
        for _ in 0..40 {
            step_aim(&mut control, &streams, left * Quat::from_rotation_z(0.5), 1.0);
        }
        assert!(control.slots[0].panel.right().y.abs() > 0.4, "{:?}", control.slots[0].panel);
        for _ in 0..40 {
            step_aim(&mut control, &streams, left * Quat::from_rotation_z(0.04), 1.0);
        }
        assert!(control.slots[0].panel.right().y.abs() < 1e-3, "{:?}", control.slots[0].panel);
        // the release puts it down and keeps the place, roll included
        for _ in 0..40 {
            step_aim(&mut control, &streams, left * Quat::from_rotation_z(0.5), 1.0);
        }
        step_aim(&mut control, &streams, left * Quat::from_rotation_z(0.5), 0.0);
        let text = std::fs::read_to_string(&control.store).unwrap();
        let (_, kept) = Panel::load(text.lines().next().unwrap()).unwrap();
        let now = control.slots[0].panel;
        assert!((kept.center - now.center).length() < 1e-4 && kept.orientation.dot(now.orientation).abs() > 0.9999);
        std::fs::remove_file(&control.store).ok();
        std::fs::remove_file(control.press_log()).ok();
    }

    #[test]
    fn a_corner_resizes_the_panel_around_its_center() {
        let mut control = control("resize");
        let streams = [(0, ASPECT, 3840)];
        let p = control.slots[0].panel;
        let corner = p.at(p.width / 2.0 + 0.02, -p.height / 2.0 - 0.02);
        step(&mut control, &streams, corner - ORIGIN, 1.0);
        // pull the corner further out
        step(&mut control, &streams, p.at(p.width / 2.0 + 0.5, -p.height / 2.0 - 0.3) - ORIGIN, 1.0);
        let grown = control.slots[0].panel;
        assert!(grown.width > p.width + 0.9, "{grown:?}");
        assert!((grown.width / grown.height - ASPECT).abs() < 1e-3);
        assert!((grown.center - p.center).length() < 1e-5);
        step(&mut control, &streams, Vec3::Y, 0.0);
        std::fs::remove_file(&control.store).ok();
    }

    #[test]
    fn buttons_send_commands_and_a_new_panel_goes_to_the_right() {
        let mut control = control("buttons");
        let streams = [(0, ASPECT, 3840)];
        let slot = &control.slots[0];
        let p = slot.panel;
        let buttons = slot.buttons(true);
        let press = |control: &mut PanelControl, button: Button| {
            let &(_, x, y) = buttons.iter().find(|b| b.0 == button).unwrap();
            let out = step(control, &streams, p.at(x, y) - ORIGIN, 1.0);
            step(control, &streams, p.at(x, y) - ORIGIN, 0.0);
            out
        };
        assert_eq!(press(&mut control, Button::Pause).commands, [Command::Pause(0, true)]);
        control.set_state(0, true, 0);
        assert_eq!(press(&mut control, Button::Pause).commands, [Command::Pause(0, false)]);
        assert_eq!(press(&mut control, Button::Quality).commands, [Command::Quality(0, 1)]);
        assert_eq!(press(&mut control, Button::Add).commands, [Command::Add]);
        // the host answers with panel 1: it goes to the right of panel 0, facing the head
        step(&mut control, &[(0, ASPECT, 3840), (1, ASPECT, 2560)], Vec3::Y, 0.0);
        let added = control.slots[1].panel;
        assert!(added.center.x > p.center.x + p.width / 2.0);
        assert!((head().position - added.center).normalize().dot(added.normal()) > 0.99);
        // panel 1 has a close button; the host's answer takes the panel away
        assert!(control.slots[1].buttons(true).iter().any(|b| b.0 == Button::Close));
        step(&mut control, &streams, Vec3::Y, 0.0);
        assert_eq!(control.slots.len(), 1);
        std::fs::remove_file(&control.store).ok();
    }

    #[test]
    fn the_console_sends_requests_and_a_stick_click_at_nothing_brings_it_back() {
        let mut control = control("console");
        let streams = [(0, ASPECT, 3840)];
        let games = vec![console::Game { name: "Beat Saber".into(), program: "Beat Saber.exe".into() }];
        control.set_menu(Menu { games, fps: 120, game_scale: 0.6, msaa: 2, ..Default::default() });
        let (panel, menu) = control.console.clone().unwrap();
        let (_, items) = console::layout(&menu);
        let item = items.iter().find(|i| i.label == "90 Hz").unwrap();
        let point = panel.at(item.center.0, item.center.1);
        let mut requests = step(&mut control, &streams, point - ORIGIN, 1.0).console;
        requests.extend(step(&mut control, &streams, point - ORIGIN, 0.0).console);
        assert_eq!(requests, [console::Action::Fps(90).request()]);
        // a console carried away comes back below and ahead on a stick click at nothing
        control.console.as_mut().unwrap().0.center = Vec3::new(3.0, 0.5, 2.0);
        let aim = Pose { orientation: Quat::from_rotation_arc(Vec3::NEG_Z, Vec3::Y), position: ORIGIN };
        let hands = [Hand::default(), Hand { aim: Some(aim), grip: None, trigger: 0.0, stick_y: 0.0 }];
        control.update(&Inputs { hands, head: head(), stick_click: true, dt: 0.01 }, &streams);
        let back = control.console.as_ref().unwrap().0;
        assert!((back.center - panel.center).length() < 1e-4, "{back:?}");
        std::fs::remove_file(&control.store).ok();
    }

    #[test]
    fn a_menu_before_the_first_head_pose_places_the_console_by_the_head() {
        let mut control = PanelControl::new(store("console-first"));
        control.set_menu(Menu { fps: 120, game_scale: 0.6, ..Default::default() });
        control.update(&Inputs { hands: [Hand::default(); 2], head: head(), stick_click: false, dt: 0.01 }, &[(0, ASPECT, 3840)]);
        let (panel, _) = control.console.clone().unwrap();
        let expected = console::default_place(head().position, head().orientation, panel.height);
        assert!((panel.center - expected.center).length() < 1e-4, "{panel:?}");
        assert!(panel.center.y > 1.0, "{panel:?}");
    }

    #[test]
    fn a_press_holds_its_point_until_the_ray_turns() {
        let mut control = control("hold");
        let streams = [(0, ASPECT, 3840)];
        let p = control.slots[0].panel;
        let at = |x: f32, y: f32| p.at(x, y) - ORIGIN;
        // the ray rests on a point, then the pull of the trigger turns it by a centimeter
        for _ in 0..20 {
            step(&mut control, &streams, at(0.1, 0.1), 0.0);
        }
        let before = step(&mut control, &streams, at(0.1, 0.1), 0.0).pointer.unwrap();
        let pressed = step(&mut control, &streams, at(0.11, 0.09), 1.0);
        assert_eq!((pressed.pointer, pressed.trigger), (Some(before), 1.0));
        assert_eq!(step(&mut control, &streams, at(0.115, 0.085), 1.0).pointer, Some(before));
        // early in the press a shake of about 6 degrees still holds the point
        assert_eq!(step(&mut control, &streams, at(0.22, 0.1), 1.0).pointer, Some(before));
        assert_eq!(step(&mut control, &streams, at(0.0, 0.1), 1.0).pointer, Some(before));
        // the release clicks there, and the press goes to the press log as a click
        step(&mut control, &streams, at(0.1, 0.1), 0.0);
        let log = control.press_log();
        let line = std::fs::read_to_string(&log).unwrap();
        assert!(line.trim_end().ends_with(" 0"), "{line}");
        // later in a press the same turn drags
        for _ in 0..20 {
            step(&mut control, &streams, at(0.1, 0.1), 0.0);
        }
        step(&mut control, &streams, at(0.1, 0.1), 1.0);
        for _ in 0..40 {
            step(&mut control, &streams, at(0.1, 0.1), 1.0);
        }
        let dragged = step(&mut control, &streams, at(0.22, 0.1), 1.0).pointer.unwrap();
        assert!(dragged.1 > before.1 + 0.03, "{dragged:?} {before:?}");
        step(&mut control, &streams, at(0.22, 0.1), 0.0);
        // a large turn drags at once
        for _ in 0..20 {
            step(&mut control, &streams, at(0.1, 0.1), 0.0);
        }
        step(&mut control, &streams, at(0.1, 0.1), 1.0);
        let fast = step(&mut control, &streams, at(0.6, 0.1), 1.0).pointer.unwrap();
        assert!(fast.1 > before.1 + 0.15, "{fast:?}");
        step(&mut control, &streams, at(0.6, 0.1), 0.0);
        std::fs::remove_file(&control.store).ok();
        std::fs::remove_file(&log).ok();
    }

    #[test]
    fn the_filter_calms_a_resting_hand_and_follows_a_turn() {
        let mut filter = AimFilter::default();
        let dt = 1.0 / 120.0;
        let aim = |angle: f32| Pose { orientation: Quat::from_rotation_y(angle), position: Vec3::ZERO };
        let direction = |pose: Pose| pose.orientation * Vec3::NEG_Z;
        // a tremor of half a degree at 8 Hz
        let mut out = Vec::new();
        for i in 0..240 {
            let t = i as f32 * dt;
            out.push(direction(filter.apply(aim((0.5f32).to_radians() * (2.0 * std::f32::consts::PI * 8.0 * t).sin()), dt)));
        }
        let spread = out[120..].iter().map(|d| d.angle_between(Vec3::NEG_Z).to_degrees()).fold(0.0f32, f32::max);
        assert!(spread < 0.2, "{spread}");
        // a fast turn of 30 degrees: within 0.1 s the ray is near
        let target = (30.0f32).to_radians();
        let mut last = Vec3::NEG_Z;
        for _ in 0..12 {
            last = direction(filter.apply(aim(target), dt));
        }
        let lag = last.angle_between(direction(aim(target))).to_degrees();
        assert!(lag < 5.0, "{lag}");
    }

    #[test]
    fn the_thumbstick_on_the_handle_moves_the_panel_away() {
        let mut control = control("push");
        let streams = [(0, ASPECT, 3840)];
        let p = control.slots[0].panel;
        let aim = Pose { orientation: Quat::from_rotation_arc(Vec3::NEG_Z, (p.at(0.0, p.handle_offset()) - ORIGIN).normalize()), position: ORIGIN };
        let hands = |stick_y| [Hand::default(), Hand { aim: Some(aim), grip: None, trigger: 0.0, stick_y }];
        for _ in 0..50 {
            control.update(&Inputs { hands: hands(1.0), head: head(), stick_click: false, dt: 0.01 }, &streams);
        }
        let moved = control.slots[0].panel;
        assert!((moved.center - ORIGIN).length() > (p.center - ORIGIN).length() + 0.3, "{moved:?}");
        // the stick at rest keeps the place
        control.update(&Inputs { hands: hands(0.0), head: head(), stick_click: false, dt: 0.01 }, &streams);
        assert!(std::fs::read_to_string(&control.store).is_ok());
        std::fs::remove_file(&control.store).ok();
    }

    #[test]
    fn the_gaze_sorts_panels_and_asks_for_the_pixels_the_headset_shows() {
        let p = panel();
        let (gaze, wanted) = p.gaze(&head());
        // 2.2 m at 1.4 m covers about 76 degrees
        assert_eq!(gaze, Gaze::Center);
        assert!((3600..4000).contains(&wanted), "{wanted}");
        // the head points at the middle of panel 0
        let mut control = control("look");
        let out = step(&mut control, &[(0, ASPECT, 3840)], Vec3::Y, 0.0);
        let (id, u, v, _) = out.look.unwrap();
        assert!(id == 0 && (u - 0.5).abs() < 1e-3 && (v - 0.5).abs() < 1e-3);
        std::fs::remove_file(&control.store).ok();
        // turned 80 degrees: the panel's nearest edge lies about 42 degrees off the view direction
        let aside = Pose { orientation: Quat::from_rotation_y(80f32.to_radians()), ..head() };
        assert_eq!(p.gaze(&aside).0, Gaze::Side);
        let behind = Pose { orientation: Quat::from_rotation_y(180f32.to_radians()), ..head() };
        assert_eq!(p.gaze(&behind).0, Gaze::Out);
    }
}
