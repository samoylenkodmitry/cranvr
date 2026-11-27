//! The sound of a recording of what the headset shows, in a process of its own that the cranvr app
//! starts beside the host: `cranvr-host --record-sound DIR [--microphone RING]`.
//!
//! The process captures this Mac's sound with a Core Audio tap, as the host does for the headset,
//! into `DIR/sound.wav` (or a test's made-up sound, `test_sound.rs`). With `--microphone` it also reads the headset's microphone from the host's
//! ring (`microphone.pcm`, see headset/native/microphone.rs) into `DIR/microphone.wav`. Its reads
//! stamp the ring as a game's do, so the host asks the headset for its microphone while the recording
//! runs. A line `start` on the input marks the moment at which the headset's recorder answered that it
//! started. The end of the input ends the recording: the app closes it, and so does the app's own end,
//! even a crash. `DIR/sound.json` holds the times of the first frame of each file and of the mark,
//! on `mach_continuous_time` in ns, for the final file (recording_mux.rs). The process writes it anew
//! as each time comes, so a recording that a kill cut short still has its times. At the end it adds
//! what the tap did: its starts, the gaps and the silence that filled them.
//!
//! A pause of the tap, such as a change of the output device, leaves a gap. The writer fills each
//! gap with silence, so the file keeps the Mac's time.

use crate::{game_audio, headset::native::microphone::continuous_ns, wav::WavWriter};
use serde_json::json;
use std::{
    io::BufRead,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::Duration,
};

const RATE: f64 = crate::wav::SAMPLE_RATE as f64;
/// A gap longer than this between two buffers of the tap becomes silence in the file
const GAP: Duration = Duration::from_millis(20);
/// How often the microphone's reader reads the ring
const MIC_EVERY: Duration = Duration::from_millis(10);

/// The times that the final file needs, on `mach_continuous_time` in ns, and what the tap did
#[derive(Clone, Copy, Debug, Default)]
struct Times {
    sound_first: Option<u64>,
    microphone_first: Option<u64>,
    start_mark: Option<u64>,
    /// When the process started
    began: u64,
    /// The starts of the tap: more than one means the output device changed
    tap_starts: u32,
    /// The gaps between two buffers of the tap that the writer filled, and their length in frames
    gaps: u32,
    filled_frames: u64,
}

impl Times {
    fn json(&self, ended: bool) -> serde_json::Value {
        json!({
            "sound_first_ns": self.sound_first,
            "microphone_first_ns": self.microphone_first,
            "start_mark_ns": self.start_mark,
            "first_sound_after_ms": self.sound_first.map(|first| first.saturating_sub(self.began) as f64 / 1e6),
            "tap_starts": self.tap_starts,
            "gaps": self.gaps,
            "filled_s": self.filled_frames as f64 / RATE,
            "ended": ended,
        })
    }
}

/// Writes `DIR/sound.json` whole or not at all: a kill between two writes keeps the last one.
fn write_times(dir: &Path, times: &Times, ended: bool) {
    let part = dir.join("sound.json.part");
    if std::fs::write(&part, times.json(ended).to_string()).is_ok() {
        std::fs::rename(&part, dir.join("sound.json")).ok();
    }
}

/// A buffer of the tap with the time of its arrival on `mach_continuous_time`
type Buffer = (u64, Vec<u8>);

static TAP: OnceLock<Mutex<Option<Sender<Buffer>>>> = OnceLock::new();

/// The tap's sink: the buffer with the time of its arrival, to the writer's thread
fn to_writer(pcm: &[u8]) {
    let now = continuous_ns();
    if let Some(sender) = TAP.get().and_then(|tap| tap.lock().unwrap().clone()) {
        sender.send((now, pcm.to_vec())).ok();
    }
}

