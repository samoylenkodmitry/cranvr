//! The eye stream's bitrate follows the link while it streams (issue #97).
//!
//! Once a second the frame loop hands the controller what the headset reported in that second
//! (`Headset::take_link_window`): how many frames it showed and how many of them missed their
//! point of the display grid (late, replaced or lost). The controller lowers the target when the
//! misses rise and raises it slowly while they stay low. The target stays between `FLOOR_BPS` and
//! the bitrate that the user set. The user's setting is the ceiling.
//!
//! The controller reads no clock: each call of `tick` is one second. Tests drive it with made-up
//! seconds.
//!
//! What the thresholds rest on. A Quest 3 at 120 Hz with a moving test game and the host's own scene,
//! each bitrate for 5 s, on the USB network (NCM, about 330 Mbps) and on a Wi-Fi 6 network with the
//! Mac on Wi-Fi too (#97, 2026-11-26 and 27):
//! - A frame crossed both links at about 330 Mbps, plus 0.4 ms on the cable and 2.3 ms on Wi-Fi. A
//!   frame of 175 KB (200 Mbps) took 4.5 ms over the cable and 6.5 ms over Wi-Fi. No packet was
//!   lost and the Mac refused no send. Both links carried a target of 200 Mbps (about 170 sent).
//! - The display lock steers the misses of its planned frames to a rest of about 2.4 percent
//!   (`display_lock.rs`). The host's scene missed 0 to 7 frames a second (3 percent on average) at
//!   200 Mbps over the cable, and 0 to 7 (2.7 percent) at 40 Mbps over Wi-Fi. Those misses are no
//!   sign of a full link, so a calm second may hold them.
//! - A link that carries less than the stream sends queues or drops the frames: most frames of a
//!   second miss, 57 to 86 percent at 70 Mbps over Wi-Fi in the issue's first measurements, and 28
//!   to 92 percent behind the test cap of `test_bitrate.rs`.
//! - The host aims a game's frame at the point that its network time allows (`native.rs`), so a
//!   frame that waits in a queue of the link shows later but counts as on time. A queue shows in
//!   two other ways: the network time grows, from at most 0.8 of a period on both links at 200 Mbps
//!   to 2 to 5 periods behind a full queue, and the headset reports fewer frames than the host sent.
//!   A second counts the frames that the headset did not report as missed, and a second with a
//!   network time over `QUEUE_PERIODS` counts as a burst.
//!
//! So a second is calm with up to `CALM_SHARE` of its frames missed, one second with `BURST_SHARE`
//! or more cuts at once, and `SPAN_SHARE` over `SPAN_TICKS` seconds cuts too. Between the calm and
//! the cuts lies a dead band in which the target stands still.
//!
//! Three more rules keep the target from swinging on a steady link:
//! - after a cut it waits `HOLD_TICKS` before it looks again: the encoder follows a new bitrate
//!   within a second, and the frames of the second after the cut still carry the old size;
//! - it remembers the level at which the misses came and raises only to `SAFE_SHARE` of it until a
//!   wait has passed. The wait doubles each time the same level fails again;
//! - a raise needs `CALM_TICKS` calm seconds in a row.

/// How often the frame loop calls `Control::tick`
pub const TICK: std::time::Duration = std::time::Duration::from_secs(1);

/// The target at the start of a session over a network, in bits a frame: 100 Mbps at 120 Hz. A
/// session over a USB cable starts at the user's setting. Over a Wi-Fi 6 network with the Mac on
/// Wi-Fi a start at 200 Mbps gave network times of 9 to 13 ms with peaks of 90 to 257 ms, 4 to 14
/// late frames a second, and the host made only 75 to 111 frames a second. That lasted 7 s, until the
/// target fell to 112 Mbps. The same network was calm at 112 to 123 Mbps and at a fixed 120: no
/// missed frame and 5 ms a frame. A later start at 100 Mbps reached 200 in 26 s with 0.7 percent
/// late. Over the cable a start at 200 Mbps was calm from its first second.
pub const NETWORK_START_FRAME_BITS: f64 = 100e6 / 120.0;

