//! The headset's microphone for the games under Wine (issue #94).
//!
//! The host keeps a ring of the headset's sound in a file of the config dir, `microphone.pcm`. Wine's
//! CoreAudio driver with cranvr's patch 0008 maps the same file. Its capture device "cranvr microphone"
//! reads from the ring while the app's microphone setting is "headset" (`microphone.txt`, which the app
//! writes). The file holds a header of 64 bytes and 16384 frames of one channel at 48 kHz, 16 bits:
//!
//! | offset | field |
//! |---|---|
//! | 0 | magic "CRMC", version 1, sample rate 48000, capacity in frames (u32 each) |
//! | 16 | the frames written in all (u64, the host writes it after the frames) |
//! | 24 | when the reader last read or ticked, on `mach_continuous_time` in ns (u64, Wine writes it) |
//! | 32 | the frames that the reader handed to the game, its underruns and its skips (u64 each, Wine) |
//!
//! A game records while its stream of the device runs. The driver then stamps the reader's time
//! every period. The host looks at the stamp ten times a second. It asks the headset for its
//! microphone with `HostMsg::Microphone(true)` while the stamp is fresh and with `false` after it
//! stops. The headset records only in between, so the microphone costs nothing while no game records.
//!
//! The headset sends 5 ms a datagram. The host writes each datagram into the ring at once. A lost
//! datagram becomes silence. A late one is dropped. The reader stays 10 ms behind the newest frame and
//! skips forward when the lag stays larger, so the drift between the headset's clock and the Mac's
//! clock adds no delay.

