//! The quality of a display's pictures after the host's encoder and a decoder.
//!
//! The test makes up a desktop: windows with lines of small text, as a display of 3840 points looks
//! when the host captures it at 1920 pixels. Parts of it change as a script says: a spinner that turns
//! and a line that grows in each picture, a cursor that moves over text, a window that scrolls, a
//! desktop that stays still and changes that come at uneven times. The pictures go through the host's
//! own `Feed` and `DisplayEncoder`, as ScreenCaptureKit's pictures go in
//! `desktop_loop`: a new picture when the desktop changed, nothing while it is still. A VideoToolbox
//! decoder then decodes each frame. Every 100 ms the test compares the picture that the headset would
//! show (the last decoded one) with the desktop at that time (PSNR of the Y plane).
//!
//! The clock is made up as well: the loop turns every 5 ms of the test's own time and waits for each
//! frame of the encoder, so the test does not depend on the Mac's load. VideoToolbox's rate control
//! reads only the time line of the frames.
//!
//! `display_quality_curve` writes a curve of 60 seconds to a file for a person to read. It is ignored:
//! run it with
//! `CRANVR_QUALITY_CSV=/path/curve.csv cargo test --release -p cranvr-host display_quality_curve -- --ignored --nocapture`.
//! `CRANVR_QUALITY_TILES=2 CRANVR_QUALITY_FPS=60` measures a display of 3840 x 2160 at 60 pictures a
//! second instead of the hand's display. `CRANVR_QUALITY_SPACING=1ms` measures the time line that the
//! host had before. `CRANVR_QUALITY_MBPS` sets the bitrate, and `CRANVR_QUALITY_NO_CAP=1` keeps the
//! host's time line without the cap on the quantizer.

use super::*;
use cranvr_vt::{DecodedFrame, Decoder};
use std::io::Write;

/// The size of the made-up desktop, the size of the hand's display
const WIDTH: usize = 1920;
const HEIGHT: usize = 1080;
/// The pictures a second of the hand's display
const FPS: i32 = 30;
/// The loop's turn, as `step` waits for a capture
const TICK: Duration = Duration::from_millis(5);
/// The curve takes a point this often
const SAMPLE_EVERY: Duration = Duration::from_millis(100);

/// A small random number source, so that every run makes the same desktop
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

/// A rectangle in pixels: left, top, width, height
type Area = (usize, usize, usize, usize);

/// The window whose text never changes. Its PSNR shows the quality of still text alone.
const STILL_TEXT: Area = (60, 80, 820, 560);
/// The window with the spinner and the line that grows, as in a terminal at work
const BUSY: Area = (960, 80, 900, 420);
const SPINNER: (usize, usize) = (1780, 140);
/// The window that scrolls, and over which the cursor moves
const PAGE: Area = (960, 560, 900, 460);
/// The rows of text of the page that scrolls
const PAGE_ROWS: usize = 4000;

/// What changes on the desktop during a phase of the script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Motion {
    /// The spinner turns and the line grows: the desktop changes 30 times a second
    Busy,
    /// The same, with a cursor that moves over the page
    Cursor,
    /// The page scrolls by a line 30 times a second
    Scroll,
    /// Nothing changes: ScreenCaptureKit hands over no picture
    Still,
    /// The line grows by a glyph at uneven times, 30 ms to 3 s apart
    Bursts,
    /// The page shows a video of fine detail that changes all over 30 times a second
    Video,
    /// The same video fills the whole picture, the hardest picture for the encoder
    FullVideo,
}

/// The state of the desktop's moving parts
#[derive(Clone, Copy, Default)]
struct State {
    spinner: usize,
    typed: usize,
    cursor: Option<f32>,
    scroll: usize,
    /// The picture of the video, from 1; 0 shows no video
    video: u64,
    /// The video fills the whole picture
    full: bool,
}

/// The made-up desktop: its still part, the page that scrolls and the glyphs of its font
struct Desktop {
    y: Vec<u8>,
    uv: Vec<u8>,
    page: Vec<u8>,
    glyphs: Vec<[u8; 24]>,
}

