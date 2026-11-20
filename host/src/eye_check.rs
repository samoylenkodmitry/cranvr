//! Finds a game whose right eye stays black. A game that renders both eyes in one pass into a
//! swapchain with two slices draws slice 1 through a path that DXMT does not support, so the right
//! eye stays empty. The host cannot tell this from the swapchain's description, because games that
//! render both eyes into two slices work too. It looks at the pictures instead.
//!
//! About once a second the frame loop reads a grid of 64 x 64 pixels from each eye of the game's
//! newest frame. Three states decide: the right eye is black when none of its samples is lit, and
//! the left eye has content when enough of its samples are lit. The symptom counts when the left
//! eye has content and the right eye is black for several checks in a row. A dark screen in both
//! eyes (a loading screen) and a flicker between states never count. The host reports the symptom
//! once for each game run (see `status.rs`, the `black_eye` line).
//!
//! The grid has 4096 samples because a menu in a dark room lights few pixels. A game that showed the
//! spectrum bars of its menu in the left eye and nothing in the right eye lit about 0.5 percent of the
//! left eye's pixels, which a grid of 16 x 16 samples rarely hit.
//!
//! The cost is two read-only locks and 8192 pixel reads once a second, with no allocation. A frame
//! between the checks costs one clock read.

use log::{info, warn};
use cranvr_vt::PixelBuffer;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Samples along each side of the grid
pub const SIDE: usize = 64;
pub const SAMPLES: usize = SIDE * SIDE;
/// A sample is lit when its brightest color channel is above this value of 255. An empty slice
/// reads 0. A dark scene stays below it only where it is nearly black.
const LIT_LEVEL: u8 = 8;
/// An eye has content when at least this many samples are lit (0.2 percent). A few bright pixels,
/// such as a cursor on a black screen, do not count.
const CONTENT_SAMPLES: usize = 8;
/// The checks in a row that must show the symptom. At one check a second the game has shown a
/// black right eye beside a lit left eye for this many seconds.
const CHECKS_IN_A_ROW: u32 = 4;
const INTERVAL: Duration = Duration::from_secs(1);

/// The folder named by `CRANVR_EYE_DUMP`, if any. A test run sets it to see what a game drew: each
/// check then writes both eyes of the game's newest frame as `eye-left.png` and `eye-right.png`.
fn dump_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| std::env::var_os("CRANVR_EYE_DUMP").filter(|dir| !dir.is_empty()).map(PathBuf::from)).as_deref()
}

/// The widest dump in pixels: `CRANVR_EYE_DUMP_WIDTH`, else 640. A test that reads a game's menu asks for the whole
/// width of an eye (1500 or more), which writes a file of several megabytes each second.
fn dump_width() -> usize {
    static WIDTH: OnceLock<usize> = OnceLock::new();
    *WIDTH.get_or_init(|| std::env::var("CRANVR_EYE_DUMP_WIDTH").ok().and_then(|width| width.parse().ok()).filter(|width| *width > 0).unwrap_or(640))
}

/// The CRC-32 that PNG chunks carry.
fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = !0u32;
    for byte in parts.iter().flat_map(|part| part.iter()) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
}

