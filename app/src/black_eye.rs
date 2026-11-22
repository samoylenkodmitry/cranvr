//! A game whose picture stays black: the right eye alone, or both eyes. The host looks at the game's
//! pictures and sends a `black_eye` status line once for each game run, with the eye "right" or
//! "both" (host/src/eye_check.rs). The app answers in the same way for both. It answers with
//! the launch option "Each eye in its own pass": it turns the option on for the program, records
//! why, shows a notice and starts the game again. When cranvr turned that option on and the picture
//! is still black with it, the game draws both eyes into a texture of its own (a Unity game with the
//! OpenVR provider does), which the option cannot reach. The app then turns the option off and turns
//! on "Both eyes in one pass" (DXMT reports that a vertex shader may write the slice), and starts the
//! game again. When the picture is still black with that option, the app says so once and changes
//! nothing, so no run ends in a loop. When the user turned an option off by hand, or turned
//! "Each eye in its own pass" on by hand, the choice stays the user's: the app only says how to fix
//! the picture.
//!
//! The option is data of the program's launch options, as the Play tab shows it. Nothing here
//! depends on which game it is.

use crate::{launch::GameOptions, steam::Game};
use std::{collections::BTreeMap, time::Duration};

/// The reason that the Options panel shows beside the option.
pub const REASON: &str = "cranvr turned this on because the picture stayed black.";

/// The reason that the Options panel shows beside "Both eyes in one pass".
pub const REASON_ONE_PASS: &str = "cranvr turned this on because the picture stayed black with \"Each eye in its own pass\".";

/// How long the notice stays in the window and in the console in VR.
pub const NOTICE_TIME: Duration = Duration::from_secs(120);

/// What a report changed, and what the app does next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The option was off. It is on now, with its reason. The game starts again.
    Restart { program: String },
    /// "Each eye in its own pass" was on (cranvr's choice) and the picture stays black. It is off now
    /// and "Both eyes in one pass" is on. The game starts again.
    Switch { program: String },
    /// The option was on already: the picture stays black with it. Nothing changes. `one_pass` tells
    /// which option: "Both eyes in one pass" (true) or "Each eye in its own pass".
    Leave { program: String, one_pass: bool },
    /// The user turned the option off by hand. Nothing changes.
    Declined { program: String },
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
    if options.eye_one_pass {
        return Outcome::Leave { program: key, one_pass: true };
    }
    if options.eye_per_pass {
        // Only cranvr's own choice gives way to the other option: a user's choice stays
        if !options.eye_per_pass_reason.is_empty() && !options.eye_one_pass_declined {
            options.eye_per_pass = false;
            options.eye_per_pass_reason.clear();
            options.eye_one_pass = true;
            options.eye_one_pass_reason = REASON_ONE_PASS.to_owned();
            return Outcome::Switch { program: key };
        }
        return Outcome::Leave { program: key, one_pass: false };
    }
    if options.eye_per_pass_declined {
        return Outcome::Declined { program: key };
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

/// `eye` is the report's word: "both", else the right eye alone.
pub fn notice(outcome: &Outcome, name: &str, eye: &str) -> Notice {
    let name = if name.is_empty() { "the game" } else { name };
    let (what, short) = if eye == "both" {
        (format!("Both eyes of {name}"), "Both eyes")
    } else {
        (format!("The right eye of {name}"), "The right eye")
    };
    let (window, headset) = match outcome {
        Outcome::Restart { .. } => (
            format!("{what} stayed black. cranvr turned on \"Each eye in its own pass\". The game starts again."),
            format!("{short} stayed black. The game restarts."),
        ),
        Outcome::Switch { .. } => (
            format!(
                "{what} stayed black with \"Each eye in its own pass\". cranvr turned that off and turned on \"Both eyes in one pass\". The game starts again."
            ),
            "Still black. Another try. The game restarts.".to_owned(),
        ),
        Outcome::Leave { one_pass, .. } => (
            format!(
                "{what} stayed black again with \"{}\". cranvr leaves the game as it is.",
                if *one_pass { "Both eyes in one pass" } else { "Each eye in its own pass" }
            ),
            format!("{short} stayed black. No restart."),
        ),
        Outcome::Declined { .. } => (
            format!("{what} stayed black. Turn on \"Each eye in its own pass\" in its options in the Play tab to fix it."),
            format!("{short} stayed black. See the cranvr window."),
        ),
        Outcome::Unknown => (
            format!("{what} stayed black. Open the options of the game in the Play tab and turn on \"Each eye in its own pass\"."),
            format!("{short} stayed black. See the cranvr window."),
        ),
    };
    Notice { window, headset }
}

/// What the app does with one report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub notice: Notice,
    /// The program to start again: only when this report turned the option on
    pub restart: Option<String>,
}

