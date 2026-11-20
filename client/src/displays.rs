//! The Mac's displays in the headset, as panels.
//!
//! The host sends each display as a video stream of its own at the display's full pixel size
//! (streams `DESKTOP_STREAM` to `DESKTOP_STREAM + MAX_DISPLAYS - 1`, see `host/src/headset/native/
//! displays.rs`). The user adds displays and removes them from the hand menu. A display that is not
//! shown costs nothing: the client wishes only the displays that show (`ClientMsg::DisplayWish`),
//! and the host runs no capture and no encoder for the others. This client makes no decoder and no
//! surface for a display that is not shown.
//!
//! A `VideoDecoder` in surface mode writes the pictures of a display into the surface of a
//! `SpatialExternalSurface`. The headset's compositor shows that surface as a layer of its own and
//! samples the picture once, at the display's resolution, so text stays sharp. The panel is mono.
//!
//! Threads and what they own, for each display:
//! - the network threads call [`Displays::on_message`], [`Displays::push_unit`] and
//!   [`Displays::on_cursor`];
//! - a decode thread ([`decode_loop`]) feeds the decoder, so a decoder that waits for an input buffer
//!   never holds up the eye stream or another display;
//! - the composition thread creates and ends the surface and takes its pointer events
//!   (`display_panel.rs`).
//!
//! One uplink thread ([`uplink`]) sends the wishes, the pointers, the requests for key frames and the
//! requests to add or remove a display.
//!
//! A host of an older protocol version offers one display and no list: the first display then gets
//! the entry "Mac display". It hides that display while a game runs, which is how this client learns
//! that a game runs. A host of version 8 says so with `HostMsg::GameRuns`.
//!
//! The decoder lives in a mutex because the composition thread must drop it before the surface's
//! end callback returns, while the decode thread feeds it.

use crate::{
    display_wish::{wish_for, Measured},
    menu_model::place_of,
    panel_layout::Layout,
};
use crate::{controls::control_of, hub::hub};
use cranpose::{AccessUnit, DecodedFrames, DecoderConfig, DecoderError, DecoderOutput, ExternalSurface, PointerEvent, PointerEventKind, VideoCodec, VideoDecoder};
use cranvr_common::controls::{mouse, Action as ControlAction, Control, Controls, Hand};
use cranvr_proto::{
    link::ClientLink,
    media::Received,
    message::{
        display_index, display_stream, ClientDatagram, ClientMsg, Cursor, DesktopParams, DesktopWish, DisplayInfo, DisplayList, DisplayRequest, HostMsg,
        Mouse, Pointer, DESKTOP_STREAM, MAX_DISPLAYS,
    },
    wire::Codec,
    DISPLAYS_VERSION, MOUSE_VERSION,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
        Condvar, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

/// The number of displays that fit.
pub const SLOTS: usize = MAX_DISPLAYS as usize;

/// How long the decoder waits for a free input buffer before it refuses a picture
const INPUT_WAIT: Duration = Duration::from_millis(30);
const KEY_REQUEST_GAP: Duration = Duration::from_millis(250);
/// The pointer goes to the host this often while it points at a display, if nothing changes
const POINTER_KEEPALIVE: Duration = Duration::from_millis(50);
/// How often a decode thread logs its display's numbers, and the uplink thread logs the sum
const LOG_EVERY: Duration = Duration::from_secs(2);
const SUMMARY_EVERY: Duration = Duration::from_secs(5);
/// How long the uplink thread sleeps between turns while no display is wished
const IDLE_TURN: Duration = Duration::from_millis(500);

fn slot_of(stream: u8) -> Option<usize> {
    display_index(stream).map(usize::from)
}

/// What the panel needs to know about one display's stream.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// The host offers the display's video and shows it
    pub visible: bool,
    /// The picture's width over its height
    pub aspect: f32,
}

/// What the app sends the host about the pointer on one display.
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
    /// Whether the host has something to do with it: a controller points at the display or holds a
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

/// What one controller does at a display's panel.
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

/// The two controllers at a display's panel, and the one that owns the Mac's mouse. The Mac has one
/// mouse, so one controller at a time drives it: the one that holds a button keeps it until it lets
/// go. Else a controller that presses takes it. Else the controller that points at the panel, and the
/// one that pointed first when both do.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Pointers {
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
    pub fn lean(&mut self, hand: Hand, lean: [f32; 2]) {
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
    /// Pictures that went into the decoder
    submitted: u64,
}

