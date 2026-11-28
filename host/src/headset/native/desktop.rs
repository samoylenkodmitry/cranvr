//! One display of the Mac for the native client's panel. `displays.rs` owns one `Desktop` for each
//! display that the client can show: the first display, and the ones that the client added. Each has
//! its own stream, wish, capture, encoder and pointer. A display that the client does not wish has no
//! capture and no encoder, sends nothing and follows no cursor.
//!
//! The client shows a display as a movable panel, a surface that a hardware decoder writes
//! into. The compositor then samples the picture once, at the display's own resolution, so text
//! stays sharp. This file sends the pictures and takes the mouse:
//! - The client asks with `ClientMsg::DisplayWish`. The host captures the display at the size that
//!   the wish allows (ScreenCaptureKit, YUV 4:2:0), encodes it with an HEVC session of its own and
//!   sends each picture as a unit of the display's stream. ScreenCaptureKit gives a picture only when
//!   the display changes, so a still desktop costs nothing. A key frame goes out at the start and
//!   after a loss that the client reports.
//! - The pictures are not mirrored: Cranpose's compositor layer already turns a decoder's surface
//!   upright.
//! - The cursor's position goes out as a datagram every 2 ms while the Mac's mouse moves.
//! - `ClientDatagram::DisplayMouse` carries where the controller that owns the mouse points on the panel,
//!   the mouse buttons that it holds (left, right, middle) and its thumbstick's lean in two axes.
//!   The host moves the Mac's mouse, clicks and scrolls (`desktop::Pointer`).
//! - `LayerState` tells the frame loop that the client owns the panels of the Mac's displays, so the
//!   eye stream leaves its own copy of the screen out. The host hides no display for a game: the client
//!   hears that a game runs (`HostMsg::GameRuns`) and decides itself which displays to show.

use super::Session;
use crate::{
    desktop::{self, Capture, CaptureSpec},
    desktop_layer::LayerState,
};
use log::{info, warn};
use cranvr_proto::{
    clock::now_ns,
    media::FrameMeta,
    message::{Cursor, DesktopParams, DesktopWish, HostDatagram, HostMsg, Mouse, DESKTOP_STREAM},
    wire::{Codec, Pose, Rect},
};
use cranvr_vt::{EncodedFrame, Encoder, EncoderConfig, Flipper, PixelBuffer, Scaler};
use std::{
    collections::VecDeque,
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

/// What the host decides about the desktop stream.
#[derive(Clone)]
pub struct Options {
    /// `--desktop`: the host shows the desktop at all
    pub enabled: bool,
    /// The display to capture, or `None` for the main display
    pub display_id: Option<u32>,
    /// The flags that the frame loop reads: the client shows the display
    pub layer: Arc<LayerState>,
    /// The picture stream's average bitrate at the full size and rate (see [`scaled_mbps`])
    pub mbps: u64,
    /// The stream that carries this display's pictures. The first display has `DESKTOP_STREAM`. The
    /// displays that the client adds have the streams after it (see `displays.rs`).
    pub stream: u8,
}

impl Default for Options {
    fn default() -> Self {
        Self { enabled: false, display_id: None, layer: Arc::default(), mbps: MBPS, stream: DESKTOP_STREAM }
    }
}

/// The picture stream's average bitrate in Mbps. A still desktop needs far less.
pub const MBPS: u64 = 40;
/// The smallest bitrate that a smaller or slower display gets
const MIN_MBPS: u64 = 6;

/// The bitrate for a display of `size` pixels at `fps` pictures a second: `base` at 3840 x 2160 and
/// 60 pictures a second, less in proportion to the pixels that the stream carries. A display that
/// shows over a game shares the media path with the eye stream, so it takes no more than it needs.
pub fn scaled_mbps(base: u64, [width, height]: [u32; 2], fps: i32) -> u64 {
    let share = f64::from(width) * f64::from(height) * f64::from(fps.max(1)) / (3840.0 * 2160.0 * 60.0);
    ((base as f64 * share).round() as u64).clamp(MIN_MBPS.min(base), base)
}
/// Pictures a second at most
const MAX_FPS: i32 = 60;
/// The cursor's position goes out this often while the mouse moves. It goes out at least this often
const CURSOR_EVERY: Duration = Duration::from_millis(2);
const CURSOR_KEEPALIVE: Duration = Duration::from_millis(50);
/// The cursor loop looks this often for a capture while none runs
const CURSOR_IDLE: Duration = Duration::from_millis(100);
/// A pointer that the client stopped reporting counts as lifted after this time
const POINTER_STALE: Duration = Duration::from_millis(250);
/// The desktop thread logs its numbers this often
const LOG_EVERY: Duration = Duration::from_secs(2);
/// The wait after a capture or an encoder that failed to start
const RETRY_AFTER: Duration = Duration::from_secs(2);

/// What one session counts about the desktop. The log line shows the changes since its last look.
#[derive(Default)]
struct Counters {
    /// Pictures that the capture gave the encoder's turn: a display that is not shown captures none
    captured: AtomicU64,
    /// Pictures that the encoder finished
    encoded: AtomicU64,
    /// Pictures that went out
    frames: AtomicU64,
    bytes: AtomicU64,
    keys: AtomicU64,
    /// Microseconds from the encode call to the encoded frame, summed over `frames`
    encode_us: AtomicU64,
    /// Microseconds that `send_unit` took, summed
    send_us: AtomicU64,
    /// Microseconds that the hand-over of each picture to the encoder took, its scaling included, summed
    submit_us: AtomicU64,
    /// Pictures handed over
    submitted: AtomicU64,
    pointer_datagrams: AtomicU64,
    cursor_datagrams: AtomicU64,
}

struct Inner {
    options: Options,
    wish: Mutex<Option<DesktopWish>>,
    /// The client lost a picture: the next one is a key frame
    key: AtomicBool,
    /// The display of the running capture, for the pointer. 0 while none runs.
    display: AtomicU32,
    pointer: Sender<Mouse>,
    stop: AtomicBool,
    counters: Counters,
}

/// What a display did, as counts since the host started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct DisplayCounts {
    pub captured: u64,
    pub encoded: u64,
    pub sent: u64,
    pub cursors: u64,
}

