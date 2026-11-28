use super::*;
use std::path::PathBuf;

const PROGRAM: &str = r"C:\Games\The Lab\TheLab\win64\TheLab.exe";

#[test]
fn a_game_that_follows_the_setting_goes_to_steamvr_with_its_reason() {
    let mut launch = BTreeMap::new();
    assert_eq!(apply(&mut launch, &[], PROGRAM, true), Outcome::Switched { program: PROGRAM.into() });
    assert_eq!(launch[PROGRAM].openvr, Some(OpenVr::SteamVr));
    assert_eq!(launch[PROGRAM].openvr_reason, REASON);
    assert!(!launch[PROGRAM].is_default());
    // A second verdict cannot come from SteamVR, and the game's choice is set now
    assert_eq!(apply(&mut launch, &[], PROGRAM, true), Outcome::Kept { program: PROGRAM.into() });
}

#[test]
fn a_choice_of_the_user_stays_and_no_steamvr_changes_nothing() {
    let mut launch = BTreeMap::new();
    launch.insert(PROGRAM.to_owned(), GameOptions { openvr: Some(OpenVr::OpenComposite), ..Default::default() });
    assert_eq!(apply(&mut launch, &[], PROGRAM, true), Outcome::Kept { program: PROGRAM.into() });
    assert_eq!(launch[PROGRAM].openvr, Some(OpenVr::OpenComposite));
    let mut launch = BTreeMap::new();
    assert_eq!(apply(&mut launch, &[], PROGRAM, false), Outcome::NoSteamVr { program: PROGRAM.into() });
    assert!(launch.is_empty());
}

#[test]
fn the_switch_restarts_the_game_and_the_notice_names_it() {
    let game = Game { app_id: "450390".into(), name: "The Lab".into(), exe: Some(PathBuf::from("/Users/x/TheLab.exe")), ..Default::default() };
    let program = r"Z:\Users\x\TheLab.exe";
    let mut launch = BTreeMap::new();
    let response = respond(&mut launch, std::slice::from_ref(&game), &program.to_ascii_lowercase(), Nothing::Aborted, true);
    assert_eq!(response.restart.as_deref(), Some(program), "the listed spelling");
    assert!(response.notice.window.starts_with("OpenComposite stopped The Lab"), "{}", response.notice.window);
    assert!(response.notice.window.contains("SteamVR"));
    let again = respond(&mut launch, std::slice::from_ref(&game), program, Nothing::Ended, true);
    assert_eq!(again.restart, None);
    // A game that ended or closed its session by itself waits for its next start
    let mut launch = BTreeMap::new();
    let ended = respond(&mut launch, &[game], program, Nothing::Ended, true);
    assert_eq!(ended.restart, None);
    assert!(ended.notice.window.contains("next start"), "{}", ended.notice.window);
    assert_eq!(launch[program].openvr, Some(OpenVr::SteamVr));
    // The console in VR has one line of 45 characters or so
    for why in [Nothing::Aborted, Nothing::Closed, Nothing::Ended] {
        for outcome in [Outcome::Switched { program: "x".into() }, Outcome::NoSteamVr { program: "x".into() }, Outcome::Kept { program: "x".into() }] {
            assert!(notice(&outcome, "The Lab", why).headset.len() <= 50);
        }
    }
}

#[test]
fn a_program_with_entries_in_two_spellings_takes_its_own() {
    // "C:\\Games\\The Lab\\TheLab\\Win64\\TheLab.exe" sorts before PROGRAM, so a match by case alone took it
    let other = r"C:\Games\The Lab\TheLab\Win64\TheLab.exe".to_owned();
    let mut launch = BTreeMap::from([(other.clone(), GameOptions::default()), (PROGRAM.to_owned(), GameOptions::default())]);
    assert_eq!(apply(&mut launch, &[], PROGRAM, true), Outcome::Switched { program: PROGRAM.into() });
    assert_eq!(launch[PROGRAM].openvr, Some(OpenVr::SteamVr));
    assert_eq!(launch[&other].openvr, None);
    // A listed program in the exact spelling wins over an entry in another case too
    let mut launch = BTreeMap::from([(other.clone(), GameOptions::default())]);
    assert_eq!(apply(&mut launch, &[PROGRAM.to_owned()], PROGRAM, true), Outcome::Switched { program: PROGRAM.into() });
    assert_eq!(launch[&other].openvr, None);
}
