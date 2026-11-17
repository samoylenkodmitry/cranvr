//! The clock estimate from simulated exchanges with an offset, a delay and jitter.

mod common;

use common::Rng;
use cranvr_proto::clock::{now_ns, ClockSync};

/// The peer's clock at local time `t` for an offset and a drift in parts per million.
fn peer_time(t: u64, offset: i64, drift_ppm: f64) -> u64 {
    ((t as f64 * (1.0 + drift_ppm * 1e-6)) as i128 + offset as i128) as u64
}

/// A simulated path with the peer's clock offset, its drift and the delay in each direction.
struct Path {
    offset: i64,
    drift_ppm: f64,
    /// Fixed delay in ns
    base: u64,
    /// Random extra delay in ns, below this
    jitter: u64,
}

impl Path {
    /// One exchange that starts at local time `local`. `spike` adds a large delay to the way
    /// there. It returns the local time when the answer arrives.
    fn exchange(&self, sync: &mut ClockSync, rng: &mut Rng, local: u64, spike: u64) -> u64 {
        let up = self.base + rng.below(self.jitter as usize) as u64 + spike;
        let down = self.base + rng.below(self.jitter as usize) as u64;
        let t0 = local;
        let t1 = peer_time(t0 + up, self.offset, self.drift_ppm);
        let t2 = t1 + 50_000; // the peer answers 50 microseconds later
        let t3 = t0 + up + 50_000 + down;
        sync.add(t0, t1, t2, t3);
        t3
    }
}

#[test]
fn the_offset_comes_out_within_a_fraction_of_the_jitter() {
    let mut rng = Rng(123);
    for offset in [7_123_456_789i64, -3_000_000_000, 0, 123] {
        let mut sync = ClockSync::new();
        assert!(sync.estimate().is_none());
        let mut t = 10_000_000_000u64;
        for _ in 0..40 {
            Path { offset, drift_ppm: 0.0, base: 3_000_000, jitter: 4_000_000 }.exchange(&mut sync, &mut rng, t, 0);
            t += 200_000_000;
        }
        let e = sync.estimate().unwrap();
        let error = (e.offset_ns - offset).abs();
        assert!(error < 600_000, "offset {offset}: error {error} ns");
        // The best exchanges have close to the base delay each way
        assert!(e.rtt_ns > 6_000_000 && e.rtt_ns < 9_000_000, "rtt {}", e.rtt_ns);
    }
}

#[test]
fn conversions_agree_with_the_offset() {
    let mut rng = Rng(5);
    let offset = -2_500_000_000i64;
    let mut sync = ClockSync::new();
    let mut t = 20_000_000_000u64;
    for _ in 0..40 {
        Path { offset, drift_ppm: 0.0, base: 1_000_000, jitter: 500_000 }.exchange(&mut sync, &mut rng, t, 0);
        t += 100_000_000;
    }
    let local = 30_000_000_000u64;
    let peer = sync.to_peer(local).unwrap();
    assert!((peer as i64 - (local as i64 + offset)).abs() < 300_000);
    let back = sync.to_local(peer).unwrap();
    assert!((back as i64 - local as i64).abs() < 2);
}

#[test]
fn a_few_slow_exchanges_do_not_move_the_estimate() {
    let mut rng = Rng(9);
    let offset = 1_000_000_000i64;
    let mut sync = ClockSync::new();
    let mut t = 5_000_000_000u64;
    for i in 0..60 {
        // Every fifth exchange crosses a 200 ms queue in one direction
        let spike = if i % 5 == 4 { 200_000_000 } else { 0 };
        Path { offset, drift_ppm: 0.0, base: 2_000_000, jitter: 2_000_000 }.exchange(&mut sync, &mut rng, t, spike);
        t += 100_000_000;
    }
    let error = (sync.estimate().unwrap().offset_ns - offset).abs();
    assert!(error < 600_000, "error {error} ns");
}

#[test]
fn a_drifting_clock_is_followed() {
    let mut rng = Rng(77);
    let (offset, drift) = (4_000_000_000i64, 50.0);
    let mut sync = ClockSync::new();
    let mut t = 1_000_000_000u64;
    // 60 seconds at 10 exchanges per second: the clocks part by 3 ms
    for _ in 0..600 {
        Path { offset, drift_ppm: drift, base: 2_000_000, jitter: 2_000_000 }.exchange(&mut sync, &mut rng, t, 0);
        t += 100_000_000;
    }
    let truth = peer_time(t, offset, drift) as i64 - t as i64;
    let error = (sync.estimate().unwrap().offset_ns - truth).abs();
    assert!(error < 800_000, "error {error} ns");
}

#[test]
fn impossible_exchanges_are_refused() {
    let mut sync = ClockSync::new();
    // The answer left before the ping arrived
    assert!(sync.add(100, 500, 400, 600).is_none());
    // The answer arrived before the ping left
    assert!(sync.add(100, 150, 160, 90).is_none());
    // The peer held the ping longer than the whole round trip
    assert!(sync.add(100, 150, 900, 300).is_none());
    assert!(sync.estimate().is_none());
    assert!(sync.add(100, 150, 160, 220).is_some());
}

#[test]
fn the_local_clock_counts_up_from_one_second() {
    let a = now_ns();
    let b = now_ns();
    assert!(a >= 1_000_000_000 && b >= a);
}
