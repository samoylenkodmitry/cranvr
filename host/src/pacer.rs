//! The frame budget of the host's own scene. The budget refills at the stream's refresh rate, so
//! over time the host sends as many frames as the headset shows. A frame draws as soon as a pose
//! arrives while the budget holds it.
//!
//! A gap check between frames lets a frame in up to the allowed share early every time: with
//! poses three times per refresh the host sent about 10 percent more frames than the headset
//! showed, and the extra frames waited in the headset's decoder queue.

use std::time::Instant;

/// A pose that comes a tenth of a frame early still draws; the budget lends it the rest.
pub const READY: f32 = 0.9;
/// A frame waits for the first pose after the budget holds it, so the budget keeps up to a pose
/// interval past one frame. A cap of one frame drops that part: with late poses the host fell
/// to 47 frames of 50.
const CAP: f32 = 2.0;

pub struct Pacer {
    budget: f32,
    last: Instant,
    fps: f32,
    /// The budget that a frame needs
    ready: f32,
}

impl Pacer {
    pub fn new(fps: f32) -> Self {
        Self { budget: 1.0, last: Instant::now(), fps, ready: READY }
    }

    /// Lets a frame draw when the budget holds this share of it, up to 1. A client that sends one
    /// pose per refresh needs a lower share than `READY`. Its poses arrive a few milliseconds
    /// early or late. Each pose that comes early would otherwise lose its frame. The budget keeps
    /// the long-run rate at the refresh rate.
    pub fn with_ready(mut self, ready: f32) -> Self {
        self.ready = ready.clamp(0.0, 1.0);
        self
    }

    pub fn set_fps(&mut self, fps: f32) {
        self.refill();
        self.fps = fps;
    }

    /// Whether a frame may draw now.
    pub fn ready(&mut self) -> bool {
        self.refill();
        self.budget >= self.ready
    }

    /// Takes one frame from the budget.
    pub fn spend(&mut self) {
        self.refill();
        self.budget -= 1.0;
    }

    fn refill(&mut self) {
        let now = Instant::now();
        self.budget = (self.budget + (now - self.last).as_secs_f32() * self.fps).min(CAP);
        self.last = now;
    }
}

#[cfg(test)]
#[path = "tests/pacer.rs"]
mod tests;