use super::Session;
use cranvr_proto::{
    clock::now_ns,
    message::{HostMsg, Microphone, MICROPHONE_FORMAT, MICROPHONE_FRAMES},
};
use log::{info, warn};
use std::{
    fs::OpenOptions,
    path::Path,
    ptr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

const MAGIC: u32 = 0x434d_5243;
const VERSION: u32 = 1;
const HEADER: usize = 64;
/// 341 ms: far more than the reader's lag of 10 ms, so the reader never meets the writer
const CAPACITY: usize = 16384;
const WRITE_POS: usize = 16;
const READER_BEAT: usize = 24;
const READER_FRAMES: usize = 32;
const READER_UNDERRUNS: usize = 40;
const READER_SKIPS: usize = 48;
/// A stamp of the reader this young means that a game records
const RECORDING: Duration = Duration::from_millis(500);
const WATCH_EVERY: Duration = Duration::from_millis(100);
const LOG_EVERY: Duration = Duration::from_secs(5);
/// The most silence that a gap of lost datagrams writes. A longer gap writes this much and goes on:
/// the reader then skips to the newest frames.
const MAX_GAP_FRAMES: usize = 4800;
/// A sound that starts after this much quiet is an onset for the test log (`CRANVR_MICROPHONE_ONSETS=1`)
const ONSET_LEVEL: i16 = 1638;
const ONSET_QUIET_FRAMES: u64 = 9600;

const _: () = assert!(CAPACITY.is_power_of_two() && MICROPHONE_FORMAT.sample_rate == 48_000 && MICROPHONE_FORMAT.channels == 1);

/// The ring, mapped into the host's memory
struct Ring {
    base: *mut u8,
}

// The ring's memory stays mapped for the life of the host. The writer is one thread at a time (under
// `WRITER`), and the reader's fields are atomics.
unsafe impl Send for Ring {}
unsafe impl Sync for Ring {}

impl Ring {
    fn create(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(path)?;
        let len = HEADER + CAPACITY * 2;
        file.set_len(len as u64)?;
        // SAFETY: a shared mapping of the whole file, which has `len` bytes. The mapping outlives the
        // file handle and stays for the life of the process.
        let base = unsafe {
            libc::mmap(ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, std::os::fd::AsRawFd::as_raw_fd(&file), 0)
        };
        if base == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        let ring = Self { base: base.cast() };
        for (offset, value) in [(0, MAGIC), (4, VERSION), (8, MICROPHONE_FORMAT.sample_rate), (12, CAPACITY as u32)] {
            // SAFETY: the header's first 16 bytes are in the mapping.
            unsafe { ptr::write_unaligned(ring.base.add(offset).cast::<u32>(), value) };
        }
        Ok(ring)
    }

    fn word(&self, offset: usize) -> &AtomicU64 {
        // SAFETY: the offsets of the header's words are 8-byte aligned in a page-aligned mapping.
        unsafe { &*self.base.add(offset).cast::<AtomicU64>() }
    }

    /// Writes `samples` at frame `pos` of the stream and publishes the new end
    fn write(&self, pos: u64, samples: impl Iterator<Item = i16>) -> u64 {
        let mut end = pos;
        for sample in samples {
            let at = HEADER + (end as usize & (CAPACITY - 1)) * 2;
            // SAFETY: `at` lies in the ring's samples.
            unsafe { ptr::write_unaligned(self.base.add(at).cast::<i16>(), sample) };
            end += 1;
        }
        self.word(WRITE_POS).store(end, Ordering::Release);
        end
    }
}

static RING: OnceLock<Ring> = OnceLock::new();
static SESSION: Mutex<Option<Arc<Session>>> = Mutex::new(None);
static WRITER: Mutex<Writer> = Mutex::new(Writer::new());

/// What the host knows of the datagrams of one session
struct Writer {
    next: Option<u32>,
    pos: u64,
    datagrams: u64,
    lost: u64,
    late: u64,
    /// The age of the first frame of each datagram at the host, in ms: the sum and the most
    age_ms: (f64, f64),
    /// The loudest sample since the last log line
    peak: u16,
    quiet: u64,
}

impl Writer {
    const fn new() -> Self {
        Self { next: None, pos: 0, datagrams: 0, lost: 0, late: 0, age_ms: (0.0, 0.0), peak: 0, quiet: 0 }
    }
}

/// The time on `mach_continuous_time` in ns, the clock of Wine's performance counter
fn continuous_ns() -> u64 {
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    extern "C" {
        fn mach_continuous_time() -> u64;
        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }
    static TIMEBASE: OnceLock<(u64, u64)> = OnceLock::new();
    let (numer, denom) = *TIMEBASE.get_or_init(|| {
        let mut info = Timebase { numer: 0, denom: 0 };
        // SAFETY: `info` is a place for the timebase.
        unsafe { mach_timebase_info(&mut info) };
        (u64::from(info.numer), u64::from(info.denom.max(1)))
    });
    // SAFETY: a plain read of the clock
    let ticks = unsafe { mach_continuous_time() };
    (u128::from(ticks) * u128::from(numer) / u128::from(denom)) as u64
}

/// Makes the ring at `path` and starts the thread that watches its reader.
pub fn start(path: &Path) {
    let ring = match Ring::create(path) {
        Ok(ring) => ring,
        Err(e) => {
            warn!("Microphone: no ring at {}: {e}. Games hear no headset microphone.", path.display());
            return;
        }
    };
    if RING.set(ring).is_err() {
        return;
    }
    info!("Microphone: the ring for games is {}", path.display());
    thread::Builder::new().name("microphone-watch".into()).spawn(watch).ok();
}

/// Whether a game records now: the reader's stamp is fresh
fn recording(ring: &Ring) -> bool {
    let beat = ring.word(READER_BEAT).load(Ordering::Relaxed);
    beat != 0 && continuous_ns().saturating_sub(beat) < RECORDING.as_nanos() as u64
}

fn watch() {
    let Some(ring) = RING.get() else { return };
    let mut was = false;
    let mut last_log = Instant::now();
    let mut read_before = (ring.word(READER_FRAMES).load(Ordering::Relaxed), ring.word(READER_UNDERRUNS).load(Ordering::Relaxed), ring.word(READER_SKIPS).load(Ordering::Relaxed));
    loop {
        thread::sleep(WATCH_EVERY);
        let now = recording(ring);
        if now != was {
            info!("Microphone: {}", if now { "a game records from cranvr's microphone" } else { "no game records" });
            if let Some(session) = SESSION.lock().unwrap().as_ref() {
                session.link.duplex.send_control(&HostMsg::Microphone(now)).ok();
            }
            was = now;
        }
        if last_log.elapsed() >= LOG_EVERY {
            let secs = last_log.elapsed().as_secs_f64();
            last_log = Instant::now();
            let read = (ring.word(READER_FRAMES).load(Ordering::Relaxed), ring.word(READER_UNDERRUNS).load(Ordering::Relaxed), ring.word(READER_SKIPS).load(Ordering::Relaxed));
            let mut writer = WRITER.lock().unwrap();
            if now || writer.datagrams > 0 {
                let datagrams = writer.datagrams.max(1) as f64;
                info!(
                    "Microphone: {:.1} datagrams a second, {} lost, {} late, age at the host {:.1} ms mean, {:.1} ms most, peak level {:.3}; the game read {:.0} frames a second, {} frames of silence for want of sound, {} skips",
                    writer.datagrams as f64 / secs,
                    writer.lost,
                    writer.late,
                    writer.age_ms.0 / datagrams,
                    writer.age_ms.1,
                    f32::from(writer.peak) / 32768.0,
                    (read.0 - read_before.0) as f64 / secs,
                    read.1 - read_before.1,
                    read.2 - read_before.2,
                );
            }
            (writer.datagrams, writer.lost, writer.late, writer.age_ms, writer.peak) = (0, 0, 0, (0.0, 0.0), 0);
            read_before = read;
        }
    }
}

/// A client connected. A client that can record hears whether a game records now.
pub(super) fn attach(session: &Arc<Session>) {
    let Some(ring) = RING.get() else { return };
    if !session.microphone {
        return;
    }
    *WRITER.lock().unwrap() = Writer { pos: ring.word(WRITE_POS).load(Ordering::Relaxed), ..Writer::new() };
    session.link.duplex.send_control(&HostMsg::Microphone(recording(ring))).ok();
    *SESSION.lock().unwrap() = Some(Arc::clone(session));
}

pub(super) fn detach(session: &Arc<Session>) {
    let mut current = SESSION.lock().unwrap();
    if current.as_ref().is_some_and(|s| Arc::ptr_eq(s, session)) {
        *current = None;
    }
}

/// Writes a datagram of the headset's microphone into the ring.
pub(super) fn on_datagram(session: &Session, mic: Microphone) {
    let Some(ring) = RING.get() else { return };
    if mic.pcm.len() != MICROPHONE_FRAMES * 2 {
        return;
    }
    let mut writer = WRITER.lock().unwrap();
    if let Some(next) = writer.next {
        let ahead = mic.sequence.wrapping_sub(next) as i32;
        if ahead < 0 {
            writer.late += 1;
            return;
        }
        if ahead > 0 {
            writer.lost += ahead as u64;
            let silence = (ahead as usize * MICROPHONE_FRAMES).min(MAX_GAP_FRAMES);
            writer.pos = ring.write(writer.pos, std::iter::repeat_n(0, silence));
        }
    }
    writer.next = Some(mic.sequence.wrapping_add(1));
    writer.datagrams += 1;
    let now = now_ns();
    let captured = session.clock.lock().unwrap().to_local(mic.captured);
    if let Some(captured) = captured {
        let age = (now as f64 - captured as f64) / 1e6;
        writer.age_ms = (writer.age_ms.0 + age, writer.age_ms.1.max(age));
    }
    let samples = || mic.pcm.as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b));
    writer.peak = samples().map(i16::unsigned_abs).fold(writer.peak, u16::max);
    if onsets_logged() {
        log_onsets(&mut writer, samples(), captured, now);
    }
    let pos = writer.pos;
    writer.pos = ring.write(pos, samples());
}