impl Inner {
    /// The name of this display in the log
    fn label(&self) -> String {
        format!("Native display {}", self.options.stream - DESKTOP_STREAM + 1)
    }

    /// Tells the client the state of this display's video: its parameters, or `None` while the host
    /// runs none.
    fn announce(&self, session: &Session, params: Option<DesktopParams>) {
        session.link.duplex.send_control(&HostMsg::DisplayStream { stream: self.options.stream, params }).ok();
    }
}

/// The desktop of one session.
pub(super) struct Desktop {
    inner: Arc<Inner>,
}

impl Desktop {
    /// Starts the threads of the session's desktop. They end with the session or with `stop`.
    pub(super) fn start(options: Options, session: Arc<Session>) -> Self {
        let (pointer, pointer_receiver) = mpsc::channel();
        let inner = Arc::new(Inner {
            options,
            wish: Mutex::new(None),
            key: AtomicBool::new(false),
            display: AtomicU32::new(0),
            pointer,
            stop: AtomicBool::new(false),
            counters: Counters::default(),
        });
        if inner.options.enabled {
            let spawn = |name: &str, run: Box<dyn FnOnce() + Send>| {
                thread::Builder::new().name(name.into()).spawn(run).ok();
            };
            let (i, s) = (Arc::clone(&inner), Arc::clone(&session));
            spawn("native-desktop", Box::new(move || desktop_loop(&i, &s)));
            let (i, s) = (Arc::clone(&inner), Arc::clone(&session));
            spawn("native-cursor", Box::new(move || cursor_loop(&i, &s)));
            let i = Arc::clone(&inner);
            spawn("native-pointer", Box::new(move || pointer_loop(&i, &pointer_receiver)));
        }
        Self { inner }
    }

    /// Ends the threads and gives the screen back to the eye stream.
    pub(super) fn stop(&self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        self.inner.options.layer.active.store(false, Ordering::Relaxed);
    }

    /// The client asks for the stream, or stops it. A host without `--desktop` answers that it has
    /// none.
    pub(super) fn on_wish(&self, wish: DesktopWish, session: &Session) {
        let label = self.inner.label();
        if !self.inner.options.enabled {
            info!("{label}: the client asks for the display and the host runs without --desktop");
            self.inner.announce(session, None);
            return;
        }
        info!(
            "{label}: the client {} the display at up to {} pixels across and {} pictures a second",
            if wish.enabled { "asks for" } else { "stops" },
            wish.max_width,
            wish.max_fps
        );
        *self.inner.wish.lock().unwrap() = Some(wish);
    }

    /// What this display did since the host started: pictures captured, pictures encoded, pictures
    /// sent and cursor datagrams sent. A display that no client shows counts none of them.
    pub(super) fn counts(&self) -> DisplayCounts {
        let c = &self.inner.counters;
        DisplayCounts {
            captured: c.captured.load(Ordering::Relaxed),
            encoded: c.encoded.load(Ordering::Relaxed),
            sent: c.frames.load(Ordering::Relaxed),
            cursors: c.cursor_datagrams.load(Ordering::Relaxed),
        }
    }

    /// Whether a capture of this display runs now.
    pub(super) fn is_running(&self) -> bool {
        self.inner.options.layer.active.load(Ordering::Relaxed)
    }

