//! The desktop as compositor layers in cranvr's Quest app: one panel per Mac display.
//!
//! In the eye stream the host draws the desktop into each eye's view, and the headset warps that
//! view through the lens. Text is sampled twice and blurs. cranvr's Quest app can show the desktop
//! as an OpenXR quad layer instead: the headset's compositor samples the desktop once, at the
//! display's resolution, with the newest head pose. A head turn then needs no new frame from the
//! Mac. A test on a Quest 3 made 10-point text readable that the eye stream blurred.
//!
//! The link copies ALVR's own direction: the Quest app listens on TCP port `PORT` while it runs,
//! and the host connects to the headset's address when ALVR connects. The store ALVR app does not
//! listen, so the host keeps the desktop in the eye stream for it.
//!
//! Panel 0 shows the display that the host captures for the desktop view. The app's "Add display"
//! button asks for one more panel: the host makes a virtual display of 1920 x 1080 points for it,
//! to the right of the other displays, and removes the display when the app closes the panel. Each
//! panel has its own capture, its own HEVC encoder session and its own decoder in the app.
//!
//! Each capture runs without the cursor, and each picture is mirrored top to bottom on the media
//! hardware: the Quest's compositor reads a decoder's surface bottom row first. ScreenCaptureKit
//! sends a frame only when the display changes, so a still desktop costs nothing. The app reports
//! for each panel how the head sees it and how many pixels across the headset can show of it. The
//! host spends the Mac's work where the eye looks:
//! - a panel near the middle of the view gets up to 60 frames per second, one at the edge of the
//!   view 15;
//! - a panel out of view gets no frames, and its capture stops after a second;
//! - in automatic quality a panel's picture is the narrowest of 1920, 2560 and 3840 pixels that
//!   covers what the headset can show of it: a sharper picture after the need held for a
//!   second, a coarser one after five;
//! - every capture stops while a game runs, and while the headset sends no reports;
//! - the panel that the head points at gets a sharp inset: a second stream of only the part of
//!   its display around where the head points, 1920 x 1080 pixels at the full density that the
//!   headset can show there. The panel's own picture then needs only half that density. The
//!   inset moves when the head points a quarter of its size away from its middle. The app shows
//!   the inset as a layer over the panel. The Quest 3 has no eye tracking, so the head's
//!   direction stands for the eyes'.
//!
//! Messages on TCP: one byte of type, four bytes of payload length (little endian), the payload.
//! - app to host, `HELLO`: the protocol version (u32);
//! - host to app, `CONFIG`: the panel (u8), the codec (u8, 0 = HEVC), width and height (u32
//!   each). A new `CONFIG` for a panel starts a new stream for it;
//! - host to app, `FRAME`: the panel (u8), the capture time in ns (u64), 1 for a key frame (u8),
//!   then Annex-B NAL units, with the parameter sets in front of each key frame;
//! - host to app, `VISIBLE`: 0 while a game runs, 1 otherwise (u8);
//! - host to app, `STATE`: the panel (u8), 1 while paused (u8), the quality (u8: 0 automatic, or
//!   1 + an index into `WIDTHS`);
//! - host to app, `CLOSED`: the panel (u8) is gone;
//! - app to host, `PAUSE`: the panel (u8), 1 to pause and 0 to resume (u8);
//! - app to host, `QUALITY`: the panel (u8), the quality (u8, as in `STATE`);
//! - app to host, `ADD`: no payload;
//! - app to host, `CLOSE`: the panel (u8). Panel 0 stays;
//! - host to app, `INSET_CONFIG`: the panel (u8), width and height (u32 each): a new inset stream;
//! - host to app, `INSET_FRAME`: the panel (u8), the part of the display that the frame shows as
//!   u0, v0, u1, v1 (four f32, from the top left corner (0, 0) to the bottom right corner (1, 1)),
//!   then the capture time, the key frame flag and the NAL units as in `FRAME`;
//! - host to app, `INSET_CLOSED`: the panel (u8) has no inset any more.
//!
//! The cursor goes over UDP to the app's `CURSOR_PORT`: a sequence number (u32), the position on
//! the display from its top left corner (0, 0) to its bottom right corner (1, 1) as two f32, 1
//! when the cursor is on a panel's display (u8), and that panel (u8). The newest packet wins, so
//! the cursor never waits behind a large frame. The app draws the cursor itself every refresh.
//!
//! The app places the panels itself, in the headset's stage space: ALVR's server recenters the
//! poses that the host sees, so a place from the host would put a panel elsewhere. The app tests
//! the controllers' rays against the panels it shows, and every refresh it sends the host's
//! `POINTER_PORT` over UDP: a sequence number (u32), the panel the active controller points at
//! (u8), the position on it (two f32), flags (u8: 1 when the controller points at a panel), the
//! trigger and the thumbstick's y (f32 each), then a count (u8) and for each panel its number
//! (u8), how the head sees it (u8: 0 out of view, 1 at the edge, 2 near the middle) and the pixels
//! across that the headset can show of it (u16), then the panel that the head points at (u8, 255
//! for none), where on it (two f32) and its pixels across (u16). The host moves the Mac's mouse to
//! the position.

