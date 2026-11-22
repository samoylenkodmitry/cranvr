//! The game that cranvr started and runs now. A run lasts from the game's start until no process of its folder
//! runs any more: a launcher may start the game's own program and end, and a mod loader may start the game twice.
//! At its start the run makes the prefix's OpenVR path file name the game's runtime (openvr.rs). At its end it
//! puts back what the file said before, so the file names OpenComposite only while a game that uses it runs.
//!
//! One game runs at a time. A restart of the game (black_eye.rs) ends it and starts it again: the new run takes
//! over the file that the old run saved, and the old run's end changes nothing, because a newer run holds the
//! file now.

use crate::{launch::OpenVr, openvr};
use std::{
    path::Path,
    process::{Child, Command},
    sync::Mutex,
    thread,
    time::Duration,
};

/// The run that holds the path file.
struct Run {
    /// Counts the runs, so an old run's end knows that a newer run took over
    generation: u64,
    /// What the file said before the first of these runs
    saved: openvr::Saved,
}

static RUN: Mutex<Option<Run>> = Mutex::new(None);
static GENERATION: Mutex<u64> = Mutex::new(0);

/// Makes the path file name the runtime that `choice` names, for a run that starts now. Returns the run's
/// number. A run that is still on (a restart) keeps the file that it saved, so the end of the new run puts back
/// the file from before both.
pub fn begin(prefix: &Path, choice: OpenVr, steamvr: Option<&openvr::SteamVr>) -> Result<u64, String> {
    let mut run = RUN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let saved = openvr::begin(prefix, choice, steamvr).map_err(|e| format!("cranvr could not set the OpenVR runtime: {e}"))?;
    let mut generation = GENERATION.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    *generation += 1;
    let saved = run.take().map_or(saved, |old| old.saved);
    *run = Some(Run { generation: *generation, saved });
    Ok(*generation)
}

/// Ends run `generation`: puts back the path file from before it. A run that a newer one replaced changes
/// nothing.
pub fn end(prefix: &Path, generation: u64) -> Result<(), String> {
    let mut run = RUN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if run.as_ref().is_none_or(|run| run.generation != generation) {
        return Ok(());
    }
    let saved = run.take().map(|run| run.saved);
    if let Some(saved) = saved {
        openvr::end(prefix, &saved).map_err(|e| format!("cranvr could not put the OpenVR path file back: {e}"))?;
    }
    Ok(())
}

/// Whether a run holds the path file now.
pub fn active() -> bool {
    RUN.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some()
}

/// Waits on a thread of its own until the game of run `generation` ends, then ends the run. `child` is the
/// Wine process that cranvr started. `folder` is the game's folder as Windows names it, with a backslash at
/// its end: the run lasts while any process with it in its command line runs.
pub fn watch(prefix: &Path, generation: u64, mut child: Child, folder: String) {
    let prefix = prefix.to_owned();
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_millis(500));
            if child.try_wait().ok().flatten().is_some() && !folder_runs(&folder) {
                break;
            }
        }
        if let Err(e) = end(&prefix, generation) {
            crate::backend::notify(e);
        }
    });
}

/// Whether a process runs whose command line holds `folder`. A Windows program under Wine shows its Windows
/// path there.
fn folder_runs(folder: &str) -> bool {
    Command::new("pgrep").args(["-if", &crate::steam::regex_escaped(folder)]).output().is_ok_and(|o| !o.stdout.is_empty())
}

/// The folder of a Windows program, with a backslash at its end, as [`watch`] takes it.
pub fn folder_of(program: &str) -> String {
    match program.rfind('\\') {
        Some(at) => program[..=at].to_owned(),
        None => program.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// The tests share the one run of the app, so they run one after another in this test
    #[test]
    fn a_run_puts_back_the_file_at_its_end_and_a_restart_keeps_the_first_file() {
        let dir = std::env::temp_dir().join(format!("cranvr-run-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(dir.join("drive_c/users/someone")).unwrap();
        fs::create_dir_all(dir.join("drive_c/opencomposite/bin")).unwrap();
        fs::write(dir.join("drive_c/opencomposite/bin/vrclient_x64.dll"), b"x").unwrap();
        let file = openvr::path_file(&dir).unwrap();
        assert!(!active());
        let first = begin(&dir, OpenVr::OpenComposite, None).unwrap();
        assert!(active() && file.exists());
        // A restart: the old run's end comes after the new run's start and changes nothing
        let second = begin(&dir, OpenVr::OpenComposite, None).unwrap();
        assert!(second > first);
        end(&dir, first).unwrap();
        assert!(file.exists(), "the newer run holds the file");
        end(&dir, second).unwrap();
        assert!(!file.exists(), "the file from before the first run: none");
        assert!(!active());
        // An end that comes twice changes nothing
        end(&dir, second).unwrap();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_program_s_folder_ends_in_a_backslash() {
        assert_eq!(folder_of(r"C:\Games\The Lab\TheLab\win64\TheLab.exe"), r"C:\Games\The Lab\TheLab\win64\");
        assert_eq!(folder_of("game.exe"), "game.exe");
    }
}