    /// The stream of this display's pictures.
    pub(super) fn stream(&self) -> u8 {
        self.inner.options.stream
    }

    /// The client lost a picture of this display's stream.
    pub(super) fn request_key_frame(&self) {
        self.inner.key.store(true, Ordering::Relaxed);
    }

    pub(super) fn on_mouse(&self, mouse: Mouse) {
        self.inner.counters.pointer_datagrams.fetch_add(1, Ordering::Relaxed);
        self.inner.pointer.send(mouse).ok();
    }
}

/// Whether the host has more to do for a mouse datagram than to wait: the controller points at the
/// panel or holds a button.
fn busy(mouse: &Mouse) -> bool {
    mouse.hit.is_some() || mouse.buttons != 0
}

/// A running capture, its encoder and what the stream has sent so far.
struct Running {
    capture: Capture,
    encoder: DisplayEncoder,
    encoded: Receiver<EncodedFrame>,
    params: DesktopParams,
    wish: DesktopWish,
    feed: Feed,
    /// The parameter sets that the client has
    config: Option<Vec<u8>>,
}

/// The highest quantizer of a display's frames while its stream stays under its bitrate. In
/// `display_quality.rs` it held the made-up text of the hand's display at 39 dB in every scene, against
/// 33 to 37 dB without it.
const DISPLAY_MAX_QP: u8 = 18;
/// The cap holds while the frames of the last second stay under `CAP_OFF` halves of the bitrate.
/// VideoToolbox passes the bitrate on a hard picture with the cap and without it: a video in a part of
/// the hand's display sent 13 Mbps at a target of 6 with the cap, and 11 Mbps without it. A video
/// that fills the display sent ten times the bitrate with the cap. VideoToolbox reads the cap only
/// when a session starts, so the display then changes to a session without the cap. It changes back,
/// with a key frame of the newest picture, once the last second's frames fall under the bitrate
/// divided by `CAP_BACK`.
const RATE_WINDOW: Duration = Duration::from_secs(1);
const CAP_OFF: u64 = 5;
/// The session without the cap stayed at a third to a half of the bitrate during a video, so a higher
/// share made the display change sessions every second.
const CAP_BACK: u64 = 8;
/// The shortest time between two changes of the session: each change costs a key frame
const SWAP_GAP: Duration = Duration::from_secs(1);
/// A display that stopped changing gets its last picture again after this time, so a picture that the
/// encoder dropped does not stay missing
const REFRESH_AFTER: Duration = Duration::from_millis(250);

/// The encoder of a display: a session with the cap on the quantizer or one without it.
pub(super) struct DisplayEncoder {
    encoder: Encoder,
    capped: bool,
    size: [u32; 2],
    fps: i32,
    mbps: u64,
    sender: Sender<EncodedFrame>,
    /// Scales a capture of another size to the stream's size
    scaler: Option<Scaler>,
    /// Mirrors each picture (`CRANVR_DESKTOP_FLIP=1`)
    flipper: Option<Flipper>,
}

/// What a display's encoder does to each capture before it encodes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Conversion {
    /// The size of the captures in pixels. A capture larger than the stream is scaled down to it.
    pub source: [u32; 2],
    /// Mirror each picture top to bottom
    pub flip: bool,
}

impl DisplayEncoder {
    /// A capped session of HEVC at `fps` pictures a second at most and `mbps` on average. Its frames
    /// go to `sender`. A key frame comes first and when the client lost a picture, never on a timer:
    /// a key frame costs 680 KB at 1920 x 1080 and 2.7 MB at 3840 x 2160 in `display_quality.rs`, and
    /// the client asks for one after each loss and each new decoder.
    /// `conversion` says what each capture needs first.
    pub(super) fn new(size: [u32; 2], fps: i32, mbps: u64, sender: Sender<EncodedFrame>, conversion: Conversion) -> Result<Self, String> {
        let scaler = if conversion.source == size { None } else { Some(Scaler::new()?) };
        let flipper = if conversion.flip { Some(Flipper::new()?) } else { None };
        Ok(Self { encoder: Self::session(size, fps, mbps, &sender, true)?, capped: true, size, fps, mbps, sender, scaler, flipper })
    }

    fn session([width, height]: [u32; 2], fps: i32, mbps: u64, sender: &Sender<EncodedFrame>, capped: bool) -> Result<Encoder, String> {
        let max_qp = capped.then_some(DISPLAY_MAX_QP);
        // Out of real time: the eye stream's real-time session goes first on the Mac's encoder
        let config = EncoderConfig { width, height, fps: fps as f32, bitrate_bps: mbps * 1_000_000, full_range: true, strict_rate: false, key_every: None, max_qp, real_time: false };
        Encoder::new(&config, sender.clone())
    }