/// Answers one report (`eye` is its word, "right" or "both"): changes the launch options as `apply`
/// says and words the notice. The caller saves the options and starts the program again.
pub fn respond(launch: &mut BTreeMap<String, GameOptions>, games: &[Game], program: Option<&str>, eye: &str) -> Response {
    let known: Vec<String> = games.iter().filter_map(crate::steam::program).collect();
    let outcome = apply(launch, &known, program);
    let (Outcome::Restart { program } | Outcome::Switch { program } | Outcome::Leave { program, .. } | Outcome::Declined { program }) = &outcome else {
        return Response { notice: notice(&outcome, "", eye), restart: None };
    };
    let notice = notice(&outcome, &name(games, program), eye);
    let restart = matches!(outcome, Outcome::Restart { .. } | Outcome::Switch { .. }).then(|| program.clone());
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
    fn a_second_report_switches_to_both_eyes_in_one_pass_and_a_third_leaves_the_game() {
        let mut launch = BTreeMap::new();
        assert!(matches!(apply(&mut launch, &[], Some(PROGRAM)), Outcome::Restart { .. }));
        // Still black with each eye in its own pass: the game owns a two-slice texture
        assert_eq!(apply(&mut launch, &[], Some(PROGRAM)), Outcome::Switch { program: PROGRAM.into() });
        let options = &launch[PROGRAM];
        assert!(!options.eye_per_pass && options.eye_per_pass_reason.is_empty());
        assert!(options.eye_one_pass);
        assert_eq!(options.eye_one_pass_reason, REASON_ONE_PASS);
        // The option reaches the game's environment, and the first one no longer does
        let environment = crate::launch::environment(&Default::default(), options);
        assert!(environment.iter().any(|(name, value)| name == "DXMT_RT_ARRAY_INDEX_FROM_ANY_SHADER" && value == "1"));
        assert!(!environment.iter().any(|(name, _)| name == "BASALT_MAX_ARRAY_SIZE"));
        // Still black with that too: nothing more changes, however often the report comes
        let after_second = launch.clone();
        for _ in 0..3 {
            assert_eq!(apply(&mut launch, &[], Some(PROGRAM)), Outcome::Leave { program: PROGRAM.into(), one_pass: true });
        }
        assert_eq!(launch, after_second);
    }

    #[test]
    fn a_user_who_turned_one_pass_off_keeps_each_eye_in_its_own_pass() {
        let mut launch = BTreeMap::new();
        assert!(matches!(apply(&mut launch, &[], Some(PROGRAM)), Outcome::Restart { .. }));
        launch.get_mut(PROGRAM).unwrap().eye_one_pass_declined = true;
        assert_eq!(apply(&mut launch, &[], Some(PROGRAM)), Outcome::Leave { program: PROGRAM.into(), one_pass: false });
        assert!(launch[PROGRAM].eye_per_pass && !launch[PROGRAM].eye_one_pass);
    }

    #[test]
    fn an_option_that_the_user_turned_on_counts_as_on() {
        let mut launch = BTreeMap::new();
        launch.insert(PROGRAM.to_owned(), GameOptions { eye_per_pass: true, ..Default::default() });
        assert_eq!(apply(&mut launch, &[], Some(PROGRAM)), Outcome::Leave { program: PROGRAM.into(), one_pass: false });
        // The user's own choice has no reason from cranvr, and it stays
        assert_eq!(launch[PROGRAM].eye_per_pass_reason, "");
        assert!(launch[PROGRAM].eye_per_pass && !launch[PROGRAM].eye_one_pass);
    }

    #[test]
    fn a_program_whose_option_the_user_turned_off_keeps_it_off() {
        let mut launch = BTreeMap::new();
        launch.insert(PROGRAM.to_owned(), GameOptions { eye_per_pass_declined: true, ..Default::default() });
        let response = respond(&mut launch, &[], Some(PROGRAM), "right");
        assert_eq!(response.restart, None);
        assert!(!launch[PROGRAM].eye_per_pass);
        assert!(response.notice.window.contains("Turn on"));
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
        assert_eq!(apply(&mut launch, &[], Some("c:\\games\\some game\\GAME.EXE")), Outcome::Switch { program: listed[0].clone() });
    }

    #[test]
    fn the_first_report_restarts_the_game_and_later_reports_do_not() {
        let game = Game { app_id: "1".into(), name: "Some Game".into(), exe: Some(PathBuf::from("/Users/x/game.exe")), ..Default::default() };
        let program = "Z:\\Users\\x\\game.exe";
        let mut launch = BTreeMap::new();
        let first = respond(&mut launch, std::slice::from_ref(&game), Some(program), "right");
        assert_eq!(first.restart.as_deref(), Some(program));
        assert!(first.notice.window.contains("Some Game") && first.notice.window.contains("starts again"));
        assert_eq!(launch[program].eye_per_pass_reason, REASON);
        // The game starts again with the option and is still black: the app tries the other option once
        let second = respond(&mut launch, std::slice::from_ref(&game), Some(program), "right");
        assert_eq!(second.restart.as_deref(), Some(program));
        assert!(second.notice.window.contains("Both eyes in one pass") && second.notice.window.contains("Some Game"));
        // Still black with that too: the app leaves it
        for _ in 0..3 {
            let later = respond(&mut launch, std::slice::from_ref(&game), Some(program), "right");
            assert_eq!(later.restart, None);
            assert!(later.notice.window.contains("black again") && later.notice.window.contains("Some Game"));
        }
        assert!(launch[program].eye_one_pass && !launch[program].eye_per_pass);
        // A report without a program restarts nothing either
        assert_eq!(respond(&mut launch, &[game], None, "right").restart, None);
    }

    #[test]
    fn the_notice_says_what_happens() {
        let restart = notice(&Outcome::Restart { program: PROGRAM.into() }, "Some Game", "right");
        assert!(restart.window.contains("The right eye of Some Game") && restart.window.contains("starts again"));
        let leave = notice(&Outcome::Leave { program: PROGRAM.into(), one_pass: false }, "Some Game", "right");
        assert!(leave.window.contains("black again") && leave.headset.contains("No restart"));
        let switched = notice(&Outcome::Switch { program: PROGRAM.into() }, "Some Game", "right");
        // Both eyes black: the words name both eyes
        let both = notice(&Outcome::Restart { program: PROGRAM.into() }, "Some Game", "both");
        assert!(both.window.starts_with("Both eyes of Some Game stayed black.") && both.headset.starts_with("Both eyes stayed black"));
        let unknown = notice(&Outcome::Unknown, "", "both");
        assert!(unknown.window.starts_with("Both eyes of the game stayed black."));
        // The console in VR has one line of 45 characters or so
        for text in [&restart.headset, &leave.headset, &switched.headset, &both.headset, &unknown.headset, &notice(&Outcome::Unknown, "x", "right").headset] {
            assert!(text.len() <= 50, "{text}");
        }
    }

    #[test]
    fn the_name_is_the_listed_game_or_the_program_file() {
        let game = Game { app_id: "1".into(), name: "Some Game".into(), exe: Some(PathBuf::from("/Users/x/game.exe")), ..Default::default() };
        assert_eq!(name(std::slice::from_ref(&game), "Z:\\Users\\x\\game.exe"), "Some Game");
        assert_eq!(name(&[game], "C:\\Other\\Thing.exe"), "Thing.exe");
        assert_eq!(name(&[], "Thing.exe"), "Thing.exe");
    }
}
