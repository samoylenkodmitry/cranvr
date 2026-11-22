use super::*;
use crate::{
    backend::{vr_menu, Model, Settings},
    setup::Setup,
};
use cranvr_common::{
    controls::{Action, Control, Controls, Hand, ModelsInGame},
    menu::{Item, Page, Request},
};
use serde_json::Value;
use std::path::PathBuf;

fn lab() -> Game {
    Game { app_id: "450390".into(), name: "The Lab".into(), exe: Some(PathBuf::from("/Users/x/The Lab/TheLab.exe")), ..Default::default() }
}

fn added() -> Game {
    Game { name: "Plain".into(), exe: Some(PathBuf::from("/Users/x/Plain/plain.exe")), ..Default::default() }
}

fn model_with(games: Vec<Game>) -> Model {
    Model { games, ..Default::default() }
}

fn page<'a>(pages: &'a [Page], id: &str) -> &'a Page {
    pages.iter().find(|page| page.id == id).unwrap_or_else(|| panic!("no page {id}"))
}

#[test]
fn the_console_lists_the_play_page_first_then_steam_and_the_stream_and_a_page_for_each_game() {
    let settings = Settings::default();
    let model = model_with(vec![lab(), added()]);
    let pages = pages(&settings, &model);
    let ids: Vec<&str> = pages.iter().map(|page| page.id.as_str()).collect();
    let lab_program = steam::program(&lab()).unwrap();
    assert_eq!(ids[..3], [PLAY, STEAM, STREAM]);
    assert!(ids.contains(&game_page(&lab_program).as_str()));
    // A game's page is reached from its row, so the menus do not list it
    assert!(pages.iter().filter(|page| page.id.starts_with(GAME_PAGE)).all(|page| !page.listed));
    // The row of a game has Play and Options, and its detail is the game's VR system
    let row = page(&pages, PLAY).find("game.play", Some(&lab_program)).unwrap();
    let Item::Buttons(row) = row else { panic!("a row of buttons") };
    assert_eq!(row.title, "The Lab");
    assert_eq!(row.detail, "OpenComposite");
    assert_eq!(row.buttons.iter().map(|b| b.label.as_str()).collect::<Vec<_>>(), ["Play", "Options"]);
}

#[test]
fn every_stream_setting_shows_its_value_and_a_request_changes_it_in_the_settings() {
    let mut settings = Settings::default();
    let games = [lab()];
    let model = model_with(games.to_vec());
    let stream = page(&pages(&settings, &model), STREAM).clone();
    for (id, value) in [("fps", "120"), ("mbps", "100"), ("foveation_level", "medium"), ("game_scale", "70"), ("game_msaa_limit", "2"), ("game_audio", "on"), ("wine.openvr", "opencomposite")] {
        assert_eq!(stream.find(id, None).and_then(Item::value), Some(value), "{id}");
    }
    // A change that the host reads at its start restarts it
    let out = apply(&mut settings, &games, &Request::set("foveation_level", "high"));
    assert_eq!((out.changed, out.restart), (true, true));
    assert_eq!(settings.foveation_level, FoveationLevel::High);
    let out = apply(&mut settings, &games, &Request::set("game_scale", "85"));
    assert!(out.restart && (settings.game_scale - 0.85).abs() < 1e-6);
    // The anti-aliasing limit counts at a game's start, so the stream goes on
    let out = apply(&mut settings, &games, &Request::set("game_msaa_limit", "0"));
    assert_eq!((out.changed, out.restart, settings.game_msaa_limit), (true, false, 0));
    let out = apply(&mut settings, &games, &Request::set("hand_controllers", "off"));
    assert!(out.restart && !settings.hand_controllers);
    // The same value again changes nothing and restarts nothing
    let out = apply(&mut settings, &games, &Request::set("hand_controllers", "off"));
    assert_eq!((out.changed, out.restart), (false, false));
    // A value that the setting does not offer, and a setting that does not exist, change nothing
    let before = settings.clone();
    for request in [Request::set("fps", "100"), Request::set("game_audio", "maybe"), Request::set("volume", "200"), Request::run("format.disk")] {
        let out = apply(&mut settings, &games, &request);
        assert!(out.refused.is_some() && !out.changed, "{request:?}");
    }
    assert_eq!(settings, before);
}