use crate::{
    desktop::{Capture, CaptureSpec, Pointer},
    virtual_display::DisplayProcess,
};
use alvr_common::{info, warn};
use cranvr_vt::{EncodedFrame, Encoder, EncoderConfig, Flipper};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpStream, UdpSocket},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

pub const PORT: u16 = 9950;
pub const CURSOR_PORT: u16 = 9951;
pub const POINTER_PORT: u16 = 9952;
pub const VERSION: u32 = 2;

const MSG_HELLO: u8 = 0;
const MSG_CONFIG: u8 = 1;
const MSG_FRAME: u8 = 3;
const MSG_VISIBLE: u8 = 4;
const MSG_STATE: u8 = 5;
const MSG_CLOSED: u8 = 6;
const MSG_PAUSE: u8 = 16;
const MSG_QUALITY: u8 = 17;
const MSG_ADD: u8 = 18;
const MSG_CLOSE: u8 = 19;
const MSG_INSET_CONFIG: u8 = 7;
const MSG_INSET_FRAME: u8 = 8;
const MSG_INSET_CLOSED: u8 = 9;

/// The inset's size in pixels
const INSET_SIZE: (usize, usize) = (1920, 1080);
/// The inset moves when the head points this share of its size away from its middle
const INSET_MOVE: f64 = 0.25;

/// The picture widths a panel chooses from. A 6K display at its full 6016 pixels and 60 frames
/// per second would ask too much of the Mac's encoder and the Quest's decoder. A Quest 3 shows a
/// panel of 76 degrees with about 1900 pixels across; the compositor's supersampling reads text
/// best from twice as many.
const WIDTHS: [usize; 3] = [1920, 2560, 3840];
/// Captures per second at most for a panel near the middle of the view: video on the Mac plays
/// at up to 60
const FPS: i32 = 60;
/// Captures per second at most for a panel at the edge of the view
const SIDE_FPS: i32 = 15;
/// Each panel's average bitrate. A still desktop needs far less.
const MBPS: u64 = 40;
/// A report older than this counts as out of view: the headset sends none
const STALE_VIEW: Duration = Duration::from_millis(500);
/// A capture stops when its panel stays out of the head's view this long
const OUT_OF_VIEW: Duration = Duration::from_secs(1);
/// Automatic quality makes a panel's picture sharper after the new need held this long, and
/// coarser only after the longer time: a short look from afar should not cost a sharp picture
const SHARPER_AFTER: Duration = Duration::from_secs(1);
const COARSER_AFTER: Duration = Duration::from_secs(5);
/// An added display's size in points; it has twice as many pixels each way
const EXTRA_SIZE: (u32, u32) = (1920, 1080);
const MAX_PANELS: usize = 4;
/// How long the host tries to reach the app after ALVR connects
const CONNECT_TIME: Duration = Duration::from_secs(15);

/// How the head sees a panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gaze {
    Out,
    Side,
    Center,
}

/// The app's report on one panel.
#[derive(Clone, Copy, Debug)]
struct View {
    gaze: Gaze,
    /// Pixels across that the headset can show of the panel
    wanted: u16,
    at: Instant,
}

/// What the frame loop and the layer link share.
#[derive(Default)]
pub struct LayerState {
    /// A game runs: the app hides the panels
    pub game: AtomicBool,
    /// The app shows the panels: the eye stream leaves the screen out
    pub active: AtomicBool,
    /// Frames the layer link sent since the last stats line
    pub frames: AtomicU32,
    /// Each panel's display, for the pointer and the cursor
    displays: Mutex<Vec<(u8, u32)>>,
    /// The app's newest report on each panel
    views: Mutex<HashMap<u8, View>>,
    /// Where the head points
    look: Mutex<Option<HeadPoint>>,
}

/// Where the head points: the panel, u and v on it, its pixels across, and when the app said so.
type HeadPoint = (u8, f32, f32, u16, Instant);