/// What one display's decoder and its stream hold.
#[derive(Default)]
struct Inner {
    /// This session shows displays: a headset asks for them, a flat window does not
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

/// What the headset's frame loop and the composition read. They must never wait for the decoder,
/// which holds `Inner` while it takes a picture, so these live apart from it.
struct Quick {
    visible: std::sync::atomic::AtomicBool,
    /// The picture's width over its height, as f32 bits
    aspect: AtomicU32,
    cursor_in_video: AtomicBool,
    cursor: Mutex<Option<Cursor>>,
    /// The numbers last made, for when `Inner` is busy
    measured: Mutex<Measured>,
    /// The parameters that the host sent and whether this session shows displays, a copy of what `Inner`
    /// holds. The composition and the frame loop read it, and the decoder never holds it.
    params: Mutex<Option<(DesktopParams, bool)>>,
}

struct Slot {
    quick: Quick,
    inner: Mutex<Inner>,
    /// The channel to this display's decode thread, while a session runs
    units: Mutex<Option<Sender<Received>>>,
}

impl Slot {
    fn new() -> Self {
        Self {
            quick: Quick {
                visible: AtomicBool::new(false),
                aspect: AtomicU32::new((16.0f32 / 9.0).to_bits()),
                cursor_in_video: AtomicBool::new(false),
                cursor: Mutex::new(None),
                measured: Mutex::new(Measured::default()),
                params: Mutex::new(None),
            },
            inner: Mutex::new(Inner::default()),
            units: Mutex::new(None),
        }
    }
}

/// What the client knows of the host's displays and wants of them.
#[derive(Default)]
struct State {
    /// The host's list, when it sent one (protocol version 8)
    list: Option<DisplayList>,
    /// What the user wants of each display now: `None` for a display that is not shown
    wanted: [Option<DesktopWish>; SLOTS],
    /// What the host heard last: `None` before the first wish
    sent: [Option<Option<DesktopWish>>; SLOTS],
    /// Requests to add and to remove a display that the uplink thread has not sent yet
    adds: u8,
    removes: Vec<u8>,
}

/// What the uplink thread sends: the keys that displays ask for.
#[derive(Default)]
struct Uplink {
    key: [bool; SLOTS],
    /// The newest frame number of each display's stream, for the key frame request
    frame: [u32; SLOTS],
    /// What the controllers do at each display's panel
    pointers: [Pointers; SLOTS],
    /// What the host last heard of the pointer on each display, and whether it changed since the last send
    pointer: [PointerNow; SLOTS],
    pointer_dirty: [bool; SLOTS],
    dirty: bool,
}

/// What a game's state is known from. `Unknown` while the session offers no display.
const GAME_UNKNOWN: u8 = 0;
const GAME_NO: u8 = 1;
const GAME_YES: u8 = 2;

pub struct Displays {
    slots: [Slot; SLOTS],
    state: Mutex<State>,
    uplink: Mutex<Uplink>,
    wake: Condvar,
    /// Whether a game runs, as the host said it with `HostMsg::GameRuns`, or `GAME_UNKNOWN`
    game_message: AtomicU8,
    /// The protocol version of the session, 0 before the first
    version: std::sync::atomic::AtomicU16,
}

/// The displays of the process. The activity may rebuild the composition. The link outlives it.
pub fn displays() -> &'static Displays {
    static DISPLAYS: OnceLock<Displays> = OnceLock::new();
    DISPLAYS.get_or_init(Displays::new)
}

/// What the menu shows of one display.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayView {
    pub info: DisplayInfo,
    pub measured: Measured,
    /// The host runs the display's video for this client now
    pub streaming: bool,
}

/// What the menu shows of all displays.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisplaysView {
    pub displays: Vec<DisplayView>,
    /// The host can add one more display now. A host of an older version cannot.
    pub can_add: bool,
    pub notice: Option<String>,
}