#[test]
fn a_game_s_page_shows_and_sets_its_vr_system_and_its_eyes_and_cranvrs_reason() {
    let games = [lab(), added()];
    let program = steam::program(&lab()).unwrap();
    let mut settings = Settings::default();
    settings.launch.insert(program.clone(), GameOptions { openvr: Some(OpenVr::SteamVr), openvr_reason: "It drew nothing through OpenComposite.".into(), ..Default::default() });
    let model = model_with(games.to_vec());
    let pages = pages(&settings, &model);
    let game = page(&pages, &game_page(&program));
    assert_eq!(game.title, "The Lab");
    assert_eq!(game.find("game.openvr", Some(&program)).and_then(Item::value), Some("steamvr"));
    assert!(game.items.iter().any(|item| matches!(item, Item::Text { text, .. } if text == "It drew nothing through OpenComposite.")));
    // The row in the list names the game's own choice, and a game without one follows the setting
    let Some(Item::Buttons(row)) = page(&pages, PLAY).find("game.play", Some(&program)) else { panic!() };
    assert_eq!(row.detail, "SteamVR");
    // The headset sets The Lab back to the setting for every program: the choice is the user's now,
    // so cranvr's reason goes and the options leave no entry
    let out = apply(&mut settings, &games, &Request::set("game.openvr", "default").of(program.clone()));
    assert!(out.changed && !out.restart);
    assert!(!settings.launch.contains_key(&program), "{:?}", settings.launch);
    // The two eye options ask for opposite things
    apply(&mut settings, &games, &Request::set("game.eye_per_pass", "on").of(program.clone()));
    apply(&mut settings, &games, &Request::set("game.eye_one_pass", "on").of(program.clone()));
    let options = &settings.launch[&program];
    assert!(options.eye_one_pass && !options.eye_per_pass && !options.eye_one_pass_declined);
    // A word that is no VR system, and a game that is not installed, change nothing
    let before = settings.clone();
    assert!(apply(&mut settings, &games, &Request::set("game.openvr", "oculus").of(program.clone())).refused.is_some());
    assert!(apply(&mut settings, &games, &Request::set("game.openvr", "off").of("C:\\Gone\\gone.exe")).refused.is_some());
    assert_eq!(settings, before);
}

#[test]
fn play_starts_a_game_by_its_program_and_stop_ends_steam_and_games() {
    let games = [lab(), added()];
    let program = steam::program(&lab()).unwrap();
    let mut settings = Settings::default();
    let out = apply(&mut settings, &games, &Request::run("game.play").of(program.clone()));
    assert_eq!(out.effects, [Effect::Play(lab())]);
    assert_eq!(out.pending, Some(pending::Request::Play(program.clone())));
    assert!(!out.changed);
    let out = apply(&mut settings, &games, &Request::run("steam.stop"));
    assert_eq!((out.effects, out.pending), (vec![Effect::StopAll], Some(pending::Request::StopSteam)));
    assert!(apply(&mut settings, &games, &Request::run("game.play").of("C:\\Gone\\gone.exe")).refused.is_some());
}

#[test]
fn a_running_game_shows_stop_and_a_play_that_runs_says_starting() {
    let program = steam::program(&lab()).unwrap();
    let mut model = model_with(vec![lab()]);
    model.pending = Some(crate::pending::Pending::new(pending::Request::Play(program.clone()), &model, Instant::now()));
    let settings = Settings::default();
    let play = |model: &Model| {
        let pages = pages(&settings, model);
        let Some(Item::Buttons(row)) = page(&pages, PLAY).find("game.play", Some(&program)).cloned() else { panic!() };
        row
    };
    assert_eq!(play(&model).buttons[0].words(), "Starting...");
    // The host names the game's program: the start is done and the row offers Stop
    model.game_program = Some("TheLab.exe".into());
    let row = play(&model);
    assert_eq!(row.buttons.iter().map(|b| b.words()).collect::<Vec<_>>(), ["Playing", "Options", "Stop"]);
}

