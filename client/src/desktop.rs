//! The Mac's desktop in the headset, as a panel in the room.
//!
//! The host sends the display as its own HEVC stream at the display's full pixel size (stream
//! `DESKTOP_STREAM`, see `host/src/headset/native/desktop.rs`). A second `VideoDecoder` in surface
//! mode writes the pictures into the surface of a `SpatialExternalSurface`. The headset's
//! compositor shows that surface as a layer of its own and samples the picture once at the
//! display's resolution, so text stays sharp. The panel is mono.
//!
//! The controllers work as the mouse. Cranpose finds where a controller's ray meets the surface and
//! reports it as pointer events in dp from the surface's top left corner (`on_pointer_event`). This
//! file turns each event into a point on the picture (u and v from 0 to 1) and the mouse buttons
//! that are down (left, right, middle). It sends them to the host as `ClientDatagram::Mouse`
//! datagrams. The host moves the Mac's mouse, clicks and scrolls. The pointer events carry no scroll, so the thumbstick's lean comes
//! from the headset's frame while the ray is on the surface, in the axes that the mapping of
//! `controls.rs` gives to scrolling.
//!
//! Cranpose carries the panel. Its move events give the pose that `place.rs` keeps for the next
//! start. They also give the cursor ring its place. The user resizes the panel with the sphere at
//! its bottom right corner, or with the thumbstick during a carry. This file keeps the width.
//!
//! Threads and what they own:
//! - the network threads call [`on_message`], [`push_unit`] and [`on_cursor`];
//! - the desktop decode thread ([`decode_loop`]) feeds the decoder, so a decoder that waits for an
//!   input buffer never holds up the eye stream;
//! - the uplink thread ([`uplink`]) sends the wish, the pointer and the requests for key frames;
//! - the composition thread creates and ends the surface ([`DesktopPanel`]), receives its pointer
//!   events and runs the panel's `SpatialFrameEffect` once in each headset frame.
//!
//! The decoder lives in a mutex because the composition thread must drop it before the surface's
//! end callback returns, while the decode thread feeds it.

use crate::{hub::hub, place};
use cranpose::{
    launch_args, prelude::*, AccessUnit, DecodedFrames, DecoderConfig, DecoderError, DecoderOutput, ExternalSurface,
    PointerEvent, PointerEventKind, VideoCodec, VideoDecoder,
};
use cranpose_xr::{
    Controller, Pose, ResizeLimits, ResizePolicy, SpatialExternalSurface, SpatialFrameEffect, SpatialMoveEvent, SpatialMoveEventType, SpatialPanel,
    SpatialResizeEventType, StereoMode, SubspaceModifier, XrFrame, METERS_PER_DP,
};
use crate::controls::control_of;
use cranvr_common::controls::{mouse, Action as ControlAction, Control, Controls, Hand};
use cranvr_proto::{
    clock::now_ns,
    link::ClientLink,
    media::Received,
    message::{ClientDatagram, ClientMsg, Cursor, DesktopParams, DesktopWish, HostMsg, Mouse, DESKTOP_STREAM},
    wire::Codec,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
        Condvar, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

/// The name under which `place.rs` keeps the panel's pose and width
const PANEL: &str = "desktop";
/// Where the panel starts when the user never carried it, relative to the origin of the subspace,
/// which lies 1 m ahead of the head: 1.5 m ahead of the head and a little below eye level
const START_POSE: Pose = Pose::at([0.0, -0.1, -0.5]);
/// The panel's width in meters at the start. The user may give it a width within `WIDTH_RANGE`.
/// 1.6 m wide fills 56 degrees at 1.5 m. The height follows the picture's aspect ratio.
const START_WIDTH: f32 = 1.6;
const WIDTH_RANGE: [f32; 2] = [0.4, 8.0];
/// Where the panel stands and how wide it is before the user carries it, for the scripted controllers of
/// a test build (`scripted.rs`)
#[cfg(all(feature = "scripted-hand", target_os = "android"))]
pub(crate) fn start_panel() -> (Pose, f32) {
    (place::start_pose(PANEL, START_POSE), place::saved_width(PANEL).map_or(START_WIDTH, |width| width.clamp(WIDTH_RANGE[0], WIDTH_RANGE[1])))
}

/// How fast the thumbstick resizes a carried panel, as a share of the width a second at full lean
const RESIZE_SPEED: f32 = 0.7;
/// A thumbstick leaning less than this counts as at rest
const DEAD_ZONE: f32 = 0.12;

/// How long the decoder waits for a free input buffer before it refuses a picture
const INPUT_WAIT: Duration = Duration::from_millis(30);
/// What the client asks for. The Quest 3's hardware decoder takes 3840 by 2160 at 60 pictures a
/// second.
const WISH_WIDTH: u32 = 3840;
const WISH_FPS: u8 = 60;
/// The pointer goes to the host this often while it points at the panel, if nothing changes
const POINTER_KEEPALIVE: Duration = Duration::from_millis(50);
const KEY_REQUEST_GAP: Duration = Duration::from_millis(250);
/// How often the panel looks at the link's state. The decode thread logs as often as `LOG_EVERY`
const POLL: Duration = Duration::from_millis(100);
const LOG_EVERY: Duration = Duration::from_secs(2);
/// The cursor: a ring that is lifted this far off the surface, in meters, with this diameter in dp
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

/// What the app sends the host about the pointer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PointerNow {
    /// Where the controller points on the picture as u and v from its top left corner
    pub hit: Option<[f32; 2]>,
    /// The mouse buttons that are down, as the bits of `cranvr_common::controls::mouse`
    pub buttons: u8,
    /// How far the thumbstick leans across (to the right) and forward, each from -1 to 1
    pub scroll: [f32; 2],
}

