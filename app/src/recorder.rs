//! The recording of what the headset shows: the gem on the left hand, the hand menu's row "Record
//! what I see" and the window's Record button all toggle it, and all three show this state.
//!
//! The headset's own recorder (Meta's MetaCam) records the picture: the compositor's final view of
//! one eye with no lens warp, with every layer and the room when passthrough shows it, at the size
//! and rate of the headset's own recording settings, which cranvr does not change. The app starts and
//! stops it over adb with a small program that it pushes to the headset (quest-recorder/), so a
//! recording needs the headset on the USB cable with USB debugging. Meanwhile a process of the host's
//! binary records this Mac's sound (`cranvr-host --record-sound`, host/src/recording_sound.rs), and
//! the headset's microphone when the setting "Microphone in recordings" is on. At the stop the app
//! pulls the headset's file, deletes it on the headset and makes the final file in
//! `~/Movies/cranvr/` (`cranvr-host --mux-recording`, host/src/recording_mux.rs): the picture as the
//! headset encoded it with the Mac's sound where the user heard it.
//!
//! The work runs on a thread of its own, so the window never waits for adb. The host passes the
//! state to the headset (`recording JSON` on its input, `HostMsg::Recording`). A recording stops by
//! itself when the Mac's free disk falls under 5 GB.

use crate::backend::{self, config_dir};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        mpsc::{self, RecvTimeoutError, Sender},
        Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

/// Where the program lives on the headset while the app uses it
const ON_HEADSET: &str = "/data/local/tmp/cranvr-quest-recorder.jar";
/// The headset's own recorder
const RECORDER_APP: &str = "com.oculus.metacam";
/// Where the headset's recorder writes its videos
const VIDEOS: &str = "/sdcard/Oculus/VideoShots";
/// A recording does not start, and one that runs stops, under this much free disk on the Mac
const LEAST_FREE: u64 = 5_000_000_000;
/// How often the thread looks at the cable and the disk
const LOOK_EVERY: Duration = Duration::from_secs(2);
/// The longest wait for the headset's recorder to finish its file after the stop
const FILE_WAIT: Duration = Duration::from_secs(20);
/// How long the console in VR says "Saved: NAME"
pub const SAVED_NOTICE: Duration = Duration::from_secs(8);
/// The time limit of a short adb call, and of a pull for each 10 MB of the file
const ADB_CALL: Duration = Duration::from_secs(10);

/// What the window and the headset show of the recording.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct View {
    /// When the running recording started
    pub since: Option<Instant>,
    /// The work after a stop runs: the file comes from the headset and the final file is made
    pub saving: bool,
    /// Why a press cannot start a recording now
    pub blocked: Option<String>,
    /// The last line for the user: "Saved: NAME" or why a recording failed, counted by `note_id`
    pub note: Option<String>,
    pub note_id: u32,
    /// The last file that a recording saved
    pub saved: Option<PathBuf>,
}

impl View {
    pub fn running(&self) -> bool {
        self.since.is_some()
    }

    /// The state for the headset: `HostMsg::Recording` as the host's input takes it
    pub fn to_headset(&self) -> Value {
        let blocked = if self.saving { Some("Saving the last recording".to_string()) } else if self.running() { None } else { self.blocked.clone() };
        json!({"running": self.running(), "blocked": blocked, "note": self.note, "note_id": self.note_id})
    }
}

enum Ask {
    Toggle { microphone: bool },
    /// The app ends: stop the headset's recorder, which keeps its file in the headset's gallery
    Quit(Sender<()>),
}

static VIEW: Mutex<Option<View>> = Mutex::new(None);
static ASK: OnceLock<Mutex<Sender<Ask>>> = OnceLock::new();

pub fn view() -> View {
    VIEW.lock().unwrap().clone().unwrap_or_default()
}

/// Starts a recording, or stops the one that runs. `microphone` is the setting "Microphone in
/// recordings".
pub fn toggle(microphone: bool) {
    start_thread();
    if let Some(ask) = ASK.get() {
        ask.lock().unwrap().send(Ask::Toggle { microphone }).ok();
    }
}

/// Stops a recording that runs as the app ends. The headset's recorder stops and keeps its file in
/// the headset's gallery: the app does not wait for the pull and the final file. Waits up to 12 s.
pub fn stop_at_exit() {
    if !view().running() {
        return;
    }
    if let Some(ask) = ASK.get() {
        let (done, wait) = mpsc::channel();
        ask.lock().unwrap().send(Ask::Quit(done)).ok();
        wait.recv_timeout(Duration::from_secs(12)).ok();
    }
}

/// Tells the host the state, as after a new start of the host.
pub fn announce() {
    backend::tell_host_recording(&view().to_headset());
}

