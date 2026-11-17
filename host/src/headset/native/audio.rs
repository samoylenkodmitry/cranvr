//! The Mac's sound for the native client. `game_audio.rs` captures what this Mac plays with
//! ScreenCaptureKit and converts it to interleaved 16-bit stereo at 48 kHz. In ALVR mode the server
//! core sends it. In native mode `send` below cuts it into datagrams of `FRAMES_PER_DATAGRAM`
//! stereo frames, 5 ms each, and the client plays them (`client/src/audio.rs`).
//!
//! The sound goes out as plain PCM. Opus would cut 1.54 Mbps to about 0.1 Mbps. The stream's
//! video takes 40 to 100 Mbps, so the PCM costs about 1.5% on top of it. PCM adds no encode or
//! decode delay and needs no codec on the Quest.
//!
//! Each datagram carries the host's clock for its first sample, so the client can tell how old the
//! sound is when it plays it. ScreenCaptureKit hands over a buffer of about 20 ms at once. The
//! datagrams of one buffer therefore leave together, and the client's jitter buffer absorbs it.

use super::Session;
use alvr_common::info;
use cranvr_proto::{
    clock::now_ns,
    message::{Audio, HostDatagram, MAX_AUDIO_BYTES},
};
use std::{
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

/// The stereo frames in one datagram: 240 frames of 4 bytes are 960 bytes, under the limit of one
/// datagram, and last 5 ms.
const FRAMES_PER_DATAGRAM: usize = 240;
const BYTES_PER_FRAME: usize = 4;
const SAMPLE_RATE: f64 = 48_000.0;
const LOG_EVERY: Duration = Duration::from_secs(5);

const _: () = assert!(FRAMES_PER_DATAGRAM * BYTES_PER_FRAME <= MAX_AUDIO_BYTES);

/// The session that plays the sound now. Its audio thread logs the numbers.
static CURRENT: Mutex<Option<Arc<Session>>> = Mutex::new(None);
static SEQUENCE: AtomicU32 = AtomicU32::new(0);
/// Datagrams and bytes sent, and the stereo frames that they hold, since the host started
static DATAGRAMS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static FRAMES: AtomicU64 = AtomicU64::new(0);
/// Buffers that arrived while no client was there to play them
static DISCARDED_FRAMES: AtomicU64 = AtomicU64::new(0);

/// A client connected: its session gets the sound from now on.
pub(super) fn attach(session: &Arc<Session>) {
    *CURRENT.lock().unwrap() = Some(Arc::clone(session));
    SEQUENCE.store(0, Ordering::Relaxed);
    let session = Arc::clone(session);
    thread::Builder::new()
        .name("native-audio-log".into())
        .spawn(move || log_loop(&session))
        .ok();
}

/// The session ended. A later session that attached already stays.
pub(super) fn detach(session: &Arc<Session>) {
    let mut current = CURRENT.lock().unwrap();
    if current.as_ref().is_some_and(|c| Arc::ptr_eq(c, session)) {
        *current = None;
    }
}

/// Sends one buffer of the Mac's sound, interleaved 16-bit stereo at 48 kHz in native byte
/// order (little endian on every Mac that runs the host), to the client. The capture calls it
/// from its own queue when the buffer arrives. The buffer's last sample is the newest one.
pub fn send(pcm: &[u8]) {
    let Some(session) = CURRENT.lock().unwrap().clone() else {
        DISCARDED_FRAMES.fetch_add((pcm.len() / BYTES_PER_FRAME) as u64, Ordering::Relaxed);
        return;
    };
    let arrived = now_ns();
    let total_frames = pcm.len() / BYTES_PER_FRAME;
    let mut first_frame = 0;
    for chunk in pcm.chunks(FRAMES_PER_DATAGRAM * BYTES_PER_FRAME) {
        let frames = chunk.len() / BYTES_PER_FRAME;
        if frames == 0 {
            break;
        }
        // The buffer ended at its arrival: this chunk's first sample is that old
        let behind = ((total_frames - first_frame) as f64 / SAMPLE_RATE * 1e9) as u64;
        let message = Audio {
            sequence: SEQUENCE.fetch_add(1, Ordering::Relaxed),
            host_time: arrived.saturating_sub(behind),
            pcm: chunk[..frames * BYTES_PER_FRAME].to_vec(),
        };
        session.link.duplex.send_datagram(&HostDatagram::Audio(message)).ok();
        DATAGRAMS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add((frames * BYTES_PER_FRAME) as u64, Ordering::Relaxed);
        FRAMES.fetch_add(frames as u64, Ordering::Relaxed);
        first_frame += frames;
    }
}

fn log_loop(session: &Arc<Session>) {
    let mut seen = (0u64, 0u64, 0u64);
    let mut at = std::time::Instant::now();
    loop {
        thread::sleep(LOG_EVERY);
        if CURRENT.lock().unwrap().as_ref().is_none_or(|c| !Arc::ptr_eq(c, session)) {
            return;
        }
        let now = (DATAGRAMS.load(Ordering::Relaxed), BYTES.load(Ordering::Relaxed), FRAMES.load(Ordering::Relaxed));
        let seconds = at.elapsed().as_secs_f64();
        info!(
            "Native audio: {:.2} s of sound a second in {:.0} datagrams a second, {:.2} Mbps of PCM",
            (now.2 - seen.2) as f64 / SAMPLE_RATE / seconds,
            (now.0 - seen.0) as f64 / seconds,
            (now.1 - seen.1) as f64 * 8.0 / seconds / 1e6
        );
        (seen, at) = (now, std::time::Instant::now());
    }
}

/// Seconds of sound that arrived while no client was connected, since the host started.
#[allow(dead_code, reason = "a test and the log read it")]
pub fn discarded_seconds() -> f64 {
    DISCARDED_FRAMES.load(Ordering::Relaxed) as f64 / SAMPLE_RATE
}
