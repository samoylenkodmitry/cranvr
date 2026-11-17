//! The headset's per-frame data: tracking, input and the display clock.
//!
//! Cranpose's `SpatialFrameEffect` calls the app once for each headset frame with an `XrFrame`.
//! [`FrameReader::read`] turns that into what the host needs:
//! - the head pose and the controllers' poses and buttons, sampled at the start of the frame;
//! - the time at which the compositor will show the frame, as a prediction;
//! - the eye views with their field of view, which the host renders with;
//! - the recenter event, when the user recentered the headset.
//!
//! [`Feed`] carries them from the frame loop to the sender thread. Each frame wakes the sender, so
//! the host gets one tracking sample for each headset frame and no placeholder between them. The
//! sender sends placeholder values only when no headset exists, as in a flat window on a desktop.
//! There they keep the host's frame loop running with a still head.
//!
//! All poses are in the app's room, the LOCAL space of OpenXR. Its origin lies where the head was
//! when the app started, so the head's height above that point is about 0 and not the height above
//! the floor. The poses go to the host as they are. Cranpose reports no floor height.

use cranpose_xr::{Controller, Pose as XrPose, Recenter as XrRecenter, RenderViewpoint, TrackedPose, XrFrame};
use cranvr_proto::{
    clock::now_ns,
    link::ClientLink,
    message::{buttons as bit, ClientDatagram, ClientMsg, HandInput, Input, Recenter, Tracking, Views},
    motion::velocities,
    wire::{Fov, Motion, Nanos, Pose},
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
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
    pub left: Option<Motion>,
    pub right: Option<Motion>,
    pub input: Input,
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
}

/// Turns the frames of the headset into [`Read`]s. It keeps the head's last pose, from which it
/// estimates the head's velocities: Cranpose reports velocities for the controllers only.
#[derive(Default)]
pub struct FrameReader {
    last_head: Option<(Duration, XrPose)>,
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
        let frame = xr.device_pose.map(|pose| HeadsetFrame {
            sample_time: now,
            display_time: display.time,
            head: self.head(pose, xr.predicted_display_time, (xr.device_linear_velocity, xr.device_angular_velocity)),
            left: hand(&xr.left_controller),
            right: hand(&xr.right_controller),
            input: Input { sample_time: now, hands: [hand_input(&xr.left_controller), hand_input(&xr.right_controller)] },
        });
        if frame.is_none() {
            self.last_head = None;
        }
        Read { display, frame, views: views(xr), recenter, floor: xr.floor_height().filter(|height| height.is_finite()) }
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

/// The pose of the hand that holds the controller. The host takes it as the controller's pose.
fn hand(controller: &Controller) -> Option<Motion> {
    controller.grip.as_ref().map(motion)
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
}

#[derive(Default)]
struct RateCount {
    frames: u32,
    longest_gap: Duration,
    since: Option<Instant>,
}

/// A frame that came from the headset counts as live for this long.
const LIVE_FOR: Duration = Duration::from_secs(1);

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

    /// Waits up to `timeout` for a frame or a recenter.
    pub fn wait(&self, timeout: Duration) -> Wake {
        let guard = self.inner.lock().unwrap();
        let (mut inner, _) =
            self.wake.wait_timeout_while(guard, timeout, |i| i.pending.is_none() && i.recenters.is_empty()).unwrap();
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
        left: Some(hand(-0.2)),
        right: Some(hand(0.2)),
        input: Input::default(),
    }
}

/// The floor's height changes by more than this before the client tells the host again, in meters
const FLOOR_EPSILON: f32 = 0.01;
/// How often the sender looks at the stop flag while it waits for a frame
const STOP_CHECK: Duration = Duration::from_millis(20);

