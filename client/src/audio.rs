//! The Mac's sound in the headset.
//!
//! The host sends the sound as PCM datagrams of 5 ms (`HostDatagram::Audio`, 48 kHz, 16-bit
//! stereo). This file plays them through Cranpose's audio backend. The backend's public part
//! (`cranpose_audio::backend::open`) runs a `Renderer` on the platform's real-time output
//! thread (AAudio on a Quest, with its low-latency mode). The renderer takes the samples from a
//! lock-free queue (`cranpose_audio::ring`) that the network thread fills.
//!
//! The two ends run on different clocks: the Mac's capture and the headset's output. A buffer
//! between them absorbs the jitter of the network and of the capture, which hands over about 20 ms
//! of sound at once. The buffer works like this:
//! - The renderer starts to play when the buffer holds `TARGET_MS` of sound. It starts again from
//!   silence after an underrun: it plays what it has, fills the rest with silence and waits until
//!   the buffer holds the target again.
//! - A small control loop changes the playback rate by at most 0.5% (the pitch moves by under nine
//!   cents) so that the fill follows the target. It absorbs the drift between the two clocks and the
//!   difference between 48 kHz and the device's own rate. Linear interpolation resamples.
//! - A buffer that grows past `MAX_MS` loses its oldest samples down to the target. This happens
//!   after a stall of the network thread.
//! - A datagram that is missing in the sequence leaves a gap of silence of its length, so the
//!   sound keeps its timing. A datagram that comes late is dropped.
//!
//! The renderer never locks, allocates or waits. It always says "continue": a stream that the
//! renderer stops is gone for good.
//!
//! Cranpose's output stream reports the device's own numbers: its output latency, the frames of a
//! burst and the times that the device had no frame to play (`AudioSink::underrun_count`). That
//! count differs from this file's own underruns, which count the times that the jitter buffer ran
//! dry. A dry buffer writes silence into a full device buffer and raises only this file's count. A
//! device that ran dry raises the device's count. `Stats` and the log show both, so a dry-out that a
//! listener hears has a cause. `Stats` also gives the buffer's fill, which is what this file controls,
//! and the age of the sound when the network thread queued it.

use cranpose_audio::{
    backend::{self, AudioSink, Renderer},
    ring, RenderStatus,
};
use cranvr_proto::message::{Audio, AudioFormat, SampleFormat};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    Arc, Mutex,
};

/// The buffer's target fill. A burst of the host's capture is about 20 ms, so the target holds
/// one burst and a little more.
pub const TARGET_MS: f32 = 30.0;
/// A buffer that holds more than this loses its oldest samples down to the target.
const MAX_MS: f32 = 90.0;
/// The queue's capacity in stereo frames. 16384 frames are 341 ms at 48 kHz.
const RING_FRAMES: usize = 16_384;
/// The largest change of the playback rate that the control loop makes
const MAX_RATE_ADJUST: f32 = 0.005;
/// The control loop's gain: a fill that is the whole target too high speeds the playback by this
/// much
const RATE_GAIN: f32 = 0.01;
/// The control loop follows the fill's average over this time in seconds. The capture hands over
/// 20 ms at once, so the fill saws up and down by that much 50 times a second. A loop that followed
/// each tooth would change the pitch by up to the full 0.5% at 50 Hz.
const SMOOTHING_S: f32 = 1.0;
/// The longest gap of silence that a missing sequence leaves, in datagrams. A longer gap is a
/// reconnect or a pause of the host.
const MAX_GAP: u32 = 20;

