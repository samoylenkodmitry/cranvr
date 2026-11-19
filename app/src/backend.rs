//! Everything outside the window: the host child process, settings on disk, paired headsets
//! from ALVR's session.json, and games under Wine.

use crate::launch::{GameOptions, WineOptions};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, VecDeque},
    env,
    ffi::OsString,
    fs,
    io::{BufRead, BufReader, Write},
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::Instant,
};

/// The TCP port of the native protocol, `cranvr-host`'s default for `--native-port`.
pub const NATIVE_PORT: u16 = 9950;

/// The host's config dir; `cranvr-host` uses the same default.
pub fn config_dir() -> PathBuf {
    env::var_os("CRANVR_HOME").map(PathBuf::from).unwrap_or_else(|| {
        env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join("Library/Application Support/cranvr")
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub fps: u32,
    pub mbps: u32,
    pub foveation: bool,
    pub desktop: bool,
    /// Desktop captures per second
    pub desktop_fps: u32,
    /// Headset hostnames the host accepts
    pub trusted: Vec<String>,
    /// The last game started: a Windows path inside the Wine prefix, or a Mac path
    pub game: String,
    /// Start the host when the window opens
    pub autostart: bool,
    /// The app on the headset, and with it the protocol that the host speaks
    pub headset_app: HeadsetApp,
    /// Counts for the ALVR client only: the cranvr client picks its media path itself
    pub stream_protocol: StreamProtocol,
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
    /// The headset holds one decoded frame instead of two: about 4 ms less latency, maybe
    /// stutter on a weak Wi-Fi. Off by default until tested in a worn headset. The ALVR client
    /// only: the host's buffering option reaches ALVR's session alone.
    pub low_latency: bool,
    /// Games under Wine may use at most this many MSAA samples; 0 leaves it to the game
    pub game_msaa_limit: u32,
    /// While a game runs, a display in a scaled mode shows its panel's own pixels
    pub unscale_displays: bool,
    /// The launch options of each Windows program, by its Windows path. A program with no entry
    /// starts with the default options.
    pub launch: BTreeMap<String, GameOptions>,
    /// The options of every Windows program that cranvr starts
    pub wine: WineOptions,
}

/// The app that runs on the headset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HeadsetApp {
    /// The ALVR client, version 20.14
    #[default]
    Alvr,
    /// cranvr's own client, `dev.cranvr.client2`: the host runs with `--protocol native`
    Cranvr,
}

/// How the stream travels between this Mac and the headset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StreamProtocol {
    /// UDP, and TCP once the host reports that no UDP stream data arrives
    #[default]
    Auto,
    Udp,
    Tcp,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fps: 120,
            mbps: 100,
            foveation: true,
            desktop: true,
            desktop_fps: 30,
            trusted: vec![],
            game: String::new(),
            autostart: true,
            headset_app: HeadsetApp::Alvr,
            stream_protocol: StreamProtocol::Auto,
            game_audio: true,
            virtual_display: None,
            vr_main_display: false,
            window_in_vr: false,
            game_scale: 0.7,
            low_latency: false,
            game_msaa_limit: 2,
            unscale_displays: true,
            launch: BTreeMap::new(),
            wine: WineOptions::default(),
        }
    }
}

impl Settings {
    fn path() -> PathBuf {
        config_dir().join("app.json")
    }

