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
//! pulls the headset's file and makes the final file in `~/Movies/cranvr/` (`cranvr-host
//! --mux-recording`, host/src/recording_mux.rs): the picture as the headset encoded it with the Mac's
//! sound where the user heard it.
//!
//! The headset's recorder runs on by itself whatever happens to the app, so the app keeps a journal of
//! each recording beside its parts: `~/Movies/cranvr/.cranvr-STAMP/recording.json`. It names the
//! headset, the headset's videos before the start and the recording's own file. With it:
//! - a quit of the app, SIGTERM among them, stops the headset's recorder and the sound, and keeps the
//!   parts. The next start of the app saves the recording;
//! - after a kill or a crash, the next start finds the journal, stops the headset's recorder if it
//!   still records, and saves the recording;
//! - a cable that comes out during a recording leaves the headset recording. The gem and the window
//!   say so. When the cable comes back, the app stops the recorder and saves what it recorded.
//!
//! The app knows the recorder's real state from the headset's videos: the recorder writes the `mdat`
//! box of a file only at its stop, so a file without one that was written in the last 20 s is being
//! recorded (`mp4_writing`). A start while such a file exists refuses, since a recording of the
//! headset's own menu runs. A start counts only once its file shows. A file of an earlier name that
//! grew counts too.
//!
//! The Quest's storage is small, so the app deletes the headset's copy once the Mac's file is safe:
//! the pulled file has the headset's size and the final file passed its check (the picture's length
//! and its sound, recording_mux.rs). When the final file cannot be made, the headset's file itself
//! becomes the final file, with the headset's own sound, and the headset keeps its copy. A file that the app did not record, such as
//! one of the headset's own menu, the app never touches.
//!
//! The work runs on a thread of its own, so the window never waits for adb. The host passes the
//! state to the headset (`recording JSON` on its input, `HostMsg::Recording`). A recording stops by
//! itself when the Mac's free disk falls under 5 GB.

use crate::backend::{self, config_dir};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        mpsc::{self, RecvTimeoutError, Sender},
        Arc, Mutex, OnceLock,
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
/// The longest wait for the recorder's file after its start
const START_WAIT: Duration = Duration::from_secs(6);
/// The longest wait for the headset's recorder to finish its file after the stop
const FILE_WAIT: Duration = Duration::from_secs(20);
/// How often a wait for the headset's file looks
const POLL: Duration = Duration::from_millis(500);
/// How long the console in VR says "Saved: NAME"
pub const SAVED_NOTICE: Duration = Duration::from_secs(8);
/// The time limit of a short adb call, and of a pull for each 10 MB of the file
const ADB_CALL: Duration = Duration::from_secs(10);
/// A recording whose saving failed this often waits for the user: its parts stay
const MOST_ATTEMPTS: u32 = 3;
/// The cable is out once this many looks in a row miss the headset: a change of the headset's USB
/// functions, as at a start of its app, hides it for a second or two
const OUT_LOOKS: u32 = 3;
/// The journal of a recording in its folder
const JOURNAL: &str = "recording.json";

const CABLE_OUT: &str = "The cable is out: the headset records on. The Mac saves the recording when the cable is back.";
const CABLE_BACK: &str = "The cable is back: the recording stops, and the Mac saves it";
const RECORDS_ALREADY: &str = "The headset's own recorder records already. Stop that recording in the headset first.";

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

/// A video in the headset's folder, its size in bytes, and whether its recorder still writes it
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Video {
    pub name: String,
    pub bytes: u64,
    #[serde(default)]
    pub writing: bool,
    /// The seconds since its last write, on the headset's clock
    #[serde(default)]
    pub age_s: u64,
}

/// A file with no `mdat` box whose last write is older than this is no recording that runs: its
/// recorder died, as with a flat battery, and never finished it. The headset's recorder writes its
/// first data about 5 s after its start and then every few seconds.
const WRITE_RECENT_S: u64 = 20;

/// Whether the first bytes of an MP4 file say that its writer still writes it. The headset's recorder
/// (Android's MPEG4Writer) starts a file with `ftyp` and `free` boxes that keep room, and writes the
/// `mdat` box's header and the `moov` box only at its stop: a file whose boxes hold neither is still
/// being recorded. Its size alone says little: the recorder writes its first data about 5 s after the
/// start, and then in pieces (2026-11-27). A file of other boxes is no recording of the headset's.
pub(crate) fn mp4_writing(header: &[u8]) -> bool {
    if header.len() < 8 {
        return true;
    }
    let mut at = 0;
    while at + 8 <= header.len() {
        let size = u32::from_be_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]]) as usize;
        match &header[at + 4..at + 8] {
            // An mdat of unknown length is still written
            b"mdat" => return size == 0 || (size == 1 && header.get(at + 8..at + 16).is_some_and(|large| large.iter().all(|byte| *byte == 0))),
            b"moov" => return false,
            b"ftyp" | b"free" | b"skip" | b"wide" if size >= 8 => at += size,
            _ => return false,
        }
    }
    true
}