    pub(super) fn in_flight(&self) -> usize {
        self.encoder.in_flight()
    }

    /// Whether the session has the cap on the quantizer.
    pub(super) fn capped(&self) -> bool {
        self.capped
    }

    /// Hands `picture` to the encoder, scaled to the stream's size and mirrored first when the
    /// conversion says so. A picture that wants the other session starts it: the old session hands
    /// over its last frames, and the picture becomes the new session's key frame.
    pub(super) fn encode(&mut self, picture: &Picture, timestamp: u64) -> Result<(), String> {
        let mut key = picture.key;
        if picture.cap != self.capped {
            self.encoder = Self::session(self.size, self.fps, self.mbps, &self.sender, picture.cap)?;
            self.capped = picture.cap;
            key = true;
        }
        let scaled = match &self.scaler {
            Some(scaler) => {
                let buffer = self.encoder.pixel_buffer().ok_or("the encoder has no buffer pool".to_string())?;
                scaler.scale(&picture.image, &buffer)?;
                Some(buffer)
            }
            None => None,
        };
        let image = scaled.as_ref().unwrap_or(&picture.image);
        match &self.flipper {
            Some(flipper) => {
                let flipped = self.encoder.pixel_buffer().ok_or("the encoder has no buffer pool".to_string())?;
                flipper.flip(image, &flipped)?;
                self.encoder.encode(&flipped, picture.presentation, timestamp, key)
            }
            None => self.encoder.encode(image, picture.presentation, timestamp, key),
        }
    }
}

/// Decides which picture goes to a display's encoder, when, where it stands on the encoder's time line
/// and whether the cap on the quantizer holds. ScreenCaptureKit hands over a picture only when the
/// display changes, so the pictures come at uneven times.
pub(super) struct Feed {
    started: Instant,
    /// The bytes that the stream may send in a second
    budget: u64,
    /// The number of the capture that went in last
    last_seq: u64,
    last_presentation: Option<Duration>,
    /// The next picture is a key frame
    key: bool,
    /// When the last picture went in
    sent_at: Instant,
    /// The last capture goes in again once the display stays still
    refresh: bool,
    /// The cap holds, and when it changed last
    cap: bool,
    cap_changed: Option<Instant>,
    /// The frames other than key frames that came out of the encoder in the last `RATE_WINDOW`: when
    /// and how many bytes
    out: VecDeque<(Instant, u64)>,
}

/// One picture for the encoder.
pub(super) struct Picture {
    pub image: PixelBuffer,
    seq: u64,
    /// Where the picture stands on the encoder's time line
    pub presentation: Duration,
    pub key: bool,
    /// The picture goes to the session with the cap on the quantizer
    pub cap: bool,
}

impl Feed {
    /// A feed for a stream of `bitrate_bps`. Its first picture is a key frame.
    pub(super) fn new(now: Instant, bitrate_bps: u64) -> Self {
        Self {
            started: now,
            budget: bitrate_bps / 8,
            last_seq: 0,
            last_presentation: None,
            key: true,
            sent_at: now,
            refresh: false,
            cap: true,
            cap_changed: None,
            out: VecDeque::new(),
        }
    }

    /// The client lost a picture: the next one is a key frame.
    pub(super) fn request_key(&mut self) {
        self.key = true;
    }

    /// A frame of `bytes` came out of the encoder at `now`. A key frame does not count: it comes once,
    /// and a sharp key frame of a rich desktop may be larger than a second's bitrate on its own.
    pub(super) fn sent(&mut self, bytes: usize, key: bool, now: Instant) {
        if !key {
            self.out.push_back((now, bytes as u64));
        }
    }

    /// The picture to encode at `now`, if any. `latest` is the newest capture and its number. A new
    /// capture goes in at once. The last capture goes in again for a key frame, when the cap comes
    /// back and once more after the display stays still for `REFRESH_AFTER`.
    pub(super) fn next(&mut self, latest: Option<(u64, PixelBuffer)>, now: Instant) -> Option<Picture> {
        while self.out.front().is_some_and(|(at, _)| now.duration_since(*at) > RATE_WINDOW) {
            self.out.pop_front();
        }
        let last_second: u64 = self.out.iter().map(|(_, bytes)| bytes).sum();
        let cap = if self.cap { last_second <= self.budget * CAP_OFF / 2 } else { last_second < self.budget / CAP_BACK };
        let swap = cap != self.cap && self.cap_changed.is_none_or(|at| now.duration_since(at) >= SWAP_GAP);
        if swap {
            (self.cap, self.cap_changed) = (cap, Some(now));
        }
        let (seq, image) = latest?;
        let back = swap && self.cap;
        let refresh = self.refresh && now.duration_since(self.sent_at) >= REFRESH_AFTER;
        if seq == self.last_seq && !self.key && !refresh && !back {
            return None;
        }
        // The time line is the real time since the capture started. VideoToolbox's rate control
        // reads the bitrate against it, so a picture gets the bits of the time since the one before.
        // The time line must grow, so a picture in the same microsecond as the last moves on by one.
        let elapsed = now.duration_since(self.started);
        let presentation = self.last_presentation.map_or(elapsed, |last| elapsed.max(last + Duration::from_micros(1)));
        Some(Picture { image, seq, presentation, key: self.key, cap: self.cap })
    }

