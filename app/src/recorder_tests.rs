use super::{local_stamp, mp4_writing, parse_video, Device, Journal, Out, Phase, Recorder, Sound, Video, View, CABLE_OUT, JOURNAL, OUT_LOOKS};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const SERIAL: &str = "2G0YC5ZH2000XX";

// The headset's recorder as the tests play it: the file of a recording grows at each look
#[derive(Default)]
struct Quest {
    on_cable: bool,
    files: Vec<Video>,
    /// The file that the recorder writes now
    recording: Option<String>,
    starts: u32,
    stops: u32,
    removed: Vec<String>,
    /// A start writes on into this file of an earlier name
    append_to: Option<String>,
    /// A pull gives fewer bytes than the file has
    short_pull: bool,
    /// The final file fails
    final_fails: bool,
    free: u64,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<Quest>>);

impl Fake {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Quest { on_cable: true, free: 100_000_000_000, ..Default::default() })))
    }

    fn quest(&self) -> std::sync::MutexGuard<'_, Quest> {
        self.0.lock().unwrap()
    }
}

struct Silent;

impl Sound for Silent {
    fn mark_start(&mut self) {}
    fn end(&mut self, _: Duration) {}
    fn messages(&mut self) -> Vec<String> {
        Vec::new()
    }
}

impl Device for Fake {
    fn unready(&self) -> Option<String> {
        None
    }

    fn headsets(&self) -> Result<Vec<String>, String> {
        Ok(if self.quest().on_cable { vec![SERIAL.to_string()] } else { Vec::new() })
    }

    fn prepare(&self, _: &str) -> Result<(), String> {
        Ok(())
    }

    fn videos(&self, _: &str) -> Result<Vec<Video>, String> {
        let mut quest = self.quest();
        if !quest.on_cable {
            return Err("no device".into());
        }
        let recording = quest.recording.clone();
        for video in &mut quest.files {
            video.writing = recording.as_ref() == Some(&video.name);
            if video.writing {
                video.bytes += 1000;
            }
        }
        Ok(quest.files.clone())
    }

    fn recorder(&self, _: &str, verb: &str) -> Result<(), String> {
        let mut quest = self.quest();
        if !quest.on_cable {
            return Err("no device".into());
        }
        match verb {
            "start" if quest.recording.is_none() => {
                quest.starts += 1;
                let name = quest.append_to.clone().unwrap_or_else(|| format!("dev.cranvr.client-2026112{}-0.mp4", quest.starts));
                if !quest.files.iter().any(|video| video.name == name) {
                    quest.files.push(Video { name: name.clone(), bytes: 0, writing: true, age_s: 0 });
                }
                quest.recording = Some(name);
            }
            "stop" => {
                quest.stops += 1;
                quest.recording = None;
            }
            _ => {}
        }
        Ok(())
    }

    fn pull(&self, _: &str, video: &Video, to: &Path) -> Result<(), String> {
        let short = self.quest().short_pull;
        std::fs::write(to, vec![7u8; (video.bytes - u64::from(short)) as usize]).map_err(|e| e.to_string())
    }

    fn remove(&self, _: &str, name: &str) -> Result<(), String> {
        let mut quest = self.quest();
        quest.files.retain(|video| video.name != name);
        quest.removed.push(name.to_string());
        Ok(())
    }

    fn free_bytes(&self, _: &Path) -> Option<u64> {
        Some(self.quest().free)
    }

    fn start_sound(&self, dir: &Path, _: bool) -> Result<Box<dyn Sound>, String> {
        std::fs::write(dir.join("sound.wav"), b"RIFF").map_err(|e| e.to_string())?;
        Ok(Box::new(Silent))
    }

    fn make_final(&self, video: &Path, _: &Path, out: &Path) -> Result<Value, String> {
        if self.quest().final_fails {
            return Err("AVFoundation failed".into());
        }
        std::fs::copy(video, out).map_err(|e| e.to_string())?;
        Ok(json!({"checked": true, "sound": {"source": "mac"}}))
    }

    fn wait(&self, _: Duration) {}
}

#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<String>>>);

impl Lines {
    fn any(&self, part: &str) -> bool {
        self.0.lock().unwrap().iter().any(|line| line.contains(part))
    }
}

impl Out for Lines {
    fn view(&self, _: &View) {}
    fn log(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_string());
    }
    fn saved(&self, path: &Path) {
        self.log(&format!("notice for {}", path.display()));
    }
}

struct Movies(PathBuf);

