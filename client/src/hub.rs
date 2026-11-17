//! What the network threads and the UI share.
//!
//! Cranpose state belongs to the UI thread. The network and decoder threads write into plain
//! structures behind mutexes in this file. A `LaunchedEffect` of the UI copies a [`Snapshot`] into
//! Cranpose state a few times a second. The decoder's frames, the headset's properties and haptics
//! and the table of the streamed frames' views also pass through here.

use crate::{
    connection::Connection,
    tracking::Feed,
};
use cranpose::{CropRect, DecodedFrames};
use cranpose_xr::{FieldOfView, Haptics, HeadsetProperties, Pose as XrPose, StreamedView, StreamedViews};
use cranvr_proto::{
    clock::now_ns,
    wire::{Fov, Nanos, Pose},
};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    time::Duration,
};

/// Where the link stands.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkState {
    Idle,
    Connecting,
    Streaming,
    Failed(String),
}

/// The stream that the host chose.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamInfo {
    /// One eye's image in pixels
    pub eye: [u32; 2],
    /// The decoded picture: both eyes side by side
    pub picture: [u32; 2],
    pub codec: String,
    pub refresh_hz: f32,
    pub target_mbps: f32,
    pub udp: bool,
}

/// What the decoder does.
#[derive(Clone, Debug, PartialEq)]
pub enum DecoderView {
    /// No stream yet
    Idle,
    /// The stream runs and the app is not in a headset, so nothing decodes. This is the case in a
    /// flat window.
    NoHeadset,
    /// The decoder could not start
    Failed(String),
    Running,
}

/// The numbers that the stats panel shows. Rates cover the last stats interval. Counts run from
/// the start of the session.
#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    /// Frames that arrived whole, per second
    pub received_fps: f32,
    /// Frames that the decoder output, per second
    pub decoded_fps: f32,
    pub received_mbps: f32,
    pub decoder: DecoderView,
    pub decode_last_ms: f32,
    pub decode_mean_ms: f32,
    pub decode_max_ms: f32,
    /// Frames that the decoder dropped or refused
    pub decoder_dropped: u64,
    pub packets_expected: u64,
    pub packets_received: u64,
    pub units_complete: u64,
    /// Units that needed the parity packets to complete: the FEC recoveries
    pub units_recovered: u64,
    pub units_lost: u64,
    pub packets_recovered: u64,
    pub key_requests: u64,
    /// The round trip of the clock pings
    pub rtt_ms: Option<f32>,
    /// The host's start of a send to the last packet's arrival, averaged
    pub network_ms: Option<f32>,
    /// The tracking sample to the end of the decode, averaged
    pub sample_to_decode_ms: Option<f32>,
    pub haptic_pulses: u64,
    /// Frames that the headset took for its display, per second
    pub shown_fps: f32,
    /// The headset's own frames a second. `headset_gap_ms` is the longest gap between two of them
    /// in a stats interval
    pub headset_fps: f32,
    pub headset_gap_ms: f32,
    /// The newest head pose that the headset reported: x, y, z in meters
    pub head_position: Option<[f32; 3]>,
    /// Of the frames that the headset took for its display, the share that showed at the display
    /// time that the host aimed for, the share that showed after it and the share that showed
    /// before it, in percent. Frames without a target do not count.
    pub at_target_pct: f32,
    pub late_pct: f32,
    pub early_pct: f32,
    /// Whether the poses come from the headset or are placeholders (see `tracking.rs`)
    pub tracking_live: bool,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            received_fps: 0.0,
            decoded_fps: 0.0,
            received_mbps: 0.0,
            decoder: DecoderView::Idle,
            decode_last_ms: 0.0,
            decode_mean_ms: 0.0,
            decode_max_ms: 0.0,
            decoder_dropped: 0,
            packets_expected: 0,
            packets_received: 0,
            units_complete: 0,
            units_recovered: 0,
            units_lost: 0,
            packets_recovered: 0,
            key_requests: 0,
            rtt_ms: None,
            network_ms: None,
            sample_to_decode_ms: None,
            haptic_pulses: 0,
            shown_fps: 0.0,
            headset_fps: 0.0,
            headset_gap_ms: 0.0,
            head_position: None,
            at_target_pct: 0.0,
            late_pct: 0.0,
            early_pct: 0.0,
            tracking_live: false,
        }
    }
}

/// The picture that the UI shows of the link.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub state: LinkState,
    pub stream: Option<StreamInfo>,
    pub stats: Stats,
}

impl Snapshot {
    fn idle() -> Self {
        Self { state: LinkState::Idle, stream: None, stats: Stats::default() }
    }
}