impl Desktop {
    fn new() -> Self {
        let mut rng = Rng(7);
        // Glyphs of 4 x 6 pixels: dark strokes with softer edges, as small text has
        let glyphs: Vec<[u8; 24]> = (0..80)
            .map(|_| {
                let mut g = [0u8; 24];
                for cell in g.iter_mut() {
                    *cell = match rng.below(10) {
                        0..=3 => 255,
                        4..=5 => 90 + rng.below(100) as u8,
                        _ => 0,
                    };
                }
                g
            })
            .collect();
        let mut desktop = Self { y: vec![0; WIDTH * HEIGHT], uv: vec![128; WIDTH * HEIGHT / 2], page: Vec::new(), glyphs };
        // A wallpaper that runs from dark blue at the top to lighter at the bottom
        for row in 0..HEIGHT {
            desktop.y[row * WIDTH..(row + 1) * WIDTH].fill(70 + (row * 50 / HEIGHT) as u8);
        }
        for pair in desktop.uv.chunks_mut(2) {
            pair.copy_from_slice(&[150, 115]);
        }
        let (mut y, mut uv) = (std::mem::take(&mut desktop.y), std::mem::take(&mut desktop.uv));
        desktop.window(&mut y, &mut uv, WIDTH, STILL_TEXT, false, &mut rng);
        desktop.window(&mut y, &mut uv, WIDTH, BUSY, true, &mut rng);
        // The page is one tall window of its own width
        let width = PAGE.2;
        let mut page = vec![0; width * PAGE_ROWS];
        let mut page_uv = vec![128; width * PAGE_ROWS / 2];
        desktop.window(&mut page, &mut page_uv, width, (0, 0, width, PAGE_ROWS), false, &mut rng);
        (desktop.y, desktop.uv, desktop.page) = (y, uv, page);
        desktop
    }

    /// A window with a title bar and lines of text in a plane `stride` pixels wide. A dark window has
    /// light text, as a terminal.
    fn window(&self, y: &mut [u8], uv: &mut [u8], stride: usize, (left, top, width, height): Area, dark: bool, rng: &mut Rng) {
        let (paper, ink) = if dark { (28u8, 210u8) } else { (246u8, 30u8) };
        for row in top..top + height {
            y[row * stride + left..row * stride + left + width].fill(if row < top + 22 { 200 } else { paper });
        }
        for row in top / 2..(top + height) / 2 {
            uv[row * stride + left..row * stride + left + width].fill(128);
        }
        // Lines 9 pixels apart, words of 2 to 9 glyphs with gaps of 4 pixels
        let mut line = top + 30;
        while line + 6 < top + height - 8 {
            let mut x = left + 10;
            let end = left + width - 10 - rng.below(width as u32 / 3) as usize;
            while x + 40 < end {
                for _ in 0..2 + rng.below(8) {
                    let glyph = self.glyphs[rng.below(self.glyphs.len() as u32) as usize];
                    draw_glyph(y, stride, x, line, &glyph, paper, ink);
                    x += 5;
                }
                x += 4;
            }
            line += 9;
        }
    }

