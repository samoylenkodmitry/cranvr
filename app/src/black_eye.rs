//! A game whose right eye stays black. The host looks at the game's pictures and sends a
//! `black_eye` status line once for each game run (host/src/eye_check.rs). The app answers with
//! the launch option "Each eye in its own pass": it turns the option on for the program, records
//! why, shows a notice and starts the game again. When the option is on already, the picture is
//! still black with it. The app then says so once and changes nothing, so no run ends in a loop.
//!
//! The option is data of the program's launch options, as the Play tab shows it. Nothing here
//! depends on which game it is.

use crate::{launch::GameOptions, steam::Game};
use std::{collections::BTreeMap, time::Duration};

/// The reason that the Options panel shows beside the option.
pub const REASON: &str = "cranvr turned this on because the right eye stayed black.";

/// How long the notice stays in the window and in the console in VR.
pub const NOTICE_TIME: Duration = Duration::from_secs(120);

/// What a report changed, and what the app does next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The option was off. It is on now, with its reason. The game starts again.
    Restart { program: String },
    /// The option was on already: the right eye stays black with it. Nothing changes.
    Leave { program: String },
    /// The host named no program, so no option can change.
    Unknown,
}

/// Applies a report to the launch options. `known` holds the programs that the Play tab lists:
/// Windows paths do not differ by case, so a program takes the spelling of an existing entry or of
/// a listed game, and its options show in the Play tab.
pub fn apply(launch: &mut BTreeMap<String, GameOptions>, known: &[String], program: Option<&str>) -> Outcome {
    let Some(program) = program.filter(|program| !program.is_empty()) else { return Outcome::Unknown };
    let key = launch.keys().chain(known).find(|key| key.eq_ignore_ascii_case(program)).cloned().unwrap_or_else(|| program.to_owned());
    let options = launch.entry(key.clone()).or_default();
    if options.eye_per_pass {
        return Outcome::Leave { program: key };
    }
    options.eye_per_pass = true;
    options.eye_per_pass_reason = REASON.to_owned();
    Outcome::Restart { program: key }
}

/// The two texts of a notice: one for the window and a short one for the console in VR, which
/// has one line of text for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub window: String,
    pub headset: String,
}

pub fn notice(outcome: &Outcome, name: &str) -> Notice {
    let (window, headset) = match outcome {
        Outcome::Restart { .. } => (
            format!("The right eye of {name} stayed black. cranvr turned on \"Each eye in its own pass\". The game starts again."),
            "The right eye stayed black. The game restarts.",
        ),
        Outcome::Leave { .. } => (
            format!("The right eye of {name} is still black with \"Each eye in its own pass\". cranvr leaves the game as it is."),
            "The right eye is still black. No restart.",
        ),
        Outcome::Unknown => (
            "The right eye of the game stays black. Open the options of the game in the Play tab and turn on \"Each eye in its own pass\"."
                .to_owned(),
            "The right eye stays black. See the cranvr window.",
        ),
    };
    Notice { window, headset: headset.to_owned() }
}

/// What the app does with one report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub notice: Notice,
    /// The program to start again: only when this report turned the option on
    pub restart: Option<String>,
}

/// Answers one report: changes the launch options as `apply` says and words the notice. The
/// caller saves the options and starts the program again.
pub fn respond(launch: &mut BTreeMap<String, GameOptions>, games: &[Game], program: Option<&str>) -> Response {
    let known: Vec<String> = games.iter().filter_map(crate::steam::program).collect();
    let outcome = apply(launch, &known, program);
    let (Outcome::Restart { program } | Outcome::Leave { program }) = &outcome else {
        return Response { notice: notice(&outcome, ""), restart: None };
    };
    let notice = notice(&outcome, &name(games, program));
    let restart = matches!(outcome, Outcome::Restart { .. }).then(|| program.clone());
    Response { notice, restart }
}