    /// The encoder took `picture` at `now`, the time of the `next` call that gave it.
    pub(super) fn took(&mut self, picture: &Picture, now: Instant) {
        // A new capture may need a refresh later; a refresh needs no second one
        self.refresh = picture.seq != self.last_seq;
        (self.last_seq, self.last_presentation, self.key, self.sent_at) = (picture.seq, Some(picture.presentation), false, now);
    }

    /// Whether `picture` is a capture that did not go in before.
    pub(super) fn is_new(&self, picture: &Picture) -> bool {
        picture.seq != self.last_seq
    }
}

/// The desktop's pictures are mirrored for a compositor that reads a surface bottom row first:
/// `CRANVR_DESKTOP_FLIP=1`. Cranpose's compositor does not need it.
fn flip_pictures() -> bool {
    std::env::var("CRANVR_DESKTOP_FLIP").is_ok_and(|v| v == "1")
}

/// The cursor goes into the pictures unless `CRANVR_DESKTOP_CURSOR_IN_VIDEO=0`. The client then
/// draws no cursor of its own. The cursor is as late as the pictures are. A panel in front of
/// the client's surface panel is hidden under the surface today (Cranpose draws surfaces after the
/// panels), so a cursor that the client draws cannot show over the desktop.
fn cursor_in_video() -> bool {
    !matches!(std::env::var("CRANVR_DESKTOP_CURSOR_IN_VIDEO").as_deref(), Ok("0"))
}

/// The size of a display's stream for a capture of `source` pixels and a wish of up to `max_width`
/// pixels across: the capture's size, or a smaller one of the same shape. Both sides are even, as
/// the encoder's 4:2:0 pictures need. The picture is at least 640 pixels wide.
fn stream_size([width, height]: [u32; 2], max_width: u32) -> [u32; 2] {
    let scaled = width.min(max_width.max(640)) & !1;
    [scaled, ((u64::from(scaled) * u64::from(height) / u64::from(width.max(1))) as u32) & !1]
}

impl Running {
    fn open(options: &Options, wish: DesktopWish, notify: &SyncSender<()>) -> Result<Self, String> {
        let fps = i32::from(wish.max_fps).clamp(1, MAX_FPS);
        // The capture keeps the display's own pixels: the encoder's scaler shrinks them to the wish
        // with far less blur than ScreenCaptureKit's own scaling (see `cranvr_vt::Scaler`)
        let capture = Capture::start_with(
            options.display_id,
            CaptureSpec { max_width: usize::MAX, fps, cursor: cursor_in_video(), yuv: true, notify: Some(notify.clone()) },
        )?;
        let source = [capture.size.0, capture.size.1];
        let [width, height] = stream_size(source, wish.max_width);
        let (sender, encoded) = mpsc::channel();
        let mbps = scaled_mbps(options.mbps, [width, height], fps);
        let encoder = DisplayEncoder::new([width, height], fps, mbps, sender, Conversion { source, flip: flip_pictures() })?;
        let params = DesktopParams {
            stream: options.stream,
            codec: Codec::Hevc,
            size: [width, height],
            points: [capture.points.0 as f32, capture.points.1 as f32],
            max_fps: fps as f32,
            bitrate_bps: mbps * 1_000_000,
            full_range: true,
            flipped: flip_pictures(),
            cursor_in_video: cursor_in_video(),
        };
        Ok(Self { capture, encoder, encoded, params, wish, feed: Feed::new(Instant::now(), mbps * 1_000_000), config: None })
    }
}

