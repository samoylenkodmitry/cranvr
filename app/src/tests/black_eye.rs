use super::*;
use std::path::PathBuf;

const PROGRAM: &str = "C:\\Games\\Some Game\\game.exe";
const OPENXR: EyeImages = EyeImages::Swapchains;
const OPENVR: EyeImages = EyeImages::GameTexture;

fn environment(options: &GameOptions) -> Vec<(String, String)> {
    crate::launch::environment(&Default::default(), options)
}

#[test]
fn a_report_turns_the_option_on_with_its_reason_and_asks_for_a_restart() {
    let mut launch = BTreeMap::new();
    let outcome = apply(&mut launch, &[], Some(PROGRAM), OPENXR);
    assert_eq!(outcome, Outcome::Restart { program: PROGRAM.into(), pass: Pass::Each });
    let options = &launch[PROGRAM];
    assert!(options.eye_per_pass);
    assert_eq!(options.eye_per_pass_reason, REASON);
    // The option reaches the game's environment
    assert!(environment(options).iter().any(|(name, value)| name == "BASALT_MAX_ARRAY_SIZE" && value == "1"));
}

#[test]
fn a_report_keeps_the_other_options_of_the_program() {
    let mut launch = BTreeMap::new();
    launch.insert(PROGRAM.to_owned(), GameOptions { args: "-fast".into(), env: "A=1".into(), ..Default::default() });
    assert_eq!(apply(&mut launch, &[], Some(PROGRAM), OPENXR), Outcome::Restart { program: PROGRAM.into(), pass: Pass::Each });
    let options = &launch[PROGRAM];
    assert_eq!((options.args.as_str(), options.env.as_str(), options.eye_per_pass), ("-fast", "A=1", true));
}

#[test]
fn a_second_report_switches_to_both_eyes_in_one_pass_and_a_third_leaves_the_game() {
    let mut launch = BTreeMap::new();
    assert!(matches!(apply(&mut launch, &[], Some(PROGRAM), OPENXR), Outcome::Restart { pass: Pass::Each, .. }));
    // Still black with each eye in its own pass: the game owns a two-slice texture
    assert_eq!(apply(&mut launch, &[], Some(PROGRAM), OPENXR), Outcome::Switch { program: PROGRAM.into() });
    let options = &launch[PROGRAM];
    assert!(!options.eye_per_pass && options.eye_per_pass_reason.is_empty());
    assert!(options.eye_one_pass);
    assert_eq!(options.eye_one_pass_reason, REASON_ONE_PASS);
    // The option reaches the game's environment, and the first one no longer does
    let vars = environment(options);
    assert!(vars.iter().any(|(name, value)| name == "DXMT_RT_ARRAY_INDEX_FROM_ANY_SHADER" && value == "1"));
    assert!(!vars.iter().any(|(name, _)| name == "BASALT_MAX_ARRAY_SIZE"));
    // Still black with that too: nothing more changes, however often the report comes
    let after_second = launch.clone();
    for _ in 0..3 {
        assert_eq!(apply(&mut launch, &[], Some(PROGRAM), OPENXR), Outcome::Leave { program: PROGRAM.into(), pass: Pass::Both });
    }
    assert_eq!(launch, after_second);
}

#[test]
fn an_openvr_game_gets_both_eyes_in_one_pass_at_its_first_report() {
    let mut launch = BTreeMap::new();
    assert_eq!(apply(&mut launch, &[], Some(PROGRAM), OPENVR), Outcome::Restart { program: PROGRAM.into(), pass: Pass::Both });
    let options = &launch[PROGRAM];
    assert!(options.eye_one_pass && !options.eye_per_pass);
    assert_eq!(options.eye_one_pass_reason, REASON_OPENVR);
    let vars = environment(options);
    assert!(vars.iter().any(|(name, value)| name == "DXMT_RT_ARRAY_INDEX_FROM_ANY_SHADER" && value == "1"));
    assert!(!vars.iter().any(|(name, _)| name == "BASALT_MAX_ARRAY_SIZE"));
    // Still black: nothing more changes
    let after_first = launch.clone();
    assert_eq!(apply(&mut launch, &[], Some(PROGRAM), OPENVR), Outcome::Leave { program: PROGRAM.into(), pass: Pass::Both });
    assert_eq!(launch, after_first);
}

#[test]
fn an_openvr_game_with_cranvr_s_earlier_choice_switches_and_a_user_s_choice_stays() {
    // An earlier run counted as OpenXR (a start that cranvr did not watch) and turned on the first option
    let mut launch = BTreeMap::new();
    assert!(matches!(apply(&mut launch, &[], Some(PROGRAM), OPENXR), Outcome::Restart { pass: Pass::Each, .. }));
    assert_eq!(apply(&mut launch, &[], Some(PROGRAM), OPENVR), Outcome::Switch { program: PROGRAM.into() });
    let mut launch = BTreeMap::new();
    launch.insert(PROGRAM.to_owned(), GameOptions { eye_per_pass: true, ..Default::default() });
    assert_eq!(apply(&mut launch, &[], Some(PROGRAM), OPENVR), Outcome::Leave { program: PROGRAM.into(), pass: Pass::Each });
    assert!(launch[PROGRAM].eye_per_pass && !launch[PROGRAM].eye_one_pass);
}

