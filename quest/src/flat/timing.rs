// The two latency numbers of the window display, from real display times:
// - round trip: the sensor sample of the pose that the host drew for, to the display of that
//   frame. The pose goes to the Mac, the frame comes back, and the device decodes it.
// - motion to display: the newest sensor sample, which warps the frame just before the swap, to
//   the display.
// Every few seconds a log line holds the median and the 90th percentile of each.

use super::window::{Presented, WindowSurface};
use serde_json::json;
use std::{collections::VecDeque, time::Duration};

/// One swap, until its display time is known.
pub struct Shown {
    pub frame_id: u64,
    /// The sample time of the pose that the host drew this frame for. None when the display
    /// shows the previous frame again, warped with a newer pose.
    pub frame_pose_time: Option<Duration>,
    pub warp_pose_time: Duration,
    pub swapped: Duration,
}

// Swaps that wait for their display time. EGL keeps the times of the last few frames only.
const MAX_PENDING: usize = 16;
const LOG_PERIOD: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct Timing {
    /// "egl" when EGL reports each display time, "derived" in front-buffer mode
    pub display_times: &'static str,
    pending: VecDeque<Shown>,
    round_trip: Vec<f64>,
    motion: Vec<f64>,
    swap_to_display: Vec<f64>,
    render: Vec<f64>,
    pose_age: Vec<f64>,
    new_frames: u32,
    repeats: u32,
    unknown: u32,
    last_log: Duration,
    /// The latest swap-to-display time, for ALVR's statistics
    pub display_lag: Duration,
}

impl Timing {
    /// The time from the start of a refresh's work to the return of its swap.
    pub fn rendered(&mut self, time: Duration) {
        self.render.push(time.as_secs_f64() * 1000.0);
    }

    /// The age of the newest sensor sample when the warp takes it.
    pub fn pose_age(&mut self, age: Duration) {
        self.pose_age.push(age.as_secs_f64() * 1000.0);
    }

    pub fn shown(&mut self, shown: Shown) {
        if self.pending.len() == MAX_PENDING {
            self.pending.pop_front();
            self.unknown += 1;
        }
        self.pending.push_back(shown);
    }

    /// A refresh whose display time is known already: a derived one, in front-buffer mode.
    pub fn shown_at(&mut self, shown: Shown, display: Duration) {
        self.record(&shown, display);
    }

    fn record(&mut self, shown: &Shown, display: Duration) {
        let ms = |from: Duration| display.saturating_sub(from).as_secs_f64() * 1000.0;
        self.motion.push(ms(shown.warp_pose_time));
        self.swap_to_display.push(ms(shown.swapped));
        self.display_lag = display.saturating_sub(shown.swapped);
        match shown.frame_pose_time {
            Some(pose_time) => {
                self.round_trip.push(ms(pose_time));
                self.new_frames += 1;
            }
            None => self.repeats += 1,
        }
    }

    /// Takes the display times that EGL knows by now.
    pub fn collect(&mut self, surface: &WindowSurface) {
        let mut still_pending = VecDeque::new();
        while let Some(shown) = self.pending.pop_front() {
            match surface.presented(shown.frame_id) {
                Presented::Pending => still_pending.push_back(shown),
                Presented::Unknown => self.unknown += 1,
                Presented::At(display) => self.record(&shown, display),
            }
        }
        self.pending = still_pending;
    }

    /// The log line, once per period.
    pub fn report(&mut self, now: Duration) -> Option<serde_json::Value> {
        if now.saturating_sub(self.last_log) < LOG_PERIOD {
            return None;
        }
        let seconds = now.saturating_sub(self.last_log).as_secs_f64();
        self.last_log = now;
        let line = json!({
            "display_times": self.display_times,
            "displayed_fps": (self.new_frames + self.repeats) as f64 / seconds,
            "new_frame_fps": self.new_frames as f64 / seconds,
            "repeats": self.repeats,
            "unknown": self.unknown,
            "round_trip_ms": percentiles(&mut self.round_trip),
            "motion_to_display_ms": percentiles(&mut self.motion),
            "swap_to_display_ms": percentiles(&mut self.swap_to_display),
            "render_ms": percentiles(&mut self.render),
            "pose_age_ms": percentiles(&mut self.pose_age),
        });
        (self.new_frames, self.repeats, self.unknown) = (0, 0, 0);
        self.round_trip.clear();
        self.motion.clear();
        self.swap_to_display.clear();
        self.render.clear();
        self.pose_age.clear();
        Some(line)
    }
}

fn percentiles(values: &mut [f64]) -> serde_json::Value {
    if values.is_empty() {
        return serde_json::Value::Null;
    }
    values.sort_by(f64::total_cmp);
    let at = |p: f64| values[((values.len() - 1) as f64 * p).round() as usize];
    json!({ "p10": at(0.1), "p50": at(0.5), "p90": at(0.9) })
}