/// What the two threads and the UI share. Every field is an atomic: the renderer runs in real
/// time.
#[derive(Default)]
struct Shared {
    /// Stereo frames pushed and popped. The difference is the fill.
    pushed: AtomicU64,
    popped: AtomicU64,
    /// Times that the renderer found the buffer empty while it played
    underruns: AtomicU64,
    /// Frames that the renderer dropped from an overfull buffer
    dropped_frames: AtomicU64,
    /// Frames that the network thread could not queue, because the queue was full
    overflow_frames: AtomicU64,
    /// Datagrams that never came. Datagrams that came after a newer one
    lost_packets: AtomicU64,
    late_packets: AtomicU64,
    packets: AtomicU64,
    /// Stereo frames that the device asks for in one callback. The device's rate
    callback_frames: AtomicU32,
    device_rate: AtomicU32,
    /// The smallest and the largest fill in frames since the last `stats` call
    fill_min: AtomicU64,
    fill_max: AtomicU64,
    /// The renderer plays. It does not wait for the buffer to fill
    playing: AtomicBool,
    /// The sum of the age of the sound when it was queued, in microseconds. The count
    age_us: AtomicU64,
    age_n: AtomicU64,
    /// The playback rate's change in parts per million, as the bits of an f32
    rate_ppm: AtomicU32,
    /// The largest absolute sample that the renderer wrote since the last log line, as the bits of
    /// an f32. The bits of a non-negative float order like the floats, so `fetch_max` works.
    peak: AtomicU32,
}

/// The numbers that the stats panel and the log show.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    /// Whether a player runs
    pub open: bool,
    pub playing: bool,
    pub device_rate: u32,
    /// The frames that the device asks for in one callback, in ms
    pub burst_ms: f32,
    /// The buffer's fill now and its range since the last look, in ms
    pub fill_ms: f32,
    pub fill_min_ms: f32,
    pub fill_max_ms: f32,
    pub target_ms: f32,
    /// The mean age of the sound when it was queued, in ms: host capture to the buffer
    pub age_ms: Option<f32>,
    /// Totals since the player opened
    pub packets: u64,
    pub lost_packets: u64,
    pub late_packets: u64,
    /// Times that the jitter buffer ran dry while the renderer played
    pub underruns: u64,
    /// Times that the device had no frame to play, since the stream opened. `None` where the
    /// platform keeps no count (cpal on a desktop).
    pub device_underruns: Option<u32>,
    /// How long a frame takes from the renderer to the output hardware, in ms, once the device
    /// reports it
    pub output_latency_ms: Option<f32>,
    /// The frames that the device fills in one burst
    pub frames_per_burst: Option<u32>,
    pub dropped_ms: f32,
    pub overflow_ms: f32,
    /// Seconds of sound played so far
    pub played_s: f32,
    /// The playback rate's change in percent
    pub rate_pct: f32,
    /// The largest sample that reached the device since the last log line, from 0 to 1. 0 means
    /// silence: the sound can be on while nothing plays.
    pub peak: f32,
}

/// The producer's end: it converts the datagrams and pushes them.
struct Producer {
    queue: ring::Producer<[f32; 2]>,
    expected: Option<u32>,
    shared: Arc<Shared>,
}

/// A running player. The sink stops the device when it drops.
struct Player {
    producer: Mutex<Producer>,
    shared: Arc<Shared>,
    source_rate: f32,
    /// Stops the device when the player drops. It also reports the device's numbers.
    sink: Box<dyn AudioSink>,
}

static PLAYER: Mutex<Option<Arc<Player>>> = Mutex::new(None);

/// Opens the output device for a stream with `format`. A format that the player does not play, or
/// a device that does not open, leaves the sound off and logs why.
pub fn open(format: AudioFormat) {
    close();
    if format.channels != 2 || format.sample != SampleFormat::S16Le || format.sample_rate == 0 {
        log::warn!("the host sends sound that the client does not play: {format:?}");
        return;
    }
    let (queue, consumer) = ring::channel::<[f32; 2]>(RING_FRAMES);
    let shared = Arc::new(Shared { fill_min: AtomicU64::new(u64::MAX), ..Shared::default() });
    let renderer = Output::new(consumer, Arc::clone(&shared), format.sample_rate as f32);
    match backend::open(Box::new(renderer)) {
        Ok(sink) => {
            log::info!(
                "audio: output open for {} Hz stereo, jitter buffer target {TARGET_MS} ms, at most {MAX_MS} ms",
                format.sample_rate
            );
            let producer = Mutex::new(Producer { queue, expected: None, shared: Arc::clone(&shared) });
            let player = Arc::new(Player { producer, shared, source_rate: format.sample_rate as f32, sink });
            *PLAYER.lock().unwrap() = Some(Arc::clone(&player));
            let spawned = std::thread::Builder::new().name("cranvr-audio-log".into()).spawn(move || log_loop(&player));
            if let Err(e) = spawned {
                log::warn!("audio: no log thread: {e}");
            }
        }
        Err(e) => log::warn!("audio: no output device ({e}): the sound stays off"),
    }
}

