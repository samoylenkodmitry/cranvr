//! The eye stream's encode time while a display's stream shares the Mac's video encoder.
//!
//! The Mac has one engine for HEVC encodes. Every session of every process takes turns on it, so a
//! display's frame of 3840 x 2160 can make an eye frame wait. The test runs both streams in real time,
//! as the host does: the eye stream at 120 Hz with the game's landscape from `foveation_levels.rs`,
//! through the host's eye encoder, at most two frames in flight; and a display of 3840 x 2160 at 40
//! Mbps through the host's own `Feed` and `DisplayEncoder`, with the made-up desktop of
//! `display_quality.rs` in 2 x 2 copies. Each run reports the eye's encode time (mean, median, p95,
//! p99, the most), the eye frames that took longer than a frame at 120 Hz, the turns that found two
//! eye frames in flight (the host's "encoder busy") and the display's encode time.
//!
//! Run it with nothing else on the encoder (no other host that streams):
//! `cargo test --release -p cranvr-host encoder_sharing -- --ignored --nocapture`.
//! `CRANVR_SHARE_RUNS` lists the runs, each `LEVEL:PICTURES[:MOTION]` (LEVEL off, low, medium or high;
//! PICTURES a second of the display, 0 for none; MOTION busy, the default, or video). The default is
//! `medium:0,medium:30,medium:60,medium:60:video,off:0,off:60`. `CRANVR_SHARE_SECONDS` sets the
//! length of each run (default 20). `CRANVR_SHARE_WIDTH=2560` gives the display the size that the
//! client asks for in a game (2560 x 1440). `CRANVR_SHARE_DISPLAY_RT=1` runs the display's session in
//! real time, as the host did before.

use super::*;
use crate::foveation_levels::{draw, landscape, Variant};
use crate::render::Renderer;
use cranvr_common::foveation::FoveationLevel;
use std::sync::atomic::AtomicBool;

/// The display's bitrate: what the host gives a display of 3840 x 2160 (the owner's log)
const DISPLAY_MBPS: u64 = 40;
/// The eye stream's rate and its bitrate on the cable
const EYE_FPS: f32 = 120.0;
const EYE_MBPS: u64 = 40;
/// The frames that the host lets the eye encoder hold (`MAX_IN_FLIGHT` in main.rs)
const IN_FLIGHT: usize = 2;

struct Share {
    label: String,
    eye_ms: Vec<f64>,
    eye_frames: usize,
    busy: usize,
    display_ms: Vec<f64>,
    display_pictures: usize,
    seconds: f64,
}

fn percentile(values: &[f64], share: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted.get(((sorted.len() as f64 * share) as usize).min(sorted.len().saturating_sub(1))).copied().unwrap_or(0.0)
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len().max(1) as f64
}

