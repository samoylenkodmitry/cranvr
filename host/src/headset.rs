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

/// An encoded frame and what the headset needs to show it.
pub struct OutFrame {
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

    /// The share of a frame's budget that the pacer needs before it draws a frame for a new pose
    /// (see `Pacer::with_ready`). A client that sends several poses per refresh keeps the default.
    fn pacer_ready(&self) -> f32 {
        crate::pacer::READY
    }

    /// Whether `send_frame` reads `OutFrame::eye_poses`. The frame loop computes them only then.
    fn wants_eye_poses(&self) -> bool {
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
