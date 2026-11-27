//! Everything outside the window: the host child process, settings on disk and games under Wine.

use crate::{
    launch::{GameOptions, WineOptions},
    usb::Connection,
};
use cranvr_common::{controls::Controls, foveation::FoveationLevel};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, VecDeque},
    env,
    ffi::OsString,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

/// The TCP port of the native protocol, `cranvr-host`'s default for `--native-port`.
pub const NATIVE_PORT: u16 = 9950;

/// The port that the host listens on for the native protocol. `CRANVR_NATIVE_PORT` replaces it for a
/// test app that runs beside the user's: the user's host keeps 9950.
pub fn native_port() -> u16 {
    env::var("CRANVR_NATIVE_PORT").ok().and_then(|port| port.parse().ok()).unwrap_or(NATIVE_PORT)
}

/// The host's config dir; `cranvr-host` uses the same default.
pub fn config_dir() -> PathBuf {
    env::var_os("CRANVR_HOME").map(PathBuf::from).unwrap_or_else(|| {
        env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join("Library/Application Support/cranvr")
    })
}

/// A program that the user added from a folder ("Add a game from a folder" in the Play tab). cranvr starts
/// it like a Steam game: the Play tab and the console in VR list it, and it has launch options of its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddedGame {
    /// The name that the lists show: the folder's name
    pub name: String,
    /// The program, as a Mac path
    pub program: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub fps: u32,
    pub mbps: u32,
    /// How strongly the host foveates each eye. The key is new: an old file's `foveation`, which
    /// said yes or no, gives way to the standard level.
    pub foveation_level: FoveationLevel,
    pub desktop: bool,
    /// Desktop captures per second
    pub desktop_fps: u32,
    /// The last game started: a Windows path inside the Wine prefix, or a Mac path
    pub game: String,
    /// Start the host when the window opens
    pub autostart: bool,
    /// How the stream reaches the headset: by the USB cable when the headset is plugged into
    /// this Mac, or by Wi-Fi, or by the cable only (see usb.rs)
    pub connection: Connection,
    /// The headset plays this Mac's sound
    pub game_audio: bool,
    /// The desktop view shows a virtual display made for the headset, of this size in points;
    /// None (the default) shows the Mac's main display, as it is
    pub virtual_display: Option<(u32, u32)>,
    /// The virtual display is the main display while a headset is connected. Off by default:
    /// macOS then moves the menu bar, the Dock and every window of the main display into VR.
    pub vr_main_display: bool,
    /// The cranvr window moves into VR while a headset is connected. Off by default.
    pub window_in_vr: bool,
    /// A game renders each eye at this share of the stream's eye size
    pub game_scale: f32,
    /// Games under Wine may use at most this many MSAA samples; 0 leaves it to the game
    pub game_msaa_limit: u32,
    /// While a game runs, a display in a scaled mode shows its panel's own pixels
    pub unscale_displays: bool,
    /// A game without hand tracking gets controllers made of the headset's tracked hands, while the
    /// headset tracks no controller
    pub hand_controllers: bool,
    /// What each button of each controller does in the headset's panels (see
    /// `cranvr_common::controls`). The Controls page of the Settings tab and the headset's console
    /// change it. The host sends it to the headset in `vr-menu.json`.
    pub controls: Controls,
    /// The launch options of each Windows program, by its Windows path. A program with no entry
    /// starts with the default options.
    pub launch: BTreeMap<String, GameOptions>,
    /// The options of every Windows program that cranvr starts
    pub wine: WineOptions,
    /// The games that the user added from a folder, in the order of adding
    pub added_games: Vec<AddedGame>,
    /// What games under Wine hear from the device "cranvr microphone" (see `microphone.rs`)
    pub microphone: crate::microphone::MicrophoneSource,
    /// What the headset shows behind its panels while no game runs: "passthrough" or "scene". None
    /// until the headset tells its own choice once: the headset kept it before the Mac did.
    pub headset_room: Option<String>,
    /// How loud the headset plays the sound, in percent of what the host sends. None until the
    /// headset tells its own choice once.
    pub headset_volume: Option<u32>,
    /// How the headset draws its menus: "3d" or "flat". None until the headset tells its own choice
    /// once.
    pub headset_interface: Option<String>,
    /// The colors of the headset's 3D menus: "cyan" or "amber". None until the headset tells its
    /// own choice once.
    pub headset_theme: Option<String>,
    /// A recording of what the headset shows takes the headset's microphone too. Off by default.
    pub recording_microphone: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fps: 120,
            mbps: 100,
            foveation_level: FoveationLevel::default(),
            desktop: true,
            desktop_fps: 30,
            game: String::new(),
            autostart: true,
            connection: Connection::Automatic,
            game_audio: true,
            virtual_display: None,
            vr_main_display: false,
            window_in_vr: false,
            game_scale: 0.7,
            game_msaa_limit: 2,
            unscale_displays: true,
            hand_controllers: true,
            controls: Controls::default(),
            launch: BTreeMap::new(),
            wine: WineOptions::default(),
            added_games: Vec::new(),
            microphone: Default::default(),
            headset_room: None,
            headset_volume: None,
            headset_interface: None,
            headset_theme: None,
            recording_microphone: false,
        }
    }
}

impl Settings {
    fn path() -> PathBuf {
        config_dir().join("app.json")
    }

    /// The settings in `app.json`. An entry that cranvr cannot read keeps its standard value, and
    /// `settings_problem` names it. A missing file gives the standard settings.
    pub fn load() -> Self {
        let reading = Self::load_from(&Self::path());
        set_settings_problem(reading.problem(&Self::path()));
        reading.settings
    }

    fn load_from(path: &Path) -> Reading {
        match fs::read_to_string(path) {
            Ok(text) => Self::read(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Reading::default(),
            Err(e) => Reading { broken: Some(e.to_string()), ..Default::default() },
        }
    }

    /// Reads the text of `app.json` entry by entry. A wrong entry, such as an action name that no
    /// action has, costs only that entry: the rest of the file still counts.
    pub fn read(text: &str) -> Reading {
        let entries = match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(entries)) => entries,
            Ok(_) => return Reading { broken: Some("it holds no settings".into()), ..Default::default() },
            Err(e) => return Reading { broken: Some(e.to_string()), ..Default::default() },
        };
        let mut doc = as_value(&Settings::default()).unwrap_or_default();
        let mut unreadable = Vec::new();
        for (key, value) in &entries {
            take(&mut doc, std::slice::from_ref(key), value, &mut unreadable);
        }
        // `take` keeps only what reads, so the document reads as a whole
        let settings = serde_json::from_value(doc).unwrap_or_default();
        Reading { settings, unreadable, broken: None }
    }

    /// The text that a save writes in place of `file`, the text that `app.json` holds now. An entry
    /// of `file` that cranvr cannot read stays as it is, while its setting keeps the value that the
    /// read gave it. A file that cranvr cannot read at all gives an error: the save would replace
    /// every setting in it with a standard value.
    pub fn file_text(&self, file: Option<&str>) -> Result<String, String> {
        let reading = file.map(Self::read).unwrap_or_default();
        if let Some(reason) = reading.broken {
            return Err(format!("cranvr cannot read the settings file ({reason}), so it does not save over it"));
        }
        // A file with no wrong entry gets the settings in the order of their fields
        if reading.unreadable.is_empty() {
            return serde_json::to_string_pretty(self).map_err(|e| e.to_string());
        }
        let mut doc = as_value(self)?;
        let read = as_value(&reading.settings)?;
        let original: Value = serde_json::from_str(file.unwrap_or_default()).map_err(|e| e.to_string())?;
        for entry in &reading.unreadable {
            let unchanged = lookup(&doc, &entry.keep) == lookup(&read, &entry.keep);
            if let (true, Some(value)) = (unchanged, lookup(&original, &entry.keep)) {
                put(&mut doc, &entry.keep, value.clone());
            }
        }
        serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Err(e) = self.save_to(&path) {
            log(format!("Settings not saved: {e}"));
        }
        if let Err(e) = crate::microphone::write_choice(&config_dir(), &self.microphone) {
            log(format!("The microphone's choice is not written: {e}"));
        }
        set_settings_problem(Self::load_from(&path).problem(&path));
    }

    /// Writes the settings to `path` through a new file, so a crash leaves the old file or the new
    /// one and never a part of either.
    fn save_to(&self, path: &Path) -> Result<(), String> {
        let file = match fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.to_string()),
        };
        let text = self.file_text(file.as_deref())?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let new = path.with_extension("json.new");
        fs::write(&new, text).map_err(|e| e.to_string())?;
        fs::rename(&new, path).map_err(|e| e.to_string())
    }
}

