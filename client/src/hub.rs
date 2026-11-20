//! What the network threads and the UI share.
//!
//! Cranpose state belongs to the UI thread. The network and decoder threads write into plain
//! structures behind mutexes in this file. A `LaunchedEffect` of the UI copies a [`Snapshot`] into
//! Cranpose state a few times a second. The decoder's frames, the headset's properties and haptics
//! and the table of the streamed frames' views also pass through here.

use crate::{
    audio,
    connection::Connection,
    console::{Action, Backdrop, Console, Menu, Room, View as ConsoleView, Volume},
    desktop::desktop,
    discovery::FoundHost,
    settings,
    tracking::{Display, Feed},
    usb::{Path, UsbMac},
};
use cranpose::{CropRect, DecodedFrames};
use cranpose_xr::{
    FieldOfView, FoveatedEncoding, Haptics, HeadsetProperties, Pose as XrPose, StreamStats, StreamedView, StreamedViews,
};
use cranvr_common::foveation::{Foveation, FoveationConfig};
use cranvr_proto::{
    clock::now_ns,
    message::{FoveationParams, Focus},
    wire::{Fov, Nanos, Pose},
};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

/// Where the link stands.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkState {
    Idle,
    Connecting,
    /// The Mac does not know this headset yet. Its user compares the code that both show and allows
    /// the headset there. `host` is the name that the Mac gave itself.
    Waiting { code: String, host: String },
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
    /// Whether the host foveates its pictures: the decoded frame holds the compressed layout
    pub foveated: bool,
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
    /// The tracking sample to the display time of the headset frame that shows the frame,
    /// averaged. The streamed scene gives the display time when it takes the frame.
    pub sample_to_display_ms: Option<f32>,
    pub haptic_pulses: u64,
    /// What the streamed scene did with the decoded frames, per second: frames that it showed,
    /// frames that it never showed. The last is headset frames that showed the last picture again
    pub shown_fps: f32,
    pub skipped_fps: f32,
    pub repeated_fps: f32,
    /// The headset's own frames a second. `headset_gap_ms` is the longest gap between two of them
    /// in a stats interval
    pub headset_fps: f32,
    pub headset_gap_ms: f32,
    /// The newest head pose that the headset reported: x, y, z in meters
    pub head_position: Option<[f32; 3]>,
    /// The newest head orientation that the client sent (x, y, z, w). `rendered_orientation` is the
    /// orientation of the left eye in the newest frame that arrived. The host rendered that frame
    /// for it. When a phone lies on a desk, the two values show that the host takes the tracking.
    /// The frames follow it.
    pub head_orientation: Option<[f32; 4]>,
    pub rendered_orientation: Option<[f32; 4]>,
    /// The share of the frames with a target that came after the headset frame of their own point,
    /// in percent
    pub late_pct: f32,
    /// Whether the poses come from the headset or are placeholders (see `tracking.rs`)
    pub tracking_live: bool,
    /// What opening the sealed packets cost in the last stats interval. `open_us_per_frame` counts
    /// every packet, so a desktop panel and the sound add to it. `open_us_per_packet` is the mean
    /// for one packet. `open_core_pct` is the share of one core in percent.
    pub open_us_per_frame: f32,
    pub open_us_per_packet: f32,
    pub open_core_pct: f32,
    /// Packets that failed to open since the session began: changed, replayed or too old
    pub packets_rejected: u64,
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
            sample_to_display_ms: None,
            haptic_pulses: 0,
            shown_fps: 0.0,
            skipped_fps: 0.0,
            repeated_fps: 0.0,
            headset_fps: 0.0,
            headset_gap_ms: 0.0,
            head_position: None,
            head_orientation: None,
            rendered_orientation: None,
            late_pct: 0.0,
            tracking_live: false,
            open_us_per_frame: 0.0,
            open_us_per_packet: 0.0,
            open_core_pct: 0.0,
            packets_rejected: 0,
        }
    }
}