fn change(update: impl FnOnce(&mut View)) {
    let state = {
        let mut guard = VIEW.lock().unwrap();
        let view = guard.get_or_insert_with(View::default);
        let before = view.clone();
        update(view);
        (*view != before).then(|| view.to_headset())
    };
    if let Some(state) = state {
        backend::tell_host_recording(&state);
    }
}

/// A line of the recording for the Log tab, also kept in `recordings.log` in the cranvr folder: a
/// recording's numbers and its failures stay there after the app ends.
fn log(line: String) {
    let stamp = local_stamp();
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(config_dir().join("recordings.log")) {
        writeln!(file, "{stamp} {line}").ok();
    }
    backend::log(format!("Recording: {line}"));
}

fn note(text: String) {
    log(text.clone());
    change(|view| {
        view.note = Some(text);
        view.note_id += 1;
    });
}

/// Starts the thread, once. It looks at the cable and the disk every 2 s.
pub fn start_thread() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let (sender, receiver) = mpsc::channel();
        ASK.set(Mutex::new(sender)).ok();
        thread::Builder::new().name("cranvr-recorder".into()).spawn(move || run(&receiver)).ok();
    });
}

/// A recording that runs
struct Running {
    adb: PathBuf,
    serial: String,
    /// The headset's videos before the start, so the stop finds the new one
    before: Vec<String>,
    /// The folder of the recording's parts, beside the final file
    dir: PathBuf,
    sound: Child,
    /// The final file's name, from the start's local time
    name: String,
}

fn run(asks: &mpsc::Receiver<Ask>) {
    // The parts of a recording that an app's end cut short may hold the room: none stays
    if let Ok(entries) = std::fs::read_dir(movies_dir()) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(".cranvr-") {
                remove_parts(&entry.path());
            }
        }
    }
    let mut running: Option<Running> = None;
    loop {
        let ask = match asks.recv_timeout(LOOK_EVERY) {
            Ok(ask) => Some(ask),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let blocked = match &running {
            Some(_) => None,
            None => program()
                .err()
                .or_else(|| cable().err())
                .or_else(|| disk_short().map(|free| format!("The Mac has {} free: a recording needs 5 GB", gigabytes(free)))),
        };
        change(|view| view.blocked = blocked.clone());
        match (ask, running.take()) {
            (Some(Ask::Toggle { microphone }), None) => match blocked {
                Some(why) => note(why),
                None => match start(microphone) {
                    Ok(started) => {
                        log(format!("started ({})", started.name));
                        change(|view| view.since = Some(Instant::now()));
                        running = Some(started);
                    }
                    Err(e) => note(format!("The recording did not start: {e}")),
                },
            },
            (Some(Ask::Toggle { .. }), Some(recording)) => running = stop(recording),
            (Some(Ask::Quit(done)), recording) => {
                if let Some(mut recording) = recording {
                    let stopped = recorder(&recording.adb, &recording.serial, "stop");
                    log(format!("the app ends; the headset's recorder keeps the file in its gallery ({stopped:?})"));
                    drop(recording.sound.stdin.take());
                    wait_for(&mut recording.sound, Duration::from_secs(2));
                    remove_parts(&recording.dir);
                }
                done.send(()).ok();
                return;
            }
            (None, Some(recording)) => match disk_short() {
                Some(free) => {
                    note(format!("The Mac has only {} free: the recording stops", gigabytes(free)));
                    running = stop(recording);
                }
                None => running = Some(recording),
            },
            (None, None) => {}
        }
    }
}

/// Whether each device on the cable has the headset's own recorder, by its serial: a phone with
/// cranvr's phone build has none
static HAS_RECORDER: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());

/// The headset on the cable that has cranvr's headset app and the headset's own recorder, with the
/// adb that reaches it
fn cable() -> Result<(PathBuf, String), String> {
    let (adb, serials) = cabled()?;
    for serial in serials {
        let known = HAS_RECORDER.lock().unwrap().iter().find(|(seen, _)| *seen == serial).map(|(_, has)| *has);
        let has = match known {
            Some(has) => has,
            None => {
                let Ok(path) = adb_call(&adb, &serial, &["shell", "pm", "path", RECORDER_APP]) else { continue };
                let has = path.contains("package:");
                HAS_RECORDER.lock().unwrap().push((serial.clone(), has));
                has
            }
        };
        if has {
            return Ok((adb, serial));
        }
    }
    Err("Recording needs the headset on the USB cable with USB debugging".into())
}

