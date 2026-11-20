//! The Mac's displays as panels in the headset.
//!
//! The client shows each display as a panel of its own. Each display is one video stream with the
//! pictures of one capture (see `desktop.rs`). The first display, the Mac's main display or the
//! virtual display that the host was started with, exists from the start. The client can add more:
//! the host then makes a virtual display of 1920 x 1080 points, to the right of the Mac's others,
//! and removes it when the client asks, or when the client leaves. Four displays at most.
//!
//! A display costs only while a client shows it. The client names the displays that it shows with
//! `ClientMsg::DisplayWish`. A display with no wish has no capture, no encoder and sends nothing,
//! not even its cursor. This file counts, for each look at the displays, what they captured, encoded
//! and sent, and writes the sums to the log, so a run can prove that a game with no display shown
//! costs the Mac nothing.
//!
//! Each added display lives in a helper process (see `virtual_display.rs`). Making one takes
//! seconds, so a request for one runs on a thread of its own.

use super::{
    desktop::{Desktop, DisplayCounts, Options},
    Session,
};
use crate::{
    desktop_layer::LayerState,
    virtual_display::{self, DisplayProcess},
};
use cranvr_proto::{
    message::{display_stream, DesktopWish, DisplayInfo, DisplayList, DisplayRequest, HostMsg, Pointer, DESKTOP_STREAM, MAX_DISPLAYS},
    DISPLAYS_VERSION,
};
use log::{info, warn};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex, Weak,
    },
    thread,
    time::{Duration, Instant},
};

/// The size in points of a display that the client adds. With HiDPI it has 3840 x 2160 pixels.
const ADDED_SIZE: [u32; 2] = [1920, 1080];
/// How often the manager looks at the layer's flags, the game and the list
const LOOK_EVERY: Duration = Duration::from_millis(50);
/// How often it checks that the added displays still exist
const ALIVE_EVERY: Duration = Duration::from_secs(1);
/// How often it writes what the displays did into the log. `CRANVR_DISPLAYS_LOG_SECS` changes it, so a
/// test can look at short windows.
fn log_every() -> Duration {
    std::env::var("CRANVR_DISPLAYS_LOG_SECS").ok().and_then(|v| v.parse::<f64>().ok()).filter(|s| *s >= 0.2).map_or(Duration::from_secs(5), Duration::from_secs_f64)
}

/// One display and what belongs to it.
struct Slot {
    desktop: Desktop,
    /// What the display's own loops share with the manager. The manager copies it to the host's
    /// shared layer state, because the frame loop wants one answer for all the displays.
    layer: Arc<LayerState>,
    /// The helper that holds an added display. The first display has none.
    process: Option<DisplayProcess>,
    name: String,
    /// The display's ID when the host knows it, for the size and for the check that it still exists
    display_id: Option<u32>,
    /// Checks in a row that found the display gone
    missing: u8,
}

/// The displays of one session.
pub(super) struct Displays {
    base: Options,
    session: Weak<Session>,
    slots: Mutex<Vec<Slot>>,
    /// A request to add a display is in progress
    adding: AtomicBool,
    /// What went wrong with the last request, in plain words, until the next request
    notice: Mutex<Option<String>>,
    /// What the displays that were removed had done
    retired: Mutex<DisplayCounts>,
    stop: AtomicBool,
}

/// Each display of this host that macOS keeps apart needs its own serial number. Two hosts that run
/// together (a test host beside the app's) must not share one, so the number starts from the
/// process's own.
fn next_serial() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    1000 + (std::process::id() % 40_000) * 16 + NEXT.fetch_add(1, Ordering::Relaxed) % 16
}

impl Displays {
    /// Makes the first display and starts the manager's thread. It ends with the session or with
    /// `stop`.
    pub(super) fn start(options: Options, session: Arc<Session>) -> Arc<Self> {
        let first = Slot::new(&options, &session, None, if options.display_id.is_some() { "Virtual display" } else { "Mac display" }.to_string());
        let displays = Arc::new(Self {
            base: options,
            session: Arc::downgrade(&session),
            slots: Mutex::new(vec![first]),
            adding: AtomicBool::new(false),
            notice: Mutex::new(None),
            retired: Mutex::new(DisplayCounts::default()),
            stop: AtomicBool::new(false),
        });
        let (d, s) = (Arc::clone(&displays), session);
        thread::Builder::new().name("native-displays".into()).spawn(move || manage(&d, &s)).ok();
        displays
    }

