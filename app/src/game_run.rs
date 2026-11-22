//! The game that cranvr started and runs now. A run lasts from the game's start until no process of its folder
//! runs any more: a launcher may start the game's own program and end, and a mod loader may start the game twice.
//! At its start the run makes the prefix's OpenVR path file name the game's runtime (openvr.rs). At its end it
//! puts back what the file said before, so the file names OpenComposite only while a game that uses it runs.
//!
//! One game runs at a time. A restart of the game (black_eye.rs, vr_system.rs) ends it and starts it again: the
//! new run takes over the file that the old run saved, and the old run's end changes nothing, because a newer run
//! holds the file now.
//!
//! A run through OpenComposite also watches whether the game draws anything (vr_system.rs). The host's status
//! lines say when the game connects to the runtime, when it disconnects and how many frames it delivers. The
//! game drew nothing through OpenComposite when OpenComposite took its OpenVR calls (OpenComposite's log of this
//! run names the game's folder), no frame came, cranvr did not stop it, and one of these holds:
//! - OpenComposite stopped the game: its log shows an abort, which also leaves a message box on the Mac,
//! - the game closed its last session and opened none for 10 s while it kept running,
//! - the game ended by itself.
//!
//! A game that loads for minutes before its first frame keeps its session open, so it gives no verdict.

use crate::{launch::OpenVr, openvr};
use std::{
    fs,
    path::Path,
    process::{Child, Command},
    sync::Mutex,
    thread,
    time::{Duration, Instant, SystemTime},
};

/// How long a game may stay without a session after it closed one, before it counts as a game that gave up VR
const NO_SESSION: Duration = Duration::from_secs(10);

/// The run that holds the path file.
struct Run {
    /// Counts the runs, so an old run's end knows that a newer run took over
    generation: u64,
    /// What the file said before the first of these runs
    saved: openvr::Saved,
    watch: Watch,
}

/// What a run of a game through OpenComposite gave, as far as it decides that the game drew nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Watch {
    /// The game's program, as Windows names it
    pub program: String,
    /// The folder of the game's processes, as Windows names it, with a backslash at its end
    pub folder: String,
    pub choice: Option<OpenVr>,
    pub started: Option<SystemTime>,
    /// The game connected to the runtime at least once
    pub connected: bool,
    /// The game holds a session now
    pub connected_now: bool,
    /// When the game's last session ended
    pub disconnected_at: Option<Instant>,
    /// A frame of the game reached the host
    pub frames: bool,
    /// cranvr ended the game: a Stop from the console, a restart
    pub stopped: bool,
    /// The verdict came already, so the run gives no second one
    pub decided: bool,
}

/// Why a game drew nothing through OpenComposite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nothing {
    /// OpenComposite stopped the game with an error
    Aborted,
    /// The game closed its session and opened none again
    Closed,
    /// The game ended by itself without a frame
    Ended,
}

/// A run's verdict that its game drew nothing through OpenComposite: the program and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub program: String,
    pub why: Nothing,
}

/// What OpenComposite's log of a run says about the game in `folder`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogSays {
    /// The log is of another program, or older than the run
    NotThisGame,
    /// OpenComposite took the game's OpenVR calls
    Loaded,
    /// OpenComposite stopped the game with an error
    Aborted,
}

/// Reads OpenComposite's log `text` for the game in `folder`.
pub fn log_says(text: &str, folder: &str) -> LogSays {
    let folder = folder.to_ascii_lowercase();
    if folder.is_empty() || !text.to_ascii_lowercase().contains(&folder) {
        return LogSays::NotThisGame;
    }
    // OpenComposite's abort writes this function's name into each of its lines (logging.cpp)
    if text.contains("oovr_abort_raw_va") {
        LogSays::Aborted
    } else {
        LogSays::Loaded
    }
}

