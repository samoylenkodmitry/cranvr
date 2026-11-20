//! A phone or a tablet as the head.
//!
//! In phone mode the screen shows the host's picture flat. The device's motion sensors move the
//! host's camera. Cranpose's motion service (`cranpose::device_motion`) gives the device's rotation
//! as a quaternion with the time at which the sensor measured it. This file turns the rotation into
//! what the sender takes (`tracking::Read`). The head turns where the screen points and stays at
//! one place. The eye views match the screen. The display clock comes from the frame loop.
//!
//! The frames of the screen play the part of a headset's frames. Each frame reads the newest sensor
//! sample, so the host gets one tracking sample for each refresh. [`PhoneReader`] holds the state
//! and makes no platform call, so its tests run on any machine. [`PhoneFrames`] is the composable
//! that feeds it.
//!
//! The host's room is OpenXR's: x points right and y points up. The viewer looks along -z. The
//! reference frame of the sensors has z up (the Cranpose module describes the frame of a sample).
//! The screen frame that `Quaternion::in_screen_frame` gives has x to the right of the picture, y
//! to its top and z toward the person. The head looks into the screen, so the screen frame is a
//! head frame. One fixed quarter turn about x takes the reference frame to the room.

use crate::{
    hub::{PhoneInfo, hub},
    tracking::{Display, HeadsetFrame, Read},
};
use cranpose::prelude::*;
use cranpose::{
    DisplayRotation, MotionConfig, Quaternion, device_motion_display_rotation,
    device_motion_now_ns, latest_device_motion, rememberDeviceMotion,
};
use cranvr_proto::{
    clock::now_ns,
    message::{Input, Views},
    motion::{extrapolate, velocities},
    wire::{Fov, Motion, Nanos, Pose},
};
use std::{
    collections::VecDeque,
    f32::consts::{FRAC_1_SQRT_2, FRAC_PI_4},
    sync::atomic::{AtomicBool, Ordering},
};

/// Where the head sits. The device gives no position, so the head stays here: the height of a
/// standing person, as a window without a headset sends too.
pub const HEAD_HEIGHT: f32 = 1.6;
/// The widest eye picture that the phone asks for. The host chooses the width of an unfoveated
/// eye by its own rule. It keeps the aspect of the size that the Hello gives.
const MAX_EYE_WIDTH: f32 = 1920.0;
/// The field of view across the screen's long side: 90 degrees, so a half of 45 degrees.
const HALF_ACROSS: f32 = FRAC_PI_4;

/// The frame period before the frame loop has measured one.
const FIRST_PERIOD: Nanos = 16_666_667;
/// The rate comes from the last `KEPT_INTERVALS` frame intervals. It needs `MIN_INTERVALS` of them
/// first. 45 intervals take three quarters of a second at 60 Hz and 0.4 seconds at 120 Hz.
const KEPT_INTERVALS: usize = 90;
const MIN_INTERVALS: usize = 45;
/// A gap between two frames longer than this is a pause of the app and is no interval.
const MAX_INTERVAL: Nanos = 250_000_000;
/// The rates that screens have. A measured rate within 6 percent of the nearest one is that rate.
const STANDARD_RATES: [f32; 12] = [
    24.0, 30.0, 45.0, 48.0, 50.0, 60.0, 72.0, 90.0, 100.0, 120.0, 144.0, 165.0,
];

/// Two sensor samples closer than this in time give no velocity: they are one sample.
const MIN_VELOCITY_GAP: Nanos = 1_500_000;
/// Two samples further apart than this give no velocity either: the device paused.
const MAX_VELOCITY_GAP: Nanos = 50_000_000;
/// How far ahead the client moves the pose that it sends, at most. The host moves it on from there.
const MAX_PREDICT: f32 = 0.06;
/// The head looks along the horizon when its forward direction has at least this much horizontal
/// part. Below it the heading is not defined well enough to take a front from it.
const MIN_HORIZONTAL: f32 = 0.5;

/// The turn from the sensors' reference frame (z up) to the host's room (y up): a quarter turn
/// about x, backward. The reference frame's y becomes the room's -z. Its z becomes the room's y.
fn room_from_reference() -> Quaternion {
    Quaternion::new(-FRAC_1_SQRT_2, 0.0, 0.0, FRAC_1_SQRT_2)
}

