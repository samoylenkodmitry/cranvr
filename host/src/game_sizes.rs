//! A render size per game. A game that stays below the headset's rate gets a smaller share of
//! the view size at its next start: the runtime reads `game-sizes.txt` next to the control file,
//! one line "<share> <program>" per game, once per game start (cranvr's runtime patch 0002).
//! A game reads its view size only when it starts, so a new share cannot apply during a session.
//!
//! The host judges a game by its whole run, at the run's end (issue #167). The host collects the
//! game's frame rate in each stats window from 20 s after the game connects until it disconnects. A
//! run of at least MIN_WINDOWS windows counts as slow when its median window is below 95% of the
//! headset's rate. A game holds its loading scene for a fixed time at the start of each run, and
//! the user plays for the rest. So the median of a long run is the rate of play. Serious Sam VR
//! holds its loading scene at 99 frames a second by its own cap for 60 to 90 s, with dips to 47 in
//! that time while it loads (#155). The old learner judged the first minute after loading and gave
//! the game a smaller size for that scene.
//!
//! The issue named two other ways. A start of flat windows is no sign of a cap: the loading scene
//! above held 99 in some windows and 47 to 93 in others, and a slow game in a still menu holds a
//! flat rate too. A start that ends with a change of the rate does not tell a cap that ends from a
//! slow game whose play begins. The end of the run waits for nothing that the learner needs: the share
//! applies only at the next start anyway. A run that ends with the host, such as at a restart for a
//! new setting, decides nothing, and the game keeps its share.
//!
//! The rate cap hides how fast a game could run, so the host never raises a share above the one
//! before its last step. The share returns to the full size when the user forgets the learned sizes
//! in the app.
//!
//! `game-rates.txt` keeps the frame rate that each game made before its last step, one line
//! "<share> <fps> <program>". When a smaller share did not speed a game up, its size is not
//! what limits it: the game gets the share from before that step back and keeps it. A game that is
//! slow for a reason other than its size, such as its CPU, would otherwise lose pixels for nothing
//! (issue #155). The rates line then names the share that the game has again, which tells the next
//! start that the size does not limit the game.

use log::warn;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// The smallest share the host sets
pub const FLOOR: f32 = 0.4;
/// A run needs this many stats windows (5 s each) for a decision: 5 minutes. A loading scene of up to
/// half the run, 2.5 minutes, then leaves the median to play.
pub const MIN_WINDOWS: usize = 60;
/// A window below this share of the headset's rate counts as slow
const SLOW: f64 = 0.95;

/// The frame rates of one game run, one for each stats window after the game's loading
#[derive(Default)]
pub struct Run {
    /// The headset's rate in the windows
    rate: f64,
    windows: Vec<f64>,
}

impl Run {
    /// Adds a stats window in which the game made `fps` while the headset showed `rate`. A new
    /// rate starts the run again: a window counts against the rate that it had.
    pub fn push(&mut self, fps: f64, rate: f64) {
        if rate != self.rate {
            self.windows.clear();
            self.rate = rate;
        }
        self.windows.push(fps);
    }
}

/// What the host decided at the end of a run
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Verdict {
    /// The run had too few windows for a decision
    Short { windows: usize },
    /// The game keeps its share: its median window `fps` held the rate, or a step would not help
    Keeps { fps: f64, windows: usize },
    /// The game's share changes for its next start
    Step(Step),
}

pub struct Sizes {
    sizes: PathBuf,
    rates: PathBuf,
}

impl Sizes {
    pub fn new(dir: &Path) -> Self {
        Self { sizes: dir.join("game-sizes.txt"), rates: dir.join("game-rates.txt") }
    }

    /// The game's share of the view size: 1 without an entry.
    pub fn share(&self, program: &str) -> f32 {
        lines(&self.sizes)
            .into_iter()
            .filter_map(|line| {
                let (share, rest) = line.split_once(' ')?;
                rest.eq_ignore_ascii_case(program).then(|| share.parse::<f32>().ok()).flatten()
            })
            .next_back()
            .unwrap_or(1.0)
    }