/// How the media reaches the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaChoice {
    /// TCP for a loopback address, which is a USB link through `adb reverse`, and UDP otherwise
    Auto,
    Udp,
    Tcp,
}

/// The decoded frames of the running session and a counter that changes with them
#[derive(Default)]
struct StreamSlot {
    frames: Option<DecodedFrames>,
    /// Counts each change, so the UI sees a new decoder
    generation: u64,
}

/// What the headset reports once for the session
#[derive(Clone)]
pub struct HeadsetInfo {
    pub properties: HeadsetProperties,
    pub haptics: Option<Haptics>,
}

/// A frame that went into the decoder, with what the headset needs to show and time it.
#[derive(Clone, Copy, Debug)]
pub struct Sent {
    /// The decoder returns this number with the frame
    pub metadata: u64,
    pub views: StreamedViews,
    pub frame: u32,
    pub tracking_sample: Nanos,
    /// The last packet of the frame arrived
    pub received: Nanos,
    /// The frame went into the decoder
    pub submitted: Nanos,
    /// The display time that the host aimed for, on the client's clock. 0 when the host gave none.
    pub target_display: Nanos,
}

/// A frame that the headset took for its display.
#[derive(Clone, Copy, Debug)]
pub struct Shown {
    pub sent: Sent,
    /// The decoder's time for the newest picture when the headset took it
    pub decode_time: Duration,
    /// When the headset's display shows the frame, on the client's clock
    pub display_time: Nanos,
    /// The time between two display frames
    pub period: Nanos,
    /// When the compositor took the frame, on the client's clock
    pub picked: Nanos,
}

/// The frames in the decoder and the frames that the headset took, in the order of arrival.
#[derive(Default)]
struct StreamTable {
    sent: VecDeque<Sent>,
    shown: VecDeque<Shown>,
}

/// The frames that the table keeps. The decoder holds a few, so this is far more than needed.
const TABLE_LEN: usize = 128;

pub struct Hub {
    snapshot: Mutex<Snapshot>,
    headset: Mutex<Option<HeadsetInfo>>,
    stream: Mutex<StreamSlot>,
    table: Mutex<StreamTable>,
    /// The numbers that frames carry through the decoder. They never repeat in a process, so a
    /// frame of an earlier session finds no entry in the table of a later one.
    next_metadata: AtomicU64,
    pub feed: Feed,
    /// The id of the connection whose reports count. 0 means none.
    current: AtomicU64,
    last_id: AtomicU64,
    connection: Mutex<Option<Connection>>,
}

/// The one hub of the process. The activity may rebuild the composition, and the link outlives it.
pub fn hub() -> &'static Hub {
    static HUB: OnceLock<Hub> = OnceLock::new();
    HUB.get_or_init(|| Hub {
        snapshot: Mutex::new(Snapshot::idle()),
        headset: Mutex::new(None),
        stream: Mutex::new(StreamSlot::default()),
        table: Mutex::new(StreamTable::default()),
        next_metadata: AtomicU64::new(1),
        feed: Feed::default(),
        current: AtomicU64::new(0),
        last_id: AtomicU64::new(0),
        connection: Mutex::new(None),
    })
}