/// The target at the start of a session: the user's setting over a USB cable, else the start for a
/// network at the stream's rate.
pub fn start_for(ceiling: u64, cable: bool, fps: f32) -> u64 {
    if cable {
        ceiling
    } else {
        ceiling.min((NETWORK_START_FRAME_BITS * f64::from(fps)) as u64)
    }
}

/// The lowest target. Over Wi-Fi a moving game missed 2.2 percent of its frames at 10 Mbps and
/// 2.0 percent at 5 Mbps: a lower bitrate made the link no calmer and the picture worse. The fixed
/// Wi-Fi limit before #97 sent about 8.5 Mbps at 120 Hz.
pub const FLOOR_BPS: u64 = 10_000_000;
/// A second needs reports of at least this share of the refresh rate to count. A headset that shows
/// no stream (Room at Passthrough, no game) reports nothing, and one without input focus gets no
/// frames.
const MIN_FRAMES_SHARE: f64 = 0.25;
/// After this many seconds without reports the target falls to the floor: nobody sees the picture,
/// and its bits cost the Mac's encoder, the link and the headset's decoder (#97: the headset decoded
/// all 120 frames a second of a stream that it did not show). When reports come again the control
/// starts anew at its start. The first second with reports also holds the frames that the headset
/// replaced while it showed nothing, up to 31 of them, so it decides nothing.
const SILENT_TICKS: u32 = 2;
/// A second is calm with at most this share of missed frames: 5 of 120, above the 0 to 7 a second
/// of a calm link at the display lock's rest.
const CALM_SHARE: f64 = 0.042;
/// A second with this share of missed frames or more cuts at once: 15 of 120, twice the most that a
/// calm second missed.
const BURST_SHARE: f64 = 0.125;
/// This share of missed frames over the last `SPAN_TICKS` seconds cuts too: twice the lock's rest.
const SPAN_TICKS: usize = 5;
const SPAN_SHARE: f64 = 0.06;
/// The frames that the host sent and the headset has not reported yet, as a share of those sent,
/// that a calm second holds: the frames of the last 40 ms are still on their way at the end of a
/// second. The reports of a calm link came within 2 of the frames sent.
const UNREPORTED_SLACK: f64 = 0.05;
/// A second whose frames took this many periods on the network on average is a burst
const QUEUE_PERIODS: f64 = 1.5;
/// Calm seconds in a row before a raise
const CALM_TICKS: u32 = 3;
/// A raise multiplies the target by this, and adds at least `RAISE_MIN_BPS`. From the floor to
/// 200 Mbps takes about 90 s of a calm link, from a cut of a third back to its level about 12 s.
const RAISE: f64 = 1.1;
const RAISE_MIN_BPS: u64 = 1_000_000;
/// A cut multiplies the target by one minus twice the share of missed frames, within these limits:
/// a full link that misses most frames halves the target, a link at the edge loses a fifth.
const CUT_MOST: f64 = 0.5;
const CUT_LEAST: f64 = 0.8;
/// Seconds after a cut without a decision. The second after a cut still holds frames of the old
/// size and the reports of frames that the old size made late.
const HOLD_TICKS: u32 = 1;
/// Seconds at the start of a session without a decision: the clock sync and the display lock learn
/// in them, and the first frame is a key frame
const START_TICKS: u32 = 2;
/// After a cut the target stays under this share of the level that failed ...
const SAFE_SHARE: f64 = 0.9;
/// ... for this many seconds at first. Each new failure of the same level doubles the wait, up to
/// `PROBE_WAIT_MOST`. On a steady link a probe then costs one second of misses every 4 minutes.
const PROBE_WAIT_TICKS: u32 = 30;
const PROBE_WAIT_MOST: u32 = 240;
/// A failure at this share of the remembered level or more counts as the same level again
const SAME_LEVEL: f64 = 0.95;

