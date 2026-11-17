//! The Mac's desktop in the headset, as a panel in the room.
//!
//! The host sends the display as its own HEVC stream at the display's full pixel size (stream
//! `DESKTOP_STREAM`, see `host/src/headset/native/desktop.rs`). A second `VideoDecoder` in surface
//! mode writes the pictures into the surface of a `SpatialExternalSurface`. The headset's
//! compositor shows that surface as a layer of its own and samples the picture once at the
//! display's resolution, so text stays sharp. The panel is mono.
//!
//! The controllers work as the mouse. `place.rs` finds where a controller's ray meets the panel and
//! moves the panel when the user carries it. This file sends the point, the trigger and the
//! thumbstick to the host as `ClientDatagram::Pointer` datagrams. The host moves the Mac's mouse,
//! clicks and scrolls.
//!
//! Threads and what they own:
//! - the network threads call [`on_message`], [`push_unit`] and [`on_cursor`];
//! - the desktop decode thread ([`decode_loop`]) feeds the decoder, so a decoder that waits for an
//!   input buffer never holds up the eye stream;
//! - the uplink thread ([`uplink`]) sends the wish, the pointer and the requests for key frames;
//! - the composition thread creates and ends the surface ([`DesktopPanel`]) and the headset's frame
//!   loop runs the panel's `SpatialFrameEffect`, which feeds `place.rs`.
//!
//! The decoder lives in a mutex because the composition thread must drop it before the surface's
//! end callback returns, while the decode thread feeds it.

use crate::place::{Engine, Frame, Hand, Place, PointerNow};
use cranpose::{
    launch_args, preferences, prelude::*, AccessUnit, DecodedFrames, DecoderConfig, DecoderError, DecoderOutput, ExternalSurface,
    VideoCodec, VideoDecoder,
};
use cranpose_xr::{Controller, SpatialExternalSurface, SpatialFrameEffect, SpatialPanel, StereoMode, SubspaceModifier, XrFrame};
use cranvr_proto::{
    clock::now_ns,
    link::ClientLink,
    media::Received,
    message::{ClientDatagram, ClientMsg, Cursor, DesktopParams, DesktopWish, HostMsg, Pointer, DESKTOP_STREAM},
    wire::Codec,
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
        Condvar, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

const PLACE_KEY: &str = "desktop_place";
/// What the client asks for. The Quest 3's hardware decoder takes 3840 by 2160 at 60 pictures a
/// second (the old Quest app ran it).
const WISH_WIDTH: u32 = 3840;
const WISH_FPS: u8 = 60;
/// The pointer goes to the host this often while it points at the panel, if nothing changes
const POINTER_KEEPALIVE: Duration = Duration::from_millis(50);
const KEY_REQUEST_GAP: Duration = Duration::from_millis(250);
/// How often the panel looks at the link's state, and how often the decode thread logs
const POLL: Duration = Duration::from_millis(100);
const LOG_EVERY: Duration = Duration::from_secs(2);
/// The panel's width in dp while the headset makes its surface, so the surface's size is the picture's
const CREATION_WIDTH_DP: f32 = 2560.0;
/// The cursor: a ring that is lifted this far off the surface, in meters, and its diameter in dp
const CURSOR_LIFT: f32 = 0.004;
const CURSOR_SIZE: f32 = 26.0;

/// What the panel needs to know about the stream.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// The host offers the desktop and shows it
    pub visible: bool,
    /// The picture's width over its height
    pub aspect: f32,
}

#[derive(Default)]
struct Counts {
    units: u64,
    key_units: u64,
    bytes: u64,
    skipped: u64,
    refused: u64,
    key_requests: u64,
    pointers_sent: u64,
    cursors: u64,
}

#[derive(Default)]
struct Inner {
    /// This session shows the desktop: a headset asks for it, a flat window does not
    active: bool,
    params: Option<DesktopParams>,
    /// The parameter sets of the stream. A key frame goes to the decoder with them in front.
    config: Vec<u8>,
    surface: Option<ExternalSurface>,
    decoder: Option<VideoDecoder>,
    frames: Option<DecodedFrames>,
    /// What the decoder was made for
    built_for: Option<(ExternalSurface, [u32; 2])>,
    last_frame: Option<u32>,
    waiting_key: bool,
    last_pts_us: u64,
    counts: Counts,
    cursor: Option<Cursor>,
    decoder_error: Option<String>,
}

