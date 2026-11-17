use super::*;

const MS: f64 = 1e6;
const PERIOD: Nanos = 8_333_333;

fn lock_with_grid() -> DisplayLock {
    let mut lock = DisplayLock::new(PERIOD);
    lock.on_tracking(1_000_000_000);
    lock
}

#[test]
fn percentile_and_grid_work() {
    let window: VecDeque<f64> = (1..=10).map(f64::from).collect();
    assert_eq!(percentile(&window, 0.9), Some(9.0));
    assert_eq!(percentile(&window, 1.0), Some(10.0));
    assert_eq!(percentile(&VecDeque::new(), 0.9), None);
    assert_eq!(grid_at_or_after(100, 10.0, 100), 100);
    assert_eq!(grid_at_or_after(100, 10.0, 101), 110);
    assert_eq!(grid_at_or_after(100, 10.0, 130), 130);
    // The grid goes back from its origin: the origin is the newest display time and can lie ahead
    assert_eq!(grid_at_or_after(100, 10.0, 90), 90);
    assert_eq!(grid_at_or_after(100, 10.0, 81), 90);
    assert_eq!(grid_at_or_after(100, 10.0, 3), 10);
}

#[test]
fn the_planner_waits_for_the_grid() {
    let mut lock = DisplayLock::new(PERIOD);
    assert!(!lock.ready() && lock.plan(5).is_none());
    // A sample without a display time says nothing about the grid
    lock.on_tracking(0);
    assert!(!lock.ready());
    assert!(lock_with_grid().ready());
}

#[test]
fn the_start_is_the_target_minus_the_lead_and_the_need() {
    let lock = lock_with_grid();
    let now = 1_100_000_000;
    let plan = lock.plan(now).unwrap();
    // The first target is the first grid point that a frame started now can make
    let (need, lead) = (lock.need() as u64, lock.lead() as u64);
    // The first target is feasible for the median need, which is the need without the margin
    let median = (FIRST_NEED_MS * MS) as u64;
    assert!(plan.target >= now + median + lead && plan.target < now + median + lead + PERIOD);
    assert!((plan.target - 1_000_000_000) % PERIOD < 2, "the target lies on the grid");
    assert_eq!(plan.start, plan.target - need - lead);
}

#[test]
fn successive_frames_take_successive_points() {
    let mut lock = lock_with_grid();
    let first = lock.plan(1_100_000_000).unwrap();
    lock.commit(first.target, 0);
    // The next frame, planned right after the first, takes the next point and waits for it
    let second = lock.plan(first.start + 5 * MS as u64).unwrap();
    assert!((second.target - first.target).abs_diff(PERIOD) < 3);
    assert!(second.start > first.start);
    // A planner that is late for the next point takes a later one
    let late = lock.plan(first.target + 50 * MS as u64).unwrap();
    assert!(late.target > second.target);
}

#[test]
fn the_period_follows_the_display_times_of_the_samples() {
    let mut lock = DisplayLock::new(PERIOD);
    let mut t = 5_000_000_000u64;
    for _ in 0..600 {
        lock.on_tracking(t);
        t += 8_300_000;
    }
    assert!(lock.period().abs_diff(8_300_000) < 20_000, "{}", lock.period());
    // A sample that is not newer changes nothing
    lock.on_tracking(t - 100_000_000);
    assert_eq!(lock.origin, Some(t - 8_300_000));
}

fn report(sample: Nanos, target: Nanos, display: Nanos) -> Report {
    Report { sample, decode_end: target - 25 * MS as u64, picked: display - 17 * MS as u64, display_time: display }
}

#[test]
fn a_miss_raises_the_margin_and_hits_lower_it_slowly() {
    let mut lock = lock_with_grid();
    let before = lock.margin_ms;
    lock.sent(1, 2_000_000_000, 1_900_000_000);
    // Shown one point after its target
    lock.on_report(&report(1, 2_000_000_000, 2_000_000_000 + PERIOD));
    assert!(lock.margin_ms > before);
    let raised = lock.margin_ms;
    for sample in 2..40 {
        lock.sent(sample, 2_000_000_000, 1_900_000_000);
        lock.on_report(&report(sample, 2_000_000_000, 2_000_000_000));
    }
    assert!(lock.margin_ms < raised && lock.margin_ms >= MIN_MARGIN_MS);
    let stats = lock.take_stats();
    assert_eq!((stats.late, stats.on_time, stats.replaced, stats.early), (1, 38, 0, 0));
}

#[test]
fn a_frame_without_a_report_counts_as_a_miss() {
    let mut lock = lock_with_grid();
    let before = lock.margin_ms;
    lock.sent(1, 2_000_000_000, 1_900_000_000);
    lock.sent(2, 2_000_000_000 + PERIOD, 1_908_000_000);
    // Frame 1 never gets a report. Frame 2 is shown.
    lock.on_report(&report(2, 2_000_000_000 + PERIOD, 2_000_000_000 + PERIOD));
    let stats = lock.take_stats();
    assert_eq!((stats.replaced, stats.on_time), (1, 1));
    assert!(lock.margin_ms > before - HIT_STEP_MS, "the miss counts");
    // A report for a frame that the planner does not know changes nothing
    lock.on_report(&report(99, 2_000_000_000, 2_000_000_000));
    assert_eq!(lock.take_stats().frames(), 0);
}

#[test]
fn a_frame_shown_a_point_early_lowers_the_margin() {
    let mut lock = lock_with_grid();
    let before = lock.margin_ms;
    lock.sent(1, 2_000_000_000, 1_900_000_000);
    lock.on_report(&report(1, 2_000_000_000, 2_000_000_000 - PERIOD));
    assert!(lock.margin_ms < before);
    assert_eq!(lock.take_stats().early, 1);
}