/// Runs the capture, the encoder and the send for the client's wish until the session ends.
fn desktop_loop(inner: &Inner, session: &Session) {
    let (notify, captured) = mpsc::sync_channel::<()>(1);
    let mut running: Option<Running> = None;
    let mut retry_at: Option<Instant> = None;
    let mut next_frame = 0u32;
    let mut log = LogState::new();
    let stopped = || inner.stop.load(Ordering::Relaxed) || *session.stopped.lock().unwrap();
    while !stopped() {
        let wish = (*inner.wish.lock().unwrap()).filter(|w| w.enabled);
        match (&mut running, wish) {
            (None, Some(wish)) if retry_at.is_none_or(|at| Instant::now() >= at) => match Running::open(&inner.options, wish, &notify) {
                Ok(run) => {
                    info!(
                        "{}: capturing display {} at {}x{} pixels ({}x{} points), {} Mbps, up to {} pictures a second{}",
                        inner.label(),
                        run.capture.display_id,
                        run.params.size[0],
                        run.params.size[1],
                        run.params.points[0],
                        run.params.points[1],
                        run.params.bitrate_bps / 1_000_000,
                        run.params.max_fps,
                        if cursor_in_video() { ", the cursor in the pictures" } else { "" }
                    );
                    inner.display.store(run.capture.display_id, Ordering::Relaxed);
                    inner.options.layer.active.store(true, Ordering::Relaxed);
                    inner.announce(session, Some(run.params));
                    running = Some(run);
                }
                Err(e) => {
                    warn!("{}: no capture: {e}", inner.label());
                    inner.announce(session, None);
                    retry_at = Some(Instant::now() + RETRY_AFTER);
                }
            },
            (Some(run), Some(wish)) if wish.max_width != run.wish.max_width || wish.max_fps != run.wish.max_fps => {
                // A new size needs a new capture and encoder. The frame numbers go on.
                running = None;
                inner.display.store(0, Ordering::Relaxed);
                continue;
            }
            (Some(_), None) => {
                running = None;
                inner.display.store(0, Ordering::Relaxed);
                inner.options.layer.active.store(false, Ordering::Relaxed);
                inner.announce(session, None);
            }
            _ => {}
        }
        let Some(run) = running.as_mut() else {
            thread::sleep(Duration::from_millis(20));
            continue;
        };
        step(inner, session, run, &mut next_frame, &captured);
        log.maybe_log(inner);
    }
    inner.options.layer.active.store(false, Ordering::Relaxed);
    inner.display.store(0, Ordering::Relaxed);
}

/// One pass: the encode of a new capture and the send of the frames that came out.
fn step(inner: &Inner, session: &Session, run: &mut Running, next_frame: &mut u32, captured: &Receiver<()>) {
    captured.recv_timeout(Duration::from_millis(5)).ok();
    // A headset without the input focus gets no new pictures. Its panel keeps the last one.
    let live = !session.paused();
    run.capture.set_running(live);
    if inner.key.swap(false, Ordering::Relaxed) {
        run.feed.request_key();
    }
    if live && run.encoder.in_flight() < 2 {
        let now = Instant::now();
        if let Some(picture) = run.feed.next(run.capture.latest(), now) {
            if run.feed.is_new(&picture) {
                inner.counters.captured.fetch_add(1, Ordering::Relaxed);
            }
            let capped = run.encoder.capped();
            let submit = Instant::now();
            let result = run.encoder.encode(&picture, now_ns());
            inner.counters.submit_us.fetch_add(submit.elapsed().as_micros() as u64, Ordering::Relaxed);
            inner.counters.submitted.fetch_add(1, Ordering::Relaxed);
            match result {
                Ok(()) => run.feed.took(&picture, now),
                Err(e) => warn!("{}: the encoder refused a picture: {e}", inner.label()),
            }
            if capped != run.encoder.capped() {
                info!(
                    "{}: {}",
                    inner.label(),
                    if picture.cap {
                        "the stream calmed: a session with the cap on the quantizer starts"
                    } else {
                        "the stream passed 2.5 times its bitrate: a session without the cap on the quantizer starts"
                    }
                );
            }
        }
    }
    while let Ok(frame) = run.encoded.try_recv() {
        inner.counters.encoded.fetch_add(1, Ordering::Relaxed);
        run.feed.sent(frame.data.len(), frame.is_idr, Instant::now());
        send_frame(inner, session, run, &frame, next_frame);
    }
}

