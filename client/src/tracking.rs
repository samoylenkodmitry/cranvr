//! The headset's per-frame data: tracking, input and the display clock.
//!
//! Cranpose's `SpatialFrameEffect` calls the app once for each headset frame with an `XrFrame`.
//! [`FrameReader::read`] turns that into what the host needs:
//! - the head pose and the controllers' grip and aim poses and buttons, sampled at the start of the
//!   frame;
//! - the time at which the compositor will show the frame, as a prediction;
//! - the eye views with their field of view, which the host renders with;
//! - the recenter event, when the user recentered the headset.
//!
//! The hands, body, face and eyes come from the same frames (`tracked.rs`). The reader copies only
//! the features that the host wished, into a buffer that the feed trades with the sender's.
//!
//! [`Feed`] carries them from the frame loop to the sender thread. Each frame wakes the sender, so
//! the host gets one tracking sample for each headset frame and no placeholder between them. The
//! sender sends placeholder values only when no headset exists, as in a flat window on a desktop.
//! There they keep the host's frame loop running with a still head.
//!
//! All poses are in the app's room, the LOCAL space of OpenXR. Its origin lies where the head was
//! when the app started, so the head's height above that point is about 0 and not the height above
//! the floor. The poses go to the host as they are. Cranpose reports no floor height. The
//! controllers' poses go as they are too: the grip pose and the aim pose, each with its own
//! velocities and with no offset. The host hands both to a game, which picks the one that it needs.

use crate::tracked::{TrackedFrame, TrackedReader};
use cranpose_xr::{Controller, Hand as XrHand, InputSource, InteractionProfile, Pose as XrPose, Recenter as XrRecenter, RenderViewpoint, TrackedPose, XrFrame};
use cranvr_proto::{
    clock::now_ns,
    link::ClientLink,
    message::{buttons as bit, ClientDatagram, ClientMsg, Focus, HandInput, Input, InputHold, Recenter, Tracking, Views},
    motion::velocities,
    tracking::{Body, Eyes, Face, Features, Hand, Hands, Joint, TRACKING_VERSION},
    wire::{Fov, Motion, Nanos, Pose},
    DISPLAYS_VERSION,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Condvar, Mutex,
    },
    time::{Duration, Instant},
};

/// What the headset reports for one frame. Times are on the client clock of
/// [`cranvr_proto::clock::now_ns`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeadsetFrame {
    /// When the poses were sampled. The host returns this time with the frame that it renders.
    pub sample_time: Nanos,
    /// When the compositor will show this frame, as the runtime predicts it
    pub display_time: Nanos,
    pub head: Motion,
    /// The grip poses: the places of the hands that hold the controllers
    pub left_grip: Option<Motion>,
    pub right_grip: Option<Motion>,
    /// The aim poses: where the controllers point
    pub left_aim: Option<Motion>,
    pub right_aim: Option<Motion>,
    pub input: Input,
    /// The hands that the app holds back from the game in this frame, left then right: a hand that
    /// points at a panel of the app presses the panel, not the game (see `hold.rs`)
    pub held: [bool; 2],
}

/// The display clock of the headset: when the frame that is in work reaches the display. It also
/// holds the time between two frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Display {
    pub time: Nanos,
    pub period: Nanos,
    /// The client's clock minus the runtime's clock, in ns. It turns a time of the runtime into
    /// the client's and back.
    pub offset: i64,
}

/// What one headset frame gives the sender.
#[derive(Clone, Debug, PartialEq)]
pub struct Read {
    pub display: Display,
    /// `None` while the runtime does not track the head
    pub frame: Option<HeadsetFrame>,
    /// `None` while the runtime does not track the head
    pub views: Option<Views>,
    pub recenter: Option<Recenter>,
    /// The height of the floor in the room, when the runtime gives its stage
    pub floor: Option<f32>,
    /// A tracked hand is what points at the panels on at least one side (see `hands.rs`)
    pub hand_pointer: bool,
}

/// Turns the frames of the headset into [`Read`]s. It keeps the head's last pose, from which it
/// estimates the head's velocities: Cranpose reports velocities for the controllers only.
#[derive(Default)]
pub struct FrameReader {
    last_head: Option<(Duration, XrPose)>,
    tracked: TrackedReader,
    /// The interaction profiles of the two hands in the newest frame, for the log
    profiles: Option<[Option<InteractionProfile>; 2]>,
}

/// Two head poses further apart in time than this give no velocity.
const MAX_VELOCITY_GAP: Duration = Duration::from_millis(50);
/// A trigger or a grip pulled further than this counts as a click.
const CLICK_AT: f32 = 0.5;

impl FrameReader {
    /// Reads `xr`. `now` is the client clock at the moment of the call. The runtime's times are on
    /// another clock, so only their differences from `xr.now` carry over.
    pub fn read(&mut self, xr: &XrFrame, now: Nanos) -> Read {
        let ahead = xr.predicted_display_time.saturating_sub(xr.now).as_nanos() as u64;
        let display = Display {
            time: now + ahead,
            period: xr.period.as_nanos() as u64,
            offset: now as i64 - xr.now.as_nanos() as i64,
        };
        let recenter = xr.recenter.as_ref().map(|r| recenter(r, xr, now));
        if recenter.is_some() {
            // The poses before and after the change lie in different spaces
            self.last_head = None;
        }
        self.note_profiles(xr);
        // A hand that the cameras track is no controller (see `held`)
        let (left, right) = (held(&xr.left_controller), held(&xr.right_controller));
        let frame = xr.device_pose.map(|pose| HeadsetFrame {
            sample_time: now,
            display_time: display.time,
            head: self.head(pose, xr.predicted_display_time, (xr.device_linear_velocity, xr.device_angular_velocity)),
            left_grip: left.and_then(|c| c.grip.as_ref()).map(motion),
            right_grip: right.and_then(|c| c.grip.as_ref()).map(motion),
            left_aim: left.and_then(|c| c.aim.as_ref()).map(motion),
            right_aim: right.and_then(|c| c.aim.as_ref()).map(motion),
            input: Input { sample_time: now, hands: [left.map_or_else(HandInput::default, hand_input), right.map_or_else(HandInput::default, hand_input)] },
            held: [false; 2],
        });
        if frame.is_none() {
            self.last_head = None;
        }
        let hand_pointer = [XrHand::Left, XrHand::Right].into_iter().any(|hand| xr.pointer(hand) == Some(InputSource::Hands));
        Read { display, frame, views: views(xr), recenter, floor: xr.floor_height().filter(|height| height.is_finite()), hand_pointer }
    }

    /// Hands the frame's hands, body, face and eyes to the feed: the features that the host wished,
    /// and the features that the headset offers. `read` is the result of `read` for the same frame.
    /// The call comes before `Feed::push` of that result, so the sender finds the tracked data when it
    /// wakes for the frame.
    pub fn push_tracked(&mut self, xr: &XrFrame, read: &Read, feed: &Feed) {
        feed.set_offer(self.tracked.offer(xr));
        let Some(frame) = read.frame else { return };
        let wish = feed.wish();
        if wish.is_empty() {
            return;
        }
        let scratch = self.tracked.read(xr, frame.sample_time, frame.display_time, wish);
        feed.swap_tracked(scratch);
    }

    /// Logs the interaction profile of each hand when it changes. A Quest reports the hand
    /// interaction profile for a hand that the cameras track, and the profile of the controller
    /// for a hand that holds one.
    fn note_profiles(&mut self, xr: &XrFrame) {
        let profiles = [xr.left_controller.profile, xr.right_controller.profile];
        if self.profiles != Some(profiles) {
            self.profiles = Some(profiles);
            log::info!(
                "the runtime reports the left hand as {:?} and the right hand as {:?}; {}",
                profiles[0],
                profiles[1],
                if profiles.iter().any(|p| matches!(p, Some(InteractionProfile::ExtHand | InteractionProfile::MicrosoftHand))) {
                    "a hand that the cameras track is sent to the host as a hand, never as a controller"
                } else {
                    "a controller is sent to the host as a controller"
                }
            );
        }
    }

    /// The head's motion. The runtime's own velocities count. A runtime that gives none leaves the
    /// estimate from the last two poses.
    fn head(&mut self, pose: XrPose, time: Duration, runtime: (Option<[f32; 3]>, Option<[f32; 3]>)) -> Motion {
        let (estimated_linear, estimated_angular) = match self.last_head.replace((time, pose)) {
            Some((before, last)) if time > before && time - before <= MAX_VELOCITY_GAP => {
                velocities(&proto_pose(&last), &proto_pose(&pose), (time - before).as_secs_f32())
            }
            _ => ([0.0; 3], [0.0; 3]),
        };
        let finite = |v: [f32; 3]| v.iter().all(|c| c.is_finite());
        Motion {
            pose: proto_pose(&pose),
            linear_velocity: runtime.0.filter(|v| finite(*v)).unwrap_or(estimated_linear),
            angular_velocity: runtime.1.filter(|v| finite(*v)).unwrap_or(estimated_angular),
        }
    }
}