impl Displays {
    fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| Slot::new()),
            state: Mutex::new(State::default()),
            uplink: Mutex::new(Uplink::default()),
            wake: Condvar::new(),
            game_message: AtomicU8::new(GAME_UNKNOWN),
            version: std::sync::atomic::AtomicU16::new(0),
        }
    }

    /// A session begins. A headset shows displays and asks the host for them. Returns the receiver of
    /// each display's pictures, for its decode thread.
    pub fn session_start(&self, in_headset: bool, version: u16) -> Vec<mpsc::Receiver<Received>> {
        self.version.store(version, Ordering::Relaxed);
        self.game_message.store(GAME_UNKNOWN, Ordering::Relaxed);
        *self.state.lock().unwrap() = State::default();
        *self.uplink.lock().unwrap() = Uplink::default();
        self.slots
            .iter()
            .map(|slot| {
                let (sender, receiver) = mpsc::channel();
                *slot.units.lock().unwrap() = Some(sender);
                let mut inner = slot.inner.lock().unwrap();
                // The surface belongs to the composition and stays. Everything else starts again.
                *inner = Inner { surface: inner.surface.take(), active: in_headset, waiting_key: true, ..Inner::default() };
                *slot.quick.cursor.lock().unwrap() = None;
                self.publish(slot, &inner);
                receiver
            })
            .collect()
    }

    /// The session ended: the decoders go and the panels hide.
    pub fn session_end(&self) {
        for slot in &self.slots {
            *slot.units.lock().unwrap() = None;
            let mut inner = slot.inner.lock().unwrap();
            inner.active = false;
            inner.params = None;
            inner.decoder = None;
            inner.frames = None;
            inner.built_for = None;
            self.publish(slot, &inner);
        }
        *self.state.lock().unwrap() = State::default();
        self.game_message.store(GAME_UNKNOWN, Ordering::Relaxed);
    }

    /// Copies what the frame loop and the composition read into `quick`.
    fn publish(&self, slot: &Slot, inner: &Inner) {
        let shown = inner.params.filter(|p| inner.active && p.visible);
        if let Some(p) = shown {
            slot.quick.aspect.store((p.size[0] as f32 / p.size[1].max(1) as f32).to_bits(), Ordering::Relaxed);
        }
        slot.quick.cursor_in_video.store(inner.params.is_some_and(|p| p.cursor_in_video), Ordering::Relaxed);
        slot.quick.visible.store(shown.is_some(), Ordering::Relaxed);
        *slot.quick.params.lock().unwrap() = inner.params.map(|p| (p, inner.active));
        if shown.is_none() {
            *slot.quick.cursor.lock().unwrap() = None;
        }
    }

    /// What the host says: the list of displays, the state of a stream, the parameter sets of a
    /// stream and whether a game runs.
    pub fn on_message(&self, message: &HostMsg) {
        match message {
            HostMsg::Desktop(params) => self.on_params(DESKTOP_STREAM, *params),
            HostMsg::DisplayStream { stream, params } => self.on_params(*stream, *params),
            HostMsg::DecoderConfig { stream, config, .. } => {
                if let Some(slot) = slot_of(*stream).map(|i| &self.slots[i]) {
                    let mut inner = slot.inner.lock().unwrap();
                    inner.config.clone_from(config);
                    // The decoder may wait for the parameter sets
                    inner.rebuild(self, *stream);
                }
            }
            HostMsg::Displays(list) => {
                log::info!(
                    "displays: the host lists {}{}{}",
                    list.displays.iter().map(|d| format!("{} ({}x{} points{})", d.name, d.points[0], d.points[1], if d.removable { ", removable" } else { "" })).collect::<Vec<_>>().join(", "),
                    if list.can_add { "; it can add one" } else { "" },
                    list.notice.as_deref().map_or(String::new(), |n| format!("; it says: {n}"))
                );
                self.state.lock().unwrap().list = Some(list.clone());
                // A display that left the list loses its stream
                for (index, slot) in self.slots.iter().enumerate() {
                    let stream = display_stream(index as u8);
                    if !list.displays.iter().any(|d| d.stream == stream) {
                        let mut inner = slot.inner.lock().unwrap();
                        if inner.params.take().is_some() {
                            inner.rebuild(self, stream);
                            self.publish(slot, &inner);
                        }
                    }
                }
            }
            HostMsg::GameRuns(runs) => {
                log::info!("displays: the host says that a game {}", if *runs { "runs" } else { "does not run" });
                self.game_message.store(if *runs { GAME_YES } else { GAME_NO }, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    fn on_params(&self, stream: u8, params: Option<DesktopParams>) {
        let Some(index) = slot_of(stream) else { return };
        let slot = &self.slots[index];
        let mut inner = slot.inner.lock().unwrap();
        match params {
            Some(p) => log::info!(
                "displays: the host offers {}x{} pixels ({}x{} points) for display {}, up to {} a second, {:.0} Mbps, {}, mirrored: {}",
                p.size[0],
                p.size[1],
                p.points[0],
                p.points[1],
                index + 1,
                p.max_fps,
                p.bitrate_bps as f64 / 1e6,
                if p.visible { "visible" } else { "hidden" },
                p.flipped
            ),
            None => log::info!("displays: the host runs no video of display {} for this client", index + 1),
        }
        inner.params = params;
        inner.waiting_key = true;
        inner.rebuild(self, stream);
        self.publish(slot, &inner);
    }

    /// The network thread hands over a whole picture. The decode thread of its display takes it.
    pub fn push_unit(&self, unit: Received) {
        if let Some(slot) = slot_of(unit.meta.stream).map(|i| &self.slots[i]) {
            if let Some(sender) = slot.units.lock().unwrap().as_ref() {
                sender.send(unit).ok();
            }
        }
    }

    /// A picture of a display's stream is lost: the next one waits for a key frame.
    pub fn on_lost(&self, stream: u8, frame: u32) {
        if let Some(slot) = slot_of(stream).map(|i| &self.slots[i]) {
            slot.inner.lock().unwrap().waiting_key = true;
            self.request_key_frame(stream, frame);
        }
    }

    fn request_key_frame(&self, stream: u8, frame: u32) {
        let Some(index) = slot_of(stream) else { return };
        let mut uplink = self.uplink.lock().unwrap();
        uplink.key[index] = true;
        uplink.frame[index] = frame;
        uplink.dirty = true;
        drop(uplink);
        self.wake.notify_all();
    }

    /// The Mac's cursor moved on a display. The newest datagram wins.
    pub fn on_cursor(&self, stream: u8, cursor: Cursor) {
        if let Some(slot) = slot_of(stream).map(|i| &self.slots[i]) {
            let mut newest = slot.quick.cursor.lock().unwrap();
            if newest.is_none_or(|old| cursor.seq.wrapping_sub(old.seq) < u32::MAX / 2) {
                *newest = Some(cursor);
            }
        }
    }

    /// The composition makes the surface of a display: its decoder starts when the stream is ready.
    pub fn set_surface(&self, stream: u8, surface: Option<ExternalSurface>) {
        let Some(index) = slot_of(stream) else { return };
        let mut inner = self.slots[index].inner.lock().unwrap();
        log::info!("displays: the surface of display {} {}", index + 1, if surface.is_some() { "is there" } else { "is gone" });
        inner.surface = surface;
        inner.rebuild(self, stream);
    }

    /// What the panel of a display needs. It never waits for the decoder.
    pub fn view(&self, stream: u8) -> View {
        match slot_of(stream) {
            Some(index) => {
                let quick = &self.slots[index].quick;
                View { visible: quick.visible.load(Ordering::Relaxed), aspect: f32::from_bits(quick.aspect.load(Ordering::Relaxed)) }
            }
            None => View { visible: false, aspect: 16.0 / 9.0 },
        }
    }

    /// Where the Mac's cursor is on the picture of a display as u and v, when it is on the display.
    pub fn cursor(&self, stream: u8) -> Option<[f32; 2]> {
        let index = slot_of(stream)?;
        self.slots[index].quick.cursor.lock().unwrap().filter(|c| c.on).map(|c| [c.u, c.v])
    }

    /// Whether the cursor is part of the pictures of the display. The client then draws none.
    pub fn cursor_in_video(&self, stream: u8) -> bool {
        slot_of(stream).is_some_and(|index| self.slots[index].quick.cursor_in_video.load(Ordering::Relaxed))
    }

    /// Whether a game runs on the host, as far as the displays tell. A host of version 8 says it in a
    /// message. A host of an older version hides its one display for a game. `None` while neither
    /// says anything.
    pub fn game_runs(&self) -> Option<bool> {
        match self.game_message.load(Ordering::Relaxed) {
            GAME_YES => return Some(true),
            GAME_NO => return Some(false),
            _ => {}
        }
        let params = *self.slots[0].quick.params.lock().unwrap();
        params.filter(|(_, active)| *active).map(|(p, _)| !p.visible)
    }

    /// The list for the menu. A host of an older version gets the one entry for its one display.
    pub fn menu_view(&self) -> DisplaysView {
        let state = self.state.lock().unwrap();
        let mut infos: Vec<DisplayInfo> = state.list.as_ref().map(|list| list.displays.clone()).unwrap_or_default();
        if state.list.is_none() {
            if let Some((p, _)) = *self.slots[0].quick.params.lock().unwrap() {
                infos.push(DisplayInfo { stream: DESKTOP_STREAM, name: "Mac display".into(), points: [p.points[0] as u32, p.points[1] as u32], removable: false });
            }
        }
        let displays = infos
            .into_iter()
            .map(|info| {
                let index = slot_of(info.stream);
                let measured = index.map(|i| *self.slots[i].quick.measured.lock().unwrap()).unwrap_or_default();
                let streaming = index.is_some_and(|i| self.slots[i].quick.visible.load(Ordering::Relaxed));
                DisplayView { info, measured, streaming }
            })
            .collect();
        DisplaysView { displays, can_add: state.list.as_ref().is_some_and(|l| l.can_add), notice: state.list.as_ref().and_then(|l| l.notice.clone()) }
    }

    /// Keeps the wishes for the displays that exist: the host's list when it sent one, else the first
    /// display. A wish for a display that the host does not have has no answer.
    pub fn existing(&self, mut wishes: [Option<DesktopWish>; SLOTS]) -> [Option<DesktopWish>; SLOTS] {
        let state = self.state.lock().unwrap();
        for (index, wish) in wishes.iter_mut().enumerate() {
            let exists = match &state.list {
                Some(list) => list.displays.iter().any(|d| d.stream == display_stream(index as u8)),
                None => index == 0,
            };
            if !exists {
                *wish = None;
            }
        }
        wishes
    }

    /// The displays that the user wants shown now, with what each asks of the host: `None` for a
    /// display that stays hidden. The uplink thread sends the changes.
    pub fn set_wanted(&self, wanted: [Option<DesktopWish>; SLOTS]) {
        let mut state = self.state.lock().unwrap();
        if state.wanted == wanted {
            return;
        }
        state.wanted = wanted;
        drop(state);
        self.uplink.lock().unwrap().dirty = true;
        self.wake.notify_all();
    }

    /// Changes what the controllers do at a display's panel. What the host should hear then follows
    /// from the mapping, and goes out when it differs from what went last.
    pub fn update_pointers(&self, stream: u8, change: impl FnOnce(&mut Pointers)) {
        let Some(index) = slot_of(stream) else { return };
        // The console's lock must not be taken under the uplink's
        let controls = hub().controls();
        let mut uplink = self.uplink.lock().unwrap();
        change(&mut uplink.pointers[index]);
        let pointer = uplink.pointers[index].now(&controls);
        self.send_if_changed(index, uplink, pointer);
    }

    /// Forgets what the controllers did at a display's panel: nothing points at it and no button is down.
    pub fn reset_pointers(&self, stream: u8) {
        let Some(index) = slot_of(stream) else { return };
        let mut uplink = self.uplink.lock().unwrap();
        uplink.pointers[index] = Pointers::default();
        self.send_if_changed(index, uplink, PointerNow::default());
    }

    /// Sets the pointer that goes to the host, for the pointer test, which has no controller.
    pub fn set_pointer(&self, stream: u8, pointer: PointerNow) {
        let Some(index) = slot_of(stream) else { return };
        let uplink = self.uplink.lock().unwrap();
        self.send_if_changed(index, uplink, pointer);
    }

    /// The pointer goes to the host when it differs from the one that went last.
    fn send_if_changed(&self, index: usize, mut uplink: std::sync::MutexGuard<'_, Uplink>, pointer: PointerNow) {
        if pointer.differs(&uplink.pointer[index]) || pointer.busy() != uplink.pointer[index].busy() {
            uplink.pointer[index] = pointer;
            uplink.pointer_dirty[index] = true;
            uplink.dirty = true;
            drop(uplink);
            self.wake.notify_all();
        }
    }

    /// A controller's ray on the surface of a display. `size` is the surface's width and height in dp,
    /// the units of the event's position. Cranpose gives the pointer of the left hand the id 1 and the
    /// pointer of the right hand the id 2.
    pub fn on_pointer_event(&self, stream: u8, event: &PointerEvent, size: [f32; 2]) {
        let at = [event.position.x / size[0], event.position.y / size[1]].map(|v| v.clamp(0.0, 1.0));
        let hand = if event.id.0 == 1 { Hand::Left } else { Hand::Right };
        self.update_pointers(stream, |pointers| pointers.on_event(hand, event.kind, at));
    }

    /// The user asks the host to add a display.
    pub fn request_add(&self) {
        self.state.lock().unwrap().adds += 1;
        self.uplink.lock().unwrap().dirty = true;
        self.wake.notify_all();
    }

    /// The user asks the host to remove an added display.
    pub fn request_remove(&self, stream: u8) {
        self.state.lock().unwrap().removes.push(stream);
        self.uplink.lock().unwrap().dirty = true;
        self.wake.notify_all();
    }

    /// The streams that the host lists, for the layout to forget the ones that left.
    pub fn listed_streams(&self) -> Option<Vec<u8>> {
        self.state.lock().unwrap().list.as_ref().map(|list| list.displays.iter().map(|d| d.stream).collect())
    }

    /// The lines of the stats panel: a label and a value for each display that has a stream.
    pub fn stat_lines(&self) -> Vec<(&'static str, String)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                // The decoder holds `Inner` while it waits for an input buffer: the line is then
                // the one last made
                let inner = slot.inner.try_lock().ok()?;
                let line = lines_of(index, &inner)?;
                Some((if index == 0 { "Desktop" } else { "Display" }, line))
            })
            .collect()
    }
}

impl Inner {
    /// Makes the decoder that the state calls for, or drops the one that it does not. A decoder
    /// is made when the surface, the stream and its parameter sets are all there.
    fn rebuild(&mut self, owner: &Displays, stream: u8) {
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
                log::info!("displays: decoder of display {} started for {}x{}", stream - DESKTOP_STREAM + 1, size[0], size[1]);
                self.frames = Some(decoder.frames());
                self.decoder = Some(decoder);
                self.built_for = Some((surface, size));
                self.decoder_error = None;
                self.waiting_key = true;
                self.last_frame = None;
                // The host sends a key frame when the wish comes and when it hears of a loss
                self.counts.key_requests += 1;
                if let Some(index) = slot_of(stream) {
                    let mut uplink = owner.uplink.lock().unwrap();
                    uplink.key[index] = true;
                    uplink.dirty = true;
                }
                owner.wake.notify_all();
            }
            Err(e) => {
                log::warn!("displays: no decoder for display {}: {e}", stream - DESKTOP_STREAM + 1);
                self.decoder_error = Some(e.to_string());
            }
        }
    }

    fn on_unit(&mut self, owner: &Displays, stream: u8, unit: &Received) {
        let meta = &unit.meta;
        self.counts.units += 1;
        self.counts.bytes += unit.payload().len() as u64;
        self.counts.key_units += u64::from(meta.key);
        let in_order = self.last_frame.is_none_or(|last| meta.frame == last.wrapping_add(1));
        self.last_frame = Some(meta.frame);
        if let Some(index) = slot_of(stream) {
            owner.uplink.lock().unwrap().frame[index] = meta.frame;
        }
        if meta.key {
            self.waiting_key = false;
        } else if !in_order {
            self.waiting_key = true;
        }
        if !meta.key && self.waiting_key {
            self.counts.skipped += 1;
            self.counts.key_requests += 1;
            owner.request_key_frame(stream, meta.frame);
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
        match decoder.submit(AccessUnit { data, timestamp: Duration::from_micros(pts_us), metadata: u64::from(meta.frame) }) {
            Ok(()) => self.counts.submitted += 1,
            Err(e) => {
                self.counts.refused += 1;
                if !matches!(e, DecoderError::Full) {
                    log::warn!("displays: the decoder of display {} refused picture {}: {e}", stream - DESKTOP_STREAM + 1, meta.frame);
                }
                self.waiting_key = true;
                self.counts.key_requests += 1;
                owner.request_key_frame(stream, meta.frame);
            }
        }
    }
}