    /// Writes the desktop in `state` into `y` and `uv` (rows of `WIDTH` bytes).
    fn picture(&self, state: State, y: &mut [u8], uv: &mut [u8]) {
        y.copy_from_slice(&self.y);
        uv.copy_from_slice(&self.uv);
        let (left, top, width, height) = PAGE;
        for row in 0..height {
            let from = ((state.scroll + row) % PAGE_ROWS) * width;
            y[(top + row) * WIDTH + left..(top + row) * WIDTH + left + width].copy_from_slice(&self.page[from..from + width]);
        }
        if state.video > 0 {
            // Blocks of 4 x 4 pixels of random grey that drift by 2 pixels a picture. The blocks are
            // new every 8 pictures, as the cuts of a film.
            let mut rng = Rng(state.video / 8 + 1);
            let (left, top, width, height) = if state.full { WHOLE } else { PAGE };
            let blocks: Vec<u8> = (0..(width / 4 + 1) * (height / 4 + 1)).map(|_| rng.below(256) as u8).collect();
            for row in 0..height {
                for col in 0..width {
                    let (bx, by) = ((col + 2 * state.video as usize) % width / 4, row / 4);
                    y[(top + row) * WIDTH + left + col] = blocks[by * (width / 4 + 1) + bx] / 2 + ((row + col) % 64) as u8 * 2;
                }
            }
        }
        // The line that grows in the busy window, a glyph at a time, and starts again after 120
        let (left, top, _, _) = BUSY;
        for i in 0..state.typed % 120 {
            let glyph = self.glyphs[(i * 7 + state.typed / 120) % self.glyphs.len()];
            draw_glyph(y, WIDTH, left + 10 + i * 5, top + 380, &glyph, 28, 210);
        }
        // The spinner: a bar that turns by 12 degrees a step
        let angle = state.spinner as f32 * 12f32.to_radians();
        let (cx, cy) = SPINNER;
        for dy in -20i32..=20 {
            for dx in -20i32..=20 {
                let (px, py) = (dx as f32, dy as f32);
                let along = px * angle.cos() + py * angle.sin();
                let across = -px * angle.sin() + py * angle.cos();
                if along.abs() <= 18.0 && across.abs() <= 3.0 {
                    let at = (cy as i32 + dy) as usize * WIDTH + (cx as i32 + dx) as usize;
                    y[at] = blend(28, 230, (255.0 * (3.0 - across.abs()).clamp(0.0, 1.0)) as u8);
                }
            }
        }
        // A cursor that circles over the page once in 4 seconds
        if let Some(seconds) = state.cursor {
            let t = seconds * std::f32::consts::TAU / 4.0;
            let (left, top, width, height) = PAGE;
            let x = left + width / 2 + (t.cos() * (width as f32 * 0.4)) as usize;
            let y0 = top + height / 2 + (t.sin() * (height as f32 * 0.4)) as usize;
            for row in 0..19usize {
                for col in 0..=row.min(12) {
                    let edge = col == 0 || col == row.min(12) || row == 18;
                    y[(y0 + row) * WIDTH + x + col] = if edge { 0 } else { 255 };
                }
            }
        }
    }
}

fn draw_glyph(y: &mut [u8], stride: usize, x: usize, top: usize, shape: &[u8; 24], paper: u8, ink: u8) {
    for (i, &cover) in shape.iter().enumerate() {
        y[(top + i / 4) * stride + x + i % 4] = blend(paper, ink, cover);
    }
}

fn blend(paper: u8, ink: u8, cover: u8) -> u8 {
    ((u32::from(paper) * (255 - u32::from(cover)) + u32::from(ink) * u32::from(cover)) / 255) as u8
}

const WHOLE: Area = (0, 0, WIDTH, HEIGHT);

/// The PSNR in dB of the Y plane in `area`.
fn psnr(a: &[u8], b: &[u8], (left, top, width, height): Area) -> f64 {
    let mut sum = 0u64;
    for row in top..top + height {
        let (a, b) = (&a[row * WIDTH + left..row * WIDTH + left + width], &b[row * WIDTH + left..row * WIDTH + left + width]);
        let mut line = 0u32;
        for (&p, &q) in a.iter().zip(b) {
            let d = u32::from(p.abs_diff(q));
            line += d * d;
        }
        sum += u64::from(line);
    }
    let mse = sum as f64 / (width * height) as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

/// How the frames stand on the encoder's time line.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Spacing {
    /// As the host does: the feed's real time
    Feed,
    /// As the host did before: 1 ms apart, a key frame every 300 frames and no refresh
    OneMs,
}

/// One point of the curve: what the headset shows against the desktop at that time.
#[derive(Clone, Copy, Debug)]
struct Point {
    /// Seconds since the start
    at: f64,
    motion: Motion,
    /// The PSNR of the whole picture and of the window whose text never changes
    psnr: f64,
    text_psnr: f64,
}

