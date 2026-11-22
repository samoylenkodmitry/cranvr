//! The foveation levels measured in the view, as the headset shows the picture (issue #93).
//!
//! Each run composes a game's eyes that pan, as `Renderer::compose` does for a game, into the
//! encoder's buffer of the level. VideoToolbox encodes the frames at the stream's rate and a
//! VideoToolbox decoder decodes them. The test then undoes the warp on the CPU, as the headset's
//! stream shader does (`view_to_frame` and a bilinear read), and compares each pixel of the full view
//! with a reference: the same eyes composed with no foveation at the full eye size. It reports the
//! PSNR of the Y plane in the center, where every level keeps the full density, in the middle ring
//! and at the edges, the SSIM of the center, the bits and the encode and decode times.
//!
//! A level `Off` runs at the eye that the host takes without foveation at 120 Hz. The run named
//! `medium alvr` keeps ALVR's own shift of the region, as the stream had it before the host centered
//! the region on the view. The run named
//! `edge blur` stands in for a quantizer per region: VideoToolbox has no public way to give one (see
//! the PR of #93). It keeps every pixel of the eye and blurs the edges as much as the level Medium
//! squeezes them, so the encoder spends few bits there.
//!
//! `cargo test --release -p cranvr-host foveation_levels -- --ignored --nocapture`. The environment
//! sets the run: `CRANVR_FOV_MBPS` (a comma list, default `40,12`), `CRANVR_FOV_FRAMES` (default
//! 240), `CRANVR_FOV_PACED=1` for real time with frames in flight, and `CRANVR_FOV_DUMP=FOLDER` for
//! crops of the center and of an edge as PGM pictures. `CRANVR_FOV_SCENE=pattern` runs the host's own
//! test scene with a head that turns, which the shader draws at each pixel's place in the view.

use super::*;
use crate::render::{ComposeParams, Params, Renderer};
use cranvr_common::foveation::{view_to_frame, Foveation, FoveationLevel};
use cranvr_vt::{DecodedFrame, Decoder};
use std::{collections::HashMap, path::PathBuf};

/// The Quest 3's eye at the size that the client asks for
const EYE: [u32; 2] = [1680, 1760];
/// The eye that the host takes without foveation at 120 Hz (`default_eye_width`), with the aspect
/// of the client's eye
const OFF_EYE: [u32; 2] = [1248, 1308];
/// A Quest 3's views as tangents: left, right, up, down. An eye sees 54 degrees out, 40 in, 44 up
/// and 55 down.
const FOV: [[f32; 4]; 2] = [[-1.3764, 0.8391, 0.9657, -1.4281], [-0.8391, 1.3764, 0.9657, -1.4281]];
/// Every fourth frame is compared, after the first few
const EVERY: usize = 4;
const SKIP: usize = 8;

/// The regions of an eye's view by the angle from straight ahead on each axis: the center within
/// 15 degrees, where every level keeps the full density, the edge beyond 35 degrees, and the middle
/// between them. `mx` runs from the eye's outer edge (the right eye mirrored), `v` from the top.
fn region(mx: f32, v: f32) -> Region {
    let [left, right, up, down] = FOV[0];
    let (tx, ty) = (left + mx * (right - left), up - v * (up - down));
    let (center, edge) = (15f32.to_radians().tan(), 35f32.to_radians().tan());
    if tx.abs() <= center && ty.abs() <= center {
        Region::Center
    } else if tx.abs() > edge || ty.abs() > edge {
        Region::Edge
    } else {
        Region::Middle
    }
}

