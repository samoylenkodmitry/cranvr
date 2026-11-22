//! The console: the menu that the host offers in the headset (its games and settings), the
//! requests that the user sends back, and the rules for when the console shows and where.
//!
//! The cranvr app on the Mac writes the games and the settings into `vr-menu.json`. The host adds
//! the stream's state and sends the text as `HostMsg::Menu`. A press on a button sends a request
//! as `ClientMsg::Console`. The host hands it to the app as a `vr_request` status line.
//!
//! The console also holds two settings of the client itself: the room behind the panels and the
//! sound's volume. They never go to the host. `settings.rs` saves them.
//!
//! This file holds the data and the rules, with no Cranpose state. `console_panel.rs` draws them
//! and places the panel.

use cranvr_common::controls::{Action as ControlAction, Control, Controls, Hand, MenuHand, ModelsInGame};
use serde_json::{json, Value};
use std::{
    sync::atomic::{AtomicU8, Ordering},
    time::{Duration, Instant},
};

/// The hand menu asks for the console on its page of the controls (see `hand_menu.rs`). The console
/// takes the request when it opens, or while it is open: 0 is none, 1 the left controller's tab and 2
/// the right controller's.
static CONTROLS_REQUEST: AtomicU8 = AtomicU8::new(0);

/// Asks for the console on its page of the controls, on the tab of the right controller.
pub fn request_controls_page() {
    CONTROLS_REQUEST.store(2, Ordering::Relaxed);
}

/// The tab that the hand menu asked for, once.
pub fn take_controls_request() -> Option<Hand> {
    match CONTROLS_REQUEST.swap(0, Ordering::Relaxed) {
        1 => Some(Hand::Left),
        2 => Some(Hand::Right),
        _ => None,
    }
}

/// The refresh rates and the render sizes that the console offers. The app accepts any value.
pub const RATES: [u32; 3] = [72, 90, 120];
pub const SCALES: [f32; 5] = [0.5, 0.6, 0.7, 0.85, 1.0];
/// The most games that the console lists
pub const MAX_GAMES: usize = 5;

/// The stream's state, from the host.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// The headset's refresh rate in Hz
    pub refresh: f32,
    /// Frames per second that the host sends
    pub fps: f32,
    /// Frames per second that the game makes
    pub game_fps: f32,
    pub latency_ms: Option<f32>,
    /// The program file of the game that runs, such as "Beat Saber.exe"
    pub game: Option<String>,
}

/// The VR system of a game on the Mac: the runtime that its OpenVR calls reach. The cranvr app keeps it in the
/// game's launch options and sends it with the menu. An OpenXR game ignores it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VrSystem {
    /// OpenComposite turns the game's OpenVR calls into OpenXR calls to cranvr's runtime
    #[default]
    OpenComposite,
    /// The game runs in SteamVR on the Mac, with cranvr's driver
    SteamVr,
    /// No OpenVR runtime: a game with an OpenXR mode takes that
    Off,
}

impl VrSystem {
    pub const ALL: [VrSystem; 3] = [VrSystem::OpenComposite, VrSystem::SteamVr, VrSystem::Off];

    /// The word of the menu and of the request, as the app's app.json has it
    pub fn key(self) -> &'static str {
        match self {
            VrSystem::OpenComposite => "opencomposite",
            VrSystem::SteamVr => "steamvr",
            VrSystem::Off => "off",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|system| system.key() == key)
    }

    /// The button's text
    pub fn label(self) -> &'static str {
        match self {
            VrSystem::OpenComposite => "OpenComposite",
            VrSystem::SteamVr => "SteamVR",
            VrSystem::Off => "No OpenVR",
        }
    }

    /// The choice after this one, for a press on the button
    pub fn next(self) -> Self {
        let at = Self::ALL.iter().position(|system| *system == self).unwrap_or(0);
        Self::ALL[(at + 1) % Self::ALL.len()]
    }
}

/// An installed game: its name, its program's file name and its VR system.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Game {
    pub name: String,
    pub program: String,
    pub vr: VrSystem,
}

