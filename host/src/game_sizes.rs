//! A render size per game. A game that stays below the headset's rate gets a smaller share of
//! the view size at its next start: the runtime reads `game-sizes.txt` next to the control file,
//! one line "<share> <program>" per game, once per game start (cranvr's runtime patch 0002).
//! A game reads its view size only when it starts, so a new share cannot apply during a session.
//!
//! The rate cap hides how fast a game could run, so the host never raises a share above the one
//! before its last step. The share returns to the full size when the user forgets the learned sizes
//! in the app.
//!
//! `game-rates.txt` keeps the frame rate that each game made before its last step, one line
//! "<share> <fps> <program>". When a smaller share did not speed a game up, its size is not
//! what limits it: the game gets the share from before that step back and keeps it. A game that
//! caps its own frame rate, such as a loading scene that the game holds at 99 frames a second, would
//! otherwise lose pixels for nothing (issue #155). The rates line then names the share that the
//! game has again, which tells the next start that the size does not limit the game.

use log::warn;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// The smallest share the host sets
pub const FLOOR: f32 = 0.4;
/// Stats windows (5 s each) the host watches before it decides, once per game start
pub const WINDOWS: usize = 12;
/// A decision needs this many of the windows below the rate
const SLOW_WINDOWS: usize = 10;
/// A window below this share of the headset's rate counts as slow
const SLOW: f64 = 0.95;

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

    /// Decides from the game's frame rates in WINDOWS stats windows at the headset's `rate`.
    /// Returns the change of the game's share for its next start, or none.
    pub fn learn(&self, program: &str, rates: &[f64], rate: f64) -> Option<Step> {
        if rates.iter().filter(|&&fps| fps < SLOW * rate).count() < SLOW_WINDOWS {
            return None;
        }
        let mut sorted = rates.to_vec();
        sorted.sort_by(f64::total_cmp);
        let fps = sorted[sorted.len() / 2];
        let share = self.share(program);
        let step = next_share(share, fps, rate, self.before(program))?;
        match step {
            Step::Smaller { old, new } => {
                replace(&self.sizes, program, &format!("{new:.3} {program}"));
                replace(&self.rates, program, &format!("{old:.3} {fps:.1} {program}"));
            }
            Step::Back { new, .. } => {
                // The full size needs no line: the app then lists no learned size for the game
                if new < 1.0 - 0.001 {
                    replace(&self.sizes, program, &format!("{new:.3} {program}"));
                } else {
                    remove(&self.sizes, program);
                }
                // The rates line keeps the share from before the step and its frame rate. Its share is
                // the game's share again, so the next start takes no step.
            }
        }
        Some(step)
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
    let mut kept = others(path, program);
    kept.push(line.to_string());
    write(path, &kept);
}

/// Takes the program's line out of the file.
fn remove(path: &Path, program: &str) {
    if path.exists() {
        write(path, &others(path, program));
    }
}

/// The lines of the file that name another program
fn others(path: &Path, program: &str) -> Vec<String> {
    lines(path).into_iter().filter(|l| !l.split_once(' ').is_some_and(|(_, rest)| ends_with_program(rest, program))).collect()
}

fn write(path: &Path, lines: &[String]) {
    let text = if lines.is_empty() { String::new() } else { lines.join("\n") + "\n" };
    if let Err(e) = fs::write(path, text) {
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

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cranvr-sizes-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The new share of a step to a smaller size
    fn smaller(step: Option<Step>) -> (f32, f32) {
        match step {
            Some(Step::Smaller { old, new }) => (old, new),
            other => panic!("expected a smaller share, got {other:?}"),
        }
    }

    /// The shares of a step back, from the share of the run to the one before the last step
    fn back(step: Option<Step>) -> (f32, f32) {
        match step {
            Some(Step::Back { old, new, .. }) => (old, new),
            other => panic!("expected the share before the last step, got {other:?}"),
        }
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.001
    }

    #[test]
    fn a_slow_game_gets_a_smaller_share_once_per_decision() {
        let dir = temp("slow");
        let sizes = Sizes::new(&dir);
        assert_eq!(sizes.share(GAME), 1.0);
        // 91 fps at 120 Hz: about 0.83 of the size
        let (old, new) = smaller(sizes.learn(GAME, &[91.0; WINDOWS], 120.0));
        assert_eq!(old, 1.0);
        assert!((0.8..0.86).contains(&new), "{new}");
        assert!((sizes.share(&GAME.to_uppercase()) - new).abs() < 0.001);
        // The runtime's format: share, one space, the program
        let text = fs::read_to_string(dir.join("game-sizes.txt")).unwrap();
        assert_eq!(text, format!("{new:.3} {GAME}\n"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_game_at_the_rate_keeps_its_size() {
        let dir = temp("fast");
        let sizes = Sizes::new(&dir);
        let mut rates = [119.0; WINDOWS];
        // Two slow windows, as while a song loads, are not enough
        rates[3] = 70.0;
        rates[7] = 80.0;
        assert_eq!(sizes.learn(GAME, &rates, 120.0), None);
        assert!(!dir.join("game-sizes.txt").exists());
        fs::remove_dir_all(&dir).ok();
    }

    /// Serious Sam VR holds its menu at 99 frames a second (issue #155). A smaller size cannot raise
    /// that rate. The game lost 15% of its size for good before; now it gets the full size back and
    /// keeps it at every later start.
    #[test]
    fn a_game_that_caps_its_own_rate_gets_its_size_back_and_keeps_it() {
        let dir = temp("capped");
        let sizes = Sizes::new(&dir);
        let other = r"C:\Games\Other\Other.exe";
        smaller(sizes.learn(other, &[90.0; WINDOWS], 120.0));
        let (_, first) = smaller(sizes.learn(GAME, &[99.0; WINDOWS], 120.0));
        assert!(first < 0.95, "{first}");
        // The next start, at the smaller size, makes the same 99 fps
        let (old, new) = back(sizes.learn(GAME, &[99.0; WINDOWS], 120.0));
        assert!(near(old, first) && new == 1.0, "{old} {new}");
        assert_eq!(sizes.share(GAME), 1.0);
        // The full size needs no line: the app lists no learned size for the game
        let text = fs::read_to_string(dir.join("game-sizes.txt")).unwrap();
        assert!(!text.contains("Some Game"), "{text}");
        assert!(text.contains(other), "{text}");
        // Every later start keeps the full size
        for _ in 0..3 {
            assert_eq!(sizes.learn(GAME, &[99.0; WINDOWS], 120.0), None);
            assert_eq!(sizes.share(GAME), 1.0);
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_step_that_did_not_help_goes_back_to_the_share_before_it() {
        let dir = temp("cpu");
        let sizes = Sizes::new(&dir);
        let (_, first) = smaller(sizes.learn(GAME, &[80.0; WINDOWS], 120.0));
        // A real gain allows one more step
        let (old, second) = smaller(sizes.learn(GAME, &[100.0; WINDOWS], 120.0));
        assert!(near(old, first) && second < first);
        // The second step gained nothing: the CPU limits the game now. It gets the first share back.
        let (old, new) = back(sizes.learn(GAME, &[101.0; WINDOWS], 120.0));
        assert!(near(old, second) && near(new, first), "{old} {new}");
        assert!(near(sizes.share(GAME), first));
        assert_eq!(sizes.learn(GAME, &[101.0; WINDOWS], 120.0), None);
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