/// The numbers for the stats panel and the log, for one display.
#[derive(Clone, Debug, Default, PartialEq)]
struct Stats {
    running: bool,
    size: [u32; 2],
    received_fps: f32,
    decoded_fps: f32,
    mbps: f32,
    decode_mean_ms: f32,
    decode_last_ms: f32,
    decode_max_ms: f32,
    dropped: u64,
    late: u64,
    skipped: u64,
    refused: u64,
    key_requests: u64,
    error: Option<String>,
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

    fn look(&mut self, inner: &Inner) -> Stats {
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
            error: inner.decoder_error.clone(),
        };
        self.units = c.units;
        self.bytes = c.bytes;
        self.decoded_before = self.decoded;
        stats
    }
}

fn lines_of(index: usize, inner: &Inner) -> Option<String> {
    let params = inner.params.filter(|_| inner.active)?;
    let decoder = inner.frames.as_ref().map(DecodedFrames::stats).unwrap_or_default();
    let ms = |d: Duration| d.as_secs_f32() * 1000.0;
    let c = &inner.counts;
    let state = match (&inner.decoder_error, inner.decoder.is_some(), params.visible) {
        (Some(e), _, _) => format!("no decoder: {e}"),
        (None, true, _) => "decoding".to_string(),
        (None, false, false) => "hidden: a game runs".to_string(),
        (None, false, true) => "waiting for the surface or the stream".to_string(),
    };
    Some(format!(
        "{}{state}; {}x{} pixels; {} pictures ({} key, {} skipped, {} refused), {:.1} MB; decoder {} decoded, {:.2} ms mean, {:.2} ms last, {:.2} ms longest, {} dropped",
        if index == 0 { String::new() } else { format!("{}: ", index + 1) },
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
    ))
}

