//! The Mac's desktop for the native client's desktop panel.
//!
//! The client shows the desktop as a movable panel, a surface that a hardware decoder writes
//! into. The compositor then samples the picture once, at the display's own resolution, so text
//! stays sharp. This file sends the pictures and takes the pointer:
//! - The client asks with `ClientMsg::Desktop`. The host captures the display at its full pixel
//!   size (ScreenCaptureKit, YUV 4:2:0), encodes it with a second HEVC session and sends each
//!   picture as a unit of the stream `DESKTOP_STREAM`. ScreenCaptureKit gives a picture only when
//!   the display changes, so a still desktop costs nothing. A key frame goes out at the start,
//!   after a loss that the client reports and when the desktop becomes visible again.
//! - The pictures are not mirrored: Cranpose's compositor layer already turns a decoder's surface
//!   upright. The old Quest app's layer needed a mirror (`desktop_layer.rs`).
//! - The cursor's position goes out as a datagram every 2 ms while the Mac's mouse moves.
//! - `ClientDatagram::Pointer` carries where a controller points on the panel, with its trigger and
//!   thumbstick. The host moves the Mac's mouse, clicks and scrolls as the compat mode does
//!   (`desktop::Pointer`).
//! - `LayerState` tells the frame loop that the client shows the desktop, so the eye stream leaves
//!   its own copy of the screen out. It tells the loop that a game hides the desktop.

use super::Session;
use crate::{
    desktop::{self, Capture, CaptureSpec},
    desktop_layer::LayerState,
};
use alvr_common::{info, warn};
use cranvr_proto::{
    clock::now_ns,
    media::FrameMeta,
    message::{Cursor, DesktopParams, DesktopWish, HostDatagram, HostMsg, Pointer, DESKTOP_STREAM},
    wire::{Codec, Pose, Rect},
};
use cranvr_vt::{EncodedFrame, Encoder, EncoderConfig, Flipper};
use std::{
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
    /// The flags that the frame loop reads: the client shows the desktop, a game hides it
    pub layer: Arc<LayerState>,
    /// The picture stream's average bitrate
    pub mbps: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self { enabled: false, display_id: None, layer: Arc::default(), mbps: MBPS }
    }
}

/// The picture stream's average bitrate in Mbps. A still desktop needs far less. The old layer used
/// the same.
pub const MBPS: u64 = 40;
/// Pictures a second at most
const MAX_FPS: i32 = 60;
/// The cursor's position goes out this often while the mouse moves. It goes out at least this often
const CURSOR_EVERY: Duration = Duration::from_millis(2);
const CURSOR_KEEPALIVE: Duration = Duration::from_millis(50);
/// A pointer that the client stopped reporting counts as lifted after this time
const POINTER_STALE: Duration = Duration::from_millis(250);
/// The desktop thread logs its numbers this often
const LOG_EVERY: Duration = Duration::from_secs(2);
/// The wait after a capture or an encoder that failed to start
const RETRY_AFTER: Duration = Duration::from_secs(2);

/// What one session counts about the desktop. The log line shows the changes since its last look.
#[derive(Default)]
struct Counters {
    frames: AtomicU64,
    bytes: AtomicU64,
    keys: AtomicU64,
    /// Microseconds from the encode call to the encoded frame, summed over `frames`
    encode_us: AtomicU64,
    /// Microseconds that `send_unit` took, summed
    send_us: AtomicU64,
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
    pointer: Sender<Pointer>,
    stop: AtomicBool,
    counters: Counters,
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
        if !self.inner.options.enabled {
            info!("Native desktop: the client asks for the desktop and the host runs without --desktop");
            session.link.duplex.send_control(&HostMsg::Desktop(None)).ok();
            return;
        }
        info!("Native desktop: the client {} the desktop at up to {} pixels across and {} pictures a second", if wish.enabled { "asks for" } else { "stops" }, wish.max_width, wish.max_fps);
        *self.inner.wish.lock().unwrap() = Some(wish);
    }

    /// The client lost a picture of the desktop stream.
    pub(super) fn request_key_frame(&self) {
        self.inner.key.store(true, Ordering::Relaxed);
    }

    pub(super) fn on_pointer(&self, pointer: Pointer) {
        self.inner.counters.pointer_datagrams.fetch_add(1, Ordering::Relaxed);
        self.inner.pointer.send(pointer).ok();
    }
}