    /// Ends the displays: every capture stops and each added display goes away.
    pub(super) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let slots = std::mem::take(&mut *self.slots.lock().unwrap());
        for slot in slots {
            slot.retire(&mut self.retired.lock().unwrap());
        }
        self.base.layer.active.store(false, Ordering::Relaxed);
    }

    /// The client asks for the video of a display, or stops it.
    pub(super) fn on_wish(&self, stream: u8, wish: DesktopWish, session: &Session) {
        match self.slots.lock().unwrap().iter().find(|slot| slot.desktop.stream() == stream) {
            Some(slot) => slot.desktop.on_wish(wish, session),
            None => info!("Native displays: the client wishes the video of stream {stream}, and no display has it"),
        }
    }

    pub(super) fn on_pointer(&self, stream: u8, pointer: Pointer) {
        if let Some(slot) = self.slots.lock().unwrap().iter().find(|slot| slot.desktop.stream() == stream) {
            slot.desktop.on_pointer(pointer);
        }
    }

    pub(super) fn request_key_frame(&self, stream: u8) {
        if let Some(slot) = self.slots.lock().unwrap().iter().find(|slot| slot.desktop.stream() == stream) {
            slot.desktop.request_key_frame();
        }
    }

    /// Every display's next picture is a key frame, as after the headset's app returned.
    pub(super) fn request_key_frames(&self) {
        for slot in self.slots.lock().unwrap().iter() {
            slot.desktop.request_key_frame();
        }
    }

    /// The client adds a display or removes one.
    pub(super) fn on_request(self: &Arc<Self>, request: DisplayRequest) {
        *self.notice.lock().unwrap() = None;
        match request {
            DisplayRequest::Add => {
                if self.adding.swap(true, Ordering::SeqCst) {
                    return;
                }
                let displays = Arc::clone(self);
                thread::Builder::new()
                    .name("native-display-add".into())
                    .spawn(move || {
                        displays.add();
                        displays.adding.store(false, Ordering::SeqCst);
                    })
                    .ok();
            }
            DisplayRequest::Remove { stream } => self.remove(stream),
        }
    }

    fn add(&self) {
        let Some(session) = self.session.upgrade() else { return };
        let fail = |text: String| {
            warn!("Native displays: {text}");
            *self.notice.lock().unwrap() = Some(text);
        };
        if !self.base.enabled {
            return fail("This Mac's cranvr runs without the desktop, so it adds no display.".to_string());
        }
        if !virtual_display::is_supported() {
            return fail("This version of macOS cannot make a display for the headset.".to_string());
        }
        let Some(stream) = (1..MAX_DISPLAYS)
            .map(display_stream)
            .find(|stream| !self.slots.lock().unwrap().iter().any(|slot| slot.desktop.stream() == *stream))
        else {
            return fail(format!("The Mac shows {MAX_DISPLAYS} displays at most. Remove one first."));
        };
        match DisplayProcess::start(ADDED_SIZE[0], ADDED_SIZE[1], next_serial()) {
            Ok(process) => {
                info!("Native displays: display {} is new on the Mac: {}", stream - DESKTOP_STREAM + 1, process.describe());
                let name = format!("Display {}", stream - DESKTOP_STREAM + 1);
                let display_id = process.id;
                let slot = Slot::new(&self.base, &session, Some((stream, process)), name);
                debug_assert_eq!(slot.display_id, Some(display_id));
                self.slots.lock().unwrap().push(slot);
            }
            Err(e) => fail(format!("The Mac made no display: {e}")),
        }
    }

    fn remove(&self, stream: u8) {
        let slot = {
            let mut slots = self.slots.lock().unwrap();
            match slots.iter().position(|slot| slot.desktop.stream() == stream && slot.process.is_some()) {
                Some(index) => slots.remove(index),
                None => {
                    info!("Native displays: the client removes stream {stream}, and no added display has it");
                    return;
                }
            }
        };
        info!("Native displays: {} removed at the client's request", slot.name);
        slot.retire(&mut self.retired.lock().unwrap());
    }

    /// The list that the client gets.
    fn list(&self) -> DisplayList {
        let slots = self.slots.lock().unwrap();
        let displays = if self.base.enabled {
            slots
                .iter()
                .map(|slot| DisplayInfo {
                    stream: slot.desktop.stream(),
                    name: slot.name.clone(),
                    points: virtual_display::points_of(slot.display_id.unwrap_or_else(virtual_display::main_display_id)),
                    removable: slot.process.is_some(),
                })
                .collect()
        } else {
            Vec::new()
        };
        DisplayList {
            can_add: self.base.enabled && slots.len() < usize::from(MAX_DISPLAYS) && virtual_display::is_supported(),
            displays,
            notice: self.notice.lock().unwrap().clone(),
        }
    }

    /// Removes an added display that macOS no longer has, as when the user removes it in the
    /// system's settings. A capture of a display that left fails for good. A display counts as gone
    /// when two checks in a row miss it: a new display can take a moment to draw.
    fn drop_gone(&self) {
        let active = virtual_display::active_displays();
        if active.is_empty() {
            return;
        }
        let gone: Vec<Slot> = {
            let mut slots = self.slots.lock().unwrap();
            for slot in slots.iter_mut().filter(|slot| slot.process.is_some()) {
                match slot.display_id {
                    Some(id) if !active.contains(&id) => slot.missing = slot.missing.saturating_add(1),
                    _ => slot.missing = 0,
                }
            }
            let (gone, kept): (Vec<Slot>, Vec<Slot>) = std::mem::take(&mut *slots).into_iter().partition(|slot| slot.missing >= 2);
            *slots = kept;
            gone
        };
        for slot in gone {
            warn!("Native displays: {} left the Mac", slot.name);
            *self.notice.lock().unwrap() = Some(format!("{} left the Mac.", slot.name));
            slot.retire(&mut self.retired.lock().unwrap());
        }
    }

    /// What the displays did in all, the removed ones included.
    fn totals(&self) -> (DisplayCounts, usize, usize) {
        let slots = self.slots.lock().unwrap();
        let mut sum = *self.retired.lock().unwrap();
        for slot in slots.iter() {
            let c = slot.desktop.counts();
            sum = DisplayCounts { captured: sum.captured + c.captured, encoded: sum.encoded + c.encoded, sent: sum.sent + c.sent, cursors: sum.cursors + c.cursors };
        }
        (sum, slots.len(), slots.iter().filter(|slot| slot.desktop.is_running()).count())
    }

    /// Copies what the host's frame loop shares with the displays: it tells them whether a game
    /// runs, and it hears whether any display shows and how many pictures went out.
    fn mirror(&self, game: bool) {
        let slots = self.slots.lock().unwrap();
        let mut active = false;
        let mut frames = 0;
        for slot in slots.iter() {
            slot.layer.game.store(game, Ordering::Relaxed);
            active |= slot.layer.active.load(Ordering::Relaxed);
            frames += slot.layer.frames.swap(0, Ordering::Relaxed);
        }
        self.base.layer.active.store(active, Ordering::Relaxed);
        self.base.layer.frames.fetch_add(frames, Ordering::Relaxed);
    }
}

