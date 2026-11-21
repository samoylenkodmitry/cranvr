//! The quality of the eye stream after the host's encoder and a decoder.
//!
//! The test renders moving frames as the frame loop does, into the encoder's own buffers: the host's
//! test scene with a head that turns (`Renderer::render`), or a game's eye images that pan as a head
//! turns in a game (`Renderer::compose` of a made-up landscape, with the default foveation). The frames
//! go to an encoder made by `eye_encoder_config`, on the time line of `TimeLine`. A VideoToolbox
//! decoder decodes the stream, and the test compares each decoded frame with the frame that went in
//! (PSNR of the Y plane). It also counts the bitrate against the target, the encode time and the
//! time between the encoder's frames.
//!
//! `eye_quality_curve` runs at the headset's rate in real time and prints the numbers. It is ignored:
//! `cargo test --release -p cranvr-host eye_quality_curve -- --ignored --nocapture`. The environment
//! sets the run: `CRANVR_EYE_FPS` (default 120), `CRANVR_EYE_MBPS` (default 100), `CRANVR_EYE_SCENE`
//! (`game` or `pattern`), `CRANVR_EYE_SPACING=1ms` for the time line that the host had before,
//! `CRANVR_EYE_STRICT=1` for the low-latency rate control and `CRANVR_EYE_SECONDS`.

use super::*;
use crate::render::{ComposeParams, Params, Renderer};
use cranvr_common::foveation::FoveationConfig;
use cranvr_vt::{DecodedFrame, Decoder};
use std::collections::HashMap;

/// The Quest 3's eye at the size that the client asks for
const EYE: [u32; 2] = [1680, 1760];
/// The views' tangents: left, right, up, down
const FOV: [[f32; 4]; 2] = [[-1.0, 0.9, 0.95, -1.15], [-0.9, 1.0, 0.95, -1.15]];

/// What the frames show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scene {
    /// The host's own test scene, with a head that turns
    Pattern,
    /// A game's eye images that pan as the head turns: a made-up landscape
    Game,
}

/// How the frames stand on the encoder's time line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Spacing {
    /// The frame loop's `TimeLine`
    Real,
    /// As the host did before: 1 ms apart
    OneMs,
}

/// A small random number source
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
}

/// A landscape for the game's eyes: a sky, hills and ground with fine detail, posts with hard edges
/// and small bright marks, in a BGRA image that other processes could open as a game's swapchain.
fn landscape(width: usize, height: usize) -> PixelBuffer {
    let mut rng = Rng(5);
    // A grid of random values; the picture mixes it at three scales
    const GRID: usize = 256;
    let grid: Vec<f32> = (0..GRID * GRID).map(|_| (rng.next() % 1000) as f32 / 1000.0).collect();
    let noise = |x: f32, y: f32| {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |i: f32, j: f32| grid[(j as usize % GRID) * GRID + (i as usize % GRID)];
        let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1.0, y0) * fx;
        let bottom = at(x0, y0 + 1.0) * (1.0 - fx) + at(x0 + 1.0, y0 + 1.0) * fx;
        top * (1.0 - fy) + bottom * fy
    };
    let posts: Vec<(usize, usize)> = (0..60).map(|_| (rng.next() as usize % width, 4 + rng.next() as usize % 12)).collect();
    let mut image = PixelBuffer::new_bgra_shared(width, height).expect("a landscape image");
    image.with_bgra_mut(|data, stride| {
        for y in 0..height {
            let v = y as f32 / height as f32;
            for x in 0..width {
                let u = x as f32 / width as f32;
                let detail = noise(u * 40.0, v * 20.0) * 0.5 + noise(u * 160.0, v * 80.0) * 0.3 + noise(u * 640.0, v * 320.0) * 0.2;
                let hill = 0.45 + 0.08 * (noise(u * 6.0, 3.0) - 0.5);
                let (r, g, b) = if v < hill {
                    // Sky with clouds
                    let cloud = (noise(u * 12.0, v * 8.0) - 0.4).max(0.0) * 1.5;
                    (0.45 + cloud, 0.6 + cloud, 0.9 + cloud * 0.3)
                } else {
                    // Grass and rock
                    (0.25 + 0.35 * detail, 0.35 + 0.4 * detail, 0.15 + 0.2 * detail)
                };
                let px = &mut data[y * stride + x * 4..y * stride + x * 4 + 4];
                let byte = |c: f32| (c.clamp(0.0, 1.0) * 255.0) as u8;
                px.copy_from_slice(&[byte(b), byte(g), byte(r), 255]);
            }
        }
        // Posts with hard edges and a bright mark at the top of each
        for &(x0, w) in &posts {
            for y in height / 3..height {
                for x in x0..(x0 + w).min(width) {
                    let shade = if (y / 6) % 2 == 0 { 30 } else { 220 };
                    data[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&[shade, shade, shade, 255]);
                }
            }
        }
    });
    image
}