/// Takes the pictures of one display from the network threads and feeds its decoder until the session
/// ends. It also logs the display's numbers.
pub fn decode_loop(stream: u8, units: &mpsc::Receiver<Received>, stop: &AtomicBool) {
    let Some(index) = slot_of(stream) else { return };
    let owner = displays();
    let slot = &owner.slots[index];
    let mut rates = Rates::new();
    let mut log_at = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        // A display that no client shows gets no picture: the thread only wakes to look at `stop`
        match units.recv_timeout(Duration::from_millis(250)) {
            Ok(unit) => slot.inner.lock().unwrap().on_unit(owner, stream, &unit),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if log_at.elapsed() >= LOG_EVERY {
            log_at = Instant::now();
            let s = rates.look(&slot.inner.lock().unwrap());
            *slot.quick.measured.lock().unwrap() = Measured {
                size: (s.size != [0, 0]).then_some(s.size),
                received_fps: s.received_fps,
                decoded_fps: s.decoded_fps,
                mbps: s.mbps,
                decode_ms: s.decode_mean_ms,
            };
            if s.size != [0, 0] && (s.received_fps > 0.0 || s.running) {
                log::info!(
                    "displays: display {}: {:.1} pictures/s received and {:.1}/s decoded of {}x{}, {:.1} Mbps, decoder {} (decode {:.2} ms mean, {:.2} ms last, {:.2} ms longest, {} dropped, {} late), {} skipped, {} refused, {} key requests{}",
                    index + 1,
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
                    s.error.map_or(String::new(), |e| format!(", {e}"))
                );
            }
        }
    }
}