/// The picture that the UI shows of the link.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub state: LinkState,
    pub stream: Option<StreamInfo>,
    pub stats: Stats,
    /// The Macs that answered the search (see `discovery.rs`). `Hub::snapshot` fills it.
    pub found: Vec<FoundHost>,
    /// The Mac that answers behind the USB cable (see `usb.rs`). `Hub::snapshot` fills it.
    pub usb: Option<UsbMac>,
    /// The way that the link goes, once a connection has chosen it
    pub path: Option<Path>,
    /// How much of the headset the app has (see `focus.rs`). `Hub::snapshot` fills it.
    pub focus: Focus,
}

impl Snapshot {
    fn idle() -> Self {
        Self { state: LinkState::Idle, stream: None, stats: Stats::default(), found: Vec::new(), usb: None, path: None, focus: Focus::Focused }
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

/// What a phone or a tablet reports about its screen. The Hello gives the host the screen's
/// shape and the rate at which the screen refreshes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhoneInfo {
    /// The screen's size in pixels with the long side first: the app shows the picture in landscape
    pub screen: [u32; 2],
    /// The screen's refresh rate in Hz, measured from the frame clock
    pub refresh_hz: f32,
}

/// What the client runs on. It decides where the head comes from and whether anything decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    /// A flat window on a computer, for development: no sensors and no decoder
    Window,
    /// A headset: the runtime gives the head pose and the controllers
    Headset,
    /// A phone or a tablet: the screen shows the picture flat and the motion sensors give the head
    Phone,
}

impl Device {
    /// Whether the device gives a head pose of its own for each frame, decodes the host's video and
    /// plays its sound. A window does none of this: the sender makes placeholder samples there.
    pub fn is_live(self) -> bool {
        self != Device::Window
    }
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

/// A frame that the headset's frame loop took out of the decoder. The streamed scene keeps up to
/// three taken frames in a queue and shows the one whose target lies closest to the display time.
/// The app is told when a frame enters the queue and not when it shows, so the display time is the
/// one that the frame will have if it is not late for its target.
#[derive(Clone, Copy, Debug)]
pub struct Shown {
    pub sent: Sent,
    /// The decoder's time for the newest picture when the headset took the frame
    pub decode_time: Duration,
    /// When the headset's display shows the frame: the display time closest to its target, but not
    /// before the display time of the headset frame that took it. A frame with no target shows at
    /// the display time of that headset frame.
    pub display_time: Nanos,
    /// The time between two display frames
    pub period: Nanos,
    /// When the frame loop took the frame, or would have taken it for its own point if the frame
    /// came early: the display time minus the time from the take to the display of the headset
    /// frame
    pub picked: Nanos,
    /// Whether the frame came after the headset frame of its own point
    pub late: bool,
}

/// The frames in the decoder and the frames that the headset took, in the order of arrival.
#[derive(Default)]
struct StreamTable {
    sent: VecDeque<Sent>,
    shown: VecDeque<Shown>,
}

/// The frames that the table keeps. The decoder holds a few, so this is far more than needed.
const TABLE_LEN: usize = 128;

/// How long a link that streamed and then ended waits before the client connects again. The host
/// restarts when the console changes a setting, which takes a few seconds.
const RECONNECT_AFTER: Duration = Duration::from_secs(2);

/// The connection that the user asked for. The client keeps it up: a link that streamed and ended
/// without the user's say connects again. A connection that never streamed stays failed, so the
/// panel shows why.
struct Wanted {
    /// The address that the user pressed or typed. Each connection chooses its own way to the
    /// Mac behind it (see `usb::target`).
    host: String,
    /// The start of the host's key fingerprint, when the host came from the search or an earlier
    /// connection reached it
    fingerprint: Option<[u8; 8]>,
    media: MediaChoice,
    streamed: bool,
    failed_at: Option<Instant>,
}

impl Wanted {
    /// Forgets that the link streamed, so a failure stays on screen and no new try follows.
    fn give_up(&mut self) {
        self.streamed = false;
        self.failed_at = None;
    }

