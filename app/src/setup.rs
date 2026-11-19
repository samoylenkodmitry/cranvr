//! The one-button setup of Windows games. "Set up games" runs the steps that this Mac still
//! lacks, in order, and each step reports a line of progress: Rosetta 2, the game support archive
//! (also the update of an older one), and Steam for Windows. A failed step keeps its message.
//! Retry runs the setup again: steps that are done no longer show up in the plan.

use crate::{backend, steam, steam::SteamState};
use std::{sync::Mutex, thread};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Rosetta,
    GameSupport,
    Steam,
}

impl Step {
    pub fn title(self) -> &'static str {
        match self {
            Step::Rosetta => "Rosetta 2",
            Step::GameSupport => "Game support",
            Step::Steam => "Steam for Windows",
        }
    }

    /// What the step does, shown before it runs. Wine in place means the game support step
    /// updates it. A Steam that needs setup is installed already.
    pub fn describe(self, state: SteamState) -> &'static str {
        match (self, state) {
            (Step::Rosetta, _) => "Runs Wine's Intel code. macOS asks for your Mac password.",
            (Step::GameSupport, SteamState::NoWine) => "Wine, DXMT and the OpenXR runtime. A download of about 250 MB.",
            (Step::GameSupport, _) => "Updates the OpenXR runtime and DXMT in Wine.",
            (Step::Steam, SteamState::NeedsSetup) => "Steam needs a repair after its own update. This takes a minute.",
            (Step::Steam, _) => "Installs Steam and prepares it for this Mac. A download of about 1.6 GB.",
        }
    }

    fn starting(self) -> &'static str {
        match self {
            Step::Rosetta => "Installing. Type your Mac password when macOS asks.",
            Step::GameSupport | Step::Steam => "Starting",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    Waiting,
    Working(String),
    Done,
    Failed(String),
}

/// The steps of one run with their progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setup {
    pub steps: Vec<(Step, Progress)>,
}

impl Setup {
    pub fn running(&self) -> bool {
        self.steps.iter().any(|(_, progress)| matches!(progress, Progress::Waiting | Progress::Working(_)))
    }

    pub fn failed(&self) -> bool {
        self.steps.iter().any(|(_, progress)| matches!(progress, Progress::Failed(_)))
    }
}

static PROGRESS: Mutex<Option<Setup>> = Mutex::new(None);

/// The run that is going or ended last.
pub fn current() -> Option<Setup> {
    PROGRESS.lock().unwrap().clone()
}

/// The steps that this Mac needs, in order. Rosetta comes first because Wine runs on it. The game
/// support step installs the missing Wine or updates an older one. Steam comes last, and a Steam
/// that is not ready needs it.
pub fn plan(rosetta: bool, state: SteamState, game_support_outdated: bool) -> Vec<Step> {
    let mut steps = vec![];
    if !rosetta {
        steps.push(Step::Rosetta);
    }
    if state == SteamState::NoWine || game_support_outdated {
        steps.push(Step::GameSupport);
    }
    if state != SteamState::Ready {
        steps.push(Step::Steam);
    }
    steps
}

/// Starts the steps that this Mac needs now. A run that is going keeps going.
pub fn start() {
    let mut slot = PROGRESS.lock().unwrap();
    if slot.as_ref().is_some_and(Setup::running) {
        return;
    }
    let steps = plan(steam::rosetta_ready(), steam::state(), steam::game_support_outdated());
    if steps.is_empty() {
        return;
    }
    *slot = Some(Setup { steps: steps.into_iter().map(|step| (step, Progress::Waiting)).collect() });
    drop(slot);
    thread::spawn(run);
}

fn update(edit: impl FnOnce(&mut Setup)) {
    if let Some(setup) = PROGRESS.lock().unwrap().as_mut() {
        edit(setup);
    }
}

fn set(index: usize, progress: Progress) {
    update(|setup| setup.steps[index].1 = progress);
}

fn step_at(index: usize) -> Option<Step> {
    current()?.steps.get(index).map(|(step, _)| *step)
}

fn run() {
    let mut index = 0;
    while let Some(step) = step_at(index) {
        set(index, Progress::Working(step.starting().into()));
        match run_step(step, index) {
            Ok(()) => set(index, Progress::Done),
            Err(message) => {
                backend::log(format!("{}: {message}", step.title()));
                set(index, Progress::Failed(message));
                return;
            }
        }
        // A new runtime can change Steam's web helper shim: Steam then needs its own step
        if step == Step::GameSupport && steam::state() == SteamState::NeedsSetup {
            update(|setup| {
                if !setup.steps.iter().any(|(step, _)| *step == Step::Steam) {
                    setup.steps.push((Step::Steam, Progress::Waiting));
                }
            });
        }
        index += 1;
    }
    // The user only signs in now: the Steam window opens for it
    if current().is_some_and(|setup| setup.steps.iter().any(|(step, _)| *step == Step::Steam)) {
        if let Err(e) = steam::start() {
            backend::notify(e);
        }
    }
}