/// A sharp stream of the part of a panel's display around where the head points.
struct Inset {
    panel: u8,
    capture: Capture,
    encoder: Encoder,
    encoded: Receiver<EncodedFrame>,
    /// The part that the capture shows: x, y, width, height in points
    rect: [f64; 4],
    /// A move that ScreenCaptureKit has not taken yet
    pending: Option<([f64; 4], Arc<AtomicBool>)>,
    /// Frames up to this number may show the part before a move
    after_seq: u64,
    last_seq: u64,
    index: i64,
    key: bool,
    /// The part, as u0, v0, u1, v1, for each frame in the encoder, by its capture time
    parts: HashMap<u64, [f32; 4]>,
}

impl Inset {
    fn open(panel: u8, display_id: u32, rect: [f64; 4], notify: &SyncSender<()>) -> Result<Self, String> {
        let capture = Capture::start_with(
            Some(display_id),
            CaptureSpec {
                max_width: INSET_SIZE.0,
                fps: FPS,
                cursor: false,
                yuv: true,
                notify: Some(notify.clone()),
                source: Some(rect),
                size: Some(INSET_SIZE),
            },
        )?;
        let (width, height) = capture.size;
        let (sender, encoded) = mpsc::channel();
        let encoder = Encoder::new(
            &EncoderConfig { width, height, fps: FPS as f32, bitrate_bps: MBPS * 1_000_000, full_range: true, strict_rate: false },
            sender,
        )?;
        Ok(Self { panel, capture, encoder, encoded, rect, pending: None, after_seq: 0, last_seq: 0, index: 0, key: true, parts: HashMap::new() })
    }

    fn part(&self, rect: [f64; 4]) -> [f32; 4] {
        let (w, h) = self.capture.points;
        [(rect[0] / w) as f32, (rect[1] / h) as f32, ((rect[0] + rect[2]) / w) as f32, ((rect[1] + rect[3]) / h) as f32]
    }
}

/// The inset's part of a display of `points` size: INSET_SIZE pixels at `wanted` pixels across the
/// whole display, around (u, v).
fn inset_rect(points: (f64, f64), (u, v): (f32, f32), wanted: u16) -> [f64; 4] {
    let (pw, ph) = points;
    let w = (pw * INSET_SIZE.0 as f64 / wanted.max(1) as f64).min(pw);
    let h = (w * INSET_SIZE.1 as f64 / INSET_SIZE.0 as f64).min(ph);
    let x = (u as f64 * pw - w / 2.0).clamp(0.0, pw - w);
    let y = (v as f64 * ph - h / 2.0).clamp(0.0, ph - h);
    [x, y, w, h]
}

/// Starts the link to the app at `ip`; panel 0 shows the display `display_id`. It ends when
/// `stop` is set, when the app does not answer, or when the connection breaks.
pub fn start(ip: IpAddr, display_id: u32, state: Arc<LayerState>, stop: Arc<AtomicBool>) {
    thread::Builder::new()
        .name("desktop-layer".into())
        .spawn(move || {
            match run(ip, display_id, &state, &stop) {
                Ok(()) => info!("Desktop layer: stopped"),
                Err(e) => info!("Desktop layer: {e}"),
            }
            state.active.store(false, Ordering::Relaxed);
            state.displays.lock().unwrap().clear();
        })
        .ok();
}

enum Command {
    Pause(u8, bool),
    Quality(u8, u8),
    Add,
    Close(u8),
}

/// One panel's display, capture and encoder.
struct Panel {
    id: u8,
    display_id: u32,
    /// The display that the host made for this panel; it goes away with the panel
    _virtual: Option<DisplayProcess>,
    capture: Capture,
    encoder: Encoder,
    encoded: Receiver<EncodedFrame>,
    /// An index into WIDTHS
    level: usize,
    /// 0 for automatic, else 1 + an index into WIDTHS
    quality: u8,
    paused: bool,
    /// When the head last saw the panel
    seen: Instant,
    /// The level that automatic quality asks for, and since when
    wanted: Option<(usize, Instant)>,
    last_seq: u64,
    index: i64,
    key: bool,
}

fn capture_and_encoder(
    display_id: u32,
    level: usize,
    notify: &SyncSender<()>,
) -> Result<(Capture, Encoder, Receiver<EncodedFrame>), String> {
    let capture = Capture::start_with(
        Some(display_id),
        CaptureSpec { max_width: WIDTHS[level], fps: FPS, cursor: false, yuv: true, notify: Some(notify.clone()), source: None, size: None },
    )?;
    let (width, height) = capture.size;
    let (sender, encoded) = mpsc::channel();
    let encoder = Encoder::new(
        &EncoderConfig { width, height, fps: FPS as f32, bitrate_bps: MBPS * 1_000_000, full_range: true, strict_rate: false },
        sender,
    )?;
    Ok((capture, encoder, encoded))
}

