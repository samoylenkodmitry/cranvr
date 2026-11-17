//! The seam for the headset's per-frame data: tracking, input and the display clock.
//!
//! The host needs these from the headset in every frame:
//! - the head pose and the controllers' poses and buttons, sampled as late as possible;
//! - the time at which the compositor will show the frame, as a prediction;
//! - the eye views with their field of view.
//!
//! Cranpose's public API gives none of them yet. An effect composable that runs once for each
//! headset frame is being built in Cranpose. It will carry the predicted display time, the head
//! pose, both eye views, the full controller state, haptics and the recenter event. When it lands,
//! its callback builds a [`HeadsetFrame`] and calls [`Feed::push`]. Nothing else in the client
//! changes.
//!
//! Until then nothing pushes, and the functions in this file make placeholder values. They keep
//! the host's frame loop running, because the host renders a frame only when a new tracking sample
//! arrives. A placeholder frame shows a still head at standing height. The display clock runs on
//! a fixed grid at the stream's rate, as the probe's simulated display does. The stream shows real
//! pictures with these values. The values do not follow a real head.

use cranvr_proto::{
    clock::now_ns,
    link::ClientLink,
    message::{ClientDatagram, Input, Tracking, Views},
    wire::{Fov, Motion, Nanos, Pose},
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};

/// What the headset reports for one frame. Times are on the client clock of
/// [`cranvr_proto::clock::now_ns`].
#[derive(Clone, Copy, Debug)]
pub struct HeadsetFrame {
    /// When the poses were sampled. The host returns this time with the frame that it renders.
    pub sample_time: Nanos,
    /// When the compositor will show this frame, as the runtime predicts it
    #[expect(dead_code, reason = "only `Feed::push` reads it, and the per-frame API of Cranpose will call that")]
    pub display_time: Nanos,
    pub head: Motion,
    pub left: Option<Motion>,
    pub right: Option<Motion>,
    pub input: Input,
}

/// The newest frame from the headset and the display clock that follows from it.
#[derive(Default)]
pub struct Feed {
    inner: Mutex<FeedInner>,
}

#[derive(Default)]
struct FeedInner {
    pending: Option<HeadsetFrame>,
    /// One display time of the headset. All others lie a whole number of periods from it.
    grid_origin: Option<Nanos>,
    /// The last time a frame came from the headset
    last_push: Option<Instant>,
}

/// A frame that came from the headset counts as live for this long.
const LIVE_FOR: Duration = Duration::from_secs(1);

impl Feed {
    /// Hands the newest frame of the headset to the sender. A frame that the sender has not taken
    /// yet is replaced.
    #[expect(dead_code, reason = "the per-frame API of Cranpose will call it")]
    pub fn push(&self, frame: HeadsetFrame) {
        let mut inner = self.inner.lock().unwrap();
        inner.grid_origin = Some(frame.display_time);
        inner.last_push = Some(Instant::now());
        inner.pending = Some(frame);
    }

    fn take(&self) -> Option<HeadsetFrame> {
        self.inner.lock().unwrap().pending.take()
    }

    /// Whether a real frame came from the headset lately.
    pub fn is_live(&self) -> bool {
        self.inner.lock().unwrap().last_push.is_some_and(|at| at.elapsed() < LIVE_FOR)
    }

    /// The first display time at or after `t` on the grid of the headset's display, for a stream
    /// that runs at `period` nanoseconds a frame.
    pub fn display_time_after(&self, t: Nanos, period: Nanos) -> Nanos {
        let origin = *self.inner.lock().unwrap().grid_origin.get_or_insert_with(now_ns);
        if t <= origin {
            origin
        } else {
            origin + (t - origin).div_ceil(period.max(1)) * period.max(1)
        }
    }
}

/// The distance between the eyes in meters. The headset reports its own through the eye views.
const EYE_X: f32 = 0.032;

/// The eye views of the Quest 3, roughly. The Hello message carries them to the host. The
/// per-frame API of Cranpose will report the real ones.
pub fn placeholder_views() -> Views {
    let fov = Fov { left: -0.9, right: 0.9, up: 0.9, down: -0.9 };
    let eye = |x: f32| Pose { position: [x, 0.0, 0.0], ..Pose::IDENTITY };
    Views { fov: [fov, fov], eye_pose: [eye(-EYE_X), eye(EYE_X)] }
}

/// A frame of a still head with both controllers held in front of it.
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

/// Sends the headset's tracking to the host once per refresh until `stop` is set. It sends the
/// frame from the feed, or a placeholder while the feed has none.
pub fn send_loop(link: &ClientLink, feed: &Feed, refresh_hz: f32, stop: &AtomicBool) {
    let period = Duration::from_secs_f64(1.0 / f64::from(refresh_hz.max(1.0)));
    let mut deadline = Instant::now();
    let mut last_input = None;
    while !stop.load(Ordering::Relaxed) {
        let now = now_ns();
        let frame = feed.take().unwrap_or_else(|| {
            let display = feed.display_time_after(now, period.as_nanos() as u64);
            placeholder_frame(now, display)
        });
        let tracking = Tracking { sample_time: frame.sample_time, head: frame.head, left: frame.left, right: frame.right };
        if link.duplex.send_datagram(&ClientDatagram::Tracking(tracking)).is_err() {
            return;
        }
        // The input travels when it changes: a held button stays held on the host
        if last_input != Some(frame.input) {
            last_input = Some(frame.input);
            let input = Input { sample_time: frame.sample_time, ..frame.input };
            link.duplex.send_datagram(&ClientDatagram::Input(input)).ok();
        }
        // `park_timeout` may wake early, so the loop reads the clock. A thread that fell far
        // behind starts again from now and sends no burst of samples.
        deadline += period;
        if deadline + period * 4 < Instant::now() {
            deadline = Instant::now();
        }
        loop {
            let now = Instant::now();
            if now >= deadline || stop.load(Ordering::Relaxed) {
                break;
            }
            thread::park_timeout((deadline - now).min(Duration::from_millis(20)));
        }
    }
}