/// What goes to the host.
#[derive(Default)]
struct Uplink {
    pointer: PointerNow,
    dirty: bool,
    key: bool,
    /// The newest desktop frame number, for the key frame request
    frame: u32,
}

pub struct Desktop {
    inner: Mutex<Inner>,
    uplink: Mutex<Uplink>,
    wake: Condvar,
    /// The channel to the decode thread, while a session runs
    units: Mutex<Option<Sender<Received>>>,
}

/// The desktop of the process. The activity may rebuild the composition, and the link outlives it.
pub fn desktop() -> &'static Desktop {
    static DESKTOP: OnceLock<Desktop> = OnceLock::new();
    DESKTOP.get_or_init(|| Desktop {
        inner: Mutex::new(Inner::default()),
        uplink: Mutex::new(Uplink::default()),
        wake: Condvar::new(),
        units: Mutex::new(None),
    })
}

/// How the cursor shows: `cursor=panel` (default) draws a ring on a small panel over the picture,
/// `cursor=none` draws nothing. The host's `CRANVR_DESKTOP_CURSOR_IN_VIDEO=1` puts the Mac's cursor
/// into the pictures.
fn draws_cursor() -> bool {
    launch_args().string("cursor") != Some("none")
}

impl Desktop {
    /// A session begins. A headset shows the desktop and asks the host for it.
    pub fn session_start(&self, in_headset: bool) -> mpsc::Receiver<Received> {
        let (sender, receiver) = mpsc::channel();
        *self.units.lock().unwrap() = Some(sender);
        let mut inner = self.inner.lock().unwrap();
        // The surface belongs to the composition and stays. Everything else starts again.
        *inner = Inner { surface: inner.surface.take(), active: in_headset, waiting_key: true, ..Inner::default() };
        *self.uplink.lock().unwrap() = Uplink::default();
        receiver
    }

    /// The session ended: the decoder goes and the panel hides.
    pub fn session_end(&self) {
        *self.units.lock().unwrap() = None;
        let mut inner = self.inner.lock().unwrap();
        inner.active = false;
        inner.params = None;
        inner.decoder = None;
        inner.frames = None;
        inner.built_for = None;
    }

    /// What the host says about the desktop stream.
    pub fn on_message(&self, message: &HostMsg) {
        match message {
            HostMsg::Desktop(params) => {
                let mut inner = self.inner.lock().unwrap();
                match params {
                    Some(p) => log::info!(
                        "desktop: the host offers {}x{} pixels ({}x{} points), up to {} a second, {:.0} Mbps, {}, mirrored: {}",
                        p.size[0],
                        p.size[1],
                        p.points[0],
                        p.points[1],
                        p.max_fps,
                        p.bitrate_bps as f64 / 1e6,
                        if p.visible { "visible" } else { "hidden" },
                        p.flipped
                    ),
                    None => log::info!("desktop: the host has no desktop for this client"),
                }
                inner.params = *params;
                inner.waiting_key = true;
                inner.rebuild();
            }
            HostMsg::DecoderConfig { stream, config, .. } if *stream == DESKTOP_STREAM => {
                self.inner.lock().unwrap().config.clone_from(config);
                // The decoder may wait for the parameter sets
                self.inner.lock().unwrap().rebuild();
            }
            _ => {}
        }
    }

    /// The network thread hands over a whole picture. The decode thread takes it.
    pub fn push_unit(&self, unit: Received) {
        if let Some(sender) = self.units.lock().unwrap().as_ref() {
            sender.send(unit).ok();
        }
    }

    /// A picture of the desktop stream is lost: the next one waits for a key frame.
    pub fn on_lost(&self, frame: u32) {
        self.inner.lock().unwrap().waiting_key = true;
        self.request_key_frame(frame);
    }

    fn request_key_frame(&self, frame: u32) {
        let mut uplink = self.uplink.lock().unwrap();
        uplink.key = true;
        uplink.frame = frame;
        drop(uplink);
        self.wake.notify_all();
    }

    /// The Mac's cursor moved. The newest datagram wins.
    pub fn on_cursor(&self, cursor: Cursor) {
        let mut inner = self.inner.lock().unwrap();
        inner.counts.cursors += 1;
        if inner.cursor.is_none_or(|old| cursor.seq.wrapping_sub(old.seq) < u32::MAX / 2) {
            inner.cursor = Some(cursor);
        }
    }

