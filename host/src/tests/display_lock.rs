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

#[test]
fn a_frame_without_a_plan_teaches_the_lead_and_nothing_else() {
    let mut lock = lock_with_grid();
    // Before any report the lead is two periods
    assert_eq!(lock.lead() as Nanos, 2 * PERIOD);
    let (need, margin) = (lock.need(), lock.margin_ms);
    for sample in 1..20 {
        lock.sent_unplanned(sample, 2_000_000_000);
        // Shown one point late, with a lead of 17 ms
        lock.on_report(&report(sample, 2_000_000_000, 2_000_000_000 + PERIOD));
    }
    assert_eq!(lock.lead() as Nanos, 17 * MS as u64);
    assert_eq!((lock.need(), lock.margin_ms), (need, margin), "a game's frames leave the planner's need and margin alone");
    assert_eq!(lock.take_stats().late, 19);
}

#[test]
fn frames_without_a_plan_take_successive_points_and_the_first_point_after_the_lead() {
    let mut lock = DisplayLock::new(PERIOD);
    assert_eq!(lock.point_for(1_100_000_000), None, "no grid yet");
    lock.on_tracking(1_000_000_000);
    let lead = lock.lead() as Nanos;
    // A frame that is ready at 1.1 s takes the first point at or after 1.1 s plus the lead
    let first = lock.point_for(1_100_000_000).unwrap();
    assert!(first >= 1_100_000_000 + lead && first < 1_100_000_000 + lead + PERIOD, "{first}");
    assert!((first - 1_000_000_000) % PERIOD < 2, "the target lies on the grid");
    // A second frame that is ready a millisecond later would take the same point: it takes the next
    let second = lock.point_for(1_101_000_000).unwrap();
    assert!(second.abs_diff(first + PERIOD) < 2, "{first} {second}");
    // A third one would take the point after, but no frame goes more than one point past the first
    // point that it could make: the backlog stays short, and the queue replaces the older frame
    let third = lock.point_for(1_101_500_000).unwrap();
    assert!(third.abs_diff(second) < 2, "{second} {third}");
    // A frame that is ready much later takes its own first point
    let late = lock.point_for(1_300_000_000).unwrap();
    assert!(late >= 1_300_000_000 + lead && late < 1_300_000_000 + lead + PERIOD);
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
#[test]
fn a_frame_loop_that_starts_late_takes_each_point_once() {
    // A loaded Mac, as measured on 18 November 2026: the needs spread from 19 to 35 ms and the
    // misses raised the margin to a period, so the need with the margin lies 14 ms above the
    // median need. Each plan's start then lies in the past, and the frame loop starts the frame
    // as soon as the encoder takes one, every 5 ms.
    let mut lock = lock_with_grid();
    lock.need = (0..WINDOW).map(|i| (19.0 + 16.0 * i as f64 / (WINDOW - 1) as f64) * MS).collect();
    lock.margin_ms = PERIOD as f64 / MS;
    let begin = 1_100_000_000;
    let mut now = begin;
    let mut targets = Vec::new();
    while now < begin + 1_000_000_000 {
        let plan = lock.plan(now).unwrap();
        lock.commit(plan.target, 0);
        targets.push(plan.target);
        now = plan.start.max(now) + 5 * MS as u64;
    }
    assert!(targets.windows(2).all(|pair| pair[1] > pair[0]), "a point was taken twice");
    // One second holds 120 points; the first frames may reach a few points ahead
    assert!(targets.len() <= 125, "{} frames in a second", targets.len());
    assert!(targets.len() >= 115, "{} frames in a second", targets.len());
}

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

#[test]
fn a_pose_that_comes_within_a_period_before_the_start_starts_the_frame() {
    let lock = lock_with_grid();
    let now = 5_000_000_000;
    let ms = MS as u64;
    // The next pose is due a period after this one. A start that lies sooner than that, less the
    // guard, comes before the next pose.
    assert!(lock.starts_early(now, now + ms));
    assert!(lock.starts_early(now, now + 7 * ms));
    // A start further away leaves time for the next pose, which the frame renders for
    assert!(!lock.starts_early(now, now + 8 * ms));
    assert!(!lock.starts_early(now, now + PERIOD));
    // A start that has passed is not early
    assert!(!lock.starts_early(now, now));
    assert!(!lock.starts_early(now, now - ms));
}

/// The poses reach the host one per period, 14 ms before their display times, each late or early by
/// up to 0.3 ms. The frame loop starts a planned frame at the arrival of the first pose for which
/// `starts_early` holds, or at the planned start. The pose that it renders for is the newest one at
/// the start. It must never be an older one than the planned start would take.
#[test]
fn starting_at_the_arrival_renders_for_the_pose_that_the_planned_start_would_take() {
    let mut rng = Rng(7);
    let display = |k: u64| 10_000_000_000 + k * PERIOD;
    let arrivals: Vec<Nanos> = (0..2000).map(|k| (display(k) as f64 - 14.0 * MS + rng.jitter(0.3 * MS)) as Nanos).collect();
    let lock = lock_with_grid();
    let (mut early_starts, mut gained) = (0, 0.0);
    for _ in 0..1000 {
        // A planned start at any point of the period
        let start = arrivals[500] + (rng.unit() * PERIOD as f64) as u64;
        let by_timer = arrivals.iter().rposition(|&a| a <= start).unwrap();
        let early = arrivals.iter().position(|&a| a < start && lock.starts_early(a, start));
        if let Some(early) = early {
            assert_eq!(early, by_timer, "the frame renders for pose {early} and not for {by_timer}");
            early_starts += 1;
            gained += (start - arrivals[early]) as f64 / MS;
        }
    }
    // Most plans start early: those that lie within the guard of the next pose do not
    assert!(early_starts > 900, "{early_starts}");
    // The frames gain half a period on average
    let mean = gained / early_starts as f64;
    assert!(mean > 3.5 && mean < 4.5, "{mean}");
}

#[test]
fn a_frame_does_not_start_early_when_it_would_wait_in_the_headsets_queue_beyond_the_cap() {
    let mut lock = lock_with_grid();
    let now = 5_000_000_000;
    // A loaded Mac: one frame in ten is quick, the others need 30 ms. The planned need is at its cap.
    lock.need = (0..WINDOW).map(|i| if i % 10 == 0 { 11.0 * MS } else { 30.0 * MS }).collect();
    let cap = 11.0 * MS + NEED_CAP_PERIODS * PERIOD as f64;
    assert!((lock.need() - cap).abs() < MS, "{} {}", lock.need(), cap);
    assert!(!lock.starts_early(now, now + MS as u64), "a start a millisecond ahead leaves the cap");
    assert!(lock.starts_early(now, now + 100_000));
    // With a rested need the whole period fits under the cap
    lock.need = (0..WINDOW).map(|_| 13.0 * MS).collect();
    assert!(lock.starts_early(now, now + 7 * MS as u64));
}