impl Movies {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("cranvr-recorder-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// The final files and the folders of parts
    fn listed(&self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let all: Vec<PathBuf> = std::fs::read_dir(&self.0).unwrap().flatten().map(|entry| entry.path()).collect();
        all.into_iter().partition(|path| path.is_file())
    }
}

impl Drop for Movies {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn recorder(fake: &Fake, movies: &Movies) -> (Recorder<Fake, Lines>, Lines) {
    let lines = Lines::default();
    (Recorder::new(fake.clone(), lines.clone(), movies.0.clone()), lines)
}

// The gem's two presses: the headset's recorder starts, then stops; the final file comes to the Mac
// and the headset's copy goes, since the final file passed its check
#[test]
fn a_press_starts_a_recording_and_the_next_press_saves_it() {
    let (fake, movies) = (Fake::new(), Movies::new("press"));
    let (mut recorder, lines) = recorder(&fake, &movies);
    recorder.press(false);
    assert!(recorder.view().running(), "{:?}", lines.0);
    assert_eq!(fake.quest().starts, 1);
    recorder.press(false);
    let (files, parts) = movies.listed();
    assert_eq!(files.len(), 1, "{:?}", lines.0);
    assert!(files[0].file_name().unwrap().to_string_lossy().starts_with("cranvr-"));
    assert!(parts.is_empty(), "no parts stay: {parts:?}");
    assert_eq!(fake.quest().removed.len(), 1, "the headset's copy is deleted");
    assert!(fake.quest().files.is_empty());
    assert!(recorder.view().note.as_deref().is_some_and(|note| note.starts_with("Saved: ")));
    assert!(lines.any("is deleted: the Mac's file passed its check") && lines.any("notice for"));
}

// The owner's press of 13:18:54 came while the headset's recorder still recorded: now it refuses and
// says why, and nothing starts
#[test]
fn a_start_while_the_headsets_recorder_records_refuses() {
    let (fake, movies) = (Fake::new(), Movies::new("busy"));
    fake.quest().files.push(Video { name: "dev.cranvr.client-20261127-131219-0.mp4".into(), bytes: 5000, writing: true, age_s: 0 });
    fake.quest().recording = Some("dev.cranvr.client-20261127-131219-0.mp4".into());
    let (mut recorder, lines) = recorder(&fake, &movies);
    recorder.press(false);
    assert!(!recorder.view().running());
    assert_eq!(fake.quest().starts, 0);
    assert!(lines.any("records already"), "{:?}", lines.0);
    assert!(fake.quest().removed.is_empty(), "a file that the app did not record stays");
}

// A kill of the app leaves the headset's recorder running. The next start of the app finds the
// journal, stops the recorder and saves the recording.
#[test]
fn after_a_kill_the_next_start_stops_the_recorder_and_saves_the_recording() {
    let (fake, movies) = (Fake::new(), Movies::new("kill"));
    let (mut first, _) = recorder(&fake, &movies);
    first.press(false);
    // A kill: the app ends with no word to the headset
    drop(first);
    assert!(fake.quest().recording.is_some(), "the headset records on");
    let (mut next, lines) = recorder(&fake, &movies);
    next.look();
    assert!(fake.quest().recording.is_none(), "the next start stopped the recorder");
    assert!(lines.any("still ran, and it stopped now"), "{:?}", lines.0);
    let (files, parts) = movies.listed();
    assert_eq!((files.len(), parts.len()), (1, 0), "{:?}", lines.0);
    assert_eq!(fake.quest().removed.len(), 1);
}

// SIGTERM, as at an update of the app: the headset's recorder stops at once and the parts stay. The
// next start saves the recording.
#[test]
fn a_quit_stops_the_recorder_and_the_next_start_saves_the_recording() {
    let (fake, movies) = (Fake::new(), Movies::new("quit"));
    let (mut first, lines) = recorder(&fake, &movies);
    first.press(false);
    first.quit();
    assert!(fake.quest().recording.is_none(), "the quit stopped the recorder");
    assert!(lines.any("the next start of cranvr saves"), "{:?}", lines.0);
    let (files, parts) = movies.listed();
    assert_eq!((files.len(), parts.len()), (0, 1), "the parts wait");
    let journal = Journal::read(&parts[0]).expect("a journal");
    assert_eq!(journal.phase, Phase::Stopped);
    assert!(parts[0].join("sound.wav").exists(), "the Mac's sound stays");
    let (mut next, lines) = recorder(&fake, &movies);
    next.look();
    let (files, parts) = movies.listed();
    assert_eq!((files.len(), parts.len()), (1, 0), "{:?}", lines.0);
    assert!(lines.any("saved "), "{:?}", lines.0);
}

// The recorder may write on into a file of an earlier name: a file that grew during the recording
// counts as the recording's file
#[test]
fn a_file_of_an_earlier_name_that_grew_counts() {
    let (fake, movies) = (Fake::new(), Movies::new("grew"));
    let old = "dev.cranvr.client-20261127-120000-0.mp4";
    fake.quest().files.push(Video { name: old.into(), bytes: 5000, writing: false, age_s: 600 });
    fake.quest().append_to = Some(old.into());
    let (mut recorder, lines) = recorder(&fake, &movies);
    recorder.press(false);
    assert!(recorder.view().running(), "{:?}", lines.0);
    recorder.press(false);
    let (files, _) = movies.listed();
    assert_eq!(files.len(), 1, "{:?}", lines.0);
    assert!(std::fs::metadata(&files[0]).unwrap().len() > 5000, "the whole grown file came");
}

// The headset keeps its copy until the Mac's file is safe: a short pull saves nothing and deletes
// nothing, and a later look tries again
#[test]
fn the_headset_keeps_its_copy_until_the_macs_file_passes_its_check() {
    let (fake, movies) = (Fake::new(), Movies::new("short"));
    fake.quest().short_pull = true;
    let (mut recorder, lines) = recorder(&fake, &movies);
    recorder.press(false);
    recorder.press(false);
    assert!(fake.quest().removed.is_empty(), "{:?}", lines.0);
    assert_eq!(fake.quest().files.len(), 1);
    assert!(lines.any("is not saved yet"), "{:?}", lines.0);
    let (files, parts) = movies.listed();
    assert_eq!((files.len(), parts.len()), (0, 1));
    assert_eq!(Journal::read(&parts[0]).unwrap().attempts, 1);
    fake.quest().short_pull = false;
    recorder.look();
    let (files, parts) = movies.listed();
    assert_eq!((files.len(), parts.len()), (1, 0), "{:?}", lines.0);
    assert_eq!(fake.quest().removed.len(), 1);
}

// A final file that cannot be made leaves the headset's own file as the recording, with its own sound.
// Nothing checked that copy, so the headset keeps its own.
#[test]
fn a_final_file_that_fails_keeps_the_headsets_file_as_the_recording() {
    let (fake, movies) = (Fake::new(), Movies::new("fails"));
    fake.quest().final_fails = true;
    let (mut recorder, lines) = recorder(&fake, &movies);
    recorder.press(false);
    recorder.press(false);
    let (files, parts) = movies.listed();
    assert_eq!((files.len(), parts.len()), (1, 0), "{:?}", lines.0);
    assert!(lines.any("the Mac's sound could not be added: AVFoundation failed"), "{:?}", lines.0);
    assert!(fake.quest().removed.is_empty(), "the headset keeps its copy");
    assert!(lines.any("the headset keeps its copy"), "{:?}", lines.0);
}

// A cable that comes out during a recording: the headset records on, a press says why it cannot stop,
// and the return of the cable saves what was recorded
#[test]
fn a_cable_pull_during_a_recording_saves_it_when_the_cable_is_back() {
    let (fake, movies) = (Fake::new(), Movies::new("cable"));
    let (mut recorder, lines) = recorder(&fake, &movies);
    recorder.press(false);
    fake.quest().on_cable = false;
    for _ in 0..OUT_LOOKS {
        recorder.look();
    }
    assert!(recorder.view().running());
    assert_eq!(recorder.view().note.as_deref(), Some(CABLE_OUT));
    let notes = recorder.view().note_id;
    recorder.press(false);
    assert_eq!((recorder.view().note.as_deref(), recorder.view().note_id), (Some(CABLE_OUT), notes + 1), "a press says it again");
    assert!(fake.quest().recording.is_some(), "the headset records on");
    fake.quest().on_cable = true;
    recorder.look();
    assert!(!recorder.view().running());
    assert_eq!(fake.quest().stops, 1);
    let (files, _) = movies.listed();
    assert_eq!(files.len(), 1, "{:?}", lines.0);
}

// A change of the headset's USB functions hides it from adb for a moment: that is no pull of the cable
#[test]
fn a_short_drop_of_the_cable_stops_nothing() {
    let (fake, movies) = (Fake::new(), Movies::new("drop"));
    let (mut recorder, _) = recorder(&fake, &movies);
    recorder.press(false);
    fake.quest().on_cable = false;
    for _ in 1..OUT_LOOKS {
        recorder.look();
    }
    fake.quest().on_cable = true;
    recorder.look();
    assert!(recorder.view().running());
    assert_eq!(fake.quest().stops, 0);
}

// Under 5 GB of free disk on the Mac no recording starts, and one that runs stops and is saved
#[test]
fn under_5_gb_of_free_disk_a_recording_stops_by_itself() {
    let (fake, movies) = (Fake::new(), Movies::new("disk"));
    let (mut recorder, lines) = recorder(&fake, &movies);
    fake.quest().free = 4_200_000_000;
    recorder.press(false);
    assert!(!recorder.view().running());
    assert!(lines.any("The Mac has 4.2 GB free: a recording needs 5 GB"), "{:?}", lines.0);
    fake.quest().free = 50_000_000_000;
    recorder.look();
    assert_eq!(recorder.view().blocked, None);
    recorder.press(false);
    assert!(recorder.view().running());
    fake.quest().free = 4_900_000_000;
    recorder.look();
    assert!(!recorder.view().running());
    assert!(lines.any("The Mac has only 4.9 GB free: the recording stops"), "{:?}", lines.0);
    let (files, _) = movies.listed();
    assert_eq!(files.len(), 1);
}

// A recording of another headset waits for that headset: this one's start touches nothing of it
#[test]
fn a_recording_of_another_headset_waits_for_it() {
    let (fake, movies) = (Fake::new(), Movies::new("other"));
    let dir = movies.0.join(".cranvr-2026-11-27-120000");
    std::fs::create_dir_all(&dir).unwrap();
    let journal = Journal { name: "cranvr-2026-11-27-120000.mp4".into(), serial: "OTHER".into(), before: Vec::new(), video: None, phase: Phase::Stopped, attempts: 0 };
    journal.write(&dir);
    let (mut recorder, _) = recorder(&fake, &movies);
    recorder.look();
    assert!(dir.join(JOURNAL).exists());
    assert!(fake.quest().removed.is_empty());
}

// The headset's gem learns what a press does: start, stop, or nothing with the reason
#[test]
fn the_headset_hears_the_reason_only_while_a_press_cannot_start_or_stop() {
    let blocked = View { blocked: Some("Recording needs the headset on the USB cable with USB debugging".into()), ..Default::default() };
    assert_eq!(blocked.to_headset()["running"], false);
    assert_eq!(blocked.to_headset()["blocked"], "Recording needs the headset on the USB cable with USB debugging");
    // A running recording always stops at a press, whatever the cable says now
    let running = View { since: Some(Instant::now()), ..blocked.clone() };
    assert_eq!(running.to_headset()["running"], true);
    assert!(running.to_headset()["blocked"].is_null());
    let saving = View { saving: true, ..Default::default() };
    assert_eq!(saving.to_headset()["blocked"], "Saving the last recording");
    let noted = View { note: Some("Saved: cranvr-2026-11-27-101500.mp4".into()), note_id: 3, ..Default::default() };
    assert_eq!(noted.to_headset()["note"], "Saved: cranvr-2026-11-27-101500.mp4");
    assert_eq!(noted.to_headset()["note_id"], 3);
}

// The final file's name holds the local time of the start: cranvr-YYYY-MM-DD-HHMMSS.mp4
#[test]
fn the_stamp_is_the_local_date_and_time() {
    let stamp = local_stamp();
    assert_eq!(stamp.len(), 17, "{stamp}");
    let parts: Vec<&str> = stamp.split('-').collect();
    assert_eq!(parts.iter().map(|part| part.len()).collect::<Vec<_>>(), vec![4, 2, 2, 6], "{stamp}");
    assert!(stamp.chars().all(|c| c.is_ascii_digit() || c == '-'), "{stamp}");
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len() / 2).map(|at| u8::from_str_radix(&text[at * 2..at * 2 + 2], 16).unwrap()).collect()
}

// The first 64 bytes of the headset's own files on 2026-11-27: one that the recorder still wrote
// (2.6 MB, 4 s in), and the owner's whole recording of 13:12 (697 MB)
const WRITTEN: &str = "0000001c667479706d703431000002006d70343169736f366d70343100000008667265650000002466726565";
const WHOLE: &str = "0000001c667479706d703431000002006d70343169736f366d70343100000008667265652981aeea6d6461740000001c667479706d703431000002006d703431";

#[test]
fn the_header_says_whether_the_headsets_recorder_still_writes_a_file() {
    assert!(mp4_writing(&hex(WRITTEN)), "ftyp and free boxes alone: the recorder writes on");
    assert!(!mp4_writing(&hex(WHOLE)), "an mdat of a known length: a whole file");
    assert!(mp4_writing(&[]), "a file that the recorder just made");
    // A file that the recorder did not make, such as a copied video with its moov first
    assert!(!mp4_writing(&hex("0000001c667479706d703431000002006d70343169736f366d7034310000ffff6d6f6f76")));
    assert!(!mp4_writing(b"not a video at all"));
    let line = format!("2621512 1795786000 x{WRITTEN} dev.cranvr.client-20261127-141739-0.mp4");
    assert_eq!(parse_video(&line, 1_795_786_003), Some(Video { name: "dev.cranvr.client-20261127-141739-0.mp4".into(), bytes: 2_621_512, writing: true, age_s: 3 }));
    assert_eq!(parse_video(&format!("696810515 1795780000 x{WHOLE} a.mp4"), 1_795_786_003).map(|video| video.writing), Some(false));
    // A file that a dead recorder never finished stops counting as written once it is old
    assert_eq!(parse_video(&line, 1_795_786_000 + 120).map(|video| video.writing), Some(false));
}
