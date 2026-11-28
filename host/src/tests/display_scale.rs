//! A capture larger than the display's stream: the encoder scales it to the stream's size.
//!
//! The host captures a display at its own pixels and the encoder's scaler shrinks the picture to the
//! width that the client asks for (`Conversion`). The test sends a capture of 3840 x 2160 with lines
//! two pixels wide through a stream of 2560 x 1440 and decodes it. The picture must come out whole
//! at the stream's size, and the lines must keep their contrast. ScreenCaptureKit's own scaling kept
//! 30 % of it in `scripts/sharpness/mac-check.sh`, this scaling 59 %.

use super::*;
use cranvr_vt::{DecodedFrame, Decoder};

const SOURCE: [u32; 2] = [3840, 2160];
const STREAM: [u32; 2] = [2560, 1440];

/// The capture: mid grey, and lines two pixels wide across its left half
fn capture() -> PixelBuffer {
    let config = EncoderConfig { width: SOURCE[0], height: SOURCE[1], fps: 30.0, bitrate_bps: 20_000_000, full_range: true, strict_rate: false, key_every: None, max_qp: None, real_time: false };
    let (sender, _frames) = mpsc::channel();
    let source = Encoder::new(&config, sender).expect("an encoder of the capture's size");
    let mut buffer = source.pixel_buffer().expect("a buffer of the capture's size");
    let (w, h) = (SOURCE[0] as usize, SOURCE[1] as usize);
    buffer.with_planes_mut(|y, ys, uv, uvs| {
        for row in 0..h {
            for col in 0..w {
                y[row * ys + col] = if col < w / 2 { if col % 4 < 2 { 16 } else { 235 } } else { 128 };
            }
        }
        for row in 0..h / 2 {
            uv[row * uvs..row * uvs + w].fill(128);
        }
    });
    buffer
}

/// The amplitude of the lines' frequency along a row of the decoded luma, as a share of the range
/// from the dark lines to the light ones
fn contrast(luma: &[u8], width: usize, row: usize, period: f64, span: std::ops::Range<usize>) -> f64 {
    let line = &luma[row * width..(row + 1) * width];
    let values: Vec<f64> = span.map(|col| f64::from(line[col])).collect();
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let (re, im) = values.iter().enumerate().fold((0.0, 0.0), |(re, im), (k, v)| {
        let phase = std::f64::consts::TAU * k as f64 / period;
        (re + (v - mean) * phase.cos(), im + (v - mean) * phase.sin())
    });
    // A square wave from 16 to 235 has a first harmonic of (4 / pi) * 219 / 2
    2.0 * (re * re + im * im).sqrt() / values.len() as f64 / (4.0 / std::f64::consts::PI * 219.0 / 2.0)
}

#[test]
fn a_capture_larger_than_the_stream_comes_out_whole_at_the_streams_size() {
    let (sender, encoded) = mpsc::channel();
    let mut display = DisplayEncoder::new(STREAM, 30, 9, sender, Conversion { source: SOURCE, flip: false }).expect("the display's encoder");
    let start = Instant::now();
    let mut feed = Feed::new(start, 9_000_000);
    let picture = feed.next(Some((1, capture())), start).expect("the first capture goes in");
    display.encode(&picture, 1).expect("the encoder takes a larger capture");
    let frame = encoded.recv_timeout(Duration::from_secs(5)).expect("an encoded frame");
    let (decoded_sender, decoded) = mpsc::channel::<DecodedFrame>();
    let mut decoder = Decoder::with_luma(frame.config_nals.as_ref().expect("a key frame"), decoded_sender).expect("a decoder");
    decoder.decode(frame.timestamp_ns, &frame.data).expect("decode");
    let picture = decoded.recv_timeout(Duration::from_secs(5)).expect("a decoded picture");
    assert_eq!([picture.width as u32, picture.height as u32], STREAM);
    // The grey half stays where it was: the picture is scaled, not cut
    let (w, row) = (picture.width, picture.height / 2);
    assert!(picture.luma[row * w + w * 3 / 4].abs_diff(128) <= 3, "the right half is grey: {}", picture.luma[row * w + w * 3 / 4]);
    // Lines of 2 pixels at 3840 are lines of 4 / 3 pixels at 2560: a period of 8 / 3 pixels
    let kept = contrast(&picture.luma, w, row, 8.0 / 3.0, 64..w / 2 - 64);
    assert!(kept > 0.4, "the lines keep {:.0} % of their contrast", kept * 100.0);
}