/// What the console shows: the app's games and settings and the host's state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Menu {
    pub games: Vec<Game>,
    pub fps: u32,
    pub game_scale: f32,
    /// The most MSAA samples a game may use; 0 lets the game choose
    pub msaa: u32,
    pub status: Status,
    /// One short line from the app about something that happens now, such as a game that restarts.
    /// The app writes it into vr-menu.json while it applies.
    pub notice: Option<String>,
    /// What the buttons of the controllers do, as the cranvr app on the Mac keeps it
    pub controls: Controls,
}

impl Menu {
    /// The menu from the host's text. `None` for a text that is no menu: it lacks the games, a game's
    /// name or VR system, a setting or the controls. The status is the host's own and is missing while no
    /// headset streams.
    pub fn parse(text: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(text).ok()?;
        let s = &v["status"];
        let number = |v: &Value| v.as_f64().map(|n| n as f32);
        Some(Self {
            games: v["games"]
                .as_array()?
                .iter()
                .map(|g| {
                    Some(Game {
                        name: g["name"].as_str()?.to_string(),
                        program: g["program"].as_str().unwrap_or_default().to_string(),
                        vr: VrSystem::from_key(g["vr"].as_str()?)?,
                    })
                })
                .collect::<Option<Vec<Game>>>()?,
            fps: v["fps"].as_u64()? as u32,
            game_scale: number(&v["game_scale"])?,
            msaa: v["msaa"].as_u64()? as u32,
            status: Status {
                refresh: number(&s["refresh"]).unwrap_or(0.0),
                fps: number(&s["fps"]).unwrap_or(0.0),
                game_fps: number(&s["game_fps"]).unwrap_or(0.0),
                latency_ms: number(&s["latency_ms"]),
                game: s["game"].as_str().map(str::to_string),
            },
            notice: v["notice"].as_str().filter(|notice| !notice.is_empty()).map(str::to_string),
            controls: Controls::from_json(&v["controls"].to_string())?,
        })
    }

    /// Whether `game` is the game that runs.
    pub fn runs(&self, game: &Game) -> bool {
        self.status.game.as_deref().is_some_and(|program| !game.program.is_empty() && program.eq_ignore_ascii_case(&game.program))
    }

    /// The games that the console lists
    pub fn listed(&self) -> &[Game] {
        &self.games[..self.games.len().min(MAX_GAMES)]
    }
}

/// How long a request to the Mac may run before the console says that it got no answer. A game's
/// start waits for Steam and for Wine, which can take a minute.
pub const ANSWER_TIME: Duration = Duration::from_secs(90);

/// A request to the Mac that takes a while, such as a game's start. The console shows that it runs
/// from the press until the host's menu shows its result or a new notice.
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub action: Action,
    since: Instant,
    /// The notice that the menu showed at the press: a new notice answers the request
    notice: Option<String>,
}

/// Where a request to the Mac stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// The Mac works on it
    Running,
    /// The Mac gave no answer within `ANSWER_TIME`
    NoAnswer,
    /// The menu shows its result, or the Mac said why it failed
    Done,
}

impl Pending {
    /// The request of `action` at `now`, while the menu shows `notice`.
    pub fn new(action: Action, notice: Option<String>, now: Instant) -> Self {
        Self { action, since: now, notice }
    }

    /// Where the request stands in the host's `menu` at `now`.
    pub fn progress(&self, menu: &Menu, now: Instant) -> Progress {
        if menu.notice.is_some() && menu.notice != self.notice {
            return Progress::Done;
        }
        let done = match &self.action {
            Action::Play(name) => menu.games.iter().any(|game| &game.name == name && menu.runs(game)),
            Action::Stop => menu.status.game.is_none(),
            Action::Fps(fps) => menu.fps == *fps,
            Action::Scale(scale) => (menu.game_scale - scale).abs() < 0.01,
            Action::Msaa(samples) => menu.msaa == *samples,
            Action::VrSystem(game, system) => menu.games.iter().any(|g| &g.name == game && g.vr == *system),
        };
        if done {
            Progress::Done
        } else if now.saturating_duration_since(self.since) > ANSWER_TIME {
            Progress::NoAnswer
        } else {
            Progress::Running
        }
    }
}

impl Action {
    /// The words of the button while the Mac works on the request
    pub fn running_label(&self) -> &'static str {
        match self {
            Action::Play(_) => "Starting...",
            Action::Stop => "Stopping...",
            Action::Fps(_) | Action::Scale(_) | Action::Msaa(_) | Action::VrSystem(..) => "Changing...",
        }
    }
}