/// The rows of the center's band, where the strips of `Errors` are counted
fn center_rows(v: f32) -> bool {
    let [_, _, up, down] = FOV[0];
    (up - v * (up - down)).abs() <= 15f32.to_radians().tan()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Region {
    Center,
    Middle,
    Edge,
}

/// What one run encodes.
#[derive(Clone, Copy, Debug)]
enum Variant {
    Level(FoveationLevel),
    /// A level with ALVR's own shift, as the stream had it before the host centered the region
    AlvrShift(FoveationLevel),
    /// Every pixel of an eye of this size, with the edges blurred by the level Medium's squeeze
    EdgeBlur([u32; 2]),
}

impl Variant {
    fn label(self) -> String {
        match self {
            Variant::Level(level) => level.name().into(),
            Variant::AlvrShift(level) => format!("{} alvr", level.name()),
            Variant::EdgeBlur(eye) => format!("edge blur {}", eye[0]),
        }
    }

    fn eye(self) -> [u32; 2] {
        match self {
            Variant::Level(FoveationLevel::Off) => OFF_EYE,
            Variant::Level(_) | Variant::AlvrShift(_) => EYE,
            Variant::EdgeBlur(eye) => eye,
        }
    }

    fn foveation(self) -> Option<Foveation> {
        match self {
            Variant::Level(level) => level.config().map(|config| Foveation::new(EYE, &config.centered(FOV[0]))),
            Variant::AlvrShift(level) => level.config().map(|config| Foveation::new(EYE, &config)),
            Variant::EdgeBlur(_) => None,
        }
    }

    /// The size of one eye in the encoded frame
    fn picture(self) -> [u32; 2] {
        self.foveation().map_or(self.eye(), |f| f.optimized)
    }
}

/// A small random number source
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
}

/// A landscape with detail at every scale, larger than an eye: a sky with clouds, hills, grass with
/// fine noise, posts with hard edges and thin wires.
fn landscape(width: usize, height: usize) -> PixelBuffer {
    let mut rng = Rng(11);
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
    let posts: Vec<(usize, usize)> = (0..120).map(|_| (rng.next() as usize % width, 3 + rng.next() as usize % 14)).collect();
    let mut image = PixelBuffer::new_bgra_shared(width, height).expect("a landscape image");
    image.with_bgra_mut(|data, stride| {
        for y in 0..height {
            let v = y as f32 / height as f32;
            for x in 0..width {
                let u = x as f32 / width as f32;
                let detail = noise(u * 60.0, v * 30.0) * 0.45 + noise(u * 240.0, v * 120.0) * 0.3 + noise(u * 960.0, v * 480.0) * 0.25;
                let hill = 0.42 + 0.08 * (noise(u * 6.0, 3.0) - 0.5);
                let (r, g, b) = if v < hill {
                    let cloud = (noise(u * 18.0, v * 12.0) - 0.4).max(0.0) * 1.5;
                    (0.45 + cloud, 0.6 + cloud, 0.9 + cloud * 0.3)
                } else {
                    (0.2 + 0.4 * detail, 0.3 + 0.45 * detail, 0.12 + 0.25 * detail)
                };
                let byte = |c: f32| (c.clamp(0.0, 1.0) * 255.0) as u8;
                data[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&[byte(b), byte(g), byte(r), 255]);
            }
        }
        for &(x0, w) in &posts {
            for y in height / 3..height {
                for x in x0..(x0 + w).min(width) {
                    let shade = if (y / 6) % 2 == 0 { 30 } else { 220 };
                    data[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&[shade, shade, shade, 255]);
                }
            }
        }
        // Wires: thin dark lines across the sky
        for k in 0..6 {
            let y0 = height / 8 + k * height / 30;
            for x in 0..width {
                let y = y0 + ((x as f32 / width as f32 * 37.0).sin() * 6.0) as usize;
                data[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&[20, 20, 20, 255]);
            }
        }
    });
    image
}

/// The game's eyes at a time: the view pans across the landscape as a head that turns about 60
/// degrees a second.
fn compose_params(seconds: f32, eye: [u32; 2], foveation: Option<&Foveation>) -> ComposeParams {
    let x = 0.3 + 0.2 * (seconds * std::f32::consts::TAU / 4.0).sin();
    let y = 0.12 + 0.05 * (seconds * std::f32::consts::TAU / 3.0).sin();
    let mut params = ComposeParams {
        fov: FOV,
        src_fov: FOV,
        src_rect: [[x, y, 0.3, 0.7], [x + 0.01, y, 0.3, 0.7]],
        eye_w: eye[0],
        eye_h: eye[1],
        full_range: 1,
        ..Default::default()
    };
    params.set_foveation(foveation);
    params
}