/// The sound of a recording that runs: a process of the host's binary
pub(crate) trait Sound: Send {
    /// The headset's recorder answered that it started
    fn mark_start(&mut self);
    /// Ends the sound's recording, which then writes its files, and waits up to `limit`
    fn end(&mut self, limit: Duration);
    /// The messages of the sound's process for the user since the last call
    fn messages(&mut self) -> Vec<String>;
}

/// What a recording needs of the Mac and the headset. `Real` reaches them; the tests use a made-up
/// headset.
pub(crate) trait Device {
    /// Why no recording can start on this Mac at all, such as a missing program for the headset
    fn unready(&self) -> Option<String>;
    /// The serials of the headsets on the USB cable that have the headset's own recorder, or why the
    /// app cannot tell
    fn headsets(&self) -> Result<Vec<String>, String>;
    /// Puts the program that starts and stops the recorder on the headset
    fn prepare(&self, serial: &str) -> Result<(), String>;
    fn videos(&self, serial: &str) -> Result<Vec<Video>, String>;
    /// Asks the headset's recorder to "start" or "stop"
    fn recorder(&self, serial: &str, verb: &str) -> Result<(), String>;
    /// Copies the headset's video of `bytes` bytes to `to`
    fn pull(&self, serial: &str, video: &Video, to: &Path) -> Result<(), String>;
    fn remove(&self, serial: &str, name: &str) -> Result<(), String>;
    fn free_bytes(&self, dir: &Path) -> Option<u64>;
    fn start_sound(&self, dir: &Path, microphone: bool) -> Result<Box<dyn Sound>, String>;
    /// Makes the final file `out` of the headset's `video` and the sound in `dir`, and returns its
    /// numbers. The numbers say `checked` when the file passed its check.
    fn make_final(&self, video: &Path, dir: &Path, out: &Path) -> Result<Value, String>;
    fn wait(&self, time: Duration);
}

/// Where the state, the log lines and the saved files go: the window, the headset and
/// `recordings.log` for the app, a list for a test
pub(crate) trait Out {
    fn view(&self, view: &View);
    fn log(&self, line: &str);
    fn saved(&self, path: &Path);
}

/// Where a recording stands in its journal
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    /// The app asked the headset's recorder to start, or it records: the app that wrote this may
    /// have died, so the recorder may still record
    Running,
    /// The headset's recorder stopped: the recording waits to be saved
    Stopped,
    /// Saving failed too often: the parts wait for the user
    Failed,
}

/// The journal of a recording, beside its parts
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Journal {
    /// The final file's name, from the start's local time
    name: String,
    /// The headset that records
    serial: String,
    /// The headset's videos before the start, so the stop finds the new one or the one that grew
    before: Vec<Video>,
    /// The headset's file of this recording, once it grew
    video: Option<String>,
    phase: Phase,
    attempts: u32,
}

impl Journal {
    fn write(&self, dir: &Path) {
        let part = dir.join(format!("{JOURNAL}.part"));
        if std::fs::write(&part, serde_json::to_vec_pretty(self).unwrap_or_default()).is_ok() {
            std::fs::rename(&part, dir.join(JOURNAL)).ok();
        }
    }

    fn read(dir: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(dir.join(JOURNAL)).ok()?).ok()
    }
}

/// A recording that runs
struct Running {
    dir: PathBuf,
    journal: Journal,
    sound: Box<dyn Sound>,
    /// The looks in a row that missed the headset on the cable
    missing: u32,
    /// The cable went out during the recording: the headset records on
    cable_out: bool,
}

/// Why a recording was not saved: it may work later, or it never will
enum Unsaved {
    Later(String),
    Never(String),
}

/// The recordings of one Mac: the one that runs and those that wait to be saved
pub(crate) struct Recorder<D: Device, O: Out> {
    device: D,
    out: O,
    /// The folder of the final files
    movies: PathBuf,
    view: View,
    running: Option<Running>,
}

impl<D: Device, O: Out> Recorder<D, O> {
    pub fn new(device: D, out: O, movies: PathBuf) -> Self {
        Self { device, out, movies, view: View::default(), running: None }
    }

    #[cfg(test)]
    pub fn view(&self) -> &View {
        &self.view
    }

    fn change(&mut self, update: impl FnOnce(&mut View)) {
        let before = self.view.clone();
        update(&mut self.view);
        if self.view != before {
            self.out.view(&self.view);
        }
    }