/// What a console button asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Play(String),
    Stop,
    Fps(u32),
    Scale(f32),
    Msaa(u32),
    /// A game's VR system, by the game's name
    VrSystem(String, VrSystem),
}

impl Action {
    /// The request for the host, as JSON.
    pub fn request(&self) -> String {
        match self {
            Action::Play(name) => json!({"action": "play", "game": name}),
            Action::Stop => json!({"action": "stop"}),
            Action::Fps(fps) => json!({"action": "set", "fps": fps}),
            Action::Scale(scale) => json!({"action": "set", "game_scale": scale}),
            Action::Msaa(samples) => json!({"action": "set", "msaa": samples}),
            Action::VrSystem(game, system) => json!({"action": "set", "game": game, "openvr": system.key()}),
        }
        .to_string()
    }
}

/// What the headset shows behind the panels while no game runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Room {
    /// The user's room, through the headset's cameras
    #[default]
    Passthrough,
    /// The host's own scene, as the stream shows it
    Scene,
}

impl Room {
    pub const ALL: [Room; 2] = [Room::Passthrough, Room::Scene];

    /// The text under which `settings.rs` saves the choice
    pub fn key(self) -> &'static str {
        match self {
            Room::Passthrough => "passthrough",
            Room::Scene => "scene",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|room| room.key() == key.trim())
    }

    /// The button's text
    pub fn label(self) -> &'static str {
        match self {
            Room::Passthrough => "Passthrough",
            Room::Scene => "Scene",
        }
    }
}

/// How loud the host's sound plays, as a multiple of the level that the host sends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Volume {
    P100,
    #[default]
    P150,
    P200,
    P300,
}

impl Volume {
    pub const ALL: [Volume; 4] = [Volume::P100, Volume::P150, Volume::P200, Volume::P300];

    pub fn percent(self) -> u32 {
        match self {
            Volume::P100 => 100,
            Volume::P150 => 150,
            Volume::P200 => 200,
            Volume::P300 => 300,
        }
    }

    /// The factor that multiplies each sample
    pub fn gain(self) -> f32 {
        self.percent() as f32 / 100.0
    }

    /// The text under which `settings.rs` saves the choice
    pub fn key(self) -> String {
        self.percent().to_string()
    }

    pub fn from_key(key: &str) -> Option<Self> {
        let percent = key.trim().parse::<u32>().ok()?;
        Self::ALL.into_iter().find(|volume| volume.percent() == percent)
    }

    pub fn label(self) -> String {
        format!("{}%", self.percent())
    }
}

/// What a console button does. Most of them ask the host. The room and the volume are the client's
/// own settings. The controller models in a game and the menu hand go to the Mac with the controls.
#[derive(Clone, Debug, PartialEq)]
pub enum Press {
    Host(Action),
    Room(Room),
    Volume(Volume),
    Models(ModelsInGame),
    MenuHand(MenuHand),
}

/// What decides whether the room shows behind the panels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Backdrop {
    pub room: Room,
    /// A game runs on the host
    pub game_runs: bool,
}

impl Backdrop {
    /// Whether the room shows through the headset's cameras. It does while no game runs and the
    /// user chose the room. A headset that cannot show the room (`available` is false) keeps the
    /// host's scene, because the app's own background is black.
    pub fn shows_room(&self, available: bool) -> bool {
        available && self.room == Room::Passthrough && !self.game_runs
    }
}

/// Whether a game runs on the host. The host's hiding of the desktop tells it (`desktop_hides`, see
/// `desktop.rs`): the host hides the desktop while a game sends frames and while it loads, and it
/// says so as soon as that changes. `None` means that the host offers no desktop. The menu then
/// tells: its status names the game that is connected. The menu comes at most once a second, so it
/// is the slower signal. With neither, no game counts.
pub fn game_runs(menu: Option<&Menu>, desktop_hides: Option<bool>) -> bool {
    desktop_hides.unwrap_or_else(|| menu.is_some_and(|menu| menu.status.game.is_some()))
}

