//! A request of the window that takes a while, such as a game's start: the button that asked shows
//! that it runs, from the press until the state shows the result or a new message says why not.

use crate::backend::Model;
use std::time::{Duration, Instant};

/// How long a request may run before the window says that it got no answer. A game's start waits
/// for Steam and for Wine, which can take a minute.
pub const ANSWER_TIME: Duration = Duration::from_secs(90);

/// What the window asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    StartHost,
    OpenSteam,
    StopSteam,
    /// A game's start, by the Windows path of its program
    Play(String),
}

/// A request in progress: when it began, and the message that the window showed then.
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub request: Request,
    since: Instant,
    message: Option<String>,
}

/// Where a request stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    Running,
    NoAnswer,
    Done,
}

impl Pending {
    pub fn new(request: Request, model: &Model, now: Instant) -> Self {
        Self { request, since: now, message: model.message.clone() }
    }

    /// Where the request stands in `model` at `now`.
    pub fn progress(&self, model: &Model, now: Instant) -> Progress {
        if model.message.is_some() && model.message != self.message {
            return Progress::Done;
        }
        let done = match &self.request {
            Request::StartHost => model.running,
            Request::OpenSteam => model.steam_running,
            Request::StopSteam => !model.steam_running && model.game.is_none(),
            Request::Play(program) => model.game_program.as_deref().is_some_and(|running| same_program(running, program)),
        };
        if done {
            Progress::Done
        } else if now.saturating_duration_since(self.since) > ANSWER_TIME {
            Progress::NoAnswer
        } else {
            Progress::Running
        }
    }

    /// The words of the button while the request runs.
    pub fn running_label(&self) -> &'static str {
        match self.request {
            Request::StartHost | Request::Play(_) => "Starting...",
            Request::OpenSteam => "Opening Steam...",
            Request::StopSteam => "Stopping...",
        }
    }
}

/// Whether two names of a program name the same file. The runtime may name a game by its file name
/// alone, and Windows does not tell the case of letters apart.
pub fn same_program(a: &str, b: &str) -> bool {
    let file = |path: &str| path.rsplit(['\\', '/']).next().unwrap_or(path).to_ascii_lowercase();
    file(a) == file(b)
}

/// The label of the button of `request`: its own words, or what the request does while it runs.
pub fn label(pending: Option<&Pending>, model: &Model, request: &Request, own: &str) -> String {
    match pending.filter(|pending| &pending.request == request).map(|pending| (pending, pending.progress(model, Instant::now()))) {
        Some((pending, Progress::Running)) => pending.running_label().into(),
        Some((_, Progress::NoAnswer)) => format!("{own}: no answer"),
        _ => own.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_game_s_start_runs_until_the_host_names_its_program_or_a_message_says_why_not() {
        let start = Instant::now();
        let mut model = Model::default();
        let play = Pending::new(Request::Play(r"C:\Games\Beat Saber\Beat Saber.exe".into()), &model, start);
        assert_eq!(play.progress(&model, start + Duration::from_secs(20)), Progress::Running);
        assert_eq!(play.running_label(), "Starting...");
        model.game_program = Some("beat saber.exe".into());
        assert_eq!(play.progress(&model, start), Progress::Done);
        model.game_program = Some("hlvr.exe".into());
        assert_eq!(play.progress(&model, start), Progress::Running);
        assert_eq!(play.progress(&model, start + ANSWER_TIME + Duration::from_secs(1)), Progress::NoAnswer);
        model.message = Some("Steam did not start".into());
        assert_eq!(play.progress(&model, start), Progress::Done);
    }

    #[test]
    fn a_message_from_before_the_press_answers_nothing() {
        let start = Instant::now();
        let mut model = Model { message: Some("an old message".into()), ..Model::default() };
        let host = Pending::new(Request::StartHost, &model, start);
        assert_eq!(host.progress(&model, start), Progress::Running);
        model.running = true;
        assert_eq!(host.progress(&model, start), Progress::Done);
        let steam = Pending::new(Request::OpenSteam, &model, start);
        assert_eq!(label(Some(&steam), &model, &Request::OpenSteam, "Open Steam"), "Opening Steam...");
        assert_eq!(label(Some(&steam), &model, &Request::StartHost, "Start"), "Start");
        model.steam_running = true;
        assert_eq!(label(Some(&steam), &model, &Request::OpenSteam, "Open Steam"), "Open Steam");
    }
}