/// What a run gave: the curve, and the time, size and kind (key or not) of each frame.
struct Run {
    points: Vec<Point>,
    frames: Vec<(f64, usize, bool)>,
    /// Pictures that the encoder dropped
    dropped: usize,
    /// The encode times of the frames, in ms
    encode_ms: Vec<f64>,
}

/// The display's pictures: `tiles` copies of the made-up desktop across and down, at up to `fps` a
/// second. A display of 3840 x 2160 pixels holds 2 x 2 copies; the curve follows the top left one.
#[derive(Clone, Copy)]
struct Format {
    tiles: usize,
    fps: i32,
}

const HAND: Format = Format { tiles: 1, fps: FPS };

/// Plays `script` through the host's feed and encoder at `mbps`. At `key_at` (from the start) the
/// client reports a lost picture.
fn measure(script: &[(Motion, Duration)], format: Format, mbps: u64, spacing: Spacing, key_at: Option<Duration>) -> Run {
    let desktop = Desktop::new();
    let (sender, encoded) = mpsc::channel();
    let Format { tiles, fps } = format;
    let size = [(WIDTH * tiles) as u32, (HEIGHT * tiles) as u32];
    let mut display = DisplayEncoder::new(size, fps, mbps, sender.clone(), Conversion { source: size, flip: false }).expect("encoder");
    // The old encoder: frames 1 ms apart on the time line, a key frame every 10 s and no cap
    let key_every = Some(Duration::from_secs(10));
    let old = Encoder::new(&EncoderConfig { width: size[0], height: size[1], fps: fps as f32, bitrate_bps: mbps * 1_000_000, full_range: true, strict_rate: false, key_every, max_qp: None, real_time: false }, sender)
        .expect("encoder");
    // The curve may try the host's time line without the cap: `CRANVR_QUALITY_NO_CAP=1`
    let no_cap = std::env::var("CRANVR_QUALITY_NO_CAP").is_ok_and(|v| v == "1");
    if no_cap {
        display.encoder = DisplayEncoder::session(size, fps, mbps, &display.sender, false).expect("encoder");
        display.capped = false;
    }
    let (decoded_sender, decoded) = mpsc::channel::<DecodedFrame>();
    let mut decoder: Option<Decoder> = None;
    let start = Instant::now();
    let mut feed = Feed::new(start, mbps * 1_000_000);
    // The old feed: a new capture or a key frame, 1 ms apart on the time line, and no cap
    let (mut old_seq, mut old_key) = (0u64, true);
    let mut run = Run { points: Vec::new(), frames: Vec::new(), dropped: 0, encode_ms: Vec::new() };
    let (mut state, mut rng) = (State::default(), Rng(11));
    let mut capture: Option<(u64, PixelBuffer)> = None;
    let mut source = vec![0u8; WIDTH * HEIGHT];
    let mut chroma = vec![0u8; WIDTH * HEIGHT / 2];
    let mut shown: Option<Vec<u8>> = None;
    let (mut t, mut next_sample, mut index) = (Duration::ZERO, Duration::ZERO, 0u64);
    let mut next_picture;
    let mut key_at = key_at;
    let mut phase_end = Duration::ZERO;
    for &(motion, length) in script {
        phase_end += length;
        // A new phase changes the desktop at once
        next_picture = t;
        while t < phase_end {
            // The desktop changes when the script says so, and ScreenCaptureKit hands over the picture
            if motion != Motion::Still && t >= next_picture {
                match motion {
                    Motion::Busy | Motion::Cursor => {
                        (state.spinner, state.typed) = (state.spinner + 1, state.typed + 1);
                        state.cursor = (motion == Motion::Cursor).then_some(t.as_secs_f32());
                    }
                    Motion::Scroll => (state.scroll, state.cursor) = (state.scroll + 9, None),
                    Motion::Bursts => (state.typed, state.cursor) = (state.typed + 1, None),
                    Motion::Video | Motion::FullVideo => (state.video, state.full, state.cursor) = (state.video + 1, motion == Motion::FullVideo, None),
                    Motion::Still => {}
                }
                desktop.picture(state, &mut source, &mut chroma);
                let mut buffer = old.pixel_buffer().expect("a buffer of the pool");
                buffer.with_planes_mut(|y, ys, uv, uvs| {
                    for (down, across) in (0..tiles).flat_map(|down| (0..tiles).map(move |across| (down, across))) {
                        for row in 0..HEIGHT {
                            let at = (down * HEIGHT + row) * ys + across * WIDTH;
                            y[at..at + WIDTH].copy_from_slice(&source[row * WIDTH..(row + 1) * WIDTH]);
                        }
                        for row in 0..HEIGHT / 2 {
                            let at = (down * HEIGHT / 2 + row) * uvs + across * WIDTH;
                            uv[at..at + WIDTH].copy_from_slice(&chroma[row * WIDTH..(row + 1) * WIDTH]);
                        }
                    }
                });
                let seq = capture.as_ref().map_or(1, |(seq, _)| seq + 1);
                capture = Some((seq, buffer));
                next_picture = t + if motion == Motion::Bursts {
                    Duration::from_millis([30, 30, 30, 400, 1700, 30, 2900, 900][rng.below(8) as usize])
                } else {
                    Duration::from_secs(1) / fps as u32
                };
            }
            if key_at.is_some_and(|at| t >= at) {
                key_at = None;
                feed.request_key();
                old_key = true;
            }
            let now = start + t;
            let picture = match spacing {
                Spacing::Feed => feed.next(capture.clone(), now).map(|picture| Picture { cap: picture.cap && !no_cap, ..picture }),
                Spacing::OneMs => capture.clone().filter(|(seq, _)| *seq != old_seq || old_key).map(|(seq, image)| Picture {
                    image,
                    seq,
                    presentation: Duration::from_millis(index),
                    key: old_key,
                    cap: false,
                }),
            };
            if let Some(picture) = picture {
                index += 1;
                match spacing {
                    Spacing::Feed => {
                        display.encode(&picture, index).expect("encode");
                        feed.took(&picture, now);
                    }
                    Spacing::OneMs => {
                        old.encode(&picture.image, picture.presentation, index, picture.key).expect("encode");
                        (old_seq, old_key) = (picture.seq, false);
                    }
                }
                // The encoder drops a frame that its rate control cannot afford: no frame comes out
                let waited = Instant::now();
                let frame = loop {
                    match encoded.recv_timeout(Duration::from_millis(5)) {
                        Ok(frame) => break Some(frame),
                        // The callback leaves the frames in flight before it sends the frame
                        Err(_) if display.in_flight() + old.in_flight() == 0 => break encoded.recv_timeout(Duration::from_millis(200)).ok(),
                        Err(_) => assert!(waited.elapsed() < Duration::from_secs(2), "no frame {index} at {t:?}"),
                    }
                };
                match frame {
                    Some(frame) => {
                        feed.sent(frame.data.len(), frame.is_idr, now);
                        if decoder.is_none() {
                            let config = frame.config_nals.as_ref().expect("the first frame is a key frame");
                            decoder = Some(Decoder::with_luma(config, decoded_sender.clone()).expect("decoder"));
                        }
                        decoder.as_mut().unwrap().decode(frame.timestamp_ns, &frame.data).expect("decode");
                        let picture = decoded.recv_timeout(Duration::from_secs(2)).expect("a decoded picture");
                        // The top left copy of the desktop
                        shown = Some(if tiles == 1 {
                            picture.luma
                        } else {
                            (0..HEIGHT).flat_map(|row| picture.luma[row * picture.width..row * picture.width + WIDTH].to_vec()).collect()
                        });
                        run.frames.push((t.as_secs_f64(), frame.data.len(), frame.is_idr));
                        run.encode_ms.push(frame.encode_time.as_secs_f64() * 1000.0);
                    }
                    None => run.dropped += 1,
                }
            }
            if t >= next_sample {
                if let Some(shown) = &shown {
                    run.points.push(Point { at: t.as_secs_f64(), motion, psnr: psnr(&source, shown, WHOLE), text_psnr: psnr(&source, shown, STILL_TEXT) });
                }
                next_sample += SAMPLE_EVERY;
            }
            t += TICK;
        }
    }
    run
}