    /// Notes the link's state and says whether the client should connect again at `now`.
    fn due(&mut self, state: &LinkState, now: Instant) -> bool {
        match state {
            LinkState::Streaming => {
                self.streamed = true;
                self.failed_at = None;
                false
            }
            LinkState::Failed(_) if self.streamed => now.duration_since(*self.failed_at.get_or_insert(now)) >= RECONNECT_AFTER,
            LinkState::Failed(_) | LinkState::Idle | LinkState::Connecting | LinkState::Waiting { .. } => false,
        }
    }
}

pub struct Hub {
    snapshot: Mutex<Snapshot>,
    headset: Mutex<Option<HeadsetInfo>>,
    /// Whether the app is the phone's. The phone's UI says so when it composes.
    is_phone: AtomicBool,
    phone: Mutex<Option<PhoneInfo>>,
    /// The size of the window in pixels, which the phone's UI reads from its layout
    screen: Mutex<Option<[u32; 2]>>,
    stream: Mutex<StreamSlot>,
    table: Mutex<StreamTable>,
    /// The numbers that frames carry through the decoder. They never repeat in a process, so a
    /// frame of an earlier session finds no entry in the table of a later one.
    next_metadata: AtomicU64,
    pub feed: Feed,
    /// What the streamed scene did with the frames: shown, skipped and repeated
    pub stream_stats: StreamStats,
    /// The id of the connection whose reports count. 0 means none.
    current: AtomicU64,
    last_id: AtomicU64,
    connection: Mutex<Option<Connection>>,
    wanted: Mutex<Option<Wanted>>,
    /// The Macs that answered the search
    found: Mutex<Vec<FoundHost>>,
    /// The Mac that answers behind the USB cable
    usb: Mutex<Option<UsbMac>>,
    console: Mutex<Console>,
    /// The saved settings of the console were loaded. A rebuilt composition must not load them
    /// again, because the choices in memory are newer when a save failed.
    settings_loaded: AtomicBool,
}

/// The one hub of the process. The activity may rebuild the composition, and the link outlives it.
pub fn hub() -> &'static Hub {
    static HUB: OnceLock<Hub> = OnceLock::new();
    HUB.get_or_init(|| Hub {
        snapshot: Mutex::new(Snapshot::idle()),
        headset: Mutex::new(None),
        is_phone: AtomicBool::new(false),
        phone: Mutex::new(None),
        screen: Mutex::new(None),
        stream: Mutex::new(StreamSlot::default()),
        table: Mutex::new(StreamTable::default()),
        next_metadata: AtomicU64::new(1),
        feed: Feed::default(),
        stream_stats: StreamStats::new(),
        current: AtomicU64::new(0),
        last_id: AtomicU64::new(0),
        connection: Mutex::new(None),
        wanted: Mutex::new(None),
        found: Mutex::new(Vec::new()),
        usb: Mutex::new(None),
        console: Mutex::new(Console::default()),
        settings_loaded: AtomicBool::new(false),
    })
}

impl Hub {
    pub fn snapshot(&self) -> Snapshot {
        let mut snapshot = self.snapshot.lock().unwrap().clone();
        snapshot.found = self.found.lock().unwrap().clone();
        snapshot.usb = self.usb.lock().unwrap().clone();
        snapshot.focus = if self.device().is_live() { crate::focus::current() } else { Focus::Focused };
        snapshot
    }

    /// The Macs that answered the search
    pub fn found(&self) -> Vec<FoundHost> {
        self.found.lock().unwrap().clone()
    }

    /// Notes the Mac that answers behind the USB cable, or that none does. The cable's thread
    /// calls it.
    pub fn set_usb(&self, usb: Option<UsbMac>) {
        *self.usb.lock().unwrap() = usb;
    }

    pub fn usb(&self) -> Option<UsbMac> {
        self.usb.lock().unwrap().clone()
    }

    /// Notes the Macs that answered the search. The search thread calls it after each round.
    pub fn set_found(&self, found: Vec<FoundHost>) {
        *self.found.lock().unwrap() = found;
    }

    /// Whether the search should ask the network: no link runs and none is being made.
    pub fn is_searching(&self) -> bool {
        matches!(self.snapshot.lock().unwrap().state, LinkState::Idle | LinkState::Failed(_))
    }