/// How often the player logs its numbers. Once a second ties a dry-out to the device's count.
const LOG_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// Logs the player's numbers until a newer player or `close` replaces it.
fn log_loop(player: &Arc<Player>) {
    loop {
        std::thread::sleep(LOG_EVERY);
        if PLAYER.lock().unwrap().as_ref().is_none_or(|current| !Arc::ptr_eq(current, player)) {
            return;
        }
        let s = stats_of(player, true);
        log::info!(
            "audio: {}, buffer {:.1} ms now (range {:.1} to {:.1}, target {:.0}), device burst {:.1} ms at {} Hz, age when queued {} ms, {} packets, {} lost, {} late, {} buffer underruns, device underruns {}, device latency {} ms, burst reported {} frames, {:.0} ms dropped, {:.0} ms overflow, rate {:+.2}%, {:.1} s played, peak level {:.3}",
            if s.playing { "playing" } else { "waiting for the buffer" },
            s.fill_ms,
            s.fill_min_ms,
            s.fill_max_ms,
            s.target_ms,
            s.burst_ms,
            s.device_rate,
            s.age_ms.map_or("-".to_string(), |a| format!("{a:.1}")),
            s.packets,
            s.lost_packets,
            s.late_packets,
            s.underruns,
            s.device_underruns.map_or("-".to_string(), |n| n.to_string()),
            s.output_latency_ms.map_or("-".to_string(), |ms| format!("{ms:.1}")),
            s.frames_per_burst.map_or("-".to_string(), |n| n.to_string()),
            s.dropped_ms,
            s.overflow_ms,
            s.rate_pct,
            s.played_s,
            s.peak,
        );
    }
}

/// Closes the output device.
pub fn close() {
    *PLAYER.lock().unwrap() = None;
}

/// Queues one datagram of sound. `age` is how old its first sample was when it arrived, when the
/// clocks are in sync.
pub fn on_datagram(audio: &Audio, age: Option<std::time::Duration>) {
    let Some(player) = PLAYER.lock().unwrap().clone() else { return };
    let shared = &player.shared;
    shared.packets.fetch_add(1, Ordering::Relaxed);
    if let Some(age) = age {
        shared.age_us.fetch_add(age.as_micros().min(5_000_000) as u64, Ordering::Relaxed);
        shared.age_n.fetch_add(1, Ordering::Relaxed);
    }
    player.producer.lock().unwrap().push(audio);
}

impl Producer {
    fn push(&mut self, audio: &Audio) {
        let frames = audio.pcm.len() / 4;
        // A gap leaves silence of the missing datagrams' length, so the sound keeps its timing.
        // A datagram older than the newest is late: it would play out of order.
        if let Some(expected) = self.expected {
            let ahead = audio.sequence.wrapping_sub(expected);
            if ahead > u32::MAX / 2 {
                self.shared.late_packets.fetch_add(1, Ordering::Relaxed);
                return;
            }
            if ahead > 0 {
                self.shared.lost_packets.fetch_add(u64::from(ahead), Ordering::Relaxed);
                if ahead <= MAX_GAP {
                    for _ in 0..u64::from(ahead) * frames as u64 {
                        self.push_frame([0.0, 0.0]);
                    }
                }
            }
        }
        self.expected = Some(audio.sequence.wrapping_add(1));
        for frame in audio.pcm.as_chunks::<4>().0 {
            let left = f32::from(i16::from_le_bytes([frame[0], frame[1]])) / 32768.0;
            let right = f32::from(i16::from_le_bytes([frame[2], frame[3]])) / 32768.0;
            self.push_frame([left, right]);
        }
    }