    /// The composition makes the surface: the decoder starts when the stream is ready.
    pub fn set_surface(&self, surface: Option<ExternalSurface>) {
        let mut inner = self.inner.lock().unwrap();
        log::info!("desktop: the surface {}", if surface.is_some() { "is there" } else { "is gone" });
        inner.surface = surface;
        inner.rebuild();
    }

    pub fn view(&self) -> View {
        let inner = self.inner.lock().unwrap();
        match inner.params.filter(|p| inner.active && p.visible) {
            Some(p) => View { visible: true, aspect: p.size[0] as f32 / (p.size[1].max(1)) as f32 },
            None => View { visible: false, aspect: 16.0 / 9.0 },
        }
    }

    /// Where the Mac's cursor is on the picture as u and v, when it is on the desktop's display.
    fn cursor(&self) -> Option<[f32; 2]> {
        self.inner.lock().unwrap().cursor.filter(|c| c.on).map(|c| [c.u, c.v])
    }

    /// The pointer of this frame. It goes to the host when it changed.
    fn set_pointer(&self, pointer: PointerNow) {
        let mut uplink = self.uplink.lock().unwrap();
        if pointer.differs(&uplink.pointer) || pointer.busy() != uplink.pointer.busy() {
            uplink.pointer = pointer;
            uplink.dirty = true;
            drop(uplink);
            self.wake.notify_all();
        }
    }
}

impl Inner {
    /// Makes the decoder that the state calls for, or drops the one that it does not. A decoder
    /// is made when the surface, the stream and its parameter sets are all there.
    fn rebuild(&mut self) {
        let wanted = match (&self.surface, self.params) {
            (Some(surface), Some(p)) if self.active && p.visible && !self.config.is_empty() => Some((surface.clone(), p.size)),
            _ => None,
        };
        if wanted == self.built_for && (wanted.is_none() || self.decoder.is_some()) {
            return;
        }
        // The old decoder stops writing to the surface before the new one starts
        self.decoder = None;
        self.frames = None;
        self.built_for = None;
        let (Some((surface, size)), Some(params)) = (wanted, self.params) else { return };
        let codec = match params.codec {
            Codec::H264 => VideoCodec::H264,
            Codec::Hevc => VideoCodec::Hevc,
            Codec::Av1 => VideoCodec::Av1,
        };
        match VideoDecoder::new(DecoderConfig::new(codec, size[0], size[1], DecoderOutput::Surface(surface.clone()))) {
            Ok(decoder) => {
                log::info!("desktop: decoder started for {}x{}", size[0], size[1]);
                self.frames = Some(decoder.frames());
                self.decoder = Some(decoder);
                self.built_for = Some((surface, size));
                self.decoder_error = None;
                self.waiting_key = true;
                self.last_frame = None;
                // The host sends a key frame when the wish comes and when it hears of a loss
                self.counts.key_requests += 1;
                let d = desktop();
                let mut uplink = d.uplink.lock().unwrap();
                uplink.key = true;
                drop(uplink);
                d.wake.notify_all();
            }
            Err(e) => {
                log::warn!("desktop: no decoder: {e}");
                self.decoder_error = Some(e.to_string());
            }
        }
    }

    fn on_unit(&mut self, unit: &Received) {
        let meta = &unit.meta;
        self.counts.units += 1;
        self.counts.bytes += unit.payload().len() as u64;
        self.counts.key_units += u64::from(meta.key);
        let in_order = self.last_frame.is_none_or(|last| meta.frame == last.wrapping_add(1));
        self.last_frame = Some(meta.frame);
        let d = desktop();
        d.uplink.lock().unwrap().frame = meta.frame;
        if meta.key {
            self.waiting_key = false;
        } else if !in_order {
            self.waiting_key = true;
        }
        if !meta.key && self.waiting_key {
            self.counts.skipped += 1;
            self.counts.key_requests += 1;
            d.request_key_frame(meta.frame);
            return;
        }
        let Some(decoder) = self.decoder.as_mut() else {
            self.counts.skipped += 1;
            return;
        };
        // The decoder wants the parameter sets in front of each key frame
        let mut with_config;
        let data: &[u8] = if meta.key {
            with_config = Vec::with_capacity(self.config.len() + unit.payload().len());
            with_config.extend_from_slice(&self.config);
            with_config.extend_from_slice(unit.payload());
            &with_config
        } else {
            unit.payload()
        };
        // The timestamp rises from one picture to the next
        let pts_us = (unit.completed / 1000).max(self.last_pts_us + 1);
        self.last_pts_us = pts_us;
        if let Err(e) = decoder.submit(AccessUnit { data, timestamp: Duration::from_micros(pts_us), metadata: u64::from(meta.frame) }) {
            self.counts.refused += 1;
            if !matches!(e, DecoderError::Full) {
                log::warn!("desktop: the decoder refused picture {}: {e}", meta.frame);
            }
            self.waiting_key = true;
            self.counts.key_requests += 1;
            d.request_key_frame(meta.frame);
        }
    }
}