impl Panel {
    fn open(id: u8, display_id: u32, level: usize, notify: &SyncSender<()>) -> Result<Self, String> {
        let (capture, encoder, encoded) = capture_and_encoder(display_id, level, notify)?;
        Ok(Self {
            id,
            display_id,
            _virtual: None,
            capture,
            encoder,
            encoded,
            level,
            quality: 0,
            paused: false,
            seen: Instant::now(),
            wanted: None,
            last_seq: 0,
            index: 0,
            key: true,
        })
    }

    /// A new capture and encoder at the width WIDTHS[level].
    fn reopen(&mut self, level: usize, notify: &SyncSender<()>) -> Result<(), String> {
        let (capture, encoder, encoded) = capture_and_encoder(self.display_id, level, notify)?;
        (self.capture, self.encoder, self.encoded) = (capture, encoder, encoded);
        (self.level, self.last_seq, self.key, self.wanted) = (level, 0, true, None);
        info!("Desktop layer: panel {} at {}x{}", self.id, self.capture.size.0, self.capture.size.1);
        Ok(())
    }

    fn config(&self) -> Vec<u8> {
        let mut config = vec![self.id, 0];
        config.extend(self.capture.size.0.to_le_bytes());
        config.extend(self.capture.size.1.to_le_bytes());
        config
    }

    fn state(&self) -> [u8; 3] {
        [self.id, self.paused as u8, self.quality]
    }
}