    fn note(&mut self, text: &str) {
        self.out.log(text);
        let text = text.to_string();
        self.change(|view| {
            view.note = Some(text);
            view.note_id += 1;
        });
    }

    /// A look every 2 s: the cable, the disk, the sound's messages, and the recordings that wait to
    /// be saved.
    pub fn look(&mut self) {
        let headsets = self.device.headsets();
        if let Some(running) = self.running.as_mut() {
            let messages = running.sound.messages();
            // adb that gives no answer says nothing of the cable
            let on_cable = headsets.as_ref().ok().map(|serials| serials.contains(&running.journal.serial));
            running.missing = match on_cable {
                Some(false) => running.missing + 1,
                Some(true) => 0,
                None => running.missing,
            };
            let (missing, cable_out) = (running.missing, running.cable_out);
            for message in messages {
                self.note(&message);
            }
            if missing >= OUT_LOOKS && !cable_out {
                self.running.as_mut().expect("a recording runs").cable_out = true;
                self.note(CABLE_OUT);
            } else if on_cable == Some(true) && cable_out {
                self.note(CABLE_BACK);
                self.stop();
            } else if let Some(free) = self.disk_short().filter(|_| on_cable == Some(true)) {
                self.note(&format!("The Mac has only {} free: the recording stops", gigabytes(free)));
                self.stop();
            }
            return;
        }
        let blocked = self.blocked(&headsets);
        self.change(|view| view.blocked = blocked);
        if let Ok(serials) = &headsets {
            if let Some((dir, journal)) = self.waiting(serials).into_iter().next() {
                self.finish(&dir, journal);
            }
        }
    }

    /// A press of the gem, the hand menu's row or the window's button.
    pub fn press(&mut self, microphone: bool) {
        if let Some(running) = &self.running {
            if running.cable_out {
                self.note(CABLE_OUT);
            } else {
                self.stop();
            }
            return;
        }
        let headsets = self.device.headsets();
        if let Some(why) = self.blocked(&headsets) {
            self.note(&why);
            return;
        }
        // A recording that waits to be saved goes first: its headset's recorder may still run
        for (dir, journal) in self.waiting(headsets.as_deref().unwrap_or_default()) {
            self.finish(&dir, journal);
        }
        match self.start(microphone) {
            Ok(running) => {
                self.out.log(&format!("started ({}, the headset's file {})", running.journal.name, running.journal.video.as_deref().unwrap_or("?")));
                self.running = Some(running);
                self.change(|view| view.since = Some(Instant::now()));
            }
            Err(e) => self.note(&format!("The recording did not start: {e}")),
        }
    }

    /// The app ends: the headset's recorder and the sound stop, and the parts wait for the next start
    /// of the app, which saves the recording.
    pub fn quit(&mut self) {
        let Some(mut running) = self.running.take() else { return };
        let stopped = self.device.recorder(&running.journal.serial, "stop");
        running.sound.end(Duration::from_secs(2));
        // A recorder that did not answer may still record: the next start looks at its file
        if stopped.is_ok() {
            running.journal.phase = Phase::Stopped;
            running.journal.write(&running.dir);
        }
        let how = match &stopped {
            Ok(()) => "the headset's recorder stopped".to_string(),
            Err(e) => format!("the headset's recorder did not answer ({e})"),
        };
        self.out.log(&format!("the app ends: {how}; the next start of cranvr saves {}", running.journal.name));
        self.change(|view| view.since = None);
    }

    fn disk_short(&self) -> Option<u64> {
        let free = self.device.free_bytes(&self.movies)?;
        (free < LEAST_FREE).then_some(free)
    }

    fn blocked(&self, headsets: &Result<Vec<String>, String>) -> Option<String> {
        self.device
            .unready()
            .or_else(|| match headsets {
                Ok(serials) if serials.is_empty() => Some("Recording needs the headset on the USB cable with USB debugging".into()),
                Ok(_) => None,
                Err(e) => Some(e.clone()),
            })
            .or_else(|| self.disk_short().map(|free| format!("The Mac has {} free: a recording needs 5 GB", gigabytes(free))))
    }