/// A running capture, its encoder and what the stream has sent so far.
struct Running {
    capture: Capture,
    encoder: Encoder,
    encoded: Receiver<EncodedFrame>,
    params: DesktopParams,
    wish: DesktopWish,
    flipper: Option<Flipper>,
    last_seq: u64,
    index: i64,
    /// The next frame is a key frame
    key: bool,
    /// The parameter sets that the client has
    config: Option<Vec<u8>>,
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

impl Running {
    fn open(options: &Options, wish: DesktopWish, notify: &SyncSender<()>) -> Result<Self, String> {
        let fps = i32::from(wish.max_fps).clamp(1, MAX_FPS);
        let capture = Capture::start_with(
            options.display_id,
            CaptureSpec {
                max_width: (wish.max_width as usize).max(640),
                fps,
                cursor: cursor_in_video(),
                yuv: true,
                notify: Some(notify.clone()),
                source: None,
                size: None,
            },
        )?;
        let (width, height) = capture.size;
        let (sender, encoded) = mpsc::channel();
        let encoder = Encoder::new(
            &EncoderConfig { width, height, fps: fps as f32, bitrate_bps: options.mbps * 1_000_000, full_range: true, strict_rate: false },
            sender,
        )?;
        let flipper = if flip_pictures() { Some(Flipper::new()?) } else { None };
        let params = DesktopParams {
            stream: DESKTOP_STREAM,
            codec: Codec::Hevc,
            size: [width, height],
            points: [capture.points.0 as f32, capture.points.1 as f32],
            max_fps: fps as f32,
            bitrate_bps: options.mbps * 1_000_000,
            full_range: true,
            flipped: flipper.is_some(),
            visible: true,
            cursor_in_video: cursor_in_video(),
        };
        Ok(Self { capture, encoder, encoded, params, wish, flipper, last_seq: 0, index: 0, key: true, config: None })
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
                        "Native desktop: capturing display {} at {}x{} pixels ({}x{} points), {} Mbps, up to {} pictures a second{}",
                        run.capture.display_id,
                        run.params.size[0],
                        run.params.size[1],
                        run.params.points[0],
                        run.params.points[1],
                        inner.options.mbps,
                        run.params.max_fps,
                        if cursor_in_video() { ", the cursor in the pictures" } else { "" }
                    );
                    inner.display.store(run.capture.display_id, Ordering::Relaxed);
                    inner.options.layer.active.store(true, Ordering::Relaxed);
                    session.link.duplex.send_control(&HostMsg::Desktop(Some(run.params))).ok();
                    running = Some(run);
                }
                Err(e) => {
                    warn!("Native desktop: no capture: {e}");
                    session.link.duplex.send_control(&HostMsg::Desktop(None)).ok();
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
                session.link.duplex.send_control(&HostMsg::Desktop(None)).ok();
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

/// One pass: visibility, the encode of a new capture and the send of the frames that came out.
fn step(inner: &Inner, session: &Session, run: &mut Running, next_frame: &mut u32, captured: &Receiver<()>) {
    captured.recv_timeout(Duration::from_millis(5)).ok();
    // A game hides the desktop: the capture stops and the client takes the panel away
    let visible = !inner.options.layer.game.load(Ordering::Relaxed);
    if visible != run.params.visible {
        run.params.visible = visible;
        run.capture.set_running(visible);
        if visible {
            run.key = true;
        }
        info!("Native desktop: {}", if visible { "visible again" } else { "hidden: a game runs" });
        session.link.duplex.send_control(&HostMsg::Desktop(Some(run.params))).ok();
    }
    if visible {
        run.capture.set_running(true);
    }
    if inner.key.swap(false, Ordering::Relaxed) {
        run.key = true;
    }
    if visible && run.encoder.in_flight() < 2 {
        if let Some((seq, image)) = run.capture.latest().filter(|(seq, _)| *seq != run.last_seq || run.key) {
            let timestamp = now_ns();
            let result = match &run.flipper {
                Some(flipper) => run.encoder.pixel_buffer().ok_or("the encoder has no buffer pool".to_string()).and_then(|flipped| {
                    flipper.flip(&image, &flipped)?;
                    run.encoder.encode(&flipped, run.index, timestamp, run.key)
                }),
                None => run.encoder.encode(&image, run.index, timestamp, run.key),
            };
            match result {
                Ok(()) => {
                    (run.last_seq, run.key) = (seq, false);
                    run.index += 1;
                }
                Err(e) => warn!("Native desktop: the encoder refused a picture: {e}"),
            }
        }
    }
    while let Ok(frame) = run.encoded.try_recv() {
        send_frame(inner, session, run, &frame, next_frame);
    }
}

fn send_frame(inner: &Inner, session: &Session, run: &mut Running, frame: &EncodedFrame, next_frame: &mut u32) {
    // The decoder needs the parameter sets before the first key frame and again when they change
    if let Some(config) = frame.config_nals.as_ref().filter(|c| run.config.as_ref() != Some(*c)) {
        let message = HostMsg::DecoderConfig { stream: DESKTOP_STREAM, codec: Codec::Hevc, config: config.clone() };
        if let Err(e) = session.link.duplex.send_control(&message) {
            warn!("Native desktop: the decoder config did not go out: {e}");
            return;
        }
        run.config = Some(config.clone());
    }
    let [width, height] = run.params.size;
    let meta = FrameMeta {
        stream: DESKTOP_STREAM,
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
        Err(e) => warn!("Native desktop: a picture did not go out: {e}"),
    }
}

/// The numbers since the last look, for the log line.
struct LogState {
    at: Instant,
    seen: (u64, u64, u64, u64, u64, u64, u64),
    mouse: desktop::InjectedCounts,
}

impl LogState {
    fn new() -> Self {
        Self { at: Instant::now(), seen: (0, 0, 0, 0, 0, 0, 0), mouse: desktop::injected_counts() }
    }

    fn maybe_log(&mut self, inner: &Inner) {
        if self.at.elapsed() < LOG_EVERY {
            return;
        }
        let seconds = self.at.elapsed().as_secs_f64();
        let c = &inner.counters;
        let now = (
            c.frames.load(Ordering::Relaxed),
            c.bytes.load(Ordering::Relaxed),
            c.keys.load(Ordering::Relaxed),
            c.encode_us.load(Ordering::Relaxed),
            c.send_us.load(Ordering::Relaxed),
            c.pointer_datagrams.load(Ordering::Relaxed),
            c.cursor_datagrams.load(Ordering::Relaxed),
        );
        let d = |a: u64, b: u64| a.saturating_sub(b);
        let frames = d(now.0, self.seen.0);
        let n = frames.max(1) as f64;
        let mouse = desktop::injected_counts();
        info!(
            "Native desktop: {:.1} pictures a second, {:.1} Mbps, {:.1} KB a picture, {} key, encode {:.2} ms, send {:.2} ms; the client sent {} pointer datagrams, {} cursor datagrams; mouse events: {} moves, {} downs, {} ups, {} px scrolled",
            frames as f64 / seconds,
            d(now.1, self.seen.1) as f64 * 8.0 / seconds / 1e6,
            d(now.1, self.seen.1) as f64 / n / 1000.0,
            d(now.2, self.seen.2),
            d(now.3, self.seen.3) as f64 / n / 1000.0,
            d(now.4, self.seen.4) as f64 / n / 1000.0,
            d(now.5, self.seen.5),
            d(now.6, self.seen.6),
            mouse.moves - self.mouse.moves,
            mouse.downs - self.mouse.downs,
            mouse.ups - self.mouse.ups,
            mouse.scroll_pixels - self.mouse.scroll_pixels,
        );
        (self.at, self.seen, self.mouse) = (Instant::now(), now, mouse);
    }
}

/// Moves the Mac's mouse to where the client says a controller points on the panel, clicks and
/// scrolls. A client that falls silent with its trigger down gets its button lifted.
fn pointer_loop(inner: &Inner, receiver: &Receiver<Pointer>) {
    let mut pointer: Option<desktop::Pointer> = None;
    let mut last_seq: Option<u32> = None;
    let mut last_update = Instant::now();
    let mut active = false;
    while !inner.stop.load(Ordering::Relaxed) {
        let message = match receiver.recv_timeout(Duration::from_millis(50)) {
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
                pointer.update_scaled(p.hit, p.trigger, p.scroll, dt * 72.0);
                active = p.hit.is_some() || p.trigger > 0.4;
                last_update = now;
            }
            None if active && now.duration_since(last_update) > POINTER_STALE => {
                pointer.update(None, 0.0, 0.0);
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
            session.link.duplex.send_datagram(&HostDatagram::Cursor(Cursor { seq, u, v, on })).ok();
            inner.counters.cursor_datagrams.fetch_add(1, Ordering::Relaxed);
            (seq, last, sent) = (seq.wrapping_add(1), now, Instant::now());
        }
    }
}
