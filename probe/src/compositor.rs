//! A model of the headset's compositor for the native probe: the display grid, the take of decoded
//! frames at the start of each headset frame and Cranpose's queue of decoded frames
//! (`FrameQueue` in `cranpose-xr`, which `StreamedScene` uses). The probe uses it to measure what
//! a Quest would do with the frames that the host sends: how many it shows, skips and repeats, and
//! whether the target display times that the host gives lie where the queue wants them.
//!
//! A headset frame shows at the display time `D`. It starts one lead earlier, at `D - lead`. There
//! the frame loop takes the decoded frames that are ready: each enters the queue and the headset's
//! report of it (`FrameTiming`) names the display point of its target. Then the queue gives the
//! frame whose target lies closest to `D`, which the compositor shows.

use std::collections::VecDeque;

/// The frames that wait in the queue, as in Cranpose
const QUEUE_FRAMES: usize = 3;

/// What the model needs to know about a decoded frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub number: u32,
    /// The tracking sample of the frame
    pub sample: u64,
    pub received: u64,
    pub decode_end: u64,
    /// The display time that the host aimed for, or 0
    pub target: u64,
}

/// A frame that entered the queue: what the headset reports of it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Intake {
    pub frame: Frame,
    /// The pick that the frame would have for its own point: one lead before it
    pub picked: u64,
    /// The display time that the frame will have if it is not late: the display time closest to
    /// its target, but not before the display time of the headset frame that took it
    pub display_time: u64,
    /// The frame came after the headset frame of its own point
    pub late: bool,
}

/// What the queue did, since the start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub shown: u64,
    /// Frames that a closer or newer frame replaced, or that found a full queue
    pub skipped: u64,
    /// Headset frames that showed the last picture again
    pub repeated: u64,
    pub intakes: u64,
    pub late: u64,
}

/// What one call of `advance` produced.
#[derive(Default)]
pub struct Output {
    pub intakes: Vec<Intake>,
    /// The frames that the compositor shows, with their display times
    pub shown: Vec<(Frame, u64)>,
}

struct Waiting {
    frame: Frame,
}

pub struct Compositor {
    origin: u64,
    period: u64,
    lead: u64,
    /// The display time of the next headset frame to run
    next: u64,
    /// Decoded frames that no headset frame took yet, in order
    pending: VecDeque<Frame>,
    waiting: VecDeque<Waiting>,
    /// The target of the frame shown last, when it had one. `Some(None)`: a frame with no target.
    shown: Option<Option<u64>>,
    pub counts: Counts,
}

impl Compositor {
    /// A display that shows at `origin + k * period`, with its frame loop starting `lead` before
    /// each display time. The first headset frame shown is the first at or after `from`.
    pub fn new(origin: u64, period: u64, lead: u64, from: u64) -> Self {
        let steps = from.saturating_sub(origin).div_ceil(period);
        Self {
            origin,
            period,
            lead,
            next: origin + steps * period,
            pending: VecDeque::new(),
            waiting: VecDeque::with_capacity(QUEUE_FRAMES),
            shown: None,
            counts: Counts::default(),
        }
    }

    /// A frame that the decoder finished.
    pub fn submit(&mut self, frame: Frame) {
        self.pending.push_back(frame);
    }

    /// Runs the headset frames that start at or before `now`.
    pub fn advance(&mut self, now: u64) -> Output {
        let mut out = Output::default();
        while self.next.saturating_sub(self.lead) <= now {
            let (display, start) = (self.next, self.next.saturating_sub(self.lead));
            while self.pending.front().is_some_and(|f| f.decode_end <= start) {
                let frame = self.pending.pop_front().expect("a frame is pending");
                out.intakes.push(self.take(frame, display, start));
            }
            if let Some(frame) = self.choose(display) {
                out.shown.push((frame, display));
            }
            self.next += self.period;
        }
        out
    }

    fn take(&mut self, frame: Frame, display: u64, start: u64) -> Intake {
        if self.waiting.len() == QUEUE_FRAMES {
            self.waiting.pop_front();
            self.counts.skipped += 1;
        }
        self.waiting.push_back(Waiting { frame });
        self.counts.intakes += 1;
        // The display time closest to the target, on the grid of the headset's display times
        let (point, late) = if frame.target == 0 {
            (display, false)
        } else {
            let steps = ((frame.target as f64 - display as f64) / self.period as f64).round();
            let point = (display as f64 + steps * self.period as f64).max(self.origin as f64) as u64;
            (point.max(display), point < display)
        };
        self.counts.late += u64::from(late);
        // The client reports the pick that the frame would have for its own point: one lead
        // before it
        Intake { frame, picked: point.saturating_sub(display - start), display_time: point, late }
    }