/// The display's pictures at `fps` a second until `stop`: the busy desktop (a spinner and a line
/// that grows, as a terminal at work) or a video in its page window. Returns the encode times.
fn display_loop(fps: u32, video: bool, stop: &AtomicBool) -> (Vec<f64>, usize) {
    let desktop = Desktop::new();
    let (sender, encoded) = mpsc::channel::<EncodedFrame>();
    // `CRANVR_SHARE_WIDTH=2560` gives the display of a game (2560 x 1440)
    let width: usize = std::env::var("CRANVR_SHARE_WIDTH").ok().and_then(|v| v.parse().ok()).unwrap_or(WIDTH * 2);
    let size = [width as u32, (width * 9 / 16) as u32];
    let mut display = DisplayEncoder::new(size, fps as i32, DISPLAY_MBPS, sender).expect("display encoder");
    // `CRANVR_SHARE_DISPLAY_RT=1` runs the display's session in real time, as the host did before
    if std::env::var("CRANVR_SHARE_DISPLAY_RT").is_ok_and(|v| v == "1") {
        display.encoder.set_real_time(true).expect("real time on");
    }
    let start = Instant::now();
    let mut feed = Feed::new(start, DISPLAY_MBPS * 1_000_000);
    let (mut source, mut chroma) = (vec![0u8; WIDTH * HEIGHT], vec![0u8; WIDTH * HEIGHT / 2]);
    let (mut state, mut seq, mut times) = (State::default(), 0u64, Vec::new());
    let period = Duration::from_secs(1) / fps;
    let (decode, mut decoder) = (decodes(), Decoding::default());
    while !stop.load(Ordering::Relaxed) {
        let due = start + period * seq as u32;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        (state.spinner, state.typed) = (state.spinner + 1, state.typed + 1);
        if video {
            state.video += 1;
        }
        desktop.picture(state, &mut source, &mut chroma);
        let mut buffer = display.encoder.pixel_buffer().expect("a display buffer");
        let (w, h) = (size[0] as usize, size[1] as usize);
        buffer.with_planes_mut(|y, ys, uv, uvs| {
            // Copies of the desktop across and down, cut at the picture's edges
            for (down, across) in (0..2).flat_map(|down| (0..2).map(move |across| (down, across))) {
                let cols = WIDTH.min(w.saturating_sub(across * WIDTH));
                for row in 0..HEIGHT.min(h.saturating_sub(down * HEIGHT)) {
                    let at = (down * HEIGHT + row) * ys + across * WIDTH;
                    y[at..at + cols].copy_from_slice(&source[row * WIDTH..row * WIDTH + cols]);
                }
                for row in 0..(HEIGHT / 2).min((h / 2).saturating_sub(down * HEIGHT / 2)) {
                    let at = (down * HEIGHT / 2 + row) * uvs + across * WIDTH;
                    uv[at..at + cols].copy_from_slice(&chroma[row * WIDTH..row * WIDTH + cols]);
                }
            }
        });
        seq += 1;
        let now = Instant::now();
        // The host's desktop loop: a picture goes in while the encoder holds none
        if display.in_flight() == 0 {
            if let Some(picture) = feed.next(Some((seq, buffer)), now) {
                display.encode(None, &picture, seq).expect("display encode");
                feed.took(&picture, now);
            }
        }
        for frame in encoded.try_iter() {
            feed.sent(frame.data.len(), frame.is_idr, Instant::now());
            times.push(frame.encode_time.as_secs_f64() * 1000.0);
            if decode {
                decoder.decode(&frame);
            }
        }
    }
    drop(display);
    times.extend(encoded.try_iter().map(|frame| frame.encode_time.as_secs_f64() * 1000.0));
    let count = times.len();
    (times, count)
}

/// `CRANVR_SHARE_DECODE=1` decodes both streams on the Mac too, as the probe of `scripts/e2e.sh` does
fn decodes() -> bool {
    std::env::var("CRANVR_SHARE_DECODE").is_ok_and(|v| v == "1")
}

/// A VideoToolbox decoder of one stream that reads no pixel back
#[derive(Default)]
struct Decoding {
    decoder: Option<(Decoder, mpsc::Receiver<DecodedFrame>)>,
}

impl Decoding {
    fn decode(&mut self, frame: &EncodedFrame) {
        if self.decoder.is_none() {
            let (sender, decoded) = mpsc::channel();
            let config = frame.config_nals.as_ref().expect("a key frame first");
            self.decoder = Some((Decoder::new(config, Vec::new(), sender).expect("decoder"), decoded));
        }
        let (decoder, decoded) = self.decoder.as_mut().unwrap();
        decoder.decode(frame.timestamp_ns, &frame.data).expect("decode");
        decoded.try_iter().for_each(drop);
    }
}