    /// The share and frame rate before the game's last step.
    fn before(&self, program: &str) -> Option<(f32, f64)> {
        lines(&self.rates)
            .into_iter()
            .filter_map(|line| {
                let mut fields = line.splitn(3, ' ');
                let (share, fps, rest) = (fields.next()?, fields.next()?, fields.next()?);
                rest.eq_ignore_ascii_case(program).then(|| Some((share.parse().ok()?, fps.parse().ok()?))).flatten()
            })
            .next_back()
    }

    /// Decides at the end of a run of `program` whether its next start gets another share.
    pub fn learn(&self, program: &str, run: &Run) -> Verdict {
        let windows = run.windows.len();
        if windows < MIN_WINDOWS {
            return Verdict::Short { windows };
        }
        let mut sorted = run.windows.clone();
        sorted.sort_by(f64::total_cmp);
        let fps = sorted[windows / 2];
        let share = self.share(program);
        let Some(step) = next_share(share, fps, run.rate, self.before(program)) else {
            return Verdict::Keeps { fps, windows };
        };
        match step {
            Step::Smaller { old, new } => {
                replace(&self.sizes, program, &format!("{new:.3} {program}"));
                replace(&self.rates, program, &format!("{old:.3} {fps:.1} {program}"));
            }
            Step::Back { new, .. } => {
                // A line stays also at the full size: the app lists it, and its Forget button clears it
                // together with the rates line, which holds the game at this share
                replace(&self.sizes, program, &format!("{new:.3} {program}"));
                // The rates line keeps the share from before the step and its frame rate. Its share is
                // the game's share again, so the next start takes no step.
            }
        }
        Verdict::Step(step)
    }
}

/// A change of a game's share for its next start
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    /// The game stayed below the headset's rate: a smaller share
    Smaller { old: f32, new: f32 },
    /// A smaller share did not raise the game's frame rate: the share from before that step. The game
    /// keeps it, because its size does not limit it.
    Back { old: f32, new: f32, fps: f64 },
}

/// The next share for a game that made `fps` at `share` while the headset shows `rate`, or none.
/// `before` is the share and frame rate before the last step.
fn next_share(share: f32, fps: f64, rate: f64, before: Option<(f32, f64)>) -> Option<Step> {
    if fps >= SLOW * rate {
        return None;
    }
    if let Some((old_share, old_fps)) = before {
        // The game had its size back after a step that did not help: its size does not limit it
        if (old_share - share).abs() <= 0.001 {
            return None;
        }
        // A smaller size that did not speed the game up: the size is not what limits it, so the game
        // gets its size back
        if old_share > share + 0.001 && fps < old_fps * 1.05 {
            return Some(Step::Back { old: share, new: old_share, fps });
        }
    }
    if share <= FLOOR + 0.001 {
        return None;
    }
    // The GPU time follows the pixel count, the square of the share. Aim 10% under the rate.
    let step = (fps / rate * 0.9).sqrt().clamp(0.7, 0.95) as f32;
    Some(Step::Smaller { old: share, new: (share * step).max(FLOOR) })
}

/// The lines of a file, without the line ends; none when it is missing.
fn lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path).map(|text| text.lines().map(str::to_string).collect()).unwrap_or_default()
}

/// Writes `line` in place of the program's line, or at the end.
fn replace(path: &Path, program: &str, line: &str) {
    let mut kept: Vec<String> =
        lines(path).into_iter().filter(|l| !l.split_once(' ').is_some_and(|(_, rest)| ends_with_program(rest, program))).collect();
    kept.push(line.to_string());
    if let Err(e) = fs::write(path, kept.join("\n") + "\n") {
        warn!("Game sizes: cannot write {}: {e}", path.display());
    }
}