/// The points from `from` to `to` seconds.
fn between(points: &[Point], from: f64, to: f64) -> impl Iterator<Item = &Point> {
    points.iter().filter(move |p| p.at >= from && p.at < to)
}

fn lowest(values: impl Iterator<Item = f64>) -> f64 {
    values.fold(f64::INFINITY, f64::min)
}

/// The script of the regression check, 15 seconds in all: a busy desktop with a lost picture at 3 s,
/// a cursor, a page that scrolls, a still desktop, changes at uneven times and a video that fills the
/// picture.
const SCRIPT: [(Motion, Duration); 7] = [
    (Motion::Busy, Duration::from_secs(4)),
    (Motion::Cursor, Duration::from_secs(2)),
    (Motion::Scroll, Duration::from_secs(2)),
    (Motion::Still, Duration::from_secs(1)),
    (Motion::Bursts, Duration::from_secs(3)),
    (Motion::FullVideo, Duration::from_secs(1)),
    (Motion::Still, Duration::from_secs(2)),
];

/// The bitrate of the hand's display: 1920 x 1080 at 30 pictures a second
fn hand_mbps() -> u64 {
    scaled_mbps(MBPS, [WIDTH as u32, HEIGHT as u32], FPS)
}

fn mean(points: impl Iterator<Item = f64>) -> f64 {
    let (sum, n) = points.fold((0.0, 0), |(sum, n), p| (sum + p, n + 1));
    sum / f64::from(n.max(1))
}

