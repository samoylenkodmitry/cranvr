//! A made-up sound of this Mac for tests: `CRANVR_TEST_SOUND=FILE@UNIX_MS` plays FILE, a WAV file of
//! 16-bit stereo at 48 kHz, from the wall-clock time UNIX_MS on, with silence before and after it.
//!
//! The file takes the place of the Core Audio tap in the host, which sends it to the headset, and in
//! a recording's sound (`recording_sound.rs`). Both read the same wall clock, so both see each sample
//! at the same moment, as both would see a sound that this Mac plays. A test host started from a
//! terminal has no System Audio Recording permission of cranvr's, so the tap gives it no sound: with
//! this file the test still sends real game sound through the headset and its recorder.

use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const RATE: u64 = crate::wav::SAMPLE_RATE as u64;
/// The sink gets a buffer this often, as the tap's 10 ms
const EVERY: Duration = Duration::from_millis(10);

/// The file and the wall-clock time of its first sample in ms, when the test asks for one
pub fn configured() -> Option<(PathBuf, u64)> {
    let value = std::env::var("CRANVR_TEST_SOUND").ok()?;
    let (path, start) = value.rsplit_once('@')?;
    Some((PathBuf::from(path), start.parse().ok()?))
}

/// The sample frame of the file that sounds at the wall-clock time `now_ns`, which may lie before
/// the file's start or after its end.
fn frame_at(now_ns: u128, start_ms: u64) -> i128 {
    (now_ns as i128 - i128::from(start_ms) * 1_000_000) * i128::from(RATE) / 1_000_000_000
}

fn now_ns() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|since| since.as_nanos()).unwrap_or(0)
}

/// The interleaved stereo bytes of the frames `from..to` of `samples`, with silence outside it.
fn frames(samples: &[i16], from: i128, to: i128) -> Vec<u8> {
    let count = (samples.len() / 2) as i128;
    let mut out = Vec::with_capacity(usize::try_from((to - from).max(0)).unwrap_or(0) * 4);
    for frame in from..to {
        let (left, right) = if (0..count).contains(&frame) {
            let at = usize::try_from(frame).unwrap_or(0) * 2;
            (samples[at], samples[at + 1])
        } else {
            (0, 0)
        };
        out.extend_from_slice(&left.to_le_bytes());
        out.extend_from_slice(&right.to_le_bytes());
    }
    out
}

/// Hands the file's sound to `sink` in buffers of about 10 ms, as the tap does, until `ending`
/// turns true. Returns an error when the file does not read.
pub fn run(sink: fn(&[u8]), ending: &AtomicBool) -> Result<(), String> {
    let (path, start_ms) = configured().ok_or("CRANVR_TEST_SOUND names no FILE@UNIX_MS")?;
    let (channels, samples) = crate::wav::read(&path).map_err(|e| format!("the test sound: {e}"))?;
    if channels != 2 {
        return Err(format!("the test sound {} has {channels} channels, not 2", path.display()));
    }
    let mut last = frame_at(now_ns(), start_ms);
    while !ending.load(Ordering::Relaxed) {
        std::thread::sleep(EVERY);
        let now = frame_at(now_ns(), start_ms);
        if now > last {
            sink(&frames(&samples, last, now));
            last = now;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{frame_at, frames};

    // A sample sounds at the same wall-clock time in every process that reads the file
    #[test]
    fn the_frame_follows_the_wall_clock_from_the_start() {
        let start_ms = 1_700_000_000_000_u64;
        let start_ns = u128::from(start_ms) * 1_000_000;
        assert_eq!(frame_at(start_ns, start_ms), 0);
        assert_eq!(frame_at(start_ns + 1_000_000_000, start_ms), 48_000);
        assert_eq!(frame_at(start_ns - 500_000_000, start_ms), -24_000);
    }

    #[test]
    fn frames_outside_the_file_are_silence() {
        let samples = [1, 2, 3, 4];
        let bytes = frames(&samples, -1, 3);
        let values: Vec<i16> = bytes.as_chunks::<2>().0.iter().map(|pair| i16::from_le_bytes(*pair)).collect();
        assert_eq!(values, vec![0, 0, 1, 2, 3, 4, 0, 0]);
    }
}
