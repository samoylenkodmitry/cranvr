//! The display lock: the host's frame clock follows the headset's display.
//!
//! The headset shows a new frame at each point of a grid, `D + k * period`. Its compositor takes
//! the newest decoded frame some time before that point (the *lead*, from the pick to the display).
//! A frame that is ready just before the pick shows at the next grid point. A frame that comes just
//! after the pick waits one more point. The next frame then replaces it. A host that renders at the
//! arrival of each pose has a fixed delay from pose to arrival. That delay can put the arrival at
//! the edge of the pick, so jitter moves frames across it.
//!
//! This module plans each frame instead:
//! - the target is the grid point at which the frame should show;
//! - the start is the target minus the lead and minus the time that the frame needs from its start
//!   to the end of its decode (render, encode, send, network and decode);
//! - the host renders for the pose that is valid at the target.
//!
//! The planner reads no clock and sends nothing. Its times are on the headset's clock, in
//! nanoseconds. The caller converts the host's clock with the clock sync.
//!
//! What the planner learns:
//! - the grid, from the display times that the headset puts in its tracking samples;
//! - the lead, from the compositor's reports: the display time minus the pick time. The planner
//!   uses a high percentile, because a pick that comes early is the case that costs a frame;
//! - the need of a frame, from the reports: the end of the decode minus the planned start. The
//!   planner uses a high percentile and an extra margin;
//! - the margin: it rises when a frame misses its target and falls slowly while frames hit.
//!
//! A report names a frame by its tracking sample. A frame that the headset replaced with a newer
//! one never gets a report. The planner counts each sent frame that is older than a reported frame
//! and has no report as a miss. Without that count the margin could not rise, because only the
//! frames that arrived in time would be measured.

use cranvr_proto::wire::Nanos;
use std::collections::VecDeque;

/// Samples that the percentiles use
const WINDOW: usize = 64;
/// The need of a frame before any report, in ms
const FIRST_NEED_MS: f64 = 16.0;
/// The margin before any report, in ms
const FIRST_MARGIN_MS: f64 = 1.5;
/// The margin falls and rises within these limits, in ms. The upper limit is a share of a period.
const MIN_MARGIN_MS: f64 = 0.3;
const MAX_MARGIN_PERIODS: f64 = 0.7;
/// A missed frame raises the margin by this much, in ms. A frame that hit lowers it by the
/// smaller step. The ratio sets the share of misses at which the margin rests: 0.01 / 0.41 is
/// about 2.5 percent.
const MISS_STEP_MS: f64 = 0.4;
const HIT_STEP_MS: f64 = 0.01;
/// A frame shown a point early lowers the margin by this much, in ms.
const EARLY_STEP_MS: f64 = 0.4;
/// The percentile of the lead and of the need
const PERCENTILE: f64 = 0.9;
/// Frames that wait for their report. An older one is forgotten.
const IN_FLIGHT: usize = 32;

/// What the planner decides for the next frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// When the frame should start, on the headset's clock. It may lie in the past: the frame is
    /// due.
    pub start: Nanos,
    /// The display time that the frame aims for
    pub target: Nanos,
}

/// What the headset reported about a shown frame. All times are on the headset's clock.
#[derive(Clone, Copy, Debug)]
pub struct Report {
    pub sample: Nanos,
    pub decode_end: Nanos,
    pub picked: Nanos,
    pub display_time: Nanos,
}

/// How the frames fared since the last call of `take_stats`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    /// Frames that showed at their target
    pub on_time: u32,
    /// Frames that showed one or more points after it
    pub late: u32,
    /// Frames that showed before it
    pub early: u32,
    /// Frames that the headset replaced with a newer one, or that never came
    pub replaced: u32,
    /// Points of the grid that no frame took, because a frame was late for its plan
    pub skipped_points: u32,
    /// How late the frames started against their plans, on average and at most, in ms
    pub late_start_ms: f64,
    pub late_start_max_ms: f64,
    /// The need of a frame and the lead now, in ms
    pub need_ms: f64,
    pub lead_ms: f64,
    pub margin_ms: f64,
}