/// The head's orientation in the room, with the heading of the sensors. It is the screen's frame in
/// the reference frame, then the quarter turn. `rotation` tells how far the person has turned the
/// device from its natural orientation.
pub fn head_in_room(device: Quaternion, rotation: DisplayRotation) -> Quaternion {
    (room_from_reference() * device.in_screen_frame(rotation)).normalized()
}

/// The turn about the vertical that puts the head's forward direction on the room's -z, which is
/// the host's front. `None` while the head looks up or down: its heading is not defined then.
fn heading_fix(head: Quaternion) -> Option<Quaternion> {
    let [x, _, z] = head.rotate([0.0, 0.0, -1.0]);
    if x.hypot(z) < MIN_HORIZONTAL {
        return None;
    }
    // A turn by `yaw` about +y takes -z to (-sin, 0, -cos)
    let yaw = f32::atan2(-x, -z);
    let (sine, cosine) = (-yaw / 2.0).sin_cos();
    Some(Quaternion::new(0.0, sine, 0.0, cosine))
}

/// The direction in which a head with this orientation looks, in the room's axes.
pub fn forward(orientation: [f32; 4]) -> [f32; 3] {
    let [x, y, z, w] = orientation;
    Quaternion::new(x, y, z, w).rotate([0.0, 0.0, -1.0])
}

/// The views of a screen of `screen` pixels. The size may come in either orientation. Both eyes
/// see 90 degrees across the long side and the screen's aspect along the short side. Both eyes
/// stand at the head's center. The host renders two eyes as for a headset. The phone shows the
/// left one, which is then the view of the head itself.
pub fn phone_views(screen: [u32; 2]) -> Views {
    let (long, short) = (
        screen[0].max(screen[1]).max(1) as f32,
        screen[0].min(screen[1]).max(1) as f32,
    );
    let half_down = (HALF_ACROSS.tan() * short / long).atan();
    let fov = Fov {
        left: -HALF_ACROSS,
        right: HALF_ACROSS,
        up: half_down,
        down: -half_down,
    };
    Views {
        fov: [fov, fov],
        eye_pose: [Pose::IDENTITY; 2],
    }
}

/// The views that the Hello carries when the screen's size is not known in time.
pub fn default_views() -> Views {
    phone_views([1920, 1080])
}

/// The size of one eye's picture that the Hello gives. It has the screen's shape with a long side
/// of at most `MAX_EYE_WIDTH`. The host takes the aspect ratio and picks the width by its own wish.
pub fn eye_size(screen: [u32; 2]) -> [u32; 2] {
    let (long, short) = (
        screen[0].max(screen[1]).max(2) as f32,
        screen[0].min(screen[1]).max(2) as f32,
    );
    let scale = (MAX_EYE_WIDTH / long).min(1.0);
    let even = |side: f32| ((side * scale).round() as u32).max(2) & !1;
    [even(long), even(short)]
}

/// The nearest standard refresh rate when the measured one lies within 6 percent of it. Any other
/// measured rate is rounded. The frame clock of a window on a computer runs a few percent slow.
pub fn snap_rate(hz: f32) -> f32 {
    STANDARD_RATES
        .iter()
        .copied()
        .min_by(|a, b| (hz - a).abs().total_cmp(&(hz - b).abs()))
        .filter(|standard| (hz - standard).abs() <= standard * 0.06)
        .unwrap_or_else(|| hz.round().max(1.0))
}

/// A measurement of the device's rotation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SensorSample {
    pub rotation: Quaternion,
    /// When the sensor measured, on the client's clock
    pub taken: Nanos,
}

/// What one frame of the screen gives the reader.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameInput {
    /// The client's clock at the start of the frame
    pub now: Nanos,
    /// The newest sample of the sensor. It is `None` on a device with no sensor and before the
    /// sensor reports.
    pub sample: Option<SensorSample>,
    /// How far the person has turned the device
    pub rotation: DisplayRotation,
    /// The window's size in pixels, once the layout knows it
    pub screen: Option<[u32; 2]>,
    /// The user asked for the front to be where the screen points now
    pub recenter: bool,
}