/// What a read of `app.json` gave.
#[derive(Debug, Default)]
pub struct Reading {
    pub settings: Settings,
    /// The entries that cranvr could not read. Each setting keeps its standard value.
    pub unreadable: Vec<Unreadable>,
    /// Why cranvr could read nothing of the file, when it could not
    pub broken: Option<String>,
}

/// An entry of `app.json` that cranvr could not read.
#[derive(Clone, Debug, PartialEq)]
pub struct Unreadable {
    /// Where it is, such as `controls.right.grip`
    pub entry: String,
    /// Why it does not read, such as "no action is named grab"
    pub reason: String,
    /// The entry that a save keeps as the file has it: the entry itself, or the whole list that
    /// holds it, because a list has no names for its items
    keep: Vec<String>,
}

impl Reading {
    /// The window's error for this read of the file at `path`, if there is one.
    pub fn problem(&self, path: &Path) -> Option<String> {
        let file = path.display();
        if let Some(reason) = &self.broken {
            return Some(format!(
                "cranvr cannot read its settings file {file} ({reason}). The app runs with the standard settings and saves no change. Fix the file or remove it, then open cranvr again."
            ));
        }
        let entries: Vec<String> = self.unreadable.iter().map(|u| format!("{} ({})", u.entry, u.reason)).collect();
        match entries.len() {
            0 => None,
            1 => Some(format!(
                "cranvr cannot read one entry of its settings file {file}: {}. The app uses the standard value for it. The file keeps the entry until you change that setting.",
                entries[0]
            )),
            n => Some(format!(
                "cranvr cannot read {n} entries of its settings file {file}: {}. The app uses the standard value for each. The file keeps each entry until you change its setting.",
                entries.join("; ")
            )),
        }
    }
}

static SETTINGS_PROBLEM: Mutex<Option<String>> = Mutex::new(None);

fn set_settings_problem(problem: Option<String>) {
    *SETTINGS_PROBLEM.lock().unwrap() = problem;
}

/// What the last read or save of `app.json` found wrong in it, for the window.
pub fn settings_problem() -> Option<String> {
    SETTINGS_PROBLEM.lock().unwrap().clone()
}

/// Puts the file's `value` at `path` of `doc` when the settings still read with it. An object or a
/// list that does not read whole goes in part by part, so one wrong entry inside it costs only that
/// entry. A part that does not read keeps what `doc` held and joins `unreadable`.
fn take(doc: &mut Value, path: &[String], value: &Value, unreadable: &mut Vec<Unreadable>) {
    let before = lookup(doc, path).cloned();
    put(doc, path, value.clone());
    let Err(reason) = reads(doc) else { return };
    match value {
        Value::Object(fields) => {
            let base = before.clone().filter(Value::is_object).unwrap_or_else(|| Value::Object(Default::default()));
            put(doc, path, base);
            if reads(doc).is_ok() {
                for (key, field) in fields {
                    take(doc, &[path, std::slice::from_ref(key)].concat(), field, unreadable);
                }
                return;
            }
        }
        Value::Array(items) => {
            put(doc, path, Value::Array(Vec::new()));
            if reads(doc).is_ok() {
                for (index, item) in items.iter().enumerate() {
                    let mut list = lookup(doc, path).cloned().unwrap_or_default();
                    let kept = list.clone();
                    if let Value::Array(list) = &mut list {
                        list.push(item.clone());
                    }
                    put(doc, path, list);
                    if let Err(reason) = reads(doc) {
                        put(doc, path, kept);
                        unreadable.push(Unreadable { entry: format!("{}[{index}]", entry_name(path)), reason, keep: path.to_vec() });
                    }
                }
                return;
            }
        }
        _ => {}
    }
    match before {
        Some(before) => put(doc, path, before),
        None => remove(doc, path),
    }
    unreadable.push(Unreadable { entry: entry_name(path), reason, keep: path.to_vec() });
}

/// The settings as JSON values. The way through the text keeps a number such as 0.7 as the file
/// shows it: a direct conversion turns the f32 into 0.699999988079071.
fn as_value(settings: &Settings) -> Result<Value, String> {
    serde_json::to_string(settings).and_then(|text| serde_json::from_str(&text)).map_err(|e| e.to_string())
}

fn reads(doc: &Value) -> Result<(), String> {
    serde_json::from_value::<Settings>(doc.clone()).map(drop).map_err(|e| e.to_string())
}

fn lookup<'a>(doc: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(doc, |value, key| value.get(key))
}

/// Sets the entry at `path`, with the objects on the way to it
fn put(doc: &mut Value, path: &[String], value: Value) {
    let Some((last, parents)) = path.split_last() else { return };
    let mut parent = doc;
    for key in parents {
        let Value::Object(map) = parent else { return };
        parent = map.entry(key.clone()).or_insert_with(|| Value::Object(Default::default()));
    }
    if let Value::Object(map) = parent {
        map.insert(last.clone(), value);
    }
}

fn remove(doc: &mut Value, path: &[String]) {
    let Some((last, parents)) = path.split_last() else { return };
    if let Some(Value::Object(map)) = parents.iter().try_fold(doc, |value, key| value.get_mut(key)) {
        map.remove(last);
    }
}

/// The entry as a user finds it in the file: `controls.right.grip`, or `launch["C:\game.exe"].args`
/// for a name that is no plain word
fn entry_name(path: &[String]) -> String {
    let mut name = String::new();
    for key in path {
        if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            if !name.is_empty() {
                name.push('.');
            }
            name.push_str(key);
        } else {
            name.push_str(&format!("[{key:?}]"));
        }
    }
    name
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Headset {
    pub eye: (u32, u32),
    pub fps: f64,
    /// The level of a foveated stream; `None` when the stream has every pixel
    pub foveation: Option<FoveationLevel>,
    /// Which way the stream travels, once the host says
    pub path: Option<StreamPath>,
}

/// The way that the stream of a connected headset travels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamPath {
    Usb,
    WiFi,
}

