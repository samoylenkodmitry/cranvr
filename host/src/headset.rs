//! What the host's frame loop needs from a headset's link. Two protocols stand behind it. ALVR's
//! v20 protocol serves the stock ALVR client (`alvr.rs`). cranvr's own protocol serves cranvr's
//! client (`native.rs`). The frame loop, the encoder and the pacer do not know which one runs.
//!
//! The interface has four parts:
//! - events from the headset: a connection, a tracking sample, buttons (`EventSource`);
//! - poses for a frame and the headset's display clock;
//! - what goes to the headset: encoded frames, the decoder configuration, haptics;
//! - numbers about the link, for the stats line.

use crate::{runtime_link::Haptic, session::Negotiated};
use alvr_common::{
    glam::{Quat, Vec3},
    DeviceMotion, Pose,
};
use alvr_server_core::ViewsConfig;
use serde_json::Value;
use std::{net::IpAddr, sync::mpsc::RecvTimeoutError, time::Duration};

pub mod alvr;
pub mod display_lock;
pub mod native;

/// The protocol between the host and the headset's app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    /// cranvr's own protocol (`cranvr-proto`), for cranvr's own Quest client
    Native,
    /// ALVR v20, for the stock ALVR client
    Alvr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Head,
    LeftHand,
    RightHand,
}

/// A button or an analog input of a controller, under the path id that ALVR gives it. The
/// runtime in Wine reads these ids, so the native protocol maps its inputs to them.
#[derive(Clone, Copy, Debug)]
pub struct Button {
    pub path_id: u64,
    /// The value is an analog one, not 0 or 1
    pub scalar: bool,
    pub value: f32,
}

pub enum Event {
    /// A headset connected. `stream` is what the two sides agreed on: `None` when the link cannot
    /// tell.
    Connected { stream: Option<Negotiated>, ip: Option<IpAddr> },
    Disconnected,
    /// The headset sent a new tracking sample. With the native protocol the sample time is on the
    /// headset's clock. With ALVR it is on ALVR's. Frames carry it back.
    Tracking { sample_ns: u64 },
    Views(ViewsConfig),
    Buttons(Vec<Button>),
    /// The client lost video and wants a frame that it can start from
    RequestIdr,
    /// The headset's origin moved, because the user recentered. The poses that follow are relative
    /// to the new origin. Only the native protocol reports it.
    Recenter,
    /// A press on a button of the headset's console, as a JSON text (see `status::console_request`).
    /// Only the native protocol reports it. With ALVR the console's request travels through the
    /// desktop layer.
    Console(String),
    /// The link wants the host to stop
    Stop,
    /// An event that the host has no use for
    Other,
}

/// Where the events of a link come from. The host's event loop owns it.
pub trait EventSource: Send {
    fn next(&mut self, timeout: Duration) -> Result<Event, RecvTimeoutError>;
}

/// A part of an encoded frame. Today the encoder makes one slice per frame, which covers all of it.
pub struct FrameSlice {
    /// x, y, width, height in the encoded frame's pixels. ALVR's client needs no slice layout.
    pub rect: [u32; 4],
    /// The encoded bytes, Annex B
    pub data: Vec<u8>,
}

/// When the next frame should start and which display time it aims for (see `display_lock.rs`).
#[derive(Clone, Copy, Debug)]
pub struct FramePlan {
    /// The time until the frame should start. Zero when it is due.
    pub start_in: Duration,
    /// The headset's display time that the frame aims for, on the headset's clock
    pub target: u64,
    /// The planned start, on the headset's clock
    pub start: u64,
}

/// An encoded frame and what the headset needs to show it.
pub struct OutFrame {
    /// The plan that the frame followed, when the link locks its frames to the display
    pub plan: Option<FramePlan>,
    /// The tracking sample that the frame was rendered for
    pub sample_ns: u64,
    pub idr: bool,
    /// The pose that each eye was rendered from. Only links that `wants_eye_poses` read them.
    pub eye_poses: [Pose; 2],
    pub slices: Vec<FrameSlice>,
}

pub trait Headset: Send + Sync {
    /// The time until the headset's display shows its next frame, when the link knows it.
    fn until_next_vsync(&self) -> Option<Duration>;

    /// The pose of a device at a tracking sample.
    fn motion(&self, device: Device, sample_ns: u64) -> Option<DeviceMotion>;

    /// The pose of a device at a tracking sample, moved by its velocities to the headset's display
    /// time `target`. A link without a display clock gives the pose of the sample.
    fn motion_at(&self, device: Device, sample_ns: u64, _target: u64) -> Option<DeviceMotion> {
        self.motion(device, sample_ns)
    }

    /// Where the next frame of the host's own scene should start so that it reaches the headset's
    /// display on its grid. `None` when the link does not lock its frames to the display, or does
    /// not know the display yet. The frame loop then renders at the arrival of a pose.
    fn frame_plan(&self) -> Option<FramePlan> {
        None
    }

    /// The frame loop takes the plan with this target and renders. `late` is how long after the
    /// planned start it began.
    fn frame_started(&self, _target: u64, _late: Duration) {}

    /// The share of a frame's budget that the pacer needs before it draws a frame for a new pose
    /// (see `Pacer::with_ready`). A client that sends several poses per refresh keeps the default.
    fn pacer_ready(&self) -> f32 {
        crate::pacer::READY
    }

    /// Whether `send_frame` reads `OutFrame::eye_poses`. The frame loop computes them only then.
    fn wants_eye_poses(&self) -> bool {
        false
    }

    /// Whether the link takes a second frame for the same tracking sample, which the frame loop
    /// numbers with the sample's time plus a few nanoseconds. A link whose client names frames
    /// by the samples that it sent cannot: ALVR's client sees a timestamp that it never sent.
    fn repeats_samples(&self) -> bool {
        false
    }

    /// The parameter sets of the HEVC stream (VPS, SPS and PPS in Annex B).
    fn set_decoder_config(&self, config: Vec<u8>);

    /// Sends an encoded frame.
    fn send_frame(&self, frame: OutFrame);

    /// The frame loop handed the frame for this sample to the encoder.
    fn frame_submitted(&self, sample_ns: u64);

    /// The bitrate that the link wants from the encoder, when it has an opinion.
    fn bitrate_bps(&self) -> Option<u64>;

    fn send_haptics(&self, haptic: Haptic);

    /// Sends the console's menu as a JSON text, when the link has a console. With ALVR the
    /// desktop layer sends the menu to the Quest app instead.
    fn send_menu(&self, _menu: &str) {}

    /// The averages since the last call: the time of each stage from the pose to the display, and
    /// the link's own counters. `None` when no report came.
    fn take_stats(&self) -> Option<Value>;
}

/// The pose of each eye in the tracking space, from the head's pose and the eyes' poses in the
/// head's space (rotation as x, y, z, w; position as x, y, z and an unused fourth number).
pub fn eye_poses(head: &Pose, eye_q: [[f32; 4]; 2], eye_p: [[f32; 4]; 2]) -> [Pose; 2] {
    std::array::from_fn(|e| {
        let q = Quat::from_array(eye_q[e]);
        let p = Vec3::new(eye_p[e][0], eye_p[e][1], eye_p[e][2]);
        Pose { orientation: head.orientation * q, position: head.position + head.orientation * p }
    })
}

#[cfg(test)]
#[path = "tests/headset.rs"]
mod tests;