impl PointerNow {
    /// Whether the host has something to do with it: a controller points at the panel or holds a
    /// button.
    pub fn busy(&self) -> bool {
        self.hit.is_some() || self.buttons != 0
    }

    /// Whether `other` differs enough to send. A move of a fifth of a pixel of a 4K picture does not
    /// count.
    pub fn differs(&self, other: &Self) -> bool {
        let moved = match (self.hit, other.hit) {
            (Some(a), Some(b)) => (a[0] - b[0]).abs() > 5e-5 || (a[1] - b[1]).abs() > 5e-5,
            (None, None) => false,
            _ => true,
        };
        moved || self.buttons != other.buttons || (0..2).any(|axis| (self.scroll[axis] - other.scroll[axis]).abs() > 0.02)
    }
}

/// The inputs that Cranpose's pointer reports as pointer buttons: the ones that `controls::button_of`
/// gives a button.
const CLICK_INPUTS: [Control; 5] = [Control::Trigger, Control::Grip, Control::Primary, Control::Secondary, Control::StickClick];

/// What one controller does at the desktop panel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct HandAt {
    /// Where the controller points on the picture as u and v from its top left corner
    hit: Option<[f32; 2]>,
    /// The inputs that are down, as bits of the position in `CLICK_INPUTS`
    held: u8,
    /// The thumbstick's lean across and forward, as the headset's frame gave it
    lean: [f32; 2],
}

impl HandAt {
    fn holds(&self) -> bool {
        self.held != 0
    }

    fn set(&mut self, control: Control, down: bool) {
        if let Some(at) = CLICK_INPUTS.iter().position(|input| *input == control) {
            if down {
                self.held |= 1 << at;
            } else {
                self.held &= !(1 << at);
            }
        }
    }
}

/// The two controllers at the desktop panel, and the one that owns the Mac's mouse. The Mac has one
/// mouse, so one controller at a time drives it: the one that holds a button keeps it until it lets
/// go. Else a controller that presses takes it. Else the controller that points at the panel, and the
/// one that pointed first when both do.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Pointers {
    hands: [HandAt; 2],
    owner: Option<Hand>,
}

impl Pointers {
    fn at(&mut self, hand: Hand) -> &mut HandAt {
        &mut self.hands[hand.index()]
    }

    /// One pointer event of the surface. `hand` is the controller whose pointer it is, and `at` is
    /// the event's place as u and v. The surface sends `Up` before `Exit` when the ray left the
    /// surface during a press, so an `Exit` always ends with the buttons up.
    fn on_event(&mut self, hand: Hand, kind: PointerEventKind, at: [f32; 2]) {
        match kind {
            PointerEventKind::Enter | PointerEventKind::Move => {
                self.at(hand).hit = Some(at);
                if self.owner.is_none_or(|owner| self.hands[owner.index()].hit.is_none()) {
                    self.owner = Some(hand);
                }
            }
            PointerEventKind::Down => self.press(hand, Control::Trigger, true, at),
            PointerEventKind::Up => self.press(hand, Control::Trigger, false, at),
            PointerEventKind::ButtonDown(button) => self.press(hand, control_of(button), true, at),
            PointerEventKind::ButtonUp(button) => self.press(hand, control_of(button), false, at),
            PointerEventKind::Exit => {
                let lean = self.at(hand).lean;
                *self.at(hand) = HandAt { lean, ..HandAt::default() };
                if self.owner == Some(hand) {
                    let other = if hand == Hand::Left { Hand::Right } else { Hand::Left };
                    self.owner = self.hands[other.index()].hit.map(|_| other);
                }
            }
            _ => {}
        }
    }

    fn press(&mut self, hand: Hand, control: Control, down: bool, at: [f32; 2]) {
        self.at(hand).hit = Some(at);
        self.at(hand).set(control, down);
        // A controller that holds a button keeps the mouse against a press of the other
        let busy = self.owner.is_some_and(|owner| owner != hand && self.hands[owner.index()].holds());
        if down && !busy {
            self.owner = Some(hand);
        }
    }

    /// The thumbsticks' lean for a hand, as the headset's frame gave it, with the dead zone left out
    fn lean(&mut self, hand: Hand, lean: [f32; 2]) {
        self.at(hand).lean = lean;
    }