/// With `CRANVR_MICROPHONE_ONSETS=1`: logs each sound of the Mac's game sound (16-bit stereo) that
/// starts after quiet, with the time on `mach_continuous_time` at which the Mac played it. A test
/// that plays a sound through the headset's speakers into its microphone gets the round trip.
pub(super) fn log_game_sound_onsets(pcm: &[u8], frames: usize) {
    static QUIET: AtomicU64 = AtomicU64::new(0);
    if !onsets_logged() {
        return;
    }
    let arrived = continuous_ns();
    for (i, frame) in pcm.as_chunks::<4>().0.iter().enumerate() {
        let left = i16::from_le_bytes([frame[0], frame[1]]);
        if left.unsigned_abs() < ONSET_LEVEL as u16 {
            QUIET.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        if QUIET.swap(0, Ordering::Relaxed) >= ONSET_QUIET_FRAMES {
            // The buffer ended at its arrival
            let behind = (frames - i) as u64 * 1_000_000_000 / u64::from(MICROPHONE_FORMAT.sample_rate);
            info!("Game sound onset: played at {} ns", arrived.saturating_sub(behind));
        }
    }
}

fn onsets_logged() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("CRANVR_MICROPHONE_ONSETS").is_ok_and(|v| v == "1"))
}

/// Logs each sound that starts after quiet with the time at which the headset's microphone took it,
/// on `mach_continuous_time`, so a test compares it with the time at which a game heard it.
fn log_onsets(writer: &mut Writer, samples: impl Iterator<Item = i16>, captured: Option<u64>, now: u64) {
    for (i, sample) in samples.enumerate() {
        if sample.unsigned_abs() < ONSET_LEVEL as u16 {
            writer.quiet += 1;
            continue;
        }
        if writer.quiet >= ONSET_QUIET_FRAMES {
            let offset = i as u64 * 1_000_000_000 / u64::from(MICROPHONE_FORMAT.sample_rate);
            match captured {
                Some(captured) => {
                    let at = continuous_ns().saturating_sub(now.saturating_sub(captured + offset));
                    info!("Microphone onset: captured at {at} ns, {:.1} ms before the host had it", now.saturating_sub(captured + offset) as f64 / 1e6);
                }
                None => info!("Microphone onset: no capture time yet (the clock sync has no estimate)"),
            }
        }
        writer.quiet = 0;
    }
}

#[cfg(test)]
#[path = "../../tests/native_microphone.rs"]
mod tests;