#[test]
fn only_a_game_that_the_user_added_has_remove() {
    let games = [lab(), added()];
    let mut settings = Settings::default();
    settings.added_games.push(crate::backend::AddedGame { name: "Plain".into(), program: PathBuf::from("/Users/x/Plain/plain.exe") });
    let model = model_with(games.to_vec());
    let pages = pages(&settings, &model);
    let lab_program = steam::program(&lab()).unwrap();
    let added_program = steam::program(&added()).unwrap();
    let removes = |id: &str| page(&pages, id).items.iter().any(|item| item.button("game.remove", Some(&added_program)).is_some() || item.button("game.remove", Some(&lab_program)).is_some());
    assert!(!removes(&game_page(&lab_program)));
    assert!(removes(&game_page(&added_program)));
    assert!(apply(&mut settings.clone(), &games, &Request::run("game.remove").of(lab_program)).refused.is_some());
    let out = apply(&mut settings, &games, &Request::run("game.remove").of(added_program));
    assert!(out.changed && settings.added_games.is_empty());
}

#[test]
fn the_controls_travel_with_the_menu_and_come_back_with_a_request() {
    let mut settings = Settings::default();
    // A file without `controls` gets the standard mapping
    let partial: Settings = serde_json::from_str(r#"{"fps": 90}"#).unwrap();
    assert_eq!((partial.fps, partial.controls), (90, Controls::default()));
    settings.controls.set(Hand::Right, Control::Secondary, Action::Keyboard);
    let menu: Value = serde_json::from_str(&vr_menu(&settings, &Model::default())).unwrap();
    assert_eq!(menu["controls"]["right"]["secondary"], "keyboard");
    assert_eq!(menu["controls"]["models_in_game"], "while_pointing");
    // The headset sends the whole mapping back as it changed it
    let mut changed = settings.controls;
    assert!(changed.set(Hand::Left, Control::Grip, Action::Recenter));
    let never = Controls { models_in_game: ModelsInGame::Never, ..changed };
    let out = apply(&mut settings, &[], &Request::set("controls", serde_json::to_string(&never).unwrap()));
    assert!(out.changed && !out.restart);
    assert_eq!(settings.controls, never);
    // A value that is no mapping changes nothing
    assert!(apply(&mut settings, &[], &Request::set("controls", "5")).refused.is_some());
    assert_eq!(settings.controls, never);
}

#[test]
fn the_menu_carries_the_pages_and_the_notice_only_while_there_is_one() {
    let mut model = Model::default();
    let read = |model: &Model| serde_json::from_str::<Value>(&vr_menu(&Settings::default(), model)).unwrap();
    assert!(read(&model).get("notice").is_none());
    assert_eq!(read(&model)["pages"][0]["id"], PLAY);
    model.note_message("The right eye stayed black. The game restarts.".into());
    assert_eq!(read(&model)["notice"], "The right eye stayed black. The game restarts.");
    // The pages read back as the headset reads them, without the window's hints
    let pages: Vec<Page> = serde_json::from_value(read(&model)["pages"].clone()).unwrap();
    let mut shown = super::pages(&Settings::default(), &model);
    shown.iter_mut().for_each(|page| page.items.iter_mut().for_each(Item::drop_hint));
    assert_eq!(pages, shown);
    assert!(!read(&model).to_string().contains("\"hint\""));
}

fn steam_words(model: &Model) -> (Vec<String>, Vec<String>) {
    let pages = pages(&Settings::default(), model);
    let steam = page(&pages, STEAM);
    let mut texts = Vec::new();
    let mut buttons = Vec::new();
    for item in &steam.items {
        match item {
            Item::Text { text, .. } => texts.push(text.clone()),
            Item::Buttons(row) => buttons.extend(row.buttons.iter().map(|button| button.words().to_string())),
            _ => {}
        }
    }
    (texts, buttons)
}

#[test]
fn the_steam_page_offers_the_setup_that_this_mac_needs() {
    // A new Mac: no Rosetta, no Wine
    let mut model = Model::default();
    let (texts, buttons) = steam_words(&model);
    assert_eq!(buttons, ["Set up games"]);
    assert!(texts.iter().any(|text| text.starts_with("Rosetta 2 is missing")), "{texts:?}");
    assert!(texts.iter().any(|text| text.starts_with("Game support: Wine runs Windows games")), "{texts:?}");
    // The play page says what keeps the games away and links the Steam page
    let pages = pages(&Settings::default(), &model);
    let Item::Buttons(row) = &page(&pages, PLAY).items[0] else { panic!() };
    assert_eq!((row.title.as_str(), row.buttons[0].press.clone()), ("This Mac needs a setup for games", cranvr_common::menu::Press::Open { page: STEAM.into() }));
    // An older game support gets its update, a broken Steam its repair
    model.rosetta = true;
    model.steam = SteamState::Ready;
    model.game_support = Some("2026.11.20.1".into());
    model.game_support_outdated = true;
    assert_eq!(steam_words(&model).1, ["Update game support", "Open Steam", "Stop Steam and games"]);
    model.game_support_outdated = false;
    model.steam = SteamState::NeedsSetup;
    assert_eq!(steam_words(&model).1, ["Repair Steam"]);
    // A ready Steam opens and stops, and a user who never signed in is told where
    model.steam = SteamState::Ready;
    let (texts, buttons) = steam_words(&model);
    assert_eq!(buttons, ["Open Steam", "Stop Steam and games"]);
    assert!(texts.iter().any(|text| text.starts_with("Sign in to Steam once")));
    model.steam_running = true;
    model.steam_signed_in = true;
    let (texts, buttons) = steam_words(&model);
    assert_eq!(buttons, ["Stop Steam and games"]);
    assert!(texts.contains(&"Steam is running".to_string()) && !texts.iter().any(|text| text.starts_with("Sign in")));
    assert!(page(&super::pages(&Settings::default(), &model), PLAY).items.iter().all(|item| !matches!(item, Item::Buttons(row) if row.buttons.iter().any(|b| b.press == cranvr_common::menu::Press::Open { page: STEAM.into() }))));
}

#[test]
fn a_running_setup_shows_its_steps_and_a_failed_one_offers_retry() {
    use crate::setup::{Progress, Step};
    let mut model = Model { rosetta: true, ..Default::default() };
    model.setup = Some(Setup { steps: vec![(Step::GameSupport, Progress::Working("Downloading 40%".into())), (Step::Steam, Progress::Waiting)] });
    let (texts, buttons) = steam_words(&model);
    assert_eq!(buttons, ["Setting up..."]);
    assert!(texts.contains(&"Game support: Downloading 40%".to_string()) && texts.contains(&"Steam for Windows: Waiting".to_string()), "{texts:?}");
    model.setup = Some(Setup { steps: vec![(Step::GameSupport, Progress::Failed("The download did not reach its server".into())), (Step::Steam, Progress::Waiting)] });
    let (texts, buttons) = steam_words(&model);
    assert_eq!(buttons, ["Retry"]);
    assert!(texts.contains(&"Game support: Failed: The download did not reach its server".to_string()));
}

#[test]
fn setup_and_open_steam_are_actions_of_either_app() {
    let mut settings = Settings::default();
    assert_eq!(apply(&mut settings, &[], &Request::run("setup.start")).effects, [Effect::Setup]);
    let out = apply(&mut settings, &[], &Request::run("steam.open"));
    assert_eq!((out.effects, out.pending), (vec![Effect::OpenSteam], Some(pending::Request::OpenSteam)));
    assert!(!out.changed);
}