    /// What goes to the host: where the owner points, the mouse buttons that its held inputs make
    /// under the mapping, and its thumbstick's lean in the axes that the mapping gives to scrolling.
    fn now(&self, controls: &Controls) -> PointerNow {
        let Some(owner) = self.owner else { return PointerNow::default() };
        let hand = &self.hands[owner.index()];
        let Some(hit) = hand.hit else { return PointerNow::default() };
        let buttons = CLICK_INPUTS.iter().enumerate().filter(|(at, _)| hand.held & (1 << at) != 0).fold(0, |bits, (_, control)| {
            bits | match controls.action(owner, *control) {
                ControlAction::LeftClick => mouse::LEFT,
                ControlAction::RightClick => mouse::RIGHT,
                ControlAction::MiddleClick => mouse::MIDDLE,
                _ => 0,
            }
        });
        let axis = |control: Control, lean: f32| if controls.action(owner, control) == ControlAction::Scroll { lean } else { 0.0 };
        PointerNow { hit: Some(hit), buttons, scroll: [axis(Control::StickHorizontal, hand.lean[0]), axis(Control::StickVertical, hand.lean[1])] }
    }
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
}

/// Cursor datagrams received since the start
static CURSOR_PACKETS: AtomicU64 = AtomicU64::new(0);

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
    decoder_error: Option<String>,
}

/// What goes to the host.
#[derive(Default)]
struct Uplink {
    /// What the controllers do at the panel
    pointers: Pointers,
    /// What the host last heard of them
    pointer: PointerNow,
    dirty: bool,
    key: bool,
    /// The newest desktop frame number, for the key frame request
    frame: u32,
}

/// What the headset's frame loop and the composition read. They must never wait for the decoder,
/// which holds `Inner` while it takes a picture, so these live apart from it.
struct Quick {
    /// The host offers the desktop and shows it
    visible: AtomicBool,
    /// Whether the host hides the desktop for a game, as `HIDES_*`: unknown while it offers none
    hides: AtomicU8,
    /// The picture's width over its height, as f32 bits
    aspect: AtomicU32,
    cursor_in_video: AtomicBool,
    cursor: Mutex<Option<Cursor>>,
    /// The panel and the lines of the stats panel that were last made, for when `Inner` is busy
    lines: Mutex<Vec<(&'static str, String)>>,
}

pub struct Desktop {
    quick: Quick,
    inner: Mutex<Inner>,
    uplink: Mutex<Uplink>,
    wake: Condvar,
    /// The channel to the decode thread, while a session runs
    units: Mutex<Option<Sender<Received>>>,
}

/// The desktop of the process. The activity may rebuild the composition. The link outlives it.
pub fn desktop() -> &'static Desktop {
    static DESKTOP: OnceLock<Desktop> = OnceLock::new();
    DESKTOP.get_or_init(|| Desktop {
        quick: Quick {
            visible: AtomicBool::new(false),
            hides: AtomicU8::new(HIDES_UNKNOWN),
            aspect: AtomicU32::new((16.0f32 / 9.0).to_bits()),
            cursor_in_video: AtomicBool::new(false),
            cursor: Mutex::new(None),
            lines: Mutex::new(Vec::new()),
        },
        inner: Mutex::new(Inner::default()),
        uplink: Mutex::new(Uplink::default()),
        wake: Condvar::new(),
        units: Mutex::new(None),
    })
}

/// What `Quick::hides` holds
const HIDES_UNKNOWN: u8 = 0;
const HIDES_NO: u8 = 1;
const HIDES_YES: u8 = 2;

/// Whether the host hides the desktop for a game. A session that shows no desktop, a host that
/// offers none and a link that ended say nothing of a game: the answer is `None`.
fn hides_for_game(active: bool, params: Option<DesktopParams>) -> Option<bool> {
    params.filter(|_| active).map(|p| !p.visible)
}