    /// The recordings that wait to be saved on the headsets of `serials`, oldest first
    fn waiting(&self, serials: &[String]) -> Vec<(PathBuf, Journal)> {
        let mut found: Vec<(PathBuf, Journal)> = std::fs::read_dir(&self.movies)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".cranvr-"))
            .filter_map(|entry| Some((entry.path(), Journal::read(&entry.path())?)))
            .filter(|(_, journal)| journal.phase != Phase::Failed && serials.contains(&journal.serial))
            .collect();
        found.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        found
    }

    /// The name of a video that is new against `before` or larger than it was, the new ones first
    fn grown(before: &[Video], now: &[Video]) -> Option<String> {
        let new = now.iter().filter(|video| !before.iter().any(|old| old.name == video.name)).max_by(|a, b| a.name.cmp(&b.name));
        let larger = now.iter().find(|video| before.iter().any(|old| old.name == video.name && video.bytes > old.bytes));
        new.or(larger).map(|video| video.name.clone())
    }

    /// The video that the headset's recorder writes now, with the list of videos
    fn growing(&self, serial: &str) -> Result<(Option<String>, Vec<Video>), String> {
        let now = self.device.videos(serial)?;
        Ok((now.iter().filter(|video| video.writing).map(|video| video.name.clone()).max(), now))
    }

    fn start(&mut self, microphone: bool) -> Result<Running, String> {
        let serial = self.device.headsets()?.into_iter().next().ok_or("Recording needs the headset on the USB cable with USB debugging")?;
        self.device.prepare(&serial).map_err(|e| format!("the push of the recorder: {e}"))?;
        let (grows, before) = self.growing(&serial).map_err(|e| format!("the headset's videos: {e}"))?;
        if grows.is_some() {
            return Err(RECORDS_ALREADY.into());
        }
        let name = free_name(&self.movies, &local_stamp());
        let dir = self.movies.join(format!(".{}", name.trim_end_matches(".mp4")));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut journal = Journal { name, serial: serial.clone(), before, video: None, phase: Phase::Running, attempts: 0 };
        journal.write(&dir);
        let mut sound = match self.device.start_sound(&dir, microphone) {
            Ok(sound) => sound,
            Err(e) => {
                remove_parts(&dir);
                return Err(format!("the sound's recording: {e}"));
            }
        };
        if let Err(e) = self.device.recorder(&serial, "start") {
            sound.end(Duration::from_secs(2));
            remove_parts(&dir);
            return Err(format!("the headset's recorder: {e}"));
        }
        sound.mark_start();
        // The start counts once the recorder writes its file
        let started = Instant::now();
        while started.elapsed() < START_WAIT {
            self.device.wait(POLL);
            let now = self.device.videos(&serial).unwrap_or_default();
            if let Some(video) = now.iter().filter(|video| video.writing).map(|video| video.name.clone()).max().or_else(|| Self::grown(&journal.before, &now)) {
                journal.video = Some(video);
                journal.write(&dir);
                return Ok(Running { dir, journal, sound, missing: 0, cable_out: false });
            }
        }
        self.device.recorder(&serial, "stop").ok();
        sound.end(Duration::from_secs(2));
        remove_parts(&dir);
        Err(format!("the headset's recorder answered but wrote no file in {} s", START_WAIT.as_secs()))
    }

    /// Stops the recording and saves its file. The recording runs on when the headset's recorder
    /// still records after its stop failed.
    fn stop(&mut self) {
        let Some(mut running) = self.running.take() else { return };
        let serial = running.journal.serial.clone();
        if let Err(e) = self.device.recorder(&serial, "stop") {
            // A recorder that stopped by itself, such as from the headset's own menu, has a whole file
            let grows = self.growing(&serial).map(|(grows, _)| grows.is_some()).unwrap_or(true);
            if grows {
                self.running = Some(running);
                self.note(&format!("The recording runs on: the headset's recorder did not stop ({e}). Press again."));
                return;
            }
            self.out.log(&format!("the headset's recorder had stopped already ({e})"));
        }
        running.sound.end(Duration::from_secs(5));
        running.journal.phase = Phase::Stopped;
        running.journal.write(&running.dir);
        self.change(|view| view.since = None);
        self.finish(&running.dir, running.journal);
    }

    /// Saves the recording of the journal in `dir`, and tells the user.
    fn finish(&mut self, dir: &Path, mut journal: Journal) {
        self.change(|view| view.saving = true);
        let result = self.save(dir, &mut journal);
        self.change(|view| view.saving = false);
        match result {
            Ok((path, numbers)) => {
                remove_parts(dir);
                self.out.log(&format!("saved {} ({numbers})", path.display()));
                let saved = path.clone();
                self.change(|view| view.saved = Some(saved));
                self.note(&format!("Saved: {}", journal.name));
                self.out.saved(&path);
            }
            Err(Unsaved::Never(e)) => {
                remove_parts(dir);
                self.note(&format!("The recording {} was not saved: {e}", journal.name));
            }
            Err(Unsaved::Later(e)) => {
                journal.attempts += 1;
                if journal.attempts >= MOST_ATTEMPTS {
                    journal.phase = Phase::Failed;
                    self.note(&format!("The recording {} was not saved: {e}. Its parts stay in {}; the headset keeps its file.", journal.name, dir.display()));
                } else {
                    self.note(&format!("The recording {} is not saved yet: {e}. The app tries again.", journal.name));
                }
                journal.write(dir);
            }
        }
    }

    fn save(&mut self, dir: &Path, journal: &mut Journal) -> Result<(PathBuf, Value), Unsaved> {
        let serial = journal.serial.clone();
        if journal.phase == Phase::Running {
            // The app that started this recording ended without stopping the headset's recorder: a kill
            // or a crash. The recorder may still record.
            let (grows, now) = self.growing(&serial).map_err(later("the headset's videos"))?;
            if journal.video.is_none() {
                journal.video = Self::grown(&journal.before, &now);
            }
            if grows.is_some() {
                self.device.recorder(&serial, "stop").map_err(later("the stop of the headset's recorder"))?;
                self.out.log(&format!("the recording {} of an earlier start of cranvr: the headset's recorder still ran, and it stopped now", journal.name));
            } else {
                self.out.log(&format!("the recording {} of an earlier start of cranvr: the headset's recorder had stopped", journal.name));
            }
            journal.phase = Phase::Stopped;
            journal.write(dir);
        }
        let name = match journal.video.clone() {
            Some(name) => name,
            None => Self::grown(&journal.before, &self.device.videos(&serial).map_err(later("the headset's videos"))?).ok_or(Unsaved::Never("the headset holds no file of it".into()))?,
        };
        let video = self.whole(&serial, &name)?;
        let local = dir.join("headset.mp4");
        let part = dir.join("headset.mp4.part");
        std::fs::remove_file(&part).ok();
        self.device.pull(&serial, &video, &part).map_err(later("the pull of the headset's file"))?;
        let pulled = std::fs::metadata(&part).map(|meta| meta.len()).unwrap_or(0);
        if pulled != video.bytes {
            return Err(Unsaved::Later(format!("the pull of the headset's file gave {pulled} of {} bytes", video.bytes)));
        }
        std::fs::rename(&part, &local).map_err(|e| Unsaved::Later(format!("{}: {e}", local.display())))?;
        let out = free_file(&self.movies, &journal.name);
        let made = dir.join("final.mp4");
        let numbers = match self.device.make_final(&local, dir, &made) {
            Ok(numbers) if numbers["checked"] == true => {
                std::fs::rename(&made, &out).map_err(|e| Unsaved::Later(format!("{}: {e}", out.display())))?;
                numbers
            }
            failed => {
                // The headset's own file is a whole recording with the headset's sound
                let why = match failed {
                    Ok(numbers) => format!("the final file did not pass its check ({numbers})"),
                    Err(e) => e,
                };
                std::fs::remove_file(&made).ok();
                std::fs::rename(&local, &out).map_err(|e| Unsaved::Later(format!("{}: {e}", out.display())))?;
                // Nothing checked that this copy plays: the headset keeps its own
                json!({"sound": {"source": "headset", "why": format!("the Mac's sound could not be added: {why}")}, "checked": false})
            }
        };
        if numbers["checked"] == true {
            match self.device.remove(&serial, &video.name) {
                Ok(()) => self.out.log(&format!("the headset's copy {} ({}) is deleted: the Mac's file passed its check", video.name, megabytes(video.bytes))),
                Err(e) => self.out.log(&format!("the headset keeps its copy {}: its delete failed ({e})", video.name)),
            }
        } else {
            self.out.log(&format!("the headset keeps its copy {}: no check passed the Mac's file", video.name));
        }
        Ok((out, numbers))
    }

    /// The headset's video `name` once its recorder has finished writing it: its header is whole and
    /// its size held for a look
    fn whole(&self, serial: &str, name: &str) -> Result<Video, Unsaved> {
        let started = Instant::now();
        let mut last: Option<Video> = None;
        while started.elapsed() < FILE_WAIT {
            let now = self.device.videos(serial).map_err(|e| Unsaved::Later(format!("the headset's videos: {e}")))?;
            let Some(video) = now.into_iter().find(|video| video.name == name) else {
                return Err(Unsaved::Never(format!("the headset's file {name} is gone")));
            };
            if !video.writing && last.as_ref() == Some(&video) {
                return Ok(video);
            }
            last = Some(video);
            self.device.wait(POLL);
        }
        Err(Unsaved::Later(format!("the headset's file {name} still grew after {} s", FILE_WAIT.as_secs())))
    }
}