/// The text of a display stays sharp while it is still, after a lost picture and after motion: the
/// curve stays flat and high. The host's old time line, with frames 1 ms apart, fails the same check.
#[test]
fn a_display_keeps_its_text_sharp_from_picture_to_picture() {
    let run = measure(&SCRIPT, HAND, hand_mbps(), Spacing::Feed, Some(Duration::from_secs(3)));
    let p = &run.points;
    // The first key frame is the least sharp: the pictures after it sharpen the text within a second
    let steady = lowest(between(p, 1.0, 3.0).map(|p| p.text_psnr));
    assert!(steady >= 38.0, "the still text reaches {steady:.1} dB");
    // The still text keeps its quality to the end: no picture makes it worse. The key frame after the
    // lost picture may blur it for a moment.
    let still_text = lowest(between(p, 1.0, 12.0).filter(|p| !(3.0..3.5).contains(&p.at)).map(|p| p.text_psnr));
    assert!(still_text >= steady - 1.0, "the still text falls to {still_text:.1} dB from {steady:.1} dB");
    // The whole picture is as sharp as before half a second after the scroll stops, and a second
    // after the video stops. The video fills the picture, so the text under it returns too.
    let whole = mean(between(p, 1.0, 3.0).map(|p| p.psnr));
    for (what, from) in [("scroll", 8.5), ("video", 14.0)] {
        let after = lowest(between(p, from, from + 1.0).map(|p| p.psnr));
        assert!(after >= whole - 2.5, "after the {what} the picture has {after:.1} dB against {whole:.1} dB");
    }
    // The video stays near the bitrate: a session with the cap sent ten times as much
    let budget = hand_mbps() as f64 * 1e6 / 8.0;
    let video: usize = run.frames.iter().filter(|f| (12.0..13.0).contains(&f.0)).map(|f| f.1).sum();
    assert!((video as f64) < 4.0 * budget, "the second of video sent {:.1} MB against {:.1} MB a second", video as f64 / 1e6, budget / 1e6);

    // The old time line: the key frame after the lost picture comes out blurred and stays so
    let old = measure(&SCRIPT, HAND, hand_mbps(), Spacing::OneMs, Some(Duration::from_secs(3)));
    let old_text = lowest(between(&old.points, 3.5, 12.0).map(|p| p.text_psnr));
    let after = |from: f64| lowest(between(p, from, from + 1.0).map(|p| p.psnr));
    println!(
        "still text {steady:.1} dB, lowest {still_text:.1} dB; whole {whole:.1} dB, after the scroll {:.1} dB, after the video {:.1} dB; video {:.2} MB in its second; old time line {old_text:.1} dB",
        after(8.5),
        after(14.0),
        video as f64 / 1e6
    );
    assert!(old_text < steady - 10.0, "the old time line keeps {old_text:.1} dB");
}