fn run(ip: IpAddr, display_id: u32, state: &LayerState, stop: &AtomicBool) -> Result<(), String> {
    let mut stream = connect(ip, stop)?;
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let (kind, payload) = read_message(&mut stream).map_err(|e| format!("no hello from the app: {e}"))?;
    let version = payload.get(..4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    if kind != MSG_HELLO || version != Some(VERSION) {
        return Err(format!("the app speaks another protocol ({kind}, {version:?}); install the same version of cranvr on both"));
    }
    stream.set_read_timeout(None).ok();

    let (notify, captured) = mpsc::sync_channel(1);
    let first = Panel::open(0, display_id, WIDTHS.len() - 1, &notify)?;
    write_message(&mut stream, MSG_CONFIG, &first.config())?;
    write_message(&mut stream, MSG_STATE, &first.state())?;
    let (width, height) = first.capture.size;
    info!("Desktop layer: the app at {ip} shows display {display_id} as a layer, {width}x{height}");
    let mut panels = vec![first];
    *state.displays.lock().unwrap() = vec![(0, display_id)];
    state.active.store(true, Ordering::Relaxed);

    let (command_sender, commands) = mpsc::channel();
    let reader = stream.try_clone().map_err(|e| e.to_string())?;
    let helpers_stop = AtomicBool::new(false);
    thread::scope(|scope| {
        thread::Builder::new().name("desktop-commands".into()).spawn_scoped(scope, || read_loop(reader, command_sender)).ok();
        thread::Builder::new().name("desktop-cursor".into()).spawn_scoped(scope, || cursor_loop(ip, state, &helpers_stop)).ok();
        thread::Builder::new()
            .name("desktop-pointer".into())
            .spawn_scoped(scope, || pointer_loop(ip, state, &helpers_stop))
            .ok();
        let result = Flipper::new()
            .and_then(|flipper| send_loop(&mut stream, &mut panels, (&notify, captured), &commands, &flipper, state, stop));
        helpers_stop.store(true, Ordering::Relaxed);
        stream.shutdown(Shutdown::Both).ok();
        result
    })
}

/// Turns the app's messages into commands for the send loop. Ends when the connection closes.
fn read_loop(mut stream: TcpStream, commands: Sender<Command>) {
    while let Ok((kind, payload)) = read_message(&mut stream) {
        let command = match (kind, payload.as_slice()) {
            (MSG_PAUSE, [id, paused, ..]) => Command::Pause(*id, *paused != 0),
            (MSG_QUALITY, [id, quality, ..]) => Command::Quality(*id, *quality),
            (MSG_ADD, _) => Command::Add,
            (MSG_CLOSE, [id, ..]) => Command::Close(*id),
            _ => continue,
        };
        if commands.send(command).is_err() {
            break;
        }
    }
}

fn send_loop(
    stream: &mut TcpStream,
    panels: &mut Vec<Panel>,
    (notify, captured): (&SyncSender<()>, Receiver<()>),
    commands: &Receiver<Command>,
    flipper: &Flipper,
    state: &LayerState,
    stop: &AtomicBool,
) -> Result<(), String> {
    let mut last_visible = None;
    let mut next_id = 1u8;
    let mut inset: Option<Inset> = None;
    let start = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        captured.recv_timeout(Duration::from_millis(10)).ok();

        while let Ok(command) = commands.try_recv() {
            match command {
                Command::Pause(id, paused) => {
                    if let Some(panel) = panels.iter_mut().find(|p| p.id == id) {
                        panel.paused = paused;
                        info!("Desktop layer: panel {id} {}", if paused { "paused" } else { "runs again" });
                        write_message(stream, MSG_STATE, &panel.state())?;
                    }
                }
                Command::Quality(id, quality) => {
                    if let Some(panel) = panels.iter_mut().find(|p| p.id == id) {
                        panel.quality = quality.min(WIDTHS.len() as u8);
                        if let Some(level) = (panel.quality as usize).checked_sub(1).filter(|&l| l != panel.level) {
                            panel.reopen(level, notify)?;
                            write_message(stream, MSG_CONFIG, &panel.config())?;
                        }
                        write_message(stream, MSG_STATE, &panel.state())?;
                    }
                }
                Command::Add if panels.len() < MAX_PANELS => {
                    let (w, h) = EXTRA_SIZE;
                    // macOS tells displays apart by their serial numbers: each display of this
                    // process gets its own
                    static SERIAL: AtomicU32 = AtomicU32::new(100);
                    match DisplayProcess::start(w, h, SERIAL.fetch_add(1, Ordering::Relaxed)) {
                        Ok(display) => match Panel::open(next_id, display.id, 1, notify) {
                            Ok(mut panel) => {
                                info!("Desktop layer: panel {next_id} shows the new display {}: {}", display.id, display.describe());
                                panel._virtual = Some(display);
                                write_message(stream, MSG_CONFIG, &panel.config())?;
                                write_message(stream, MSG_STATE, &panel.state())?;
                                panels.push(panel);
                                next_id = next_id.wrapping_add(1).max(1);
                            }
                            // The display goes with `display`; the other panels keep running
                            Err(e) => warn!("Desktop layer: no capture of the new display {}: {e}", display.id),
                        },
                        Err(e) => warn!("Desktop layer: no new display: {e}"),
                    }
                }
                Command::Add => warn!("Desktop layer: {MAX_PANELS} panels at most"),
                Command::Close(id) => {
                    if id != 0 && panels.iter().any(|p| p.id == id) {
                        panels.retain(|p| p.id != id);
                        write_message(stream, MSG_CLOSED, &[id])?;
                        info!("Desktop layer: panel {id} closed");
                    }
                }
            }
            *state.displays.lock().unwrap() = panels.iter().map(|p| (p.id, p.display_id)).collect();
        }

        let visible = !state.game.load(Ordering::Relaxed);
        if Some(visible) != last_visible {
            write_message(stream, MSG_VISIBLE, &[visible as u8])?;
            last_visible = Some(visible);
        }

        let views = state.views.lock().unwrap().clone();
        // The panel that the head points at, while it needs more than the coarsest picture
        let look = (*state.look.lock().unwrap())
            .filter(|l| l.4.elapsed() < STALE_VIEW && l.3 as usize > WIDTHS[0] * 5 / 4 && visible)
            .filter(|l| panels.iter().any(|p| p.id == l.0 && !p.paused));
        for panel in panels.iter_mut() {
            let view = views.get(&panel.id).copied().filter(|v| v.at.elapsed() < STALE_VIEW);
            let gaze = view.map_or(Gaze::Out, |v| v.gaze);
            if gaze != Gaze::Out {
                panel.seen = Instant::now();
            }
            let run = visible && !panel.paused && panel.seen.elapsed() < OUT_OF_VIEW;
            panel.capture.set_running(run);
            panel.capture.set_fps(if gaze == Gaze::Side { SIDE_FPS } else { FPS });

            if let (0, Some(view)) = (panel.quality, view.filter(|v| v.gaze != Gaze::Out)) {
                // With an inset the panel's own picture needs half the density
                let wanted = if look.is_some_and(|l| l.0 == panel.id) { view.wanted / 2 } else { view.wanted } as usize;
                let level = WIDTHS.iter().position(|&w| w >= wanted).unwrap_or(WIDTHS.len() - 1);
                match panel.wanted {
                    Some((wanted, since)) if wanted == level => {
                        let settle = if level > panel.level { SHARPER_AFTER } else { COARSER_AFTER };
                        if level != panel.level && since.elapsed() > settle {
                            panel.reopen(level, notify)?;
                            write_message(stream, MSG_CONFIG, &panel.config())?;
                        }
                    }
                    _ => panel.wanted = Some((level, Instant::now())),
                }
            }

            // Only frames the eye can see
            if run && gaze != Gaze::Out && panel.encoder.in_flight() < 2 {
                if let Some((seq, image)) = panel.capture.latest().filter(|(seq, _)| *seq != panel.last_seq) {
                    // The Quest's compositor reads the decoded picture bottom row first: send it
                    // mirrored, so it shows upright
                    let flipped = panel.encoder.pixel_buffer().ok_or("the encoder has no buffer pool")?;
                    flipper.flip(&image, &flipped)?;
                    panel.encoder.encode(&flipped, panel.index, start.elapsed().as_nanos() as u64, panel.key)?;
                    (panel.last_seq, panel.key) = (seq, false);
                    panel.index += 1;
                }
            }
            while let Ok(frame) = panel.encoded.try_recv() {
                let mut payload = Vec::with_capacity(frame.data.len() + 512);
                payload.push(panel.id);
                payload.extend(frame.timestamp_ns.to_le_bytes());
                payload.push(frame.is_idr as u8);
                if let Some(config) = &frame.config_nals {
                    payload.extend(config);
                }
                payload.extend(&frame.data);
                write_message(stream, MSG_FRAME, &payload)?;
                state.frames.fetch_add(1, Ordering::Relaxed);
            }
        }
        update_inset(stream, &mut inset, look, panels, (notify, flipper, start))?;
    }
    Ok(())
}