    /// Cranpose's `FrameQueue::choose`: the waiting frame whose target lies closest to
    /// `display_time`, unless the frame shown last is closer.
    fn choose(&mut self, display_time: u64) -> Option<Frame> {
        let Some(index) = self.closest(display_time) else {
            if self.shown.is_some() {
                self.counts.repeated += 1;
            }
            return None;
        };
        self.counts.skipped += self.waiting.drain(..index).count() as u64;
        let chosen = self.waiting.pop_front()?;
        self.shown = Some((chosen.frame.target != 0).then_some(chosen.frame.target));
        self.counts.shown += 1;
        Some(chosen.frame)
    }

    fn closest(&self, display_time: u64) -> Option<usize> {
        let newest = self.waiting.len().checked_sub(1)?;
        if self.waiting.iter().any(|w| w.frame.target == 0) {
            return Some(newest);
        }
        // A full queue means that the decoder is ahead of the display, so the frame shown last no
        // longer competes
        let mut best_gap = match self.shown {
            Some(Some(target)) if self.waiting.len() < QUEUE_FRAMES => Some(target.abs_diff(display_time)),
            _ => None,
        };
        let mut best = None;
        for (index, waiting) in self.waiting.iter().enumerate() {
            let gap = waiting.frame.target.abs_diff(display_time);
            // A tie goes to the later frame
            if best_gap.is_none_or(|best| gap <= best) {
                best_gap = Some(gap);
                best = Some(index);
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD: u64 = 8_333_333;
    const LEAD: u64 = 2 * PERIOD;

    fn frame(number: u32, decode_end: u64, target: u64) -> Frame {
        Frame { number, sample: 1_000 + u64::from(number), received: decode_end - 1_000_000, decode_end, target }
    }

    /// Frames that arrive one a period with the targets that the display needs show one by one.
    #[test]
    fn frames_with_the_right_targets_all_show() {
        let origin = 1_000_000_000;
        let mut c = Compositor::new(origin, PERIOD, LEAD, origin);
        let mut shown = 0;
        for k in 0..100u64 {
            // The frame decodes 3 ms before the start of the headset frame that takes it: point
            // k + 3 of the grid, whose start is point k + 1
            let display = origin + (k + 3) * PERIOD;
            let f = frame(k as u32, display - LEAD - 3_000_000, display);
            c.submit(f);
            shown += c.advance(f.decode_end).shown.len();
        }
        shown += c.advance(origin + 200 * PERIOD).shown.len();
        assert_eq!(shown, 100);
        assert_eq!((c.counts.skipped, c.counts.late), (0, 0));
    }

    /// The case of the host's old targets: a target that lies one lead before the point that
    /// shows the frame counts as late, and two frames in one headset frame leave one of them out.
    #[test]
    fn targets_that_leave_out_the_lead_make_frames_late_and_skip_some() {
        let origin = 1_000_000_000;
        let mut c = Compositor::new(origin, PERIOD, LEAD, origin);
        for k in 0..100u64 {
            // Each frame decodes a little before the start of a headset frame and aims for the
            // point at which it decoded
            let decode_end = origin + (k + 2) * PERIOD - 1_000_000;
            c.submit(frame(k as u32, decode_end, decode_end));
            c.advance(decode_end);
        }
        c.advance(origin + 200 * PERIOD);
        assert!(c.counts.late > 90, "{:?}", c.counts);
    }

    /// A newest frame wins over an older one in the same headset frame, as the queue gives the
    /// closest target to the display time.
    #[test]
    fn two_frames_for_one_headset_frame_show_the_closer_one() {
        let origin = 1_000_000_000;
        let mut c = Compositor::new(origin, PERIOD, LEAD, origin);
        let display = origin + 5 * PERIOD;
        let start = display - LEAD;
        c.submit(frame(1, start - 6_000_000, display - PERIOD));
        c.submit(frame(2, start - 2_000_000, display));
        let out = c.advance(display);
        assert_eq!(out.intakes.len(), 2);
        let first = out.shown.iter().find(|(_, d)| *d == display).expect("the headset frame at the display time shows a frame");
        assert_eq!(first.0.number, 2);
        assert_eq!(c.counts.skipped, 1);
    }

    #[test]
    fn a_headset_frame_without_a_new_frame_repeats_the_last_one() {
        let origin = 1_000_000_000;
        let mut c = Compositor::new(origin, PERIOD, LEAD, origin);
        c.submit(frame(1, origin, origin + 3 * PERIOD));
        let out = c.advance(origin + 10 * PERIOD);
        assert_eq!(out.shown.len(), 1);
        assert!(c.counts.repeated >= 4, "{:?}", c.counts);
    }
}
