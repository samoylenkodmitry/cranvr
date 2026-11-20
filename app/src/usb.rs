//! The USB cable to the headset. While the Connection setting allows the cable, a thread asks adb
//! every few seconds which devices sit on the Mac's USB ports. A device that has a cranvr headset
//! app gets `adb reverse` for the host's port, so the headset reaches the host at its own address
//! 127.0.0.1 (see client/src/usb.rs). With the setting at Wi-Fi the thread takes that reverse away.
//!
//! The thread does the adb work, so the window never waits for adb. The window reads `status()`.
//! The adb tool comes from the Mac (PATH, Android Studio's SDK, Homebrew). A Mac with none gets a
//! button that downloads Google's platform-tools into the cranvr folder.

use crate::backend::config_dir;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env,
    ffi::OsStr,
    fs,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicU8, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
        Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

/// The application ids of cranvr's headset app and of its phone build. A device counts when one of
/// them is installed, not by its model name.
const HEADSET_APPS: [&str; 2] = ["dev.cranvr.client", "dev.cranvr.client.phone"];
/// Where Google publishes the platform-tools for macOS, and their size at the time of writing
const TOOLS_URL: &str = "https://dl.google.com/android/repository/platform-tools-latest-darwin.zip";
pub const TOOLS_SIZE: &str = "about 16 MB";
/// How often the thread asks adb
const POLL: Duration = Duration::from_secs(3);
/// A device without the headset app is asked again after this long: the user may install the app
const APP_RECHECK: Duration = Duration::from_secs(15);
/// The time that one adb call may take. The first call starts adb's server, which takes a moment.
const ADB_TIMEOUT: Duration = Duration::from_secs(8);

/// How the stream reaches the headset: the Connection setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Connection {
    /// The cable while the headset is plugged into this Mac, else Wi-Fi
    #[default]
    Automatic,
    /// Wi-Fi only: the cable's link is taken away
    WiFi,
    /// The cable: the window says when the headset is not on it
    Usb,
}

impl Connection {
    pub fn allows_usb(self) -> bool {
        self != Connection::WiFi
    }

    fn code(self) -> u8 {
        match self {
            Connection::Automatic => 0,
            Connection::WiFi => 1,
            Connection::Usb => 2,
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            1 => Connection::WiFi,
            2 => Connection::Usb,
            _ => Connection::Automatic,
        }
    }
}

/// A device on the USB cable that has a cranvr headset app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cabled {
    pub serial: String,
    /// The device's link is set up: the headset reaches the host at 127.0.0.1
    pub reversed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Download {
    #[default]
    Idle,
    Working(String),
    Failed(String),
}

/// What the window shows of the cable.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Status {
    /// The adb tool, when the Mac has one
    pub adb: Option<PathBuf>,
    /// The first look for adb and devices is done
    pub looked: bool,
    pub headsets: Vec<Cabled>,
    /// A device on the cable waits for its user to allow USB debugging
    pub unauthorized: bool,
    /// Why the downloaded adb was not used, when another adb server runs
    pub blocked: Option<String>,
    pub download: Download,
}

// ---------------------------------------------------------------------------------------------
// What adb prints

/// The state that `adb devices` gives a device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceState {
    Device,
    /// The device asks its user to allow this Mac
    Unauthorized,
    Offline,
    Other(String),
}

/// A line of `adb devices -l`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub serial: String,
    pub state: DeviceState,
    /// The device sits on a USB port. A device on Wi-Fi debugging has no `usb:` field.
    pub usb: bool,
}

/// The devices of `adb devices -l`. The header and the lines that adb prints while it starts its
/// server (they begin with `*`) are no devices.
pub fn parse_devices(text: &str) -> Vec<Listed> {
    text.lines()
        .filter(|line| !line.starts_with('*') && !line.starts_with("List of devices"))
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let serial = fields.next()?.to_string();
            let state = match fields.next()? {
                "device" => DeviceState::Device,
                "unauthorized" => DeviceState::Unauthorized,
                "offline" => DeviceState::Offline,
                other => DeviceState::Other(other.to_string()),
            };
            Some(Listed { serial, state, usb: fields.any(|field| field.starts_with("usb:")) })
        })
        .collect()
}