/// The host's own test scene at a time, with a head that turns 40 degrees to each side and nods,
/// once in 4 s: a sky with lines, a floor of checkers. The shader draws each pixel at its place in
/// the view, so no picture of a fixed size is resampled.
fn pattern_params(seconds: f32, eye: [u32; 2], foveation: Option<&Foveation>) -> Params {
    let (yaw, pitch) = (0.35 * (seconds * std::f32::consts::TAU / 4.0).sin(), 0.1 * (seconds * std::f32::consts::TAU / 3.0).sin());
    let q = glam::Quat::from_euler(glam::EulerRot::YXZ, yaw, pitch, 0.0);
    let mut params = Params {
        fov: FOV,
        head_q: q.to_array(),
        head_p: [0.0, 1.6, 0.0, 0.0],
        eye_q: [[0.0, 0.0, 0.0, 1.0]; 2],
        eye_p: [[-0.032, 0.0, 0.0, 0.0], [0.032, 0.0, 0.0, 0.0]],
        eye_w: eye[0],
        eye_h: eye[1],
        full_range: 1,
        ..Default::default()
    };
    params.set_foveation(foveation);
    params
}

/// Draws the frame at `seconds` of the run's scene into `out`: the landscape of a game, or the
/// host's test scene when there is no landscape.
fn draw(renderer: &mut Renderer, out: &PixelBuffer, land: Option<&PixelBuffer>, seconds: f32, eye: [u32; 2], foveation: Option<&Foveation>) {
    match land {
        Some(land) => renderer.compose(out, land, land, &compose_params(seconds, eye, foveation)).expect("compose"),
        None => renderer.render(out, &pattern_params(seconds, eye, foveation)).expect("render"),
    }
}

/// How many pixels of an eye of `eye` pixels one encoded pixel of the level Medium stands for,
/// along one axis, at the view point `t` (0 to 1), at least 1. `medium` is the layout of the full
/// eye, so a smaller eye gets the same density at the edges.
fn squeeze(medium: &Foveation, axis: usize, t: f32, eye: [u32; 2]) -> f32 {
    let at = |t: f32| {
        let mut view = [0.5, 0.5];
        view[axis] = t.clamp(0.0, 1.0);
        medium.view_to_encoded(view)[axis]
    };
    let h = 0.002;
    let slope = (at(t + h) - at(t - h)) / (2.0 * h) * medium.optimized[axis] as f32;
    (eye[axis] as f32 / slope.max(1e-3)).max(1.0)
}

/// Blurs one plane in place with a box along each axis whose width follows the squeeze of the
/// level Medium. `channels` is 1 for Y and 2 for the interleaved CbCr plane, whose pixels each stand
/// for two of the eye's.
fn blur_edges(plane: &mut [u8], stride: usize, size: [usize; 2], channels: usize, eye: [u32; 2], medium: &Foveation) {
    let [w, h] = size;
    let scale = if channels == 2 { 2.0 } else { 1.0 };
    let eye_w = w / 2;
    // Box widths for the columns of one eye and for the rows, in this plane's pixels
    let column: Vec<usize> = (0..eye_w)
        .map(|x| {
            let t = (x as f32 + 0.5) / eye_w as f32;
            (squeeze(medium, 0, t, eye) / scale).round().max(1.0) as usize
        })
        .collect();
    let row: Vec<usize> = (0..h).map(|y| (squeeze(medium, 1, (y as f32 + 0.5) / h as f32, eye) / scale).round().max(1.0) as usize).collect();
    let mut line = vec![0u32; w.max(h) + 1];
    for c in 0..channels {
        // Rows: each pixel takes the mean of a box as wide as its column's squeeze. The right eye
        // is the mirror of the left one in the view, so its outer edge is on the right.
        for y in 0..h {
            line[0] = 0;
            for x in 0..w {
                line[x + 1] = line[x] + u32::from(plane[y * stride + x * channels + c]);
            }
            for x in 0..w {
                let in_eye = x % eye_w;
                let outer = if x >= eye_w { eye_w - 1 - in_eye } else { in_eye };
                let half = column[outer] / 2;
                let (lo, hi) = (x.saturating_sub(half).max(x - in_eye), (x + half + 1).min(x - in_eye + eye_w));
                plane[y * stride + x * channels + c] = ((line[hi] - line[lo]) / (hi - lo) as u32) as u8;
            }
        }
        for x in 0..w {
            line[0] = 0;
            for y in 0..h {
                line[y + 1] = line[y] + u32::from(plane[y * stride + x * channels + c]);
            }
            for y in 0..h {
                let half = row[y] / 2;
                let (lo, hi) = (y.saturating_sub(half), (y + half + 1).min(h));
                plane[y * stride + x * channels + c] = ((line[hi] - line[lo]) / (hi - lo) as u32) as u8;
            }
        }
    }
}