/// Whether the app draws the Mac's cursor as a ring on a small panel in front of the picture. It
/// does when the host leaves the cursor out of the pictures and the launch does not say
/// `cursor=none`. On a Quest today the ring is hidden under the surface panel: the compositor draws
/// the surface layers after the panel layers, so the host puts the cursor into the pictures.
fn draws_cursor() -> bool {
    let in_video = desktop().quick.cursor_in_video.load(Ordering::Relaxed);
    !in_video && launch_args().string("cursor") != Some("none")
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
        self.publish(&inner);
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
        self.publish(&inner);
    }

    /// Copies what the frame loop and the composition read into `quick`.
    fn publish(&self, inner: &Inner) {
        let shown = inner.params.filter(|p| inner.active && p.visible);
        if let Some(p) = shown {
            self.quick.aspect.store((p.size[0] as f32 / p.size[1].max(1) as f32).to_bits(), Ordering::Relaxed);
        }
        self.quick.cursor_in_video.store(inner.params.is_some_and(|p| p.cursor_in_video), Ordering::Relaxed);
        self.quick.visible.store(shown.is_some(), Ordering::Relaxed);
        let hides = match hides_for_game(inner.active, inner.params) {
            None => HIDES_UNKNOWN,
            Some(false) => HIDES_NO,
            Some(true) => HIDES_YES,
        };
        self.quick.hides.store(hides, Ordering::Relaxed);
        if shown.is_none() {
            *self.quick.cursor.lock().unwrap() = None;
        }
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
                self.publish(&inner);
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
        CURSOR_PACKETS.fetch_add(1, Ordering::Relaxed);
        let mut newest = self.quick.cursor.lock().unwrap();
        if newest.is_none_or(|old| cursor.seq.wrapping_sub(old.seq) < u32::MAX / 2) {
            *newest = Some(cursor);
        }
    }

    /// The composition makes the surface: the decoder starts when the stream is ready.
    pub fn set_surface(&self, surface: Option<ExternalSurface>) {
        if surface.is_none() {
            // A surface that leaves sends no `Exit`: a press that was on would stay down
            self.reset_pointers();
        }
        let mut inner = self.inner.lock().unwrap();
        log::info!("desktop: the surface {}", if surface.is_some() { "is there" } else { "is gone" });
        inner.surface = surface;
        inner.rebuild();
    }

    /// What the panel needs. It never waits for the decoder.
    pub fn view(&self) -> View {
        View { visible: self.quick.visible.load(Ordering::Relaxed), aspect: f32::from_bits(self.quick.aspect.load(Ordering::Relaxed)) }
    }

    /// Whether the host hides the desktop, which it does while a game runs or loads. The app takes
    /// it as a sign of a game, which the stream's picture then shows. `None` while the host offers
    /// no desktop to this session.
    pub fn game_hides(&self) -> Option<bool> {
        match self.quick.hides.load(Ordering::Relaxed) {
            HIDES_NO => Some(false),
            HIDES_YES => Some(true),
            _ => None,
        }
    }

    /// Where the Mac's cursor is on the picture as u and v, when it is on the desktop's display.
    fn cursor(&self) -> Option<[f32; 2]> {
        self.quick.cursor.lock().unwrap().filter(|c| c.on).map(|c| [c.u, c.v])
    }

    /// Changes what the controllers do at the panel. What the host should hear then follows from the
    /// mapping, and goes out when it differs from what went last.
    fn update_pointers(&self, change: impl FnOnce(&mut Pointers)) {
        // The console's lock must not be taken under the uplink's
        let controls = hub().controls();
        let mut uplink = self.uplink.lock().unwrap();
        change(&mut uplink.pointers);
        let pointer = uplink.pointers.now(&controls);
        self.send_if_changed(uplink, pointer);
    }

    /// Forgets what the controllers did: nothing points at the panel and no button is down.
    fn reset_pointers(&self) {
        let mut uplink = self.uplink.lock().unwrap();
        uplink.pointers = Pointers::default();
        self.send_if_changed(uplink, PointerNow::default());
    }

    /// Sets the pointer that goes to the host, for the pointer test, which has no controller.
    fn set_pointer(&self, pointer: PointerNow) {
        let uplink = self.uplink.lock().unwrap();
        self.send_if_changed(uplink, pointer);
    }

    /// The pointer goes to the host when it differs from the one that went last.
    fn send_if_changed(&self, mut uplink: std::sync::MutexGuard<'_, Uplink>, pointer: PointerNow) {
        if pointer.differs(&uplink.pointer) || pointer.busy() != uplink.pointer.busy() {
            uplink.pointer = pointer;
            uplink.dirty = true;
            drop(uplink);
            self.wake.notify_all();
        }
    }

    /// A controller's ray on the surface. `size` is the surface's width and height in dp, the units
    /// of the event's position. Cranpose gives the pointer of the left hand the id 1 and the pointer
    /// of the right hand the id 2.
    fn on_pointer_event(&self, event: &PointerEvent, size: [f32; 2]) {
        let at = [event.position.x / size[0], event.position.y / size[1]].map(|v| v.clamp(0.0, 1.0));
        let hand = if event.id.0 == 1 { Hand::Left } else { Hand::Right };
        self.update_pointers(|pointers| pointers.on_event(hand, event.kind, at));
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
        // This decoder has a thread of its own, so it may wait longer for an input buffer than the
        // default 4 ms: a picture that is refused breaks the chain and costs a key frame
        let mut config = DecoderConfig::new(codec, size[0], size[1], DecoderOutput::Surface(surface.clone()));
        config.input_wait = INPUT_WAIT;
        match VideoDecoder::new(config) {
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
            cursors: CURSOR_PACKETS.load(Ordering::Relaxed),
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
    // The decoder holds `Inner` while it waits for an input buffer: the stats panel then shows
    // its last lines
    let Ok(inner) = d.inner.try_lock() else { return d.quick.lines.lock().unwrap().clone() };
    let lines = lines_of(&inner);
    *d.quick.lines.lock().unwrap() = lines.clone();
    lines
}

fn lines_of(inner: &Inner) -> Vec<(&'static str, String)> {
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
            CURSOR_PACKETS.load(Ordering::Relaxed)
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
    // `--ez desktop false` leaves the desktop out, to compare the eye stream without it
    if !d.inner.lock().unwrap().active || launch_args().boolean("desktop") == Some(false) {
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
            link.duplex.send_datagram(&ClientDatagram::Mouse(Mouse { seq, sent: now_ns(), hit: pointer.hit, buttons: pointer.buttons, scroll: pointer.scroll })).ok();
            seq = seq.wrapping_add(1);
            d.inner.lock().unwrap().counts.pointers_sent += 1;
        }
    }
}