/// The sizes of the console's parts in dp. A dp is a millimeter in the room.
pub mod size {
    pub const WIDTH: f32 = 620.0;
    pub const PADDING: f32 = 26.0;
    pub const GAP: f32 = 10.0;
    pub const TITLE: f32 = 40.0;
    pub const TEXT: f32 = 30.0;
    pub const BUTTON: f32 = 46.0;
    pub const HEADING: f32 = 34.0;
}

/// The height of the Controls page in dp: the title, the two tabs for the controllers, one row for
/// each of the eight inputs, two lines of notes and the buttons at the bottom. A controller with no
/// menu button leaves its row empty, so both pages have one height.
pub fn controls_height() -> f32 {
    use size::*;
    let rows = |heights: &[f32]| heights.iter().map(|h| h + GAP).sum::<f32>();
    // The title, the tabs, a row for each input, the name and the choices of the menu hand, two lines
    // and the buttons
    2.0 * PADDING + rows(&[TITLE, BUTTON]) + Control::ALL.len() as f32 * (BUTTON + GAP) + rows(&[TEXT, BUTTON, TEXT, TEXT, BUTTON])
}

/// The console's height in dp for a menu: the rows that `console_panel.rs` draws, with their gaps.
pub fn height(menu: Option<&Menu>) -> f32 {
    use size::*;
    let rows = |heights: &[f32]| heights.iter().map(|h| h + GAP).sum::<f32>();
    let Some(menu) = menu else {
        // The title, a line of text, the heading with the two settings of the client and the buttons
        return 2.0 * PADDING + rows(&[TITLE, TEXT, HEADING, BUTTON, BUTTON, BUTTON]);
    };
    let games = menu.listed().len().max(1) as f32;
    // The notice has a line of its own
    let notice = if menu.notice.is_some() { TEXT + GAP } else { 0.0 };
    // The title, the game's state, the heading and the games, the heading and a row for each of the
    // five settings, the name and the choices of the controller models, the note and the buttons at
    // the bottom
    2.0 * PADDING
        + notice
        + rows(&[TITLE, TEXT, HEADING])
        + games * (BUTTON + GAP)
        + rows(&[HEADING, BUTTON, BUTTON, BUTTON, BUTTON, BUTTON, TEXT, BUTTON, TEXT, BUTTON])
}

/// How long the headset waits for the Mac to repeat a change of the controls that the user made in
/// the console. A menu with other controls that comes later wins.
const ECHO_WAIT: Duration = Duration::from_secs(3);

/// The page of the console that shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Main,
    /// The buttons of one controller
    Controls(Hand),
}

/// What the UI draws of the console.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub open: bool,
    pub menu: Option<Menu>,
    pub room: Room,
    pub volume: Volume,
    /// What the buttons do. The mapping that the Mac last sent, or the headset's copy of it while
    /// no link runs.
    pub controls: Controls,
    /// The Mac's app keeps the controls and the headset changes them through it, so a change needs a
    /// link
    pub controls_editable: bool,
}

/// The console's state: its menu and whether it shows.
#[derive(Default)]
pub struct Console {
    menu: Option<Menu>,
    open: bool,
    /// Whether a game ran at the last menu
    game_ran: Option<bool>,
    /// What the buttons of the controllers do. The Mac owns the mapping (`app.json`): its menu
    /// sets it, and the headset keeps the last one for the time without a link.
    controls: Controls,
    /// A change that the user made here and that the Mac has not repeated in a menu yet
    pending_controls: Option<(Controls, Instant)>,
    /// The console stays open when the link ends. A launch option sets it, for a run without a
    /// controller that could open the console.
    keep_open: bool,
    /// The client's own settings. The hub loads the saved ones when the app starts.
    room: Room,
    volume: Volume,
}

impl Console {
    pub fn view(&self) -> View {
        View {
            open: self.open,
            menu: self.menu.clone(),
            room: self.room,
            volume: self.volume,
            controls: self.controls,
            controls_editable: self.menu.is_some(),
        }
    }

    /// The mapping that the headset uses now.
    pub fn controls(&self) -> Controls {
        self.controls
    }

    /// Takes the mapping that the headset kept from an earlier link.
    pub fn set_controls(&mut self, controls: Controls) {
        self.controls = controls;
    }

