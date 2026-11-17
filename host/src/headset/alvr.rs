//! ALVR v20 behind the headset interface. The server core runs ALVR's discovery, handshake and
//! streaming. This file passes the frame loop's calls to it and turns its events into `Event`.

use super::{Button, Device, Event, EventSource, Headset, OutFrame};
use crate::{latency, network, runtime_link::Haptic, session};
use alvr_common::{DeviceMotion, HAND_LEFT_ID, HAND_RIGHT_ID, HEAD_ID};
use alvr_packets::{ButtonValue, Haptics};
use alvr_server_core::{ServerCoreContext, ServerCoreEvent};
use alvr_session::CodecType;
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{
        mpsc::{self, RecvTimeoutError},
        Arc,
    },
    time::Duration,
};

struct Alvr {
    context: ServerCoreContext,
}

struct Events {
    receiver: mpsc::Receiver<ServerCoreEvent>,
    /// ALVR's session file: the agreed stream and the client's address are in it
    session_path: PathBuf,
}

/// Starts the server core and ALVR's connection. Call it after `init_logging`.
pub fn start(session_path: PathBuf) -> (Arc<dyn Headset>, Box<dyn EventSource>) {
    latency::start();
    crate::compat::start();
    let (context, receiver) = ServerCoreContext::new();
    context.start_connection();
    (Arc::new(Alvr { context }), Box::new(Events { receiver, session_path }))
}

impl EventSource for Events {
    fn next(&mut self, timeout: Duration) -> Result<Event, RecvTimeoutError> {
        Ok(match self.receiver.recv_timeout(timeout)? {
            ServerCoreEvent::ClientConnected => {
                let ip = network::client_ip(&self.session_path);
                Event::Connected { stream: session::read_negotiated(&self.session_path), ip }
            }
            ServerCoreEvent::ClientDisconnected => Event::Disconnected,
            ServerCoreEvent::Tracking { sample_timestamp } => Event::Tracking { sample_ns: sample_timestamp.as_nanos() as u64 },
            ServerCoreEvent::ViewsConfig(config) => Event::Views(config),
            ServerCoreEvent::Buttons(entries) => Event::Buttons(
                entries
                    .into_iter()
                    .map(|entry| match entry.value {
                        ButtonValue::Binary(on) => Button { path_id: entry.path_id, scalar: false, value: on as u32 as f32 },
                        ButtonValue::Scalar(v) => Button { path_id: entry.path_id, scalar: true, value: v },
                    })
                    .collect(),
            ),
            ServerCoreEvent::RequestIDR => Event::RequestIdr,
            ServerCoreEvent::ShutdownPending | ServerCoreEvent::RestartPending => Event::Stop,
            _ => Event::Other,
        })
    }
}

impl Headset for Alvr {
    fn until_next_vsync(&self) -> Option<Duration> {
        self.context.duration_until_next_vsync()
    }

    fn motion(&self, device: Device, sample_ns: u64) -> Option<DeviceMotion> {
        let id = match device {
            Device::Head => *HEAD_ID,
            Device::LeftHand => *HAND_LEFT_ID,
            Device::RightHand => *HAND_RIGHT_ID,
        };
        self.context.get_device_motion(id, Duration::from_nanos(sample_ns))
    }

    fn set_decoder_config(&self, config: Vec<u8>) {
        self.context.set_video_config_nals(config, CodecType::Hevc);
    }

    fn send_frame(&self, frame: OutFrame) {
        // ALVR's client decodes one stream of NALs per frame, so it needs one slice
        let mut slices = frame.slices.into_iter();
        if let Some(slice) = slices.next() {
            self.context.send_video_nal(Duration::from_nanos(frame.sample_ns), slice.data, frame.idr);
        }
    }

    fn frame_submitted(&self, sample_ns: u64) {
        let timestamp = Duration::from_nanos(sample_ns);
        self.context.report_present(timestamp, Duration::ZERO);
        self.context.report_composed(timestamp, Duration::ZERO);
    }

    fn bitrate_bps(&self) -> Option<u64> {
        self.context.get_dynamic_encoder_params().map(|p| p.bitrate_bps as u64)
    }

    fn send_haptics(&self, haptic: Haptic) {
        let device_id = if haptic.hand == 0 { *HAND_LEFT_ID } else { *HAND_RIGHT_ID };
        self.context.send_haptics(Haptics { device_id, duration: haptic.duration, frequency: haptic.frequency, amplitude: haptic.amplitude });
    }

    fn take_stats(&self) -> Option<Value> {
        latency::take()
    }
}