    pub fn load() -> Self {
        fs::read(Self::path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        fs::create_dir_all(config_dir()).ok();
        if let Ok(text) = serde_json::to_vec_pretty(self) {
            fs::write(Self::path(), text).ok();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Headset {
    pub eye: (u32, u32),
    pub fps: f64,
    pub foveated: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Stats {
    pub fps: f64,
    pub game_frames: u64,
    pub render_ms: f64,
    pub encode_ms: f64,
    pub mbps: u64,
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

/// A headset that announced itself to the host.
#[derive(Clone, Debug, PartialEq)]
pub struct Client {
    pub hostname: String,
    pub trusted: bool,
}

/// What the window shows.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Model {
    /// Changes when the learned game sizes change, so the window reads them again
    pub sizes_version: u32,
    pub running: bool,
    pub headset: Option<Headset>,
    pub game: Option<String>,
    pub desktop: Option<(bool, String)>,
    /// Whether the host captures this Mac's sound for the headset, and why not
    pub audio: Option<(bool, String)>,
    /// The ID of the virtual display that the desktop view shows
    pub vr_display: Option<u32>,
    /// The virtual display is the main display now: a headset streams
    pub vr_main: bool,
    pub stats: Option<Stats>,
    pub clients: Vec<Client>,
    /// The host's state in native mode, once it has said it
    pub native: Option<Native>,
    /// This Mac's addresses on the local network: the headset connects to one of them
    pub lan: Vec<Ipv4Addr>,
    pub log: VecDeque<String>,
    pub message: Option<String>,
    /// Screen Recording and Accessibility
    pub permissions: [bool; 2],
    pub steam: crate::steam::SteamState,
    /// Whether Rosetta 2 runs Intel code, which Wine needs
    pub rosetta: bool,
    pub steam_running: bool,
    pub steam_signed_in: bool,
    pub games: Vec<crate::steam::Game>,
    /// A setup script that runs now, by name
    pub task: Option<String>,
    /// Problems on the network path that the host found, newest last
    pub network: Vec<String>,
    /// The host found that UDP stream data does not arrive: in Auto mode the stream uses TCP
    pub tcp_fallback: bool,
    /// The host reported that no UDP stream data arrives, and the window has not acted on it yet
    pub udp_blocked: bool,
    /// Requests from the console in VR that the window has not run yet
    pub vr_requests: Vec<Value>,
    /// Reports of a black right eye that the window has not answered yet (see black_eye.rs)
    pub black_eyes: Vec<Value>,
    /// What the window and the console in VR say about the last black right eye, until it expires
    pub notice: Option<(crate::black_eye::Notice, Instant)>,
}

const LOG_LINES: usize = 200;

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
    pub fn log(&mut self, line: String) {
        if self.log.len() == LOG_LINES {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }

    fn apply_status(&mut self, v: &Value) {
        match v["type"].as_str() {
            Some("headset") => {
                self.headset = v["connected"].as_bool().unwrap_or(false).then(|| Headset {
                    eye: (v["eye"][0].as_u64().unwrap_or(0) as u32, v["eye"][1].as_u64().unwrap_or(0) as u32),
                    fps: v["fps"].as_f64().unwrap_or(0.0),
                    foveated: v["foveated"].as_bool().unwrap_or(false),
                });
                if self.headset.is_none() {
                    self.stats = None;
                    self.vr_main = false;
                }
            }
            Some("game") => {
                self.game = v["connected"].as_bool().unwrap_or(false).then(|| v["name"].as_str().unwrap_or("game").to_string());
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
                if v["problem"] == "no_poses" && v["protocol"] == "udp" {
                    self.udp_blocked = true;
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
            Some("black_eye") => self.black_eyes.push(v.clone()),
            Some("stats") => {
                self.stats = Some(Stats {
                    fps: v["fps"].as_f64().unwrap_or(0.0),
                    game_frames: v["game_frames"].as_u64().unwrap_or(0),
                    render_ms: v["render_ms"].as_f64().unwrap_or(0.0),
                    encode_ms: v["encode_ms"].as_f64().unwrap_or(0.0),
                    mbps: v["mbps"].as_u64().unwrap_or(0),
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
    TaskDone { name: String, ok: bool },
    Message(String),
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

static HOST: Mutex<Option<Child>> = Mutex::new(None);
/// The host's input. The user's answers to a headset's request go there.
static HOST_INPUT: Mutex<Option<ChildStdin>> = Mutex::new(None);

/// `cranvr-host` next to this executable, or at $CRANVR_HOST_BIN.
fn host_binary() -> Result<PathBuf, String> {
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

/// The command line of `cranvr-host`. `tcp_fallback` is the window's switch to TCP in Auto mode.
/// `pair_code` opens the pairing window of the native protocol. The options that count for ALVR
/// only stay off in native mode: `--tcp` would force TCP media on every native client. The host
/// ignores `--buffering-frames` and `--trust` there.
fn host_args(settings: &Settings, tcp_fallback: bool, pair_code: Option<&str>, config_dir: &Path, test: bool) -> Vec<OsString> {
    let native = settings.headset_app == HeadsetApp::Cranvr;
    let mut args: Vec<OsString> = vec![];
    args.extend(["--status", "--fps", &settings.fps.to_string(), "--mbps", &settings.mbps.to_string()].map(OsString::from));
    if native {
        args.extend(["--protocol", "native"].map(OsString::from));
        if let Some(code) = pair_code {
            args.extend(["--pair", code].map(OsString::from));
        }
    }
    let tcp = !native
        && match settings.stream_protocol {
            StreamProtocol::Auto => tcp_fallback,
            StreamProtocol::Udp => false,
            StreamProtocol::Tcp => true,
        };
    args.extend(["--config-dir".into(), config_dir.as_os_str().to_owned()]);
    // For tests on one Mac: the probe needs TCP and trusts nothing by hostname
    if test {
        args.push("--trust-all".into());
    }
    if tcp || test {
        args.push("--tcp".into());
    }
    if !settings.foveation {
        args.push("--no-foveation".into());
    }
    if !settings.game_audio {
        args.push("--no-game-audio".into());
    }
    args.extend(["--game-scale", &settings.game_scale.to_string()].map(OsString::from));
    if !settings.unscale_displays {
        args.push("--keep-display-modes".into());
    }
    if settings.low_latency && !native {
        args.extend(["--buffering-frames", "1"].map(OsString::from));
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
    if !native {
        for name in &settings.trusted {
            args.extend(["--trust", name.as_str()].map(OsString::from));
        }
    }
    args
}

/// Starts the host with these settings, after stopping a running one. See `host_args` for the
/// other two arguments.
pub fn start_host(settings: &Settings, tcp_fallback: bool, pair_code: Option<&str>) -> Result<(), String> {
    stop_host();
    let mut command = Command::new(host_binary()?);
    command.args(host_args(settings, tcp_fallback, pair_code, &config_dir(), env::var_os("CRANVR_TEST_TRUST_ALL").is_some()));
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    *HOST_INPUT.lock().unwrap() = child.stdin.take();
    let sender = channel().0.clone();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let log = HostLog::create();
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

impl HostLog {
    fn create() -> Self {
        let file = fs::File::create(config_dir().join("host.log")).ok().map(|f| Arc::new(Mutex::new(f)));
        Self { file, started: Instant::now() }
    }

    fn write(&self, line: &str) {
        if let Some(file) = &self.file {
            writeln!(file.lock().unwrap(), "{:.3} {line}", self.started.elapsed().as_secs_f64()).ok();
        }
    }
}

/// Writes vr-menu.json for the console in VR: the games and the settings that it offers, and a
/// short notice while there is one. The host reads it, adds the stream's state and sends it to the
/// headset. Writes only on a change.
pub fn write_vr_menu(settings: &Settings, games: &[crate::steam::Game], notice: Option<&str>) {
    static WRITTEN: Mutex<String> = Mutex::new(String::new());
    let menu = vr_menu(settings, games, notice);
    let mut written = WRITTEN.lock().unwrap();
    if *written != menu && fs::write(config_dir().join("vr-menu.json"), &menu).is_ok() {
        *written = menu;
    }
}

/// The text of vr-menu.json.
fn vr_menu(settings: &Settings, games: &[crate::steam::Game], notice: Option<&str>) -> String {
    let games: Vec<Value> = games
        .iter()
        .filter_map(|g| {
            let program = g.exe.as_ref()?.file_name()?.to_string_lossy().to_string();
            Some(serde_json::json!({"name": g.name, "program": program}))
        })
        .collect();
    let mut menu = serde_json::json!({
        "games": games,
        "fps": settings.fps,
        "game_scale": settings.game_scale,
        "msaa": settings.game_msaa_limit,
        "low_latency": settings.low_latency,
    });
    if let Some(notice) = notice {
        menu["notice"] = notice.into();
    }
    menu.to_string()
}

pub fn stop_host() {
    HOST_INPUT.lock().unwrap().take();
    if let Some(mut child) = HOST.lock().unwrap().take() {
        // SIGTERM first, so ALVR can say goodbye to the headset
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
            HostLine::Status(v) => model.apply_status(&v),
            HostLine::Log(l) => model.log(l),
            HostLine::Exited => {
                model.headset = None;
                model.game = None;
                model.stats = None;
                // The line of an old host can arrive after the first line of its successor
                if !host_running() {
                    model.native = None;
                }
            }
            HostLine::Message(text) => model.message = Some(text),
            HostLine::TaskDone { name, ok } => {
                model.task = None;
                model.message = (!ok).then(|| format!("{name} failed; see the log"));
            }
        }
    }
    model.running = host_running();
}

/// The setup scripts: in the app bundle's Resources, or in the source tree.
pub fn scripts_dir() -> PathBuf {
    let bundled = env::current_exe().ok().and_then(|e| Some(e.parent()?.parent()?.join("Resources/scripts")));
    match bundled {
        Some(b) if b.is_dir() => b,
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts"),
    }
}

/// Runs a setup script in the background; its output goes to the log.
pub fn run_script(title: &str, script: &str, args: &[&str]) -> Result<(), String> {
    let path = scripts_dir().join(script);
    let mut child = Command::new("/bin/zsh")
        .arg(&path)
        .args(args)
        .env("CRANVR_HOME", config_dir())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let sender = channel().0.clone();
    let (stdout, stderr) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let err_sender = sender.clone();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            err_sender.send(HostLine::Log(line)).ok();
        }
    });
    let name = title.to_string();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            sender.send(HostLine::Log(line)).ok();
        }
        let ok = child.wait().is_ok_and(|s| s.success());
        sender.send(HostLine::TaskDone { name, ok }).ok();
    });
    Ok(())
}

/// Headsets from ALVR's session.json, which the host updates when a headset announces itself.
pub fn read_clients() -> Vec<Client> {
    let Ok(bytes) = fs::read(config_dir().join("session.json")) else { return vec![] };
    let Ok(v) = serde_json::from_slice::<Value>(&bytes) else { return vec![] };
    let mut clients: Vec<Client> = v["client_connections"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(name, c)| Client { hostname: name.clone(), trusted: c["trusted"].as_bool().unwrap_or(false) })
                .collect()
        })
        .unwrap_or_default();
    clients.sort_by(|a, b| a.hostname.cmp(&b.hostname));
    clients
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

    // The window switches to TCP only for missing UDP data, and shows each problem once.
    #[test]
    fn a_udp_problem_asks_for_tcp_once() {
        let mut model = Model::default();
        let udp = serde_json::json!({"type": "network", "problem": "no_poses", "protocol": "udp", "message": "no UDP"});
        model.apply_status(&udp);
        model.apply_status(&udp);
        assert!(model.udp_blocked);
        assert_eq!(model.network, ["no UDP"]);

        let mut model = Model::default();
        model.apply_status(&serde_json::json!({"type": "network", "problem": "no_poses", "protocol": "tcp", "message": "no TCP"}));
        model.apply_status(&serde_json::json!({"type": "network", "problem": "vpn_route", "message": "tunnel"}));
        assert!(!model.udp_blocked);
        assert_eq!(model.network, ["no TCP", "tunnel"]);
    }

    fn host_options(settings: &Settings, tcp_fallback: bool, pair_code: Option<&str>) -> Vec<String> {
        host_args(settings, tcp_fallback, pair_code, Path::new("/cfg"), false).into_iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    fn native_settings() -> Settings {
        Settings { headset_app: HeadsetApp::Cranvr, ..Default::default() }
    }

    #[test]
    fn the_alvr_client_gets_the_same_host_arguments_as_before() {
        assert_eq!(
            host_options(&Settings::default(), false, None),
            ["--status", "--fps", "120", "--mbps", "100", "--config-dir", "/cfg", "--game-scale", "0.7", "--desktop", "--desktop-fps", "30"]
        );
        let alvr = Settings { trusted: vec!["1234.client".into()], stream_protocol: StreamProtocol::Tcp, low_latency: true, ..Default::default() };
        let args = host_options(&alvr, false, Some("482913"));
        assert!(!args.contains(&"--protocol".to_string()) && !args.contains(&"--pair".to_string()), "{args:?}");
        for wanted in ["--tcp", "--buffering-frames", "--trust", "1234.client"] {
            assert!(args.contains(&wanted.to_string()), "{wanted} in {args:?}");
        }
        // Auto mode streams over TCP once the window found that UDP data does not arrive
        assert!(host_options(&Settings::default(), true, None).contains(&"--tcp".to_string()));
    }

    #[test]
    fn the_cranvr_client_gets_the_native_protocol_with_or_without_a_pairing_code() {
        let without = host_options(&native_settings(), false, None);
        assert_eq!(
            without,
            ["--status", "--fps", "120", "--mbps", "100", "--protocol", "native", "--config-dir", "/cfg", "--game-scale", "0.7", "--desktop", "--desktop-fps", "30"]
        );
        let with = host_options(&native_settings(), false, Some("482913"));
        assert_eq!(with[5..9], ["--protocol", "native", "--pair", "482913"]);
        assert_eq!(with.len(), without.len() + 2);
    }

    #[test]
    fn settings_for_the_alvr_client_do_not_reach_a_native_host() {
        let settings = Settings {
            trusted: vec!["1234.client".into()],
            stream_protocol: StreamProtocol::Tcp,
            low_latency: true,
            ..native_settings()
        };
        // `--tcp` would force TCP media on every native client
        let args = host_options(&settings, true, None);
        for unwanted in ["--tcp", "--buffering-frames", "--trust", "1234.client"] {
            assert!(!args.contains(&unwanted.to_string()), "{unwanted} in {args:?}");
        }
        // Settings of both clients stay
        let quiet = Settings { foveation: false, game_audio: false, virtual_display: Some((1920, 1080)), ..native_settings() };
        let args = host_options(&quiet, false, None);
        for wanted in ["--no-foveation", "--no-game-audio", "--virtual-display", "1920x1080"] {
            assert!(args.contains(&wanted.to_string()), "{wanted} in {args:?}");
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
        model.apply_status(&serde_json::json!({"type": "native", "port": 9950, "paired": [], "pairing_s": null, "pending": []}));
        let native = model.native.clone().unwrap();
        assert_eq!((native.port, native.paired.len(), native.pending.len()), (9950, 0, 0));
        model.apply_status(&serde_json::json!({
            "type": "native", "port": 9951, "paired": ["Meta Quest 3"], "pairing_s": null,
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
    fn the_hosts_black_eye_line_waits_in_the_model_for_the_window_to_answer() {
        let mut model = Model::default();
        model.apply_status(&serde_json::json!({"type": "black_eye", "eye": "right", "program": "C:\\Games\\game.exe"}));
        assert_eq!(model.black_eyes.len(), 1);
        assert_eq!(model.black_eyes[0]["program"], "C:\\Games\\game.exe");
    }

    #[test]
    fn the_console_menu_carries_the_notice_only_while_there_is_one() {
        let read = |notice| serde_json::from_str::<Value>(&vr_menu(&Settings::default(), &[], notice)).unwrap();
        assert!(read(None).get("notice").is_none());
        assert_eq!(read(Some("The right eye stayed black. The game restarts."))["notice"], "The right eye stayed black. The game restarts.");
        assert_eq!(read(None)["fps"], Settings::default().fps);
    }

    // The native host sends the same `headset` and `stats` lines as the ALVR host. Its latency
    // report has the `total_ms` field that the window reads.
    #[test]
    fn a_native_hosts_headset_and_stats_lines_fill_the_model() {
        let mut model = Model::default();
        model.apply_status(&serde_json::json!({"type": "headset", "connected": true, "eye": [2064, 2208], "fps": 120.0, "foveated": true}));
        model.apply_status(&serde_json::json!({
            "type": "stats", "fps": 119.6, "game_frames": 0, "render_ms": 3.1, "encode_ms": 4.2, "mbps": 100,
            "latency": {"frames": 590, "total_ms": 21.5, "rtt_ms": 2.0, "decode_ms": 3.0}
        }));
        let headset = model.headset.clone().unwrap();
        assert_eq!((headset.eye, headset.fps, headset.foveated), ((2064, 2208), 120.0, true));
        let stats = model.stats.clone().unwrap();
        assert_eq!((stats.fps, stats.mbps, stats.latency_ms), (119.6, 100, Some(21.5)));
        model.apply_status(&serde_json::json!({"type": "headset", "connected": false}));
        assert!(model.headset.is_none() && model.stats.is_none());
    }

    #[test]
    fn settings_files_from_before_the_headset_app_choice_use_the_alvr_client() {
        assert_eq!(serde_json::from_str::<Settings>(r#"{"fps": 90, "trusted": ["1234.client"]}"#).unwrap().headset_app, HeadsetApp::Alvr);
        assert_eq!(serde_json::from_str::<Settings>(r#"{"headset_app": "Cranvr"}"#).unwrap().headset_app, HeadsetApp::Cranvr);
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
        let settings = Settings { fps: 72, desktop: false, ..Default::default() };
        start_host(&settings, false, None).expect("host starts");
        std::thread::sleep(std::time::Duration::from_secs(2));
        let probe_home = home.join("probe");
        fs::create_dir_all(&probe_home).unwrap();
        let mut probe = Command::new(release.join("cranvr-probe"))
            .args(["--seconds", "8"])
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
        let clients = read_clients();
        fs::remove_dir_all(&home).ok();
        assert!(probe_status.success(), "probe saw a good stream");
        assert!(running, "host ran");
        assert!(saw_headset, "a headset line");
        assert!(best_fps > 30.0, "stream fps {best_fps}");
        assert!(!clients.is_empty(), "session.json lists the probe");
        // The capture of the Mac's sound needs Screen Recording for the app that runs the test
        assert!(matches!(audio, Some((true, _))), "the host captures the sound: {audio:?}");
    }
}