/// The widest picture that the headset's decoder takes, once asked.
fn decoder_width() -> Option<u32> {
    static WIDTH: OnceLock<Option<u32>> = OnceLock::new();
    *WIDTH.get_or_init(|| VideoDecoder::capabilities(VideoCodec::Hevc).ok().and_then(|limits| limits.max_width))
}

/// What the client asks of each display that shows in `layout`: `None` for the others. A display that
/// is not shown is not asked for, and costs the Mac and the headset nothing.
pub fn wishes_of(layout: &Layout) -> [Option<DesktopWish>; SLOTS] {
    let shown = layout.displays_shown();
    let mut wishes = [None; SLOTS];
    for &stream in &shown {
        if let Some(index) = slot_of(stream) {
            wishes[index] = Some(wish_for(place_of(layout, stream), layout.in_game(), shown.len(), decoder_width()));
        }
    }
    wishes
}

/// The message for the wish of a display, in the form of the host's protocol version. A host of an
/// older version knows only the first display.
fn wish_message(stream: u8, wish: DesktopWish) -> ClientMsg {
    if stream == DESKTOP_STREAM {
        ClientMsg::Desktop(wish)
    } else {
        ClientMsg::DisplayWish { stream, wish }
    }
}

/// The datagram for a pointer on a display, in the form of the host's protocol version. The first
/// display uses the datagrams of the older versions. A host of version 6 has the trigger and one axis
/// only: the left button is its trigger, and the forward lean its scroll.
fn pointer_datagram(version: u16, stream: u8, seq: u32, pointer: &PointerNow) -> ClientDatagram {
    let sent = cranvr_proto::clock::now_ns();
    if version >= MOUSE_VERSION {
        let mouse = Mouse { seq, sent, hit: pointer.hit, buttons: pointer.buttons, scroll: pointer.scroll };
        if stream == DESKTOP_STREAM {
            ClientDatagram::Mouse(mouse)
        } else {
            ClientDatagram::DisplayMouse { stream, mouse }
        }
    } else {
        let left = pointer.buttons & mouse::LEFT != 0;
        ClientDatagram::Pointer(Pointer { seq, sent, hit: pointer.hit, trigger: if left { 1.0 } else { 0.0 }, scroll: pointer.scroll[1] })
    }
}

/// The wish that ends a display's video.
const NO_WISH: DesktopWish = DesktopWish { enabled: false, max_width: 0, max_fps: 0 };

/// Sends the wishes, the requests to add and remove a display and the requests for key frames, until
/// the session ends. It also logs, every few seconds, what all displays cost: a game with no display
/// shown logs zero pictures.
pub fn uplink(link: &ClientLink, stop: &AtomicBool) {
    let owner = displays();
    if !owner.slots[0].inner.lock().unwrap().active || cranpose::launch_args().boolean("desktop") == Some(false) {
        return;
    }
    let newer = link.welcome.version >= DISPLAYS_VERSION;
    let mut last_key = [Instant::now() - KEY_REQUEST_GAP; SLOTS];
    let mut last_send = [Instant::now(); SLOTS];
    let mut seq = [0u32; SLOTS];
    let mut pointers_sent = 0u64;
    let (mut summary_at, mut seen) = (Instant::now(), (0u64, 0u64));
    while !stop.load(Ordering::Relaxed) {
        // With no display wished nothing needs the 20 ms turns (a key frame, a pointer's keep-alive): every
        // change wakes the loop at once, so it sleeps longer then
        let idle = owner.state.lock().unwrap().wanted.iter().all(Option::is_none);
        let (keys, frames, pointers, pointer_dirty) = {
            let guard = owner.uplink.lock().unwrap();
            let (mut uplink, _) = owner.wake.wait_timeout_while(guard, if idle { IDLE_TURN } else { Duration::from_millis(20) }, |u| !u.dirty).unwrap();
            // A request for a key frame waits in `uplink.key` until it went out: it may come before the
            // gap since the last request has passed. The next turn of the loop, at most 20 ms later, sees it.
            uplink.dirty = false;
            (uplink.key, uplink.frame, uplink.pointer, std::mem::take(&mut uplink.pointer_dirty))
        };
        // The wishes that changed, the displays to add and the displays to remove
        let (changes, adds, removes) = {
            let mut state = owner.state.lock().unwrap();
            let mut changes = Vec::new();
            for index in 0..SLOTS {
                // A host of an older version has the first display only
                if !newer && index > 0 {
                    continue;
                }
                let wanted = state.wanted[index];
                match state.sent[index] {
                    Some(sent) if sent == wanted => {}
                    // Nothing was asked for yet, so there is nothing to end
                    None if wanted.is_none() => state.sent[index] = Some(None),
                    _ => {
                        state.sent[index] = Some(wanted);
                        changes.push((display_stream(index as u8), wanted));
                    }
                }
            }
            (changes, std::mem::take(&mut state.adds), std::mem::take(&mut state.removes))
        };
        for (stream, wanted) in changes {
            let wish = wanted.unwrap_or(NO_WISH);
            log::info!(
                "displays: {} the video of display {}{}",
                if wish.enabled { "asking for" } else { "ending" },
                stream - DESKTOP_STREAM + 1,
                if wish.enabled { format!(" at up to {} pixels across and {} pictures a second", wish.max_width, wish.max_fps) } else { String::new() }
            );
            link.duplex.send_control(&wish_message(stream, wish)).ok();
        }
        if newer {
            for _ in 0..adds {
                link.duplex.send_control(&ClientMsg::Display(DisplayRequest::Add)).ok();
            }
            for stream in removes {
                link.duplex.send_control(&ClientMsg::Display(DisplayRequest::Remove { stream })).ok();
            }
        }
        for index in 0..SLOTS {
            if keys[index] && last_key[index].elapsed() >= KEY_REQUEST_GAP {
                last_key[index] = Instant::now();
                link.duplex.send_control(&ClientMsg::RequestKeyFrame { stream: display_stream(index as u8), frame: frames[index] }).ok();
                owner.uplink.lock().unwrap().key[index] = false;
            }
        }
        for index in 0..SLOTS {
            let pointer = pointers[index];
            if pointer_dirty[index] || (pointer.busy() && last_send[index].elapsed() >= POINTER_KEEPALIVE) {
                last_send[index] = Instant::now();
                let datagram = pointer_datagram(link.welcome.version, display_stream(index as u8), seq[index], &pointer);
                seq[index] = seq[index].wrapping_add(1);
                link.duplex.send_datagram(&datagram).ok();
                pointers_sent += 1;
            }
        }
        if summary_at.elapsed() >= SUMMARY_EVERY {
            summary_at = Instant::now();
            let (received, submitted, wished) = owner.totals();
            log::info!(
                "displays: {wished} wished; in the last {} s: {} pictures received, {} sent to a decoder; {pointers_sent} pointer datagrams sent in all",
                SUMMARY_EVERY.as_secs(),
                received.saturating_sub(seen.0),
                submitted.saturating_sub(seen.1)
            );
            seen = (received, submitted);
        }
    }
}