impl Watch {
    /// A host status line about the game or its frames
    pub fn note(&mut self, status: &serde_json::Value, now: Instant) {
        match status["type"].as_str() {
            Some("game") if status["connected"] == true => {
                self.connected = true;
                self.connected_now = true;
                self.disconnected_at = None;
            }
            Some("game") => {
                if self.connected_now {
                    self.disconnected_at = Some(now);
                }
                self.connected_now = false;
            }
            Some("stats") if status["game_frames"].as_u64().is_some_and(|frames| frames > 0) => self.frames = true,
            _ => {}
        }
    }

    /// Whether the run can still give a verdict
    fn open(&self) -> bool {
        self.choice == Some(OpenVr::OpenComposite) && !self.frames && !self.stopped && !self.decided
    }

    /// The verdict while the game runs, from what OpenComposite's log says now
    pub fn check(&mut self, log: LogSays, now: Instant) -> Option<Nothing> {
        if !self.open() || log == LogSays::NotThisGame {
            return None;
        }
        let why = if log == LogSays::Aborted {
            Nothing::Aborted
        } else if self.disconnected_at.is_some_and(|at| now.saturating_duration_since(at) >= NO_SESSION) {
            Nothing::Closed
        } else {
            return None;
        };
        self.decided = true;
        Some(why)
    }

    /// The verdict at the end of the game's processes
    pub fn ended(&mut self, log: LogSays) -> Option<Nothing> {
        if !self.open() || log == LogSays::NotThisGame {
            return None;
        }
        self.decided = true;
        Some(if log == LogSays::Aborted { Nothing::Aborted } else { Nothing::Ended })
    }
}

static RUN: Mutex<Option<Run>> = Mutex::new(None);
static GENERATION: Mutex<u64> = Mutex::new(0);
static VERDICTS: Mutex<Vec<Verdict>> = Mutex::new(Vec::new());

fn run() -> std::sync::MutexGuard<'static, Option<Run>> {
    RUN.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Makes the path file name the runtime that `choice` names, for a run of `program` that starts now. Returns the
/// run's number. A run that is still on (a restart) keeps the file that it saved, so the end of the new run puts
/// back the file from before both.
pub fn begin(prefix: &Path, program: &str, choice: OpenVr, steamvr: Option<&openvr::SteamVr>) -> Result<u64, String> {
    let mut run = run();
    let saved = openvr::begin(prefix, choice, steamvr).map_err(|e| format!("cranvr could not set the OpenVR runtime: {e}"))?;
    let mut generation = GENERATION.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    *generation += 1;
    let saved = run.take().map_or(saved, |old| old.saved);
    let watch = Watch { program: program.to_owned(), folder: folder_of(program), choice: Some(choice), started: Some(SystemTime::now()), ..Default::default() };
    *run = Some(Run { generation: *generation, saved, watch });
    Ok(*generation)
}

/// Ends run `generation`: puts back the path file from before it. A run that a newer one replaced changes
/// nothing. Returns the run's verdict at its end, if any.
pub fn end(prefix: &Path, generation: u64) -> Result<Option<Verdict>, String> {
    let mut run = run();
    if run.as_ref().is_none_or(|run| run.generation != generation) {
        return Ok(None);
    }
    let Some(mut ended) = run.take() else { return Ok(None) };
    let verdict = ended.watch.ended(log_of(prefix, &ended.watch)).map(|why| Verdict { program: ended.watch.program.clone(), why });
    openvr::end(prefix, &ended.saved).map_err(|e| format!("cranvr could not put the OpenVR path file back: {e}"))?;
    Ok(verdict)
}

/// What OpenComposite's log says about the run's game, when the log is of this run
fn log_of(prefix: &Path, watch: &Watch) -> LogSays {
    let Some(file) = openvr::opencomposite_log(prefix) else { return LogSays::NotThisGame };
    let fresh = fs::metadata(&file).and_then(|m| m.modified()).is_ok_and(|modified| watch.started.is_some_and(|started| modified >= started));
    if !fresh {
        return LogSays::NotThisGame;
    }
    log_says(&fs::read_to_string(&file).unwrap_or_default(), &watch.folder)
}

/// Whether a run holds the path file now.
pub fn active() -> bool {
    run().is_some()
}