/// The controller of a hand, or `None` while a hand that the cameras track stands in its place. The
/// runtime reports such a hand through the hand interaction profile, with an aim pose and a pinch
/// as the trigger, and a Quest keeps that aim pose valid when no hand is in view. The host must not
/// take it for a controller. It decides by itself when a hand makes a controller, from the hand's
/// joints (`host/src/tracking.rs`): it asks for hands only while a controller is missing. A hand
/// that counted as a controller would hide the missing one from the host. The game would then
/// get a pose and a trigger when no hand is in view. The user's hands reach a game with hand code
/// through the hand tracking that the game asked for (`tracked.rs`).
fn held(controller: &Controller) -> Option<&Controller> {
    (!matches!(controller.profile, Some(InteractionProfile::ExtHand | InteractionProfile::MicrosoftHand))).then_some(controller)
}

fn proto_pose(pose: &XrPose) -> Pose {
    Pose { orientation: pose.orientation, position: pose.position }
}

fn motion(tracked: &TrackedPose) -> Motion {
    Motion {
        pose: proto_pose(&tracked.pose),
        linear_velocity: tracked.linear_velocity.unwrap_or_default(),
        angular_velocity: tracked.angular_velocity.unwrap_or_default(),
    }
}

fn hand_input(c: &Controller) -> HandInput {
    let set = |pairs: &[(bool, u32)]| pairs.iter().filter(|(on, _)| *on).fold(0, |bits, (_, bit)| bits | bit);
    HandInput {
        buttons: set(&[
            (c.primary.pressed, bit::PRIMARY),
            (c.secondary.pressed, bit::SECONDARY),
            (c.menu, bit::MENU),
            (c.thumbstick.pressed, bit::STICK_CLICK),
            (c.trigger.value > CLICK_AT, bit::TRIGGER_CLICK),
            (c.squeeze > CLICK_AT, bit::GRIP_CLICK),
        ]),
        touches: set(&[
            (c.primary.touched, bit::PRIMARY),
            (c.secondary.touched, bit::SECONDARY),
            (c.thumbstick.touched, bit::STICK_CLICK),
            (c.trigger.touched, bit::TRIGGER_CLICK),
            (c.thumbrest_touched, bit::THUMB_REST),
        ]),
        trigger: c.trigger.value,
        grip: c.squeeze,
        stick: [c.thumbstick.x, c.thumbstick.y],
    }
}

fn views(xr: &XrFrame) -> Option<Views> {
    let (left, right) = (xr.left_eye?, xr.right_eye?);
    let fov = |eye: &RenderViewpoint| {
        let f = eye.field_of_view;
        Fov { left: f.angle_left, right: f.angle_right, up: f.angle_up, down: f.angle_down }
    };
    Some(Views { fov: [fov(&left), fov(&right)], eye_pose: [proto_pose(&left.local_pose), proto_pose(&right.local_pose)] })
}

fn recenter(change: &XrRecenter, xr: &XrFrame, now: Nanos) -> Recenter {
    // The change may lie before or after the frame's own clock reading
    let offset = change.change_time.as_nanos() as i128 - xr.now.as_nanos() as i128;
    Recenter {
        change_time: (now as i128 + offset).max(1) as Nanos,
        origin: change.pose_in_previous_space.as_ref().map(proto_pose),
    }
}

/// What the sender takes from the feed.
#[derive(Default)]
pub struct Wake {
    pub frame: Option<HeadsetFrame>,
    pub recenters: Vec<Recenter>,
    /// The newest views of the headset
    pub views: Option<Views>,
    /// The floor's height in the room, once the headset gave it
    pub floor: Option<f32>,
}

/// The newest frame from the headset, the display clock and the views that follow from it.
#[derive(Default)]
pub struct Feed {
    inner: Mutex<FeedInner>,
    wake: Condvar,
    /// The tracking features that the host wishes, as bits of `Features`. The link's reader sets it.
    wish: AtomicU8,
    /// The tracking features that the headset offers now, from its newest frame
    offer: AtomicU8,
}

#[derive(Default)]
struct FeedInner {
    pending: Option<HeadsetFrame>,
    recenters: Vec<Recenter>,
    views: Option<Views>,
    display: Option<Display>,
    /// The views that the host renders with, from the Hello or the last change
    host_views: Option<Views>,
    /// The last time a tracked frame came from the headset
    last_push: Option<Instant>,
    /// The frames and the longest gap between two frames since the last call of `frame_rate`
    rate: RateCount,
    /// The head of the newest tracked frame
    head: Option<Motion>,
    /// The floor's height in the room, from the newest frame that had it
    floor: Option<f32>,
    /// The hands, body, face and eyes of the newest frame, until the sender takes them
    tracked: Option<Box<TrackedFrame>>,
    tracked_fresh: bool,
    /// Set by `Feed::wake`, so the sender's wait ends at once and looks at the app's focus
    poked: bool,
    /// The last frame in which a tracked hand pointed at the panels
    hand_pointer_at: Option<Instant>,
}

#[derive(Default)]
struct RateCount {
    frames: u32,
    longest_gap: Duration,
    since: Option<Instant>,
}

/// A frame that came from the headset counts as live for this long.
const LIVE_FOR: Duration = Duration::from_secs(1);
/// A hand that pointed at the panels counts as pointing for this long after its last frame.
const HAND_HOLD: Duration = Duration::from_millis(1500);

impl Feed {
    /// Hands one frame of the headset to the sender. A frame that the sender has not taken yet is
    /// replaced. A recenter is never replaced.
    pub fn push(&self, read: Read) {
        let mut inner = self.inner.lock().unwrap();
        inner.display = Some(read.display);
        if let Some(views) = read.views {
            inner.views = Some(views);
        }
        inner.recenters.extend(read.recenter);
        if read.floor.is_some() {
            inner.floor = read.floor;
        }
        if read.hand_pointer {
            inner.hand_pointer_at = Some(Instant::now());
        }
        if read.frame.is_some() {
            inner.pending = read.frame;
            inner.head = read.frame.map(|frame| frame.head);
            let now = Instant::now();
            if let Some(last) = inner.last_push {
                inner.rate.longest_gap = inner.rate.longest_gap.max(now - last);
            }
            inner.rate.frames += 1;
            inner.rate.since.get_or_insert(now);
            inner.last_push = Some(now);
        }
        drop(inner);
        self.wake.notify_all();
    }

    /// The tracking features that the host wishes now.
    pub fn wish(&self) -> Features {
        Features(self.wish.load(Ordering::Relaxed)).known()
    }

    /// The host's wish changed. The link's reader calls it, and a new session starts with none.
    pub fn set_wish(&self, wish: Features) {
        self.wish.store(wish.known().0, Ordering::Relaxed);
    }

    /// The tracking features that the headset offers, from its newest frame.
    pub fn offer(&self) -> Features {
        Features(self.offer.load(Ordering::Relaxed))
    }

    pub fn set_offer(&self, offer: Features) {
        self.offer.store(offer.0, Ordering::Relaxed);
    }

    /// Trades the buffer of the frame that the reader filled for the one that the sender left. The
    /// sender takes the data with [`Feed::take_tracked`]. The call allocates only the first time.
    pub fn swap_tracked(&self, frame: &mut Box<TrackedFrame>) {
        let mut inner = self.inner.lock().unwrap();
        match &mut inner.tracked {
            Some(slot) => std::mem::swap(slot, frame),
            none => *none = Some(std::mem::replace(frame, Box::new(TrackedFrame::new()))),
        }
        inner.tracked_fresh = true;
    }