/// What a run measured.
#[derive(Debug, Default)]
struct Measure {
    frames: usize,
    psnr: Vec<f64>,
    bytes: usize,
    seconds: f64,
    /// From the encode call to the encoded frame, in ms
    encode_ms: Vec<f64>,
    /// Between two encoded frames, in ms
    interval_ms: Vec<f64>,
    /// Turns that found the encoder with two frames in it
    busy: usize,
    /// The largest frame other than key frames, in bytes
    largest: usize,
    keys: usize,
}

fn percentile(values: &[f64], share: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted.get(((sorted.len() as f64 * share) as usize).min(sorted.len().saturating_sub(1))).copied().unwrap_or(0.0)
}

/// The Y plane of a frame, `width` bytes a row.
fn luma_of(buffer: &mut PixelBuffer) -> Vec<u8> {
    let (width, height) = (buffer.width(), buffer.height());
    buffer.with_planes_mut(|y, ys, _, _| (0..height).flat_map(|row| y[row * ys..row * ys + width].to_vec()).collect())
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let sum: u64 = a.iter().zip(b).map(|(&p, &q)| u64::from(p.abs_diff(q)).pow(2)).sum();
    let mse = sum as f64 / a.len() as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

/// A run: `frames` frames of `scene` at `fps`, encoded at `mbps`, with the low-latency rate control
/// when `strict`. `paced` runs at the rate in real time; otherwise the time line advances by a frame
/// each turn and the test waits for each frame. `pause` leaves out the frames of that range of
/// seconds, as a game that holds its picture.
#[derive(Clone, Copy)]
struct Setup {
    scene: Scene,
    spacing: Spacing,
    fps: f32,
    mbps: u64,
    strict: bool,
    frames: usize,
    paced: bool,
    pause: Option<(f64, f64)>,
}

fn measure(setup: Setup) -> Measure {
    let Setup { scene, spacing, fps, mbps, strict, frames, paced, pause } = setup;
    let foveation = Foveation::new(EYE, &FoveationConfig::default());
    let [eye_w, eye_h] = foveation.optimized;
    let (sender, encoded) = mpsc::channel();
    let encoder = Encoder::new(&eye_encoder_config([eye_w * 2, eye_h], fps, true, mbps * 1_000_000, strict), sender).expect("encoder");
    let mut renderer = Renderer::new().expect("renderer");
    let land = (scene == Scene::Game).then(|| landscape(if paced { 4096 } else { 2048 }, if paced { 2048 } else { 1024 }));
    // The decoder and the comparison run on a thread of their own, as the headset's decoder does
    let (source_tx, source_rx) = mpsc::channel::<(u64, Vec<u8>)>();
    let checker = thread::spawn(move || {
        let (decoded_tx, decoded_rx) = mpsc::channel::<DecodedFrame>();
        let mut decoder: Option<Decoder> = None;
        let mut sources: HashMap<u64, Vec<u8>> = HashMap::new();
        let mut m = Measure::default();
        let mut last_out: Option<Instant> = None;
        for frame in encoded {
            let frame: EncodedFrame = frame;
            let now = Instant::now();
            if let Some(last) = last_out {
                m.interval_ms.push(now.duration_since(last).as_secs_f64() * 1000.0);
            }
            last_out = Some(now);
            m.frames += 1;
            m.bytes += frame.data.len();
            m.keys += usize::from(frame.is_idr);
            if !frame.is_idr {
                m.largest = m.largest.max(frame.data.len());
            }
            m.encode_ms.push(frame.encode_time.as_secs_f64() * 1000.0);
            if decoder.is_none() {
                decoder = Some(Decoder::with_luma(frame.config_nals.as_ref().expect("a key frame first"), decoded_tx.clone()).expect("decoder"));
            }
            decoder.as_mut().unwrap().decode(frame.timestamp_ns, &frame.data).expect("decode");
            let picture = decoded_rx.recv_timeout(Duration::from_secs(2)).expect("a decoded frame");
            sources.extend(source_rx.try_iter());
            let source = sources.remove(&picture.timestamp_ns).expect("the source of the frame");
            m.psnr.push(psnr(&source, &picture.luma));
        }
        m
    });
    let start = Instant::now();
    let period = Duration::from_secs_f64(1.0 / f64::from(fps));
    let mut time_line = TimeLine::new(fps);
    let mut busy = 0;
    for n in 0..frames {
        let t = period * n as u32;
        if pause.is_some_and(|(from, to)| (from..to).contains(&t.as_secs_f64())) {
            continue;
        }
        let now = if paced {
            if let Some(wait) = (start + t).checked_duration_since(Instant::now()) {
                thread::sleep(wait);
            }
            if encoder.in_flight() >= 2 {
                busy += 1;
                continue;
            }
            Instant::now()
        } else {
            while encoder.in_flight() > 0 {
                thread::sleep(Duration::from_micros(200));
            }
            start + t
        };
        let mut buffer = encoder.pixel_buffer().expect("a buffer");
        let seconds = t.as_secs_f32();
        match &land {
            None => {
                // The head turns 40 degrees to each side and nods, once in 4 s
                let (yaw, pitch) = (0.35 * (seconds * std::f32::consts::TAU / 4.0).sin(), 0.1 * (seconds * std::f32::consts::TAU / 3.0).sin());
                let q = glam::Quat::from_euler(glam::EulerRot::YXZ, yaw, pitch, 0.0);
                let mut params = Params {
                    fov: FOV,
                    head_q: q.to_array(),
                    head_p: [0.0, 1.6, 0.0, 0.0],
                    eye_q: [[0.0, 0.0, 0.0, 1.0]; 2],
                    eye_p: [[-0.032, 0.0, 0.0, 0.0], [0.032, 0.0, 0.0, 0.0]],
                    eye_w,
                    eye_h,
                    full_range: 1,
                    ..Default::default()
                };
                params.set_foveation(Some(&foveation));
                renderer.render(&buffer, &params).expect("render");
            }
            Some(land) => {
                // The view pans across the landscape as a head that turns about 60 degrees a second
                let x = 0.3 + 0.2 * (seconds * std::f32::consts::TAU / 4.0).sin();
                let y = 0.15 + 0.05 * (seconds * std::f32::consts::TAU / 3.0).sin();
                let mut params = ComposeParams {
                    fov: FOV,
                    src_fov: FOV,
                    src_rect: [[x, y, 0.35, 0.7], [x + 0.01, y, 0.35, 0.7]],
                    eye_w,
                    eye_h,
                    full_range: 1,
                    ..Default::default()
                };
                params.set_foveation(Some(&foveation));
                renderer.compose(&buffer, land, land, &params).expect("compose");
            }
        }
        let presentation = match spacing {
            Spacing::Real => time_line.at(now),
            Spacing::OneMs => Duration::from_millis(n as u64),
        };
        let stamp = n as u64 + 1;
        source_tx.send((stamp, luma_of(&mut buffer))).unwrap();
        encoder.encode(&buffer, presentation, stamp, n == 0).expect("encode");
    }
    let seconds = start.elapsed().as_secs_f64();
    drop(encoder);
    drop(source_tx);
    let mut m = checker.join().expect("the checker");
    m.busy = busy;
    m.seconds = if paced { seconds } else { frames as f64 / f64::from(fps) };
    m
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len().max(1) as f64
}

fn report(label: &str, m: &Measure, mbps: u64) -> String {
    let sent = m.bytes as f64 * 8.0 / m.seconds / 1e6;
    format!(
        "{label}: {} frames, {} key; PSNR mean {:.1} dB, lowest {:.1} dB; {:.1} Mbps of {mbps} ({:.0} percent), {:.1} KB a frame, {:.0} KB the largest; encode {:.2} ms median, {:.2} ms p95, {:.2} ms max; between frames {:.2} ms median, {:.2} ms p95, {:.2} ms max; {} turns found the encoder full",
        m.frames,
        m.keys,
        mean(&m.psnr),
        m.psnr.iter().copied().fold(f64::INFINITY, f64::min),
        sent,
        sent * 100.0 / mbps as f64,
        m.bytes as f64 / m.frames.max(1) as f64 / 1000.0,
        m.largest as f64 / 1000.0,
        percentile(&m.encode_ms, 0.5),
        percentile(&m.encode_ms, 0.95),
        percentile(&m.encode_ms, 1.0),
        percentile(&m.interval_ms, 0.5),
        percentile(&m.interval_ms, 0.95),
        percentile(&m.interval_ms, 1.0),
        m.busy
    )
}

/// The eye stream spends its bitrate on a moving picture: a game's eyes that pan come out sharp,
/// near the target bitrate. The old time line, with frames 1 ms apart, sent a tenth of it.
#[test]
fn the_eye_stream_spends_its_bitrate_on_a_moving_picture() {
    let (fps, mbps, frames) = (120.0, 100, 120);
    let setup = Setup { scene: Scene::Game, spacing: Spacing::Real, fps, mbps, strict: false, frames, paced: false, pause: None };
    let real = measure(setup);
    let old = measure(Setup { spacing: Spacing::OneMs, ..setup });
    let sent = |m: &Measure| m.bytes as f64 * 8.0 / m.seconds / 1e6;
    println!("{}", report("real time line", &real, mbps));
    println!("{}", report("1 ms apart", &old, mbps));
    assert!(sent(&real) > 0.4 * mbps as f64, "the stream sent {:.1} Mbps of {mbps}", sent(&real));
    assert!(sent(&real) < 1.5 * mbps as f64, "the stream sent {:.1} Mbps of {mbps}", sent(&real));
    assert!(sent(&old) < 0.2 * mbps as f64, "the old time line sent {:.1} Mbps", sent(&old));
    assert!(mean(&real.psnr) > mean(&old.psnr) + 3.0, "PSNR {:.1} dB against {:.1} dB before", mean(&real.psnr), mean(&old.psnr));
}

#[test]
#[ignore = "runs in real time; prints the eye stream's numbers"]
fn eye_quality_curve() {
    let number = |name: &str, default: f64| std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default);
    let fps = number("CRANVR_EYE_FPS", 120.0) as f32;
    let mbps = number("CRANVR_EYE_MBPS", 100.0) as u64;
    let seconds = number("CRANVR_EYE_SECONDS", 10.0);
    let scene = if std::env::var("CRANVR_EYE_SCENE").is_ok_and(|v| v == "pattern") { Scene::Pattern } else { Scene::Game };
    let spacing = if std::env::var("CRANVR_EYE_SPACING").is_ok_and(|v| v == "1ms") { Spacing::OneMs } else { Spacing::Real };
    let strict = std::env::var("CRANVR_EYE_STRICT").is_ok_and(|v| v == "1");
    let pause = std::env::var("CRANVR_EYE_PAUSE").is_ok_and(|v| v == "1").then_some((4.0, 5.0));
    let m = measure(Setup { scene, spacing, fps, mbps, strict, frames: (seconds * f64::from(fps)) as usize, paced: true, pause });
    println!("{}", report(&format!("{scene:?}, {spacing:?}, {fps} Hz{}", if strict { ", strict" } else { "" }), &m, mbps));
}
