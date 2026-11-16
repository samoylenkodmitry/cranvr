//! The time of each frame from the headset's pose to its display, split by stage, as the ALVR
//! client reports it. ALVR's server core turns each client report into a log event. This module
//! reads those events and keeps their sums until the stats line takes the averages.

use alvr_events::EventType;
use alvr_server_core::LOGGING_EVENTS_SENDER;
use serde_json::{json, Value};
use std::{sync::Mutex, thread};
use tokio::sync::broadcast::error::RecvError;

/// Stage sums in seconds, in the order of `STAGES`, and the client's frame rate.
#[derive(Default)]
struct Sums {
    frames: u32,
    stages: [f64; STAGES.len()],
    client_fps: f64,
    /// The video bitrate of each frame: its size over the frame interval, in bits per second
    bitrate: f64,
    peak_bitrate: f64,
}

/// The stages of a frame, in order. "total" is the whole time from the pose to the display.
const STAGES: [&str; 9] = [
    "total", "game", "compose", "encode", "network", "decode", "decode_queue", "client_compose", "vsync_wait",
];

static SUMS: Mutex<Sums> =
    Mutex::new(Sums { frames: 0, stages: [0.0; STAGES.len()], client_fps: 0.0, bitrate: 0.0, peak_bitrate: 0.0 });

/// Starts reading the client's reports. Call it after `init_logging`.
pub fn start() {
    let mut events = LOGGING_EVENTS_SENDER.subscribe();
    thread::Builder::new()
        .name("latency".into())
        .spawn(move || loop {
            let event = match events.blocking_recv() {
                Ok(event) => event,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            };
            let EventType::GraphStatistics(s) = event.event_type else { continue };
            let stages = [
                s.total_pipeline_latency_s,
                s.game_time_s,
                s.server_compositor_s,
                s.encoder_s,
                s.network_s,
                s.decoder_s,
                s.decoder_queue_s,
                s.client_compositor_s,
                s.vsync_queue_s,
            ];
            let mut sums = SUMS.lock().unwrap();
            sums.frames += 1;
            for (sum, stage) in sums.stages.iter_mut().zip(stages) {
                *sum += stage as f64;
            }
            sums.client_fps += s.client_fps as f64;
            sums.bitrate += s.bitrate_bps as f64;
            sums.peak_bitrate = sums.peak_bitrate.max(s.bitrate_bps as f64);
        })
        .expect("latency thread");
}

/// The average of each stage in milliseconds since the last call, the client's frame rate, and
/// the video bitrate: on average, and of the largest frame; `None` when no report came.
pub fn take() -> Option<Value> {
    let sums = std::mem::take(&mut *SUMS.lock().unwrap());
    if sums.frames == 0 {
        return None;
    }
    let n = sums.frames as f64;
    let mut value = json!({
        "frames": sums.frames,
        "client_fps": sums.client_fps / n,
        "video_mbps": sums.bitrate / n / 1e6,
        "peak_frame_mbps": sums.peak_bitrate / 1e6,
    });
    for (name, sum) in STAGES.iter().zip(sums.stages) {
        value[format!("{name}_ms")] = json!(sum * 1000.0 / n);
    }
    Some(value)
}

