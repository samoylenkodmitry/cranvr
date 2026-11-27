//! A knob for tests of the eye stream's bitrate. `CRANVR_TEST_BITRATE=FILE` makes the host read FILE
//! about every 100 ms. A host without the variable, or with no file, controls its bitrate as usual.
//!
//! The file holds `NAME=VALUE` words separated by spaces, for example `fixed=80 cap=40`:
//!
//! - `fixed`: the eye stream's bitrate in Mbps. The bitrate's control (`bitrate.rs`) then stands still,
//!   so a test can measure the link at a given bitrate.
//! - `cap`: the host sends at most this many Mbps, as a slower link would carry. A frame that a
//!   link of that rate could not carry within `QUEUE_LIMIT` after the frames before it is dropped,
//!   as a full queue of a router drops it. The frames that pass go out at once. The cap stands for
//!   a link that gets worse while it streams, which a test cannot make on demand.
//! - `slow`: the host's send of a frame waits for the frame's time on a link of this many Mbps
//!   before it sends, as a TCP write waits over a link that carries less than the stream (`adb
//!   reverse`). The sending thread blocks. Before #157 the encoded frames piled up behind it without
//!   a bound and went out all at once when the slow part ended; now the queue before the send drops
//!   them (`send_queue.rs`).

use std::{
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const REREAD: Duration = Duration::from_millis(100);
/// The longest wait of a frame behind the cap. A Wi-Fi 6 link that got 70 Mbps for about 50 held
/// the frames for 41 ms on average (issue #97).
const QUEUE_LIMIT: Duration = Duration::from_millis(50);

/// What the file says.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Knob {
    pub fixed_mbps: Option<u64>,
    pub cap_mbps: Option<u64>,
    pub slow_mbps: Option<u64>,
}

impl Knob {
    pub fn parse(text: &str) -> Self {
        let mut knob = Self::default();
        for word in text.split_whitespace() {
            let Some((name, value)) = word.split_once('=') else { continue };
            let Ok(value) = value.parse::<u64>() else { continue };
            match name {
                "fixed" => knob.fixed_mbps = Some(value).filter(|v| *v > 0),
                "cap" => knob.cap_mbps = Some(value).filter(|v| *v > 0),
                "slow" => knob.slow_mbps = Some(value).filter(|v| *v > 0),
                _ => {}
            }
        }
        knob
    }
}

struct Source {
    path: Option<PathBuf>,
    read_at: Option<Instant>,
    knob: Knob,
    /// When the capped link would be free of the frames before
    free_at: Option<Instant>,
}

fn source() -> &'static Mutex<Source> {
    static SOURCE: OnceLock<Mutex<Source>> = OnceLock::new();
    SOURCE.get_or_init(|| {
        Mutex::new(Source { path: std::env::var_os("CRANVR_TEST_BITRATE").map(PathBuf::from), read_at: None, knob: Knob::default(), free_at: None })
    })
}

fn current(source: &mut Source) -> Knob {
    let Some(path) = source.path.clone() else { return Knob::default() };
    if source.read_at.is_none_or(|at| at.elapsed() >= REREAD) {
        source.read_at = Some(Instant::now());
        source.knob = std::fs::read_to_string(path).map(|text| Knob::parse(&text)).unwrap_or_default();
    }
    source.knob
}

/// Whether a test runs the knob: the host then logs what the link did in each second.
pub fn active() -> bool {
    source().lock().unwrap().path.is_some()
}

/// The bitrate that the test fixes, in Mbps.
pub fn fixed_mbps() -> Option<u64> {
    current(&mut source().lock().unwrap()).fixed_mbps
}

/// Whether a link of the test's cap carries a frame of `bytes` that leaves now. False: the frame is
/// dropped.
pub fn passes_cap(bytes: usize) -> bool {
    let mut source = source().lock().unwrap();
    let Some(cap) = current(&mut source).cap_mbps else {
        source.free_at = None;
        return true;
    };
    let now = Instant::now();
    let start = source.free_at.filter(|t| *t > now).unwrap_or(now);
    if start - now > QUEUE_LIMIT {
        return false;
    }
    source.free_at = Some(start + Duration::from_secs_f64(bytes as f64 * 8.0 / (cap as f64 * 1e6)));
    true
}

/// How long the send of a frame of `bytes` waits on the test's slow link. `None` without one.
pub fn slow_wait(bytes: usize) -> Option<Duration> {
    let slow = current(&mut source().lock().unwrap()).slow_mbps?;
    Some(Duration::from_secs_f64(bytes as f64 * 8.0 / (slow as f64 * 1e6)))
}

#[cfg(test)]
mod tests {
    use super::Knob;

    #[test]
    fn the_words_set_the_fixed_bitrate_the_cap_and_the_slow_link() {
        assert_eq!(Knob::parse("fixed=80 cap=40"), Knob { fixed_mbps: Some(80), cap_mbps: Some(40), slow_mbps: None });
        assert_eq!(Knob::parse("cap=25\n"), Knob { fixed_mbps: None, cap_mbps: Some(25), slow_mbps: None });
        assert_eq!(Knob::parse("slow=60"), Knob { slow_mbps: Some(60), ..Knob::default() });
        assert_eq!(Knob::parse("fixed=0 cap=x other=3"), Knob::default());
    }
}