    /// Starts a connection to `host`, an address with an optional port. `fingerprint` is the start
    /// of the host's key fingerprint when the host came from the search. An earlier connection ends
    /// first.
    pub fn connect(&'static self, host: String, fingerprint: Option<[u8; 8]>, media: MediaChoice) {
        self.start(host, fingerprint, media, false);
    }

    fn start(&'static self, host: String, fingerprint: Option<[u8; 8]>, media: MediaChoice, streamed: bool) {
        self.end_connection();
        let id = self.last_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.current.store(id, Ordering::Release);
        *self.snapshot.lock().unwrap() = Snapshot { state: LinkState::Connecting, ..Snapshot::idle() };
        *self.wanted.lock().unwrap() = Some(Wanted { host: host.clone(), fingerprint, media, streamed, failed_at: None });
        *self.connection.lock().unwrap() = Some(Connection::start(self, id, host, fingerprint, media));
    }

    /// Ends the connection, because the user asked. Its threads end in the background.
    pub fn disconnect(&self) {
        *self.wanted.lock().unwrap() = None;
        self.end_connection();
    }

    fn end_connection(&self) {
        self.current.store(0, Ordering::Release);
        if let Some(connection) = self.connection.lock().unwrap().take() {
            connection.stop();
        }
        self.console.lock().unwrap().clear();
        *self.snapshot.lock().unwrap() = Snapshot::idle();
    }

    /// Connects again when a link that streamed has ended and a pause has passed, unless the user
    /// ended the connection. The UI calls it a few times a second.
    pub fn reconnect_if_due(&'static self) {
        let state = self.snapshot.lock().unwrap().state.clone();
        let again = {
            let mut wanted = self.wanted.lock().unwrap();
            wanted
                .as_mut()
                .and_then(|wanted| wanted.due(&state, Instant::now()).then(|| (wanted.host.clone(), wanted.fingerprint, wanted.media)))
        };
        if let Some((host, fingerprint, media)) = again {
            log::info!("the link ended: connecting to {host} again");
            self.start(host, fingerprint, media, true);
        }
    }

    /// Lets the connection `id` change the snapshot. A connection that was replaced or ended by the
    /// user changes nothing.
    pub fn report(&self, id: u64, update: impl FnOnce(&mut Snapshot)) {
        if self.current.load(Ordering::Acquire) != id {
            return;
        }
        let streaming = {
            let mut snapshot = self.snapshot.lock().unwrap();
            update(&mut snapshot);
            snapshot.state == LinkState::Streaming
        };
        // A link that streamed for any time counts, also when the UI never saw the state
        if let Some(wanted) = self.wanted.lock().unwrap().as_mut().filter(|_| streaming) {
            wanted.streamed = true;
        }
    }

    /// The connection `id` met a failure that ends only when the user acts: the Mac's user said no
    /// or did not answer, the Mac's key changed, or the apps speak no common version. It stops
    /// retrying. A cable that fell back to Wi-Fi would otherwise ask the Mac's user again every few
    /// seconds.
    pub(crate) fn give_up(&self, id: u64) {
        if self.current.load(Ordering::Acquire) != id {
            return;
        }
        if let Some(wanted) = self.wanted.lock().unwrap().as_mut() {
            wanted.give_up();
        }
    }