/// Takes a host status line for the run: the game's sessions and frames.
pub fn note_status(status: &serde_json::Value) {
    if let Some(run) = run().as_mut() {
        run.watch.note(status, Instant::now());
    }
}

/// cranvr ends the game now (a Stop from the console, a restart): the end is not the game's own.
pub fn stopping() {
    if let Some(run) = run().as_mut() {
        run.watch.stopped = true;
    }
}

/// The verdicts that the window has not answered yet (see vr_system.rs).
pub fn take_verdicts() -> Vec<Verdict> {
    std::mem::take(&mut *VERDICTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
}

fn give(verdict: Verdict) {
    VERDICTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(verdict);
}

/// Looks at run `generation` while its game runs: a verdict that OpenComposite stopped the game or that the
/// game gave up its session.
fn check(prefix: &Path, generation: u64) {
    let mut run = run();
    let Some(run) = run.as_mut().filter(|run| run.generation == generation) else { return };
    if !run.watch.open() {
        return;
    }
    let log = log_of(prefix, &run.watch);
    if let Some(why) = run.watch.check(log, Instant::now()) {
        give(Verdict { program: run.watch.program.clone(), why });
    }
}

/// Waits on a thread of its own until the game of run `generation` ends, then ends the run. `child` is the
/// Wine process that cranvr started. The run lasts while any process with the run's folder in its command line
/// runs. Each second the thread also looks for a verdict.
pub fn watch(prefix: &Path, generation: u64, mut child: Child) {
    let prefix = prefix.to_owned();
    let folder = run().as_ref().filter(|run| run.generation == generation).map(|run| run.watch.folder.clone()).unwrap_or_default();
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_millis(1000));
            if child.try_wait().ok().flatten().is_some() && !folder_runs(&folder) {
                break;
            }
            check(&prefix, generation);
        }
        match end(&prefix, generation) {
            Ok(Some(verdict)) => give(verdict),
            Ok(None) => {}
            Err(e) => crate::backend::notify(e),
        }
    });
}

/// Whether a process runs whose command line holds `folder`. A Windows program under Wine shows its Windows
/// path there.
fn folder_runs(folder: &str) -> bool {
    !folder.is_empty() && Command::new("pgrep").args(["-if", &crate::steam::regex_escaped(folder)]).output().is_ok_and(|o| !o.stdout.is_empty())
}