    /// Takes the newest hands, body, face and eyes, if a frame brought them since the last call. The
    /// buffer `out` goes back to the feed in trade.
    pub fn take_tracked(&self, out: &mut Box<TrackedFrame>) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let fresh = std::mem::take(&mut inner.tracked_fresh);
        match &mut inner.tracked {
            Some(slot) if fresh => {
                std::mem::swap(slot, out);
                true
            }
            _ => false,
        }
    }

    /// Wakes the sender, so it looks at the app's focus now and not at its next turn.
    pub fn wake(&self) {
        self.inner.lock().unwrap().poked = true;
        self.wake.notify_all();
    }

    /// Whether a tracked hand pointed at the panels lately. A hand that the cameras lose for a
    /// moment keeps the answer for `HAND_HOLD`, so the panels do not flicker.
    pub fn hand_points(&self) -> bool {
        self.inner.lock().unwrap().hand_pointer_at.is_some_and(|at| at.elapsed() < HAND_HOLD)
    }

    /// Waits up to `timeout` for a frame or a recenter.
    pub fn wait(&self, timeout: Duration) -> Wake {
        let guard = self.inner.lock().unwrap();
        let (mut inner, _) =
            self.wake.wait_timeout_while(guard, timeout, |i| i.pending.is_none() && i.recenters.is_empty() && !i.poked).unwrap();
        inner.poked = false;
        Wake { frame: inner.pending.take(), recenters: std::mem::take(&mut inner.recenters), views: inner.views, floor: inner.floor }
    }

    /// Waits up to `timeout` for the first views of the headset. The headset reports them once it
    /// tracks the head, which is a moment after the app starts.
    pub fn wait_views(&self, timeout: Duration) -> Option<Views> {
        let guard = self.inner.lock().unwrap();
        let (inner, _) = self.wake.wait_timeout_while(guard, timeout, |i| i.views.is_none()).unwrap();
        inner.views
    }

    /// The display clock of the frame that the headset has in work, when a frame came
    pub fn display(&self) -> Option<Display> {
        self.inner.lock().unwrap().display
    }

    /// A time on the client's clock as a time of the headset runtime, the clock of `XrFrame`.
    /// `None` before the first frame.
    pub fn to_runtime_time(&self, client_time: Nanos) -> Option<Duration> {
        let offset = self.display()?.offset;
        Some(Duration::from_nanos(u64::try_from(client_time as i128 - offset as i128).ok()?))
    }

    /// Notes the views that the host renders with. The frames that come back carry the render
    /// poses of these views.
    pub fn set_host_views(&self, views: Views) {
        self.inner.lock().unwrap().host_views = Some(views);
    }

    pub fn host_views(&self) -> Option<Views> {
        self.inner.lock().unwrap().host_views
    }

    /// The headset's frames a second and the longest gap between two of them since the last call.
    /// `None` when no frame came.
    pub fn frame_rate(&self) -> Option<(f32, Duration)> {
        let mut inner = self.inner.lock().unwrap();
        let rate = std::mem::take(&mut inner.rate);
        let seconds = rate.since?.elapsed().as_secs_f32();
        (seconds > 0.0).then(|| (rate.frames as f32 / seconds, rate.longest_gap))
    }

    /// The head of the newest tracked frame
    pub fn head(&self) -> Option<Motion> {
        self.inner.lock().unwrap().head
    }

    /// Whether a tracked frame came from the headset lately.
    pub fn is_live(&self) -> bool {
        self.inner.lock().unwrap().last_push.is_some_and(|at| at.elapsed() < LIVE_FOR)
    }

    /// The first display time at or after `t` on the grid of the headset's display, for a stream
    /// that runs at `period` nanoseconds a frame. The grid starts at the newest display time of
    /// the headset, or at the first call when no frame came.
    pub fn display_time_after(&self, t: Nanos, period: Nanos) -> Nanos {
        let period = period.max(1);
        let origin = self.inner.lock().unwrap().display.map_or_else(now_ns, |d| d.time);
        if t <= origin {
            origin
        } else {
            origin + (t - origin).div_ceil(period) * period
        }
    }
}

/// The eye views of the Quest 3, roughly. A flat window has no headset to ask. The Hello of a
/// headset that reports no views in time uses them too.
pub fn placeholder_views() -> Views {
    const EYE_X: f32 = 0.032;
    let fov = Fov { left: -0.9, right: 0.9, up: 0.9, down: -0.9 };
    let eye = |x: f32| Pose { position: [x, 0.0, 0.0], ..Pose::IDENTITY };
    Views { fov: [fov, fov], eye_pose: [eye(-EYE_X), eye(EYE_X)] }
}

/// Whether two sets of views differ by more than the runtime's rounding. The host hears of a
/// change of the distance between the eyes or of the field of view.
fn views_differ(a: &Views, b: &Views) -> bool {
    const FOV_EPSILON: f32 = 1e-3;
    const EYE_EPSILON: f32 = 5e-4;
    let fov = |a: &Fov, b: &Fov| {
        [(a.left, b.left), (a.right, b.right), (a.up, b.up), (a.down, b.down)].iter().any(|(x, y)| (x - y).abs() > FOV_EPSILON)
    };
    let eye = |a: &Pose, b: &Pose| {
        (0..3).any(|i| (a.position[i] - b.position[i]).abs() > EYE_EPSILON)
            || (0..4).any(|i| (a.orientation[i] - b.orientation[i]).abs() > FOV_EPSILON)
    };
    (0..2).any(|i| fov(&a.fov[i], &b.fov[i]) || eye(&a.eye_pose[i], &b.eye_pose[i]))
}

/// A frame of a still head with both controllers held in front of it, for a window that has no
/// headset. The head stands at the height of a person.
fn placeholder_frame(at: Nanos, display_time: Nanos) -> HeadsetFrame {
    let hand = |x: f32| Motion { pose: Pose { position: [x, 1.2, -0.4], ..Pose::IDENTITY }, ..Motion::default() };
    HeadsetFrame {
        sample_time: at,
        display_time,
        head: Motion { pose: Pose { position: [0.0, 1.6, 0.0], ..Pose::IDENTITY }, ..Motion::default() },
        left_grip: Some(hand(-0.2)),
        right_grip: Some(hand(0.2)),
        // A window without a headset has no pointing ray. The host then gives a game the grip pose for
        // both, and its own scene tilts the ray down from the grip pose. A level aim pose would make
        // the host's scene point the Mac's cursor at the screen straight ahead.
        left_aim: None,
        right_aim: None,
        input: Input::default(),
        held: [false; 2],
    }
}

/// The datagrams of hands, body and face, kept from one frame to the next. A sample refills them, so
/// their joint blocks keep their capacity and a frame allocates nothing.
#[derive(Default)]
struct Outgoing {
    hands: Hands,
    /// The hands' joint blocks while a hand is not tracked
    spare: [Hand; 2],
    /// A tracked hand went out, so the host must hear when the last one is gone
    hands_sent: bool,
    body: Body,
    face: Face,
}

/// Sends a datagram that `value` fills and takes the value back, with its allocations. `false` when
/// the link failed.
fn send_value<T: Default>(link: &ClientLink, value: &mut T, wrap: fn(T) -> ClientDatagram, unwrap: fn(ClientDatagram) -> Option<T>) -> bool {
    let datagram = wrap(std::mem::take(value));
    let sent = link.duplex.send_datagram(&datagram).is_ok();
    if let Some(taken) = unwrap(datagram) {
        *value = taken;
    }
    sent
}

/// Sends one datagram for each wished feature that the frame has. A hand that the cameras lost goes
/// once, as an absent hand, and then nothing. A body, a face or a gaze that is not there sends
/// nothing: the game's runtime takes data that stops for an inactive tracker. `false` when the link
/// failed.
fn send_tracked(link: &ClientLink, frame: &TrackedFrame, wish: Features, out: &mut Outgoing) -> bool {
    let mut ok = true;
    if wish.contains(Features::HANDS) {
        let any = frame.hands.iter().any(|hand| hand.tracked);
        if any || out.hands_sent {
            out.hands_sent = any;
            out.hands.sample_time = frame.sample_time;
            out.hands.display_time = frame.display_time;
            let load = |slot: &mut Option<Hand>, spare: &mut Hand, sample: &crate::tracked::HandSample| {
                *slot = sample.tracked.then(|| {
                    let mut hand = std::mem::take(spare);
                    hand.set(sample.root, &sample.joints);
                    hand
                });
            };
            let [spare_left, spare_right] = &mut out.spare;
            load(&mut out.hands.left, spare_left, &frame.hands[0]);
            load(&mut out.hands.right, spare_right, &frame.hands[1]);
            ok &= send_value(link, &mut out.hands, ClientDatagram::Hands, |d| match d {
                ClientDatagram::Hands(hands) => Some(hands),
                _ => None,
            });
            let [spare_left, spare_right] = &mut out.spare;
            for (slot, spare) in [(&mut out.hands.left, spare_left), (&mut out.hands.right, spare_right)] {
                if let Some(hand) = slot.take() {
                    *spare = hand;
                }
            }
        }
    }
    let body = &frame.body;
    if (wish.contains(Features::BODY) || wish.contains(Features::FULL_BODY)) && body.active {
        let joints: &[Joint] = &body.joints[..body.set.joints()];
        if out.body.set(frame.sample_time, frame.display_time, body.set, body.confidence, body.root, joints).is_ok() {
            ok &= send_value(link, &mut out.body, ClientDatagram::Body, |d| match d {
                ClientDatagram::Body(body) => Some(body),
                _ => None,
            });
        }
    }
    if wish.contains(Features::FACE) && frame.face.valid {
        out.face.set(frame.sample_time, frame.display_time, frame.face.flags, frame.face.confidence, &frame.face.weights);
        ok &= send_value(link, &mut out.face, ClientDatagram::Face, |d| match d {
            ClientDatagram::Face(face) => Some(face),
            _ => None,
        });
    }
    if let (true, Some(gaze)) = (wish.contains(Features::EYES), frame.gaze) {
        let eyes = Eyes { sample_time: frame.sample_time, display_time: frame.display_time, gaze: Some(gaze) };
        ok &= link.duplex.send_datagram(&ClientDatagram::Eyes(eyes)).is_ok();
    }
    ok
}