/// Opens, moves, closes and sends the inset for `look`.
fn update_inset(
    stream: &mut TcpStream,
    inset: &mut Option<Inset>,
    look: Option<HeadPoint>,
    panels: &[Panel],
    (notify, flipper, start): (&SyncSender<()>, &Flipper, Instant),
) -> Result<(), String> {
    let target = look.and_then(|l| panels.iter().find(|p| p.id == l.0).map(|p| (p, l)));
    let Some((panel, (_, u, v, wanted, _))) = target else {
        if let Some(old) = inset.take() {
            write_message(stream, MSG_INSET_CLOSED, &[old.panel])?;
        }
        return Ok(());
    };
    let rect = inset_rect(panel.capture.points, (u, v), wanted);
    match inset {
        Some(current) if current.panel == panel.id => {
            // Move when the head points too far from the middle, or the needed size changed
            let r = current.pending.as_ref().map_or(current.rect, |p| p.0);
            let (cx, cy) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
            let (lx, ly) = (u as f64 * current.capture.points.0, v as f64 * current.capture.points.1);
            let strayed = (lx - cx).abs() > r[2] * INSET_MOVE || (ly - cy).abs() > r[3] * INSET_MOVE;
            let resized = (rect[2] - r[2]).abs() > r[2] * 0.25;
            if (strayed || resized) && current.pending.is_none() {
                let done = Arc::new(AtomicBool::new(false));
                current.capture.set_source(rect, Arc::clone(&done));
                current.pending = Some((rect, done));
            }
        }
        _ => {
            if let Some(old) = inset.take() {
                write_message(stream, MSG_INSET_CLOSED, &[old.panel])?;
            }
            match Inset::open(panel.id, panel.display_id, rect, notify) {
                Ok(new) => {
                    let mut config = vec![new.panel];
                    config.extend(new.capture.size.0.to_le_bytes());
                    config.extend(new.capture.size.1.to_le_bytes());
                    write_message(stream, MSG_INSET_CONFIG, &config)?;
                    *inset = Some(new);
                }
                Err(e) => warn!("Desktop layer: no inset: {e}"),
            }
        }
    }
    let Some(current) = inset.as_mut() else { return Ok(()) };
    if let Some((rect, done)) = &current.pending {
        if done.load(Ordering::Relaxed) {
            current.rect = *rect;
            current.after_seq = current.capture.latest().map_or(0, |l| l.0);
            current.pending = None;
        }
    }
    if current.pending.is_none() && current.encoder.in_flight() < 2 {
        let fresh = |seq: u64| seq != current.last_seq && seq > current.after_seq;
        if let Some((seq, image)) = current.capture.latest().filter(|(seq, _)| fresh(*seq)) {
            let flipped = current.encoder.pixel_buffer().ok_or("the inset encoder has no buffer pool")?;
            flipper.flip(&image, &flipped)?;
            let ts = start.elapsed().as_nanos() as u64;
            current.parts.insert(ts, current.part(current.rect));
            current.encoder.encode(&flipped, current.index, ts, current.key)?;
            (current.last_seq, current.key) = (seq, false);
            current.index += 1;
        }
    }
    while let Ok(frame) = current.encoded.try_recv() {
        let Some(part) = current.parts.remove(&frame.timestamp_ns) else { continue };
        let mut payload = Vec::with_capacity(frame.data.len() + 512);
        payload.push(current.panel);
        for f in part {
            payload.extend(f.to_le_bytes());
        }
        payload.extend(frame.timestamp_ns.to_le_bytes());
        payload.push(frame.is_idr as u8);
        if let Some(config) = &frame.config_nals {
            payload.extend(config);
        }
        payload.extend(&frame.data);
        write_message(stream, MSG_INSET_FRAME, &payload)?;
    }
    // Parts of frames that the encoder dropped
    if current.parts.len() > 8 {
        let newest = current.parts.keys().copied().max().unwrap_or(0);
        current.parts.retain(|ts, _| *ts + 1_000_000_000 > newest);
    }
    Ok(())
}