/// What the headset reported in one second, and what the host sent in it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Second {
    /// Frames that showed or that the headset replaced
    pub frames: u32,
    /// Frames that missed their point: late, replaced or lost
    pub missed: u32,
    /// Frames that the host handed to the encoder
    pub sent: u32,
    /// The average time of a frame from the start of its send to its last packet, in ms
    pub network_ms: f64,
}

impl Second {
    /// The missed frames with those that the headset did not report, against all frames
    fn misses(&self) -> (u32, u32) {
        let slack = (f64::from(self.sent) * UNREPORTED_SLACK).ceil() as u32;
        let unreported = self.sent.saturating_sub(self.frames + slack);
        (self.missed + unreported, self.frames.max(self.sent))
    }

    fn share(&self) -> f64 {
        let (missed, frames) = self.misses();
        f64::from(missed) / f64::from(frames.max(1))
    }
}

/// Why the target changed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Change {
    /// Misses came. `share` is the share of missed frames that caused the cut.
    Cut { from: u64, to: u64, share: f64 },
    /// The link stayed calm.
    Raise { from: u64, to: u64 },
    /// The headset reported no frames for `SILENT_TICKS` seconds: it shows no stream.
    Unseen { from: u64, to: u64 },
    /// The headset reports frames again: the control starts anew.
    Seen { from: u64, to: u64 },
}

pub struct Control {
    ceiling: u64,
    floor: u64,
    start: u64,
    fps: f32,
    target: u64,
    /// Seconds in a row without reports
    silent: u32,
    /// The seconds since the last cut, newest last, at most `SPAN_TICKS`
    span: Vec<Second>,
    calm: u32,
    hold: u32,
    /// The level at which misses came last
    failed: Option<u64>,
    /// Seconds until a raise may pass `SAFE_SHARE` of the failed level, and the wait after the
    /// next failure of that level
    probe_in: u32,
    probe_wait: u32,
    min_frames: u32,
    /// The network time of a second that counts as a burst, in ms
    queue_ms: f64,
}

impl Control {
    /// A controller for a stream at `fps` frames a second with the user's bitrate `ceiling`. It
    /// starts at `start`.
    pub fn new(ceiling: u64, start: u64, fps: f32) -> Self {
        let floor = FLOOR_BPS.min(ceiling);
        let start = start.clamp(floor, ceiling);
        Self {
            ceiling,
            floor,
            start,
            fps,
            target: start,
            silent: 0,
            span: Vec::with_capacity(SPAN_TICKS),
            calm: 0,
            hold: START_TICKS,
            failed: None,
            probe_in: 0,
            probe_wait: PROBE_WAIT_TICKS,
            min_frames: (f64::from(fps) * MIN_FRAMES_SHARE).round() as u32,
            queue_ms: QUEUE_PERIODS * 1000.0 / f64::from(fps.max(1.0)),
        }
    }

    pub fn target(&self) -> u64 {
        self.target
    }

    /// One second passed with these reports, or none. Returns the change of the target, if any.
    pub fn tick(&mut self, second: Option<Second>) -> Option<Change> {
        self.probe_in = self.probe_in.saturating_sub(1);
        let Some(second) = second.filter(|s| s.frames >= self.min_frames) else {
            return self.silence();
        };
        if self.silent >= SILENT_TICKS {
            let from = self.target;
            *self = Self::new(self.ceiling, self.start, self.fps);
            return Some(Change::Seen { from, to: self.target });
        }
        self.silent = 0;
        if self.hold > 0 {
            self.hold -= 1;
            return None;
        }
        if self.span.len() == SPAN_TICKS {
            self.span.remove(0);
        }
        self.span.push(second);
        if second.share() >= BURST_SHARE || second.network_ms > self.queue_ms {
            return self.cut(second.share().max(BURST_SHARE));
        }
        let (missed, frames) = self.span.iter().map(Second::misses).fold((0, 0), |(m, f), (sm, sf)| (m + sm, f + sf));
        let span_share = f64::from(missed) / f64::from(frames.max(1));
        if self.span.len() == SPAN_TICKS && span_share >= SPAN_SHARE {
            return self.cut(span_share);
        }
        self.calm = if second.share() <= CALM_SHARE { self.calm + 1 } else { 0 };
        if self.calm < CALM_TICKS {
            return None;
        }
        self.calm = 0;
        self.raise()
    }