fn run_step(step: Step, index: usize) -> Result<(), String> {
    let show = |text: String| set(index, Progress::Working(text));
    match step {
        Step::Rosetta => {
            steam::install_rosetta()?;
            if steam::rosetta_ready() {
                Ok(())
            } else {
                Err("Rosetta 2 is still missing after the install.".into())
            }
        }
        Step::GameSupport => {
            // An update replaces files that Steam and games hold open
            steam::stop_all();
            backend::run_script("install-game-support.sh", &[], |line| show(plain_progress(line))).map_err(|e| failure(&e))
        }
        Step::Steam => backend::run_script("setup-steam.sh", &[], |line| show(plain_progress(line))).map_err(|e| failure(&e)),
    }
}

/// A failed script's complaint, in words for the window. The Log tab keeps the script's output.
fn failure(complaint: &str) -> String {
    let reason = if complaint.contains("Could not resolve host") || complaint.contains("Failed to connect") || complaint.contains("timed out") {
        "The download did not reach its server. Check this Mac's internet connection".to_owned()
    } else {
        complaint.trim_start_matches("FAIL:").trim().trim_end_matches('.').replace("Run this script again", "Press Retry")
    };
    format!("{reason}. The Log tab has the details.")
}

/// The progress line of a step, from a line that its script printed. The scripts name paths and
/// tools; the window names what happens.
pub fn plain_progress(line: &str) -> String {
    if let Some(percent) = backend::percent(line) {
        return format!("Downloading: {percent}%");
    }
    let rules: [(&str, &str); 10] = [
        ("Downloading the game support", "Downloading"),
        ("Checked the archive", "Checked the download"),
        ("Unpacking", "Unpacking"),
        ("Cloning Wine", "Copying Wine"),
        ("Creating the Wine prefix", "Creating the Wine folder for games. This takes a minute."),
        ("Checking the OpenXR runtime", "Checking the OpenXR runtime"),
        ("Downloading SteamSetup", "Downloading the Steam installer"),
        ("Installing Steam", "Installing Steam"),
        ("Starting Steam once", "Steam downloads its client. This takes several minutes."),
        ("Installed the web helper", "Preparing Steam's window"),
    ];
    if let Some((_, text)) = rules.iter().find(|(prefix, _)| line.starts_with(prefix)) {
        return (*text).to_owned();
    }
    // "Steam folder: 812 MB", printed while Steam downloads its client
    if let Some(size) = line.strip_prefix("Steam folder:") {
        return format!("Steam downloads its client: {} so far. This takes several minutes.", size.trim());
    }
    line.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_mac_needs_every_step_in_order() {
        assert_eq!(plan(false, SteamState::NoWine, false), [Step::Rosetta, Step::GameSupport, Step::Steam]);
        assert_eq!(plan(true, SteamState::NoWine, false), [Step::GameSupport, Step::Steam]);
    }

    #[test]
    fn a_mac_with_wine_needs_the_steps_that_are_missing() {
        assert_eq!(plan(true, SteamState::NotInstalled, false), [Step::Steam]);
        assert_eq!(plan(true, SteamState::NeedsSetup, false), [Step::Steam]);
        assert_eq!(plan(true, SteamState::NotInstalled, true), [Step::GameSupport, Step::Steam]);
        assert!(plan(true, SteamState::Ready, false).is_empty());
    }

    #[test]
    fn an_update_of_the_game_support_is_one_step() {
        assert_eq!(plan(true, SteamState::Ready, true), [Step::GameSupport]);
    }

    #[test]
    fn a_run_is_going_while_a_step_waits_or_works() {
        let mut setup = Setup { steps: vec![(Step::Rosetta, Progress::Done), (Step::GameSupport, Progress::Waiting)] };
        assert!(setup.running() && !setup.failed());
        setup.steps[1].1 = Progress::Working("Unpacking".into());
        assert!(setup.running());
        setup.steps[1].1 = Progress::Failed("no network".into());
        assert!(!setup.running() && setup.failed());
        setup.steps[1].1 = Progress::Done;
        assert!(!setup.running() && !setup.failed());
    }

    #[test]
    fn script_lines_become_plain_progress() {
        assert_eq!(plain_progress("###########            37.5%"), "Downloading: 37%");
        assert_eq!(plain_progress("Downloading the game support (about 250 MB) from https://x/y"), "Downloading");
        assert_eq!(plain_progress("Creating the Wine prefix /Users/a/wine/prefix"), "Creating the Wine folder for games. This takes a minute.");
        assert_eq!(plain_progress("Steam folder: 812 MB"), "Steam downloads its client: 812 MB so far. This takes several minutes.");
        assert_eq!(plain_progress("Something new"), "Something new");
    }

    #[test]
    fn a_failure_gives_the_reason_and_points_to_the_log() {
        assert_eq!(
            failure("FAIL: the archive's SHA-256 is a, the release says b."),
            "the archive's SHA-256 is a, the release says b. The Log tab has the details."
        );
        assert!(failure("curl: (6) Could not resolve host: github.com").contains("internet connection"));
        assert!(failure("FAIL: Steam did not finish its update. Run this script again.").contains("Press Retry"));
    }
}