/// Turns the frames of the screen into what the sender takes. It keeps the frame intervals. The
/// refresh rate follows from them. It also keeps the last sensor pose, from which it estimates the
/// head's angular velocity.
#[derive(Default)]
pub struct PhoneReader {
    last_frame: Option<Nanos>,
    intervals: VecDeque<Nanos>,
    scratch: Vec<Nanos>,
    screen: Option<[u32; 2]>,
    /// The turn about the vertical that puts the first look at the host's front
    heading: Option<Quaternion>,
    /// The sample that the last velocity came from: its orientation in the room and its time
    last_pose: Option<(Quaternion, Nanos)>,
    angular_velocity: [f32; 3],
    /// The head's last orientation, which a frame without a sample sends again
    orientation: Option<Quaternion>,
    last_taken: Nanos,
}

impl PhoneReader {
    /// The interval between frames, once enough frames came. It is the lower quartile of the kept
    /// intervals. A frame that comes late for any reason lengthens its interval. No frame comes
    /// early for the display, so the short intervals are the display's own.
    fn measured_period(&mut self) -> Option<Nanos> {
        if self.intervals.len() < MIN_INTERVALS {
            return None;
        }
        self.scratch.clear();
        self.scratch.extend(self.intervals.iter());
        let quartile = self.scratch.len() / 4;
        Some(*self.scratch.select_nth_unstable(quartile).1)
    }

    /// The screen's refresh rate in Hz, measured from the frame intervals
    pub fn refresh_hz(&mut self) -> Option<f32> {
        self.measured_period()
            .map(|period| snap_rate(1e9 / period as f32))
    }

    /// What the Hello needs to know about the screen: its size and its rate. It is `None` until
    /// both are known.
    pub fn info(&mut self) -> Option<PhoneInfo> {
        let screen = self.screen?;
        Some(PhoneInfo {
            screen: [screen[0].max(screen[1]), screen[0].min(screen[1])],
            refresh_hz: self.refresh_hz()?,
        })
    }

    fn note_frame(&mut self, now: Nanos) {
        if let Some(last) = self
            .last_frame
            .filter(|last| now > *last && now - last <= MAX_INTERVAL)
        {
            if self.intervals.len() >= KEPT_INTERVALS {
                self.intervals.pop_front();
            }
            self.intervals.push_back(now - last);
        }
        self.last_frame = Some(now);
    }

    /// Reads one frame of the screen.
    pub fn read(&mut self, input: &FrameInput) -> Read {
        self.note_frame(input.now);
        self.screen = input.screen.or(self.screen);
        let period = self.measured_period().unwrap_or(FIRST_PERIOD);
        // The frame in work reaches the display one period after it starts
        let display = Display {
            time: input.now + period,
            period,
            offset: 0,
        };
        let (taken, head) = self.head(input, display.time);
        let frame = HeadsetFrame {
            sample_time: taken,
            display_time: display.time,
            head,
            left_grip: None,
            right_grip: None,
            left_aim: None,
            right_aim: None,
            input: Input::default(),
        };
        // The views wait for the rate, because the Hello needs both
        let views = self.info().map(|info| phone_views(info.screen));
        Read {
            display,
            frame: Some(frame),
            views,
            recenter: None,
            floor: None,
            hand_pointer: false,
        }
    }

    /// The head at the frame. It returns the time at which the sensor measured it and its motion.
    /// The pose lies ahead at `display_time`, as a headset's runtime predicts it.
    fn head(&mut self, input: &FrameInput, display_time: Nanos) -> (Nanos, Motion) {
        let position = [0.0, HEAD_HEIGHT, 0.0];
        let Some(sample) = input.sample else {
            // Without a sensor the head looks straight ahead and stays there
            let orientation = self.orientation.unwrap_or(Quaternion::IDENTITY);
            let pose = Pose {
                orientation: [orientation.x, orientation.y, orientation.z, orientation.w],
                position,
            };
            return (
                input.now,
                Motion {
                    pose,
                    ..Motion::default()
                },
            );
        };
        let measured = head_in_room(sample.rotation, input.rotation);
        if input.recenter {
            self.heading = None;
            self.last_pose = None;
        }
        if self.heading.is_none() {
            self.heading = heading_fix(measured);
        }
        let orientation = (self.heading.unwrap_or(Quaternion::IDENTITY) * measured).normalized();
        // Samples never go back in time. A sensor that reports slower than the screen gives the
        // same sample for a few frames.
        let taken = sample.taken.max(self.last_taken).min(input.now);
        self.last_taken = taken;
        self.angular_velocity = self.angular_velocity_at(orientation, taken);
        self.orientation = Some(orientation);
        let pose = Pose {
            orientation: [orientation.x, orientation.y, orientation.z, orientation.w],
            position,
        };
        let lead = (display_time.saturating_sub(taken) as f32 / 1e9).min(MAX_PREDICT);
        let motion = Motion {
            pose,
            linear_velocity: [0.0; 3],
            angular_velocity: self.angular_velocity,
        };
        let predicted = Pose {
            position,
            ..extrapolate(&motion, lead)
        };
        (
            taken,
            Motion {
                pose: predicted,
                ..motion
            },
        )
    }