/// A line of the headset's list of videos, `SIZE MTIME xHEX NAME`, at the headset's time `now`
fn parse_video(line: &str, now: u64) -> Option<Video> {
    let mut parts = line.splitn(4, ' ');
    let (bytes, written, hex, name) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let hex = hex.strip_prefix('x')?;
    let header: Vec<u8> = (0..hex.len() / 2).filter_map(|at| u8::from_str_radix(hex.get(at * 2..at * 2 + 2)?, 16).ok()).collect();
    let age_s = now.saturating_sub(written.parse().ok()?);
    Some(Video { name: name.to_string(), bytes: bytes.parse().ok()?, writing: mp4_writing(&header) && age_s < WRITE_RECENT_S, age_s })
}

/// A reason of a failure that a later try may not have
fn later(what: &'static str) -> impl Fn(String) -> Unsaved {
    move |e| Unsaved::Later(format!("{what}: {e}"))
}

/// `NAME.mp4`, or `NAME-2.mp4` and so on when the folder holds that name already
fn numbered(name: &str, count: u32) -> String {
    let stem = name.trim_end_matches(".mp4");
    if count == 1 { format!("{stem}.mp4") } else { format!("{stem}-{count}.mp4") }
}

/// The name of a new recording, `cranvr-STAMP.mp4`, that neither a file nor the parts of another
/// recording use
fn free_name(movies: &Path, stamp: &str) -> String {
    (1..)
        .map(|count| numbered(&format!("cranvr-{stamp}"), count))
        .find(|name| !movies.join(name).exists() && !movies.join(format!(".{}", name.trim_end_matches(".mp4"))).exists())
        .expect("a free name")
}