impl Stats {
    /// The frames that the headset showed or replaced
    pub fn frames(&self) -> u32 {
        self.on_time + self.late + self.early + self.replaced
    }
}

struct InFlight {
    sample: Nanos,
    target: Nanos,
    start: Nanos,
}

pub struct DisplayLock {
    nominal_period: Nanos,
    /// A display time from a tracking sample. `period` is the time between two
    origin: Option<Nanos>,
    period: f64,
    lead: VecDeque<f64>,
    need: VecDeque<f64>,
    margin_ms: f64,
    last_target: Nanos,
    in_flight: VecDeque<InFlight>,
    stats: Stats,
    late_starts: (f64, u32),
}

fn push(window: &mut VecDeque<f64>, value: f64) {
    if window.len() >= WINDOW {
        window.pop_front();
    }
    window.push_back(value);
}

/// The value at `share` of the sorted samples, or `None` for no samples.
fn percentile(window: &VecDeque<f64>, share: f64) -> Option<f64> {
    if window.is_empty() {
        return None;
    }
    let mut sorted: Vec<f64> = window.iter().copied().collect();
    sorted.sort_by(f64::total_cmp);
    let index = ((sorted.len() as f64 * share).ceil() as usize).clamp(1, sorted.len()) - 1;
    Some(sorted[index])
}

/// The first point of the grid `start + k * period` at or after `t`.
fn grid_at_or_after(start: Nanos, period: f64, t: Nanos) -> Nanos {
    if t <= start || period < 1.0 {
        return start.max(t);
    }
    start + (((t - start) as f64 / period).ceil() * period) as Nanos
}

impl DisplayLock {
    pub fn new(nominal_period: Nanos) -> Self {
        Self {
            nominal_period,
            origin: None,
            period: nominal_period as f64,
            lead: VecDeque::new(),
            need: VecDeque::new(),
            margin_ms: FIRST_MARGIN_MS,
            last_target: 0,
            in_flight: VecDeque::new(),
            stats: Stats::default(),
            late_starts: (0.0, 0),
        }
    }

    /// Notes the display time of a tracking sample. Samples may arrive out of order: a sample that
    /// is not newer is ignored.
    pub fn on_tracking(&mut self, display_time: Nanos) {
        // A client that gives no display time leaves the grid unknown
        if display_time == 0 {
            return;
        }
        if let Some(origin) = self.origin {
            if display_time <= origin {
                return;
            }
            // The gap between two successive samples is one period, or a few when samples are lost
            let gap = (display_time - origin) as f64;
            let points = (gap / self.period).round();
            if points >= 1.0 && points <= 4.0 {
                let one = gap / points;
                if (one - self.nominal_period as f64).abs() < 0.2 * self.nominal_period as f64 {
                    self.period += 0.02 * (one - self.period);
                }
            }
        }
        self.origin = Some(display_time);
    }

    /// Whether the planner knows the grid.
    pub fn ready(&self) -> bool {
        self.origin.is_some()
    }

    pub fn period(&self) -> Nanos {
        self.period as Nanos
    }

    /// The time from a frame's start to the end of its decode that the planner counts on, in ns
    fn need(&self) -> f64 {
        let measured = percentile(&self.need, PERCENTILE).unwrap_or(FIRST_NEED_MS * 1e6);
        measured + self.margin_ms * 1e6
    }

    /// The time from the pick to the display that the planner counts on, in ns
    fn lead(&self) -> f64 {
        percentile(&self.lead, PERCENTILE).unwrap_or(2.0 * self.period)
    }