    fn silence(&mut self) -> Option<Change> {
        self.silent += 1;
        if self.silent != SILENT_TICKS || self.target <= self.floor {
            return None;
        }
        let from = self.target;
        self.target = self.floor;
        Some(Change::Unseen { from, to: self.floor })
    }

    fn raise(&mut self) -> Option<Change> {
        let most = match self.failed {
            Some(failed) if self.probe_in > 0 => self.ceiling.min((failed as f64 * SAFE_SHARE) as u64),
            _ => self.ceiling,
        };
        let raised = whole_mbps((self.target as f64 * RAISE) as u64).max(self.target + RAISE_MIN_BPS).min(most);
        if raised <= self.target {
            return None;
        }
        let from = self.target;
        self.target = raised;
        // A raise past the failed level after the wait is the probe. Its calm seconds before the
        // next raise show that the level holds now.
        if self.failed.is_some_and(|failed| from > failed) {
            self.failed = None;
            self.probe_wait = PROBE_WAIT_TICKS;
        }
        Some(Change::Raise { from, to: raised })
    }

    fn cut(&mut self, share: f64) -> Option<Change> {
        let from = self.target;
        self.span.clear();
        self.calm = 0;
        self.hold = HOLD_TICKS;
        let again = self.failed.is_some_and(|failed| from as f64 >= failed as f64 * SAME_LEVEL);
        self.probe_wait = if again { (self.probe_wait * 2).min(PROBE_WAIT_MOST) } else { PROBE_WAIT_TICKS };
        self.probe_in = self.probe_wait;
        self.failed = Some(from);
        let to = whole_mbps((from as f64 * (1.0 - 2.0 * share).clamp(CUT_MOST, CUT_LEAST)) as u64).max(self.floor);
        if to >= from {
            return None;
        }
        self.target = to;
        Some(Change::Cut { from, to, share })
    }
}

/// A bitrate in whole Mbps, so the log and the app show the target as it is
fn whole_mbps(bps: u64) -> u64 {
    bps / 1_000_000 * 1_000_000
}

/// The log line of one second of the link: what the headset reported, what the host sent, and the
/// target with its change.
pub fn describe(w: &crate::headset::LinkWindow, encoded: u32, change: Option<Change>, target: u64) -> String {
    let per = |sum: f64, n: u32| sum / f64::from(n.max(1));
    let change = match change {
        Some(Change::Cut { from, to, share }) => {
            format!("; the target falls from {} to {} Mbps: {:.1}% of the frames missed their point", from / 1_000_000, to / 1_000_000, share * 100.0)
        }
        Some(Change::Raise { from, to }) => format!("; the target rises from {} to {} Mbps: the link stayed calm", from / 1_000_000, to / 1_000_000),
        Some(Change::Unseen { from, to }) => {
            format!("; the target falls from {} to {} Mbps: the headset shows no stream", from / 1_000_000, to / 1_000_000)
        }
        Some(Change::Seen { from, to }) => format!("; the target goes from {} to {} Mbps: the headset shows the stream again", from / 1_000_000, to / 1_000_000),
        None => String::new(),
    };
    format!(
        "Eye stream link: {encoded} frames encoded, {} reported, {} late, {} replaced; network {:.2} ms (most {:.2}); {} frames sent, {:.1} KB a frame, send {:.2} ms (most {:.2}), {} sends refused; target {} Mbps{change}",
        w.frames,
        w.late,
        w.replaced,
        per(w.network_ms, w.network_frames),
        w.network_max_ms,
        w.sent_frames,
        w.sent_bytes as f64 / f64::from(w.sent_frames.max(1)) / 1000.0,
        per(w.send_ms, w.sent_frames),
        w.send_max_ms,
        w.refused_sends,
        target / 1_000_000,
    )
}

#[cfg(test)]
#[path = "tests/bitrate.rs"]
mod tests;
