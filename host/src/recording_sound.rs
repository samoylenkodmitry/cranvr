//! The sound of a recording of what the headset shows, in a process of its own that the cranvr app
//! starts beside the host: `cranvr-host --record-sound DIR [--microphone RING]`.
//!
//! The process captures this Mac's sound with a Core Audio tap, as the host does for the headset,
//! into `DIR/sound.wav`. With `--microphone` it also reads the headset's microphone from the host's
//! ring (`microphone.pcm`, see headset/native/microphone.rs) into `DIR/microphone.wav`. Its reads
//! stamp the ring as a game's do, so the host asks the headset for its microphone while the recording
//! runs. A line `start` on the input marks the moment at which the headset's recorder answered that it
//! started. The end of the input ends the recording. `DIR/sound.json` then holds the times of the
//! first frame of each file and of the mark, on `mach_continuous_time` in ns, for the final file
//! (recording_mux.rs).
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

/// The times that the final file needs, on `mach_continuous_time` in ns
#[derive(Clone, Copy, Debug, Default)]
struct Times {
    sound_first: Option<u64>,
    microphone_first: Option<u64>,
    start_mark: Option<u64>,
}

static TAP: OnceLock<Mutex<Option<Sender<(u64, Vec<u8>)>>>> = OnceLock::new();

/// The tap's sink: the buffer with the time of its arrival, to the writer's thread
fn to_writer(pcm: &[u8]) {
    let now = continuous_ns();
    if let Some(sender) = TAP.get().and_then(|tap| tap.lock().unwrap().clone()) {
        sender.send((now, pcm.to_vec())).ok();
    }
}

/// Runs the recording of the sound until the input ends. Returns an error when no file could start.
pub fn run(dir: &Path, microphone: Option<&Path>) -> Result<(), String> {
    let times = Arc::new(Mutex::new(Times::default()));
    let mut sound = WavWriter::create(&dir.join("sound.wav"), 2).map_err(|e| format!("the sound's file: {e}"))?;
    let (sender, receiver) = mpsc::channel::<(u64, Vec<u8>)>();
    *TAP.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(sender);
    let writer = {
        let times = Arc::clone(&times);
        thread::spawn(move || {
            let mut start: Option<u64> = None;
            let mut written: u64 = 0;
            for (arrived, pcm) in receiver {
                let frames = (pcm.len() / 4) as u64;
                let first = arrived.saturating_sub(nanos(frames));
                let start = *start.get_or_insert_with(|| {
                    times.lock().unwrap().sound_first = Some(first);
                    first
                });
                let expected = start + nanos(written);
                if first > expected + GAP.as_nanos() as u64 {
                    let silence = ((first - expected) as f64 * 1e-9 * RATE) as u64;
                    sound.write_samples(&vec![0; silence as usize * 2]).ok();
                    written += silence;
                }
                sound.write_bytes(&pcm).ok();
                written += frames;
            }
            sound.finish().ok();
        })
    };
    let ending = Arc::new(AtomicBool::new(false));
    let tap = {
        let ending = Arc::clone(&ending);
        thread::spawn(move || run_tap(&ending))
    };
    let reader = microphone.map(|ring| {
        let (ring, times, ending, path) = (ring.to_path_buf(), Arc::clone(&times), Arc::clone(&ending), dir.join("microphone.wav"));
        thread::spawn(move || read_microphone(&ring, &path, &times, &ending))
    });
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        if line.trim() == "start" {
            times.lock().unwrap().start_mark = Some(continuous_ns());
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
    let summary = json!({
        "sound_first_ns": times.sound_first,
        "microphone_first_ns": times.microphone_first,
        "start_mark_ns": times.start_mark,
    });
    std::fs::write(dir.join("sound.json"), summary.to_string()).map_err(|e| format!("sound.json: {e}"))?;
    crate::status::emit(json!({"type": "recording_sound", "done": true, "times": summary}));
    Ok(())
}

/// The time of `frames` frames, in ns
fn nanos(frames: u64) -> u64 {
    (frames as f64 / RATE * 1e9) as u64
}

/// Runs the tap until the recording ends, and starts it again when the output device changes.
fn run_tap(ending: &AtomicBool) {
    if cranvr_common::system_audio::access() != cranvr_common::system_audio::Access::Granted {
        crate::status::emit(json!({"type": "recording_sound", "ok": false, "message": "cranvr may not record this Mac's sound: allow System Audio Recording"}));
        return;
    }
    let (changed, change) = mpsc::channel();
    game_audio::watch_default_output(changed.clone());
    while !ending.load(Ordering::Relaxed) {
        let capture = match game_audio::Capture::start(changed.clone(), to_writer) {
            Ok(capture) => capture,
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

/// Reads the headset's microphone from the host's ring into `path` until the recording ends.
fn read_microphone(ring: &Path, path: &Path, times: &Mutex<Times>, ending: &AtomicBool) {
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
            times.lock().unwrap().microphone_first.get_or_insert(first);
            file.write_samples(&frames).ok();
        }
        thread::sleep(MIC_EVERY);
    }
    file.finish().ok();
}