/// Moves the Mac's mouse to where the app says the active controller points, and keeps the app's
/// reports on the panels. Ends with `stop`.
fn pointer_loop(ip: IpAddr, state: &LayerState, stop: &AtomicBool) {
    // The link before this one may still hold the port for a moment
    let deadline = Instant::now() + Duration::from_secs(2);
    let socket = loop {
        match UdpSocket::bind(("0.0.0.0", POINTER_PORT)) {
            Ok(s) => break s,
            Err(e) if Instant::now() > deadline => {
                warn!("Desktop layer: cannot listen for the controller on port {POINTER_PORT}: {e}");
                return;
            }
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    };
    socket.set_read_timeout(Some(Duration::from_millis(100))).ok();
    let first_display = state.displays.lock().unwrap().first().map_or(0, |d| d.1);
    let mut pointer = Pointer::new(first_display);
    let mut last: Option<u32> = None;
    let mut buffer = [0u8; 256];
    while !stop.load(Ordering::Relaxed) {
        let Ok((len, from)) = socket.recv_from(&mut buffer) else { continue };
        if len < 23 || from.ip() != ip {
            continue;
        }
        let seq = u32::from_le_bytes(buffer[..4].try_into().unwrap());
        // UDP may swap packets: an older one would move the mouse back. A far older number means
        // a new app session that counts from 0 again.
        if last.is_some_and(|l| l.wrapping_sub(seq) < 1000) {
            continue;
        }
        last = Some(seq);
        let f = |i: usize| f32::from_le_bytes(buffer[i..i + 4].try_into().unwrap());
        let now = Instant::now();
        let count = buffer[22] as usize;
        {
            let mut views = state.views.lock().unwrap();
            for entry in buffer[23..len].as_chunks::<4>().0.iter().take(count) {
                let gaze = match entry[1] {
                    2 => Gaze::Center,
                    1 => Gaze::Side,
                    _ => Gaze::Out,
                };
                views.insert(entry[0], View { gaze, wanted: u16::from_le_bytes([entry[2], entry[3]]), at: now });
            }
        }
        // Where the head points, after the reports
        let after = 23 + 4 * count;
        if len >= after + 11 {
            let b = &buffer[after..after + 11];
            *state.look.lock().unwrap() = (b[0] != 255).then(|| {
                let f = |i: usize| f32::from_le_bytes(b[i..i + 4].try_into().unwrap());
                (b[0], f(1), f(5), u16::from_le_bytes([b[9], b[10]]), now)
            });
        }
        let display = state.displays.lock().unwrap().iter().find(|d| d.0 == buffer[4]).map(|d| d.1);
        if let Some(display) = display {
            pointer.set_display(display);
        }
        let hit = (buffer[13] & 1 != 0 && display.is_some()).then(|| [f(5), f(9)]);
        pointer.update(hit, f(14), f(18));
    }
}

fn connect(ip: IpAddr, stop: &AtomicBool) -> Result<TcpStream, String> {
    let address = SocketAddr::new(ip, PORT);
    let deadline = Instant::now() + CONNECT_TIME;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err("the headset left before the app answered".into());
        }
        match TcpStream::connect_timeout(&address, Duration::from_secs(1)) {
            Ok(stream) => return Ok(stream),
            Err(e) if Instant::now() > deadline => {
                return Err(format!("the headset app offers no desktop layer ({e}); the desktop stays in the eye stream"))
            }
            Err(_) => thread::sleep(Duration::from_millis(500)),
        }
    }
}