/// The surface's width and height in dp (a millimeter each) for a panel `width` meters wide that
/// shows a picture of `aspect` (width over height).
fn surface_size(width: f32, aspect: f32) -> [f32; 2] {
    let width = width / METERS_PER_DP;
    [width, width / aspect.max(0.1)]
}

/// A test with no controller in the room: `--es desktop_pointer_test circle` sends the pointer of a
/// controller that circles over the picture, pulls its trigger for 0.3 s every 3 s and leans its
/// thumbstick forward for 2 s every 6 s. The packets go to the host as the surface's events do.
/// Cranpose's ray is not part of the test.
fn pointer_test() -> bool {
    launch_args().string("desktop_pointer_test") == Some("circle")
}

/// The pointer of the pointer test at the headset time `time`: a point that circles the middle of
/// the picture once in 4 seconds.
fn synthetic_pointer(time: Duration) -> PointerNow {
    let t = time.as_secs_f32();
    let angle = t * std::f32::consts::TAU / 4.0;
    PointerNow {
        hit: Some([0.5 + 0.3 * angle.cos(), 0.5 + 0.3 * angle.sin()]),
        // The left button every 3 s, the right one in the second 1.0 to 1.3 of each 3 s and the
        // middle one in the second 2.0 to 2.3
        buttons: [(0.0, mouse::LEFT), (1.0, mouse::RIGHT), (2.0, mouse::MIDDLE)]
            .into_iter()
            .filter(|(from, _)| (t % 3.0 - from).abs() < 0.3 && t % 3.0 >= *from)
            .fold(0, |bits, (_, bit)| bits | bit),
        scroll: if t % 6.0 > 4.0 { [0.6, 0.6] } else { [0.0, 0.0] },
    }
}

/// How far the thumbstick that leans furthest along `axis` (0 across, 1 forward) leans, from -1 to
/// 1, with the dead zone left out. A panel that is carried follows the stronger of the two sticks.
fn stick_lean(frame: &XrFrame, axis: usize) -> f32 {
    [&frame.left_controller, &frame.right_controller]
        .map(|controller| [controller.thumbstick.x, controller.thumbstick.y][axis])
        .into_iter()
        .filter(|lean| lean.abs() >= DEAD_ZONE)
        .max_by(|a, b| a.abs().total_cmp(&b.abs()))
        .unwrap_or(0.0)
}

/// How far a controller's thumbstick leans across and forward, each from -1 to 1, with the dead zone
/// left out.
fn lean_of(controller: &Controller) -> [f32; 2] {
    [controller.thumbstick.x, controller.thumbstick.y].map(|lean| if lean.abs() >= DEAD_ZONE { lean } else { 0.0 })
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
    // Where the user left the panel and how wide it is, as `place.rs` kept it
    let pose = rememberMutableStateOf(|| place::start_pose(PANEL, START_POSE));
    let width = rememberMutableStateOf(|| {
        place::saved_width(PANEL).map_or(START_WIDTH, |width| width.clamp(WIDTH_RANGE[0], WIDTH_RANGE[1]))
    });
    // Whether the user carries the panel now
    let carrying = rememberMutableStateOf(|| false);
    let cursor = rememberMutableStateOf(|| None::<[f32; 2]>);
    SpatialFrameEffect(move |frame: &XrFrame| {
        if !desktop().view().visible {
            desktop().reset_pointers();
            // A surface that leaves during a carry sends no `End`
            if carrying.get_non_reactive() {
                carrying.set(false);
            }
            return;
        }
        if carrying.get_non_reactive() {
            // The thumbstick's forward lean moves a carried panel away in Cranpose. Its lean across
            // resizes the panel.
            let lean = stick_lean(frame, 0);
            let before = width.get_non_reactive();
            let after = (before * (lean * RESIZE_SPEED * frame.period.as_secs_f32().clamp(0.0, 0.1)).exp()).clamp(WIDTH_RANGE[0], WIDTH_RANGE[1]);
            if after != before {
                width.set(after);
            }
            // The stick that carries a panel scrolls nothing
            desktop().update_pointers(|pointers| Hand::BOTH.into_iter().for_each(|hand| pointers.lean(hand, [0.0; 2])));
        } else if pointer_test() {
            desktop().set_pointer(synthetic_pointer(frame.predicted_display_time));
        } else {
            let leans = [lean_of(&frame.left_controller), lean_of(&frame.right_controller)];
            desktop().update_pointers(|pointers| Hand::BOTH.into_iter().for_each(|hand| pointers.lean(hand, leans[hand.index()])));
        }
        let at = desktop().cursor();
        if cursor.get_non_reactive() != at {
            cursor.set(at);
        }
    });
    let view = view.get();
    if !view.visible {
        return;
    }
    Surface(pose, width, carrying, view.aspect);
    if draws_cursor() {
        CursorRing(pose, width, cursor, view.aspect);
    }
}