/// One run: the eye stream of `level` for `seconds`, beside a display at `display_fps` (none at 0).
fn share(level: FoveationLevel, display_fps: u32, video: bool, seconds: f64) -> Share {
    let stop = Arc::new(AtomicBool::new(false));
    let display = (display_fps > 0).then(|| {
        let stop = Arc::clone(&stop);
        thread::spawn(move || display_loop(display_fps, video, &stop))
    });
    // The display's session starts first, as when the client shows a display before a game
    thread::sleep(Duration::from_millis(500));
    let variant = Variant::Level(level);
    let (foveation, picture) = (variant.foveation(), variant.picture());
    let (sender, encoded) = mpsc::channel::<EncodedFrame>();
    let encoder = Encoder::new(&crate::eye_encoder_config([picture[0] * 2, picture[1]], EYE_FPS, true, EYE_MBPS * 1_000_000, false), sender).expect("eye encoder");
    let decode = decodes();
    let collector = thread::spawn(move || {
        let mut decoder = Decoding::default();
        encoded
            .iter()
            .map(|frame| {
                if decode {
                    decoder.decode(&frame);
                }
                frame.encode_time.as_secs_f64() * 1000.0
            })
            .collect::<Vec<f64>>()
    });
    let mut renderer = Renderer::new().expect("renderer");
    let land = landscape(6144, 3072);
    let mut time_line = crate::TimeLine::new(EYE_FPS);
    let period = Duration::from_secs_f64(1.0 / f64::from(EYE_FPS));
    let start = Instant::now();
    let (mut n, mut busy, mut frames) = (0u32, 0, 0);
    while start.elapsed().as_secs_f64() < seconds {
        let due = start + period * n;
        n += 1;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        if encoder.in_flight() >= IN_FLIGHT {
            busy += 1;
            continue;
        }
        let buffer = encoder.pixel_buffer().expect("an eye buffer");
        let at = due.duration_since(start).as_secs_f32();
        draw(&mut renderer, &buffer, Some(&land), at, picture, foveation.as_ref());
        encoder.encode(&buffer, time_line.at(Instant::now()), u64::from(n), frames == 0).expect("eye encode");
        frames += 1;
    }
    let elapsed = start.elapsed().as_secs_f64();
    drop(encoder);
    let eye_ms = collector.join().expect("the collector");
    stop.store(true, Ordering::Relaxed);
    let (display_ms, display_pictures) = display.map(|d| d.join().expect("the display")).unwrap_or_default();
    let motion = if display_fps == 0 { String::new() } else if video { " video".into() } else { " busy".into() };
    Share {
        label: format!("{} {}x{}, display {display_fps}{motion}", level.name(), picture[0] * 2, picture[1]),
        eye_frames: eye_ms.len(),
        eye_ms,
        busy,
        display_ms,
        display_pictures,
        seconds: elapsed,
    }
}

fn report(s: &Share) -> String {
    let frame_ms = 1000.0 / f64::from(EYE_FPS);
    let over = s.eye_ms.iter().filter(|&&ms| ms > frame_ms).count();
    let display = if s.display_ms.is_empty() {
        "no display".to_string()
    } else {
        format!(
            "display {:.1} pictures a second, encode {:.2} ms mean, {:.2} ms p95",
            s.display_pictures as f64 / s.seconds,
            mean(&s.display_ms),
            percentile(&s.display_ms, 0.95)
        )
    };
    format!(
        "{:<36} eye {:.1} fps, encode {:.2} ms mean, {:.2} median, {:.2} p95, {:.2} p99, {:.2} most; {:.1}% over {:.2} ms; {} turns found the encoder busy; {display}",
        s.label,
        s.eye_frames as f64 / s.seconds,
        mean(&s.eye_ms),
        percentile(&s.eye_ms, 0.5),
        percentile(&s.eye_ms, 0.95),
        percentile(&s.eye_ms, 0.99),
        s.eye_ms.iter().copied().fold(0.0, f64::max),
        100.0 * over as f64 / s.eye_ms.len().max(1) as f64,
        frame_ms,
        s.busy,
    )
}

#[test]
#[ignore = "runs in real time for minutes and needs the encoder to itself"]
fn encoder_sharing() {
    let runs = std::env::var("CRANVR_SHARE_RUNS").unwrap_or_else(|_| "medium:0,medium:30,medium:60,medium:60:video,off:0,off:60".into());
    let seconds = std::env::var("CRANVR_SHARE_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(20.0);
    for run in runs.split(',') {
        let parts: Vec<&str> = run.trim().split(':').collect();
        let level = FoveationLevel::ALL.into_iter().find(|l| l.name().eq_ignore_ascii_case(parts[0])).expect("a level");
        let fps = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);
        let video = parts.get(2).is_some_and(|m| *m == "video");
        println!("{}", report(&share(level, fps, video, seconds)));
    }
}