    /// Notes which Mac the connection `id` reached. A connection to a typed address learns it
    /// there, so a later connection can tell the Mac's cable from its Wi-Fi.
    pub(crate) fn learn_mac(&self, id: u64, fingerprint: [u8; 8]) {
        if self.current.load(Ordering::Acquire) != id {
            return;
        }
        if let Some(wanted) = self.wanted.lock().unwrap().as_mut() {
            wanted.fingerprint.get_or_insert(fingerprint);
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

    /// Tells the hub that the app is the phone's: a screen with motion sensors and no headset.
    /// Called on the UI thread when the app composes, before any connection starts.
    pub fn attach_phone(&self) {
        self.is_phone.store(true, Ordering::Release);
    }

    /// Notes the phone's screen and its refresh rate. The phone's frame loop calls it once the
    /// rate is known and again when either value changes.
    pub fn set_phone_info(&self, info: PhoneInfo) {
        *self.phone.lock().unwrap() = Some(info);
    }

    pub fn phone(&self) -> Option<PhoneInfo> {
        *self.phone.lock().unwrap()
    }

    /// Notes the size of the phone's window in pixels. The layout knows it. The frame loop reads it
    /// for the views that the Hello carries.
    pub fn set_screen(&self, size: [u32; 2]) {
        *self.screen.lock().unwrap() = Some(size);
    }

    pub fn screen(&self) -> Option<[u32; 2]> {
        *self.screen.lock().unwrap()
    }

    /// What the app runs on
    pub fn device(&self) -> Device {
        if self.headset.lock().unwrap().is_some() {
            Device::Headset
        } else if self.is_phone.load(Ordering::Acquire) {
            Device::Phone
        } else {
            Device::Window
        }
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
    /// for each decoded frame that it takes into its queue, at the start of a headset frame.
    /// `frames` are the decoder's.
    ///
    /// The views carry the host's target display time on the runtime's clock, so the scene shows
    /// the frame at its point. The call also gives the frame's timing: the frame loop took the
    /// frame now. The headset frame that does so reaches the display `lead` after this moment.
    pub fn views_of(&self, frames: &DecodedFrames, metadata: u64) -> StreamedViews {
        let mut table = self.table.lock().unwrap();
        let Some(sent) = table.sent.iter().rev().find(|sent| sent.metadata == metadata).copied() else {
            // A frame that the table lost has views that the headset refuses and logs
            return StreamedViews { left: NO_VIEW, right: NO_VIEW, target_display_time: None };
        };
        let mut views = sent.views;
        // A phone's scene shows the newest frame, so it has no target. The connection reports the
        // phone's timing when the frame arrives (see `connection.rs`).
        if self.device() == Device::Phone {
            return views;
        }
        let Some(display) = self.feed.display() else { return views };
        if sent.target_display != 0 {
            views.target_display_time = self.feed.to_runtime_time(sent.target_display);
        }
        let intake = now_ns();
        let timing = intake_timing(display, intake, sent.target_display);
        if table.shown.len() >= TABLE_LEN {
            table.shown.pop_front();
        }
        table.shown.push_back(Shown {
            sent,
            decode_time: frames.stats().last_decode,
            display_time: timing.display_time,
            period: display.period,
            picked: timing.picked,
            late: timing.late,
        });
        views
    }

    /// The console's menu from the host, for the connection `id`.
    pub(crate) fn set_menu(&self, id: u64, text: &str) {
        if self.current.load(Ordering::Acquire) != id {
            return;
        }
        match Menu::parse(text) {
            Some(menu) => self.console.lock().unwrap().on_menu(menu),
            None => log::warn!("the host sent a menu that the console cannot read"),
        }
    }

    pub fn console(&self) -> ConsoleView {
        self.console.lock().unwrap().view()
    }

    /// One frame of the headset for the console: whether the left menu button is down. Returns
    /// whether a hold of the button showed or hid the console.
    pub fn console_frame(&self, menu_down: bool, now: Nanos) -> bool {
        self.console.lock().unwrap().on_frame(menu_down, now)
    }

    pub fn close_console(&self) {
        self.console.lock().unwrap().close();
    }

    /// Opens the console and keeps it open when the link ends (see `settings::console_on_launch`).
    pub fn pin_console(&self) {
        self.console.lock().unwrap().pin();
    }

    /// What decides the room behind the panels: the user's choice and whether a game runs.
    pub fn backdrop(&self) -> Backdrop {
        self.console.lock().unwrap().backdrop(desktop().game_hides())
    }

    /// Takes the saved choices of the console's Room and Volume settings and applies them. The
    /// headset's app calls it when it composes, before a link can start. Only the first call in
    /// the process loads.
    pub fn load_settings(&self) {
        if self.settings_loaded.swap(true, Ordering::AcqRel) {
            return;
        }
        let (room, volume) = (settings::room(), settings::volume());
        {
            let mut console = self.console.lock().unwrap();
            console.set_room(room);
            console.set_volume(volume);
        }
        audio::set_gain(volume.gain());
    }

    /// The user chose what shows behind the panels while no game runs.
    pub fn choose_room(&self, room: Room) {
        log::info!("the console sets the room to {}", room.key());
        self.console.lock().unwrap().set_room(room);
        settings::remember_room(room);
    }

    /// The user chose how loud the sound plays.
    pub fn choose_volume(&self, volume: Volume) {
        self.console.lock().unwrap().set_volume(volume);
        audio::set_gain(volume.gain());
        settings::remember_volume(volume);
    }

    /// Sends a press of a console button to the host.
    pub fn press(&self, action: &Action) {
        log::info!("the console asks for {}", action.request());
        if let Some(connection) = self.connection.lock().unwrap().as_ref() {
            connection.send_console(action.request());
        }
    }

    /// The frames that the headset took since the last call
    pub(crate) fn take_shown(&self) -> Vec<Shown> {
        self.table.lock().unwrap().shown.drain(..).collect()
    }
}

/// The timing of a frame that the frame loop took at `intake`, in the headset frame whose display
/// time is `display.time`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IntakeTiming {
    display_time: Nanos,
    picked: Nanos,
    late: bool,
}

fn intake_timing(display: Display, intake: Nanos, target: Nanos) -> IntakeTiming {
    // The headset frame reaches the display this long after the take
    let lead = display.time.saturating_sub(intake);
    let period = display.period.max(1);
    let (display_time, late) = if target == 0 {
        (display.time, false)
    } else {
        // The display time closest to the target, on the grid of the headset's display times
        let steps = ((target as f64 - display.time as f64) / period as f64).round();
        let point = (display.time as f64 + steps * period as f64).max(0.0) as Nanos;
        // A frame whose point lies before this headset frame is late. It shows in this frame or
        // later.
        (point.max(display.time), point < display.time)
    };
    IntakeTiming { display_time, picked: display_time.saturating_sub(lead), late }
}

/// A view that the headset refuses, because its picture has no area
const NO_VIEW: StreamedView = StreamedView {
    pose: XrPose::IDENTITY,
    fov: FieldOfView { angle_left: 0.0, angle_right: 0.0, angle_up: 0.0, angle_down: 0.0 },
    rect: CropRect { x: 0, y: 0, width: 0, height: 0 },
    foveation: None,
};

/// How the host's pictures lie in the decoded frame: the size of one eye's full view and, when the
/// host foveates, the layout that its encoder derived from the parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StreamLayout {
    /// One eye's full view in pixels
    pub eye: [u32; 2],
    pub foveation: Option<Foveation>,
}

impl StreamLayout {
    /// The layout of a stream with an eye of `eye` pixels and the foveation that the host chose.
    /// The host and the client derive the aligned sizes from the same parameters.
    pub fn new(eye: [u32; 2], foveation: Option<FoveationParams>) -> Self {
        Self { eye, foveation: foveation.map(|params| Foveation::new(eye, &FoveationConfig::from(params))) }
    }

