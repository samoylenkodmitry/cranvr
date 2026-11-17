//! Clock sync. The host and the client each read their own monotonic clock. One side pings. The
//! other answers with the times it saw. The pinger computes the offset between the clocks and the
//! round trip, as NTP does.
//!
//! A single exchange is off by half the difference between the two one-way delays. Queueing makes
//! that difference large for slow exchanges. The estimate therefore keeps the last
//! [`WINDOW`] exchanges, picks the [`BEST`] with the shortest round trip and averages their
//! offsets. The short exchanges crossed an idle path, so their delays are closest to equal.

use crate::wire::Nanos;
use std::{sync::OnceLock, time::Instant};

/// Exchanges that the estimate looks at.
pub const WINDOW: usize = 32;
/// Exchanges among them that count: the ones with the shortest round trip.
pub const BEST: usize = 8;

static START: OnceLock<Instant> = OnceLock::new();

/// This process's monotonic clock in nanoseconds. The first reading is one second, so no reading
/// is 0, which other code uses for "no time".
pub fn now_ns() -> Nanos {
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64 + 1_000_000_000
}

#[derive(Clone, Copy, Debug, Default)]
struct Exchange {
    offset: i64,
    rtt: u64,
}

/// The estimate: `peer_clock - local_clock` and the round trip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Estimate {
    pub offset_ns: i64,
    pub rtt_ns: u64,
}

#[derive(Debug, Default)]
pub struct ClockSync {
    ring: [Exchange; WINDOW],
    len: usize,
    next: usize,
    estimate: Option<Estimate>,
}

impl ClockSync {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one exchange. `t0` is the local time when the ping left, `t1` the peer's time when it
    /// arrived, `t2` the peer's time when the answer left, `t3` the local time when the answer
    /// arrived. It returns the new estimate, or `None` for an exchange with times that cannot be
    /// right (the answer left before the ping arrived, or arrived before the ping left).
    pub fn add(&mut self, t0: Nanos, t1: Nanos, t2: Nanos, t3: Nanos) -> Option<Estimate> {
        let (t0, t1, t2, t3) = (t0 as i128, t1 as i128, t2 as i128, t3 as i128);
        let rtt = (t3 - t0) - (t2 - t1);
        if rtt < 0 || t2 < t1 || t3 < t0 {
            return None;
        }
        let offset = ((t1 - t0) + (t2 - t3)) / 2;
        self.ring[self.next] = Exchange { offset: offset as i64, rtt: rtt as u64 };
        self.next = (self.next + 1) % WINDOW;
        self.len = (self.len + 1).min(WINDOW);
        self.estimate = Some(self.compute());
        self.estimate
    }

    fn compute(&self) -> Estimate {
        let mut best = [Exchange::default(); WINDOW];
        best[..self.len].copy_from_slice(&self.ring[..self.len]);
        let best = &mut best[..self.len];
        best.sort_unstable_by_key(|e| e.rtt);
        let best = &best[..self.len.min(BEST)];
        let n = best.len() as i128;
        Estimate {
            offset_ns: (best.iter().map(|e| e.offset as i128).sum::<i128>() / n) as i64,
            rtt_ns: (best.iter().map(|e| e.rtt as i128).sum::<i128>() / n) as u64,
        }
    }

    pub fn estimate(&self) -> Option<Estimate> {
        self.estimate
    }

    /// A local time on the peer's clock.
    pub fn to_peer(&self, local: Nanos) -> Option<Nanos> {
        let e = self.estimate?;
        Some((local as i128 + e.offset_ns as i128).max(0) as Nanos)
    }

    /// A peer time on the local clock.
    pub fn to_local(&self, peer: Nanos) -> Option<Nanos> {
        let e = self.estimate?;
        Some((peer as i128 - e.offset_ns as i128).max(0) as Nanos)
    }
}