/// The path of the final file `name`, or of `name-2` and so on when a file holds that name already
fn free_file(movies: &Path, name: &str) -> PathBuf {
    (1..).map(|count| movies.join(numbered(name, count))).find(|path| !path.exists()).expect("a free name")
}

fn gigabytes(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

fn megabytes(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1e6)
}

/// Deletes the parts of a recording and their folder. The parts may hold the room, so none stays.
fn remove_parts(dir: &Path) {
    let parts = [
        "headset.mp4", "headset.mp4.part", "final.mp4", "sound.wav", "microphone.wav", "sound.json", "sound.json.part",
        "final-sound.wav", "final-sound.m4a", JOURNAL, "recording.json.part",
    ];
    for part in parts {
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

// The app's one recorder on its own thread

enum Ask {
    // A test's press counts in `TOGGLES` and sends nothing
    #[cfg_attr(test, allow(dead_code))]
    Toggle { microphone: bool },
    /// The app ends: stop the headset's recorder; the next start saves the recording
    Quit(Sender<()>),
}

static VIEW: Mutex<Option<View>> = Mutex::new(None);
static ASK: OnceLock<Mutex<Sender<Ask>>> = OnceLock::new();

pub fn view() -> View {
    VIEW.lock().unwrap().clone().unwrap_or_default()
}

/// The presses of the window's Record button that a test saw
#[cfg(test)]
pub static TOGGLES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Starts a recording, or stops the one that runs. `microphone` is the setting "Microphone in
/// recordings".
pub fn toggle(microphone: bool) {
    #[cfg(test)]
    {
        let _ = microphone;
        TOGGLES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    #[cfg(not(test))]
    {
        start_thread();
        if let Some(ask) = ASK.get() {
            ask.lock().unwrap().send(Ask::Toggle { microphone }).ok();
        }
    }
}

/// Stops a recording that runs as the app ends: the headset's recorder and the sound stop, and the
/// next start of the app saves the recording. Waits up to 12 s.
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

/// Starts the thread, once. It looks at the cable and the disk every 2 s.
pub fn start_thread() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let (sender, receiver) = mpsc::channel();
        ASK.set(Mutex::new(sender)).ok();
        thread::Builder::new().name("cranvr-recorder".into()).spawn(move || run(&receiver)).ok();
    });
}