#[test]
#[ignore = "runs for a minute; writes the curve to CRANVR_QUALITY_CSV"]
fn display_quality_curve() {
    let spacing = if std::env::var("CRANVR_QUALITY_SPACING").is_ok_and(|v| v == "1ms") { Spacing::OneMs } else { Spacing::Feed };
    let number = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    let format = Format { tiles: number("CRANVR_QUALITY_TILES").map_or(1, |n| n as usize), fps: number("CRANVR_QUALITY_FPS").map_or(FPS, |n| n as i32) };
    let mbps = number("CRANVR_QUALITY_MBPS").unwrap_or_else(|| scaled_mbps(MBPS, [(WIDTH * format.tiles) as u32, (HEIGHT * format.tiles) as u32], format.fps));
    let script = [
        (Motion::Busy, Duration::from_secs(20)),
        (Motion::Cursor, Duration::from_secs(20)),
        (Motion::Scroll, Duration::from_secs(3)),
        (Motion::Still, Duration::from_secs(5)),
        (Motion::Bursts, Duration::from_secs(12)),
        (Motion::Video, Duration::from_secs(4)),
        (Motion::Still, Duration::from_secs(2)),
        (Motion::FullVideo, Duration::from_secs(4)),
        (Motion::Still, Duration::from_secs(2)),
    ];
    println!("{} x {} pixels at {} a second, {mbps} Mbps", WIDTH * format.tiles, HEIGHT * format.tiles, format.fps);
    let run = measure(&script, format, mbps, spacing, Some(Duration::from_secs(15)));
    if let Ok(path) = std::env::var("CRANVR_QUALITY_CSV") {
        let mut file = std::fs::File::create(path).expect("the curve's file");
        writeln!(file, "seconds,motion,psnr,text_psnr").unwrap();
        for p in &run.points {
            writeln!(file, "{:.2},{:?},{:.2},{:.2}", p.at, p.motion, p.psnr, p.text_psnr).unwrap();
        }
    }
    let keys: Vec<String> = run.frames.iter().filter(|f| f.2).map(|f| format!("{:.1} s ({} KB)", f.0, f.1 / 1000)).collect();
    let mut encode = run.encode_ms.clone();
    encode.sort_by(f64::total_cmp);
    println!(
        "{} frames, {} dropped, encode {:.1} ms (median), {:.1} ms (p95), key frames at {}",
        run.frames.len(),
        run.dropped,
        encode.get(encode.len() / 2).copied().unwrap_or(0.0),
        encode.get(encode.len() * 95 / 100).copied().unwrap_or(0.0),
        keys.join(", ")
    );
    for second in 0..72 {
        let inside: Vec<&Point> = between(&run.points, f64::from(second), f64::from(second + 1)).collect();
        let bytes: usize = run.frames.iter().filter(|f| f.0 as u32 == second).map(|f| f.1).sum();
        if let Some(first) = inside.first() {
            println!(
                "second {second:2} {:?}: PSNR {:5.1} dB (lowest {:5.1}), still text lowest {:5.1} dB, {:5.2} Mbps",
                first.motion,
                inside.iter().map(|p| p.psnr).sum::<f64>() / inside.len() as f64,
                lowest(inside.iter().map(|p| p.psnr)),
                lowest(inside.iter().map(|p| p.text_psnr)),
                bytes as f64 * 8.0 / 1e6
            );
        }
    }
}

/// The feed's decisions, without an encoder: what goes in, when and to which session.
mod feed {
    use super::*;