/// The packages of `pm list packages`: one `package:NAME` per line.
pub fn parse_packages(text: &str) -> Vec<String> {
    text.lines().filter_map(|line| Some(line.trim().strip_prefix("package:")?.split_whitespace().next()?.to_string())).collect()
}

/// Whether the packages hold a cranvr headset app. `pm list packages dev.cranvr.client` also lists
/// a package that merely contains the text, so the names must match whole.
pub fn has_headset_app(packages: &[String]) -> bool {
    packages.iter().any(|package| HEADSET_APPS.contains(&package.as_str()))
}

/// Whether `adb reverse --list` shows the link of this port: `host tcp:9950 tcp:9950`, or on an
/// older adb `UsbFfs tcp:9950 tcp:9950`.
pub fn reverse_has(list: &str, port: u16) -> bool {
    let link = format!("tcp:{port}");
    list.lines().any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        fields.len() >= 2 && fields[fields.len() - 2] == link && fields[fields.len() - 1] == link
    })
}

/// The number at the end of adb's own version: `Android Debug Bridge version 1.0.41` gives 41.
pub fn parse_adb_version(text: &str) -> Option<u32> {
    let line = text.lines().find(|line| line.contains("Android Debug Bridge version"))?;
    line.split_whitespace().last()?.rsplit('.').next()?.parse().ok()
}

// ---------------------------------------------------------------------------------------------
// What to do

/// What the thread knows of one device after it asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    pub serial: String,
    pub state: DeviceState,
    pub usb: bool,
    /// Whether a cranvr headset app is installed, once asked
    pub has_app: Option<bool>,
    /// Whether the device has the link for the host's port
    pub reversed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Add(String),
    Remove(String),
}

/// The adb calls that bring the devices to what the setting asks. A device that is not ready, is
/// not on a USB port or has no headset app is left alone.
pub fn plan(choice: Connection, devices: &[Facts]) -> Vec<Action> {
    devices
        .iter()
        .filter(|d| d.usb && d.state == DeviceState::Device && d.has_app == Some(true))
        .filter_map(|d| match (choice.allows_usb(), d.reversed) {
            (true, false) => Some(Action::Add(d.serial.clone())),
            (false, true) => Some(Action::Remove(d.serial.clone())),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Finding adb

/// The places where an adb may be, in the order of preference: the PATH, the Android SDK of
/// Android Studio, Homebrew's android-platform-tools, and the copy that cranvr downloaded. An app
/// opened from the Dock has a short PATH, so the usual places are named too.
pub fn adb_candidates(path_var: Option<&OsStr>, home: &Path, sdk_homes: &[PathBuf], casks: &[PathBuf], own: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = path_var.map(|paths| env::split_paths(paths).map(|dir| dir.join("adb")).collect()).unwrap_or_default();
    found.extend(sdk_homes.iter().map(|sdk| sdk.join("platform-tools/adb")));
    found.push(home.join("Library/Android/sdk/platform-tools/adb"));
    found.extend(["/opt/homebrew/bin/adb", "/usr/local/bin/adb"].map(PathBuf::from));
    found.extend(casks.iter().map(|cask| cask.join("platform-tools/adb")));
    found.push(own.join("platform-tools/adb"));
    found
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The adb that this Mac has. `CRANVR_ADB` names one by hand, or says `none` to act as a Mac
/// without adb: only the copy that cranvr downloaded counts then.
pub fn locate_adb() -> Option<PathBuf> {
    if let Some(chosen) = env::var_os("CRANVR_ADB") {
        let own = own_adb_in(&config_dir());
        return if chosen == "none" { Some(own).filter(|path| is_executable(path)) } else { Some(PathBuf::from(chosen)) };
    }
    let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let sdk_homes: Vec<PathBuf> = ["ANDROID_HOME", "ANDROID_SDK_ROOT"].iter().filter_map(|name| env::var_os(name).map(PathBuf::from)).collect();
    let casks: Vec<PathBuf> = fs::read_dir("/opt/homebrew/Caskroom/android-platform-tools")
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    adb_candidates(env::var_os("PATH").as_deref(), &home, &sdk_homes, &casks, &config_dir()).into_iter().find(|path| is_executable(path))
}

/// The adb in the folder where the download puts it
fn own_adb_in(dir: &Path) -> PathBuf {
    dir.join("platform-tools/adb")
}

// ---------------------------------------------------------------------------------------------
// Running adb

/// Runs adb with a time limit and returns what it printed on stdout. `None` when it failed or
/// took too long.
fn adb(tool: &Path, args: &[&str], limit: Duration) -> Option<String> {
    let mut child = Command::new(tool).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut text = String::new();
        stdout.read_to_string(&mut text).ok();
        sender.send(text).ok();
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < limit => thread::sleep(Duration::from_millis(20)),
            _ => {
                child.kill().ok();
                child.wait().ok();
                return None;
            }
        }
    };
    let text = receiver.recv_timeout(Duration::from_secs(1)).ok()?;
    status.success().then_some(text)
}

/// The version of the adb server that runs on this Mac, or `None` when none runs. It asks the
/// server directly (`host:version`), because a client of another version would stop the server.
fn running_server_version() -> Option<u32> {
    let mut tcp = TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, 5037)), Duration::from_millis(300)).ok()?;
    tcp.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
    tcp.write_all(b"000chost:version").ok()?;
    let mut reply = [0u8; 12];
    tcp.read_exact(&mut reply).ok()?;
    if &reply[..4] != b"OKAY" {
        return None;
    }
    u32::from_str_radix(std::str::from_utf8(&reply[8..12]).ok()?, 16).ok()
}