    /// The angular velocity from the last sample to this one, in the room's axes. A sample that is
    /// the same instant as the last one keeps the last velocity.
    fn angular_velocity_at(&mut self, orientation: Quaternion, taken: Nanos) -> [f32; 3] {
        let pose = |q: Quaternion| Pose {
            orientation: [q.x, q.y, q.z, q.w],
            position: [0.0; 3],
        };
        let Some((before, before_time)) = self.last_pose else {
            self.last_pose = Some((orientation, taken));
            return [0.0; 3];
        };
        let gap = taken.saturating_sub(before_time);
        if gap < MIN_VELOCITY_GAP {
            return self.angular_velocity;
        }
        self.last_pose = Some((orientation, taken));
        if gap > MAX_VELOCITY_GAP {
            return [0.0; 3];
        }
        velocities(&pose(before), &pose(orientation), gap as f32 / 1e9).1
    }
}

/// Set by the Recenter button. The next frame with a sensor sample puts the front where the screen
/// points.
static RECENTER: AtomicBool = AtomicBool::new(false);

/// Asks for the host's front to lie where the screen points now.
pub fn request_recenter() {
    RECENTER.store(true, Ordering::Release);
}

/// The number of frames between two asks for the display's rotation. The call may cost a call into
/// the platform. A person turns the device slowly.
const ROTATION_EVERY: u32 = 8;

/// Feeds the sender with the phone's head for each frame of the screen. The motion sensor runs
/// while this is composed. The frame loop never stops. The frames that it asks for keep the
/// screen's clock running. The refresh rate comes from that clock.
#[composable]
pub fn PhoneFrames() {
    // The sensor stays on while this call is in the composition
    let _motion = rememberDeviceMotion(MotionConfig::default());
    LaunchedEffect((), async move |scope| {
        let clock = scope.runtime().frame_clock();
        let mut reader = PhoneReader::default();
        let mut rotation = DisplayRotation::default();
        let mut frame = 0u32;
        let mut told = None;
        loop {
            clock.next_frame().await;
            let now = now_ns();
            if frame.is_multiple_of(ROTATION_EVERY) {
                rotation = device_motion_display_rotation();
            }
            frame = frame.wrapping_add(1);
            let sample = latest_device_motion().map(|sample| {
                // The sensor's clock is not the client's: the age of the sample carries over
                let age = device_motion_now_ns()
                    .unwrap_or(sample.timestamp_ns)
                    .saturating_sub(sample.timestamp_ns);
                SensorSample {
                    rotation: sample.rotation,
                    taken: now.saturating_sub(age),
                }
            });
            let input = FrameInput {
                now,
                sample,
                rotation,
                screen: hub().screen(),
                recenter: RECENTER.swap(false, Ordering::AcqRel),
            };
            let read = reader.read(&input);
            // The hub learns the rate before the first views reach the feed. The Hello waits for
            // the views, then reads the rate.
            let info = reader.info();
            if info != told {
                told = info;
                if let Some(info) = info {
                    log::info!(
                        "phone screen {}x{} at {} Hz",
                        info.screen[0],
                        info.screen[1],
                        info.refresh_hz
                    );
                    hub().set_phone_info(info);
                }
            }
            hub().feed.push(read);
        }
    });
}

#[cfg(test)]
#[path = "tests/phone.rs"]
mod tests;