/// Sends the headset's tracking to the host until `stop` is set. In a headset each frame of the
/// feed goes out as it comes. Without a headset the sender makes placeholder frames once per
/// refresh. `hello_views` are the views that the host got in the Hello.
pub fn send_loop(link: &ClientLink, feed: &Feed, in_headset: bool, refresh_hz: f32, hello_views: Views, stop: &AtomicBool) {
    let period = Duration::from_secs_f64(1.0 / f64::from(refresh_hz.max(1.0)));
    let mut deadline = Instant::now();
    let mut sent_views = hello_views;
    let mut sent_floor: Option<f32> = None;
    while !stop.load(Ordering::Relaxed) {
        let wake = feed.wait(if in_headset { STOP_CHECK } else { period.min(STOP_CHECK) });
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
        let frame = match wake.frame {
            Some(frame) => frame,
            None if !in_headset && Instant::now() >= deadline => {
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
        let tracking = Tracking {
            sample_time: frame.sample_time,
            display_time: frame.display_time,
            head: frame.head,
            left: frame.left,
            right: frame.right,
        };
        if link.duplex.send_datagram(&ClientDatagram::Tracking(tracking)).is_err() {
            return;
        }
        // The input goes out with every sample. A datagram may get lost, and the host takes the
        // changes from the values that it sees, so a lost one leaves a button wrong for one frame.
        link.duplex.send_datagram(&ClientDatagram::Input(frame.input)).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranpose_xr::{Button, FieldOfView, Thumbstick, Trigger};
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
        assert!(read.left.is_none());
        let right = read.right.unwrap();
        assert_eq!((right.pose.position, right.linear_velocity, right.angular_velocity), ([0.2, 1.1, -0.3], [1.0, 0.0, 0.0], [0.0; 3]));
        let input = read.input.hands[1];
        assert_eq!(input.buttons, bit::PRIMARY | bit::STICK_CLICK | bit::TRIGGER_CLICK);
        assert_eq!(input.touches, bit::PRIMARY | bit::SECONDARY | bit::STICK_CLICK | bit::TRIGGER_CLICK | bit::THUMB_REST);
        assert_eq!((input.trigger, input.grip, input.stick), (0.75, 0.25, [0.5, -1.0]));
        assert_eq!(read.input.hands[0], HandInput::default());
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
    fn the_sender_sends_each_frame_the_input_when_it_changes_and_every_recenter() {
        let (link, inbox) = loopback();
        let link = std::sync::Arc::new(link);
        let feed = std::sync::Arc::new(Feed::default());
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let sender = {
            let (link, feed, stop) = (link.clone(), feed.clone(), stop.clone());
            let hello = FrameReader::default().read(&frame(), 1).views.unwrap();
            thread::spawn(move || send_loop(&link, &feed, true, 120.0, hello, &stop))
        };
        let next = |what: &str| inbox.recv_timeout(Duration::from_secs(10)).unwrap_or_else(|_| panic!("no {what}"));
        let mut reader = FrameReader::default();

        // The first frame carries a pressed button: tracking, then the input
        let mut xr = frame();
        xr.right_controller.primary = Button { pressed: true, touched: true };
        feed.push(reader.read(&xr, 100));
        let Heard::Datagram(ClientDatagram::Tracking(tracking)) = next("tracking") else { panic!("not tracking") };
        assert_eq!((tracking.sample_time, tracking.head.pose.position), (100, [0.0, 1.6, 0.0]));
        let Heard::Datagram(ClientDatagram::Input(input)) = next("input") else { panic!("not input") };
        assert_eq!(input.hands[1].buttons, bit::PRIMARY);

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
            thread::spawn(move || send_loop(&link, &feed, false, 120.0, placeholder_views(), &stop))
        };
        let mut heard = 0;
        while heard < 3 {
            if let Heard::Datagram(ClientDatagram::Tracking(tracking)) = inbox.recv_timeout(Duration::from_secs(10)).unwrap() {
                assert_eq!(tracking.head.pose.position, [0.0, 1.6, 0.0]);
                heard += 1;
            }
        }
        stop.store(true, Ordering::Relaxed);
        sender.join().unwrap();
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