    fn push_frame(&mut self, frame: [f32; 2]) {
        match self.queue.push(frame) {
            Ok(()) => {
                self.shared.pushed.fetch_add(1, Ordering::Release);
            }
            Err(_) => {
                self.shared.overflow_frames.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// What the stats panel shows now: the totals, the fill's range and the sound's age since the
/// log's last line.
pub fn stats() -> Stats {
    let Some(player) = PLAYER.lock().unwrap().clone() else { return Stats::default() };
    stats_of(&player, false)
}

/// The numbers of `player`. With `reset` the range of the fill and the mean age start again.
fn stats_of(player: &Player, reset: bool) -> Stats {
    let s = &player.shared;
    let rate = player.source_rate.max(1.0);
    let frames_to_ms = |frames: u64| frames as f32 * 1000.0 / rate;
    let pushed = s.pushed.load(Ordering::Acquire);
    let popped = s.popped.load(Ordering::Acquire);
    let (min, max, age_us, age_n, peak) = if reset {
        (
            s.fill_min.swap(u64::MAX, Ordering::Relaxed),
            s.fill_max.swap(0, Ordering::Relaxed),
            s.age_us.swap(0, Ordering::Relaxed),
            s.age_n.swap(0, Ordering::Relaxed),
            s.peak.swap(0, Ordering::Relaxed),
        )
    } else {
        (
            s.fill_min.load(Ordering::Relaxed),
            s.fill_max.load(Ordering::Relaxed),
            s.age_us.load(Ordering::Relaxed),
            s.age_n.load(Ordering::Relaxed),
            s.peak.load(Ordering::Relaxed),
        )
    };
    let fill = pushed.saturating_sub(popped);
    let device_rate = s.device_rate.load(Ordering::Relaxed);
    Stats {
        open: true,
        playing: s.playing.load(Ordering::Relaxed),
        device_rate,
        burst_ms: s.callback_frames.load(Ordering::Relaxed) as f32 * 1000.0 / device_rate.max(1) as f32,
        fill_ms: frames_to_ms(fill),
        fill_min_ms: if min == u64::MAX { frames_to_ms(fill) } else { frames_to_ms(min) },
        fill_max_ms: frames_to_ms(max.max(fill)),
        target_ms: TARGET_MS,
        age_ms: (age_n > 0).then(|| age_us as f32 / age_n as f32 / 1000.0),
        packets: s.packets.load(Ordering::Relaxed),
        lost_packets: s.lost_packets.load(Ordering::Relaxed),
        late_packets: s.late_packets.load(Ordering::Relaxed),
        underruns: s.underruns.load(Ordering::Relaxed),
        device_underruns: player.sink.underrun_count(),
        output_latency_ms: player.sink.output_latency().map(|latency| latency.as_secs_f32() * 1000.0),
        frames_per_burst: player.sink.frames_per_burst(),
        dropped_ms: frames_to_ms(s.dropped_frames.load(Ordering::Relaxed)),
        overflow_ms: frames_to_ms(s.overflow_frames.load(Ordering::Relaxed)),
        played_s: popped as f32 / rate,
        rate_pct: f32::from_bits(s.rate_ppm.load(Ordering::Relaxed)) / 10_000.0,
        peak: f32::from_bits(peak),
    }
}

/// The lines for the stats panel.
pub fn stat_lines() -> Vec<(&'static str, String)> {
    stat_lines_of(&stats())
}

fn stat_lines_of(s: &Stats) -> Vec<(&'static str, String)> {
    if !s.open {
        return Vec::new();
    }
    vec![(
        "Sound",
        format!(
            "{}; buffer {:.0} ms (target {:.0}, range {:.0} to {:.0}), burst {:.1} ms at {} Hz, age when queued {} ms; {} packets, {} lost, {} late, {} buffer underruns, {} device underruns, device latency {} ms, {:.0} ms dropped",
            if s.playing { "playing" } else { "waiting for the buffer" },
            s.fill_ms,
            s.target_ms,
            s.fill_min_ms,
            s.fill_max_ms,
            s.burst_ms,
            s.device_rate,
            s.age_ms.map_or("-".to_string(), |a| format!("{a:.1}")),
            s.packets,
            s.lost_packets,
            s.late_packets,
            s.underruns,
            s.device_underruns.map_or("-".to_string(), |n| n.to_string()),
            s.output_latency_ms.map_or("-".to_string(), |ms| format!("{ms:.1}")),
            s.dropped_ms,
        ),
    )]
}

/// Takes the queued frames, adjusts their rate to the device's and fills the device's buffers.
struct Output {
    queue: ring::Consumer<[f32; 2]>,
    shared: Arc<Shared>,
    source_rate: f32,
    device_rate: f32,
    channels: usize,
    primed: bool,
    /// The fill's moving average in frames, which the rate follows
    smoothed: f32,
    /// The frames between which the playback position lies. `phase` is how far past `before` it is
    before: [f32; 2],
    after: [f32; 2],
    phase: f32,
    started: bool,
}

impl Output {
    fn new(queue: ring::Consumer<[f32; 2]>, shared: Arc<Shared>, source_rate: f32) -> Self {
        Self {
            queue,
            shared,
            source_rate,
            device_rate: backend::NOMINAL_SAMPLE_RATE,
            channels: backend::NOMINAL_CHANNELS,
            primed: false,
            smoothed: 0.0,
            before: [0.0; 2],
            after: [0.0; 2],
            phase: 1.0,
            started: false,
        }
    }

    fn fill(&self) -> u64 {
        let pushed = self.shared.pushed.load(Ordering::Acquire);
        pushed.saturating_sub(self.shared.popped.load(Ordering::Relaxed))
    }

    fn pop(&mut self) -> Option<[f32; 2]> {
        let frame = self.queue.pop()?;
        self.shared.popped.fetch_add(1, Ordering::Release);
        Some(frame)
    }
}

impl Renderer for Output {
    fn set_device_format(&mut self, sample_rate: f32, channels: usize) {
        if self.device_rate != sample_rate || self.channels != channels {
            self.device_rate = sample_rate.max(1.0);
            self.channels = channels.max(1);
        }
        self.shared.device_rate.store(self.device_rate as u32, Ordering::Relaxed);
    }

    fn render(&mut self, out: &mut [f32]) -> RenderStatus {
        let channels = self.channels;
        let out_frames = out.len() / channels;
        self.shared.callback_frames.store(out_frames as u32, Ordering::Relaxed);
        out.fill(0.0);
        let target = (TARGET_MS * self.source_rate / 1000.0) as u64;
        let fill = self.fill();
        self.shared.fill_min.fetch_min(fill, Ordering::Relaxed);
        self.shared.fill_max.fetch_max(fill, Ordering::Relaxed);

        // A buffer that grew far past its target loses its oldest samples
        let max = (MAX_MS * self.source_rate / 1000.0) as u64;
        if fill > max {
            let mut dropped = 0;
            while self.fill() > target {
                if self.pop().is_none() {
                    break;
                }
                dropped += 1;
            }
            self.shared.dropped_frames.fetch_add(dropped, Ordering::Relaxed);
            self.started = false;
        }

        if !self.primed {
            if fill < target {
                self.shared.playing.store(false, Ordering::Relaxed);
                return RenderStatus::Continue;
            }
            self.primed = true;
            self.started = false;
            self.smoothed = fill as f32;
            self.shared.playing.store(true, Ordering::Relaxed);
        }

        // The rate: the source's samples per device sample, nudged toward the target fill
        self.smoothed += (fill as f32 - self.smoothed) * (out_frames as f32 / self.device_rate / SMOOTHING_S).min(1.0);
        let error = (self.smoothed - target as f32) / target.max(1) as f32;
        let adjust = (error * RATE_GAIN).clamp(-MAX_RATE_ADJUST, MAX_RATE_ADJUST);
        self.shared.rate_ppm.store((adjust * 1e6).to_bits(), Ordering::Relaxed);
        let step = self.source_rate / self.device_rate * (1.0 + adjust);

        if !self.started {
            // The first two frames set the interpolation's ends
            match (self.pop(), self.pop()) {
                (Some(a), Some(b)) => (self.before, self.after, self.phase, self.started) = (a, b, 0.0, true),
                _ => {
                    self.primed = false;
                    return RenderStatus::Continue;
                }
            }
        }
        let mut peak = 0.0f32;
        for frame in 0..out_frames {
            while self.phase >= 1.0 {
                match self.pop() {
                    Some(next) => {
                        self.before = self.after;
                        self.after = next;
                        self.phase -= 1.0;
                    }
                    None => {
                        // The buffer ran dry: the rest of this callback stays silent. The
                        // renderer waits until the buffer holds the target again
                        self.shared.underruns.fetch_add(1, Ordering::Relaxed);
                        self.primed = false;
                        self.started = false;
                        self.shared.playing.store(false, Ordering::Relaxed);
                        return RenderStatus::Continue;
                    }
                }
            }
            let t = self.phase;
            let left = self.before[0] + (self.after[0] - self.before[0]) * t;
            let right = self.before[1] + (self.after[1] - self.before[1]) * t;
            let at = frame * channels;
            match channels {
                1 => out[at] = (left + right) * 0.5,
                _ => {
                    out[at] = left;
                    out[at + 1] = right;
                }
            }
            peak = peak.max(left.abs()).max(right.abs());
            self.phase += step;
        }
        self.shared.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
        RenderStatus::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A renderer on a queue that the test fills, at the device's rate and channels.
    fn output(rate: f32, device_rate: f32, channels: usize) -> (ring::Producer<[f32; 2]>, Output) {
        let (queue, consumer) = ring::channel::<[f32; 2]>(RING_FRAMES);
        let shared = Arc::new(Shared { fill_min: AtomicU64::new(u64::MAX), ..Shared::default() });
        let mut output = Output::new(consumer, shared, rate);
        output.set_device_format(device_rate, channels);
        (queue, output)
    }

    fn push(output: &Output, queue: &mut ring::Producer<[f32; 2]>, frames: impl Iterator<Item = [f32; 2]>) {
        for frame in frames {
            queue.push(frame).unwrap();
            output.shared.pushed.fetch_add(1, Ordering::Release);
        }
    }

    #[test]
    fn it_stays_silent_until_the_buffer_holds_the_target() {
        let (mut queue, mut out) = output(48_000.0, 48_000.0, 2);
        let mut buffer = [1.0f32; 480];
        push(&out, &mut queue, (0..1000).map(|_| [0.5, 0.5]));
        out.render(&mut buffer);
        assert!(buffer.iter().all(|s| *s == 0.0));
        assert!(!out.shared.playing.load(Ordering::Relaxed));
        // 1440 frames are 30 ms: with them the renderer starts
        push(&out, &mut queue, (0..500).map(|_| [0.5, 0.5]));
        out.render(&mut buffer);
        assert!(out.shared.playing.load(Ordering::Relaxed));
        assert!(buffer.iter().all(|s| (*s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn a_dry_buffer_counts_an_underrun_and_waits_for_the_target_again() {
        let (mut queue, mut out) = output(48_000.0, 48_000.0, 2);
        push(&out, &mut queue, (0..1500).map(|_| [0.25, -0.25]));
        let mut buffer = [0.0f32; 2 * 960];
        out.render(&mut buffer);
        // 1500 frames minus the 960 played leave 540: the next callback of 960 frames runs dry
        out.render(&mut buffer);
        assert_eq!(out.shared.underruns.load(Ordering::Relaxed), 1);
        assert!(!out.shared.playing.load(Ordering::Relaxed));
        // The part of the callback after the end is silent
        assert_eq!(buffer[buffer.len() - 1], 0.0);
        // A little sound is not enough to restart
        push(&out, &mut queue, (0..100).map(|_| [0.25, -0.25]));
        out.render(&mut buffer);
        assert!(!out.shared.playing.load(Ordering::Relaxed));
    }

    #[test]
    fn the_fill_above_the_target_speeds_the_playback_a_little() {
        let (mut queue, mut out) = output(48_000.0, 48_000.0, 2);
        // 60 ms: twice the target
        push(&out, &mut queue, (0..2880).map(|i| [(i as f32 / 4000.0).sin(), 0.0]));
        let mut buffer = [0.0f32; 2 * 480];
        out.render(&mut buffer);
        let ppm = f32::from_bits(out.shared.rate_ppm.load(Ordering::Relaxed));
        assert!(ppm > 0.0 && ppm <= MAX_RATE_ADJUST * 1e6, "{ppm}");
    }

    #[test]
    fn the_renderer_reports_the_peak_of_what_it_played() {
        let (mut queue, mut out) = output(48_000.0, 48_000.0, 2);
        push(&out, &mut queue, (0..2000).map(|i| if i == 700 { [0.0, -0.8] } else { [0.25, 0.1] }));
        let mut buffer = [0.0f32; 2 * 960];
        out.render(&mut buffer);
        let peak = f32::from_bits(out.shared.peak.load(Ordering::Relaxed));
        assert!((peak - 0.8).abs() < 0.3 && peak > 0.25, "{peak}");
        // Silence leaves a peak of 0
        let (mut queue, mut quiet) = output(48_000.0, 48_000.0, 2);
        push(&quiet, &mut queue, (0..2000).map(|_| [0.0, 0.0]));
        quiet.render(&mut buffer);
        assert_eq!(quiet.shared.peak.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn the_rate_follows_the_average_fill_and_not_each_tooth_of_the_saw() {
        let (mut queue, mut out) = output(48_000.0, 48_000.0, 2);
        // The buffer starts at the target. Then 20 ms of sound arrives every 20 ms while the device
        // takes 4 ms at a time: the fill saws between about 20 and 40 ms around the target.
        push(&out, &mut queue, (0..1440).map(|_| [0.1, 0.1]));
        let mut buffer = [0.0f32; 2 * 192];
        let mut rates = Vec::new();
        for callback in 0..400 {
            if callback % 5 == 0 {
                push(&out, &mut queue, (0..960).map(|_| [0.1, 0.1]));
            }
            out.render(&mut buffer);
            rates.push(f32::from_bits(out.shared.rate_ppm.load(Ordering::Relaxed)));
        }
        let (min, max) = rates.iter().fold((f32::MAX, f32::MIN), |(lo, hi), r| (lo.min(*r), hi.max(*r)));
        // Without the average the rate would swing over the whole range of 10 000 ppm
        assert!(max - min < 2500.0, "{min} {max}");
    }

    #[test]
    fn an_overfull_buffer_drops_down_to_the_target() {
        let (mut queue, mut out) = output(48_000.0, 48_000.0, 2);
        // 150 ms: past the 90 ms maximum
        push(&out, &mut queue, (0..7200).map(|_| [0.1, 0.1]));
        let mut buffer = [0.0f32; 2 * 480];
        out.render(&mut buffer);
        assert!(out.shared.dropped_frames.load(Ordering::Relaxed) > 5000);
        assert!(out.fill() < 1440);
    }

    #[test]
    fn another_device_rate_is_resampled_and_a_mono_device_gets_the_mean() {
        // 48 kHz source on a 24 kHz mono device: it takes two source frames for each output
        let (mut queue, mut out) = output(48_000.0, 24_000.0, 1);
        push(&out, &mut queue, (0..2000).map(|_| [0.4, 0.2]));
        let mut buffer = [0.0f32; 240];
        out.render(&mut buffer);
        assert!(buffer.iter().all(|s| (*s - 0.3).abs() < 1e-5));
        let popped = out.shared.popped.load(Ordering::Relaxed);
        assert!((480..=490).contains(&popped), "{popped}");
    }

    fn datagram(sequence: u32, value: i16, frames: usize) -> Audio {
        let mut pcm = Vec::new();
        for _ in 0..frames {
            pcm.extend(value.to_le_bytes());
            pcm.extend(value.to_le_bytes());
        }
        Audio { sequence, host_time: 0, pcm }
    }

    fn producer() -> (Producer, ring::Consumer<[f32; 2]>) {
        let (queue, consumer) = ring::channel::<[f32; 2]>(RING_FRAMES);
        (Producer { queue, expected: None, shared: Arc::default() }, consumer)
    }

    #[test]
    fn a_missing_datagram_leaves_silence_of_its_length_and_a_late_one_is_dropped() {
        let (mut producer, _consumer) = producer();
        producer.push(&datagram(10, 16384, 240));
        producer.push(&datagram(13, 16384, 240));
        let s = Arc::clone(&producer.shared);
        // Two datagrams between 10 and 13 are missing: 480 frames of silence
        assert_eq!(s.lost_packets.load(Ordering::Relaxed), 2);
        assert_eq!(s.pushed.load(Ordering::Relaxed), 240 + 480 + 240);
        producer.push(&datagram(12, 16384, 240));
        assert_eq!(s.late_packets.load(Ordering::Relaxed), 1);
        assert_eq!(s.pushed.load(Ordering::Relaxed), 960);
    }

    #[test]
    fn samples_convert_to_floats_in_range() {
        let (mut producer, mut consumer) = producer();
        let mut pcm = Vec::new();
        for (l, r) in [(i16::MAX, i16::MIN), (0, 16384)] {
            pcm.extend(l.to_le_bytes());
            pcm.extend(r.to_le_bytes());
        }
        producer.push(&Audio { sequence: 0, host_time: 0, pcm });
        let first = consumer.pop().unwrap();
        assert!((first[0] - 0.99997).abs() < 1e-4 && first[1] == -1.0);
        assert_eq!(consumer.pop().unwrap(), [0.0, 0.5]);
    }

    /// A device that reports numbers, as AAudio does on a Quest.
    struct ReportingSink;

    impl AudioSink for ReportingSink {
        fn underrun_count(&self) -> Option<u32> {
            Some(3)
        }
        fn output_latency(&self) -> Option<std::time::Duration> {
            Some(std::time::Duration::from_millis(21))
        }
        fn frames_per_burst(&self) -> Option<u32> {
            Some(96)
        }
    }

    /// A device that reports nothing, as the desktop's cpal backend does for the count.
    struct SilentSink;

    impl AudioSink for SilentSink {}

    fn player(sink: Box<dyn AudioSink>) -> Player {
        let (producer, _consumer) = producer();
        let shared = Arc::clone(&producer.shared);
        Player { producer: Mutex::new(producer), shared, source_rate: 48_000.0, sink }
    }

    #[test]
    fn the_stats_carry_the_devices_underruns_apart_from_the_buffers() {
        let reporting = player(Box::new(ReportingSink));
        // The jitter buffer ran dry twice. The device's count is its own.
        reporting.shared.underruns.store(2, Ordering::Relaxed);
        let s = stats_of(&reporting, false);
        assert_eq!((s.underruns, s.device_underruns, s.frames_per_burst), (2, Some(3), Some(96)));
        assert!((s.output_latency_ms.unwrap() - 21.0).abs() < 1e-3);
        let line = stat_lines_of(&s)[0].1.clone();
        assert!(line.contains("2 buffer underruns, 3 device underruns, device latency 21.0 ms"), "{line}");

        // A platform that keeps no count shows a dash and not a zero
        let s = stats_of(&player(Box::new(SilentSink)), false);
        assert_eq!((s.device_underruns, s.output_latency_ms, s.frames_per_burst), (None, None, None));
        assert!(stat_lines_of(&s)[0].1.contains("- device underruns, device latency - ms"));
    }
}