impl Slot {
    /// Starts the display's loops. An added display brings its stream and its helper.
    fn new(base: &Options, session: &Arc<Session>, added: Option<(u8, DisplayProcess)>, name: String) -> Self {
        let layer: Arc<LayerState> = Arc::default();
        let (stream, display_id, process) = match added {
            Some((stream, process)) => (stream, Some(process.id), Some(process)),
            None => (DESKTOP_STREAM, base.display_id, None),
        };
        let options = Options { layer: Arc::clone(&layer), stream, display_id, ..base.clone() };
        Self { desktop: Desktop::start(options, Arc::clone(session)), layer, process, name, display_id, missing: 0 }
    }

    /// The display is gone: its loops end, its helper ends and its counts stay in `retired`.
    fn retire(self, retired: &mut DisplayCounts) {
        self.desktop.stop();
        let c = self.desktop.counts();
        *retired = DisplayCounts { captured: retired.captured + c.captured, encoded: retired.encoded + c.encoded, sent: retired.sent + c.sent, cursors: retired.cursors + c.cursors };
    }
}

/// The manager's loop: the game's state and the list go to the client when they change, the added
/// displays are checked and the counts go to the log.
fn manage(displays: &Displays, session: &Session) {
    let mut game_sent: Option<bool> = None;
    let mut list_sent: Option<DisplayList> = None;
    let (mut alive_at, mut log_at) = (Instant::now(), Instant::now());
    let mut seen = DisplayCounts::default();
    let log_every = log_every();
    let send = |message: &HostMsg| session.link.duplex.send_control(message).ok();
    while !displays.stop.load(Ordering::Relaxed) && !*session.stopped.lock().unwrap() {
        thread::sleep(LOOK_EVERY);
        let game = displays.base.layer.game.load(Ordering::Relaxed);
        displays.mirror(game);
        if session.version >= DISPLAYS_VERSION {
            if game_sent != Some(game) {
                game_sent = Some(game);
                send(&HostMsg::GameRuns(game));
            }
            let list = displays.list();
            if list_sent.as_ref() != Some(&list) {
                send(&HostMsg::Displays(list.clone()));
                list_sent = Some(list);
            }
        }
        if alive_at.elapsed() >= ALIVE_EVERY {
            alive_at = Instant::now();
            displays.drop_gone();
        }
        if log_at.elapsed() >= log_every {
            log_at = Instant::now();
            let (now, known, shown) = displays.totals();
            let d = |a: u64, b: u64| a.saturating_sub(b);
            info!(
                "Native displays: {known} known, {shown} shown, {}; in the last {:.1} s: {} pictures captured, {} encoded, {} sent, {} cursor datagrams sent",
                if game { "a game runs" } else { "no game runs" },
                log_every.as_secs_f64(),
                d(now.captured, seen.captured),
                d(now.encoded, seen.encoded),
                d(now.sent, seen.sent),
                d(now.cursors, seen.cursors),
            );
            seen = now;
        }
    }
}