impl StreamPath {
    /// From the `cable` of the host's `headset` line: the headset says whether its link runs over
    /// the USB cable, through `adb reverse` or its USB network.
    fn of(cable: bool) -> Self {
        if cable {
            StreamPath::Usb
        } else {
            StreamPath::WiFi
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Stats {
    pub fps: f64,
    pub game_frames: u64,
    pub render_ms: f64,
    pub encode_ms: f64,
    /// The eye stream's bitrate now: the host lowers it while the link misses frames and raises it
    /// again up to the user's setting
    pub mbps: u64,
    /// The bitrate that the user set
    pub mbps_set: u64,
    /// What the eye stream sent
    pub sent_mbps: f64,
    /// From the pose to the display, as the headset reports it
    pub latency_ms: Option<f64>,
}

/// What the host's `native` status line says (see host/src/status.rs).
#[derive(Clone, Debug, PartialEq)]
pub struct Native {
    pub port: u16,
    /// The names of the paired headsets
    pub paired: Vec<String>,
    /// The headsets that wait for the user's answer. The host lists one at a time.
    pub pending: Vec<PairRequest>,
}

/// A headset that waits for the user to allow it.
#[derive(Clone, Debug, PartialEq)]
pub struct PairRequest {
    /// The headset's key in hex. The answer names it.
    pub key: String,
    pub name: String,
    /// The headset's address on the network
    pub ip: String,
    /// The code that the headset shows too, such as "482 913"
    pub code: String,
}

/// What the window shows.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Model {
    pub running: bool,
    pub headset: Option<Headset>,
    pub game: Option<String>,
    /// The program of the game that is connected, as the runtime or cranvr's SteamVR driver names it
    pub game_program: Option<String>,
    pub desktop: Option<(bool, String)>,
    /// Whether the host captures this Mac's sound for the headset, and why not
    pub audio: Option<(bool, String)>,
    /// The ID of the virtual display that the desktop view shows
    pub vr_display: Option<u32>,
    /// The virtual display is the main display now: a headset streams
    pub vr_main: bool,
    pub stats: Option<Stats>,
    /// The last stats line of the host that showed a live stream: frames went out, or the
    /// headset's app paused the stream. Before the first one, the headset's connection. The host
    /// sends a stats line every 5 s while frames go out or the stream pauses. A line that says that
    /// the frames stopped does not count.
    pub stats_at: Option<Instant>,
    /// The host's state in native mode, once it has said it
    pub native: Option<Native>,
    /// This Mac's addresses on the local network: the headset connects to one of them
    pub lan: Vec<Ipv4Addr>,
    /// The USB cable: adb, the headsets on it and the download of adb (see usb.rs)
    pub usb: crate::usb::Status,
    pub log: VecDeque<String>,
    pub message: Option<String>,
    /// What cranvr cannot read in `app.json`. It has its own place: the host's messages replace
    /// `message`.
    pub settings_problem: Option<String>,
    /// Screen Recording and Accessibility
    pub permissions: [bool; 3],
    /// The Mac's input devices, for the microphone's choice
    pub input_devices: Vec<crate::microphone::InputDevice>,
    pub steam: crate::steam::SteamState,
    /// Whether Rosetta 2 runs Intel code, which Wine needs
    pub rosetta: bool,
    pub steam_running: bool,
    pub steam_signed_in: bool,
    pub games: Vec<crate::steam::Game>,
    /// The game setup that runs now or ran last: the steps with their progress
    pub setup: Option<crate::setup::Setup>,
    /// The version of the game support in the Wine folder, when it has a stamp
    pub game_support: Option<String>,
    /// The game support is older than this build of the app expects
    pub game_support_outdated: bool,
    /// Problems on the network path that the host found, newest last
    pub network: Vec<String>,
    /// Requests from the console in VR that the window has not run yet
    pub vr_requests: Vec<Value>,
    /// Reports of a black picture that the window has not answered yet (see black_eye.rs)
    pub black_eyes: Vec<Value>,
    /// What the window and the console in VR say about the last black picture, until it expires
    pub notice: Option<(crate::black_eye::Notice, Instant)>,
    /// The program whose launch options are open in the lists of games
    pub open_options: Option<String>,
    /// The request of a button that runs now, such as a game's start (see pending.rs)
    pub pending: Option<crate::pending::Pending>,
    /// The render sizes that the host learned for games (see [`learned_game_sizes`])
    pub learned_sizes: Vec<(String, f32)>,
    /// A setup ran at the last refresh, so its end shows at once
    pub setup_was_running: bool,
    /// The connected headset's own page: its panels, its displays and its actions (see
    /// `HostMsg::HeadsetRequest`)
    pub headset_page: Option<cranvr_common::menu::Page>,
    /// The Windows programs that the Mac found and that no game starts (see `steam::found_programs`)
    pub found_programs: Vec<PathBuf>,
    /// The pages carry the newest lines of the log until then: the headset asked to see them
    pub log_until: Option<Instant>,
    /// The file of the last recording and when it was saved: the window shows it with Show in Finder
    pub saved: Option<(PathBuf, Instant)>,
    /// The recording of what the headset shows (see recorder.rs), and how long the running one runs
    pub recording: crate::recorder::View,
    pub recording_seconds: u64,
}

impl Model {
    /// Takes a message of the window. Each one tells of a request that failed, so the console in VR
    /// shows it too for a while: a start that the headset asked for ends there as well.
    pub fn note_message(&mut self, text: String) {
        let notice = crate::black_eye::Notice { window: String::new(), headset: text.clone() };
        self.notice = Some((notice, Instant::now() + crate::black_eye::NOTICE_TIME));
        self.message = Some(text);
    }
}

const LOG_LINES: usize = 200;

/// The console's line in VR for a game that keeps the eye size of its start at a new link
const GAME_EYE_KEPT: &str = "The game keeps its start size. Start it again for the new size.";

/// The render sizes that the host learned for games: each program's file name and its share
/// of the size set in the app (game-sizes.txt, written by the host).
pub fn learned_game_sizes() -> Vec<(String, f32)> {
    let text = fs::read_to_string(config_dir().join("game-sizes.txt")).unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let (share, program) = line.split_once(' ')?;
            let name = program.rsplit('\\').next().unwrap_or(program);
            Some((name.to_string(), share.parse().ok()?))
        })
        .collect()
}

/// Forgets the learned sizes: every game starts at the size set in the app again.
pub fn forget_game_sizes() {
    for name in ["game-sizes.txt", "game-rates.txt"] {
        fs::remove_file(config_dir().join(name)).ok();
    }
}

impl Model {
    /// A headset streams with no game. The host sent no stats line of a live stream for `STALL`:
    /// its frame loop is stuck, or its frames stopped. A game may hold the picture for a long time
    /// while it loads. A connected game therefore leaves the host alone.
    pub fn stream_stalled(&self, now: Instant) -> bool {
        self.headset.is_some() && self.game.is_none() && self.stats_at.is_some_and(|at| now.duration_since(at) > STALL)
    }

    pub fn log(&mut self, line: String) {
        if self.log.len() == LOG_LINES {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }

    /// Takes a status line of the host, for a test of another module.
    #[cfg(test)]
    pub fn apply_status_for_test(&mut self, v: &Value) {
        self.apply_status(v);
    }

    fn apply_status(&mut self, v: &Value) {
        match v["type"].as_str() {
            Some("headset") => {
                self.headset = v["connected"].as_bool().unwrap_or(false).then(|| Headset {
                    eye: (v["eye"][0].as_u64().unwrap_or(0) as u32, v["eye"][1].as_u64().unwrap_or(0) as u32),
                    fps: v["fps"].as_f64().unwrap_or(0.0),
                    foveation: v["foveation"].as_str().and_then(FoveationLevel::parse).filter(|level| *level != FoveationLevel::Off),
                    path: v["cable"].as_bool().map(StreamPath::of),
                });
                self.stats_at = self.headset.is_some().then(Instant::now);
                if self.headset.is_none() {
                    self.stats = None;
                    self.vr_main = false;
                    self.headset_page = None;
                }
            }
            Some("game") => {
                let connected = v["connected"].as_bool().unwrap_or(false);
                self.game = connected.then(|| v["name"].as_str().unwrap_or("game").to_string());
                self.game_program = v["program"].as_str().filter(|_| connected).map(str::to_string);
            }
            Some("desktop") => {
                self.desktop = Some((v["ok"].as_bool().unwrap_or(false), v["message"].as_str().unwrap_or("").to_string()));
                self.vr_display = v["virtual"].as_bool().unwrap_or(false).then(|| v["display"].as_u64()).flatten().map(|id| id as u32);
            }
            Some("vr_display") => {
                self.vr_main = v["main"].as_bool().unwrap_or(false);
            }
            Some("virtual_display") => {
                self.desktop = Some((false, format!("no virtual display: {}", v["message"].as_str().unwrap_or(""))));
            }
            Some("audio") => {
                self.audio = Some((v["ok"].as_bool().unwrap_or(false), v["message"].as_str().unwrap_or("").to_string()));
            }
            Some("network") => {
                let message = v["message"].as_str().unwrap_or("A network problem").to_string();
                if !self.network.contains(&message) {
                    self.network.push(message);
                }
            }
            Some("native") => {
                let text = |request: &Value, field: &str| request[field].as_str().map(str::to_string);
                self.native = Some(Native {
                    port: v["port"].as_u64().and_then(|p| u16::try_from(p).ok()).unwrap_or(NATIVE_PORT),
                    paired: v["paired"].as_array().into_iter().flatten().filter_map(|n| n.as_str().map(str::to_string)).collect(),
                    pending: v["pending"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|r| {
                            Some(PairRequest { key: text(r, "key")?, name: text(r, "name")?, ip: text(r, "ip").unwrap_or_default(), code: text(r, "code")? })
                        })
                        .collect(),
                });
            }
            Some("vr_request") => self.vr_requests.push(v.clone()),
            Some("headset_page") => self.headset_page = serde_json::from_value(v["page"].clone()).ok(),
            Some("black_eye") => self.black_eyes.push(v.clone()),
            // A headset linked with another eye than the one that the running game read at its start (#170)
            Some("game_eye") => {
                let window = v["message"].as_str().unwrap_or("The running game keeps the eye size of its start. Start the game again for the new size.");
                let notice = crate::black_eye::Notice { window: window.to_string(), headset: GAME_EYE_KEPT.to_string() };
                self.notice = Some((notice, Instant::now() + crate::black_eye::NOTICE_TIME));
            }
            Some("stats") => {
                let live = v["fps"].as_f64().unwrap_or(0.0) > 0.0 || v["paused"].as_bool().unwrap_or(false);
                if live {
                    self.stats_at = Some(Instant::now());
                }
                self.stats = Some(Stats {
                    fps: v["fps"].as_f64().unwrap_or(0.0),
                    game_frames: v["game_frames"].as_u64().unwrap_or(0),
                    render_ms: v["render_ms"].as_f64().unwrap_or(0.0),
                    encode_ms: v["encode_ms"].as_f64().unwrap_or(0.0),
                    mbps: v["mbps"].as_u64().unwrap_or(0),
                    mbps_set: v["mbps_set"].as_u64().unwrap_or(0),
                    sent_mbps: v["sent_mbps"].as_f64().unwrap_or(0.0),
                    latency_ms: v["latency"]["total_ms"].as_f64(),
                });
            }
            _ => {}
        }
    }
}

enum HostLine {
    Status(Value),
    Log(String),
    Exited,
    Message(String),
    /// A recording saved this file
    Saved(PathBuf),
}

/// Shows a message in the window, from any thread.
pub fn notify(text: String) {
    channel().0.send(HostLine::Message(text)).ok();
}

fn channel() -> &'static (Sender<HostLine>, Mutex<Receiver<HostLine>>) {
    static CHANNEL: OnceLock<(Sender<HostLine>, Mutex<Receiver<HostLine>>)> = OnceLock::new();
    CHANNEL.get_or_init(|| {
        let (s, r) = mpsc::channel();
        (s, Mutex::new(r))
    })
}

