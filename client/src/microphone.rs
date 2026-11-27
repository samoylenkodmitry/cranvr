//! The headset's microphone for the host's games (issue #94).
//!
//! The host asks with `HostMsg::Microphone(true)` while a game records from the device "cranvr
//! microphone" under Wine, and with `false` after it stops. The client records only in between, so
//! the microphone costs nothing while no game records.
//!
//! Cranpose's `backend::open_microphone` records with an AAudio input stream on a Quest, in
//! low-latency mode with the preset for speech recognition. On a Quest 3 a game hears the sound about
//! 30 ms sooner with it than with the preset for a call, which cancels the echo of the headset's
//! speakers (see "The microphone in games" in docs/desktop.md). The launch extra
//! `microphone_preset=communication` records with the call preset. The recorder runs
//! on the input thread. It mixes the channels down to one, resamples to 48 kHz when the device runs
//! at another rate and puts the frames into a lock-free queue. A thread of this file sends them in
//! datagrams of 10 ms (`ClientDatagram::Microphone`, 16 bits). Each datagram carries the time at
//! which the microphone took its first frame, from the stream's capture time, so the host knows how
//! old the sound is.
//!
//! The app asks for the microphone permission when the host first asks to record and the person has
//! not answered yet. The recording starts once the permission is there.