impl Hub {
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().unwrap().clone()
    }

    /// Starts a connection to `host`, an address with an optional port. An earlier connection ends
    /// first.
    pub fn connect(&'static self, host: String, media: MediaChoice) {
        self.disconnect();
        let id = self.last_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.current.store(id, Ordering::Release);
        *self.snapshot.lock().unwrap() = Snapshot { state: LinkState::Connecting, ..Snapshot::idle() };
        *self.connection.lock().unwrap() = Some(Connection::start(self, id, host, media));
    }

    /// Ends the connection. Its threads end in the background.
    pub fn disconnect(&self) {
        self.current.store(0, Ordering::Release);
        if let Some(connection) = self.connection.lock().unwrap().take() {
            connection.stop();
        }
        *self.snapshot.lock().unwrap() = Snapshot::idle();
    }

    /// Lets the connection `id` change the snapshot. A connection that was replaced or ended by the
    /// user changes nothing.
    pub fn report(&self, id: u64, update: impl FnOnce(&mut Snapshot)) {
        if self.current.load(Ordering::Acquire) == id {
            update(&mut self.snapshot.lock().unwrap());
        }
    }

    /// Tells the hub what the headset is. Both are `None` outside a headset. Called on the UI
    /// thread when the app composes.
    pub fn attach(&self, properties: Option<HeadsetProperties>, haptics: Option<Haptics>) {
        *self.headset.lock().unwrap() = properties.map(|properties| HeadsetInfo { properties, haptics });
    }

    pub fn headset(&self) -> Option<HeadsetInfo> {
        self.headset.lock().unwrap().clone()
    }

    /// Hands the decoder's frames to the UI, which gives them to the streamed scene.
    pub(crate) fn set_stream(&self, frames: DecodedFrames) {
        let mut slot = self.stream.lock().unwrap();
        slot.frames = Some(frames);
        slot.generation += 1;
    }

    /// Takes the frames of a decoder back from the UI. A newer decoder may have replaced them
    /// already, as when a new session starts while the last one ends. Its frames then stay.
    pub(crate) fn clear_stream(&self, frames: &DecodedFrames) {
        let mut slot = self.stream.lock().unwrap();
        if slot.frames.as_ref() == Some(frames) {
            slot.frames = None;
            slot.generation += 1;
        }
    }

    /// The decoder's frames and the count of changes
    pub fn stream(&self) -> (u64, Option<DecodedFrames>) {
        let slot = self.stream.lock().unwrap();
        (slot.generation, slot.frames.clone())
    }

    /// A number for the next frame that goes into a decoder
    pub(crate) fn new_metadata(&self) -> u64 {
        self.next_metadata.fetch_add(1, Ordering::Relaxed)
    }

    /// Notes a frame that goes into the decoder, before the decoder can return it.
    pub(crate) fn frame_sent(&self, sent: Sent) {
        let mut table = self.table.lock().unwrap();
        if table.sent.len() >= TABLE_LEN {
            table.sent.pop_front();
        }
        table.sent.push_back(sent);
    }

    /// The views of the frame that carries `metadata`. The headset's frame loop calls this once
    /// for each frame that it takes for its display, so the call also says that the frame reaches
    /// the display at the display time of the frame that is in work. `frames` are the decoder's.
    pub fn views_of(&self, frames: &DecodedFrames, metadata: u64) -> StreamedViews {
        let mut table = self.table.lock().unwrap();
        let Some(sent) = table.sent.iter().rev().find(|sent| sent.metadata == metadata).copied() else {
            // A frame that the table lost has views that the headset refuses and logs
            return StreamedViews { left: NO_VIEW, right: NO_VIEW };
        };
        if let Some(display) = self.feed.display() {
            if table.shown.len() >= TABLE_LEN {
                table.shown.pop_front();
            }
            table.shown.push_back(Shown {
                sent,
                decode_time: frames.stats().last_decode,
                display_time: display.time,
                period: display.period,
                picked: now_ns(),
            });
        }
        sent.views
    }

    /// The frames that the headset took since the last call
    pub(crate) fn take_shown(&self) -> Vec<Shown> {
        self.table.lock().unwrap().shown.drain(..).collect()
    }
}

/// A view that the headset refuses, because its picture has no area
const NO_VIEW: StreamedView = StreamedView {
    pose: XrPose::IDENTITY,
    fov: FieldOfView { angle_left: 0.0, angle_right: 0.0, angle_up: 0.0, angle_down: 0.0 },
    rect: CropRect { x: 0, y: 0, width: 0, height: 0 },
};

/// The views of a frame from the render poses that the host sent with it. `eye` is one eye's
/// image size in pixels. The two eyes lie side by side in the picture. `fov` is what the host
/// renders with. The poses are in the headset's room.
pub fn streamed_views(eye_poses: &[Pose; 2], fov: &[Fov; 2], eye: [u32; 2]) -> StreamedViews {
    let view = |i: usize| StreamedView {
        pose: XrPose { position: eye_poses[i].position, orientation: eye_poses[i].orientation },
        fov: FieldOfView { angle_left: fov[i].left, angle_right: fov[i].right, angle_up: fov[i].up, angle_down: fov[i].down },
        rect: CropRect { x: eye[0] * i as u32, y: 0, width: eye[0], height: eye[1] },
    };
    StreamedViews { left: view(0), right: view(1) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_eyes_lie_side_by_side_in_the_picture() {
        let poses = [Pose { position: [-0.03, 1.0, 0.0], ..Pose::IDENTITY }, Pose { position: [0.03, 1.0, 0.0], ..Pose::IDENTITY }];
        let fov = Fov { left: -0.8, right: 0.7, up: 0.75, down: -0.9 };
        let views = streamed_views(&poses, &[fov, fov], [1248, 1306]);
        assert_eq!(views.left.rect, CropRect { x: 0, y: 0, width: 1248, height: 1306 });
        assert_eq!(views.right.rect, CropRect { x: 1248, y: 0, width: 1248, height: 1306 });
        assert_eq!(views.right.pose.position, [0.03, 1.0, 0.0]);
        assert_eq!((views.left.fov.angle_left, views.left.fov.angle_down), (-0.8, -0.9));
    }
}