// ---------------------------------------------------------------------------------------------
// The thread

static STATUS: Mutex<Status> = Mutex::new(Status {
    adb: None,
    looked: false,
    headsets: Vec::new(),
    unauthorized: false,
    blocked: None,
    download: Download::Idle,
});
static CHOICE: AtomicU8 = AtomicU8::new(0);
static WAKE: OnceLock<Mutex<Sender<()>>> = OnceLock::new();

pub fn status() -> Status {
    STATUS.lock().unwrap().clone()
}

fn update(change: impl FnOnce(&mut Status)) {
    change(&mut STATUS.lock().unwrap());
}

/// Tells the thread what the Connection setting says now. A change wakes it.
pub fn set_choice(choice: Connection) {
    if CHOICE.swap(choice.code(), Ordering::Relaxed) != choice.code() {
        wake();
    }
}

fn wake() {
    if let Some(sender) = WAKE.get() {
        sender.lock().unwrap().send(()).ok();
    }
}

/// Starts the thread, once.
pub fn start(port: u16) {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let (sender, receiver) = mpsc::channel();
        WAKE.set(Mutex::new(sender)).ok();
        if let Err(e) = thread::Builder::new().name("cranvr-usb".into()).spawn(move || run(port, receiver)) {
            crate::backend::log(format!("The USB cable is not watched: {e}"));
        }
    });
}

/// What the thread remembers of a device between two looks.
struct Known {
    has_app: bool,
    asked: Instant,
}