/// An 8-bit RGB PNG from `rgb` (`width` x `height` pixels, 3 bytes each). The data is not compressed
/// (stored deflate blocks), which keeps this free of a library and is small enough at the sizes that the
/// dump uses.
fn png_rgb(width: usize, height: usize, rgb: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((width * 3 + 1) * height);
    for row in rgb.chunks(width * 3) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut zlib = vec![0x78, 0x01];
    let blocks = raw.chunks(65535).collect::<Vec<_>>();
    for (i, block) in blocks.iter().enumerate() {
        zlib.push(u8::from(i + 1 == blocks.len()));
        zlib.extend_from_slice(&(block.len() as u16).to_le_bytes());
        zlib.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut header = Vec::new();
    header.extend_from_slice(&(width as u32).to_be_bytes());
    header.extend_from_slice(&(height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png_chunk(&mut png, b"IHDR", &header);
    png_chunk(&mut png, b"IDAT", &zlib);
    png_chunk(&mut png, b"IEND", &[]);
    png
}

/// The brightest color channel of each sampled pixel of one eye.
pub struct Grid([u8; SAMPLES]);

impl Default for Grid {
    fn default() -> Self {
        Self([0; SAMPLES])
    }
}

impl Grid {
    fn lit(&self) -> usize {
        self.0.iter().filter(|&&level| level > LIT_LEVEL).count()
    }

    /// No sample is lit: the eye is black, or its slice was never drawn.
    fn is_black(&self) -> bool {
        self.lit() == 0
    }

    fn has_content(&self) -> bool {
        self.lit() >= CONTENT_SAMPLES
    }

    /// Reads the grid from 4-byte pixels (`data`, `stride` bytes per row, `size` in pixels) inside
    /// `rect` (x, y, width, height). A rect that is empty or outside the image stands for the
    /// whole image. Returns false when the data is too short for the image.
    fn fill(&mut self, data: &[u8], stride: usize, size: (usize, usize), rect: [i32; 4]) -> bool {
        let (width, height) = size;
        if width == 0 || height == 0 || stride < width * 4 || data.len() < stride * (height - 1) + width * 4 {
            return false;
        }
        let clamp = |value: i32, limit: usize| (value.max(0) as usize).min(limit);
        let (x0, y0) = (clamp(rect[0], width), clamp(rect[1], height));
        let (w, h) = (clamp(rect[2], width - x0), clamp(rect[3], height - y0));
        let (x0, y0, w, h) = if w == 0 || h == 0 { (0, 0, width, height) } else { (x0, y0, w, h) };
        for row in 0..SIDE {
            let y = y0 + (row * 2 + 1) * h / (2 * SIDE);
            for column in 0..SIDE {
                let x = x0 + (column * 2 + 1) * w / (2 * SIDE);
                let pixel = &data[y * stride + x * 4..y * stride + x * 4 + 3];
                self.0[row * SIDE + column] = pixel[0].max(pixel[1]).max(pixel[2]);
            }
        }
        true
    }

    /// The part of a BGRA image inside `rect` as a PNG, shrunk by a whole factor to at most `dump_width()` pixels
    /// across. The rect follows the rules of `fill`.
    fn png(data: &[u8], stride: usize, size: (usize, usize), rect: [i32; 4]) -> Option<Vec<u8>> {
        let (width, height) = size;
        if width == 0 || height == 0 || stride < width * 4 || data.len() < stride * (height - 1) + width * 4 {
            return None;
        }
        let clamp = |value: i32, limit: usize| (value.max(0) as usize).min(limit);
        let (x0, y0) = (clamp(rect[0], width), clamp(rect[1], height));
        let (w, h) = (clamp(rect[2], width - x0), clamp(rect[3], height - y0));
        let (x0, y0, w, h) = if w == 0 || h == 0 { (0, 0, width, height) } else { (x0, y0, w, h) };
        let step = w.div_ceil(dump_width());
        let (out_w, out_h) = (w / step, h / step);
        let mut rgb = Vec::with_capacity(out_w * out_h * 3);
        for y in 0..out_h {
            for x in 0..out_w {
                let at = (y0 + y * step) * stride + (x0 + x * step) * 4;
                rgb.extend_from_slice(&[data[at + 2], data[at + 1], data[at]]);
            }
        }
        Some(png_rgb(out_w, out_h, &rgb))
    }

    /// Reads the grid from a game's swapchain image. False when the image cannot be read.
    fn read(&mut self, image: &PixelBuffer, rect: [i32; 4]) -> bool {
        let size = (image.width(), image.height());
        image.with_pixels(|data, stride| self.fill(data, stride, size, rect)).unwrap_or(false)
    }
}

/// Decides from one pair of grids per check whether the symptom has held long enough.
#[derive(Default)]
pub struct Detector {
    streak: u32,
    reported: bool,
}

impl Detector {
    /// One check. Returns true once: at the check that completes the streak.
    pub fn check(&mut self, left: &Grid, right: &Grid) -> bool {
        if self.reported {
            return false;
        }
        if left.has_content() && right.is_black() {
            self.streak += 1;
        } else {
            self.streak = 0;
        }
        self.reported = self.streak >= CHECKS_IN_A_ROW;
        self.reported
    }

    /// A check that could not read an eye ends the streak.
    fn unreadable(&mut self) {
        self.streak = 0;
    }
}

/// The detector with its clock and its two grids, for one game run.
#[derive(Default)]
pub struct Watch {
    detector: Detector,
    last: Option<Instant>,
    left: Grid,
    right: Grid,
    /// What the last check saw: whether the left eye has content and whether the right eye is black
    seen: Option<(bool, bool)>,
}

impl Watch {
    /// Whether the next game frame should be checked: about once a second, and never after the
    /// report.
    pub fn due(&self) -> bool {
        !self.detector.reported && self.last.is_none_or(|last| last.elapsed() >= INTERVAL)
    }

    /// Checks the two eyes of a frame (the images and their rects). Returns true once, when the
    /// symptom has held long enough to report.
    pub fn check(&mut self, images: [&PixelBuffer; 2], rects: [[i32; 4]; 2]) -> bool {
        self.last = Some(Instant::now());
        if let Some(dir) = dump_dir() {
            for (name, image, rect) in [("left", images[0], rects[0]), ("right", images[1], rects[1])] {
                let size = (image.width(), image.height());
                let png = image.with_pixels(|data, stride| Grid::png(data, stride, size, rect)).flatten();
                if let Some(png) = png {
                    if let Err(e) = std::fs::write(dir.join(format!("eye-{name}.png")), png) {
                        warn!("Eye check: cannot write the {name} eye to {}: {e}", dir.display());
                    }
                }
            }
        }
        if self.left.read(images[0], rects[0]) && self.right.read(images[1], rects[1]) {
            // The log keeps the first check of a run and each change, so it shows what the host saw
            let seen = (self.left.has_content(), self.right.is_black());
            if self.seen != Some(seen) {
                info!(
                    "Eye check: {} of {SAMPLES} samples lit in the left eye, {} in the right eye",
                    self.left.lit(),
                    self.right.lit()
                );
                self.seen = Some(seen);
            }
            self.detector.check(&self.left, &self.right)
        } else {
            self.detector.unreadable();
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grid with `lit` samples at `level` and the rest at 0.
    fn grid(lit: usize, level: u8) -> Grid {
        let mut grid = Grid::default();
        grid.0[..lit].fill(level);
        grid
    }

    fn lit() -> Grid {
        grid(SAMPLES, 200)
    }

    fn black() -> Grid {
        grid(0, 0)
    }

    /// Runs the checks and returns the number of the one that reported, counting from 1.
    fn reported_at(detector: &mut Detector, checks: &[(Grid, Grid)]) -> Option<usize> {
        checks.iter().position(|(left, right)| detector.check(left, right)).map(|i| i + 1)
    }

    fn repeated(left: fn() -> Grid, right: fn() -> Grid, times: usize) -> Vec<(Grid, Grid)> {
        (0..times).map(|_| (left(), right())).collect()
    }

    #[test]
    fn a_black_right_eye_beside_a_lit_left_eye_reports_once_after_four_checks() {
        let mut detector = Detector::default();
        let checks = repeated(lit, black, 10);
        assert_eq!(reported_at(&mut detector, &checks), Some(4));
        // The later checks stay silent
        assert!(!detector.check(&lit(), &black()));
    }

    #[test]
    fn two_lit_eyes_never_report() {
        let mut detector = Detector::default();
        assert_eq!(reported_at(&mut detector, &repeated(lit, lit, 20)), None);
    }

    #[test]
    fn two_dark_eyes_such_as_a_loading_screen_never_report() {
        let mut detector = Detector::default();
        assert_eq!(reported_at(&mut detector, &repeated(black, black, 20)), None);
    }

    #[test]
    fn a_dim_left_eye_and_a_few_bright_pixels_are_not_content() {
        let mut detector = Detector::default();
        // Below the lit level, as a nearly black scene reads
        let dim = (0..20).map(|_| (grid(SAMPLES, LIT_LEVEL), black())).collect::<Vec<_>>();
        assert_eq!(reported_at(&mut detector, &dim), None);
        // Fewer lit samples than content needs, as a cursor on a black screen
        let cursor = (0..20).map(|_| (grid(CONTENT_SAMPLES - 1, 255), black())).collect::<Vec<_>>();
        assert_eq!(reported_at(&mut detector, &cursor), None);
        // Exactly enough lit samples count
        let enough = (0..CHECKS_IN_A_ROW as usize).map(|_| (grid(CONTENT_SAMPLES, 255), black())).collect::<Vec<_>>();
        assert_eq!(reported_at(&mut detector, &enough), Some(4));
    }

    #[test]
    fn a_right_eye_with_one_lit_sample_is_not_black() {
        let mut detector = Detector::default();
        let checks = (0..20).map(|_| (lit(), grid(1, 255))).collect::<Vec<_>>();
        assert_eq!(reported_at(&mut detector, &checks), None);
    }

    #[test]
    fn a_flicker_between_states_resets_the_count() {
        let mut detector = Detector::default();
        // Three bad checks, then one with both eyes lit, again and again: never four in a row
        let mut checks = Vec::new();
        for _ in 0..10 {
            checks.extend(repeated(lit, black, 3));
            checks.push((lit(), lit()));
        }
        assert_eq!(reported_at(&mut detector, &checks), None);
        // A loading screen between the checks resets the count as well
        let mut checks = repeated(lit, black, 3);
        checks.push((black(), black()));
        checks.extend(repeated(lit, black, 3));
        assert_eq!(reported_at(&mut Detector::default(), &checks), None);
        // The right count after the interruption reports
        checks.push((lit(), black()));
        assert_eq!(reported_at(&mut Detector::default(), &checks), Some(checks.len()));
    }

    #[test]
    fn an_unreadable_check_resets_the_count() {
        let mut detector = Detector::default();
        for _ in 0..3 {
            assert!(!detector.check(&lit(), &black()));
        }
        detector.unreadable();
        assert!(!detector.check(&lit(), &black()));
        assert!(!detector.check(&lit(), &black()));
        assert!(!detector.check(&lit(), &black()));
        assert!(detector.check(&lit(), &black()));
    }

    /// 4-byte pixels of an image: `paint(x, y)` gives a pixel's [b, g, r, a].
    fn image(width: usize, height: usize, stride: usize, paint: impl Fn(usize, usize) -> [u8; 4]) -> Vec<u8> {
        let mut data = vec![0; stride * height];
        for y in 0..height {
            for x in 0..width {
                data[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&paint(x, y));
            }
        }
        data
    }

    #[test]
    fn the_grid_reads_the_brightest_channel_inside_the_rect() {
        // 64 x 32 pixels with 8 bytes of padding per row. The left half is red, the right half is
        // empty. The alpha is 255 everywhere: only the color decides.
        let data = image(64, 32, 64 * 4 + 8, |x, _| if x < 32 { [0, 0, 100, 255] } else { [0, 0, 0, 255] });
        let mut grid = Grid::default();
        assert!(grid.fill(&data, 64 * 4 + 8, (64, 32), [0, 0, 64, 32]));
        assert_eq!(grid.lit(), SAMPLES / 2);
        // A rect on the empty half reads black
        assert!(grid.fill(&data, 64 * 4 + 8, (64, 32), [32, 0, 32, 32]));
        assert!(grid.is_black());
        // A rect on the red half reads lit everywhere
        assert!(grid.fill(&data, 64 * 4 + 8, (64, 32), [0, 0, 32, 32]));
        assert_eq!(grid.lit(), SAMPLES);
        // An empty rect and a rect outside the image stand for the whole image
        assert!(grid.fill(&data, 64 * 4 + 8, (64, 32), [0, 0, 0, 0]));
        assert_eq!(grid.lit(), SAMPLES / 2);
        assert!(grid.fill(&data, 64 * 4 + 8, (64, 32), [500, 500, 10, 10]));
        assert_eq!(grid.lit(), SAMPLES / 2);
    }

    #[test]
    fn a_png_has_valid_chunks_and_the_pixels_of_the_rect() {
        // 8 x 4 pixels, the left half blue (BGRA 255, 0, 0): the PNG holds RGB (0, 0, 255) there
        let data = image(8, 4, 8 * 4, |x, _| if x < 4 { [255, 0, 0, 255] } else { [0, 255, 0, 255] });
        let png = Grid::png(&data, 8 * 4, (8, 4), [0, 0, 8, 4]).expect("an image");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        // Walk the chunks and check each CRC
        let mut at = 8;
        let mut kinds = Vec::new();
        while at < png.len() {
            let length = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = png[at + 4..at + 8].try_into().unwrap();
            let body = &png[at + 8..at + 8 + length];
            let crc = u32::from_be_bytes(png[at + 8 + length..at + 12 + length].try_into().unwrap());
            assert_eq!(crc, crc32(&[&kind, body]), "chunk {}", String::from_utf8_lossy(&kind));
            if &kind == b"IHDR" {
                assert_eq!(u32::from_be_bytes(body[..4].try_into().unwrap()), 8);
                assert_eq!(u32::from_be_bytes(body[4..8].try_into().unwrap()), 4);
            }
            if &kind == b"IDAT" {
                // zlib header, then one stored block that holds the 4 rows of (1 + 8 * 3) bytes
                assert_eq!(&body[..2], &[0x78, 0x01]);
                assert_eq!(body[2], 1);
                assert_eq!(u16::from_le_bytes(body[3..5].try_into().unwrap()), 100);
                assert_eq!(&body[7..11], &[0, 0, 0, 255], "filter byte, then the first pixel as RGB");
            }
            kinds.push(kind);
            at += 12 + length;
        }
        assert_eq!(kinds, [*b"IHDR", *b"IDAT", *b"IEND"]);
        // A known CRC-32: "123456789" gives 0xCBF43926
        assert_eq!(crc32(&[b"123456789"]), 0xCBF4_3926);
        // A rect shrinks the picture: the right half only
        let half = Grid::png(&data, 8 * 4, (8, 4), [4, 0, 4, 4]).expect("an image");
        let width = u32::from_be_bytes(half[16..20].try_into().unwrap());
        assert_eq!(width, 4);
    }

    #[test]
    fn a_grid_does_not_read_past_the_data() {
        let data = vec![0u8; 100];
        let mut grid = Grid::default();
        assert!(!grid.fill(&data, 64 * 4, (64, 32), [0, 0, 64, 32]));
        // A stride smaller than a row of pixels is no image
        assert!(!grid.fill(&vec![0u8; 64 * 32 * 4], 100, (64, 32), [0, 0, 64, 32]));
        // A small image still gives a full grid: samples may repeat a pixel
        let tiny = image(4, 4, 16, |_, _| [255, 255, 255, 255]);
        assert!(grid.fill(&tiny, 16, (4, 4), [0, 0, 4, 4]));
        assert_eq!(grid.lit(), SAMPLES);
    }

    #[test]
    fn a_menu_that_lights_half_a_percent_of_a_dark_eye_has_content() {
        // The spectrum bars of a menu in a dark room: a band of thin bars at the bottom, 0.4 percent of the pixels
        let (width, height) = (1444, 1546);
        let bars = image(width, height, width * 4, |x, y| {
            if y >= height - 80 && (400..900).contains(&x) && x % 3 == 0 { [255, 255, 255, 255] } else { [0, 0, 0, 255] }
        });
        let mut grid = Grid::default();
        assert!(grid.fill(&bars, width * 4, (width, height), [0, 0, width as i32, height as i32]));
        assert!(grid.has_content(), "{} samples lit", grid.lit());
        // The same eye beside a black one reports after four checks, as a lit eye does
        let mut detector = Detector::default();
        let checks = (0..CHECKS_IN_A_ROW as usize).map(|_| (Grid(grid.0), black())).collect::<Vec<_>>();
        assert_eq!(reported_at(&mut detector, &checks), Some(4));
    }

    #[test]
    fn a_watch_reads_real_buffers_once_a_second_and_reports_the_symptom_once() {
        let mut left = PixelBuffer::new_bgra(256, 128).expect("left");
        let right = PixelBuffer::new_bgra(256, 128).expect("right");
        left.with_bgra_mut(|data, stride| {
            for row in 0..128 {
                for x in 0..256 {
                    data[row * stride + x * 4..row * stride + x * 4 + 4].copy_from_slice(&[40, 80, 120, 255]);
                }
            }
        });
        // The new buffer of the right eye holds zeros: an empty slice
        let rects = [[0, 0, 256, 128]; 2];
        let mut watch = Watch::default();
        assert!(watch.due());
        let mut reports = 0;
        for check in 1..=8 {
            if watch.check([&left, &right], rects) {
                reports += 1;
                assert_eq!(check, CHECKS_IN_A_ROW as usize);
            }
        }
        assert_eq!(reports, 1);
        // After the report no frame is due
        assert!(!watch.due());
    }

    #[test]
    fn a_watch_with_two_lit_eyes_stays_quiet_and_waits_between_checks() {
        let mut both = PixelBuffer::new_bgra(64, 64).expect("buffer");
        both.with_bgra_mut(|data, _| data.fill(255));
        let mut watch = Watch::default();
        assert!(watch.due());
        assert!(!watch.check([&both, &both], [[0, 0, 64, 64]; 2]));
        // The clock gates the next check
        assert!(!watch.due());
    }
}