/// The name that a notice shows for a program: the game's name in Steam, else the program's file.
pub fn name(games: &[Game], program: &str) -> String {
    let listed = games.iter().find(|game| crate::steam::program(game).is_some_and(|own| own.eq_ignore_ascii_case(program)));
    match listed {
        Some(game) => game.name.clone(),
        None => program.rsplit('\\').next().unwrap_or(program).to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const PROGRAM: &str = "C:\\Games\\Some Game\\game.exe";

    #[test]
    fn a_report_turns_the_option_on_with_its_reason_and_asks_for_a_restart() {
        let mut launch = BTreeMap::new();
        let outcome = apply(&mut launch, &[], Some(PROGRAM));
        assert_eq!(outcome, Outcome::Restart { program: PROGRAM.into() });
        let options = &launch[PROGRAM];
        assert!(options.eye_per_pass);
        assert_eq!(options.eye_per_pass_reason, REASON);
        // The option reaches the game's environment
        assert!(crate::launch::environment(&Default::default(), options).iter().any(|(name, value)| name == "BASALT_MAX_ARRAY_SIZE" && value == "1"));
    }

    #[test]
    fn a_report_keeps_the_other_options_of_the_program() {
        let mut launch = BTreeMap::new();
        launch.insert(PROGRAM.to_owned(), GameOptions { args: "-fast".into(), env: "A=1".into(), ..Default::default() });
        assert_eq!(apply(&mut launch, &[], Some(PROGRAM)), Outcome::Restart { program: PROGRAM.into() });
        let options = &launch[PROGRAM];
        assert_eq!((options.args.as_str(), options.env.as_str(), options.eye_per_pass), ("-fast", "A=1", true));
    }

    #[test]
    fn a_second_report_with_the_option_on_restarts_nothing_and_changes_nothing() {
        let mut launch = BTreeMap::new();
        assert!(matches!(apply(&mut launch, &[], Some(PROGRAM)), Outcome::Restart { .. }));
        let after_first = launch.clone();
        for _ in 0..3 {
            assert_eq!(apply(&mut launch, &[], Some(PROGRAM)), Outcome::Leave { program: PROGRAM.into() });
        }
        assert_eq!(launch, after_first);
    }

    #[test]
    fn an_option_that_the_user_turned_on_counts_as_on() {
        let mut launch = BTreeMap::new();
        launch.insert(PROGRAM.to_owned(), GameOptions { eye_per_pass: true, ..Default::default() });
        assert_eq!(apply(&mut launch, &[], Some(PROGRAM)), Outcome::Leave { program: PROGRAM.into() });
        // The user's own choice has no reason from cranvr
        assert_eq!(launch[PROGRAM].eye_per_pass_reason, "");
    }

    #[test]
    fn a_report_without_a_program_changes_nothing() {
        let mut launch = BTreeMap::new();
        assert_eq!(apply(&mut launch, &[], None), Outcome::Unknown);
        assert_eq!(apply(&mut launch, &[], Some("")), Outcome::Unknown);
        assert!(launch.is_empty());
    }

    #[test]
    fn a_program_takes_the_spelling_of_the_listed_game_or_of_its_entry() {
        let listed = vec!["C:\\Games\\Some Game\\Game.exe".to_owned()];
        let mut launch = BTreeMap::new();
        assert_eq!(apply(&mut launch, &listed, Some(PROGRAM)), Outcome::Restart { program: listed[0].clone() });
        assert!(launch.contains_key(&listed[0]) && launch.len() == 1);
        // The entry exists now: the same program in a third spelling finds it
        assert_eq!(apply(&mut launch, &[], Some("c:\\games\\some game\\GAME.EXE")), Outcome::Leave { program: listed[0].clone() });
    }

    #[test]
    fn the_first_report_restarts_the_game_and_later_reports_do_not() {
        let game = Game { app_id: "1".into(), name: "Some Game".into(), exe: Some(PathBuf::from("/Users/x/game.exe")) };
        let program = "Z:\\Users\\x\\game.exe";
        let mut launch = BTreeMap::new();
        let first = respond(&mut launch, std::slice::from_ref(&game), Some(program));
        assert_eq!(first.restart.as_deref(), Some(program));
        assert!(first.notice.window.contains("Some Game") && first.notice.window.contains("starts again"));
        assert_eq!(launch[program].eye_per_pass_reason, REASON);
        // The game starts again with the option and is still black: the app leaves it
        for _ in 0..3 {
            let later = respond(&mut launch, std::slice::from_ref(&game), Some(program));
            assert_eq!(later.restart, None);
            assert!(later.notice.window.contains("still black") && later.notice.window.contains("Some Game"));
        }
        assert!(launch[program].eye_per_pass);
        // A report without a program restarts nothing either
        assert_eq!(respond(&mut launch, &[game], None).restart, None);
    }

    #[test]
    fn the_notice_says_what_happens() {
        let restart = notice(&Outcome::Restart { program: PROGRAM.into() }, "Some Game");
        assert!(restart.window.contains("Some Game") && restart.window.contains("starts again"));
        let leave = notice(&Outcome::Leave { program: PROGRAM.into() }, "Some Game");
        assert!(leave.window.contains("still black") && leave.headset.contains("No restart"));
        // The console in VR has one line of 45 characters or so
        for text in [&restart.headset, &leave.headset, &notice(&Outcome::Unknown, "x").headset] {
            assert!(text.len() <= 50, "{text}");
        }
    }

    #[test]
    fn the_name_is_the_listed_game_or_the_program_file() {
        let game = Game { app_id: "1".into(), name: "Some Game".into(), exe: Some(PathBuf::from("/Users/x/game.exe")) };
        assert_eq!(name(std::slice::from_ref(&game), "Z:\\Users\\x\\game.exe"), "Some Game");
        assert_eq!(name(&[game], "C:\\Other\\Thing.exe"), "Thing.exe");
        assert_eq!(name(&[], "Thing.exe"), "Thing.exe");
    }
}