/// The numbers for the stats panel and the log.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub running: bool,
    pub size: [u32; 2],
    pub received_fps: f32,
    pub decoded_fps: f32,
    pub mbps: f32,
    pub decode_mean_ms: f32,
    pub decode_last_ms: f32,
    pub decode_max_ms: f32,
    pub dropped: u64,
    pub late: u64,
    pub skipped: u64,
    pub refused: u64,
    pub key_requests: u64,
    pub pointers_sent: u64,
    pub cursors: u64,
    pub error: Option<String>,
}

/// Remembers the counts at the last look, so the next look gives rates.
struct Rates {
    at: Instant,
    units: u64,
    bytes: u64,
    decoded: u64,
    /// The decoder's own count starts from 0 with each decoder
    decoder_decoded: u64,
    /// Pictures that the decoders output in all, as of the last look
    decoded_before: u64,
}

impl Rates {
    fn new() -> Self {
        Self { at: Instant::now(), units: 0, bytes: 0, decoded: 0, decoder_decoded: 0, decoded_before: 0 }
    }

    fn look(&mut self) -> Stats {
        let d = desktop();
        let inner = d.inner.lock().unwrap();
        let seconds = self.at.elapsed().as_secs_f32().max(1e-3);
        self.at = Instant::now();
        let decoder = inner.frames.as_ref().map(DecodedFrames::stats).unwrap_or_default();
        self.decoded += if decoder.decoded >= self.decoder_decoded { decoder.decoded - self.decoder_decoded } else { decoder.decoded };
        self.decoder_decoded = decoder.decoded;
        let c = &inner.counts;
        let ms = |d: Duration| d.as_secs_f32() * 1000.0;
        let stats = Stats {
            running: inner.decoder.is_some(),
            size: inner.params.map_or([0, 0], |p| p.size),
            received_fps: c.units.saturating_sub(self.units) as f32 / seconds,
            decoded_fps: self.decoded.saturating_sub(self.decoded_before) as f32 / seconds,
            mbps: c.bytes.saturating_sub(self.bytes) as f32 * 8.0 / 1e6 / seconds,
            decode_mean_ms: ms(decoder.mean_decode),
            decode_last_ms: ms(decoder.last_decode),
            decode_max_ms: ms(decoder.max_decode),
            dropped: decoder.dropped,
            late: decoder.late,
            skipped: c.skipped,
            refused: c.refused,
            key_requests: c.key_requests,
            pointers_sent: c.pointers_sent,
            cursors: c.cursors,
            error: inner.decoder_error.clone(),
        };
        self.units = c.units;
        self.bytes = c.bytes;
        self.decoded_before = self.decoded;
        stats
    }
}

/// The lines of the stats panel: a label and a value.
pub fn stat_lines() -> Vec<(&'static str, String)> {
    let d = desktop();
    let inner = d.inner.lock().unwrap();
    let Some(params) = inner.params.filter(|_| inner.active) else { return Vec::new() };
    let decoder = inner.frames.as_ref().map(DecodedFrames::stats).unwrap_or_default();
    let ms = |d: Duration| d.as_secs_f32() * 1000.0;
    let c = &inner.counts;
    let state = match (&inner.decoder_error, inner.decoder.is_some(), params.visible) {
        (Some(e), _, _) => format!("no decoder: {e}"),
        (None, true, _) => "decoding".to_string(),
        (None, false, false) => "hidden: a game runs".to_string(),
        (None, false, true) => "waiting for the surface or the stream".to_string(),
    };
    vec![(
        "Desktop",
        format!(
            "{state}; {}x{} pixels; {} pictures ({} key, {} skipped, {} refused), {:.1} MB; decoder {} decoded, {:.2} ms mean, {:.2} ms last, {:.2} ms longest, {} dropped; {} pointer packets sent, {} cursor packets",
            params.size[0],
            params.size[1],
            c.units,
            c.key_units,
            c.skipped,
            c.refused,
            c.bytes as f64 / 1e6,
            decoder.decoded,
            ms(decoder.mean_decode),
            ms(decoder.last_decode),
            ms(decoder.max_decode),
            decoder.dropped,
            c.pointers_sent,
            c.cursors
        ),
    )]
}