    /// The user picks the next (`step` 1) or the previous (`step` -1) action for one button. The
    /// choice shows at once. Returns the mapping to send to the Mac, or `None` when no link runs,
    /// because the Mac keeps the mapping.
    pub fn step_control(&mut self, hand: Hand, control: Control, step: i32, now: Instant) -> Option<Controls> {
        self.menu.as_ref()?;
        let offers = control.offers();
        let at = offers.iter().position(|action| *action == self.controls.action(hand, control)).unwrap_or(0) as i32;
        let next = offers[(at + step).rem_euclid(offers.len() as i32) as usize];
        self.pick_control(hand, control, next, now)
    }

    /// The user picks `action` for one button. See `step_control`.
    pub fn pick_control(&mut self, hand: Hand, control: Control, action: ControlAction, now: Instant) -> Option<Controls> {
        self.menu.as_ref()?;
        let mut next = self.controls;
        next.set(hand, control, action).then_some(())?;
        self.show_controls(next, now)
    }

    /// The user picks when the controllers' models show in a game. See `step_control`.
    pub fn pick_models(&mut self, choice: ModelsInGame, now: Instant) -> Option<Controls> {
        self.menu.as_ref()?;
        let next = Controls { models_in_game: choice, ..self.controls };
        self.show_controls(next, now)
    }

    /// The user picks the hand that carries the hand menu. See `step_control`.
    pub fn pick_menu_hand(&mut self, choice: MenuHand, now: Instant) -> Option<Controls> {
        self.menu.as_ref()?;
        let next = Controls { menu_hand: choice, ..self.controls };
        self.show_controls(next, now)
    }

    /// The user asks for the standard mapping. See `step_control`.
    pub fn reset_controls(&mut self, now: Instant) -> Option<Controls> {
        self.menu.as_ref()?;
        self.show_controls(Controls::default(), now)
    }

    fn show_controls(&mut self, next: Controls, now: Instant) -> Option<Controls> {
        self.controls = next;
        self.pending_controls = Some((next, now));
        Some(next)
    }

    pub fn set_room(&mut self, room: Room) {
        self.room = room;
    }

    pub fn set_volume(&mut self, volume: Volume) {
        self.volume = volume;
    }

    /// What decides the room behind the panels. `desktop_hides` is the host's hiding of the desktop
    /// (see `game_runs`).
    pub fn backdrop(&self, desktop_hides: Option<bool>) -> Backdrop {
        Backdrop { room: self.room, game_runs: game_runs(self.menu.as_ref(), desktop_hides) }
    }

    /// Shows the console, or hides it.
    fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    /// Opens the console and keeps it open when the link ends.
    pub fn pin(&mut self) {
        self.keep_open = true;
        self.open = true;
    }

    /// A new menu from the host. The console shows when the first menu comes and while no game
    /// runs, and it hides when a game starts. The user's own toggle holds until the game starts or
    /// stops. Returns the mapping that the headset must keep for the time without a link, when the
    /// menu changed it.
    pub fn on_menu(&mut self, menu: Menu) -> Option<Controls> {
        self.on_menu_at(menu, Instant::now())
    }

    pub fn on_menu_at(&mut self, menu: Menu, now: Instant) -> Option<Controls> {
        let runs = menu.status.game.is_some();
        if self.game_ran != Some(runs) {
            self.game_ran = Some(runs);
            self.open = !runs;
        }
        let mut changed = None;
        let controls = menu.controls;
        // A change of the user's shows at once and the Mac repeats it a moment later. A menu that
        // was on its way before the Mac saw the change must not undo it.
        let waiting = match self.pending_controls {
            Some((asked, since)) if controls == asked || now.saturating_duration_since(since) > ECHO_WAIT => {
                self.pending_controls = None;
                false
            }
            Some(_) => true,
            None => false,
        };
        if !waiting && controls != self.controls {
            self.controls = controls;
            changed = Some(controls);
        }
        self.menu = Some(menu);
        changed
    }

    /// The link ended: the menu is gone.
    pub fn clear(&mut self) {
        self.menu = None;
        self.game_ran = None;
        self.open = self.keep_open;
        self.pending_controls = None;
    }

    /// Shows the console, or hides it: the action of a button, such as the menu button.
    pub fn toggle_open(&mut self) {
        self.toggle();
    }
}

#[cfg(test)]
#[path = "tests/console.rs"]
mod tests;