    fn picture() -> PixelBuffer {
        PixelBuffer::new_bgra(16, 16).expect("a buffer")
    }

    /// Asks the feed at `at` after `start` and hands the picture to a pretend encoder.
    fn step(feed: &mut Feed, start: Instant, at: Duration, latest: &Option<(u64, PixelBuffer)>) -> Option<(u64, bool, bool)> {
        let now = start + at;
        let picture = feed.next(latest.clone(), now)?;
        feed.took(&picture, now);
        Some((picture.seq, picture.key, picture.cap))
    }

    #[test]
    fn a_new_capture_goes_in_at_once_and_a_still_one_goes_in_again_once() {
        let start = Instant::now();
        let mut feed = Feed::new(start, 6_000_000);
        let ms = Duration::from_millis;
        assert_eq!(step(&mut feed, start, ms(0), &None), None, "no capture yet");
        let latest = Some((1, picture()));
        assert_eq!(step(&mut feed, start, ms(0), &latest), Some((1, true, true)), "the first picture is a capped key frame");
        assert_eq!(step(&mut feed, start, ms(100), &latest), None);
        // The refresh, once
        assert_eq!(step(&mut feed, start, ms(260), &latest), Some((1, false, true)));
        assert_eq!(step(&mut feed, start, ms(600), &latest), None);
        assert_eq!(step(&mut feed, start, ms(5000), &latest), None);
        // A lost picture: the last capture goes in again as a key frame
        feed.request_key();
        assert_eq!(step(&mut feed, start, ms(5005), &latest), Some((1, true, true)));
        assert_eq!(step(&mut feed, start, ms(5010), &latest), None);
        let latest = Some((2, picture()));
        assert_eq!(step(&mut feed, start, ms(5020), &latest), Some((2, false, true)));
    }

    #[test]
    fn the_cap_comes_off_past_the_bitrate_and_back_when_the_stream_calms() {
        let start = Instant::now();
        let ms = Duration::from_millis;
        // 6 Mbps: 750 KB a second
        let mut feed = Feed::new(start, 6_000_000);
        let mut seq = 1;
        assert_eq!(step(&mut feed, start, ms(0), &Some((seq, picture()))), Some((1, true, true)));
        // A key frame larger than a second's bitrate does not count
        feed.sent(900_000, true, start);
        // Pictures of 50 KB, 30 a second, twice the bitrate: the cap holds
        for n in 1..=40u64 {
            seq += 1;
            let at = ms(n * 33);
            assert!(step(&mut feed, start, at, &Some((seq, picture()))).expect("a new capture").2);
            feed.sent(50_000, false, start + at);
        }
        // Hard pictures of 500 KB: the first one brings the last second to 2 MB, past 2.5 times the
        // bitrate, so the second one goes to the session without the cap
        let mut cap = Vec::new();
        for n in 41..=50u64 {
            seq += 1;
            let at = ms(n * 33);
            let (_, _, capped) = step(&mut feed, start, at, &Some((seq, picture()))).expect("a new capture");
            feed.sent(500_000, false, start + at);
            cap.push(capped);
        }
        assert_eq!(cap, [true, false, false, false, false, false, false, false, false, false]);
        // The video goes on at 2 Mbps without the cap: the cap stays off
        for n in 51..=100u64 {
            seq += 1;
            let at = ms(n * 33);
            assert!(!step(&mut feed, start, at, &Some((seq, picture()))).expect("a new capture").2);
            feed.sent(8_000, false, start + at);
        }
        // The video stops: once the last second holds less than an eighth of the bitrate, the last
        // capture goes in again to a capped session
        let latest = Some((seq, picture()));
        let back = (101..300u64).map(|n| ms(n * 33)).find_map(|at| step(&mut feed, start, at, &latest).filter(|p| p.2).map(|_| at));
        let back = back.expect("the cap comes back");
        assert!(back > ms(3300) && back < ms(4300), "the cap came back at {back:?}");
    }
}

#[path = "encoder_sharing.rs"]
mod encoder_sharing;