/// Takes the pictures from the network threads and feeds the decoder until the session ends. It
/// also logs the desktop's numbers.
pub fn decode_loop(units: &mpsc::Receiver<Received>, stop: &AtomicBool) {
    let d = desktop();
    let mut rates = Rates::new();
    let mut log_at = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        match units.recv_timeout(Duration::from_millis(20)) {
            Ok(unit) => d.inner.lock().unwrap().on_unit(&unit),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if log_at.elapsed() >= LOG_EVERY {
            log_at = Instant::now();
            let s = rates.look();
            if s.size != [0, 0] {
                log::info!(
                    "desktop: {:.1} pictures/s received and {:.1}/s decoded of {}x{}, {:.1} Mbps, decoder {} (decode {:.2} ms mean, {:.2} ms last, {:.2} ms longest, {} dropped, {} late), {} skipped, {} refused, {} key requests, {} pointer packets sent, {} cursor packets{}",
                    s.received_fps,
                    s.decoded_fps,
                    s.size[0],
                    s.size[1],
                    s.mbps,
                    if s.running { "runs" } else { "waits" },
                    s.decode_mean_ms,
                    s.decode_last_ms,
                    s.decode_max_ms,
                    s.dropped,
                    s.late,
                    s.skipped,
                    s.refused,
                    s.key_requests,
                    s.pointers_sent,
                    s.cursors,
                    s.error.map_or(String::new(), |e| format!(", {e}"))
                );
            }
        }
    }
}

/// The widest picture and the most pictures a second that the decoder takes, within the wish.
fn wish() -> DesktopWish {
    let mut wish = DesktopWish { enabled: true, max_width: WISH_WIDTH, max_fps: WISH_FPS };
    if let Ok(limits) = VideoDecoder::capabilities(VideoCodec::Hevc) {
        if let Some(width) = limits.max_width {
            wish.max_width = wish.max_width.min(width);
        }
    }
    if let Some(width) = launch_args().int("desktop_width").and_then(|w| u32::try_from(w).ok()).filter(|w| *w >= 640) {
        wish.max_width = width;
    }
    wish
}

/// Sends the wish for the desktop, then the pointer and the requests for key frames, until the
/// session ends.
pub fn uplink(link: &ClientLink, stop: &AtomicBool) {
    let d = desktop();
    if !d.inner.lock().unwrap().active {
        return;
    }
    let wish = wish();
    log::info!("desktop: asking the host for the desktop at up to {} pixels across and {} pictures a second", wish.max_width, wish.max_fps);
    link.duplex.send_control(&ClientMsg::Desktop(wish)).ok();
    let (mut seq, mut last_send, mut last_key) = (0u32, Instant::now(), Instant::now() - KEY_REQUEST_GAP);
    while !stop.load(Ordering::Relaxed) {
        let (pointer, dirty, key, frame) = {
            let guard = d.uplink.lock().unwrap();
            let (mut u, _) = d.wake.wait_timeout_while(guard, Duration::from_millis(20), |u| !u.dirty && !u.key).unwrap();
            let taken = (u.pointer, std::mem::take(&mut u.dirty), std::mem::take(&mut u.key), u.frame);
            drop(u);
            taken
        };
        if key && last_key.elapsed() >= KEY_REQUEST_GAP {
            last_key = Instant::now();
            link.duplex.send_control(&ClientMsg::RequestKeyFrame { stream: DESKTOP_STREAM, frame }).ok();
        }
        if dirty || (pointer.busy() && last_send.elapsed() >= POINTER_KEEPALIVE) {
            last_send = Instant::now();
            let datagram = Pointer { seq, sent: now_ns(), hit: pointer.hit, trigger: pointer.trigger, scroll: pointer.scroll };
            seq = seq.wrapping_add(1);
            link.duplex.send_datagram(&ClientDatagram::Pointer(datagram)).ok();
            d.inner.lock().unwrap().counts.pointers_sent += 1;
        }
    }
}

/// The place that the preferences keep, or the default.
fn saved_place() -> Place {
    preferences().get(PLACE_KEY).and_then(|text| Place::from_text(&text)).unwrap_or_default()
}