fn write_message(stream: &mut TcpStream, kind: u8, payload: &[u8]) -> Result<(), String> {
    let mut header = [0u8; 5];
    header[0] = kind;
    header[1..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    stream
        .write_all(&header)
        .and_then(|()| stream.write_all(payload))
        .map_err(|e| format!("the app closed the connection ({e})"))
}

fn read_message(stream: &mut TcpStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    stream.read_exact(&mut header)?;
    let len = u32::from_le_bytes(header[1..].try_into().unwrap()) as usize;
    let mut payload = vec![0u8; len.min(1 << 20)];
    stream.read_exact(&mut payload)?;
    Ok((header[0], payload))
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
    fn CGEventCreate(source: *const std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CGEventGetLocation(event: *mut std::ffi::c_void) -> CGPoint;
    fn CGDisplayBounds(display: u32) -> CGRect;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *mut std::ffi::c_void);
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

/// Sends the cursor position on the panel whose display holds it, every 2 ms when it moves and
/// every 50 ms anyway, so a lost packet does not leave the app with an old position.
fn cursor_loop(ip: IpAddr, state: &LayerState, stop: &AtomicBool) {
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else { return };
    if socket.connect(SocketAddr::new(ip, CURSOR_PORT)).is_err() {
        return;
    }
    let (mut seq, mut last, mut sent) = (0u32, None, Instant::now());
    // For tests, instead of the real cursor: CRANVR_CURSOR_TEST=circle sends a cursor that circles
    // the middle of panel 0 once a second; CRANVR_CURSOR_TEST=U,V holds it still at (U, V)
    let test = std::env::var("CRANVR_CURSOR_TEST").ok();
    let still = test.as_deref().and_then(|t| t.split_once(',')).and_then(|(u, v)| Some((u.parse::<f64>().ok()?, v.parse::<f64>().ok()?)));
    let start = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let displays = state.displays.lock().unwrap().clone();
        let bounds: Vec<(u8, CGRect)> = displays.iter().map(|&(panel, d)| (panel, unsafe { CGDisplayBounds(d) })).collect();
        let point = match (&test, still, bounds.first()) {
            (_, Some((u, v)), Some((_, b))) => Some(CGPoint { x: b.origin.x + b.size.x * u, y: b.origin.y + b.size.y * v }),
            (Some(_), None, Some((_, b))) => {
                let a = start.elapsed().as_secs_f64() * std::f64::consts::TAU;
                Some(CGPoint { x: b.origin.x + b.size.x * (0.5 + 0.2 * a.cos()), y: b.origin.y + b.size.y * (0.5 + 0.2 * a.sin()) })
            }
            _ => mouse_location(),
        };
        if let Some(point) = point {
            let inside = |b: &CGRect| {
                (b.origin.x..b.origin.x + b.size.x).contains(&point.x) && (b.origin.y..b.origin.y + b.size.y).contains(&point.y)
            };
            let (panel, u, v, on) = match bounds.iter().find(|(_, b)| inside(b)) {
                Some((panel, b)) => (
                    *panel,
                    ((point.x - b.origin.x) / b.size.x.max(1.0)) as f32,
                    ((point.y - b.origin.y) / b.size.y.max(1.0)) as f32,
                    true,
                ),
                None => (0, 0.0, 0.0, false),
            };
            let now = Some((panel, u.to_bits(), v.to_bits(), on));
            if now != last || sent.elapsed() > Duration::from_millis(50) {
                let mut packet = [0u8; 14];
                packet[..4].copy_from_slice(&seq.to_le_bytes());
                packet[4..8].copy_from_slice(&u.to_le_bytes());
                packet[8..12].copy_from_slice(&v.to_le_bytes());
                packet[12] = on as u8;
                packet[13] = panel;
                if let Err(e) = socket.send(&packet) {
                    warn!("Desktop cursor: {e}");
                }
                (seq, last, sent) = (seq.wrapping_add(1), now, Instant::now());
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inset_covers_the_needed_density_around_the_head_point() {
        // A 3008 x 1692 point display that the headset can show at 3800 pixels across
        let rect = inset_rect((3008.0, 1692.0), (0.5, 0.5), 3800);
        assert!((rect[2] - 3008.0 * 1920.0 / 3800.0).abs() < 1e-6);
        assert!((rect[0] + rect[2] / 2.0 - 1504.0).abs() < 1e-6 && (rect[1] + rect[3] / 2.0 - 846.0).abs() < 1e-6);
        // near a corner the inset stays on the display
        let corner = inset_rect((3008.0, 1692.0), (0.99, 0.01), 3800);
        assert!((corner[0] + corner[2] - 3008.0).abs() < 1e-6 && corner[1] == 0.0);
        // a far panel needs fewer pixels than the inset has: the inset covers the whole display
        let whole = inset_rect((3008.0, 1692.0), (0.5, 0.5), 1500);
        assert_eq!(whole[2], 3008.0);
    }
}
