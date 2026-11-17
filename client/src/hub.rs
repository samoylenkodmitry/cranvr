//! What the network threads and the UI share.
//!
//! Cranpose state belongs to the UI thread. The network and decoder threads write into plain
//! structures behind mutexes in this file. A `LaunchedEffect` of the UI copies a [`Snapshot`] into
//! Cranpose state a few times a second. The surface that the headset's compositor makes for the
//! stream also passes through here, from the UI thread to the decoder thread.

use crate::{
    connection::Connection,
    tracking::Feed,
};
use cranpose::ExternalSurface;
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Condvar, Mutex, OnceLock,
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
    /// The stream runs and no surface exists to decode into. This is the case in a flat window.
    NoSurface,
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

/// The surface of the stream and whether a decoder writes into it.
#[derive(Default)]
struct SurfaceSlot {
    surface: Option<ExternalSurface>,
    /// Counts each creation and destruction, so the decoder thread sees a change
    generation: u64,
    /// A decoder holds the surface
    in_use: bool,
}

/// How long the destruction of the surface waits for the decoder to let go of it.
const RELEASE_WAIT: Duration = Duration::from_secs(1);

pub struct Hub {
    snapshot: Mutex<Snapshot>,
    slot: Mutex<SurfaceSlot>,
    released: Condvar,
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
        slot: Mutex::new(SurfaceSlot::default()),
        released: Condvar::new(),
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

    /// The compositor made the surface for the stream. Called on the UI thread.
    pub fn surface_created(&self, surface: ExternalSurface) {
        let mut slot = self.slot.lock().unwrap();
        slot.surface = Some(surface);
        slot.generation += 1;
    }

    /// The surface goes away. The decoder must let go of it before this returns, so the call
    /// waits for the decoder thread, which sees the change within a few milliseconds.
    pub fn surface_destroyed(&self) {
        let mut slot = self.slot.lock().unwrap();
        slot.surface = None;
        slot.generation += 1;
        let (slot, timeout) = self.released.wait_timeout_while(slot, RELEASE_WAIT, |slot| slot.in_use).unwrap();
        if timeout.timed_out() {
            log::warn!("the decoder did not let go of the stream surface within {RELEASE_WAIT:?}");
        }
        drop(slot);
    }

    /// The current surface and its generation, for the decoder thread.
    pub(crate) fn surface(&self) -> (u64, Option<ExternalSurface>) {
        let slot = self.slot.lock().unwrap();
        (slot.generation, slot.surface.clone())
    }

    /// The decoder thread tells whether a decoder holds the surface.
    pub(crate) fn set_surface_in_use(&self, in_use: bool) {
        self.slot.lock().unwrap().in_use = in_use;
        if !in_use {
            self.released.notify_all();
        }
    }
}
