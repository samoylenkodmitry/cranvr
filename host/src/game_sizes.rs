//! A render size per game. A game that stays below the headset's rate gets a smaller share of
//! the view size at its next start: the runtime reads `game-sizes.txt` next to the control file,
//! one line "<share> <program>" per game, once per game start (cranvr's runtime patch 0002).
//! A game reads its view size only when it starts, so a new share cannot apply during a session.
//!
//! The rate cap hides how fast a game could run, so the host only lowers a share. The share
//! returns to the full size when the user forgets the learned sizes in the app.
//!
//! `game-rates.txt` keeps the frame rate that each game made before its last step, one line
//! "<share> <fps> <program>". When a smaller share did not speed a game up, its size is not
//! what limits it, and the host stops lowering it.

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
    /// Returns the old and the new share when the game gets a smaller size at its next start.
    pub fn learn(&self, program: &str, rates: &[f64], rate: f64) -> Option<(f32, f32)> {
        if rates.iter().filter(|&&fps| fps < SLOW * rate).count() < SLOW_WINDOWS {
            return None;
        }
        let mut sorted = rates.to_vec();
        sorted.sort_by(f64::total_cmp);
        let fps = sorted[sorted.len() / 2];
        let share = self.share(program);
        let next = next_share(share, fps, rate, self.before(program))?;
        replace(&self.sizes, program, &format!("{next:.3} {program}"));
        replace(&self.rates, program, &format!("{share:.3} {fps:.1} {program}"));
        Some((share, next))
    }
}

/// The next share for a game that made `fps` at `share` while the headset shows `rate`, or none.
/// `before` is the share and frame rate before the last step.
fn next_share(share: f32, fps: f64, rate: f64, before: Option<(f32, f64)>) -> Option<f32> {
    if fps >= SLOW * rate || share <= FLOOR + 0.001 {
        return None;
    }
    // A smaller size that did not speed the game up: the size is not what limits it
    if let Some((old_share, old_fps)) = before {
        if old_share > share + 0.001 && fps < old_fps * 1.05 {
            return None;
        }
    }
    // The GPU time follows the pixel count, the square of the share. Aim 10% under the rate.
    let step = (fps / rate * 0.9).sqrt().clamp(0.7, 0.95) as f32;
    Some((share * step).max(FLOOR))
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

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cranvr-sizes-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_slow_game_gets_a_smaller_share_once_per_decision() {
        let dir = temp("slow");
        let sizes = Sizes::new(&dir);
        assert_eq!(sizes.share(GAME), 1.0);
        // 91 fps at 120 Hz: about 0.83 of the size
        let (old, new) = sizes.learn(GAME, &[91.0; WINDOWS], 120.0).unwrap();
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

    #[test]
    fn a_game_that_a_smaller_size_did_not_speed_up_stops_shrinking() {
        let dir = temp("cpu");
        let sizes = Sizes::new(&dir);
        let (_, first) = sizes.learn(GAME, &[80.0; WINDOWS], 120.0).unwrap();
        // The next start, at the smaller size, makes the same 81 fps: the CPU limits this game
        assert_eq!(sizes.learn(GAME, &[81.0; WINDOWS], 120.0), None);
        assert!((sizes.share(GAME) - first).abs() < 0.001);
        // A real gain allows one more step
        let (old, second) = sizes.learn(GAME, &[100.0; WINDOWS], 120.0).unwrap();
        assert!((old - first).abs() < 0.001 && second < first);
        assert_eq!(fs::read_to_string(dir.join("game-sizes.txt")).unwrap().lines().count(), 1);
        assert_eq!(fs::read_to_string(dir.join("game-rates.txt")).unwrap().lines().count(), 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_share_stops_at_the_floor() {
        assert_eq!(next_share(0.42, 30.0, 120.0, None), Some(FLOOR));
        assert_eq!(next_share(FLOOR, 30.0, 120.0, None), None);
        assert_eq!(next_share(1.0, 118.0, 120.0, None), None);
    }
}