/// The Y plane of a frame, `width` bytes a row.
fn luma_of(buffer: &mut PixelBuffer) -> Vec<u8> {
    let (width, height) = (buffer.width(), buffer.height());
    buffer.with_planes_mut(|y, ys, _, _| (0..height).flat_map(|row| y[row * ys..row * ys + width].to_vec()).collect())
}

/// The squared errors and their count in each region, and the SSIM of the center of each eye.
#[derive(Default)]
struct Errors {
    sse: [f64; 3],
    count: [u64; 3],
    /// The squared error of each of 32 strips across the center's rows (within 15 degrees of straight
    /// ahead), from the outer edge
    strips: [(f64, u64); 32],
    ssim: Vec<f64>,
}

impl Errors {
    fn psnr(&self, region: usize) -> f64 {
        let mse = self.sse[region] / self.count[region].max(1) as f64;
        if mse == 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
    }

    fn all(&self) -> f64 {
        let mse = self.sse.iter().sum::<f64>() / self.count.iter().sum::<u64>().max(1) as f64;
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

/// The decoded frame read at a point of an eye's view: the headset's unwarp with a bilinear read.
fn unwarp(frame: &[u8], size: [usize; 2], foveation: Option<&Foveation>, eye: usize, view: [f32; 2]) -> f32 {
    read(frame, size, view_to_frame(foveation, eye, view))
}

/// The decoded frame at `uv` (0 to 1 of the frame), read bilinear as a GPU's sampler reads it
fn read(frame: &[u8], size: [usize; 2], uv: [f32; 2]) -> f32 {
    let x = (uv[0] * size[0] as f32 - 0.5).clamp(0.0, size[0] as f32 - 1.0);
    let y = (uv[1] * size[1] as f32 - 0.5).clamp(0.0, size[1] as f32 - 1.0);
    let (x0, y0) = (x as usize, y as usize);
    let (x1, y1) = ((x0 + 1).min(size[0] - 1), (y0 + 1).min(size[1] - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let at = |x: usize, y: usize| f32::from(frame[y * size[0] + x]);
    let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
    let bottom = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
    top * (1.0 - fy) + bottom * fy
}

/// SSIM of two pictures of `w` by `h`, in blocks of 8 by 8 that step by 4
fn ssim(a: &[f32], b: &[f32], w: usize, h: usize) -> f64 {
    let (c1, c2) = ((0.01f64 * 255.0).powi(2), (0.03f64 * 255.0).powi(2));
    let mut total = 0.0;
    let mut blocks = 0;
    for by in (0..h.saturating_sub(8)).step_by(4) {
        for bx in (0..w.saturating_sub(8)).step_by(4) {
            let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for y in by..by + 8 {
                for x in bx..bx + 8 {
                    let (p, q) = (f64::from(a[y * w + x]), f64::from(b[y * w + x]));
                    sa += p;
                    sb += q;
                    saa += p * p;
                    sbb += q * q;
                    sab += p * q;
                }
            }
            let n = 64.0;
            let (ma, mb) = (sa / n, sb / n);
            let (va, vb, cov) = (saa / n - ma * ma, sbb / n - mb * mb, sab / n - ma * mb);
            total += ((2.0 * ma * mb + c1) * (2.0 * cov + c2)) / ((ma * ma + mb * mb + c1) * (va + vb + c2));
            blocks += 1;
        }
    }
    total / blocks.max(1) as f64
}

/// Compares a decoded frame with the reference of the full view, both eyes, and adds to `errors`.
/// It returns the left eye's unwarped view, for the crops.
fn compare(decoded: &[u8], size: [usize; 2], foveation: Option<&Foveation>, reference: &[u8], errors: &mut Errors) -> Vec<f32> {
    let [w, h] = [EYE[0] as usize, EYE[1] as usize];
    let mut left = vec![0f32; w * h];
    for eye in 0..2 {
        let mut center = (Vec::new(), Vec::new());
        let (mut cw, mut ch) = (0, 0);
        // The unwarp is one function of each axis, so each column and each row is looked up once
        let columns: Vec<f32> = (0..w).map(|i| view_to_frame(foveation, eye, [(i as f32 + 0.5) / w as f32, 0.5])[0]).collect();
        let rows: Vec<f32> = (0..h).map(|j| view_to_frame(foveation, eye, [0.5, (j as f32 + 0.5) / h as f32])[1]).collect();
        for j in 0..h {
            let v = (j as f32 + 0.5) / h as f32;
            let mut in_row = 0;
            for i in 0..w {
                let u = (i as f32 + 0.5) / w as f32;
                let got = read(decoded, size, [columns[i], rows[j]]);
                let want = f32::from(reference[j * 2 * w + eye * w + i]);
                if eye == 0 {
                    left[j * w + i] = got;
                }
                let mx = if eye == 1 { 1.0 - u } else { u };
                let r = region(mx, v) as usize;
                let d = f64::from(got - want);
                errors.sse[r] += d * d;
                errors.count[r] += 1;
                if r == Region::Center as usize {
                    center.0.push(got);
                    center.1.push(want);
                    in_row += 1;
                }
                if center_rows(v) {
                    let strip = ((mx * 32.0) as usize).min(31);
                    errors.strips[strip].0 += d * d;
                    errors.strips[strip].1 += 1;
                }
            }
            if in_row > 0 {
                cw = in_row;
                ch += 1;
            }
        }
        errors.ssim.push(ssim(&center.0, &center.1, cw, ch));
    }
    left
}

/// A crop of a picture of width `w` as a PGM file
fn write_pgm(path: &std::path::Path, picture: &[f32], w: usize, x0: usize, y0: usize, cw: usize, ch: usize) {
    let mut out = format!("P5\n{cw} {ch}\n255\n").into_bytes();
    for y in y0..y0 + ch {
        out.extend(picture[y * w + x0..y * w + x0 + cw].iter().map(|v| v.round().clamp(0.0, 255.0) as u8));
    }
    std::fs::write(path, out).expect("a crop");
}

struct Run {
    variant: Variant,
    mbps: u64,
    frames: usize,
    bytes: usize,
    seconds: f64,
    encode_ms: Vec<f64>,
    decode_ms: Vec<f64>,
    busy: usize,
    errors: Errors,
}

fn percentile(values: &[f64], share: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted.get(((sorted.len() as f64 * share) as usize).min(sorted.len().saturating_sub(1))).copied().unwrap_or(0.0)
}

fn run(variant: Variant, mbps: u64, frames: usize, paced: bool, land: Option<&PixelBuffer>, dump: Option<&PathBuf>) -> Run {
    let fps = 120.0;
    let foveation = variant.foveation();
    let picture = variant.picture();
    let frame_size = [picture[0] * 2, picture[1]];
    let (sender, encoded) = mpsc::channel();
    let encoder = Encoder::new(&eye_encoder_config(frame_size, fps, true, mbps * 1_000_000, false), sender).expect("encoder");
    // The reference's buffers come from an encoder of the full view's size, which encodes nothing
    let (unused, _) = mpsc::channel();
    let reference_pool = Encoder::new(&eye_encoder_config([EYE[0] * 2, EYE[1]], fps, true, 1_000_000, false), unused).expect("a pool");
    let mut renderer = Renderer::new().expect("renderer");
    let medium = Foveation::new(EYE, &FoveationLevel::Medium.config().unwrap().centered(FOV[0]));
    let (source_tx, source_rx) = mpsc::channel::<(u64, Vec<u8>)>();
    let label = variant.label();
    let dump = dump.cloned();
    let checker = thread::spawn(move || {
        let (decoded_tx, decoded_rx) = mpsc::channel::<DecodedFrame>();
        let mut decoder: Option<Decoder> = None;
        let mut references: HashMap<u64, Vec<u8>> = HashMap::new();
        let (mut errors, mut decode_ms, mut encode_ms, mut bytes, mut count) = (Errors::default(), Vec::new(), Vec::new(), 0, 0);
        let mut last_left = None;
        for frame in encoded {
            let frame: EncodedFrame = frame;
            count += 1;
            bytes += frame.data.len();
            encode_ms.push(frame.encode_time.as_secs_f64() * 1000.0);
            if decoder.is_none() {
                decoder = Some(Decoder::with_luma(frame.config_nals.as_ref().expect("a key frame first"), decoded_tx.clone()).expect("decoder"));
            }
            let started = Instant::now();
            decoder.as_mut().unwrap().decode(frame.timestamp_ns, &frame.data).expect("decode");
            let picture = decoded_rx.recv_timeout(Duration::from_secs(2)).expect("a decoded frame");
            decode_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            references.extend(source_rx.try_iter());
            if let Some(reference) = references.remove(&picture.timestamp_ns) {
                let size = [frame_size[0] as usize, frame_size[1] as usize];
                last_left = Some(compare(&picture.luma, size, foveation.as_ref(), &reference, &mut errors));
                if let Some(dir) = &dump {
                    // The reference's left eye once, for the crops of each run
                    let w = EYE[0] as usize;
                    let left: Vec<f32> = (0..EYE[1] as usize).flat_map(|j| reference[j * 2 * w..j * 2 * w + w].iter().map(|&v| f32::from(v))).collect();
                    write_pgm(&dir.join("reference-center.pgm"), &left, w, 803, 529, 480, 360);
                    write_pgm(&dir.join("reference-edge.pgm"), &left, w, 0, 1240, 480, 360);
                }
            }
        }
        if let (Some(dir), Some(left)) = (&dump, last_left) {
            let name = label.replace(' ', "-");
            write_pgm(&dir.join(format!("{name}-{mbps}-center.pgm")), &left, EYE[0] as usize, 803, 529, 480, 360);
            write_pgm(&dir.join(format!("{name}-{mbps}-edge.pgm")), &left, EYE[0] as usize, 0, 1240, 480, 360);
        }
        (errors, decode_ms, encode_ms, bytes, count)
    });
    let start = Instant::now();
    let period = Duration::from_secs_f64(1.0 / f64::from(fps));
    let mut time_line = TimeLine::new(fps);
    let mut busy = 0;
    for n in 0..frames {
        let t = period * n as u32;
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
        let seconds = t.as_secs_f32();
        let mut buffer = encoder.pixel_buffer().expect("a buffer");
        draw(&mut renderer, &buffer, land, seconds, picture, foveation.as_ref());
        if let Variant::EdgeBlur(eye) = variant {
            let size = [frame_size[0] as usize, frame_size[1] as usize];
            buffer.with_planes_mut(|y, ys, uv, uvs| {
                blur_edges(y, ys, size, 1, eye, &medium);
                blur_edges(uv, uvs, [size[0] / 2, size[1] / 2], 2, eye, &medium);
            });
        }
        let stamp = n as u64 + 1;
        if n >= SKIP && n % EVERY == 0 {
            let mut reference = reference_pool.pixel_buffer().expect("a reference buffer");
            draw(&mut renderer, &reference, land, seconds, EYE, None);
            source_tx.send((stamp, luma_of(&mut reference))).unwrap();
        }
        encoder.encode(&buffer, time_line.at(now), stamp, n == 0).expect("encode");
    }
    let seconds = if paced { start.elapsed().as_secs_f64() } else { frames as f64 / f64::from(fps) };
    drop(encoder);
    drop(source_tx);
    let (errors, decode_ms, encode_ms, bytes, count) = checker.join().expect("the checker");
    Run { variant, mbps, frames: count, bytes, seconds, encode_ms, decode_ms, busy, errors }
}

fn report(r: &Run) -> String {
    let picture = r.variant.picture();
    let e = &r.errors;
    format!(
        "{:<14} {:>3} Mbps: frame {}x{} ({:.2} MP), sent {:.1} Mbps; PSNR center {:.2} dB, middle {:.2} dB, edge {:.2} dB, all {:.2} dB; SSIM center {:.4}; encode {:.2} ms median, {:.2} ms p95; decode {:.2} ms median; {} frames, {} turns found the encoder full",
        r.variant.label(),
        r.mbps,
        picture[0] * 2,
        picture[1],
        f64::from(picture[0] * 2 * picture[1]) / 1e6,
        r.bytes as f64 * 8.0 / r.seconds / 1e6,
        e.psnr(Region::Center as usize),
        e.psnr(Region::Middle as usize),
        e.psnr(Region::Edge as usize),
        e.all(),
        e.ssim.iter().sum::<f64>() / e.ssim.len().max(1) as f64,
        percentile(&r.encode_ms, 0.5),
        percentile(&r.encode_ms, 0.95),
        percentile(&r.decode_ms, 0.5),
        r.frames,
        r.busy,
    )
}

/// The PSNR of each strip across the center's rows, from the outer edge of the eye: a seam shows as
/// a dip that its neighbors do not have.
fn strips(r: &Run) -> String {
    r.errors
        .strips
        .iter()
        .map(|(sse, n)| {
            let mse = sse / (*n).max(1) as f64;
            format!("{:.1}", 10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
#[ignore = "runs for minutes; prints the numbers of each foveation level"]
fn foveation_levels() {
    let rates: Vec<u64> = std::env::var("CRANVR_FOV_MBPS").unwrap_or_else(|_| "40,12".into()).split(',').filter_map(|v| v.trim().parse().ok()).collect();
    let frames = std::env::var("CRANVR_FOV_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(240);
    let paced = std::env::var("CRANVR_FOV_PACED").is_ok_and(|v| v == "1");
    let dump = std::env::var_os("CRANVR_FOV_DUMP").map(PathBuf::from);
    if let Some(dir) = &dump {
        std::fs::create_dir_all(dir).expect("the dump folder");
    }
    // `CRANVR_FOV_SCENE=pattern` runs the host's test scene instead of the game's landscape
    let land = (!std::env::var("CRANVR_FOV_SCENE").is_ok_and(|v| v == "pattern")).then(|| landscape(6144, 3072));
    let variants = FoveationLevel::ALL
        .map(Variant::Level)
        .into_iter()
        .chain([Variant::AlvrShift(FoveationLevel::Medium), Variant::EdgeBlur(OFF_EYE), Variant::EdgeBlur(EYE)]);
    let variants: Vec<Variant> = match std::env::var("CRANVR_FOV_ONLY") {
        Ok(only) => variants.filter(|v| only.split(',').any(|o| v.label() == o)).collect(),
        Err(_) => variants.collect(),
    };
    for &mbps in &rates {
        for &variant in &variants {
            let r = run(variant, mbps, frames, paced, land.as_ref(), dump.as_ref());
            println!("{}", report(&r));
            println!("{:<14} {:>3} Mbps: strips {}", "", mbps, strips(&r));
        }
    }
}

/// The unwarp of the test reads a foveated frame back to the view it came from: a picture of the
/// view, warped by `encoded_to_view` and read back, matches away from the squeezed edges.
#[test]
fn the_unwarp_reads_the_view_back() {
    let f = Foveation::new(EYE, &FoveationLevel::Medium.config().unwrap().centered(FOV[0]));
    let [ow, oh] = f.optimized.map(|v| v as usize);
    let size = [ow * 2, oh];
    let gradient = |view: [f32; 2]| view[0] * 200.0 + view[1] * 50.0;
    let mut frame = vec![0u8; size[0] * size[1]];
    for y in 0..oh {
        for x in 0..size[0] {
            let (eye, ex) = if x < ow { (0, x) } else { (1, size[0] - 1 - x) };
            let mut view = f.encoded_to_view([(ex as f32 + 0.5) / ow as f32, (y as f32 + 0.5) / oh as f32]);
            if eye == 1 {
                view[0] = 1.0 - view[0];
            }
            frame[y * size[0] + x] = gradient(view).round() as u8;
        }
    }
    for eye in 0..2 {
        for view in [[0.6, 0.5], [0.5, 0.45], [0.3, 0.3], [0.85, 0.7]] {
            let got = unwarp(&frame, size, Some(&f), eye, view);
            assert!((got - gradient(view)).abs() < 2.0, "eye {eye} at {view:?}: {got} against {}", gradient(view));
        }
    }
}