use cranpose_audio::{
    backend::{self, InputFormat, InputPreset, Recorder},
    ring,
};
use cranpose::{microphone_permission, request_microphone_permission, MicrophonePermission};
use cranvr_proto::{
    clock::now_ns,
    link::ClientLink,
    message::{ClientDatagram, Microphone, MICROPHONE_FORMAT, MICROPHONE_FRAMES},
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

const RATE: f64 = MICROPHONE_FORMAT.sample_rate as f64;
/// 200 ms: the sender takes the frames every few milliseconds, so the queue never fills
const QUEUE_FRAMES: usize = 9600;
const SEND_EVERY: Duration = Duration::from_millis(2);
/// How long the app waits for the person's answer to the permission
const PERMISSION_WAIT: Duration = Duration::from_secs(60);
const LOG_EVERY: Duration = Duration::from_secs(5);

/// The recording of this session, while the host asks for it
static RECORDING: Mutex<Option<Recording>> = Mutex::new(None);

struct Recording {
    stop: Arc<AtomicBool>,
    thread: thread::JoinHandle<()>,
}

/// The host's wish of `HostMsg::Microphone`.
pub fn on_wish(link: &Arc<ClientLink>, record: bool) {
    let mut recording = RECORDING.lock().unwrap();
    match (record, recording.is_some()) {
        (true, false) => {
            log::info!("microphone: the host asks to record");
            let stop = Arc::new(AtomicBool::new(false));
            let (link, flag) = (Arc::clone(link), Arc::clone(&stop));
            match thread::Builder::new().name("cranvr-microphone".into()).spawn(move || record_until(&link, &flag)) {
                Ok(thread) => *recording = Some(Recording { stop, thread }),
                Err(e) => log::warn!("microphone: no thread: {e}"),
            }
        }
        (false, true) => {
            log::info!("microphone: the host asks to stop");
            drop(recording);
            close();
        }
        _ => {}
    }
}

/// Ends the recording, at the end of a session or when the host asks.
pub fn close() {
    let recording = RECORDING.lock().unwrap().take();
    if let Some(recording) = recording {
        recording.stop.store(true, Ordering::Relaxed);
        recording.thread.join().ok();
    }
}

/// What the input thread hands the sender: the capture time of the first frame of the queue's stream
/// minus the frames before it, so the sender gets the capture time of any frame
struct Origin(AtomicU64);

impl Origin {
    /// The capture time on the protocol's clock of the frame numbered `frame`
    fn captured(&self, frame: u64) -> u64 {
        self.0.load(Ordering::Relaxed) + (frame as f64 * 1e9 / RATE) as u64
    }
}

/// Runs on the input thread: one channel at 48 kHz into the queue
struct Capture {
    queue: ring::Producer<i16>,
    origin: Arc<Origin>,
    /// Frames put into the queue so far
    written: u64,
    /// The resampler's place after the last input frame, in input frames: the next output frame
    /// lies there
    phase: f64,
    last: f32,
    dropped: Arc<AtomicU64>,
}

impl Recorder for Capture {
    fn record(&mut self, input: &[f32], format: InputFormat, captured: Option<Instant>) {
        let channels = format.channels.max(1);
        // The capture time of this block's first output frame, on the protocol's clock
        // The resampler's place within a frame shifts it by less than 21 microseconds
        let now = now_ns();
        let at = captured.map_or(now, |captured| now.saturating_sub(captured.elapsed().as_nanos() as u64));
        self.origin.0.store(at.saturating_sub((self.written as f64 * 1e9 / RATE) as u64), Ordering::Relaxed);
        let step = f64::from(format.sample_rate) / RATE;
        for frame in input.chunks_exact(channels) {
            let value = frame.iter().sum::<f32>() / channels as f32;
            if step == 1.0 {
                self.put(value);
                continue;
            }
            // Output frames fall between the last input frame and this one
            while self.phase < 1.0 {
                let t = self.phase as f32;
                self.put(self.last + (value - self.last) * t);
                self.phase += step;
            }
            self.phase -= 1.0;
            self.last = value;
        }
    }
}

impl Capture {
    fn put(&mut self, value: f32) {
        let sample = (value.clamp(-1.0, 1.0) * 32767.0) as i16;
        if self.queue.push(sample).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        self.written += 1;
    }
}

/// Waits for the permission, records and sends until `stop`.
fn record_until(link: &ClientLink, stop: &AtomicBool) {
    let asked = Instant::now();
    while microphone_permission() != MicrophonePermission::Granted {
        if asked.elapsed() >= PERMISSION_WAIT || stop.load(Ordering::Relaxed) {
            log::warn!("microphone: no permission to record ({:?})", microphone_permission());
            return;
        }
        if asked.elapsed() < Duration::from_millis(10) {
            log::info!("microphone: asking for the permission to record");
            request_microphone_permission();
        }
        thread::sleep(Duration::from_millis(200));
    }
    let (producer, mut consumer) = ring::channel::<i16>(QUEUE_FRAMES);
    let origin = Arc::new(Origin(AtomicU64::new(now_ns())));
    let dropped = Arc::new(AtomicU64::new(0));
    let capture = Capture { queue: producer, origin: Arc::clone(&origin), written: 0, phase: 0.0, last: 0.0, dropped: Arc::clone(&dropped) };
    let preset = if cranpose::launch_args().string("microphone_preset") == Some("communication") {
        InputPreset::VoiceCommunication
    } else {
        InputPreset::VoiceRecognition
    };
    let source = match backend::open_microphone(Box::new(capture), preset) {
        Ok(source) => source,
        Err(e) => {
            log::warn!("microphone: the microphone did not open: {e}");
            return;
        }
    };
    log::info!("microphone: recording ({preset:?})");
    // One datagram that the loop fills again each time
    let mut datagram = ClientDatagram::Microphone(Microphone { sequence: 0, captured: 0, pcm: Vec::with_capacity(MICROPHONE_FRAMES * 2) });
    let (mut taken, mut sent, mut since) = (0u64, 0u64, Instant::now());
    while !stop.load(Ordering::Relaxed) && source.is_running() {
        while let Some(sample) = consumer.pop() {
            let ClientDatagram::Microphone(message) = &mut datagram else { unreachable!("the datagram is the microphone's") };
            if message.pcm.is_empty() {
                message.captured = origin.captured(taken);
            }
            message.pcm.extend_from_slice(&sample.to_le_bytes());
            taken += 1;
            if message.pcm.len() == MICROPHONE_FRAMES * 2 {
                link.duplex.send_datagram(&datagram).ok();
                let ClientDatagram::Microphone(message) = &mut datagram else { unreachable!("the datagram is the microphone's") };
                message.sequence = message.sequence.wrapping_add(1);
                message.pcm.clear();
                sent += 1;
            }
        }
        if since.elapsed() >= LOG_EVERY {
            let age = source.input_latency().map_or("unknown".to_string(), |age| format!("{:.1} ms", age.as_secs_f64() * 1e3));
            log::info!(
                "microphone: {:.1} datagrams a second, input latency {age}, {} overruns, {} frames lost in the queue",
                sent as f64 / since.elapsed().as_secs_f64(),
                source.overrun_count().unwrap_or(0),
                dropped.swap(0, Ordering::Relaxed)
            );
            (sent, since) = (0, Instant::now());
        }
        thread::sleep(SEND_EVERY);
    }
    log::info!("microphone: stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture() -> (Capture, ring::Consumer<i16>, Arc<Origin>) {
        let (producer, consumer) = ring::channel::<i16>(QUEUE_FRAMES);
        let origin = Arc::new(Origin(AtomicU64::new(0)));
        let capture = Capture { queue: producer, origin: Arc::clone(&origin), written: 0, phase: 0.0, last: 0.0, dropped: Arc::new(AtomicU64::new(0)) };
        (capture, consumer, origin)
    }

    fn drain(consumer: &mut ring::Consumer<i16>) -> Vec<i16> {
        std::iter::from_fn(|| consumer.pop()).collect()
    }

    #[test]
    fn one_channel_at_48_khz_goes_through_as_it_is() {
        let (mut capture, mut queue, _) = capture();
        let input: Vec<f32> = (0..480).map(|i| i as f32 / 1000.0).collect();
        capture.record(&input, InputFormat { sample_rate: 48_000.0, channels: 1 }, None);
        let out = drain(&mut queue);
        assert_eq!(out.len(), 480);
        assert_eq!(out[100], (0.1f32 * 32767.0) as i16);
    }

    #[test]
    fn stereo_at_44_1_khz_becomes_one_channel_at_48_khz() {
        let (mut capture, mut queue, _) = capture();
        // One second of a 1 kHz tone in the left channel and silence in the right, in blocks of 441 frames
        let mut out = Vec::new();
        for block in 0..100 {
            let input: Vec<f32> = (0..441)
                .flat_map(|i| {
                    let t = (block * 441 + i) as f32 / 44_100.0;
                    [(t * 1000.0 * std::f32::consts::TAU).sin() * 0.8, 0.0]
                })
                .collect();
            capture.record(&input, InputFormat { sample_rate: 44_100.0, channels: 2 }, None);
            out.extend(drain(&mut queue));
        }
        assert!((out.len() as i64 - 48_000).abs() <= 1, "{} frames", out.len());
        // The mix of the two channels: half the tone, still at 1 kHz (48 frames a period)
        let peak = out.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!((f32::from(peak) / 32767.0 - 0.4).abs() < 0.01, "peak {peak}");
        let crossings = out.windows(2).filter(|w| w[0] < 0 && w[1] >= 0).count();
        assert!((999..=1001).contains(&crossings), "{crossings} periods in a second");
    }

    #[test]
    fn the_origin_gives_each_frame_its_capture_time() {
        let (mut capture, mut queue, origin) = capture();
        let captured = Instant::now() - Duration::from_millis(20);
        capture.record(&[0.0; 480], InputFormat { sample_rate: 48_000.0, channels: 1 }, Some(captured));
        drain(&mut queue);
        // The block's first frame was 20 ms old, and frame 240 came 5 ms after it
        let first = now_ns() - origin.captured(0);
        let later = origin.captured(240) - origin.captured(0);
        assert!((19_000_000..=22_000_000).contains(&first), "{first}");
        assert_eq!(later, 5_000_000);
    }
}