/// A small generator, so that the simulation repeats.
struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A number in `-spread..spread`
    fn jitter(&mut self, spread: f64) -> f64 {
        (self.unit() * 2.0 - 1.0) * spread
    }
}

struct Outcome {
    /// Slots that showed a frame newer than the one before
    new_frames: usize,
    /// Slots in all
    slots: usize,
    /// Frames that showed at their own target
    on_target: usize,
    /// Frames that the headset replaced
    replaced: usize,
}

/// A headset with a display grid and a pick `lead` before each point. A host whose frames take
/// `pipeline` ms from the start to the end of the decode with `jitter` ms of spread. The headset
/// takes the newest frame that is ready at the pick. With `locked` the host follows the planner.
/// Otherwise it starts a frame as each tracking sample arrives, as the host did before.
fn simulate(locked: bool, pipeline_ms: f64, jitter_ms: f64, seed: u64) -> Outcome {
    let (lead, ahead) = (17.0 * MS, 18.0 * MS);
    let d0 = 10_000_000_000u64;
    let slot_time = |j: usize| d0 + (j as f64 * PERIOD as f64) as u64;
    let pick = |j: usize, rng: &mut Rng| slot_time(j) - (lead + rng.jitter(0.4 * MS)) as u64;
    let slots = 2400;
    let mut rng = Rng(seed);
    let mut lock = DisplayLock::new(PERIOD);
    // A tracking sample at each slot, `ahead` before its display time
    let mut next_sample = 0usize;
    let mut next_slot = 0usize;
    let mut frames: Vec<(Nanos, Nanos, Nanos)> = Vec::new(); // arrival, target, id
    let mut consumed = 0usize;
    let mut last_shown: Option<Nanos> = None;
    let mut out = Outcome { new_frames: 0, slots: 0, on_target: 0, replaced: 0 };
    let mut host_free = 0u64;
    let mut id = 0u64;
    let mut picks: Vec<u64> = (0..slots).map(|j| pick(j, &mut rng)).collect();
    picks.push(u64::MAX);
    while next_slot < slots {
        // The host's next frame: its start time
        let sample_at = |k: usize| slot_time(k) - ahead as u64;
        let (start, target) = if locked {
            let now = host_free.max(sample_at(next_sample.max(1) - 1));
            while next_sample < slots && sample_at(next_sample) <= now {
                lock.on_tracking(slot_time(next_sample));
                next_sample += 1;
            }
            match lock.plan(now) {
                Some(plan) => (plan.start.max(now), plan.target),
                None => {
                    host_free = sample_at(next_sample) + 1;
                    continue;
                }
            }
        } else {
            let k = next_sample;
            next_sample += 1;
            let start = host_free.max(sample_at(k) + MS as u64);
            (start, slot_time(k + 2))
        };
        // The headset's side up to this start: picks and the reports that came back
        id += 1;
        let arrival = start + ((pipeline_ms + rng.jitter(jitter_ms)) * MS) as u64;
        if locked {
            lock.commit(target, 0);
            lock.sent(id, target, start.min(target));
        }
        frames.push((arrival, target, id));
        host_free = start + (5.0 * MS) as u64;
        // Settle the slots whose pick lies before the next start by taking the frames that arrived
        while next_slot < slots && picks[next_slot] + (2.0 * MS) as u64 <= host_free {
            let u = picks[next_slot];
            let ready: Vec<usize> = (consumed..frames.len()).filter(|&i| frames[i].0 <= u).collect();
            let shown = ready.last().copied();
            if let Some(i) = shown {
                for &r in &ready {
                    if r != i {
                        out.replaced += 1;
                    }
                }
                consumed = consumed.max(i + 1);
                if next_slot >= 200 {
                    if last_shown != Some(frames[i].2) {
                        out.new_frames += 1;
                    }
                    if frames[i].1.abs_diff(slot_time(next_slot)) < PERIOD / 2 {
                        out.on_target += 1;
                    }
                }
                last_shown = Some(frames[i].2);
                if locked {
                    lock.on_report(&Report { sample: frames[i].2, decode_end: frames[i].0, picked: u, display_time: slot_time(next_slot) });
                }
            }
            if next_slot >= 200 {
                out.slots += 1;
            }
            next_slot += 1;
        }
    }
    out
}

#[test]
fn the_lock_shows_a_new_frame_at_nearly_every_point() {
    for seed in [1, 2, 3] {
        let locked = simulate(true, 13.0, 2.0, seed);
        let share = |n: usize| n as f64 / locked.slots as f64;
        assert!(share(locked.new_frames) > 0.95, "seed {seed}: {} of {} new", locked.new_frames, locked.slots);
        assert!(share(locked.on_target) > 0.85, "seed {seed}: {} of {} on target", locked.on_target, locked.slots);
    }
}

#[test]
fn without_the_lock_a_frame_at_the_edge_of_the_pick_is_replaced_or_repeated() {
    // The frame arrives about when the pick of its second point falls: this is the case of the
    // Quest, where 78 to 88 of 120 frames showed
    let unlocked = simulate(false, 16.9, 1.5, 1);
    let share = unlocked.new_frames as f64 / unlocked.slots as f64;
    assert!(share < 0.9, "{share}");
    assert!(unlocked.replaced > 100);
    let locked = simulate(true, 16.9, 1.5, 1);
    assert!(locked.new_frames as f64 / locked.slots as f64 > 0.95);
}

#[test]
fn the_lock_copes_with_a_jittery_pipeline() {
    // Spread of 4 ms around 14 ms: the margin has to rise
    let locked = simulate(true, 14.0, 4.0, 7);
    assert!(locked.new_frames as f64 / locked.slots as f64 > 0.9, "{} of {}", locked.new_frames, locked.slots);
}