    /// The size of one eye's picture in the frame: the encoded eye with its padding, or the eye
    pub fn eye_picture(&self) -> [u32; 2] {
        self.foveation.map_or(self.eye, |f| f.optimized)
    }

    /// The size of the decoded frame: both eyes side by side
    pub fn frame(&self) -> [u32; 2] {
        let [w, h] = self.eye_picture();
        [w * 2, h]
    }
}

/// The views of a frame from the render poses that the host sent with it. The two eyes lie side by
/// side in the picture. A foveated picture has the host's compressed layout. The right eye is
/// mirrored, as the foveated layout stores it. `fov` is the field of view of the full view that the host rendered.
/// The poses are in the headset's room. The target display time comes later (see `views_of`).
pub fn streamed_views(eye_poses: &[Pose; 2], fov: &[Fov; 2], layout: &StreamLayout) -> StreamedViews {
    let [width, height] = layout.eye_picture();
    let view = |i: usize| StreamedView {
        pose: XrPose { position: eye_poses[i].position, orientation: eye_poses[i].orientation },
        fov: FieldOfView { angle_left: fov[i].left, angle_right: fov[i].right, angle_up: fov[i].up, angle_down: fov[i].down },
        rect: CropRect { x: width * i as u32, y: 0, width, height },
        foveation: layout.foveation.map(|f| FoveatedEncoding {
            full_size: (layout.eye[0], layout.eye[1]),
            center_size: f.center_size,
            center_shift: f.center_shift,
            edge_ratio: f.edge_ratio,
            eye_size_ratio: f.eye_size_ratio,
            mirrored: i == 1,
        }),
    };
    StreamedViews { left: view(0), right: view(1), target_display_time: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fov() -> [Fov; 2] {
        let fov = Fov { left: -0.8, right: 0.7, up: 0.75, down: -0.9 };
        [fov, fov]
    }

    fn poses() -> [Pose; 2] {
        [Pose { position: [-0.03, 1.0, 0.0], ..Pose::IDENTITY }, Pose { position: [0.03, 1.0, 0.0], ..Pose::IDENTITY }]
    }

    #[test]
    fn the_two_eyes_lie_side_by_side_in_the_picture() {
        let layout = StreamLayout::new([1248, 1306], None);
        let views = streamed_views(&poses(), &fov(), &layout);
        assert_eq!(views.left.rect, CropRect { x: 0, y: 0, width: 1248, height: 1306 });
        assert_eq!(views.right.rect, CropRect { x: 1248, y: 0, width: 1248, height: 1306 });
        assert_eq!(views.right.pose.position, [0.03, 1.0, 0.0]);
        assert_eq!((views.left.fov.angle_left, views.left.fov.angle_down), (-0.8, -0.9));
        assert!(views.left.foveation.is_none() && views.target_display_time.is_none());
        assert_eq!(layout.frame(), [2496, 1306]);
    }

    #[test]
    fn a_foveated_eye_has_the_layout_that_the_hosts_encoder_aligned() {
        // The aligned values of an eye of 640 by 720 pixels: Cranpose's demo clip holds them
        let params = FoveationParams { center_size: [0.45, 0.4], center_shift: [0.4, 0.1], edge_ratio: [4.0, 5.0] };
        let layout = StreamLayout::new([640, 720], Some(params));
        assert_eq!(layout.eye_picture(), [384, 384]);
        let views = streamed_views(&poses(), &fov(), &layout);
        let left = views.left.foveation.unwrap();
        assert_eq!(left.full_size, (640, 720));
        assert!((left.center_size[0] - 0.45).abs() < 1e-3 && (left.center_size[1] - 0.3889).abs() < 1e-3);
        assert!((left.center_shift[0] - 0.4091).abs() < 1e-3 && (left.center_shift[1] - 0.1136).abs() < 1e-3);
        assert!((left.eye_size_ratio[0] - 0.9792).abs() < 1e-3 && (left.eye_size_ratio[1] - 0.9583).abs() < 1e-3);
        assert_eq!(left.edge_ratio, [4.0, 5.0]);
        assert!(!left.mirrored && views.right.foveation.unwrap().mirrored);
        // Each eye's rectangle is the padded encoded eye. The right eye is the right half
        assert_eq!(views.left.rect, CropRect { x: 0, y: 0, width: 384, height: 384 });
        assert_eq!(views.right.rect, CropRect { x: 384, y: 0, width: 384, height: 384 });
        assert_eq!(layout.frame(), [768, 384]);
    }

    #[test]
    fn a_quest_3_eye_foveates_to_about_a_third_of_the_pixels() {
        let params = FoveationParams { center_size: [0.45, 0.4], center_shift: [0.4, 0.1], edge_ratio: [4.0, 5.0] };
        let layout = StreamLayout::new([1680, 1760], Some(params));
        let [w, h] = layout.eye_picture();
        assert_eq!((w % 32, h % 32), (0, 0));
        let share = f64::from(w * h) / f64::from(1680 * 1760);
        assert!((0.28..0.36).contains(&share), "{share}");
    }

    #[test]
    fn a_link_that_streamed_connects_again_after_a_pause_and_one_that_never_did_does_not() {
        let start = Instant::now();
        let failed = LinkState::Failed("the link ended".into());
        let mut wanted = Wanted { host: "mac".into(), fingerprint: None, media: MediaChoice::Auto, streamed: false, failed_at: None };
        // Never streamed: the failure stays, as the panel shows why
        assert!(!wanted.due(&LinkState::Connecting, start));
        assert!(!wanted.due(&failed, start) && !wanted.due(&failed, start + Duration::from_secs(60)));
        // Streaming, then the host restarts: wait for the pause, then connect again
        assert!(!wanted.due(&LinkState::Streaming, start));
        assert!(!wanted.due(&failed, start + Duration::from_secs(1)));
        assert!(!wanted.due(&failed, start + Duration::from_secs(2) + Duration::from_millis(900)));
        assert!(wanted.due(&failed, start + Duration::from_secs(3) + Duration::from_millis(100)));
        // The attempt fails again (the host is still down): the next try follows after a new pause.
        // The caller starts it with `streamed` kept, so the failed state does not end the retries.
        assert!(wanted.streamed);
    }

    // A cable that fell back to Wi-Fi meets the Mac's card for a new headset. A no from the Mac's
    // user ends the tries: the panel keeps the answer until the user presses the Mac again.
    #[test]
    fn a_refusal_ends_the_tries_of_a_link_that_streamed() {
        let start = Instant::now();
        let failed = LinkState::Failed("The Mac denied this headset. Press the Mac to ask again.".into());
        let mut wanted = Wanted { host: "mac".into(), fingerprint: None, media: MediaChoice::Auto, streamed: false, failed_at: None };
        assert!(!wanted.due(&LinkState::Streaming, start));
        wanted.give_up();
        assert!(!wanted.due(&failed, start) && !wanted.due(&failed, start + Duration::from_secs(60)));
        // A new press starts a new connection, whose streaming makes retries count again
        assert!(!wanted.due(&LinkState::Streaming, start));
        assert!(!wanted.due(&failed, start));
        assert!(wanted.due(&failed, start + Duration::from_secs(3)));
    }

    #[test]
    fn a_link_that_waits_for_the_mac_is_neither_streaming_nor_due_for_a_new_try() {
        let start = Instant::now();
        let mut wanted = Wanted { host: "mac".into(), fingerprint: None, media: MediaChoice::Auto, streamed: false, failed_at: None };
        let waiting = LinkState::Waiting { code: "123 456".into(), host: "Mac".into() };
        assert!(!wanted.due(&waiting, start) && !wanted.due(&waiting, start + Duration::from_secs(60)));
        assert!(!wanted.streamed);
    }

    fn display(time: Nanos) -> Display {
        Display { time, period: 8_333_333, offset: 0 }
    }

    #[test]
    fn a_frame_taken_for_its_own_point_has_the_lead_of_the_headset_frame() {
        // The headset frame reaches the display 16 ms after the take. The frame's target is that display time.
        let d = display(1_000_000_000);
        let t = intake_timing(d, 984_000_000, 1_000_000_000);
        assert_eq!(t, IntakeTiming { display_time: 1_000_000_000, picked: 984_000_000, late: false });
        // A target with a little jitter still means this point
        assert_eq!(intake_timing(d, 984_000_000, 1_001_500_000).display_time, 1_000_000_000);
        // A frame with no target shows at the headset frame's display time
        assert_eq!(intake_timing(d, 984_000_000, 0), IntakeTiming { display_time: 1_000_000_000, picked: 984_000_000, late: false });
    }

    #[test]
    fn an_early_frame_waits_for_its_point_and_a_late_frame_shows_at_once() {
        let d = display(1_000_000_000);
        // Two points early: the point lies two periods later. The pick moves with it
        let early = intake_timing(d, 984_000_000, 1_016_666_666);
        assert_eq!((early.display_time, early.picked, early.late), (1_016_666_666, 1_000_666_666, false));
        // One point late: the frame shows in this headset frame, as it is the next chance
        let late = intake_timing(d, 984_000_000, 991_666_667);
        assert_eq!((late.display_time, late.picked, late.late), (1_000_000_000, 984_000_000, true));
    }
}