/// Runs the recording of the sound until the input ends. Returns an error when no file could start.
pub fn run(dir: &Path, microphone: Option<&Path>) -> Result<(), String> {
    let times = Arc::new(Mutex::new(Times { began: continuous_ns(), ..Default::default() }));
    let mut sound = WavWriter::create(&dir.join("sound.wav"), 2).map_err(|e| format!("the sound's file: {e}"))?;
    write_times(dir, &times.lock().unwrap(), false);
    let (sender, receiver) = mpsc::channel::<(u64, Vec<u8>)>();
    *TAP.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(sender);
    let writer = {
        let (times, dir) = (Arc::clone(&times), dir.to_path_buf());
        thread::spawn(move || {
            let mut start: Option<u64> = None;
            let mut written: u64 = 0;
            for (arrived, pcm) in receiver {
                let frames = (pcm.len() / 4) as u64;
                let first = arrived.saturating_sub(nanos(frames));
                let start = *start.get_or_insert_with(|| {
                    let mut times = times.lock().unwrap();
                    times.sound_first = Some(first);
                    write_times(&dir, &times, false);
                    first
                });
                let expected = start + nanos(written);
                if first > expected + GAP.as_nanos() as u64 {
                    let silence = ((first - expected) as f64 * 1e-9 * RATE) as u64;
                    sound.write_samples(&vec![0; silence as usize * 2]).ok();
                    written += silence;
                    let mut times = times.lock().unwrap();
                    times.gaps += 1;
                    times.filled_frames += silence;
                }
                sound.write_bytes(&pcm).ok();
                written += frames;
            }
            sound.finish().ok();
        })
    };
    let ending = Arc::new(AtomicBool::new(false));
    let tap = {
        let (ending, times) = (Arc::clone(&ending), Arc::clone(&times));
        thread::spawn(move || run_tap(&ending, &times))
    };
    let reader = microphone.map(|ring| {
        let (ring, times, ending, dir) = (ring.to_path_buf(), Arc::clone(&times), Arc::clone(&ending), dir.to_path_buf());
        thread::spawn(move || read_microphone(&ring, &dir, &times, &ending))
    });
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        if line.trim() == "start" {
            let mut times = times.lock().unwrap();
            times.start_mark = Some(continuous_ns());
            write_times(dir, &times, false);
        }
    }
    ending.store(true, Ordering::Relaxed);
    tap.join().ok();
    // The writer ends once the tap's sender is gone
    TAP.get().map(|tap| tap.lock().unwrap().take());
    writer.join().ok();
    if let Some(reader) = reader {
        reader.join().ok();
    }
    let times = *times.lock().unwrap();
    write_times(dir, &times, true);
    crate::status::emit(json!({"type": "recording_sound", "done": true, "times": times.json(true)}));
    Ok(())
}

/// The time of `frames` frames, in ns
fn nanos(frames: u64) -> u64 {
    (frames as f64 / RATE * 1e9) as u64
}

/// Runs the tap until the recording ends, and starts it again when the output device changes.
fn run_tap(ending: &AtomicBool, times: &Mutex<Times>) {
    if crate::test_sound::configured().is_some() {
        times.lock().unwrap().tap_starts += 1;
        if let Err(e) = crate::test_sound::run(to_writer, ending) {
            crate::status::emit(json!({"type": "recording_sound", "ok": false, "message": e}));
        }
        return;
    }
    if cranvr_common::system_audio::access() != cranvr_common::system_audio::Access::Granted {
        crate::status::emit(json!({"type": "recording_sound", "ok": false, "message": "cranvr may not record this Mac's sound: allow System Audio Recording"}));
        return;
    }
    let (changed, change) = mpsc::channel();
    game_audio::watch_default_output(changed.clone());
    while !ending.load(Ordering::Relaxed) {
        let capture = match game_audio::Capture::start(changed.clone(), to_writer) {
            Ok(capture) => {
                times.lock().unwrap().tap_starts += 1;
                capture
            }
            Err(e) => {
                crate::status::emit(json!({"type": "recording_sound", "ok": false, "message": e}));
                return;
            }
        };
        while !ending.load(Ordering::Relaxed) {
            if change.recv_timeout(Duration::from_millis(100)).is_ok() {
                break;
            }
        }
        drop(capture);
    }
}

/// Reads the headset's microphone from the host's ring into `DIR/microphone.wav` until the
/// recording ends.
fn read_microphone(ring: &Path, dir: &Path, times: &Mutex<Times>, ending: &AtomicBool) {
    let path = &dir.join("microphone.wav");
    let mut reader = match crate::headset::native::microphone::RingReader::open(ring) {
        Ok(reader) => reader,
        Err(e) => {
            crate::status::emit(json!({"type": "recording_sound", "ok": false, "message": format!("the headset's microphone: {e}")}));
            return;
        }
    };
    let Ok(mut file) = WavWriter::create(path, 1) else { return };
    let mut frames = Vec::with_capacity(4800);
    while !ending.load(Ordering::Relaxed) {
        frames.clear();
        reader.read(&mut frames);
        if !frames.is_empty() {
            let first = continuous_ns().saturating_sub(nanos(frames.len() as u64));
            let mut times = times.lock().unwrap();
            if times.microphone_first.is_none() {
                times.microphone_first = Some(first);
                write_times(dir, &times, false);
            }
            drop(times);
            file.write_samples(&frames).ok();
        }
        thread::sleep(MIC_EVERY);
    }
    file.finish().ok();
}