fn send_frame(inner: &Inner, session: &Session, run: &mut Running, frame: &EncodedFrame, next_frame: &mut u32) {
    // The decoder needs the parameter sets before the first key frame and again when they change
    if let Some(config) = frame.config_nals.as_ref().filter(|c| run.config.as_ref() != Some(*c)) {
        let message = HostMsg::DecoderConfig { stream: inner.options.stream, codec: Codec::Hevc, config: config.clone() };
        if let Err(e) = session.link.duplex.send_control(&message) {
            warn!("{}: the decoder config did not go out: {e}", inner.label());
            return;
        }
        run.config = Some(config.clone());
    }
    let [width, height] = run.params.size;
    let meta = FrameMeta {
        stream: inner.options.stream,
        frame: *next_frame,
        slice: 0,
        key: frame.is_idr,
        tracking_sample: 0,
        target_display: 0,
        sent: now_ns(),
        eye_poses: [Pose::IDENTITY; 2],
        slices: vec![Rect { x: 0, y: 0, w: width, h: height }],
    };
    *next_frame = next_frame.wrapping_add(1);
    let parity = session.parity.lock().unwrap().clone();
    let started = Instant::now();
    match session.link.send_unit(&meta, &frame.data, &parity) {
        Ok(stats) => {
            let c = &inner.counters;
            c.frames.fetch_add(1, Ordering::Relaxed);
            c.bytes.fetch_add(stats.bytes as u64, Ordering::Relaxed);
            c.keys.fetch_add(u64::from(frame.is_idr), Ordering::Relaxed);
            c.encode_us.fetch_add(frame.encode_time.as_micros() as u64, Ordering::Relaxed);
            c.send_us.fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
            inner.options.layer.frames.fetch_add(1, Ordering::Relaxed);
        }
        Err(e) => warn!("{}: a picture did not go out: {e}", inner.label()),
    }
}

/// The numbers since the last look, for the log line.
struct LogState {
    at: Instant,
    seen: Snapshot,
    mouse: desktop::InjectedCounts,
}

/// The counters of a display at one time, for the changes since the last log line
#[derive(Clone, Copy, Default)]
struct Snapshot {
    frames: u64,
    bytes: u64,
    keys: u64,
    encode_us: u64,
    send_us: u64,
    pointer_datagrams: u64,
    cursor_datagrams: u64,
    submit_us: u64,
    submitted: u64,
}

impl Snapshot {
    fn of(c: &Counters) -> Self {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        Self {
            frames: read(&c.frames),
            bytes: read(&c.bytes),
            keys: read(&c.keys),
            encode_us: read(&c.encode_us),
            send_us: read(&c.send_us),
            pointer_datagrams: read(&c.pointer_datagrams),
            cursor_datagrams: read(&c.cursor_datagrams),
            submit_us: read(&c.submit_us),
            submitted: read(&c.submitted),
        }
    }
}

impl LogState {
    fn new() -> Self {
        Self { at: Instant::now(), seen: Snapshot::default(), mouse: desktop::injected_counts() }
    }

    fn maybe_log(&mut self, inner: &Inner) {
        if self.at.elapsed() < LOG_EVERY {
            return;
        }
        let seconds = self.at.elapsed().as_secs_f64();
        let (now, seen) = (Snapshot::of(&inner.counters), self.seen);
        let frames = now.frames - seen.frames;
        let n = frames.max(1) as f64;
        let bytes = (now.bytes - seen.bytes) as f64;
        let mouse = desktop::injected_counts();
        info!(
            "{}: {:.1} pictures a second, {:.1} Mbps, {:.1} KB a picture, {} key, hand-over {:.2} ms, encode {:.2} ms, send {:.2} ms; the client sent {} pointer datagrams, {} cursor datagrams; mouse events: {} moves, {} downs ({} right, {} middle), {} ups, {} px scrolled, {} px scrolled across",
            inner.label(),
            frames as f64 / seconds,
            bytes * 8.0 / seconds / 1e6,
            bytes / n / 1000.0,
            now.keys - seen.keys,
            (now.submit_us - seen.submit_us) as f64 / (now.submitted - seen.submitted).max(1) as f64 / 1000.0,
            (now.encode_us - seen.encode_us) as f64 / n / 1000.0,
            (now.send_us - seen.send_us) as f64 / n / 1000.0,
            now.pointer_datagrams - seen.pointer_datagrams,
            now.cursor_datagrams - seen.cursor_datagrams,
            mouse.moves - self.mouse.moves,
            mouse.downs - self.mouse.downs,
            mouse.right_downs - self.mouse.right_downs,
            mouse.middle_downs - self.mouse.middle_downs,
            mouse.ups - self.mouse.ups,
            mouse.scroll_pixels - self.mouse.scroll_pixels,
            mouse.scroll_across_pixels - self.mouse.scroll_across_pixels,
        );
        (self.at, self.seen, self.mouse) = (Instant::now(), now, mouse);
    }
}