    /// The next frame's start and target, given the time `now` on the headset's clock. `None`
    /// while the grid is unknown.
    pub fn plan(&self, now: Nanos) -> Option<Plan> {
        let origin = self.origin?;
        let (need, lead) = (self.need(), self.lead());
        let period = self.period;
        // The earliest point at which a frame that starts now can show. The test uses the median
        // need, without the margin: a frame that starts a little after its plan still makes its
        // point. A test with the margin would send it to the point after.
        let median = percentile(&self.need, 0.5).unwrap_or(FIRST_NEED_MS * 1e6);
        let earliest = grid_at_or_after(origin, period, now + (median + lead) as Nanos);
        let target = if self.last_target == 0 {
            earliest
        } else {
            let next = self.last_target + period as Nanos;
            // Wait for the point after the last one, but never more than one point past the
            // earliest. A frame that cannot make the next point takes a later one.
            if next > earliest {
                next.min(earliest + period as Nanos)
            } else {
                earliest
            }
        };
        let start = (target as f64 - need - lead).max(0.0) as Nanos;
        Some(Plan { start, target })
    }

    /// The frame loop took the plan with this target, `late` nanoseconds after the planned start.
    /// The next plan takes a later point.
    pub fn commit(&mut self, target: Nanos, late: Nanos) {
        if self.last_target != 0 && target > self.last_target {
            let points = ((target - self.last_target) as f64 / self.period).round();
            if points > 1.0 {
                self.stats.skipped_points += points as u32 - 1;
            }
        }
        self.last_target = self.last_target.max(target);
        let late_ms = late as f64 / 1e6;
        self.late_starts.0 += late_ms;
        self.late_starts.1 += 1;
        self.stats.late_start_max_ms = self.stats.late_start_max_ms.max(late_ms);
    }

    /// Notes a frame that went out. `start` is the start that the plan gave, so a late wake counts
    /// as part of the frame's need.
    pub fn sent(&mut self, sample: Nanos, target: Nanos, start: Nanos) {
        if self.in_flight.len() >= IN_FLIGHT {
            self.in_flight.pop_front();
        }
        self.in_flight.push_back(InFlight { sample, target, start });
    }

    /// Notes what the headset did with a frame.
    pub fn on_report(&mut self, report: &Report) {
        let Some(at) = self.in_flight.iter().position(|f| f.sample == report.sample) else { return };
        let older = at;
        self.in_flight.drain(..older);
        let frame = self.in_flight.pop_front().expect("the frame is at the front now");
        push(&mut self.need, report.decode_end.saturating_sub(frame.start) as f64);
        push(&mut self.lead, report.display_time.saturating_sub(report.picked) as f64);
        let points = ((report.display_time as f64 - frame.target as f64) / self.period).round();
        // Frames that were sent before this one and have no report were replaced, or they never
        // came. If this frame showed at its target or later, the older ones were late for their
        // own points. The newer frame took their place. If this frame showed early, it was
        // ready at the pick of an older frame's point and replaced that frame. The fault is then
        // this frame's earliness.
        for _ in 0..older {
            self.stats.replaced += 1;
            if points >= 0.0 {
                self.miss();
            }
        }
        if points == 0.0 {
            self.stats.on_time += 1;
            self.margin_ms = (self.margin_ms - HIT_STEP_MS).max(MIN_MARGIN_MS);
        } else if points > 0.0 {
            self.stats.late += 1;
            self.miss();
        } else {
            self.stats.early += 1;
            self.margin_ms = (self.margin_ms - EARLY_STEP_MS).max(MIN_MARGIN_MS);
        }
    }

    fn miss(&mut self) {
        let most = MAX_MARGIN_PERIODS * self.period / 1e6;
        self.margin_ms = (self.margin_ms + MISS_STEP_MS).min(most);
    }

    /// The counts since the last call, with the planner's present estimates.
    pub fn take_stats(&mut self) -> Stats {
        let mut stats = std::mem::take(&mut self.stats);
        stats.late_start_ms = self.late_starts.0 / f64::from(self.late_starts.1.max(1));
        self.late_starts = (0.0, 0);
        stats.need_ms = self.need() / 1e6;
        stats.lead_ms = self.lead() / 1e6;
        stats.margin_ms = self.margin_ms;
        stats
    }
}

#[cfg(test)]
#[path = "../tests/display_lock.rs"]
mod tests;