#[test]
fn an_openvr_game_whose_one_pass_the_user_turned_off_keeps_it_off() {
    let mut launch = BTreeMap::new();
    launch.insert(PROGRAM.to_owned(), GameOptions { eye_one_pass_declined: true, ..Default::default() });
    let response = respond(&mut launch, &[], Some(PROGRAM), "right", OPENVR);
    assert_eq!(response.restart, None);
    assert!(!launch[PROGRAM].eye_one_pass && !launch[PROGRAM].eye_per_pass);
    assert!(response.notice.window.contains("Turn on \"Both eyes in one pass\""), "{}", response.notice.window);
    // The other option's refusal does not matter to an OpenVR game
    let mut launch = BTreeMap::new();
    launch.insert(PROGRAM.to_owned(), GameOptions { eye_per_pass_declined: true, ..Default::default() });
    assert_eq!(apply(&mut launch, &[], Some(PROGRAM), OPENVR), Outcome::Restart { program: PROGRAM.into(), pass: Pass::Both });
}

#[test]
fn only_the_steamvr_driver_s_name_or_opencomposite_s_log_make_an_openvr_game() {
    assert_eq!(EyeImages::of(Some(crate::openvr::STEAMVR_DRIVER), false), OPENVR);
    assert_eq!(EyeImages::of(Some("Apple M2 Max"), true), OPENVR);
    // An OpenXR game whose options chose SteamVR or OpenComposite: the host names its GPU, and
    // OpenComposite's log does not name it
    assert_eq!(EyeImages::of(Some("Apple M2 Max"), false), OPENXR);
    assert_eq!(EyeImages::of(None, false), OPENXR);
}

#[test]
fn a_user_who_turned_one_pass_off_keeps_each_eye_in_its_own_pass() {
    let mut launch = BTreeMap::new();
    assert!(matches!(apply(&mut launch, &[], Some(PROGRAM), OPENXR), Outcome::Restart { .. }));
    launch.get_mut(PROGRAM).unwrap().eye_one_pass_declined = true;
    assert_eq!(apply(&mut launch, &[], Some(PROGRAM), OPENXR), Outcome::Leave { program: PROGRAM.into(), pass: Pass::Each });
    assert!(launch[PROGRAM].eye_per_pass && !launch[PROGRAM].eye_one_pass);
}

#[test]
fn an_option_that_the_user_turned_on_counts_as_on() {
    let mut launch = BTreeMap::new();
    launch.insert(PROGRAM.to_owned(), GameOptions { eye_per_pass: true, ..Default::default() });
    assert_eq!(apply(&mut launch, &[], Some(PROGRAM), OPENXR), Outcome::Leave { program: PROGRAM.into(), pass: Pass::Each });
    // The user's own choice has no reason from cranvr, and it stays
    assert_eq!(launch[PROGRAM].eye_per_pass_reason, "");
    assert!(launch[PROGRAM].eye_per_pass && !launch[PROGRAM].eye_one_pass);
}

#[test]
fn a_program_whose_option_the_user_turned_off_keeps_it_off() {
    let mut launch = BTreeMap::new();
    launch.insert(PROGRAM.to_owned(), GameOptions { eye_per_pass_declined: true, ..Default::default() });
    let response = respond(&mut launch, &[], Some(PROGRAM), "right", OPENXR);
    assert_eq!(response.restart, None);
    assert!(!launch[PROGRAM].eye_per_pass);
    assert!(response.notice.window.contains("Turn on \"Each eye in its own pass\""));
}

#[test]
fn a_report_without_a_program_changes_nothing() {
    let mut launch = BTreeMap::new();
    assert_eq!(apply(&mut launch, &[], None, OPENXR), Outcome::Unknown { pass: Pass::Each });
    assert_eq!(apply(&mut launch, &[], Some(""), OPENVR), Outcome::Unknown { pass: Pass::Both });
    assert!(launch.is_empty());
}

#[test]
fn a_program_takes_the_spelling_of_the_listed_game_or_of_its_entry() {
    let listed = vec!["C:\\Games\\Some Game\\Game.exe".to_owned()];
    let mut launch = BTreeMap::new();
    assert_eq!(apply(&mut launch, &listed, Some(PROGRAM), OPENXR), Outcome::Restart { program: listed[0].clone(), pass: Pass::Each });
    assert!(launch.contains_key(&listed[0]) && launch.len() == 1);
    // The entry exists now: the same program in a third spelling finds it
    assert_eq!(apply(&mut launch, &[], Some("c:\\games\\some game\\GAME.EXE"), OPENXR), Outcome::Switch { program: listed[0].clone() });
}