/// The names of a set of features for the log.
fn describe(features: Features) -> String {
    if features.is_empty() {
        "no tracking".to_string()
    } else {
        features.names().join(", ")
    }
}

/// The floor's height changes by more than this before the client tells the host again, in meters
const FLOOR_EPSILON: f32 = 0.01;
/// How often the sender looks at the stop flag while it waits for a frame
const STOP_CHECK: Duration = Duration::from_millis(20);

/// Sends the device's tracking to the host until `stop` is set. On a device that tracks (a headset
/// or a phone) each frame of the feed goes out as it comes. Without one the sender makes placeholder
/// frames once per refresh. `hello_views` are the views that the host got in the Hello.
///
/// A device that tracks follows Meta's focus rules (see `focus.rs`). While the app has no input
/// focus the sender tells the host so, and sends no tracking and no input. The connection and its
/// pings go on. The host pauses the stream, lets go of the buttons and shows the game no controller. A
/// window sends placeholder frames at all times. `focus_now` tells the app's focus: the lifecycle
/// of the app in the real sender (`focus::current`).
pub fn send_loop(link: &ClientLink, feed: &Feed, tracked: bool, refresh_hz: f32, hello_views: Views, focus_now: &dyn Fn() -> Focus, stop: &AtomicBool) {
    let period = Duration::from_secs_f64(1.0 / f64::from(refresh_hz.max(1.0)));
    let mut deadline = Instant::now();
    let mut sent_views = hello_views;
    let mut sent_floor: Option<f32> = None;
    // The features that the host heard that the headset offers. A host of an older protocol version
    // knows no such message.
    let mut sent_offer: Option<Features> = None;
    let sends_tracking = link.welcome.version >= TRACKING_VERSION;
    let mut outgoing = Outgoing::default();
    let mut buffer = Box::new(TrackedFrame::new());
    // A host takes a client that never says anything as focused
    let mut sent_focus = Focus::Focused;
    while !stop.load(Ordering::Relaxed) {
        let wake = feed.wait(if tracked { STOP_CHECK } else { period.min(STOP_CHECK) });
        let focus = if tracked { focus_now() } else { Focus::Focused };
        if focus != sent_focus {
            if focus.is_focused() {
                log::info!("the app is focused again: tracking and input go on and the host resumes the stream");
            } else {
                log::info!(
                    "the app lost the focus ({}): tracking and input stop, the host pauses the stream and the link stays up",
                    crate::focus::describe(focus)
                );
            }
            link.duplex.send_control(&ClientMsg::Focus(focus)).ok();
            sent_focus = focus;
        }
        for recenter in wake.recenters {
            log::info!("the headset recentered; the new origin takes effect at {} ns", recenter.change_time);
            link.duplex.send_control(&ClientMsg::Recenter(recenter)).ok();
        }
        if let Some(views) = wake.views.filter(|views| views_differ(views, &sent_views)) {
            log::info!("the headset's views changed: {views:?}");
            sent_views = views;
            feed.set_host_views(views);
            link.duplex.send_control(&ClientMsg::Views(views)).ok();
        }
        // The floor goes to the host when it is first known and when it moves by a centimeter
        if let Some(floor) = wake.floor.filter(|floor| sent_floor.is_none_or(|sent| (sent - floor).abs() > FLOOR_EPSILON)) {
            log::info!("the headset's floor lies {floor:.2} m from the room's origin");
            sent_floor = Some(floor);
            link.duplex.send_control(&ClientMsg::Floor(floor)).ok();
        }
        // No tracking and no input without the focus. The frame that the wait took is not sent.
        if !focus.is_focused() {
            continue;
        }
        let from_headset = wake.frame.is_some();
        let frame = match wake.frame {
            Some(frame) => frame,
            None if !tracked && Instant::now() >= deadline => {
                // The wait may end early, so the loop reads the clock. A thread that fell far
                // behind starts again from now and sends no burst of samples.
                deadline += period;
                if deadline + period * 4 < Instant::now() {
                    deadline = Instant::now();
                }
                let now = now_ns();
                placeholder_frame(now, feed.display_time_after(now, period.as_nanos() as u64))
            }
            None => continue,
        };
        // The offer follows the permissions that the user gives, so it goes with a frame of the headset
        // when it changes. A window without a headset offers nothing.
        if from_headset {
            let offer = feed.offer();
            if sent_offer != Some(offer) {
                log::info!("the headset offers {}", describe(offer));
                if sends_tracking && !(sent_offer.is_none() && offer.is_empty()) {
                    link.duplex.send_control(&ClientMsg::TrackingOffer(offer)).ok();
                }
                sent_offer = Some(offer);
            }
        }
        let tracking = Tracking {
            sample_time: frame.sample_time,
            display_time: frame.display_time,
            head: frame.head,
            left_grip: frame.left_grip,
            right_grip: frame.right_grip,
            left_aim: frame.left_aim,
            right_aim: frame.right_aim,
        };
        if link.duplex.send_datagram(&ClientDatagram::Tracking(tracking)).is_err() {
            return;
        }
        // The input goes out with every sample. A datagram may get lost, and the host takes the
        // changes from the values that it sees, so a lost one leaves a button wrong for one frame.
        // A host of version 8 gets the hands that the app holds back from the game in the same datagram, so
        // no press of such a hand reaches the game before the hold does
        let datagram = if link.welcome.version >= DISPLAYS_VERSION {
            ClientDatagram::InputHold(InputHold { input: frame.input, held: frame.held })
        } else {
            ClientDatagram::Input(frame.input)
        };
        link.duplex.send_datagram(&datagram).ok();
        // The hands, the body, the face and the eyes that the host wished, of the same sample
        let wish = feed.wish();
        if from_headset && !wish.is_empty() && feed.take_tracked(&mut buffer) && buffer.sample_time == frame.sample_time && !send_tracked(link, &buffer, wish, &mut outgoing) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranpose_xr::{Button, FieldOfView, Thumbstick, Trigger};
    /// The wrist's place among the hand's joints
    const HAND_WRIST: usize = cranpose_xr::HandJointType::Wrist.index();
    use cranvr_proto::{
        identity::Identity,
        link::{HostListener, Incoming, MediaMode},
        message::{negotiate, ClientCaps, HostWishes},
        wire::Codec,
    };
    use std::{sync::mpsc, thread};

    fn pose_at(x: f32, y: f32, z: f32) -> XrPose {
        XrPose::at([x, y, z])
    }

    fn eye(x: f32) -> RenderViewpoint {
        RenderViewpoint {
            pose: pose_at(x, 1.6, 0.0),
            local_pose: pose_at(x, 0.0, 0.0),
            field_of_view: FieldOfView { angle_left: -0.8, angle_right: 0.7, angle_up: 0.75, angle_down: -0.9 },
        }
    }

    fn frame() -> XrFrame {
        let mut xr = XrFrame::default();
        xr.now = Duration::from_secs(100);
        xr.predicted_display_time = Duration::from_secs(100) + Duration::from_millis(17);
        xr.period = Duration::from_micros(8333);
        xr.device_pose = Some(pose_at(0.0, 1.6, 0.0));
        xr.left_eye = Some(eye(-0.032));
        xr.right_eye = Some(eye(0.032));
        xr
    }

    #[test]
    fn the_display_time_is_the_clock_reading_plus_the_runtimes_lead() {
        let read = FrameReader::default().read(&frame(), 5_000_000_000);
        // The runtime's clock reads 100 s while the client's reads 5 s
        assert_eq!(read.display, Display { time: 5_017_000_000, period: 8_333_000, offset: -95_000_000_000 });
        let sent = read.frame.unwrap();
        assert_eq!((sent.sample_time, sent.display_time), (5_000_000_000, 5_017_000_000));
    }

    #[test]
    fn a_time_of_the_client_becomes_a_time_of_the_runtime() {
        let feed = Feed::default();
        assert_eq!(feed.to_runtime_time(5_000_000_000), None);
        feed.push(FrameReader::default().read(&frame(), 5_000_000_000));
        // The frame's clock reading was 5 s on the client and 100 s on the runtime
        assert_eq!(feed.to_runtime_time(5_017_000_000), Some(Duration::from_millis(100_017)));
        assert_eq!(feed.to_runtime_time(4_990_000_000), Some(Duration::from_millis(99_990)));
    }

    #[test]
    fn the_views_carry_the_angles_and_the_eyes_place_in_the_head() {
        let views = FrameReader::default().read(&frame(), 1).views.unwrap();
        assert_eq!(views.fov[0], Fov { left: -0.8, right: 0.7, up: 0.75, down: -0.9 });
        assert_eq!(views.eye_pose[0].position, [-0.032, 0.0, 0.0]);
        assert_eq!(views.eye_pose[1].position, [0.032, 0.0, 0.0]);
    }

    #[test]
    fn a_head_that_is_not_tracked_sends_no_frame() {
        let mut xr = frame();
        xr.device_pose = None;
        xr.left_eye = None;
        let read = FrameReader::default().read(&xr, 1);
        assert!(read.frame.is_none() && read.views.is_none());
        assert_eq!(read.display.period, 8_333_000);
    }

    #[test]
    fn the_head_velocity_follows_two_frames() {
        let mut reader = FrameReader::default();
        let mut xr = frame();
        reader.read(&xr, 1);
        xr.predicted_display_time += Duration::from_millis(10);
        xr.device_pose = Some(pose_at(0.01, 1.6, -0.02));
        let head = reader.read(&xr, 2).frame.unwrap().head;
        assert!((head.linear_velocity[0] - 1.0).abs() < 1e-3 && (head.linear_velocity[2] + 2.0).abs() < 1e-3);
        assert_eq!(head.angular_velocity, [0.0; 3]);
    }

    #[test]
    fn the_runtimes_velocities_of_the_head_count_and_the_estimate_fills_the_gaps() {
        let mut reader = FrameReader::default();
        let mut xr = frame();
        xr.device_linear_velocity = Some([0.5, 0.0, -0.25]);
        xr.device_angular_velocity = Some([0.0, 1.5, 0.0]);
        let head = reader.read(&xr, 1).frame.unwrap().head;
        assert_eq!((head.linear_velocity, head.angular_velocity), ([0.5, 0.0, -0.25], [0.0, 1.5, 0.0]));
        // A runtime that gives only the linear velocity leaves the angular one to the estimate
        xr.predicted_display_time += Duration::from_millis(10);
        xr.device_pose = Some(pose_at(0.01, 1.6, 0.0));
        xr.device_linear_velocity = Some([2.0, 0.0, 0.0]);
        xr.device_angular_velocity = None;
        let head = reader.read(&xr, 2).frame.unwrap().head;
        assert_eq!(head.linear_velocity, [2.0, 0.0, 0.0]);
        assert_eq!(head.angular_velocity, [0.0; 3]);
        // A velocity that is not a number does not reach the host
        xr.device_linear_velocity = Some([f32::NAN, 0.0, 0.0]);
        assert!(reader.read(&xr, 3).frame.unwrap().head.linear_velocity.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn the_floor_comes_from_the_stage() {
        let mut xr = frame();
        assert_eq!(FrameReader::default().read(&xr, 1).floor, None);
        xr.stage_pose = Some(pose_at(0.3, -1.62, 0.1));
        assert_eq!(FrameReader::default().read(&xr, 1).floor, Some(-1.62));
    }

    #[test]
    fn a_gap_between_frames_gives_no_velocity() {
        let mut reader = FrameReader::default();
        let mut xr = frame();
        reader.read(&xr, 1);
        xr.predicted_display_time += Duration::from_millis(200);
        xr.device_pose = Some(pose_at(1.0, 1.6, 0.0));
        assert_eq!(reader.read(&xr, 2).frame.unwrap().head.linear_velocity, [0.0; 3]);
    }

    #[test]
    fn a_controller_gives_its_grip_pose_and_buttons() {
        let mut xr = frame();
        xr.right_controller = Controller {
            grip: Some(TrackedPose { pose: pose_at(0.2, 1.1, -0.3), linear_velocity: Some([1.0, 0.0, 0.0]), angular_velocity: None }),
            aim: Some(TrackedPose { pose: XrPose { orientation: [0.0, 0.382_683_43, 0.0, 0.923_879_5], position: [0.2, 1.05, -0.4] }, linear_velocity: Some([1.0, 0.1, 0.0]), angular_velocity: Some([0.0, 0.5, 0.0]) }),
            trigger: Trigger { value: 0.75, touched: true },
            squeeze: 0.25,
            thumbstick: Thumbstick { x: 0.5, y: -1.0, pressed: true, touched: true },
            primary: Button { pressed: true, touched: true },
            secondary: Button { pressed: false, touched: true },
            menu: false,
            thumbrest_touched: true,
            ..Controller::default()
        };
        let read = FrameReader::default().read(&xr, 1).frame.unwrap();
        assert!(read.left_grip.is_none() && read.left_aim.is_none());
        let grip = read.right_grip.unwrap();
        assert_eq!((grip.pose.position, grip.linear_velocity, grip.angular_velocity), ([0.2, 1.1, -0.3], [1.0, 0.0, 0.0], [0.0; 3]));
        // The aim pose has its own place and velocities: the client adds no offset to either pose
        let aim = read.right_aim.unwrap();
        assert_eq!((aim.pose.position, aim.linear_velocity, aim.angular_velocity), ([0.2, 1.05, -0.4], [1.0, 0.1, 0.0], [0.0, 0.5, 0.0]));
        assert_eq!((grip.pose.orientation, aim.pose.orientation), ([0.0, 0.0, 0.0, 1.0], [0.0, 0.382_683_43, 0.0, 0.923_879_5]));
        let input = read.input.hands[1];
        assert_eq!(input.buttons, bit::PRIMARY | bit::STICK_CLICK | bit::TRIGGER_CLICK);
        assert_eq!(input.touches, bit::PRIMARY | bit::SECONDARY | bit::STICK_CLICK | bit::TRIGGER_CLICK | bit::THUMB_REST);
        assert_eq!((input.trigger, input.grip, input.stick), (0.75, 0.25, [0.5, -1.0]));
        assert_eq!(read.input.hands[0], HandInput::default());
    }

    #[test]
    fn an_app_without_the_focus_sends_no_tracking_and_no_input_and_tells_the_host_once() {
        use std::sync::atomic::AtomicU8;
        let (link, inbox) = loopback();
        let link = std::sync::Arc::new(link);
        let feed = std::sync::Arc::new(Feed::default());
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let focus = std::sync::Arc::new(AtomicU8::new(0));
        let sender = {
            let (link, feed, stop, focus) = (link.clone(), feed.clone(), stop.clone(), focus.clone());
            let hello = FrameReader::default().read(&frame(), 1).views.unwrap();
            let now = move || [Focus::Focused, Focus::Visible, Focus::Hidden][usize::from(focus.load(Ordering::Relaxed))];
            thread::spawn(move || send_loop(&link, &feed, true, 120.0, hello, &now, &stop))
        };
        let next = |what: &str| inbox.recv_timeout(Duration::from_secs(10)).unwrap_or_else(|_| panic!("no {what}"));
        let mut reader = FrameReader::default();
        let mut xr = frame();
        xr.right_controller.primary = Button { pressed: true, touched: true };
        feed.push(reader.read(&xr, 100));
        assert!(matches!(next("tracking"), Heard::Datagram(ClientDatagram::Tracking(t)) if t.sample_time == 100));
        assert!(matches!(next("input"), Heard::Datagram(ClientDatagram::InputHold(_))));

        // The system's menu takes the focus: the host hears it once, and the frames after it go nowhere
        focus.store(1, Ordering::Relaxed);
        feed.wake();
        loop {
            if let Heard::Control(ClientMsg::Focus(state)) = next("the focus") {
                assert_eq!(state, Focus::Visible);
                break;
            }
        }
        for sample in 200..210 {
            feed.push(reader.read(&xr, sample));
            thread::sleep(Duration::from_millis(5));
        }
        // The headset hides the app: one more message, and still nothing else
        focus.store(2, Ordering::Relaxed);
        feed.wake();
        let mut messages = Vec::new();
        let quiet_until = Instant::now() + Duration::from_millis(300);
        while Instant::now() < quiet_until {
            feed.push(reader.read(&xr, 300));
            if let Ok(heard) = inbox.recv_timeout(Duration::from_millis(20)) {
                messages.push(heard);
            }
        }
        assert!(
            messages.iter().all(|heard| matches!(heard, Heard::Control(ClientMsg::Focus(Focus::Hidden)) | Heard::Datagram(ClientDatagram::Ping(_)))),
            "only the focus and pings go out while the app is not focused"
        );
        assert_eq!(messages.iter().filter(|heard| matches!(heard, Heard::Control(ClientMsg::Focus(_)))).count(), 1);

        // The focus returns: the host hears it, and the next frame goes out
        focus.store(0, Ordering::Relaxed);
        feed.wake();
        let mut saw = (false, false);
        while saw != (true, true) {
            feed.push(reader.read(&xr, 400));
            match inbox.recv_timeout(Duration::from_millis(50)) {
                Ok(Heard::Control(ClientMsg::Focus(state))) => {
                    assert_eq!(state, Focus::Focused);
                    saw.0 = true;
                }
                Ok(Heard::Datagram(ClientDatagram::Tracking(t))) if t.sample_time == 400 => saw.1 = true,
                _ => {}
            }
        }
        stop.store(true, Ordering::Relaxed);
        sender.join().unwrap();
    }

    #[test]
    fn a_wake_ends_the_senders_wait_at_once() {
        let feed = std::sync::Arc::new(Feed::default());
        let waker = {
            let feed = feed.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(50));
                feed.wake();
            })
        };
        let started = Instant::now();
        let wake = feed.wait(Duration::from_secs(10));
        assert!(started.elapsed() < Duration::from_secs(5), "the wait ran to its timeout");
        assert!(wake.frame.is_none() && wake.recenters.is_empty());
        waker.join().unwrap();
        // The poke is used up: the next wait runs to its timeout
        let started = Instant::now();
        feed.wait(Duration::from_millis(100));
        assert!(started.elapsed() >= Duration::from_millis(90));
    }

    #[test]
    fn a_hand_pointer_counts_for_a_moment_after_the_last_frame_that_had_one() {
        let feed = Feed::default();
        assert!(!feed.hand_points());
        let mut read = FrameReader::default().read(&frame(), 1);
        assert!(!read.hand_pointer);
        read.hand_pointer = true;
        feed.push(read.clone());
        assert!(feed.hand_points());
        read.hand_pointer = false;
        feed.push(read);
        assert!(feed.hand_points(), "a hand that the cameras lose for a moment keeps the answer");
    }

    #[test]
    fn a_hand_that_the_cameras_track_is_no_controller() {
        let mut xr = frame();
        let pointing = |profile| Controller {
            profile: Some(profile),
            aim: Some(TrackedPose { pose: pose_at(0.2, 1.1, -0.4), linear_velocity: None, angular_velocity: None }),
            grip: Some(TrackedPose { pose: pose_at(0.2, 1.1, -0.3), linear_velocity: None, angular_velocity: None }),
            trigger: Trigger { value: 0.9, touched: true },
            squeeze: 0.8,
            ..Controller::default()
        };
        // The left hand holds a Touch controller. The right hand is a hand that the cameras track, and its pinch
        // is the trigger of the hand interaction profile.
        xr.left_controller = pointing(InteractionProfile::MetaTouchPlus);
        xr.right_controller = pointing(InteractionProfile::ExtHand);
        let read = FrameReader::default().read(&xr, 1).frame.unwrap();
        assert!(read.left_grip.is_some() && read.left_aim.is_some());
        assert_eq!(read.input.hands[0].trigger, 0.9);
        assert!(read.right_grip.is_none() && read.right_aim.is_none());
        assert_eq!(read.input.hands[1], HandInput::default());
        // Microsoft's hand interaction is a hand too
        xr.right_controller = pointing(InteractionProfile::MicrosoftHand);
        assert!(FrameReader::default().read(&xr, 1).frame.unwrap().right_grip.is_none());
    }

    #[test]
    fn a_recenter_keeps_its_distance_from_the_frames_clock() {
        let mut xr = frame();
        xr.recenter = Some(XrRecenter {
            change_time: xr.now + Duration::from_millis(20),
            pose_in_previous_space: Some(pose_at(0.5, 0.0, 0.25)),
        });
        let mut reader = FrameReader::default();
        let read = reader.read(&xr, 9_000_000_000);
        let recenter = read.recenter.unwrap();
        assert_eq!(recenter.change_time, 9_020_000_000);
        assert_eq!(recenter.origin.unwrap().position, [0.5, 0.0, 0.25]);
        // The velocity restarts, since the two spaces differ
        assert_eq!(reader.read(&frame(), 9_010_000_000).frame.unwrap().head.linear_velocity, [0.0; 3]);
    }

    #[test]
    fn the_feed_wakes_the_sender_at_each_push_and_keeps_every_recenter() {
        let feed = Feed::default();
        assert!(feed.wait(Duration::from_millis(1)).frame.is_none());
        let mut reader = FrameReader::default();
        feed.push(reader.read(&frame(), 10));
        let mut xr = frame();
        xr.recenter = Some(XrRecenter { change_time: xr.now, pose_in_previous_space: None });
        feed.push(reader.read(&xr, 20));
        feed.push(reader.read(&xr, 30));
        let wake = feed.wait(Duration::from_millis(1));
        // Only the newest frame is left. Both recenters are kept
        assert_eq!(wake.frame.unwrap().sample_time, 30);
        assert_eq!(wake.recenters.len(), 2);
        assert!(wake.views.is_some());
        assert!(feed.is_live());
        assert!(feed.wait(Duration::from_millis(1)).frame.is_none());
    }

    #[test]
    fn views_differ_when_the_eyes_move_apart() {
        let views = placeholder_views();
        assert!(!views_differ(&views, &views));
        let mut wider = views;
        wider.eye_pose[1].position[0] += 0.002;
        assert!(views_differ(&views, &wider));
        let mut rounded = views;
        rounded.eye_pose[1].position[0] += 0.0001;
        assert!(!views_differ(&views, &rounded));
    }

    /// A host and a client on loopback with the media over TCP. The host's messages go to a channel.
    fn loopback() -> (ClientLink, mpsc::Receiver<Heard>) {
        let listener = HostListener::bind("127.0.0.1:0", Identity::from_seed([4; 32])).unwrap();
        let addr = listener.local_addr().unwrap();
        let (heard, inbox) = mpsc::channel();
        thread::spawn(move || {
            let pending = listener.accept().unwrap();
            let wishes = HostWishes {
                eye_width: 1248,
                eye_width_unfoveated: 0,
                refresh_hz: 120.0,
                bitrate_bps: 100_000_000,
                codecs: vec![Codec::Hevc],
                foveation: None,
                full_range: true,
                slices: 1,
            };
            let stream = negotiate(&wishes, &pending.caps).unwrap();
            let host = pending.accept(stream, MediaMode::Tcp, 60_000).unwrap();
            host.duplex.read_tcp(|incoming| match incoming {
                Incoming::Control(message) => drop(heard.send(Heard::Control(message))),
                Incoming::Datagram(datagram) => drop(heard.send(Heard::Datagram(datagram))),
                Incoming::Media(_) => {}
            });
        });
        let identity = Identity::from_seed([3; 32]);
        let caps = ClientCaps {
            name: "test".into(),
            eye_size: [1680, 1760],
            views: placeholder_views(),
            refresh_rates: vec![120.0],
            codecs: vec![Codec::Hevc],
            decoders: 1,
            foveation: false,
            tcp_only: true,
        };
        (ClientLink::connect(addr, &caps, &identity, None, Duration::from_secs(10)).unwrap(), inbox)
    }

    enum Heard {
        Control(ClientMsg),
        Datagram(ClientDatagram),
    }

    #[test]
    fn a_hand_that_the_app_holds_back_travels_in_the_datagram_of_its_input() {
        let (link, inbox) = loopback();
        let link = std::sync::Arc::new(link);
        let feed = std::sync::Arc::new(Feed::default());
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let sender = {
            let (link, feed, stop) = (link.clone(), feed.clone(), stop.clone());
            let hello = FrameReader::default().read(&frame(), 1).views.unwrap();
            thread::spawn(move || send_loop(&link, &feed, true, 120.0, hello, &|| Focus::Focused, &stop))
        };
        let mut reader = FrameReader::default();
        // The right trigger is pulled and a panel holds the right hand
        let mut xr = frame();
        xr.right_controller.trigger = Trigger { value: 0.9, touched: true };
        let mut read = reader.read(&xr, 100);
        read.frame.as_mut().unwrap().held = [false, true];
        feed.push(read);
        let held = loop {
            match inbox.recv_timeout(Duration::from_secs(10)).expect("no input") {
                Heard::Datagram(ClientDatagram::InputHold(hold)) => break hold,
                _ => {}
            }
        };
        // The input is the real one: the host does the holding back
        assert_eq!(held.held, [false, true]);
        assert!(held.input.hands[1].trigger > 0.8);
        stop.store(true, Ordering::Relaxed);
        feed.wake();
        sender.join().unwrap();
    }

    #[test]
    fn the_sender_sends_each_frame_the_input_when_it_changes_and_every_recenter() {
        let (link, inbox) = loopback();
        let link = std::sync::Arc::new(link);
        let feed = std::sync::Arc::new(Feed::default());
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let sender = {
            let (link, feed, stop) = (link.clone(), feed.clone(), stop.clone());
            let hello = FrameReader::default().read(&frame(), 1).views.unwrap();
            thread::spawn(move || send_loop(&link, &feed, true, 120.0, hello, &|| Focus::Focused, &stop))
        };
        let next = |what: &str| inbox.recv_timeout(Duration::from_secs(10)).unwrap_or_else(|_| panic!("no {what}"));
        let mut reader = FrameReader::default();

        // The first frame carries a pressed button: tracking, then the input
        let mut xr = frame();
        xr.right_controller.primary = Button { pressed: true, touched: true };
        feed.push(reader.read(&xr, 100));
        let Heard::Datagram(ClientDatagram::Tracking(tracking)) = next("tracking") else { panic!("not tracking") };
        assert_eq!((tracking.sample_time, tracking.head.pose.position), (100, [0.0, 1.6, 0.0]));
        let Heard::Datagram(ClientDatagram::InputHold(InputHold { input, held })) = next("input") else { panic!("not input") };
        assert_eq!(input.hands[1].buttons, bit::PRIMARY);
        // No panel holds a hand back in this frame
        assert_eq!(held, [false, false]);

        // The same input again sends tracking only: the next message is the next sample
        feed.push(reader.read(&xr, 200));
        let Heard::Datagram(ClientDatagram::Tracking(tracking)) = next("tracking") else { panic!("not tracking") };
        assert_eq!(tracking.sample_time, 200);

        // A recenter goes over the control path, with the frame that follows it
        let mut xr = frame();
        xr.recenter = Some(XrRecenter { change_time: xr.now + Duration::from_millis(5), pose_in_previous_space: None });
        feed.push(reader.read(&xr, 300));
        let mut saw = (false, false);
        while saw != (true, true) {
            match next("recenter") {
                Heard::Control(ClientMsg::Recenter(recenter)) => {
                    assert_eq!(recenter.change_time, 300 + 5_000_000);
                    saw.0 = true;
                }
                Heard::Datagram(ClientDatagram::Tracking(tracking)) if tracking.sample_time == 300 => saw.1 = true,
                _ => {}
            }
        }

        // The floor goes once. It goes again when it moves by more than a centimeter
        let mut xr = frame();
        xr.stage_pose = Some(pose_at(0.0, -1.6, 0.0));
        feed.push(reader.read(&xr, 350));
        loop {
            if let Heard::Control(ClientMsg::Floor(height)) = next("floor") {
                assert_eq!(height, -1.6);
                break;
            }
        }
        xr.stage_pose = Some(pose_at(0.0, -1.605, 0.0));
        feed.push(reader.read(&xr, 360));
        xr.stage_pose = Some(pose_at(0.0, -1.5, 0.0));
        feed.push(reader.read(&xr, 370));
        loop {
            if let Heard::Control(ClientMsg::Floor(height)) = next("floor again") {
                assert_eq!(height, -1.5, "the move of half a centimeter did not go out");
                break;
            }
        }

        // A change of the distance between the eyes goes to the host as new views
        let mut xr = frame();
        xr.right_eye = Some(eye(0.04));
        feed.push(reader.read(&xr, 400));
        loop {
            if let Heard::Control(ClientMsg::Views(views)) = next("views") {
                assert_eq!(views.eye_pose[1].position[0], 0.04);
                break;
            }
        }
        assert_eq!(feed.host_views().unwrap().eye_pose[1].position[0], 0.04);

        stop.store(true, Ordering::Relaxed);
        sender.join().unwrap();
    }

    #[test]
    fn a_window_without_a_headset_sends_placeholder_frames_on_its_own() {
        let (link, inbox) = loopback();
        let link = std::sync::Arc::new(link);
        let feed = std::sync::Arc::new(Feed::default());
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let sender = {
            let (link, feed, stop) = (link.clone(), feed.clone(), stop.clone());
            thread::spawn(move || send_loop(&link, &feed, false, 120.0, placeholder_views(), &|| Focus::Focused, &stop))
        };
        let mut heard = 0;
        while heard < 3 {
            if let Heard::Datagram(ClientDatagram::Tracking(tracking)) = inbox.recv_timeout(Duration::from_secs(10)).unwrap() {
                assert_eq!(tracking.head.pose.position, [0.0, 1.6, 0.0]);
                // Both hands have a grip pose and no aim pose
                assert!(tracking.left_grip.is_some() && tracking.right_grip.is_some());
                assert!(tracking.left_aim.is_none() && tracking.right_aim.is_none());
                heard += 1;
            }
        }
        stop.store(true, Ordering::Relaxed);
        sender.join().unwrap();
    }

    #[test]
    fn the_feed_trades_the_tracked_buffers_without_a_copy_and_gives_each_frame_once() {
        let feed = Feed::default();
        let mut scratch = Box::new(TrackedFrame::new());
        let mut taken = Box::new(TrackedFrame::new());
        assert!(!feed.take_tracked(&mut taken), "nothing came yet");
        scratch.sample_time = 7;
        feed.swap_tracked(&mut scratch);
        scratch.sample_time = 8;
        feed.swap_tracked(&mut scratch);
        // The newest frame is the one that the sender gets, once
        assert!(feed.take_tracked(&mut taken));
        assert_eq!(taken.sample_time, 8);
        assert!(!feed.take_tracked(&mut taken));
        // The wish and the offer keep only the bits that this version knows
        feed.set_wish(Features(0xff));
        assert_eq!(feed.wish(), Features::ALL);
        feed.set_offer(Features::HANDS);
        assert_eq!(feed.offer(), Features::HANDS);
    }

    fn tracked_right_hand(xr: &mut XrFrame, x: f32) {
        let mut hand = cranpose_xr::HandState { tracking_state: cranpose_xr::TrackingState::Tracking, ..Default::default() };
        for (i, joint) in hand.joints.iter_mut().enumerate() {
            *joint = cranpose_xr::HandJoint { pose: pose_at(x + 0.005 * i as f32, 1.2, -0.4), radius: 0.01, valid: true, tracked: true };
        }
        xr.right_hand = hand;
    }

    /// Everything that the host hears for about a third of a second
    fn heard_for_a_moment(inbox: &mpsc::Receiver<Heard>) -> Vec<Heard> {
        let mut all = Vec::new();
        while let Ok(heard) = inbox.recv_timeout(Duration::from_millis(400)) {
            all.push(heard);
        }
        all
    }

    fn datagrams(heard: &[Heard]) -> Vec<&ClientDatagram> {
        heard.iter().filter_map(|h| if let Heard::Datagram(d) = h { Some(d) } else { None }).collect()
    }

    #[test]
    fn the_sender_offers_what_the_headset_has_and_sends_only_what_the_host_wished() {
        let (link, inbox) = loopback();
        let link = std::sync::Arc::new(link);
        let feed = std::sync::Arc::new(Feed::default());
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let sender = {
            let (link, feed, stop) = (link.clone(), feed.clone(), stop.clone());
            let hello = FrameReader::default().read(&frame(), 1).views.unwrap();
            thread::spawn(move || send_loop(&link, &feed, true, 120.0, hello, &|| Focus::Focused, &stop))
        };
        let mut reader = FrameReader::default();
        // A headset frame, as the frame loop hands it over: the tracked data first, then the frame
        let mut push = |xr: &XrFrame, at: Nanos, offer: Features| {
            let read = reader.read(xr, at);
            reader.push_tracked(xr, &read, &feed);
            // The test's capabilities cannot be Active: Cranpose builds them from a runtime
            feed.set_offer(offer);
            feed.push(read);
        };
        let offered = Features::HANDS.union(Features::BODY).union(Features::FACE);

        // The host wished nothing: the offer goes once, and no tracking datagram besides the head's
        let mut xr = frame();
        tracked_right_hand(&mut xr, 0.25);
        push(&xr, 100, offered);
        let heard = heard_for_a_moment(&inbox);
        let offers: Vec<Features> = heard.iter().filter_map(|h| if let Heard::Control(ClientMsg::TrackingOffer(f)) = h { Some(*f) } else { None }).collect();
        assert_eq!(offers, [offered]);
        assert!(datagrams(&heard).iter().all(|d| matches!(d, ClientDatagram::Tracking(_) | ClientDatagram::InputHold(_))), "nothing was wished");

        // The same offer again goes no more. The host wishes the hands: the right one arrives with its joints.
        feed.set_wish(Features::HANDS);
        push(&xr, 200, offered);
        let heard = heard_for_a_moment(&inbox);
        assert!(!heard.iter().any(|h| matches!(h, Heard::Control(ClientMsg::TrackingOffer(_)))));
        let hands: Vec<&Hands> = datagrams(&heard).into_iter().filter_map(|d| if let ClientDatagram::Hands(h) = d { Some(h) } else { None }).collect();
        assert_eq!(hands.len(), 1);
        assert_eq!(hands[0].sample_time, 200);
        assert!(hands[0].left.is_none());
        let joints = hands[0].right.as_ref().unwrap().joints().unwrap();
        assert!((joints[HAND_WRIST].pose.position[0] - (0.25 + 0.005)).abs() < 2e-4 && (joints[25].pose.position[0] - (0.25 + 0.125)).abs() < 2e-4);
        assert_eq!(hands[0].right.as_ref().unwrap().root.pose.position, [0.25 + 0.005, 1.2, -0.4]);

        // A lost hand goes once, as no hand at all. Then the sender sends no hands.
        let lost = frame();
        push(&lost, 300, offered);
        push(&lost, 400, offered);
        let heard = heard_for_a_moment(&inbox);
        let hands: Vec<&Hands> = datagrams(&heard).into_iter().filter_map(|d| if let ClientDatagram::Hands(h) = d { Some(h) } else { None }).collect();
        assert_eq!(hands.len(), 1, "the loss goes once");
        assert!(hands[0].left.is_none() && hands[0].right.is_none());

        // The host wishes the full body and the face. The face is valid, the body has 84 joints.
        feed.set_wish(Features::FULL_BODY.union(Features::FACE));
        let mut xr = frame();
        xr.body.active = true;
        xr.body.joint_count = 84;
        xr.body.confidence = 0.5;
        for (i, joint) in xr.body.joints.iter_mut().enumerate() {
            *joint = cranpose_xr::BodyJoint { pose: pose_at(0.0, 0.02 * i as f32, 0.0), valid: true, tracked: true };
        }
        xr.meta_face.is_valid = true;
        xr.meta_face.weights[3] = 1.0;
        push(&xr, 500, offered);
        let heard = heard_for_a_moment(&inbox);
        let sent = datagrams(&heard);
        let body = sent.iter().find_map(|d| if let ClientDatagram::Body(b) = d { Some(b) } else { None }).expect("a body");
        assert_eq!((body.set, body.sample_time, body.confidence), (cranvr_proto::tracking::BodySet::Full, 500, 0.5));
        assert!((body.joints().unwrap()[83].pose.position[1] - 0.02 * 83.0).abs() < 2e-4);
        let face = sent.iter().find_map(|d| if let ClientDatagram::Face(f) = d { Some(f) } else { None }).expect("a face");
        assert_eq!(face.weights().unwrap()[3], 1.0);
        assert!(!sent.iter().any(|d| matches!(d, ClientDatagram::Hands(_) | ClientDatagram::Eyes(_))), "the hands and the eyes were not wished");
        assert_eq!(face.flags, cranvr_proto::tracking::face::VALID, "no source was named");

        // The headset learns that its face comes from the microphone, as a Quest 3's does. The offer
        // goes again with the source, and each sample says it too.
        xr.meta_face.data_source = Some(cranpose_xr::FaceDataSource::Audio);
        let with_source = offered.union(Features::FACE_AUDIO);
        push(&xr, 550, with_source);
        let heard = heard_for_a_moment(&inbox);
        let offers: Vec<Features> = heard.iter().filter_map(|h| if let Heard::Control(ClientMsg::TrackingOffer(f)) = h { Some(*f) } else { None }).collect();
        assert_eq!(offers, [with_source]);
        assert!(with_source.offers_face_audio() && !with_source.offers_face_visual());
        let face = datagrams(&heard).into_iter().find_map(|d| if let ClientDatagram::Face(f) = d { Some(f.clone()) } else { None }).expect("a face");
        assert_eq!(face.flags, cranvr_proto::tracking::face::VALID | cranvr_proto::tracking::face::FROM_AUDIO);

        // The host wishes the eyes only
        feed.set_wish(Features::EYES);
        let mut xr = frame();
        xr.eyes.gaze = Some(XrPose { position: [0.0, 1.6, 0.0], orientation: [0.0, 0.0, 0.0, 1.0] });
        push(&xr, 600, Features::EYES);
        let heard = heard_for_a_moment(&inbox);
        assert!(heard.iter().any(|h| matches!(h, Heard::Control(ClientMsg::TrackingOffer(f)) if *f == Features::EYES)), "a changed offer goes again");
        assert!(datagrams(&heard).iter().any(|d| matches!(d, ClientDatagram::Eyes(e) if e.sample_time == 600 && e.gaze.is_some())));

        stop.store(true, Ordering::Relaxed);
        sender.join().unwrap();
    }

    #[test]
    fn a_face_of_android_xr_reaches_the_host_as_the_70_weights_of_meta() {
        use cranpose_xr::{FaceBlendShapeType, FaceConfidenceRegion, FaceExpression, FaceState, FaceTrackingProvider};
        let (link, inbox) = loopback();
        let mut xr = frame();
        xr.face = FaceState { tracking_state: cranpose_xr::TrackingState::Tracking, is_valid: true, ..FaceState::default() };
        xr.face.blend_shapes[FaceBlendShapeType::JawDrop.index()] = 0.5;
        xr.face.blend_shapes[FaceBlendShapeType::TongueOut.index()] = 1.0;
        // Android XR has tongue shapes that Meta lacks: they reach no slot
        xr.face.blend_shapes[FaceBlendShapeType::TongueLeft.index()] = 1.0;
        xr.face.confidence_regions[FaceConfidenceRegion::Lower.index()] = 0.25;
        xr.face.confidence_regions[FaceConfidenceRegion::LeftUpper.index()] = 0.5;
        xr.face.confidence_regions[FaceConfidenceRegion::RightUpper.index()] = 1.0;
        let mut reader = TrackedReader::default();
        let tracked = reader.read_face_of(&xr, 10, 20, Features::FACE, Some(FaceTrackingProvider::Android)).clone();
        assert!(send_tracked(&link, &tracked, Features::FACE, &mut Outgoing::default()));
        let heard = heard_for_a_moment(&inbox);
        let sent = datagrams(&heard);
        let face = sent.iter().find_map(|d| if let ClientDatagram::Face(f) = d { Some(f) } else { None }).expect("a face datagram");
        assert_eq!((face.sample_time, face.display_time), (10, 20));
        assert_eq!(face.flags, cranvr_proto::tracking::face::VALID | cranvr_proto::tracking::face::EYES_FOLLOWING_VALID);
        let weights = face.weights().unwrap();
        // A weight crosses the wire as a byte: within half a step of 1/255
        assert!((weights[FaceExpression::JawDrop.index()] - 0.5).abs() < 0.003);
        assert_eq!(weights[FaceExpression::TongueOut.index()], 1.0);
        assert_eq!(weights.iter().filter(|w| **w > 0.0).count(), 2, "nothing else is set");
        let confidences = face.confidences();
        assert!((confidences[0] - 0.25).abs() < 0.003 && (confidences[1] - 0.75).abs() < 0.003, "{confidences:?}");
    }

    #[test]
    fn a_frame_with_no_wish_reads_nothing_from_the_tracking() {
        // The reader returns before it copies a joint: the feed keeps no tracked frame
        let feed = Feed::default();
        let mut reader = FrameReader::default();
        let mut xr = frame();
        tracked_right_hand(&mut xr, 0.25);
        let read = reader.read(&xr, 100);
        reader.push_tracked(&xr, &read, &feed);
        assert!(!feed.take_tracked(&mut Box::new(TrackedFrame::new())));
        // A frame without a tracked head has no sample to pair the data with
        feed.set_wish(Features::HANDS);
        let mut headless = frame();
        headless.device_pose = None;
        headless.left_eye = None;
        let read = reader.read(&headless, 200);
        reader.push_tracked(&headless, &read, &feed);
        assert!(!feed.take_tracked(&mut Box::new(TrackedFrame::new())));
        // With a wish and a head, the hand arrives with the frame's sample time
        let read = reader.read(&xr, 300);
        reader.push_tracked(&xr, &read, &feed);
        let mut taken = Box::new(TrackedFrame::new());
        assert!(feed.take_tracked(&mut taken));
        assert_eq!(taken.sample_time, 300);
        assert!(taken.hands[1].tracked && !taken.hands[0].tracked);
    }

    #[test]
    fn the_grid_starts_at_the_headsets_display_time() {
        let feed = Feed::default();
        feed.push(FrameReader::default().read(&frame(), 1_000_000_000));
        let origin = 1_017_000_000;
        assert_eq!(feed.display_time_after(origin - 5, 8_000_000), origin);
        assert_eq!(feed.display_time_after(origin + 1, 8_000_000), origin + 8_000_000);
    }
}