/// The surface that the decoder writes into. The headset makes its swapchain at the size of the
/// panel's first composition (1.5 pixels to a dp) and keeps it when the user resizes the panel. The
/// decoder's pictures are larger. A test with the swapchain at 1600 by 900 dp (2400 by 1350 pixels) and
/// at 2560 by 1440 dp (3840 by 2160 pixels) showed the whole picture both times. The sharpness of a
/// panel wider than the view is not tested.
///
/// The surface starts at the pose that `pose` holds at its composition. Cranpose keeps the pose of
/// the user's last carry until the surface leaves the composition. The move events update `pose` for
/// the cursor ring. The last event goes to `place.rs`.
#[composable]
fn Surface(pose: MutableState<Pose>, width: MutableState<f32>, carrying: MutableState<bool>, aspect: f32) {
    let [surface_width, surface_height] = surface_size(width.get(), aspect);
    // The sphere at the bottom right corner resizes the panel. The picture keeps its shape.
    let [smallest, largest] = WIDTH_RANGE.map(|width| surface_size(width, aspect));
    let limits = ResizeLimits::new().minimum_size(smallest[0], smallest[1]).maximum_size(largest[0], largest[1]).maintain_aspect_ratio(true);
    let resize = ResizePolicy::custom(move |event| {
        width.set((event.size[0] * METERS_PER_DP).clamp(WIDTH_RANGE[0], WIDTH_RANGE[1]));
        if event.event_type == SpatialResizeEventType::End {
            place::save_width(PANEL, width.get_non_reactive());
        }
    });
    let modifier = place::movable(
        PANEL,
        SubspaceModifier::new().size(surface_width, surface_height).resizable_with(limits, resize),
        pose.get_non_reactive(),
        move |event: &SpatialMoveEvent| {
            let ended = event.event_type == SpatialMoveEventType::End;
            carrying.set(!ended);
            pose.set(event.pose);
            if ended {
                place::save_width(PANEL, width.get_non_reactive());
            }
        },
    );
    SpatialExternalSurface(StereoMode::Mono, modifier, move |scope| {
        scope.on_surface_created(|surface| desktop().set_surface(Some(surface)));
        // The decoder stops before the callback returns, as the surface's owner requires
        scope.on_surface_destroyed(|_| desktop().set_surface(None));
        scope.on_pointer_event(move |event| {
            desktop().on_pointer_event(&event, surface_size(width.get_non_reactive(), desktop().view().aspect));
        });
    });
}

/// The pose of a ring that lies on the point `[u, v]` of a picture (from its top left corner) on
/// a panel at `panel`, `CURSOR_LIFT` meters in front of it. `size` is the panel's width and height
/// in meters.
fn ring_pose(panel: &Pose, [width, height]: [f32; 2], [u, v]: [f32; 2]) -> Pose {
    panel.then(&Pose::at([(u - 0.5) * width, (0.5 - v) * height, CURSOR_LIFT]))
}