#[test]
fn a_program_with_entries_in_two_spellings_takes_its_own() {
    // "C:\\Games\\Some Game\\Game.exe" sorts before PROGRAM, so a match by case alone took it
    let other = "C:\\Games\\Some Game\\Game.exe".to_owned();
    let mut launch = BTreeMap::from([(other.clone(), GameOptions::default()), (PROGRAM.to_owned(), GameOptions::default())]);
    assert_eq!(apply(&mut launch, &[], Some(PROGRAM), OPENXR), Outcome::Restart { program: PROGRAM.to_owned(), pass: Pass::Each });
    assert!(launch[PROGRAM].eye_per_pass);
    assert!(!launch[&other].eye_per_pass);
}

#[test]
fn the_first_report_restarts_the_game_and_later_reports_do_not() {
    let game = Game { app_id: "1".into(), name: "Some Game".into(), exe: Some(PathBuf::from("/Users/x/game.exe")), ..Default::default() };
    let program = "Z:\\Users\\x\\game.exe";
    let mut launch = BTreeMap::new();
    let first = respond(&mut launch, std::slice::from_ref(&game), Some(program), "right", OPENXR);
    assert_eq!(first.restart.as_deref(), Some(program));
    assert!(first.notice.window.contains("Some Game") && first.notice.window.contains("starts again"));
    assert_eq!(launch[program].eye_per_pass_reason, REASON);
    // The game starts again with the option and is still black: the app tries the other option once
    let second = respond(&mut launch, std::slice::from_ref(&game), Some(program), "right", OPENXR);
    assert_eq!(second.restart.as_deref(), Some(program));
    assert!(second.notice.window.contains("Both eyes in one pass") && second.notice.window.contains("Some Game"));
    // Still black with that too: the app leaves it
    for _ in 0..3 {
        let later = respond(&mut launch, std::slice::from_ref(&game), Some(program), "right", OPENXR);
        assert_eq!(later.restart, None);
        assert!(later.notice.window.contains("black again") && later.notice.window.contains("Some Game"));
    }
    assert!(launch[program].eye_one_pass && !launch[program].eye_per_pass);
    // A report without a program restarts nothing either
    assert_eq!(respond(&mut launch, &[game], None, "right", OPENXR).restart, None);
}

#[test]
fn an_openvr_game_restarts_once_with_both_eyes_in_one_pass() {
    let game = Game { app_id: "1".into(), name: "Some Game".into(), exe: Some(PathBuf::from("/Users/x/game.exe")), ..Default::default() };
    let program = "Z:\\Users\\x\\game.exe";
    let mut launch = BTreeMap::new();
    let first = respond(&mut launch, std::slice::from_ref(&game), Some(program), "right", OPENVR);
    assert_eq!(first.restart.as_deref(), Some(program));
    assert!(first.notice.window.contains("Some Game") && first.notice.window.contains("\"Both eyes in one pass\". The game starts again."));
    assert!(!first.notice.window.contains("Each eye"));
    let second = respond(&mut launch, std::slice::from_ref(&game), Some(program), "right", OPENVR);
    assert_eq!(second.restart, None);
    assert!(second.notice.window.contains("black again with \"Both eyes in one pass\""));
}

#[test]
fn the_notice_says_what_happens() {
    let restart = notice(&Outcome::Restart { program: PROGRAM.into(), pass: Pass::Each }, "Some Game", "right");
    assert!(restart.window.contains("The right eye of Some Game") && restart.window.contains("starts again"));
    let openvr = notice(&Outcome::Restart { program: PROGRAM.into(), pass: Pass::Both }, "Some Game", "right");
    assert!(openvr.window.contains("OpenVR") && openvr.window.contains("Both eyes in one pass"));
    let leave = notice(&Outcome::Leave { program: PROGRAM.into(), pass: Pass::Each }, "Some Game", "right");
    assert!(leave.window.contains("black again") && leave.headset.contains("No restart"));
    let switched = notice(&Outcome::Switch { program: PROGRAM.into() }, "Some Game", "right");
    // Both eyes black: the words name both eyes
    let both = notice(&Outcome::Restart { program: PROGRAM.into(), pass: Pass::Each }, "Some Game", "both");
    assert!(both.window.starts_with("Both eyes of Some Game stayed black.") && both.headset.starts_with("Both eyes stayed black"));
    let unknown = notice(&Outcome::Unknown { pass: Pass::Each }, "", "both");
    assert!(unknown.window.starts_with("Both eyes of the game stayed black."));
    let unknown_openvr = notice(&Outcome::Unknown { pass: Pass::Both }, "", "right");
    assert!(unknown_openvr.window.ends_with("turn on \"Both eyes in one pass\"."));
    let declined = notice(&Outcome::Declined { program: PROGRAM.into(), pass: Pass::Both }, "x", "both");
    // The console in VR has one line of 45 characters or so
    for text in [&restart.headset, &openvr.headset, &leave.headset, &switched.headset, &both.headset, &unknown.headset, &unknown_openvr.headset, &declined.headset] {
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