/// The adb tool and the serials of the headsets on the USB cable. The cable thread knows them. The
/// app with no window runs no cable thread: it asks adb itself.
fn cabled() -> Result<(PathBuf, Vec<String>), String> {
    const NO_ADB: &str = "Recording needs adb: press Add USB support in the Connection settings";
    if crate::usb::watching() {
        let status = crate::usb::status();
        let adb = status.adb.ok_or(NO_ADB)?;
        return Ok((adb, status.headsets.into_iter().map(|headset| headset.serial).collect()));
    }
    let adb = crate::usb::locate_adb().ok_or(NO_ADB)?;
    let listed = crate::usb::call(&adb, &["devices", "-l"], ADB_CALL).map(|text| crate::usb::parse_devices(&text)).unwrap_or_default();
    let serials = listed.into_iter().filter(|device| device.usb && device.state == crate::usb::DeviceState::Device).map(|device| device.serial).collect();
    Ok((adb, serials))
}

/// The free disk under 5 GB where the final files go, or `None` while there is more
fn disk_short() -> Option<u64> {
    let free = free_bytes(&movies_dir())?;
    (free < LEAST_FREE).then_some(free)
}

fn gigabytes(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

fn free_bytes(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|dir| dir.exists())?;
    let c_path = std::ffi::CString::new(existing.as_os_str().as_encoded_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a plain query of the file system that holds `c_path`
    (unsafe { libc::statvfs(c_path.as_ptr(), &mut stats) } == 0).then(|| u64::from(stats.f_bavail) * stats.f_frsize)
}

/// `~/Movies/cranvr`
pub fn movies_dir() -> PathBuf {
    std::env::var_os("CRANVR_RECORDINGS").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Movies/cranvr"))
}

/// The program for the headset: in the app bundle's Resources, or in the build folder
fn program() -> Result<PathBuf, String> {
    let bundled = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.parent()?.join("Resources/cranvr-quest-recorder.jar")));
    let built = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/quest-recorder/cranvr-quest-recorder.jar");
    [bundled, Some(built)].into_iter().flatten().find(|path| path.exists()).ok_or_else(|| "this build of cranvr has no recorder for the headset (scripts/build-quest-recorder.sh)".into())
}

fn adb_call(tool: &Path, serial: &str, args: &[&str]) -> Result<String, String> {
    adb(tool, serial, args, ADB_CALL)
}

fn adb(tool: &Path, serial: &str, args: &[&str], limit: Duration) -> Result<String, String> {
    let mut all = vec!["-s", serial];
    all.extend_from_slice(args);
    crate::usb::call(tool, &all, limit)
}

/// Runs the program on the headset: `start` or `stop`
fn recorder(tool: &Path, serial: &str, verb: &str) -> Result<(), String> {
    let line = format!("CLASSPATH={ON_HEADSET} app_process / dev.cranvr.recorder.Main {verb}");
    let answer = adb(tool, serial, &["shell", &line], ADB_CALL).map_err(|e| format!("adb: {e}"))?;
    let answer = answer.trim();
    match answer.strip_prefix("ok") {
        Some(_) => Ok(()),
        None => Err(answer.strip_prefix("error ").unwrap_or(answer).to_string()),
    }
}

fn videos(tool: &Path, serial: &str) -> Result<Vec<String>, String> {
    let list = adb(tool, serial, &["shell", "ls", VIDEOS], ADB_CALL)?;
    Ok(list.lines().map(str::trim).filter(|name| name.ends_with(".mp4")).map(String::from).collect())
}

fn start(microphone: bool) -> Result<Running, String> {
    let (adb_tool, serial) = cable()?;
    let program = program()?;
    adb(&adb_tool, &serial, &["push", &program.display().to_string(), ON_HEADSET], ADB_CALL).map_err(|e| format!("the push of the recorder: {e}"))?;
    // A missing folder lists as an error: the headset has recorded nothing yet
    let before = videos(&adb_tool, &serial).unwrap_or_default();
    let name = format!("cranvr-{}.mp4", local_stamp());
    let dir = movies_dir().join(format!(".{}", name.trim_end_matches(".mp4")));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut command = Command::new(backend::host_binary()?);
    command.arg("--record-sound").arg(&dir);
    if microphone {
        command.arg("--microphone").arg(config_dir().join("microphone.pcm"));
    }
    let mut sound = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|e| format!("the sound's recording: {e}"))?;
    if let Some(out) = sound.stdout.take() {
        thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if let Some(message) = line.strip_prefix("@cranvr ").and_then(|text| serde_json::from_str::<Value>(text).ok()).and_then(|v| v["message"].as_str().map(String::from)) {
                    note(message);
                }
            }
        });
    }
    if let Err(e) = recorder(&adb_tool, &serial, "start") {
        sound.kill().ok();
        sound.wait().ok();
        remove_parts(&dir);
        return Err(format!("the headset's recorder: {e}"));
    }
    if let Some(input) = sound.stdin.as_mut() {
        input.write_all(b"start\n").and_then(|()| input.flush()).ok();
    }
    Ok(Running { adb: adb_tool, serial, before, dir, sound, name })
}