/// The Mac's cursor as a small panel in front of the picture.
#[composable]
fn CursorRing(pose: MutableState<Pose>, width: MutableState<f32>, cursor: MutableState<Option<[f32; 2]>>, aspect: f32) {
    let Some(at) = cursor.get() else { return };
    let size = surface_size(width.get(), aspect).map(|dp| dp * METERS_PER_DP);
    SpatialPanel(SubspaceModifier::new().size(CURSOR_SIZE, CURSOR_SIZE).with_pose(ring_pose(&pose.get(), size, at)), || {
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

#[cfg(test)]
mod tests {
    use super::*;
    use cranpose::PointerButton;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn the_host_hides_the_desktop_for_a_game_only_in_a_session_that_has_a_desktop() {
        let params = |visible| DesktopParams {
            stream: DESKTOP_STREAM,
            codec: Codec::Hevc,
            size: [3840, 2160],
            points: [1920.0, 1080.0],
            max_fps: 60.0,
            bitrate_bps: 40_000_000,
            full_range: true,
            flipped: false,
            visible,
            cursor_in_video: false,
        };
        assert_eq!(hides_for_game(true, Some(params(false))), Some(true));
        assert_eq!(hides_for_game(true, Some(params(true))), Some(false));
        // A host with no desktop, and a session that ended or that shows none, say nothing of a game
        assert_eq!(hides_for_game(true, None), None);
        assert_eq!(hides_for_game(false, Some(params(false))), None);
    }

    #[test]
    fn the_panel_is_as_wide_as_the_user_made_it_and_as_high_as_the_picture_is() {
        let [w, h] = surface_size(1.6, 16.0 / 9.0);
        assert!(close(w, 1600.0) && close(h, 900.0), "{w} {h}");
        let [w, h] = surface_size(1.0, 2.0);
        assert!((w - 1000.0).abs() < 1e-2 && (h - 500.0).abs() < 1e-2, "{w} {h}");
    }

    const AT: [f32; 2] = [0.25, 0.75];

    fn now(pointers: &Pointers) -> PointerNow {
        pointers.now(&Controls::default())
    }

    fn pointing(hand: Hand, at: [f32; 2]) -> Pointers {
        let mut pointers = Pointers::default();
        pointers.on_event(hand, PointerEventKind::Enter, at);
        pointers
    }

    #[test]
    fn a_ray_on_the_surface_is_a_point_on_the_picture_a_press_and_a_release() {
        assert_eq!(now(&Pointers::default()), PointerNow::default());
        let mut pointers = pointing(Hand::Right, AT);
        assert_eq!(now(&pointers), PointerNow { hit: Some(AT), buttons: 0, scroll: [0.0; 2] });
        pointers.on_event(Hand::Right, PointerEventKind::Move, [0.3, 0.7]);
        assert_eq!(now(&pointers).hit, Some([0.3, 0.7]));
        // The trigger is the left button in the standard mapping
        pointers.on_event(Hand::Right, PointerEventKind::Down, [0.3, 0.7]);
        assert_eq!(now(&pointers).buttons, mouse::LEFT);
        pointers.on_event(Hand::Right, PointerEventKind::Up, [0.3, 0.7]);
        assert_eq!((now(&pointers).buttons, now(&pointers).hit), (0, Some([0.3, 0.7])));
        // The ray leaves: nothing points and nothing presses
        pointers.on_event(Hand::Right, PointerEventKind::Exit, [0.3, 0.7]);
        assert_eq!(now(&pointers), PointerNow::default());
        assert!(!PointerNow::default().busy() && PointerNow { hit: Some(AT), ..PointerNow::default() }.busy());
    }

    #[test]
    fn each_input_that_cranpose_reports_is_looked_up_in_the_mapping_of_its_hand() {
        let mut pointers = pointing(Hand::Right, AT);
        let press = |pointers: &mut Pointers, button| pointers.on_event(Hand::Right, PointerEventKind::ButtonDown(button), AT);
        // A is the right button and the press of the stick is the middle button
        press(&mut pointers, PointerButton::Secondary);
        assert_eq!(now(&pointers).buttons, mouse::RIGHT);
        press(&mut pointers, PointerButton::Middle);
        assert_eq!(now(&pointers).buttons, mouse::RIGHT | mouse::MIDDLE);
        // The grip and the second face button do nothing mouse-like in the standard mapping
        press(&mut pointers, PointerButton::Back);
        press(&mut pointers, PointerButton::Forward);
        assert_eq!(now(&pointers).buttons, mouse::RIGHT | mouse::MIDDLE);
        pointers.on_event(Hand::Right, PointerEventKind::ButtonUp(PointerButton::Secondary), AT);
        assert_eq!(now(&pointers).buttons, mouse::MIDDLE);
        // The user gives each hand its own mapping: the right trigger is the right button, and the
        // left controller's X is the left button
        let mut controls = Controls::default();
        controls.set(Hand::Right, Control::Trigger, ControlAction::RightClick);
        controls.set(Hand::Left, Control::Primary, ControlAction::LeftClick);
        let mut right = pointing(Hand::Right, AT);
        right.on_event(Hand::Right, PointerEventKind::Down, AT);
        assert_eq!(right.now(&controls).buttons, mouse::RIGHT);
        let mut left = pointing(Hand::Left, AT);
        left.on_event(Hand::Left, PointerEventKind::ButtonDown(PointerButton::Secondary), AT);
        assert_eq!(left.now(&controls).buttons, mouse::LEFT);
        assert_eq!(left.now(&Controls::default()).buttons, mouse::RIGHT, "the right hand's mapping did not leak into the left's");
        // An input whose action is not a mouse button presses nothing
        let mut headset = Controls::default();
        headset.set(Hand::Right, Control::Primary, ControlAction::Recenter);
        assert_eq!(pointers.now(&headset).buttons, mouse::MIDDLE);
    }

    #[test]
    fn the_mac_has_one_mouse_and_one_controller_owns_it() {
        let mut pointers = pointing(Hand::Left, [0.2, 0.2]);
        // The second controller to point does not take it
        pointers.on_event(Hand::Right, PointerEventKind::Enter, [0.8, 0.8]);
        assert_eq!(now(&pointers).hit, Some([0.2, 0.2]));
        pointers.on_event(Hand::Right, PointerEventKind::Move, [0.81, 0.8]);
        assert_eq!(now(&pointers).hit, Some([0.2, 0.2]));
        // A press takes it
        pointers.on_event(Hand::Right, PointerEventKind::Down, [0.81, 0.8]);
        assert_eq!((now(&pointers).hit, now(&pointers).buttons), (Some([0.81, 0.8]), mouse::LEFT));
        // While the right controller holds a button, a press of the left does not take the mouse
        pointers.on_event(Hand::Left, PointerEventKind::Down, [0.2, 0.2]);
        assert_eq!((now(&pointers).hit, now(&pointers).buttons), (Some([0.81, 0.8]), mouse::LEFT));
        // The right lets go, and the left's next press takes it
        pointers.on_event(Hand::Right, PointerEventKind::Up, [0.81, 0.8]);
        pointers.on_event(Hand::Left, PointerEventKind::Up, [0.2, 0.2]);
        pointers.on_event(Hand::Left, PointerEventKind::Down, [0.2, 0.2]);
        assert_eq!((now(&pointers).hit, now(&pointers).buttons), (Some([0.2, 0.2]), mouse::LEFT));
        // The owner leaves: the other controller that still points takes over, with nothing held
        pointers.on_event(Hand::Left, PointerEventKind::Up, [0.2, 0.2]);
        pointers.on_event(Hand::Left, PointerEventKind::Exit, [0.2, 0.2]);
        assert_eq!((now(&pointers).hit, now(&pointers).buttons), (Some([0.81, 0.8]), 0));
        pointers.on_event(Hand::Right, PointerEventKind::Exit, [0.81, 0.8]);
        assert_eq!(now(&pointers), PointerNow::default());
    }

    #[test]
    fn a_controller_that_leaves_with_a_button_down_lets_it_go() {
        let mut pointers = pointing(Hand::Right, AT);
        pointers.on_event(Hand::Right, PointerEventKind::Down, AT);
        pointers.on_event(Hand::Right, PointerEventKind::ButtonDown(PointerButton::Secondary), AT);
        assert_eq!(now(&pointers).buttons, mouse::LEFT | mouse::RIGHT);
        pointers.on_event(Hand::Right, PointerEventKind::Exit, AT);
        assert_eq!(now(&pointers), PointerNow::default());
        // It comes back with nothing held
        pointers.on_event(Hand::Right, PointerEventKind::Enter, AT);
        assert_eq!(now(&pointers).buttons, 0);
    }

    #[test]
    fn the_owners_thumbstick_scrolls_in_the_axes_that_its_mapping_gives_to_scrolling() {
        let mut pointers = pointing(Hand::Right, AT);
        pointers.lean(Hand::Right, [0.4, -0.8]);
        pointers.lean(Hand::Left, [1.0, 1.0]);
        assert_eq!(now(&pointers).scroll, [0.4, -0.8], "only the controller that owns the mouse scrolls");
        let mut controls = Controls::default();
        controls.set(Hand::Right, Control::StickHorizontal, ControlAction::Nothing);
        assert_eq!(pointers.now(&controls).scroll, [0.0, -0.8]);
        controls.set(Hand::Right, Control::StickVertical, ControlAction::Nothing);
        assert_eq!(pointers.now(&controls).scroll, [0.0, 0.0]);
        // A lean at no panel scrolls nothing, and the headset's frame gives the lean, not the events
        let mut away = Pointers::default();
        away.lean(Hand::Right, [0.5, 0.5]);
        assert_eq!(now(&away), PointerNow::default());
        pointers.on_event(Hand::Right, PointerEventKind::Move, [0.3, 0.3]);
        assert_eq!(now(&pointers).scroll, [0.4, -0.8]);
        pointers.on_event(Hand::Right, PointerEventKind::Exit, [0.3, 0.3]);
        pointers.on_event(Hand::Right, PointerEventKind::Enter, [0.3, 0.3]);
        assert_eq!(now(&pointers).scroll, [0.4, -0.8]);
    }

    #[test]
    fn a_pointer_that_barely_moved_is_not_worth_sending() {
        let a = PointerNow { hit: Some([0.5, 0.5]), buttons: 0, scroll: [0.0; 2] };
        assert!(!a.differs(&PointerNow { hit: Some([0.500_02, 0.5]), ..a }));
        assert!(a.differs(&PointerNow { hit: Some([0.501, 0.5]), ..a }));
        assert!(a.differs(&PointerNow { hit: None, ..a }));
        assert!(a.differs(&PointerNow { buttons: mouse::RIGHT, ..a }));
        assert!(a.differs(&PointerNow { scroll: [0.5, 0.0], ..a }), "a lean across is worth sending");
    }

    #[test]
    fn the_pointer_test_presses_each_button_in_its_turn() {
        let seen: Vec<u8> = (0..60).map(|i| synthetic_pointer(Duration::from_secs_f32(i as f32 * 0.1)).buttons).collect();
        for bit in [mouse::LEFT, mouse::RIGHT, mouse::MIDDLE] {
            assert!(seen.contains(&bit), "{bit}: {seen:?}");
        }
        assert!(synthetic_pointer(Duration::from_secs_f32(4.5)).scroll[0] > 0.0);
    }

    #[test]
    fn the_cursor_ring_lies_on_the_point_in_front_of_the_picture() {
        // A panel 1.6 m by 0.9 m, 1.5 m ahead of the head, which is 0.5 m behind the subspace's
        // origin. The cursor at one quarter across and one quarter down is 40 cm left of the
        // middle and 22.5 cm above it, 4 mm in front.
        let panel = Pose::at([0.0, 0.0, -0.5]);
        let at = ring_pose(&panel, [1.6, 0.9], [0.25, 0.25]).position;
        assert!(close(at[0], -0.4) && close(at[1], 0.225) && close(at[2], -0.5 + CURSOR_LIFT), "{at:?}");
        // A panel turned to the right by 90 degrees carries the ring along: the picture's left
        // edge then lies toward the viewer
        let turned = Pose { position: [1.0, 0.0, 0.0], ..SubspaceModifier::new().rotate(0.0, 90.0, 0.0).pose() };
        let at = ring_pose(&turned, [1.6, 0.9], [0.0, 0.5]).position;
        assert!(close(at[0], 1.0 + CURSOR_LIFT) && close(at[2], 0.8), "{at:?}");
    }
}
