use super::{start_for, Change, Control, Second, FLOOR_BPS};

const MBPS: u64 = 1_000_000;
const FPS: f32 = 120.0;

/// Missed frames in the calm seconds of the measurements of #97: the host's scene at 200 Mbps over the
/// cable and at 40 Mbps over Wi-Fi, at the display lock's rest
const CALM_MISSES: [u32; 20] = [7, 0, 6, 3, 2, 3, 3, 3, 5, 3, 7, 2, 3, 5, 3, 0, 3, 4, 4, 1];

/// A link that carries `capacity` bits a second. Under 90 percent of it a second misses frames as a
/// calm link did in the measurements. Near the capacity a few more frames miss. Over it the frames
/// queue and most of them miss.
struct Link {
    capacity: u64,
    seed: u64,
}

impl Link {
    fn new(capacity: u64) -> Self {
        Self { capacity, seed: 7 }
    }

    fn random(&mut self) -> usize {
        // A fixed sequence, so each run of the test sees the same seconds
        self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.seed >> 33) as usize
    }

    fn second(&mut self, target: u64) -> Second {
        let frames = 120;
        let calm = CALM_MISSES[self.random() % CALM_MISSES.len()];
        let load = target as f64 / self.capacity as f64;
        let missed = if load > 1.0 {
            30 + ((load - 1.0) * 200.0) as u32
        } else if load > 0.9 {
            calm + 5
        } else {
            calm
        };
        second(frames, missed.min(frames))
    }
}

/// Runs the controller on the link for `seconds` and returns the target of each second and the changes.
fn run(control: &mut Control, link: &mut Link, seconds: usize) -> (Vec<u64>, Vec<Change>) {
    let (mut targets, mut changes) = (Vec::new(), Vec::new());
    for _ in 0..seconds {
        let second = link.second(control.target());
        changes.extend(control.tick(Some(second)));
        targets.push(control.target());
    }
    (targets, changes)
}

/// A second in which the host sent `frames` and the headset reported them all, `missed` of them late
fn second(frames: u32, missed: u32) -> Second {
    Second { frames, missed, sent: frames, network_ms: 2.0 }
}

fn calm() -> Option<Second> {
    Some(second(120, 0))
}

#[test]
fn a_rise_of_late_frames_lowers_the_target() {
    let mut control = Control::new(200 * MBPS, 200 * MBPS, FPS);
    for _ in 0..10 {
        assert_eq!(control.tick(calm()), None);
    }
    // A burst in one second cuts at once
    let cut = control.tick(Some(second(118, 20)));
    assert!(matches!(cut, Some(Change::Cut { from, to, .. }) if from == 200 * MBPS && to <= 160 * MBPS), "{cut:?}");
    let after_burst = control.target();
    // More misses than a calm link has, over several seconds, cut too
    for _ in 0..3 {
        control.tick(calm());
    }
    let mut cut = None;
    for _ in 0..5 {
        cut = cut.or(control.tick(Some(second(120, 9))));
    }
    assert!(matches!(cut, Some(Change::Cut { from, .. }) if from == after_burst), "{cut:?}");
    assert!(control.target() < after_burst);
}

#[test]
fn one_burst_cuts_once() {
    let mut control = Control::new(200 * MBPS, 200 * MBPS, FPS);
    for _ in 0..5 {
        control.tick(calm());
    }
    // The second after the cut still holds the reports of frames that the old bitrate made late
    let burst = Some(second(120, 30));
    let seconds = [burst, burst, calm(), calm()];
    let cuts = seconds.iter().filter_map(|s| control.tick(*s)).count();
    assert_eq!(cuts, 1);
    // A link that still misses most frames after that second cuts again
    let mut control = Control::new(200 * MBPS, 200 * MBPS, FPS);
    for _ in 0..5 {
        control.tick(calm());
    }
    let cuts: Vec<_> = (0..3).filter_map(|_| control.tick(burst)).collect();
    assert_eq!(cuts.len(), 2, "{cuts:?}");
}

#[test]
fn a_calm_link_raises_the_target_to_the_ceiling() {
    let mut control = Control::new(200 * MBPS, FLOOR_BPS, FPS);
    let mut link = Link::new(1000 * MBPS);
    let (targets, changes) = run(&mut control, &mut link, 300);
    assert!(changes.iter().all(|c| matches!(c, Change::Raise { .. })), "{changes:?}");
    let reached = targets.iter().position(|t| *t == 200 * MBPS).expect("the target reaches the ceiling");
    assert!(reached < 180, "the ceiling came after {reached} s");
    assert!(targets[reached..].iter().all(|t| *t == 200 * MBPS), "the target stays at the ceiling");
}