/// A program that the user chose in a file chooser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chosen {
    /// "Add a game from a folder"
    Add(PathBuf),
    /// "Choose a program": start it once
    Run(PathBuf),
}

/// The choices that the window has not used yet. The chooser runs on a thread of its own (see
/// `ui::choose_program`), and the window takes its choice on the next refresh.
static CHOSEN: Mutex<Vec<Chosen>> = Mutex::new(Vec::new());

pub fn choose(chosen: Chosen) {
    CHOSEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(chosen);
}

pub fn take_chosen() -> Vec<Chosen> {
    std::mem::take(&mut *CHOSEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
}

static HOST: Mutex<Option<Child>> = Mutex::new(None);
/// The process ID of the last host whose output ended: all its lines are in the channel then
static LAST_EXITED: AtomicU32 = AtomicU32::new(0);
static WATCH: Mutex<Watch> = Mutex::new(Watch { started: None, quick_ends: 0, ended_at: None });

/// A host that sends no stats line of a live stream this long while a headset is connected is
/// stuck (`Model::stream_stalled`)
const STALL: Duration = Duration::from_secs(15);
/// A host that ran this long ended for a reason of its own time, not of its start
const QUICK_END: Duration = Duration::from_secs(60);
/// The longest wait before the watch starts a host again. The watch never gives up: a host that
/// cannot start now (another program holds its port) may start later.
const MAX_DELAY: Duration = Duration::from_secs(30);

/// The app's watch over the host that it started (issue #148). A host that ends by itself, for
/// example when macOS ignores its GPU work, starts again at once. A host whose frame loop stopped
/// starts again too. Hosts that end soon after their start, again and again, wait longer each time
/// (see `Watch::delay`).
struct Watch {
    /// The start of the current host, by the user or by the watch
    started: Option<Instant>,
    /// The hosts in a row that ended within `QUICK_END` of their start
    quick_ends: u32,
    /// When the watch saw the current host's end
    ended_at: Option<Instant>,
}

impl Watch {
    /// How long a new start waits after the end of a host that ran `lived`. A host that ran long
    /// starts again at once. The first quick end starts again at once too. Each further quick end
    /// in a row waits twice as long as the one before, from 2 s to `MAX_DELAY`.
    fn delay(&self, lived: Duration) -> Duration {
        if lived >= QUICK_END {
            return Duration::ZERO;
        }
        match self.quick_ends + 1 {
            1 => Duration::ZERO,
            ends => Duration::from_secs(1 << (ends - 1).min(5)).min(MAX_DELAY),
        }
    }

    /// Counts the end of a host that ran `lived`.
    fn count(&mut self, lived: Duration) {
        self.quick_ends = if lived >= QUICK_END { 0 } else { self.quick_ends + 1 };
    }
}

/// The app wants a host: one runs, or one ended by itself and the watch starts it again. False
/// after `stop_host` and after a start that failed.
pub fn host_wanted() -> bool {
    HOST.lock().unwrap().is_some()
}

/// Starts the host again when it ended by itself or its stream stalled (see [`Watch`]). Returns
/// what the watch did, for the log.
pub fn watch_host(settings: &Settings, model: &Model) -> Option<String> {
    let now = Instant::now();
    let mut watch = WATCH.lock().unwrap();
    let reason = {
        let mut host = HOST.lock().unwrap();
        let child = host.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => {
                let ended_at = *watch.ended_at.get_or_insert(now);
                // The old host's last lines come first. They then do not mix with the new host's.
                // A host whose output stays open (a helper that took it) gets 2 s.
                if LAST_EXITED.load(Ordering::Relaxed) != child.id() && now.duration_since(ended_at) < Duration::from_secs(2) {
                    return None;
                }
                format!("the host ended by itself ({status})")
            }
            _ if model.stream_stalled(now) => format!("the host sent no frames for {} s while the headset was connected", STALL.as_secs()),
            _ => return None,
        }
    };
    let ended_at = *watch.ended_at.get_or_insert(now);
    let lived = watch.started.map_or(Duration::MAX, |started| ended_at.duration_since(started));
    if now.duration_since(ended_at) < watch.delay(lived) {
        return None;
    }
    watch.count(lived);
    drop(watch);
    Some(match start_host(settings) {
        Ok(()) => {
            let line = format!("cranvr: {reason}; the app started it again");
            HostLog::note(&line);
            line
        }
        Err(e) => format!("cranvr: {reason}; the app could not start it again: {e}"),
    })
}

/// The host's input. The user's answers to a headset's request go there.
static HOST_INPUT: Mutex<Option<ChildStdin>> = Mutex::new(None);

/// `cranvr-host` next to this executable, or at $CRANVR_HOST_BIN.
pub(crate) fn host_binary() -> Result<PathBuf, String> {
    let host = match env::var_os("CRANVR_HOST_BIN") {
        Some(p) => PathBuf::from(p),
        None => env::current_exe().map_err(|e| e.to_string())?.with_file_name("cranvr-host"),
    };
    host.exists().then_some(host.clone()).ok_or(format!("{} is missing; build it with cargo build --release", host.display()))
}

/// The line that tells the host the user's answer to a headset's request (see host/src/headset/native.rs).
fn answer_line(key: &str, allow: bool) -> String {
    format!("{} {key}\n", if allow { "allow" } else { "deny" })
}

/// Tells the host that the user allowed or denied the headset with this key.
pub fn answer_pairing(key: &str, allow: bool) {
    let mut input = HOST_INPUT.lock().unwrap();
    let sent = input.as_mut().map(|input| input.write_all(answer_line(key, allow).as_bytes()).and_then(|()| input.flush()));
    if !matches!(sent, Some(Ok(()))) {
        notify("The host did not take the answer. Start the host and let the headset ask again.".into());
    }
}

/// The line that tells the host the user's Connection choice: off for Wi-Fi, so the host takes no
/// link over the USB cable (see host/src/headset/native.rs).
fn cable_line(connection: Connection) -> &'static str {
    if connection.allows_usb() {
        "cable on\n"
    } else {
        "cable off\n"
    }
}

/// Tells the running host the user's Connection choice. It counts from the headset's next
/// connection. A host that starts later gets the choice on its command line.
pub fn tell_host_connection(connection: Connection) {
    let mut input = HOST_INPUT.lock().unwrap();
    let sent = input.as_mut().map(|input| input.write_all(cable_line(connection).as_bytes()).and_then(|()| input.flush()));
    if matches!(sent, Some(Err(_))) {
        notify("The host did not take the Connection choice. It counts from the next start of the host.".into());
    }
}

/// The line that asks the host to open the hand menu of the connected headset (see
/// host/src/headset/native.rs).
const OPEN_MENU_LINE: &str = "menu open\n";

/// Hands a press of the window on the headset's own page to the host, which sends it to the headset
/// (`HostMsg::HeadsetRequest`).
pub fn send_headset_request(request: &cranvr_common::menu::Request) {
    let mut input = HOST_INPUT.lock().unwrap();
    let line = format!("headset {}\n", request.to_json());
    let sent = input.as_mut().map(|input| input.write_all(line.as_bytes()).and_then(|()| input.flush()));
    if !matches!(sent, Some(Ok(()))) {
        notify("The host did not take the request. Start the host and connect the headset.".into());
    }
}

/// Tells the running host the state of the recording of what the headset shows, which it passes to
/// the headset (`HostMsg::Recording`, see recorder.rs).
pub fn tell_host_recording(state: &Value) {
    let mut input = HOST_INPUT.lock().unwrap();
    if let Some(input) = input.as_mut() {
        input.write_all(format!("recording {state}\n").as_bytes()).and_then(|()| input.flush()).ok();
    }
}