/// Whether the rest of a line, after its first field, names the program: directly in
/// game-sizes.txt, after the frame rate in game-rates.txt.
fn ends_with_program(rest: &str, program: &str) -> bool {
    rest.eq_ignore_ascii_case(program) || rest.split_once(' ').is_some_and(|(_, p)| p.eq_ignore_ascii_case(program))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAME: &str = r"C:\Program Files (x86)\Steam\steamapps\common\Some Game\Game.exe";

    /// Serious Sam VR's capped loading scene in a test host at 120 Hz: its windows from 20 s after it
    /// connected until its menu (#155, Direct3D 11, eyes of 1444x1546, load 8 to 26). The game holds
    /// 99 frames a second by its own cap and dips while it loads.
    const LOADING: [f64; 17] =
        [95.8, 98.1, 99.0, 99.0, 95.6, 93.0, 76.2, 47.2, 98.4, 99.0, 62.2, 97.9, 98.9, 97.2, 97.8, 91.3, 96.4];
    /// The same game's menu and first level after the loading scene, in the same runs
    const PLAY: [f64; 12] = [119.9, 119.4, 119.8, 115.3, 118.8, 118.0, 119.4, 119.1, 118.2, 118.6, 120.0, 119.2];

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cranvr-sizes-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A run of `windows` at 120 Hz
    fn run(windows: impl IntoIterator<Item = f64>) -> Run {
        let mut run = Run::default();
        for fps in windows {
            run.push(fps, 120.0);
        }
        run
    }

    /// A run that starts with the loading scene and plays `rates` in turn for the rest of five minutes
    fn after_loading(rates: &[f64]) -> Run {
        run(LOADING.iter().copied().chain(rates.iter().copied().cycle()).take(MIN_WINDOWS))
    }

    /// The new share of a step to a smaller size
    fn smaller(verdict: Verdict) -> (f32, f32) {
        match verdict {
            Verdict::Step(Step::Smaller { old, new }) => (old, new),
            other => panic!("expected a smaller share, got {other:?}"),
        }
    }

    /// The shares of a step back, from the share of the run to the one before the last step
    fn back(verdict: Verdict) -> (f32, f32) {
        match verdict {
            Verdict::Step(Step::Back { old, new, .. }) => (old, new),
            other => panic!("expected the share before the last step, got {other:?}"),
        }
    }

    fn keeps(verdict: Verdict) -> f64 {
        match verdict {
            Verdict::Keeps { fps, .. } => fps,
            other => panic!("expected no change, got {other:?}"),
        }
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.001
    }

    /// The capped loading scene filled the first minute of the old judgment: 12 of its first 12 windows
    /// were slow, and the game lost 15% of its size for one start (issue #167). The whole run's median
    /// is play.
    #[test]
    fn a_capped_loading_scene_is_not_slow_play() {
        let dir = temp("loading");
        let sizes = Sizes::new(&dir);
        assert_eq!(LOADING[..12].iter().filter(|&&fps| fps < SLOW * 120.0).count(), 12);
        let fps = keeps(sizes.learn(GAME, &after_loading(&PLAY)));
        assert!(fps > 118.0, "{fps}");
        assert!(!dir.join("game-sizes.txt").exists() && !dir.join("game-rates.txt").exists());
        assert_eq!(sizes.share(GAME), 1.0);
        fs::remove_dir_all(&dir).ok();
    }

    /// A game that is capped while it loads and slow in play gets a step that aims from its play
    #[test]
    fn a_capped_start_then_slow_play_gets_a_step_from_its_play_rate() {
        let dir = temp("loading-slow");
        let sizes = Sizes::new(&dir);
        let (old, new) = smaller(sizes.learn(GAME, &after_loading(&[84.0, 86.5, 85.0, 83.2, 87.1, 85.6])));
        // 85 fps at 120 Hz: about 0.80 of the size. The loading scene's 99 would give 0.86.
        assert_eq!(old, 1.0);
        assert!((0.78..0.82).contains(&new), "{new}");
        let rates = fs::read_to_string(dir.join("game-rates.txt")).unwrap();
        assert!(rates.starts_with("1.000 85."), "{rates}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_slow_game_with_no_cap_gets_a_smaller_share() {
        let dir = temp("slow");
        let sizes = Sizes::new(&dir);
        assert_eq!(sizes.share(GAME), 1.0);
        // 88 to 94 fps at 120 Hz, the median 91: about 0.83 of the size
        let slow = [88.0, 91.0, 94.0, 90.2, 92.5, 91.4];
        let (old, new) = smaller(sizes.learn(GAME, &run(slow.iter().copied().cycle().take(MIN_WINDOWS))));
        assert_eq!(old, 1.0);
        assert!((0.8..0.86).contains(&new), "{new}");
        assert!((sizes.share(&GAME.to_uppercase()) - new).abs() < 0.001);
        // The runtime's format: share, one space, the program
        let text = fs::read_to_string(dir.join("game-sizes.txt")).unwrap();
        assert_eq!(text, format!("{new:.3} {GAME}\n"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_fast_game_keeps_its_size() {
        let dir = temp("fast");
        let sizes = Sizes::new(&dir);
        // Slow windows while a song or a level loads, a third of the run, are not enough
        let fast = [119.0, 120.0, 118.5, 70.0, 119.6, 80.0];
        keeps(sizes.learn(GAME, &run(fast.iter().copied().cycle().take(MIN_WINDOWS))));
        assert!(!dir.join("game-sizes.txt").exists());
        fs::remove_dir_all(&dir).ok();
    }

    /// A run that ends before five minutes decides nothing: it may be all loading
    #[test]
    fn a_short_run_decides_nothing() {
        let dir = temp("short");
        let sizes = Sizes::new(&dir);
        let loading_and_menu = run(LOADING.iter().chain(&PLAY[..3]).copied());
        assert_eq!(sizes.learn(GAME, &loading_and_menu), Verdict::Short { windows: 20 });
        assert_eq!(sizes.learn(GAME, &run([60.0; MIN_WINDOWS - 1])), Verdict::Short { windows: MIN_WINDOWS - 1 });
        assert!(!dir.join("game-sizes.txt").exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_new_rate_starts_the_run_again() {
        let mut run = run([60.0; MIN_WINDOWS]);
        run.push(85.0, 90.0);
        assert_eq!(run.windows, [85.0]);
    }

    /// A game that holds its play at 99 frames a second by its own cap. A smaller size cannot raise
    /// that rate: the game gets the full size back and keeps it at every later start (issue #155).
    #[test]
    fn a_game_that_caps_its_own_rate_gets_its_size_back_and_keeps_it() {
        let dir = temp("capped");
        let sizes = Sizes::new(&dir);
        let other = r"C:\Games\Other\Other.exe";
        let capped = || run([99.0; MIN_WINDOWS]);
        smaller(sizes.learn(other, &run([90.0; MIN_WINDOWS])));
        let (_, first) = smaller(sizes.learn(GAME, &capped()));
        assert!(first < 0.95, "{first}");
        // The next start, at the smaller size, makes the same 99 fps
        let (old, new) = back(sizes.learn(GAME, &capped()));
        assert!(near(old, first) && new == 1.0, "{old} {new}");
        assert_eq!(sizes.share(GAME), 1.0);
        // The line stays at the full size, so the app lists the game and can forget what it learned
        let text = fs::read_to_string(dir.join("game-sizes.txt")).unwrap();
        assert!(text.contains(&format!("1.000 {GAME}")), "{text}");
        assert!(text.contains(other), "{text}");
        // Every later start keeps the full size
        for _ in 0..3 {
            keeps(sizes.learn(GAME, &capped()));
            assert_eq!(sizes.share(GAME), 1.0);
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_step_that_did_not_help_goes_back_to_the_share_before_it() {
        let dir = temp("cpu");
        let sizes = Sizes::new(&dir);
        let at = |fps: f64| run([fps; MIN_WINDOWS]);
        let (_, first) = smaller(sizes.learn(GAME, &at(80.0)));
        // A real gain allows one more step
        let (old, second) = smaller(sizes.learn(GAME, &at(100.0)));
        assert!(near(old, first) && second < first);
        // The second step gained nothing: the CPU limits the game now. It gets the first share back.
        let (old, new) = back(sizes.learn(GAME, &at(101.0)));
        assert!(near(old, second) && near(new, first), "{old} {new}");
        assert!(near(sizes.share(GAME), first));
        keeps(sizes.learn(GAME, &at(101.0)));
        assert!(near(sizes.share(GAME), first));
        assert_eq!(fs::read_to_string(dir.join("game-sizes.txt")).unwrap().lines().count(), 1);
        assert_eq!(fs::read_to_string(dir.join("game-rates.txt")).unwrap().lines().count(), 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_share_stops_at_the_floor() {
        let to = |share: f32| Some(Step::Smaller { old: 0.42, new: share });
        assert_eq!(next_share(0.42, 30.0, 120.0, None), to(FLOOR));
        assert_eq!(next_share(FLOOR, 30.0, 120.0, None), None);
        assert_eq!(next_share(1.0, 118.0, 120.0, None), None);
    }
}