fn save_place(place: &Place) {
    if let Err(e) = preferences().set(PLACE_KEY, &place.to_text()) {
        log::warn!("the desktop panel's place was not saved: {e}");
    }
}

/// One controller as `place.rs` reads it.
fn hand(controller: &Controller) -> Hand {
    Hand {
        aim: controller.aim.map(|aim| aim.pose),
        trigger: controller.trigger.value,
        squeeze: controller.squeeze,
        stick: [controller.thumbstick.x, controller.thumbstick.y],
    }
}

/// What the headset's frame loop shares with the composition: the engine and the last saved place.
struct Shared {
    engine: RefCell<Engine>,
}

/// The desktop panel and the cursor over it. It composes nothing while the host has no desktop for
/// this client, or while a game hides it.
#[composable]
pub fn DesktopPanel() {
    let view = rememberMutableStateOf(|| desktop().view());
    LaunchedEffect((), async move |_| loop {
        let now = desktop().view();
        if view.get_non_reactive() != now {
            view.set(now);
        }
        delay(POLL).await;
    });
    let shared = remember(|| Rc::new(Shared { engine: RefCell::new(Engine::new(saved_place(), 16.0 / 9.0)) })).with(Clone::clone);
    let place = rememberMutableStateOf({
        let shared = Rc::clone(&shared);
        move || shared.engine.borrow().place
    });
    let cursor = rememberMutableStateOf(|| None::<[f32; 2]>);
    {
        let shared = Rc::clone(&shared);
        SpatialFrameEffect(move |frame: &XrFrame| {
            let now = desktop().view();
            let mut engine = shared.engine.borrow_mut();
            engine.aspect = now.aspect;
            if !now.visible {
                desktop().set_pointer(PointerNow::default());
                return;
            }
            let input = Frame {
                hands: [hand(&frame.left_controller), hand(&frame.right_controller)],
                head: frame.device_pose.map(|pose| pose.position),
                dt: frame.period.as_secs_f32(),
            };
            let step = engine.step(&input);
            desktop().set_pointer(step.pointer);
            if place.get_non_reactive() != engine.place {
                place.set(engine.place);
            }
            if step.dropped {
                log::info!("desktop: the panel stays at {:?}", engine.place);
                save_place(&engine.place);
            }
            let at = desktop().cursor();
            if cursor.get_non_reactive() != at {
                cursor.set(at);
            }
        });
    }
    let view = view.get();
    if !view.visible {
        return;
    }
    Surface(place, view.aspect);
    if draws_cursor() {
        CursorRing(place, cursor, view.aspect);
    }
}

/// The surface that the decoder writes into. The headset makes the swapchain at the size of the
/// panel's first composition and keeps it, so the panel starts as large as the picture's pixels and
/// takes its own size when the surface is there.
#[composable]
fn Surface(place: MutableState<Place>, aspect: f32) {
    let created = rememberMutableStateOf(|| false);
    let p = place.get();
    let modifier = if created.get() {
        p.modifier(aspect)
    } else {
        // Far away, so that the empty panel is a speck until the surface is there
        SubspaceModifier::new().size(CREATION_WIDTH_DP, CREATION_WIDTH_DP / aspect).offset(0.0, 0.0, -60_000.0)
    };
    let was_created = created;
    SpatialExternalSurface(StereoMode::Mono, modifier, move |scope| {
        scope.on_surface_created(move |surface| {
            desktop().set_surface(Some(surface));
            was_created.set(true);
        });
        // The decoder stops before the callback returns, as the surface's owner requires
        scope.on_surface_destroyed(|_| desktop().set_surface(None));
    });
}

/// The Mac's cursor as a small panel in front of the picture.
#[composable]
fn CursorRing(place: MutableState<Place>, cursor: MutableState<Option<[f32; 2]>>, aspect: f32) {
    let Some([u, v]) = cursor.get() else { return };
    let p = place.get();
    SpatialPanel(p.cursor_modifier(aspect, [u, v], CURSOR_SIZE, CURSOR_LIFT), || {
        Box(
            Modifier::empty()
                .fill_max_size()
                .background(Color(1.0, 1.0, 1.0, 0.9))
                .rounded_corners(CURSOR_SIZE / 2.0)
                .border(3.0, Color(0.05, 0.05, 0.08, 1.0), RoundedCornerShape::uniform(CURSOR_SIZE / 2.0)),
            BoxSpec::default(),
            || {},
        );
    });
}