/// Tells the window that a recording saved its file, from any thread.
pub fn notify_saved(path: &Path) {
    channel().0.send(HostLine::Saved(path.to_path_buf())).ok();
}

/// Asks the connected headset to open its hand menu, for a user whose controllers do not reach it.
pub fn open_headset_menu() {
    let mut input = HOST_INPUT.lock().unwrap();
    let sent = input.as_mut().map(|input| input.write_all(OPEN_MENU_LINE.as_bytes()).and_then(|()| input.flush()));
    if !matches!(sent, Some(Ok(()))) {
        notify("The host did not take the request. Start the host and connect the headset.".into());
    }
}

/// The command line of `cranvr-host`. `test` is for tests on one Mac: the probe needs no pairing and
/// TCP media.
fn host_args(settings: &Settings, config_dir: &Path, test: bool) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![];
    args.extend(["--status", "--fps", &settings.fps.to_string(), "--mbps", &settings.mbps.to_string()].map(OsString::from));
    if native_port() != NATIVE_PORT {
        args.extend(["--native-port".into(), native_port().to_string().into()]);
    }
    args.extend(["--config-dir".into(), config_dir.as_os_str().to_owned()]);
    if test {
        args.extend(["--trust-all", "--tcp"].map(OsString::from));
    }
    if !settings.connection.allows_usb() {
        args.push("--no-cable".into());
    }
    args.extend(["--foveation", settings.foveation_level.name()].map(OsString::from));
    if !settings.game_audio {
        args.push("--no-game-audio".into());
    }
    args.extend(["--game-scale", &settings.game_scale.to_string()].map(OsString::from));
    if !settings.unscale_displays {
        args.push("--keep-display-modes".into());
    }
    if !settings.hand_controllers {
        args.push("--no-hand-controllers".into());
    }
    if settings.desktop {
        args.extend(["--desktop", "--desktop-fps", &settings.desktop_fps.to_string()].map(OsString::from));
        if let Some((width, height)) = settings.virtual_display {
            args.extend(["--virtual-display", &format!("{width}x{height}")].map(OsString::from));
            if settings.vr_main_display {
                args.push("--vr-main-display".into());
            }
        }
    }
    args
}

/// Starts the host with these settings, after stopping a running one.
pub fn start_host(settings: &Settings) -> Result<(), String> {
    stop_host();
    let mut command = Command::new(host_binary()?);
    command.args(host_args(settings, &config_dir(), env::var_os("CRANVR_TEST_TRUST_ALL").is_some()));
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let pid = child.id();
    {
        let mut watch = WATCH.lock().unwrap();
        watch.started = Some(Instant::now());
        watch.ended_at = None;
    }
    *HOST_INPUT.lock().unwrap() = child.stdin.take();
    // A new host knows nothing of a recording that runs: the gem shows the state again
    crate::recorder::announce();
    let sender = channel().0.clone();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let log = HostLog::create();
    *CURRENT_LOG.lock().unwrap() = Some(log.clone());
    {
        let sender = sender.clone();
        let log = log.clone();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                log.write(&line);
                let msg = match line.strip_prefix("@cranvr ").and_then(|j| serde_json::from_str(j).ok()) {
                    Some(v) => HostLine::Status(v),
                    None => HostLine::Log(line),
                };
                sender.send(msg).ok();
            }
            sender.send(HostLine::Exited).ok();
            LAST_EXITED.store(pid, Ordering::Relaxed);
        });
    }
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            log.write(&line);
            sender.send(HostLine::Log(line)).ok();
        }
    });
    *HOST.lock().unwrap() = Some(child);
    Ok(())
}

/// Every line of the host's output, in `host.log` in the config folder, for debugging: the window
/// keeps only the last 200. Each line starts with the seconds since the host started. A new host
/// start empties the file.
#[derive(Clone)]
struct HostLog {
    file: Option<Arc<Mutex<fs::File>>>,
    started: Instant,
}

/// The log of the host that runs now
static CURRENT_LOG: Mutex<Option<HostLog>> = Mutex::new(None);

impl HostLog {
    /// A new, empty `host.log`. The last host's log stays as `host-previous.log`: the watch starts a
    /// new host after an end. The old log says why it ended.
    fn create() -> Self {
        let path = config_dir().join("host.log");
        fs::rename(&path, config_dir().join("host-previous.log")).ok();
        let file = fs::File::create(path).ok().map(|f| Arc::new(Mutex::new(f)));
        Self { file, started: Instant::now() }
    }

    /// Writes a line of the app into the log of the host that runs now.
    fn note(line: &str) {
        if let Some(log) = CURRENT_LOG.lock().unwrap().as_ref() {
            log.write(line);
        }
    }

    fn write(&self, line: &str) {
        if let Some(file) = &self.file {
            writeln!(file.lock().unwrap(), "{:.3} {line}", self.started.elapsed().as_secs_f64()).ok();
        }
    }
}

/// Writes vr-menu.json for the console in VR: the pages (see pages.rs), the controls and a short
/// notice while there is one. The host reads it, adds the stream's state and sends it to the headset.
/// Writes only on a change, so the host sends nothing new while nothing changes.
pub fn write_vr_menu(settings: &Settings, model: &Model) {
    static WRITTEN: Mutex<String> = Mutex::new(String::new());
    let menu = vr_menu(settings, model);
    let mut written = WRITTEN.lock().unwrap();
    if *written != menu && fs::write(config_dir().join("vr-menu.json"), &menu).is_ok() {
        *written = menu;
    }
}

/// The text of vr-menu.json. The headset shows no hints, so the pages go without them: the host
/// sends the menu again at each change of the stream's numbers.
pub fn vr_menu(settings: &Settings, model: &Model) -> String {
    let notice = model.notice.as_ref().map(|(notice, _)| notice.headset.as_str()).filter(|notice| !notice.is_empty());
    let mut pages = crate::pages::pages(settings, model);
    for page in &mut pages {
        page.items.iter_mut().for_each(cranvr_common::menu::Item::drop_hint);
    }
    let mut menu = serde_json::json!({
        // The headset applies these two itself, and it tells its own choice while they are null
        "headset": {"room": settings.headset_room, "volume": settings.headset_volume, "interface": settings.headset_interface, "theme": settings.headset_theme},
        "pages": pages,
        "controls": settings.controls,
    });
    if let Some(notice) = notice {
        menu["notice"] = notice.into();
    }
    menu.to_string()
}

pub fn stop_host() {
    HOST_INPUT.lock().unwrap().take();
    if let Some(mut child) = HOST.lock().unwrap().take() {
        // A host that ended by itself needs no signal
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        // SIGTERM first. The host has no handler, so it ends at once. A host that is still there after 1.5 s is killed.
        Command::new("kill").args(["-TERM", &child.id().to_string()]).status().ok();
        for _ in 0..30 {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(std::time::Duration::from_millis(50));
        }
        child.kill().ok();
        child.wait().ok();
    }
}

pub fn host_running() -> bool {
    HOST.lock().unwrap().as_mut().is_some_and(|c| c.try_wait().ok().flatten().is_none())
}

/// Moves what the host printed since the last call into the model.
pub fn pump(model: &mut Model) {
    let receiver = channel().1.lock().unwrap();
    for line in receiver.try_iter() {
        match line {
            HostLine::Status(v) => {
                // The game's run watches the game's sessions and frames (game_run.rs)
                crate::game_run::note_status(&v);
                model.apply_status(&v);
            }
            HostLine::Log(l) => model.log(l),
            HostLine::Exited => {
                model.headset = None;
                model.game = None;
                model.game_program = None;
                model.stats = None;
                // The line of an old host can arrive after the first line of its successor
                if !host_running() {
                    model.native = None;
                }
            }
            HostLine::Message(text) => model.note_message(text),
            HostLine::Saved(path) => {
                // The console in VR says it for a few seconds too
                let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                let notice = crate::black_eye::Notice { window: String::new(), headset: format!("Saved: {name}") };
                model.notice = Some((notice, Instant::now() + crate::recorder::SAVED_NOTICE));
                model.saved = Some((path, Instant::now()));
            }
        }
    }
    model.running = host_running();
    model.setup = crate::setup::current();
}

/// The setup scripts: in the app bundle's Resources, or in the source tree.
pub fn scripts_dir() -> PathBuf {
    let bundled = env::current_exe().ok().and_then(|e| Some(e.parent()?.parent()?.join("Resources/scripts")));
    match bundled {
        Some(b) if b.is_dir() => b,
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts"),
    }
}

/// Adds a line to the Log tab, from any thread.
pub fn log(line: String) {
    channel().0.send(HostLine::Log(line)).ok();
}

