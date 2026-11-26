//! What the frame loop does when the GPU stops its frames (issue #148).
//!
//! macOS stops a command buffer of the host when another process holds the GPU too long. It says
//! "Impacting Interactivity", or "Caused GPU Hang Error" for the work that ran when the GPU hung,
//! or "Discarded (victim of GPU error/recovery)" for the work that the recovery threw away. The
//! next frame draws again. The loop therefore drops the frame and goes on. After several such
//! errors macOS ignores all GPU work of the process ("Ignored (for causing prior/excessive GPU
//! errors)"). Then no frame draws again. The host tries new command queues once and then ends. The
//! cranvr app starts a new host, which macOS lets use the GPU.
//!
//! The log gets one line a second at most: an ignored host failed 6146 frames in 25 s, each with a
//! line.

use crate::render::GpuError;
use std::time::{Duration, Instant};

/// The exit code of a host that ends because the GPU no longer draws its frames
pub const EXIT_GPU: i32 = 75;
/// So many failed frames in a row end the host. A stall of another process stops one or two
/// frames, each after hundreds of milliseconds. A host whose work macOS ignores fails every frame
/// at once: 30 frames are a quarter of a second at 120 Hz.
const MAX_FAILED_IN_ROW: u32 = 30;
/// A wait for the GPU this long ends the host. Another process's work held a frame up to about a
/// second on 2026-11-26. macOS stopped the frame then. A frame that waits three times as long
/// waits for a GPU that does not come back.
pub const MAX_GPU_WAIT: Duration = Duration::from_secs(3);
/// A frame that took this long waited for the GPU behind another process's work
const SLOW_FRAME_MS: f64 = 100.0;

/// What the frame loop does after a failed frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Drop the frame and draw the next one
    Continue,
    /// Make new command queues, then draw the next frame
    NewQueues,
    /// End the host, for this reason
    Exit(String),
}

/// A log line at most once a second for each kind, with the count of the lines left out.
#[derive(Default)]
struct Throttle {
    logged_at: Option<Instant>,
    skipped: u32,
}

impl Throttle {
    /// The suffix for a line that may go out now, or `None` when this line stays out.
    fn admit(&mut self, now: Instant, every: Duration) -> Option<String> {
        if self.logged_at.is_some_and(|at| now.duration_since(at) < every) {
            self.skipped += 1;
            return None;
        }
        self.logged_at = Some(now);
        let suffix = match std::mem::take(&mut self.skipped) {
            0 => String::new(),
            n => format!(" ({n} more since the last line)"),
        };
        Some(suffix)
    }
}

#[derive(Default)]
pub struct GpuHealth {
    failed_in_row: u32,
    /// The host made new command queues after macOS ignored its work
    renewed: bool,
    errors: Throttle,
    slow: Throttle,
}

impl GpuHealth {
    /// Notes a frame that drew, with its times in milliseconds: from the commit to the end and on
    /// the GPU. Returns a log line for a slow frame, at most one every 5 s.
    pub fn drew(&mut self, wall_ms: f64, gpu_ms: f64, now: Instant) -> Option<String> {
        self.failed_in_row = 0;
        if wall_ms < SLOW_FRAME_MS {
            return None;
        }
        let suffix = self.slow.admit(now, Duration::from_secs(5))?;
        Some(format!("Render: a frame took {wall_ms:.0} ms, {gpu_ms:.1} ms of it on the GPU: another process held the GPU{suffix}"))
    }

    /// Notes a frame that the GPU stopped. Returns what the loop does and the log line, when one
    /// is due.
    pub fn failed(&mut self, e: &GpuError, now: Instant) -> (Verdict, Option<String>) {
        self.failed_in_row += 1;
        let line = self.errors.admit(now, Duration::from_secs(1)).map(|suffix| format!("Render: {e}{suffix}"));
        let verdict = if e.ignored() && !self.renewed {
            self.renewed = true;
            Verdict::NewQueues
        } else if e.ignored() {
            Verdict::Exit("macOS ignores the GPU work of this host, also on new command queues".into())
        } else if self.failed_in_row >= MAX_FAILED_IN_ROW {
            Verdict::Exit(format!("the GPU stopped {} frames in a row", self.failed_in_row))
        } else {
            Verdict::Continue
        };
        (verdict, line)
    }
}

#[cfg(test)]
#[path = "tests/gpu_health.rs"]
mod tests;