fn run(asks: &mpsc::Receiver<Ask>) {
    let mut recorder = Recorder::new(Real::default(), AppOut, movies_dir());
    recorder.look();
    loop {
        let ask = match asks.recv_timeout(LOOK_EVERY) {
            Ok(ask) => ask,
            Err(RecvTimeoutError::Timeout) => {
                recorder.look();
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        match ask {
            Ask::Toggle { microphone } => {
                let pressed = Instant::now();
                recorder.press(microphone);
                // Presses that came while a long save ran would start a recording that nobody asked for
                // now: the user presses again
                if pressed.elapsed() > LOOK_EVERY {
                    loop {
                        match asks.try_recv() {
                            Ok(Ask::Toggle { .. }) => continue,
                            Ok(Ask::Quit(done)) => {
                                recorder.quit();
                                done.send(()).ok();
                                return;
                            }
                            Err(_) => break,
                        }
                    }
                }
            }
            Ask::Quit(done) => {
                recorder.quit();
                done.send(()).ok();
                return;
            }
        }
    }
}

/// The app's state, log and notices
struct AppOut;

impl Out for AppOut {
    fn view(&self, view: &View) {
        *VIEW.lock().unwrap() = Some(view.clone());
        backend::tell_host_recording(&view.to_headset());
    }

    /// A line of the recording for the Log tab, also kept in `recordings.log` in the cranvr folder: a
    /// recording's numbers and its failures stay there after the app ends.
    fn log(&self, line: &str) {
        let stamp = local_stamp();
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(config_dir().join("recordings.log")) {
            writeln!(file, "{stamp} {line}").ok();
        }
        backend::log(format!("Recording: {line}"));
    }

    fn saved(&self, path: &Path) {
        backend::notify_saved(path);
    }
}

/// `~/Movies/cranvr`
pub fn movies_dir() -> PathBuf {
    std::env::var_os("CRANVR_RECORDINGS").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Movies/cranvr"))
}

/// The Mac and the headset as they are: adb, the host's binary and the disk
#[derive(Default)]
struct Real {
    /// Whether each device on the cable has the headset's own recorder, by its serial: a phone with
    /// cranvr's phone build has none
    has_recorder: Mutex<Vec<(String, bool)>>,
}

impl Real {
    /// The adb tool and the serials of the devices on the USB cable. The cable thread knows them. The
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

    /// The adb tool, as the cable thread found it, or found here for the app with no window
    fn tool() -> Result<PathBuf, String> {
        let tool = if crate::usb::watching() { crate::usb::status().adb } else { crate::usb::locate_adb() };
        tool.ok_or_else(|| "Recording needs adb: press Add USB support in the Connection settings".to_string())
    }

    fn adb(serial: &str, args: &[&str], limit: Duration) -> Result<String, String> {
        let tool = Self::tool()?;
        let mut all = vec!["-s", serial];
        all.extend_from_slice(args);
        crate::usb::call(&tool, &all, limit)
    }

    /// The program for the headset: in the app bundle's Resources, or in the build folder
    fn program() -> Result<PathBuf, String> {
        let bundled = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.parent()?.join("Resources/cranvr-quest-recorder.jar")));
        let built = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/quest-recorder/cranvr-quest-recorder.jar");
        [bundled, Some(built)].into_iter().flatten().find(|path| path.exists()).ok_or_else(|| "this build of cranvr has no recorder for the headset (scripts/build-quest-recorder.sh)".into())
    }

    /// A test makes the cable look pulled while the file `CRANVR_TEST_CABLE_OUT` names exists
    fn cable_out_in_test() -> bool {
        std::env::var_os("CRANVR_TEST_CABLE_OUT").is_some_and(|path| Path::new(&path).exists())
    }
}

impl Device for Real {
    fn unready(&self) -> Option<String> {
        Self::program().err()
    }

    fn headsets(&self) -> Result<Vec<String>, String> {
        let (adb, serials) = Self::cabled()?;
        if Self::cable_out_in_test() {
            return Ok(Vec::new());
        }
        let mut found = Vec::new();
        for serial in serials {
            let known = self.has_recorder.lock().unwrap().iter().find(|(seen, _)| *seen == serial).map(|(_, has)| *has);
            let has = match known {
                Some(has) => has,
                None => {
                    let Ok(path) = crate::usb::call(&adb, &["-s", &serial, "shell", "pm", "path", RECORDER_APP], ADB_CALL) else { continue };
                    let has = path.contains("package:");
                    self.has_recorder.lock().unwrap().push((serial.clone(), has));
                    has
                }
            };
            if has {
                found.push(serial);
            }
        }
        Ok(found)
    }

    fn prepare(&self, serial: &str) -> Result<(), String> {
        Self::adb(serial, &["push", &Self::program()?.display().to_string(), ON_HEADSET], ADB_CALL).map(drop)
    }

    fn videos(&self, serial: &str) -> Result<Vec<Video>, String> {
        // A folder that does not exist yet holds no videos: the headset has recorded nothing yet. The
        // first line holds the headset's time, each other line a video's size, the time of its last
        // write, its first 64 bytes in hex and its name.
        let line = format!("date +now%s; cd {VIDEOS} 2> /dev/null && for f in *.mp4; do [ -e \"$f\" ] && echo \"$(stat -c '%s %Y' \"$f\") x$(head -c 64 \"$f\" | od -An -tx1 | tr -d ' \\n') $f\"; done; echo end");
        let listed = Self::adb(serial, &["shell", &line], ADB_CALL)?;
        if !listed.lines().any(|line| line.trim() == "end") {
            return Err(format!("the headset's list of videos ended early: {}", listed.trim()));
        }
        let now = listed.lines().find_map(|line| line.trim().strip_prefix("now")?.parse::<u64>().ok()).ok_or("the headset's list of videos has no time")?;
        Ok(listed.lines().filter_map(|line| parse_video(line.trim(), now)).collect())
    }