/// Runs a setup script and waits for it. Its output goes to the Log tab. `progress` sees each
/// line, and each update of curl's progress bar, as they come. The error holds the script's last
/// complaint on stderr, or the exit code when it said nothing.
pub fn run_script(script: &str, args: &[&str], mut progress: impl FnMut(&str)) -> Result<(), String> {
    let path = scripts_dir().join(script);
    let mut child = Command::new("/bin/zsh")
        .arg(&path)
        .args(args)
        .env("CRANVR_HOME", config_dir())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let (sender, receiver) = mpsc::channel::<(bool, String)>();
    for (is_stderr, stream) in [(false, Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>), (true, Box::new(child.stderr.take().unwrap()))] {
        let sender = sender.clone();
        thread::spawn(move || split_lines(stream, |line| drop(sender.send((is_stderr, line)))));
    }
    drop(sender);
    let mut complaint = None;
    for (is_stderr, line) in receiver {
        progress(&line);
        // A progress bar redraws one line hundreds of times: the log keeps the lines that say something
        if percent(&line).is_none() {
            log(line.clone());
            if is_stderr {
                complaint = Some(line);
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() {
        return Ok(());
    }
    Err(complaint.unwrap_or_else(|| {
        log(format!("{script} stopped with {status}"));
        "The step stopped without saying why".into()
    }))
}

/// Calls `line` for each text that ends in a line feed or a carriage return: curl's progress bar
/// redraws with carriage returns only. Empty texts are left out.
fn split_lines(mut stream: impl Read, mut line: impl FnMut(String)) {
    let (mut pending, mut buffer) = (Vec::new(), [0u8; 4096]);
    while let Ok(n @ 1..) = stream.read(&mut buffer) {
        for &byte in &buffer[..n] {
            if byte == b'\n' || byte == b'\r' {
                let text = String::from_utf8_lossy(&pending).trim().to_owned();
                if !text.is_empty() {
                    line(text);
                }
                pending.clear();
            } else {
                pending.push(byte);
            }
        }
    }
    let text = String::from_utf8_lossy(&pending).trim().to_owned();
    if !text.is_empty() {
        line(text);
    }
}

/// The percentage that curl's progress bar shows ("######   37.5%"), whole.
pub fn percent(line: &str) -> Option<u32> {
    let text = line.trim_end().strip_suffix('%')?;
    let (bar, number) = text.rsplit_once(|c: char| c.is_whitespace() || c == '#').unwrap_or(("", text));
    if !bar.chars().all(|c| matches!(c, '#' | ' ' | '=' | '-' | 'O' | '\t')) {
        return None;
    }
    number.parse::<f32>().ok().map(|n| n as u32)
}

/// The Steam app ID of a game under `steamapps/common/<installdir>`, from Steam's
/// `appmanifest_<id>.acf` files. A Steam game started by its .exe needs `SteamAppId`.
pub fn steam_app_id(dir: &Path) -> Option<String> {
    let parts: Vec<_> = dir.components().collect();
    let common = parts.windows(2).position(|w| w[0].as_os_str() == "steamapps" && w[1].as_os_str() == "common")?;
    let installdir = parts.get(common + 2)?.as_os_str().to_str()?;
    let steamapps: PathBuf = parts[..=common].iter().collect();
    for entry in fs::read_dir(&steamapps).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(id) = name.strip_prefix("appmanifest_").and_then(|n| n.strip_suffix(".acf")) else { continue };
        let text = fs::read_to_string(entry.path()).unwrap_or_default();
        let matches = text.lines().any(|l| {
            let mut fields = l.split('"').filter(|f| !f.trim().is_empty());
            fields.next() == Some("installdir") && fields.next() == Some(installdir)
        });
        if matches {
            return Some(id.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranvr_common::controls::{Action, Control, Hand};

    /// A file with changed settings and one wrong action name, as a hand edit leaves it
    const WRONG_ACTION: &str = r#"{
        "fps": 72,
        "mbps": 60,
        "game_scale": 0.5,
        "controls": {
            "left": {"trigger": "right_click", "menu": "recenter"},
            "right": {"grip": "grab", "secondary": "keyboard"}
        },
        "launch": {"C:\\Games\\game.exe": {"args": "-vr", "openvr": "off"}},
        "wine": {"mac_window": true},
        "added_games": [{"name": "Mods", "program": "/Games/Mods/game.exe"}]
    }"#;

    #[test]
    fn a_failed_request_shows_in_the_window_once_and_in_the_console_in_vr() {
        let mut model = Model::default();
        model.note_message("The game did not start: wine exited with 1".into());
        assert_eq!(model.message.as_deref(), Some("The game did not start: wine exited with 1"));
        let (notice, until) = model.notice.expect("the console shows it");
        assert_eq!((notice.window.as_str(), notice.headset.as_str()), ("", "The game did not start: wine exited with 1"));
        assert!(until > Instant::now());
    }

    #[test]
    fn a_wrong_action_name_costs_only_its_entry() {
        let reading = Settings::read(WRONG_ACTION);
        let s = &reading.settings;
        assert_eq!((s.fps, s.mbps, s.game_scale), (72, 60, 0.5));
        assert_eq!(s.controls.action(Hand::Left, Control::Trigger), Action::RightClick);
        assert_eq!(s.controls.action(Hand::Left, Control::Menu), Action::Recenter);
        assert_eq!(s.controls.action(Hand::Right, Control::Secondary), Action::Keyboard);
        // The wrong entry keeps the standard action
        assert_eq!(s.controls.action(Hand::Right, Control::Grip), Action::Nothing);
        assert_eq!(s.launch["C:\\Games\\game.exe"].args, "-vr");
        assert_eq!(s.launch["C:\\Games\\game.exe"].openvr, Some(crate::launch::OpenVr::Off));
        assert!(s.wine.mac_window);
        assert_eq!(s.added_games.len(), 1);
        assert_eq!(reading.unreadable.len(), 1, "{:?}", reading.unreadable);
        assert_eq!(reading.unreadable[0].entry, "controls.right.grip");
        assert_eq!(reading.unreadable[0].reason, "no action is named grab");
        let problem = reading.problem(Path::new("/cfg/app.json")).unwrap();
        assert!(problem.contains("controls.right.grip (no action is named grab)") && problem.contains("/cfg/app.json"), "{problem}");
    }

    #[test]
    fn a_save_keeps_the_wrong_entry_until_the_user_changes_that_setting() {
        let mut settings = Settings::read(WRONG_ACTION).settings;
        settings.fps = 90;
        let text = settings.file_text(Some(WRONG_ACTION)).unwrap();
        assert!(text.contains("\"game_scale\": 0.5,"), "{text}");
        let saved: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(saved["controls"]["right"]["grip"], "grab");
        assert_eq!(saved["fps"], 90);
        assert_eq!(saved["controls"]["left"]["trigger"], "right_click");
        assert_eq!(saved["launch"]["C:\\Games\\game.exe"]["args"], "-vr");
        // The next read finds the same settings and the same wrong entry
        let again = Settings::read(&text);
        assert_eq!(again.settings, settings);
        assert_eq!(again.unreadable.len(), 1);

        // The user picks an action for the grip: the file takes it, and the error goes away
        settings.controls.set(Hand::Right, Control::Grip, Action::Console);
        let text = settings.file_text(Some(&text)).unwrap();
        let fixed = Settings::read(&text);
        assert!(fixed.unreadable.is_empty() && fixed.problem(Path::new("app.json")).is_none());
        assert_eq!(fixed.settings, settings);
    }

    #[test]
    fn wrong_parts_of_a_list_and_of_a_game_s_options_cost_only_themselves() {
        let text = r#"{
            "added_games": [{"name": "Good", "program": "/a.exe"}, {"name": 7}],
            "launch": {"C:\\a.exe": {"args": "-x", "eye_per_pass": "yes"}},
            "fps": "fast"
        }"#;
        let reading = Settings::read(text);
        let entries: Vec<&str> = reading.unreadable.iter().map(|u| u.entry.as_str()).collect();
        assert_eq!(entries, ["added_games[1]", "fps", r#"launch["C:\\a.exe"].eye_per_pass"#]);
        assert_eq!(reading.settings.added_games.len(), 1);
        assert_eq!(reading.settings.launch["C:\\a.exe"].args, "-x");
        assert_eq!(reading.settings.fps, Settings::default().fps);
        // A save keeps the list as the file has it while the list stays as read
        let saved: Value = serde_json::from_str(&reading.settings.file_text(Some(text)).unwrap()).unwrap();
        assert_eq!(saved["added_games"][1]["name"], 7);
        assert_eq!(saved["fps"], "fast");
        assert_eq!(saved["launch"]["C:\\a.exe"]["eye_per_pass"], "yes");
        assert!(reading.problem(Path::new("app.json")).unwrap().contains("3 entries"));
    }

    #[test]
    fn a_file_that_does_not_read_at_all_is_never_saved_over() {
        let dir = env::temp_dir().join(format!("cranvr-app-json-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.json");
        let broken = "{\"fps\": 72,";
        fs::write(&path, broken).unwrap();
        let reading = Settings::load_from(&path);
        assert!(reading.broken.is_some());
        assert_eq!(reading.settings, Settings::default());
        assert!(reading.problem(&path).unwrap().contains("saves no change"));
        assert!(reading.settings.save_to(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), broken);

        // A file with no wrong entry gets the same text as before, with its fields in their order
        let good = Settings { game_scale: 0.7, ..Default::default() };
        let text = good.file_text(Some(&serde_json::to_string(&good).unwrap())).unwrap();
        assert_eq!(text, serde_json::to_string_pretty(&good).unwrap());
        assert!(text.contains("\"game_scale\": 0.7,"), "{text}");

        // A readable file takes the save, through a new file that replaces it
        fs::write(&path, WRONG_ACTION).unwrap();
        let mut settings = Settings::load_from(&path).settings;
        settings.mbps = 80;
        settings.save_to(&path).unwrap();
        let saved = Settings::load_from(&path);
        assert_eq!(saved.settings.mbps, 80);
        assert_eq!(saved.unreadable.len(), 1);
        assert!(!path.with_extension("json.new").exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn each_network_problem_shows_once() {
        let mut model = Model::default();
        let udp = serde_json::json!({"type": "network", "problem": "no_poses", "protocol": "udp", "message": "no UDP"});
        model.apply_status(&udp);
        model.apply_status(&udp);
        model.apply_status(&serde_json::json!({"type": "network", "problem": "vpn_route", "message": "tunnel"}));
        assert_eq!(model.network, ["no UDP", "tunnel"]);
    }

    /// A host that ran long starts again at once. So does the first quick end. Each further quick end
    /// in a row waits twice as long, up to 30 s. The watch never gives up.
    #[test]
    fn the_watch_waits_longer_after_each_quick_end() {
        let mut watch = Watch { started: None, quick_ends: 0, ended_at: None };
        let long = Duration::from_secs(600);
        assert_eq!(watch.delay(long), Duration::ZERO);
        let quick = Duration::from_secs(3);
        let mut delays = Vec::new();
        for _ in 0..40 {
            delays.push(watch.delay(quick).as_secs());
            watch.count(quick);
        }
        assert_eq!(delays[..7], [0, 2, 4, 8, 16, 30, 30]);
        assert_eq!(delays[39], 30);
        // A host that runs long again counts from zero
        watch.count(long);
        assert_eq!(watch.delay(quick), Duration::ZERO);
    }

    /// The stream counts as stalled only while a headset is connected with no game and no stats
    /// line of a live stream came for 15 s. A line that says that the frames stopped is no sign of
    /// life.
    #[test]
    fn a_stream_without_stats_for_15_s_stalls() {
        let mut model = Model::default();
        let now = Instant::now();
        assert!(!model.stream_stalled(now + Duration::from_secs(60)), "no headset");
        model.apply_status(&serde_json::json!({"type": "headset", "connected": true, "eye": [1248, 1306], "fps": 120.0}));
        assert!(!model.stream_stalled(now + Duration::from_secs(10)));
        assert!(model.stream_stalled(now + Duration::from_secs(16)));
        model.apply_status(&serde_json::json!({"type": "stats", "fps": 119.9}));
        let live_at = model.stats_at.unwrap();
        model.apply_status(&serde_json::json!({"type": "stats", "fps": 0.0, "stopped": true}));
        assert_eq!(model.stats_at, Some(live_at), "frames that stopped are no sign of life");
        assert!(model.stream_stalled(live_at + Duration::from_secs(16)));
        model.apply_status(&serde_json::json!({"type": "stats", "fps": 0.0, "paused": true}));
        let stats_at = model.stats_at.unwrap();
        assert!(!model.stream_stalled(stats_at + Duration::from_secs(14)), "a paused stream sends stats too");
        model.game = Some("Beat Saber".into());
        assert!(!model.stream_stalled(stats_at + Duration::from_secs(60)), "a game may hold the picture while it loads");
        model.game = None;
        model.apply_status(&serde_json::json!({"type": "headset", "connected": false}));
        assert!(!model.stream_stalled(stats_at + Duration::from_secs(60)), "no headset");
    }

    fn host_options(settings: &Settings) -> Vec<String> {
        host_args(settings, Path::new("/cfg"), false).into_iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn the_host_gets_its_arguments() {
        let args = host_options(&Settings::default());
        assert_eq!(
            args,
            ["--status", "--fps", "120", "--mbps", "100", "--config-dir", "/cfg", "--foveation", "medium", "--game-scale", "0.7", "--desktop", "--desktop-fps", "30"]
        );
        // `--tcp` would force TCP media on every client: only a test asks for it
        assert!(!args.contains(&"--tcp".to_string()));
        let test = host_args(&Settings::default(), Path::new("/cfg"), true);
        assert!(test.iter().any(|a| a == "--tcp") && test.iter().any(|a| a == "--trust-all"));
    }

    #[test]
    fn the_settings_reach_the_host() {
        let quiet = Settings {
            foveation_level: FoveationLevel::Off,
            game_audio: false,
            hand_controllers: false,
            virtual_display: Some((1920, 1080)),
            connection: Connection::WiFi,
            ..Default::default()
        };
        let args = host_options(&quiet);
        for wanted in ["off", "--no-game-audio", "--no-hand-controllers", "--virtual-display", "1920x1080", "--no-cable"] {
            assert!(args.contains(&wanted.to_string()), "{wanted} in {args:?}");
        }
    }

    // The cable is on for Automatic and USB cable, off for Wi-Fi: on the command line and in a change
    #[test]
    fn the_connection_choice_reaches_the_host() {
        assert_eq!(cable_line(Connection::WiFi), "cable off\n");
        for cable in [Connection::Automatic, Connection::Usb] {
            assert_eq!(cable_line(cable), "cable on\n");
            assert!(!host_options(&Settings { connection: cable, ..Default::default() }).contains(&"--no-cable".to_string()));
        }
    }

    #[test]
    fn the_answer_to_a_request_names_the_headsets_key() {
        assert_eq!(answer_line("ab12", true), "allow ab12\n");
        assert_eq!(answer_line("ab12", false), "deny ab12\n");
    }

    #[test]
    fn the_hosts_native_line_shows_the_paired_headsets_and_the_one_that_waits() {
        let mut model = Model::default();
        model.apply_status(&serde_json::json!({"type": "native", "port": 9950, "paired": [], "pending": []}));
        let native = model.native.clone().unwrap();
        assert_eq!((native.port, native.paired.len(), native.pending.len()), (9950, 0, 0));
        model.apply_status(&serde_json::json!({
            "type": "native", "port": 9951, "paired": ["Meta Quest 3"],
            "pending": [{"key": "ab12", "name": "Meta Quest 3", "ip": "192.168.1.30", "code": "482 913"}, {"name": "no key"}]
        }));
        let native = model.native.unwrap();
        assert_eq!((native.port, native.paired), (9951, vec!["Meta Quest 3".to_string()]));
        // A request without a key or a code cannot be answered, so the window skips it
        assert_eq!(
            native.pending,
            [PairRequest { key: "ab12".into(), name: "Meta Quest 3".into(), ip: "192.168.1.30".into(), code: "482 913".into() }]
        );
    }

    #[test]
    fn a_program_chosen_on_the_chooser_thread_waits_for_the_window_in_order() {
        // The chooser runs on a thread of its own: the choices cross to the window through a queue
        take_chosen();
        std::thread::spawn(|| {
            choose(Chosen::Add(PathBuf::from("/games/a/game.exe")));
            choose(Chosen::Run(PathBuf::from("/games/b/other.exe")));
        })
        .join()
        .unwrap();
        assert_eq!(take_chosen(), [Chosen::Add(PathBuf::from("/games/a/game.exe")), Chosen::Run(PathBuf::from("/games/b/other.exe"))]);
        assert!(take_chosen().is_empty());
    }

    #[test]
    fn a_game_that_keeps_its_start_size_shows_in_the_window_and_in_the_console_in_vr() {
        let mut model = Model::default();
        let message = "The running game keeps the eye size of its start, 1176x1232. The stream's eye at the game render size is 874x914 now. Start the game again for the new size.";
        model.apply_status(&serde_json::json!({"type": "game_eye", "kept": [1176, 1232], "eye": [874, 914], "message": message}));
        let (notice, until) = model.notice.expect("a notice");
        assert_eq!(notice.window, message);
        assert_eq!(notice.headset, GAME_EYE_KEPT);
        assert!(until > Instant::now());
    }

    #[test]
    fn the_hosts_black_eye_line_waits_in_the_model_for_the_window_to_answer() {
        let mut model = Model::default();
        model.apply_status(&serde_json::json!({"type": "black_eye", "eye": "right", "program": "C:\\Games\\game.exe"}));
        assert_eq!(model.black_eyes.len(), 1);
        assert_eq!(model.black_eyes[0]["program"], "C:\\Games\\game.exe");
    }

    // The host's latency report has the `total_ms` field that the window reads.
    #[test]
    fn the_hosts_headset_and_stats_lines_fill_the_model() {
        let mut model = Model::default();
        model.apply_status(&serde_json::json!({"type": "headset", "connected": true, "eye": [2064, 2208], "fps": 120.0, "foveated": true, "foveation": "high"}));
        model.apply_status(&serde_json::json!({
            "type": "stats", "fps": 119.6, "game_frames": 0, "render_ms": 3.1, "encode_ms": 4.2, "mbps": 40, "mbps_set": 100,
            "sent_mbps": 28.2, "latency": {"frames": 590, "total_ms": 21.5, "rtt_ms": 2.0, "decode_ms": 3.0}
        }));
        let headset = model.headset.clone().unwrap();
        assert_eq!((headset.eye, headset.fps, headset.foveation), ((2064, 2208), 120.0, Some(FoveationLevel::High)));
        let stats = model.stats.clone().unwrap();
        assert_eq!((stats.fps, stats.mbps, stats.mbps_set, stats.sent_mbps, stats.latency_ms), (119.6, 40, 100, 28.2, Some(21.5)));
        model.apply_status(&serde_json::json!({"type": "headset", "connected": false}));
        assert!(model.headset.is_none() && model.stats.is_none());
    }

    // The host's `headset` line says whether the link runs over the USB cable. The headset's
    // address does not tell: its USB network has a link-local address like any network.
    #[test]
    fn the_headset_line_tells_whether_the_stream_uses_the_cable() {
        let line = |ip: &str, cable: serde_json::Value| {
            serde_json::json!({"type": "headset", "connected": true, "eye": [2064, 2208], "fps": 120.0, "foveated": true, "ip": ip, "cable": cable})
        };
        let path = |ip: &str, cable: serde_json::Value| {
            let mut model = Model::default();
            model.apply_status(&line(ip, cable));
            model.headset.unwrap().path
        };
        assert_eq!(path("127.0.0.1", true.into()), Some(StreamPath::Usb));
        assert_eq!(path("fe80::20:c91:c353:87c3", true.into()), Some(StreamPath::Usb));
        assert_eq!(path("192.168.1.30", false.into()), Some(StreamPath::WiFi));
        // A host that does not say leaves the path unknown
        assert_eq!(path("192.168.1.30", serde_json::Value::Null), None);
    }

    // The level has a key of its own. The yes or no of an old file stays out of the way: no error,
    // the standard level, and a save drops it.
    #[test]
    fn the_foveation_level_reads_back_and_an_old_yes_or_no_gives_the_standard_level() {
        let old = Settings::read(r#"{"fps": 90, "foveation": true}"#);
        assert!(old.unreadable.is_empty() && old.problem(Path::new("app.json")).is_none());
        assert_eq!(old.settings.foveation_level, FoveationLevel::Medium);
        let text = old.settings.file_text(Some(r#"{"fps": 90, "foveation": true}"#)).unwrap();
        assert!(!text.contains("\"foveation\"") && text.contains("\"foveation_level\": \"Medium\""), "{text}");
        for level in FoveationLevel::ALL {
            let text = serde_json::to_string(&Settings { foveation_level: level, ..Default::default() }).unwrap();
            assert_eq!(Settings::read(&text).settings.foveation_level, level);
        }
    }

    #[test]
    fn a_settings_file_without_the_connection_key_uses_automatic() {
        assert_eq!(serde_json::from_str::<Settings>(r#"{"fps": 90}"#).unwrap().connection, Connection::Automatic);
        assert_eq!(serde_json::from_str::<Settings>(r#"{"connection": "Usb"}"#).unwrap().connection, Connection::Usb);
    }

    #[test]
    fn a_settings_file_saves_and_loads_what_it_holds() {
        assert_eq!(serde_json::from_str::<Settings>(&serde_json::to_string(&Settings::default()).unwrap()).unwrap(), Settings::default());
        assert!(serde_json::from_str::<Settings>(r#"{"fps": 90}"#).unwrap().autostart);
    }

    #[test]
    fn curl_s_progress_bar_is_not_a_log_line() {
        assert_eq!(percent("######################                          37.5%"), Some(37));
        assert_eq!(percent("100.0%"), Some(100));
        assert_eq!(percent("Steam folder: 50%"), None);
        assert_eq!(percent("Unpacking"), None);
        let mut lines = vec![];
        split_lines(&b"#   2.0%\r##  5.0%\rUnpacking\n\nlast"[..], |line| lines.push(line));
        assert_eq!(lines, ["#   2.0%", "##  5.0%", "Unpacking", "last"]);
    }

    #[test]
    fn finds_steam_app_id_from_manifest() {
        let root = env::temp_dir().join(format!("cranvr-steam-test-{}", std::process::id()));
        let game = root.join("steamapps/common/Beat Saber");
        fs::create_dir_all(&game).unwrap();
        fs::write(
            root.join("steamapps/appmanifest_620980.acf"),
            "\"AppState\"\n{\n\t\"appid\"\t\t\"620980\"\n\t\"installdir\"\t\t\"Beat Saber\"\n}\n",
        )
        .unwrap();
        fs::write(root.join("steamapps/appmanifest_1.acf"), "\"installdir\" \"Other\"").unwrap();
        assert_eq!(steam_app_id(&game).as_deref(), Some("620980"));
        assert_eq!(steam_app_id(&root.join("steamapps/common/Other")).as_deref(), Some("1"));
        assert_eq!(steam_app_id(&root), None);
        fs::remove_dir_all(&root).ok();
    }

    /// Starts the real host the way the window does, connects the headless probe, and checks
    /// that the model learns about the headset and the stream. Needs `cargo build --release`
    /// first; run with `cargo test --release -p cranvr-app -- --ignored`.
    #[test]
    #[ignore]
    fn host_status_reaches_the_model() {
        let release = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/release");
        let home = env::temp_dir().join(format!("cranvr-app-test-{}", std::process::id()));
        fs::create_dir_all(&home).unwrap();
        env::set_var("CRANVR_HOME", &home);
        env::set_var("CRANVR_HOST_BIN", release.join("cranvr-host"));
        env::set_var("CRANVR_TEST_TRUST_ALL", "1");
        // The user's host keeps port 9950
        env::set_var("CRANVR_NATIVE_PORT", "9962");
        let settings = Settings { fps: 72, desktop: false, ..Default::default() };
        start_host(&settings).expect("host starts");
        std::thread::sleep(std::time::Duration::from_secs(2));
        let probe_home = home.join("probe");
        fs::create_dir_all(&probe_home).unwrap();
        let mut probe = Command::new(release.join("cranvr-probe"))
            .args(["--seconds", "8", "--tcp", "--connect", "127.0.0.1:9962"])
            .env("HOME", &probe_home)
            .stdout(Stdio::null())
            .spawn()
            .expect("probe runs");
        // sample the model while the probe streams, as the window does
        let mut model = Model::default();
        let (mut saw_headset, mut best_fps) = (false, 0.0f64);
        let probe_status = loop {
            std::thread::sleep(std::time::Duration::from_millis(150));
            pump(&mut model);
            saw_headset |= model.headset.is_some();
            best_fps = best_fps.max(model.stats.as_ref().map_or(0.0, |s| s.fps));
            if let Some(status) = probe.try_wait().unwrap() {
                break status;
            }
        };
        let (running, audio) = (model.running, model.audio.clone());
        stop_host();
        fs::remove_dir_all(&home).ok();
        assert!(probe_status.success(), "probe saw a good stream");
        assert!(running, "host ran");
        assert!(saw_headset, "a headset line");
        assert!(best_fps > 30.0, "stream fps {best_fps}");
        // The capture of the Mac's sound needs Screen Recording for the app that runs the test
        assert!(matches!(audio, Some((true, _))), "the host captures the sound: {audio:?}");
    }
}