/// The folder of a run of `program`, as Windows names it, with a backslash at its end: the install folder of a
/// Steam game, where its launcher or another of its programs may run the game on, else the program's folder.
pub fn folder_of(program: &str) -> String {
    const COMMON: &str = "\\steamapps\\common\\";
    if let Some(at) = program.to_ascii_lowercase().find(COMMON) {
        let rest = at + COMMON.len();
        if let Some(end) = program[rest..].find('\\') {
            return program[..=rest + end].to_owned();
        }
    }
    match program.rfind('\\') {
        Some(at) => program[..=at].to_owned(),
        None => program.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        let first = begin(&dir, r"C:\Games\game.exe", OpenVr::OpenComposite, None).unwrap();
        assert!(active() && file.exists());
        // A restart: the old run's end comes after the new run's start and changes nothing
        stopping();
        let second = begin(&dir, r"C:\Games\game.exe", OpenVr::OpenComposite, None).unwrap();
        assert!(second > first);
        assert_eq!(end(&dir, first).unwrap(), None);
        assert!(file.exists(), "the newer run holds the file");
        // OpenComposite's log of this run names the game, and the game ends with no frame: a verdict
        let log = openvr::opencomposite_log(&dir).unwrap();
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        fs::write(&log, "No config file found at C:\\Games\\opencomposite.ini\n").unwrap();
        assert_eq!(end(&dir, second).unwrap(), Some(Verdict { program: r"C:\Games\game.exe".into(), why: Nothing::Ended }));
        assert!(!file.exists(), "the file from before the first run: none");
        assert!(!active());
        // An end that comes twice changes nothing
        assert_eq!(end(&dir, second).unwrap(), None);
        fs::remove_dir_all(&dir).ok();
    }

    fn watch(choice: OpenVr) -> Watch {
        Watch { program: r"C:\Lab\win64\Lab.exe".into(), folder: r"C:\Lab\".into(), choice: Some(choice), ..Default::default() }
    }

    #[test]
    fn opencomposite_s_log_tells_its_game_and_its_abort() {
        let loaded = "wini_parse:167\t- No config file found at C:\\LAB\\win64\\opencomposite.ini\nVR_InitInternal2:242\t- Initializing";
        assert_eq!(log_says(loaded, r"C:\Lab\"), LogSays::Loaded);
        assert_eq!(log_says(&format!("{loaded}\noovr_abort_raw_va:306\t- Abort!"), r"C:\Lab\"), LogSays::Aborted);
        // Steam's own process names Steam's folder
        assert_eq!(log_says("No config file found at C:\\Program Files (x86)\\Steam\\opencomposite.ini", r"C:\Lab\"), LogSays::NotThisGame);
        assert_eq!(log_says(loaded, ""), LogSays::NotThisGame);
    }

    #[test]
    fn an_abort_of_opencomposite_is_a_verdict_at_once_and_only_once() {
        let now = Instant::now();
        let mut run = watch(OpenVr::OpenComposite);
        run.note(&json!({"type": "game", "connected": true, "program": r"C:\Lab\win64\Lab.exe"}), now);
        assert_eq!(run.check(LogSays::Loaded, now), None, "a game that loads gives no verdict");
        assert_eq!(run.check(LogSays::Aborted, now), Some(Nothing::Aborted));
        assert_eq!(run.check(LogSays::Aborted, now), None);
        assert_eq!(run.ended(LogSays::Aborted), None);
    }

    #[test]
    fn a_game_that_closed_its_session_for_good_drew_nothing() {
        let now = Instant::now();
        let mut run = watch(OpenVr::OpenComposite);
        run.note(&json!({"type": "game", "connected": true}), now);
        run.note(&json!({"type": "game", "connected": false}), now);
        assert_eq!(run.check(LogSays::Loaded, now + Duration::from_secs(9)), None);
        // OpenComposite restarts its session at a game's start: a new session ends the wait
        run.note(&json!({"type": "game", "connected": true}), now + Duration::from_secs(9));
        assert_eq!(run.check(LogSays::Loaded, now + Duration::from_secs(30)), None, "a session is open");
        run.note(&json!({"type": "game", "connected": false}), now + Duration::from_secs(30));
        assert_eq!(run.check(LogSays::Loaded, now + Duration::from_secs(40)), Some(Nothing::Closed));
    }

    #[test]
    fn frames_a_stop_another_runtime_or_another_program_s_log_give_no_verdict() {
        let now = Instant::now();
        let mut drew = watch(OpenVr::OpenComposite);
        drew.note(&json!({"type": "stats", "game_frames": 0}), now);
        assert_eq!(drew.ended(LogSays::Loaded), Some(Nothing::Ended), "no frame: a verdict");
        let mut drew = watch(OpenVr::OpenComposite);
        drew.note(&json!({"type": "stats", "game_frames": 340}), now);
        assert_eq!(drew.ended(LogSays::Aborted), None);
        let mut stopped = watch(OpenVr::OpenComposite);
        stopped.stopped = true;
        assert_eq!(stopped.ended(LogSays::Loaded), None);
        assert_eq!(watch(OpenVr::SteamVr).ended(LogSays::Loaded), None);
        assert_eq!(watch(OpenVr::OpenComposite).ended(LogSays::NotThisGame), None, "an OpenXR game: no OpenComposite");
    }

    #[test]
    fn a_run_s_folder_is_the_steam_game_s_install_folder_or_the_program_s_folder() {
        assert_eq!(folder_of(r"C:\Program Files (x86)\Steam\steamapps\common\The Lab\TheLab\win64\TheLab.exe"), r"C:\Program Files (x86)\Steam\steamapps\common\The Lab\");
        assert_eq!(folder_of(r"Z:\Games\Some Game\bin\game.exe"), r"Z:\Games\Some Game\bin\");
        assert_eq!(folder_of("game.exe"), "game.exe");
    }
}