impl Displays {
    /// Pictures that arrived and pictures that went into a decoder, since the session began, and the
    /// number of displays that the client wishes now.
    fn totals(&self) -> (u64, u64, usize) {
        let mut sum = (0, 0);
        for slot in &self.slots {
            if let Ok(inner) = slot.inner.try_lock() {
                sum = (sum.0 + inner.counts.units, sum.1 + inner.counts.submitted);
            }
        }
        (sum.0, sum.1, self.state.lock().unwrap().wanted.iter().flatten().count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranpose::PointerButton;
    use cranvr_proto::wire::Codec;

    fn params(stream: u8, visible: bool) -> DesktopParams {
        DesktopParams {
            stream,
            codec: Codec::Hevc,
            size: [3840, 2160],
            points: [1920.0, 1080.0],
            max_fps: 60.0,
            bitrate_bps: 40_000_000,
            full_range: true,
            flipped: false,
            visible,
            cursor_in_video: true,
        }
    }

    fn info(stream: u8, removable: bool) -> DisplayInfo {
        DisplayInfo { stream, name: format!("Display {}", stream - DESKTOP_STREAM + 1), points: [1920, 1080], removable }
    }

    fn wish(width: u32) -> DesktopWish {
        DesktopWish { enabled: true, max_width: width, max_fps: 30 }
    }

    /// A set of displays that no other test shares, in a session that shows displays
    fn fresh(version: u16) -> Displays {
        let owner = Displays::new();
        let _units = owner.session_start(true, version);
        owner
    }

    #[test]
    fn a_host_of_version_8_says_that_a_game_runs_and_a_host_of_an_older_version_hides_its_display() {
        let owner = fresh(8);
        assert_eq!(owner.game_runs(), None);
        owner.on_message(&HostMsg::GameRuns(true));
        assert_eq!(owner.game_runs(), Some(true));
        owner.on_message(&HostMsg::GameRuns(false));
        assert_eq!(owner.game_runs(), Some(false));

        // A host of version 7 offers the first display and hides it while a game runs
        let old = fresh(7);
        assert_eq!(old.game_runs(), None);
        old.on_message(&HostMsg::Desktop(Some(params(DESKTOP_STREAM, true))));
        assert_eq!(old.game_runs(), Some(false));
        old.on_message(&HostMsg::Desktop(Some(params(DESKTOP_STREAM, false))));
        assert_eq!(old.game_runs(), Some(true));
        // A session that shows no display says nothing of a game
        old.session_end();
        assert_eq!(old.game_runs(), None);
    }

    #[test]
    fn the_menu_lists_the_hosts_displays_and_a_host_of_an_older_version_gets_one_entry() {
        let owner = fresh(8);
        assert!(owner.menu_view().displays.is_empty());
        owner.on_message(&HostMsg::Displays(DisplayList { displays: vec![info(1, false), info(2, true)], can_add: true, notice: Some("one".into()) }));
        let view = owner.menu_view();
        assert_eq!(view.displays.iter().map(|d| d.info.stream).collect::<Vec<_>>(), vec![1, 2]);
        assert!(view.can_add);
        assert_eq!(view.notice.as_deref(), Some("one"));
        assert_eq!(owner.listed_streams(), Some(vec![1, 2]));

        let old = fresh(7);
        assert!(old.menu_view().displays.is_empty());
        old.on_message(&HostMsg::Desktop(Some(params(DESKTOP_STREAM, true))));
        let view = old.menu_view();
        assert_eq!(view.displays.len(), 1);
        assert_eq!((view.displays[0].info.name.as_str(), view.displays[0].info.removable, view.can_add), ("Mac display", false, false));
        assert_eq!(old.listed_streams(), None);
    }

    #[test]
    fn only_the_displays_that_exist_are_asked_for() {
        let wishes = [Some(wish(3840)), Some(wish(2560)), Some(wish(2560)), None];
        // Before the host's list only the first display exists, as for a host of an older version
        let owner = fresh(8);
        assert_eq!(owner.existing(wishes), [Some(wish(3840)), None, None, None]);
        owner.on_message(&HostMsg::Displays(DisplayList { displays: vec![info(1, false), info(3, true)], can_add: false, notice: None }));
        assert_eq!(owner.existing(wishes), [Some(wish(3840)), None, Some(wish(2560)), None]);
    }

    #[test]
    fn a_display_that_leaves_the_list_loses_its_stream() {
        let owner = fresh(8);
        owner.on_message(&HostMsg::DisplayStream { stream: 2, params: Some(params(2, true)) });
        assert!(owner.slots[1].quick.params.lock().unwrap().is_some());
        owner.on_message(&HostMsg::Displays(DisplayList { displays: vec![info(1, false)], can_add: true, notice: None }));
        assert!(owner.slots[1].quick.params.lock().unwrap().is_none());
        assert!(!owner.view(2).visible);
    }

    #[test]
    fn a_change_of_the_wish_and_a_request_to_add_wake_the_uplink() {
        let owner = fresh(8);
        owner.uplink.lock().unwrap().dirty = false;
        owner.set_wanted([None; SLOTS]);
        assert!(!owner.uplink.lock().unwrap().dirty, "the same wish is nothing new");
        owner.set_wanted([Some(wish(2560)), None, None, None]);
        assert!(owner.uplink.lock().unwrap().dirty);
        owner.uplink.lock().unwrap().dirty = false;
        owner.request_add();
        owner.request_remove(3);
        let state = owner.state.lock().unwrap();
        assert_eq!((state.adds, state.removes.clone()), (1, vec![3]));
        assert!(owner.uplink.lock().unwrap().dirty);
    }

    #[test]
    fn a_pointer_goes_to_the_display_that_the_ray_is_on() {
        let owner = fresh(8);
        owner.uplink.lock().unwrap().pointer_dirty = [false; SLOTS];
        owner.update_pointers(3, |pointers| pointers.on_event(Hand::Right, PointerEventKind::Enter, [0.5, 0.5]));
        let uplink = owner.uplink.lock().unwrap();
        assert_eq!(uplink.pointer_dirty, [false, false, true, false]);
        assert_eq!(uplink.pointer[2].hit, Some([0.5, 0.5]));
        drop(uplink);
        // A stream that is no display's changes nothing
        owner.update_pointers(0, |pointers| pointers.on_event(Hand::Right, PointerEventKind::Enter, [0.1, 0.1]));
        assert_eq!(owner.uplink.lock().unwrap().pointer_dirty, [false, false, true, false]);
        // A ray that leaves lets the display's pointer go
        owner.reset_pointers(3);
        assert_eq!(owner.uplink.lock().unwrap().pointer[2], PointerNow::default());
    }

    #[test]
    fn the_cursor_of_a_display_is_the_newest_datagram_on_the_display() {
        let owner = fresh(8);
        owner.on_cursor(2, Cursor { seq: 5, u: 0.2, v: 0.3, on: true });
        owner.on_cursor(2, Cursor { seq: 4, u: 0.9, v: 0.9, on: true });
        assert_eq!(owner.cursor(2), Some([0.2, 0.3]));
        owner.on_cursor(2, Cursor { seq: 6, u: 0.0, v: 0.0, on: false });
        assert_eq!(owner.cursor(2), None);
        assert_eq!(owner.cursor(1), None);
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
    fn each_display_and_each_protocol_version_has_its_datagram() {
        let pointer = PointerNow { hit: Some([0.1, 0.2]), buttons: mouse::LEFT | mouse::RIGHT, scroll: [0.3, 0.6] };
        // The first display uses the datagrams of the older versions: Mouse, and Pointer for a host of version 6
        match pointer_datagram(8, DESKTOP_STREAM, 4, &pointer) {
            ClientDatagram::Mouse(m) => assert_eq!((m.seq, m.hit, m.buttons, m.scroll), (4, Some([0.1, 0.2]), mouse::LEFT | mouse::RIGHT, [0.3, 0.6])),
            other => panic!("{other:?}"),
        }
        match pointer_datagram(6, DESKTOP_STREAM, 5, &pointer) {
            ClientDatagram::Pointer(p) => assert_eq!((p.seq, p.hit, p.trigger, p.scroll), (5, Some([0.1, 0.2]), 1.0, 0.6)),
            other => panic!("{other:?}"),
        }
        // Another display names its stream
        match pointer_datagram(8, 3, 6, &pointer) {
            ClientDatagram::DisplayMouse { stream, mouse } => assert_eq!((stream, mouse.seq, mouse.buttons), (3, 6, mouse::LEFT | mouse::RIGHT)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_wish_of_the_layout_follows_the_displays_that_show() {
        let mut layout = Layout::default();
        let wishes = wishes_of(&layout);
        assert!(wishes[0].is_some() && wishes[1..].iter().all(Option::is_none));
        layout.set_shown(crate::panel_layout::PanelId::Display(2), true);
        layout.toggle_pinned(2);
        let wishes = wishes_of(&layout);
        // Two displays show: the first asks for less, and the one on the hand asks for the hand's size
        assert_eq!(wishes[1].map(|w| (w.max_width, w.max_fps)), Some((1920, 30)));
        assert_eq!(wishes[0].map(|w| w.max_fps), Some(30));
        layout.set_in_game(true);
        assert!(wishes_of(&layout).iter().all(Option::is_none), "a game starts with no display shown");
    }
}