fn run(port: u16, wake: mpsc::Receiver<()>) {
    let mut known: HashMap<String, Known> = HashMap::new();
    // A link from an earlier run may still sit on a device: the first pass takes it away when the
    // setting says Wi-Fi
    let mut cleanup = true;
    let mut last = Connection::from_code(CHOICE.load(Ordering::Relaxed));
    loop {
        let choice = Connection::from_code(CHOICE.load(Ordering::Relaxed));
        if choice != last {
            cleanup |= !choice.allows_usb();
            last = choice;
        }
        let tool = locate_adb();
        update(|s| s.adb = tool.clone());
        match tool.filter(|_| choice.allows_usb() || cleanup) {
            Some(tool) => {
                look(&tool, choice, port, &mut known);
                cleanup = false;
            }
            None => update(|s| {
                s.headsets.clear();
                s.unauthorized = false;
                s.blocked = None;
            }),
        }
        update(|s| s.looked = true);
        // Wi-Fi needs no polling once the link is gone. A change of the setting wakes the thread.
        let wait = if choice.allows_usb() { POLL } else { Duration::from_secs(60) };
        match wake.recv_timeout(wait) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// One look: the devices, the headsets among them, and the calls that `plan` asks for.
fn look(tool: &Path, choice: Connection, port: u16, known: &mut HashMap<String, Known>) {
    // A copy of adb that cranvr downloaded must not meet a server of another version: the client
    // would stop that server, and with it the user's other tools
    if tool == own_adb_in(&config_dir()) {
        let ours = adb(tool, &["version"], ADB_TIMEOUT).as_deref().and_then(parse_adb_version);
        if let (Some(server), Some(ours)) = (running_server_version(), ours) {
            if server != ours {
                update(|s| {
                    s.headsets.clear();
                    s.blocked = Some("Another adb tool runs on this Mac. Close it, then cranvr can use the cable.".into());
                });
                return;
            }
        }
    }
    update(|s| s.blocked = None);
    let Some(text) = adb(tool, &["devices", "-l"], ADB_TIMEOUT) else {
        update(|s| {
            s.headsets.clear();
            s.unauthorized = false;
        });
        return;
    };
    let listed: Vec<Listed> = parse_devices(&text).into_iter().filter(|d| d.usb).collect();
    known.retain(|serial, _| listed.iter().any(|d| &d.serial == serial));
    let mut facts = Vec::new();
    for device in &listed {
        let mut has_app = None;
        let mut reversed = false;
        if device.state == DeviceState::Device {
            let fresh = known.get(&device.serial).is_some_and(|k| k.has_app || k.asked.elapsed() < APP_RECHECK);
            if !fresh {
                let packages = adb(tool, &["-s", &device.serial, "shell", "pm", "list", "packages", HEADSET_APPS[0]], ADB_TIMEOUT)
                    .map(|text| parse_packages(&text));
                if let Some(packages) = packages {
                    known.insert(device.serial.clone(), Known { has_app: has_headset_app(&packages), asked: Instant::now() });
                }
            }
            has_app = known.get(&device.serial).map(|k| k.has_app);
            if has_app == Some(true) {
                reversed = adb(tool, &["-s", &device.serial, "reverse", "--list"], ADB_TIMEOUT).is_some_and(|list| reverse_has(&list, port));
            }
        }
        facts.push(Facts { serial: device.serial.clone(), state: device.state.clone(), usb: device.usb, has_app, reversed });
    }
    let link = format!("tcp:{port}");
    for action in plan(choice, &facts) {
        match &action {
            Action::Add(serial) => {
                let done = adb(tool, &["-s", serial, "reverse", &link, &link], ADB_TIMEOUT).is_some();
                crate::backend::log(format!("USB: {} for {serial}", if done { "the host's port reaches the headset" } else { "adb reverse failed" }));
                if let Some(f) = facts.iter_mut().find(|f| &f.serial == serial) {
                    f.reversed = done;
                }
            }
            Action::Remove(serial) => {
                let done = adb(tool, &["-s", serial, "reverse", "--remove", &link], ADB_TIMEOUT).is_some();
                crate::backend::log(format!("USB: {} for {serial}", if done { "the host's port no longer reaches the headset" } else { "adb reverse --remove failed" }));
                if done {
                    if let Some(f) = facts.iter_mut().find(|f| &f.serial == serial) {
                        f.reversed = false;
                    }
                }
            }
        }
    }
    update(|s| {
        s.headsets = facts
            .iter()
            .filter(|f| f.state == DeviceState::Device && f.has_app == Some(true))
            .map(|f| Cabled { serial: f.serial.clone(), reversed: f.reversed })
            .collect();
        s.unauthorized = facts.iter().any(|f| f.state == DeviceState::Unauthorized);
    });
}

// ---------------------------------------------------------------------------------------------
// The download

/// Downloads Google's platform-tools into the cranvr folder. The user pressed the button that says
/// where the files come from and how large they are. The work runs on its own thread.
pub fn download_tools() {
    {
        let mut status = STATUS.lock().unwrap();
        if matches!(status.download, Download::Working(_)) {
            return;
        }
        status.download = Download::Working("Starting".into());
    }
    thread::spawn(|| {
        let (url, protocols) = tools_url();
        let result = fetch_tools(&config_dir(), &url, protocols);
        update(|s| {
            s.download = match &result {
                Ok(()) => Download::Idle,
                Err(e) => Download::Failed(e.clone()),
            }
        });
        if let Err(e) = result {
            crate::backend::log(format!("USB support: {e}"));
        }
        wake();
    });
}

/// The address of the zip. `CRANVR_PLATFORM_TOOLS_URL` replaces it for a test with a local file.
fn tools_url() -> (String, &'static str) {
    match env::var("CRANVR_PLATFORM_TOOLS_URL") {
        Ok(url) => (url, "=https,http,file"),
        Err(_) => (TOOLS_URL.to_string(), "=https"),
    }
}

fn fetch_tools(dir: &Path, url: &str, protocols: &str) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("The folder {} cannot be made: {e}", dir.display()))?;
    let zip = dir.join("platform-tools.zip.part");
    let unpacked = dir.join("platform-tools-unpacking");
    fs::remove_file(&zip).ok();
    fs::remove_dir_all(&unpacked).ok();
    let mut curl = Command::new("/usr/bin/curl")
        .args(["--fail", "--silent", "--show-error", "--location", "--proto", protocols, "--tlsv1.2", "--max-time", "900", "--output"])
        .arg(&zip)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl did not start: {e}"))?;
    let mut complaint = curl.stderr.take();
    let status = loop {
        match curl.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                let megabytes = fs::metadata(&zip).map_or(0, |m| m.len()) / 1_000_000;
                update(|s| s.download = Download::Working(format!("Downloading: {megabytes} MB of {TOOLS_SIZE}")));
                thread::sleep(Duration::from_millis(300));
            }
            Err(e) => return Err(format!("The download stopped: {e}")),
        }
    };
    if !status.success() {
        let mut text = String::new();
        if let Some(stderr) = complaint.as_mut() {
            stderr.read_to_string(&mut text).ok();
        }
        fs::remove_file(&zip).ok();
        return Err(format!("The download failed. {}", text.trim().lines().last().unwrap_or("Check the internet connection.")));
    }
    update(|s| s.download = Download::Working("Unpacking".into()));
    let unzip = Command::new("/usr/bin/unzip")
        .args(["-q", "-o"])
        .arg(&zip)
        .arg("-d")
        .arg(&unpacked)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("unzip did not start: {e}"))?;
    fs::remove_file(&zip).ok();
    if !unzip.status.success() {
        fs::remove_dir_all(&unpacked).ok();
        return Err("The download is not a zip file. Try again.".into());
    }
    let tools = dir.join("platform-tools");
    fs::remove_dir_all(&tools).ok();
    let moved = fs::rename(unpacked.join("platform-tools"), &tools);
    fs::remove_dir_all(&unpacked).ok();
    moved.map_err(|e| format!("The platform-tools are not in the zip: {e}"))?;
    // The tool must run before the window offers it as ready
    match adb(&own_adb_in(dir), &["version"], ADB_TIMEOUT).as_deref().and_then(parse_adb_version) {
        Some(version) => {
            crate::backend::log(format!("USB support: adb {version} is in {}", tools.display()));
            Ok(())
        }
        None => Err("The downloaded adb does not run on this Mac.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The Tab, a phone without USB debugging allowed, a headset that is not ready, a device on
    // Wi-Fi debugging and an emulator: only the first three sit on a USB port.
    const DEVICES: &str = "List of devices attached
* daemon not running; starting now at tcp:5037
* daemon started successfully
R52R40K1F2A            device usb:51511296X product:gts7xlwifiser model:SM_T970 device:gts7xlwifi transport_id:3594
2G0YC5ZH2000XX         unauthorized usb:1-1 transport_id:2
0123456789ABCDEF       offline usb:2-3 transport_id:7
192.168.1.55:5555      device product:eureka_user model:Quest_3 device:eureka transport_id:5
adb-2G0YC5ZH-ab._adb-tls-connect._tcp device product:eureka_user model:Quest_3 device:eureka transport_id:6
emulator-5554          device product:sdk_gphone64 model:sdk device:emu64 transport_id:1

";

    #[test]
    fn the_devices_of_adb_are_told_by_state_and_by_the_usb_port() {
        let devices = parse_devices(DEVICES);
        assert_eq!(devices.len(), 6, "{devices:?}");
        assert_eq!(devices[0], Listed { serial: "R52R40K1F2A".into(), state: DeviceState::Device, usb: true });
        assert_eq!(devices[1], Listed { serial: "2G0YC5ZH2000XX".into(), state: DeviceState::Unauthorized, usb: true });
        assert_eq!(devices[2].state, DeviceState::Offline);
        // Devices of Wi-Fi debugging and emulators are no cable
        assert!(devices[3..].iter().all(|d| !d.usb && d.state == DeviceState::Device));
        // A header alone, or nothing, lists nobody
        assert!(parse_devices("List of devices attached\n\n").is_empty());
        assert!(parse_devices("").is_empty());
    }

    #[test]
    fn an_unusual_state_stays_as_adb_said_it() {
        let devices = parse_devices("ABC123 authorizing usb:1-4 transport_id:9\nABC124 recovery usb:1-5 transport_id:10\n");
        assert_eq!(devices[0].state, DeviceState::Other("authorizing".into()));
        assert_eq!(devices[1].state, DeviceState::Other("recovery".into()));
    }

    // The Tab's real answer holds the phone build and the headset build. A package that only
    // contains the id's text does not count.
    #[test]
    fn a_headset_app_is_told_by_its_whole_package_name() {
        let tab = parse_packages("package:dev.cranvr.client.phone\npackage:dev.cranvr.client\n");
        assert_eq!(tab, ["dev.cranvr.client.phone", "dev.cranvr.client"]);
        assert!(has_headset_app(&tab));
        assert!(has_headset_app(&parse_packages("package:dev.cranvr.client\n")));
        assert!(!has_headset_app(&parse_packages("package:dev.cranvr.clients\npackage:dev.cranvr.client2\n")));
        assert!(!has_headset_app(&parse_packages("")));
        assert!(!has_headset_app(&parse_packages("package:com.oculus.vrshell installer=com.oculus\n")));
        assert_eq!(parse_packages("package:dev.cranvr.client installer=x\n"), ["dev.cranvr.client"]);
    }

    #[test]
    fn the_reverse_list_shows_the_link_of_one_port() {
        assert!(reverse_has("UsbFfs tcp:9950 tcp:9950\n", 9950));
        assert!(reverse_has("host-18 tcp:9962 tcp:9962\nUsbFfs tcp:9950 tcp:9950\n", 9962));
        assert!(!reverse_has("UsbFfs tcp:9950 tcp:9950\n", 9962));
        // A link of the same port to another port is not the host's link
        assert!(!reverse_has("UsbFfs tcp:9950 tcp:8080\n", 9950));
        assert!(!reverse_has("", 9950));
    }

    #[test]
    fn adb_tells_its_version() {
        assert_eq!(parse_adb_version("Android Debug Bridge version 1.0.41\nVersion 36.0.0-13206524\nInstalled as /x/adb\n"), Some(41));
        assert_eq!(parse_adb_version("nothing"), None);
    }

    fn device(serial: &str, state: DeviceState, usb: bool, has_app: Option<bool>, reversed: bool) -> Facts {
        Facts { serial: serial.into(), state, usb, has_app, reversed }
    }

    #[test]
    fn the_cable_gets_the_link_in_automatic_and_usb_mode_and_loses_it_in_wifi_mode() {
        let devices = [
            device("tab", DeviceState::Device, true, Some(true), false),
            device("quest", DeviceState::Device, true, Some(true), true),
        ];
        for choice in [Connection::Automatic, Connection::Usb] {
            assert_eq!(plan(choice, &devices), [Action::Add("tab".into())]);
        }
        assert_eq!(plan(Connection::WiFi, &devices), [Action::Remove("quest".into())]);
    }

    #[test]
    fn a_device_that_is_not_a_ready_headset_on_the_cable_is_left_alone() {
        let devices = [
            device("asks", DeviceState::Unauthorized, true, None, false),
            device("offline", DeviceState::Offline, true, None, false),
            device("phone", DeviceState::Device, true, Some(false), false),
            device("not asked yet", DeviceState::Device, true, None, false),
            device("wifi debugging", DeviceState::Device, false, Some(true), false),
            // Another tool's link on a device without the app stays too
            device("other", DeviceState::Device, true, Some(false), true),
        ];
        for choice in [Connection::Automatic, Connection::WiFi, Connection::Usb] {
            assert!(plan(choice, &devices).is_empty(), "{choice:?}");
        }
    }

    #[test]
    fn the_connection_setting_defaults_to_automatic_and_reads_back() {
        assert_eq!(Connection::default(), Connection::Automatic);
        assert_eq!(serde_json::from_str::<Connection>("\"WiFi\"").unwrap(), Connection::WiFi);
        for choice in [Connection::Automatic, Connection::WiFi, Connection::Usb] {
            assert_eq!(Connection::from_code(choice.code()), choice);
        }
        assert!(Connection::Automatic.allows_usb() && Connection::Usb.allows_usb() && !Connection::WiFi.allows_usb());
    }

    #[test]
    fn adb_is_looked_for_in_the_usual_places_and_the_downloaded_copy_comes_last() {
        let path = env::join_paths(["/usr/bin", "/Users/me/bin"]).unwrap();
        let found = adb_candidates(
            Some(&path),
            Path::new("/Users/me"),
            &[PathBuf::from("/sdk")],
            &[PathBuf::from("/opt/homebrew/Caskroom/android-platform-tools/36.0.0")],
            Path::new("/cfg"),
        );
        let names: Vec<&str> = found.iter().map(|p| p.to_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "/usr/bin/adb",
                "/Users/me/bin/adb",
                "/sdk/platform-tools/adb",
                "/Users/me/Library/Android/sdk/platform-tools/adb",
                "/opt/homebrew/bin/adb",
                "/usr/local/bin/adb",
                "/opt/homebrew/Caskroom/android-platform-tools/36.0.0/platform-tools/adb",
                "/cfg/platform-tools/adb",
            ]
        );
        // The Dock gives an app a short PATH, or none: the named places still come
        assert_eq!(adb_candidates(None, Path::new("/h"), &[], &[], Path::new("/c")).len(), 4);
    }

    // The button's work without Google: a zip with the platform-tools layout comes from a file.
    #[test]
    fn the_download_unpacks_the_platform_tools_and_checks_that_adb_runs() {
        let root = env::temp_dir().join(format!("cranvr-usb-test-{}", std::process::id()));
        let tools = root.join("zip-source/platform-tools");
        fs::create_dir_all(&tools).unwrap();
        fs::write(tools.join("adb"), "#!/bin/sh\necho 'Android Debug Bridge version 1.0.41'\n").unwrap();
        fs::set_permissions(tools.join("adb"), fs::Permissions::from_mode(0o755)).unwrap();
        let zip = root.join("tools.zip");
        let made = Command::new("/usr/bin/zip").args(["-q", "-r"]).arg(&zip).arg("platform-tools").current_dir(root.join("zip-source")).status().unwrap();
        assert!(made.success());
        let (home, url) = (root.join("home"), format!("file://{}", zip.display()));
        fetch_tools(&home, &url, "=file").unwrap();
        assert!(is_executable(&own_adb_in(&home)));
        assert!(!home.join("platform-tools.zip.part").exists() && !home.join("platform-tools-unpacking").exists());
        // A second download replaces the first
        fetch_tools(&home, &url, "=file").unwrap();
        // A file that is no zip fails with words, and leaves the working copy
        fs::write(&zip, "not a zip").unwrap();
        let error = fetch_tools(&home, &url, "=file").unwrap_err();
        assert!(error.contains("not a zip"), "{error}");
        assert!(is_executable(&own_adb_in(&home)));
        // A real https address is refused where only files are allowed: the protocol list holds
        let refused = fetch_tools(&home, "https://example.invalid/x.zip", "=file").unwrap_err();
        assert!(refused.starts_with("The download failed"), "{refused}");
        fs::remove_dir_all(&root).ok();
    }
}