    /// Runs the program on the headset: `start` or `stop`
    fn recorder(&self, serial: &str, verb: &str) -> Result<(), String> {
        let line = format!("CLASSPATH={ON_HEADSET} app_process / dev.cranvr.recorder.Main {verb}");
        let answer = Self::adb(serial, &["shell", &line], ADB_CALL).map_err(|e| format!("adb: {e}"))?;
        let answer = answer.trim();
        match answer.strip_prefix("ok") {
            Some(_) => Ok(()),
            None => Err(answer.strip_prefix("error ").unwrap_or(answer).to_string()),
        }
    }

    fn pull(&self, serial: &str, video: &Video, to: &Path) -> Result<(), String> {
        let limit = ADB_CALL * (1 + u32::try_from(video.bytes / 10_000_000).unwrap_or(u32::MAX));
        Self::adb(serial, &["pull", &format!("{VIDEOS}/{}", video.name), &to.display().to_string()], limit).map(drop)
    }

    fn remove(&self, serial: &str, name: &str) -> Result<(), String> {
        // Only a plain name of the recorder's folder: never a path elsewhere
        if name.contains('/') || !name.ends_with(".mp4") {
            return Err(format!("{name} is no video of the headset's recorder"));
        }
        Self::adb(serial, &["shell", "rm", &format!("{VIDEOS}/{name}")], ADB_CALL).map(drop)
    }

    /// The free disk where the final files go. A test names a file that holds the number instead
    /// (`CRANVR_TEST_FREE_DISK`).
    fn free_bytes(&self, dir: &Path) -> Option<u64> {
        if let Some(path) = std::env::var_os("CRANVR_TEST_FREE_DISK") {
            return std::fs::read_to_string(path).ok()?.trim().parse().ok();
        }
        let existing = dir.ancestors().find(|dir| dir.exists())?;
        let c_path = std::ffi::CString::new(existing.as_os_str().as_encoded_bytes()).ok()?;
        let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: a plain query of the file system that holds `c_path`
        (unsafe { libc::statvfs(c_path.as_ptr(), &mut stats) } == 0).then(|| u64::from(stats.f_bavail) * stats.f_frsize)
    }

    fn start_sound(&self, dir: &Path, microphone: bool) -> Result<Box<dyn Sound>, String> {
        let mut command = Command::new(backend::host_binary()?);
        command.arg("--record-sound").arg(dir);
        if microphone {
            command.arg("--microphone").arg(config_dir().join("microphone.pcm"));
        }
        let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|e| e.to_string())?;
        let messages = Arc::new(Mutex::new(Vec::new()));
        if let Some(out) = child.stdout.take() {
            let messages = Arc::clone(&messages);
            thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    if let Some(message) = line.strip_prefix("@cranvr ").and_then(|text| serde_json::from_str::<Value>(text).ok()).and_then(|v| v["message"].as_str().map(String::from)) {
                        messages.lock().unwrap().push(message);
                    }
                }
            });
        }
        Ok(Box::new(SoundProcess { child, messages }))
    }

    fn make_final(&self, video: &Path, dir: &Path, out: &Path) -> Result<Value, String> {
        let mux = Command::new(backend::host_binary()?).arg("--mux-recording").args([video, dir, out]).stderr(Stdio::null()).output().map_err(|e| format!("the final file: {e}"))?;
        let line = String::from_utf8_lossy(&mux.stdout).lines().filter_map(|line| line.strip_prefix("@cranvr ")).filter_map(|text| serde_json::from_str::<Value>(text).ok()).find(|v| v["type"] == "recording_file").unwrap_or(Value::Null);
        if line["ok"] != true {
            return Err(line["message"].as_str().unwrap_or("the final file was not made").to_string());
        }
        Ok(line["numbers"].clone())
    }

    fn wait(&self, time: Duration) {
        thread::sleep(time);
    }
}

/// The process that records the Mac's sound (`cranvr-host --record-sound`)
struct SoundProcess {
    child: Child,
    messages: Arc<Mutex<Vec<String>>>,
}

impl Sound for SoundProcess {
    fn mark_start(&mut self) {
        if let Some(input) = self.child.stdin.as_mut() {
            input.write_all(b"start\n").and_then(|()| input.flush()).ok();
        }
    }

    fn end(&mut self, limit: Duration) {
        // The end of its input ends the sound's recording, which then writes its files
        drop(self.child.stdin.take());
        let started = Instant::now();
        while started.elapsed() < limit {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        self.child.kill().ok();
        self.child.wait().ok();
    }

    fn messages(&mut self) -> Vec<String> {
        std::mem::take(&mut *self.messages.lock().unwrap())
    }
}

#[cfg(test)]
#[path = "recorder_tests.rs"]
mod tests;