/// Stops the recording and saves its file. Returns the recording when the headset's recorder could
/// not be reached, so a later press stops it.
fn stop(mut recording: Running) -> Option<Running> {
    if let Err(e) = recorder(&recording.adb, &recording.serial, "stop") {
        note(format!("The recording runs on: the headset's recorder did not stop ({e}). Plug the cable in and press again."));
        return Some(recording);
    }
    change(|view| {
        view.since = None;
        view.saving = true;
    });
    // The end of its input ends the sound's recording, which then writes its files
    drop(recording.sound.stdin.take());
    wait_for(&mut recording.sound, Duration::from_secs(5));
    let result = save(&recording);
    remove_parts(&recording.dir);
    change(|view| view.saving = false);
    match result {
        Ok((path, numbers)) => {
            log(format!("saved {} ({numbers})", path.display()));
            change(|view| view.saved = Some(path.clone()));
            note(format!("Saved: {}", recording.name));
            backend::notify_saved(&path);
        }
        Err(e) => note(format!("The recording was not saved: {e}")),
    }
    None
}

/// Takes the headset's file and makes the final one. Returns its path and numbers.
fn save(recording: &Running) -> Result<(PathBuf, Value), String> {
    let (tool, serial) = (&recording.adb, &recording.serial);
    let video = new_video(recording)?;
    let remote = format!("{VIDEOS}/{video}");
    let local = recording.dir.join("headset.mp4");
    let bytes = adb(tool, serial, &["shell", "stat", "-c", "%s", &remote], ADB_CALL).ok().and_then(|size| size.trim().parse::<u64>().ok()).unwrap_or(0);
    let limit = ADB_CALL * (1 + u32::try_from(bytes / 10_000_000).unwrap_or(u32::MAX));
    adb(tool, serial, &["pull", &remote, &local.display().to_string()], limit).map_err(|e| format!("the pull of the headset's file: {e}"))?;
    adb(tool, serial, &["shell", "rm", &remote], ADB_CALL).ok();
    let out = movies_dir().join(&recording.name);
    let mux = Command::new(backend::host_binary()?)
        .arg("--mux-recording")
        .args([&local, &recording.dir, &out])
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("the final file: {e}"))?;
    let line = String::from_utf8_lossy(&mux.stdout).lines().filter_map(|line| line.strip_prefix("@cranvr ")).filter_map(|text| serde_json::from_str::<Value>(text).ok()).find(|v| v["type"] == "recording_file").unwrap_or(Value::Null);
    if line["ok"] != true {
        return Err(line["message"].as_str().unwrap_or("the final file was not made").to_string());
    }
    Ok((out, line["numbers"].clone()))
}

/// The name of the headset's new video, once its recorder has finished writing it
fn new_video(recording: &Running) -> Result<String, String> {
    let started = Instant::now();
    let mut last: Option<(String, String)> = None;
    while started.elapsed() < FILE_WAIT {
        let fresh = videos(&recording.adb, &recording.serial)?.into_iter().filter(|name| !recording.before.contains(name)).max();
        if let Some(name) = fresh {
            let size = adb(&recording.adb, &recording.serial, &["shell", "stat", "-c", "%s", &format!("{VIDEOS}/{name}")], ADB_CALL).unwrap_or_default();
            // The file is whole when its size held for one look
            if last.as_ref().is_some_and(|(was, before)| *was == name && *before == size && !size.trim().is_empty()) {
                return Ok(name);
            }
            last = Some((name, size));
        }
        thread::sleep(Duration::from_millis(400));
    }
    Err("the headset's recorder wrote no file".into())
}

fn wait_for(child: &mut Child, limit: Duration) {
    let started = Instant::now();
    while started.elapsed() < limit {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    child.kill().ok();
    child.wait().ok();
}

/// Deletes the parts of a recording and their folder. The parts may hold the room, so none stays.
fn remove_parts(dir: &Path) {
    for part in ["headset.mp4", "sound.wav", "microphone.wav", "sound.json", "final-sound.wav", "final-sound.m4a"] {
        std::fs::remove_file(dir.join(part)).ok();
    }
    std::fs::remove_dir(dir).ok();
}

/// The local time as YYYY-MM-DD-HHMMSS
fn local_stamp() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as libc::time_t).unwrap_or(0);
    let mut parts: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r fills `parts` from `now`
    unsafe { libc::localtime_r(&now, &mut parts) };
    format!("{:04}-{:02}-{:02}-{:02}{:02}{:02}", parts.tm_year + 1900, parts.tm_mon + 1, parts.tm_mday, parts.tm_hour, parts.tm_min, parts.tm_sec)
}

#[cfg(test)]
#[path = "recorder_tests.rs"]
mod tests;