/// Moves the Mac's mouse to where the client says a controller points on the panel, clicks and
/// scrolls. A client that falls silent with its trigger down gets its button lifted.
fn pointer_loop(inner: &Inner, receiver: &Receiver<Mouse>) {
    let mut pointer: Option<desktop::Pointer> = None;
    let mut last_seq: Option<u32> = None;
    let mut last_update = Instant::now();
    let mut active = false;
    while !inner.stop.load(Ordering::Relaxed) {
        // While nothing points and nothing is held, the thread has nothing to time: it wakes for a
        // datagram or twice a second to look at `stop`
        let wait = if active { Duration::from_millis(50) } else { Duration::from_millis(500) };
        let message = match receiver.recv_timeout(wait) {
            Ok(mut message) => {
                // Only the newest of a burst counts
                while let Ok(newer) = receiver.try_recv() {
                    message = newer;
                }
                Some(message)
            }
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let display = inner.display.load(Ordering::Relaxed);
        if display == 0 {
            continue;
        }
        let pointer = pointer.get_or_insert_with(|| desktop::Pointer::new(display));
        pointer.set_display(display);
        let now = Instant::now();
        match message {
            Some(p) => {
                // UDP may swap datagrams: an older one would move the mouse back. A far older
                // number means a new client session that counts from 0 again.
                if last_seq.is_some_and(|last| last.wrapping_sub(p.seq) < 1000) {
                    continue;
                }
                last_seq = Some(p.seq);
                // The scroll of one update is 20 pixels at 72 updates a second: scale it to the
                // time since the last update, so the speed does not depend on the client's rate
                let dt = now.duration_since(last_update).as_secs_f32().min(0.05);
                pointer.update_mouse(p.hit, p.buttons, p.scroll, dt * 72.0);
                active = busy(&p);
                last_update = now;
            }
            // Every button that is down goes up
            None if active && now.duration_since(last_update) > POINTER_STALE => {
                pointer.update_mouse(None, 0, [0.0; 2], 1.0);
                active = false;
                last_update = now;
            }
            None => {}
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGPoint,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreate(source: *const c_void) -> *mut c_void;
    fn CGEventGetLocation(event: *mut c_void) -> CGPoint;
    fn CGDisplayBounds(display: u32) -> CGRect;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *mut c_void);
}

/// The mouse position in global display coordinates. Reading it needs no permission.
fn mouse_location() -> Option<CGPoint> {
    unsafe {
        let event = CGEventCreate(std::ptr::null());
        if event.is_null() {
            return None;
        }
        let point = CGEventGetLocation(event);
        CFRelease(event);
        Some(point)
    }
}

/// Sends the Mac's cursor position on the captured display every 2 ms while it moves. It sends it every
/// 50 ms otherwise, so a lost datagram does not leave the client with an old position.
fn cursor_loop(inner: &Inner, session: &Session) {
    let (mut seq, mut last, mut sent) = (0u32, None::<(u32, u32, bool)>, Instant::now());
    // For tests without a real mouse: CRANVR_CURSOR_TEST=circle makes the cursor circle the middle of
    // the display once a second
    let circle = std::env::var("CRANVR_CURSOR_TEST").is_ok_and(|v| v == "circle");
    let start = Instant::now();
    while !inner.stop.load(Ordering::Relaxed) && !*session.stopped.lock().unwrap() {
        // A display that no capture serves has no cursor to follow: the loop only waits for one
        if inner.display.load(Ordering::Relaxed) == 0 {
            thread::sleep(CURSOR_IDLE);
            continue;
        }
        thread::sleep(CURSOR_EVERY);
        let display = inner.display.load(Ordering::Relaxed);
        if display == 0 {
            continue;
        }
        let bounds = unsafe { CGDisplayBounds(display) };
        let point = if circle {
            let a = start.elapsed().as_secs_f64() * std::f64::consts::TAU;
            Some(CGPoint { x: bounds.origin.x + bounds.size.x * (0.5 + 0.2 * a.cos()), y: bounds.origin.y + bounds.size.y * (0.5 + 0.2 * a.sin()) })
        } else {
            mouse_location()
        };
        let Some(point) = point else { continue };
        let on = (bounds.origin.x..bounds.origin.x + bounds.size.x).contains(&point.x)
            && (bounds.origin.y..bounds.origin.y + bounds.size.y).contains(&point.y);
        let (u, v) = if on { (((point.x - bounds.origin.x) / bounds.size.x.max(1.0)) as f32, ((point.y - bounds.origin.y) / bounds.size.y.max(1.0)) as f32) } else { (0.0, 0.0) };
        let now = Some((u.to_bits(), v.to_bits(), on));
        if now != last || sent.elapsed() >= CURSOR_KEEPALIVE {
            let cursor = Cursor { seq, u, v, on };
            session.link.duplex.send_datagram(&HostDatagram::DisplayCursor { stream: inner.options.stream, cursor }).ok();
            inner.counters.cursor_datagrams.fetch_add(1, Ordering::Relaxed);
            (seq, last, sent) = (seq.wrapping_add(1), now, Instant::now());
        }
    }
}

#[cfg(test)]
#[path = "../../tests/display_quality.rs"]
mod quality_tests;

#[cfg(test)]
#[path = "../../tests/display_scale.rs"]
mod scale_tests;