#[test]
fn a_steady_link_does_not_swing() {
    let capacity = 80 * MBPS;
    let mut control = Control::new(200 * MBPS, 200 * MBPS, FPS);
    let mut link = Link::new(capacity);
    let (targets, changes) = run(&mut control, &mut link, 600);
    // The first cuts find the link within the first minute
    let settled = &targets[60..];
    let cuts = changes.iter().filter(|c| matches!(c, Change::Cut { .. })).count();
    let over = settled.iter().filter(|t| **t > capacity).count();
    let under = settled.iter().filter(|t| (**t as f64) < 0.6 * capacity as f64).count();
    assert!(over <= 10, "the target lay over the link for {over} of 540 s: {targets:?}");
    assert!(under <= 30, "the target lay under 60% of the link for {under} of 540 s: {targets:?}");
    assert!(cuts <= 10, "{cuts} cuts in 10 minutes: {changes:?}");
}

#[test]
fn the_floor_holds() {
    let mut control = Control::new(200 * MBPS, 200 * MBPS, FPS);
    // A link that misses many frames at every bitrate
    for _ in 0..120 {
        control.tick(Some(second(120, 20)));
        assert!(control.target() >= FLOOR_BPS);
    }
    assert_eq!(control.target(), FLOOR_BPS);
    // A ceiling under the floor is the target
    let low = Control::new(5 * MBPS, 200 * MBPS, FPS);
    assert_eq!(low.target(), 5 * MBPS);
}

#[test]
fn seconds_without_reports_change_nothing() {
    let mut control = Control::new(200 * MBPS, 50 * MBPS, FPS);
    // No stream shown (Room at Passthrough), or a headset without focus: no reports, or a few
    for _ in 0..120 {
        assert_eq!(control.tick(None), None);
        assert_eq!(control.tick(Some(second(10, 10))), None);
    }
    assert_eq!(control.target(), 50 * MBPS);
}

#[test]
fn frames_that_the_headset_does_not_report_count_as_missed() {
    let mut control = Control::new(200 * MBPS, 200 * MBPS, FPS);
    for _ in 0..5 {
        control.tick(calm());
    }
    // A link that delays the frames more and more: the headset reports 60 of the 120 frames sent,
    // all of them on time
    let cut = control.tick(Some(Second { frames: 60, missed: 0, sent: 120, network_ms: 9.0 }));
    assert!(matches!(cut, Some(Change::Cut { from, .. }) if from == 200 * MBPS), "{cut:?}");
    // The frames of the last 40 ms of a calm second are still on their way: no cut
    let mut control = Control::new(200 * MBPS, 200 * MBPS, FPS);
    for _ in 0..20 {
        assert_eq!(control.tick(Some(Second { frames: 115, missed: 1, sent: 120, network_ms: 4.5 })), None);
    }
}

#[test]
fn a_network_time_of_a_queue_cuts() {
    let mut control = Control::new(200 * MBPS, 200 * MBPS, FPS);
    for _ in 0..5 {
        control.tick(calm());
    }
    // Frames of 175 KB took 4.5 ms over the cable and 6.5 ms over Wi-Fi, under a period. Behind a
    // full queue they took 16 to 28 ms.
    assert_eq!(control.tick(Some(Second { network_ms: 6.5, ..second(120, 0) })), None);
    let cut = control.tick(Some(Second { network_ms: 16.0, ..second(120, 0) }));
    assert!(matches!(cut, Some(Change::Cut { from, .. }) if from == 200 * MBPS), "{cut:?}");
}

#[test]
fn a_session_over_a_network_starts_lower_than_over_the_cable() {
    assert_eq!(start_for(200 * MBPS, true, 120.0), 200 * MBPS);
    assert_eq!(start_for(200 * MBPS, false, 120.0), 100 * MBPS);
    // The same bits a frame at 72 Hz
    assert_eq!(start_for(200 * MBPS, false, 72.0), 60 * MBPS);
    // A setting under the start stays
    assert_eq!(start_for(50 * MBPS, false, 120.0), 50 * MBPS);
}
