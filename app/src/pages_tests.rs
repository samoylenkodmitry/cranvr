use super::*;
use crate::{
    backend::{vr_menu, Model, Settings},
    setup::Setup,
};
use cranvr_common::{
    controls::{Action, Control, Controls, Hand, ModelsInGame},
    menu::{Buttons, Item, Page, Request},
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
    for (id, value) in [("fps", "120"), ("mbps", "100"), ("foveation_level", "medium"), ("game_scale", "70"), ("game_msaa_limit", "2"), ("hand_controllers", "on"), ("autostart", "on")] {
        assert_eq!(stream.find(id, None).and_then(Item::value), Some(value), "{id}");
    }
    // A change that the host reads at its start restarts it
    let out = apply(&mut settings, &model_with(games.to_vec()), &Request::set("foveation_level", "high"));
    assert_eq!((out.changed, out.restart), (true, true));
    assert_eq!(settings.foveation_level, FoveationLevel::High);
    let out = apply(&mut settings, &model_with(games.to_vec()), &Request::set("game_scale", "85"));
    assert!(out.restart && (settings.game_scale - 0.85).abs() < 1e-6);
    // The anti-aliasing limit counts at a game's start, so the stream goes on
    let out = apply(&mut settings, &model_with(games.to_vec()), &Request::set("game_msaa_limit", "0"));
    assert_eq!((out.changed, out.restart, settings.game_msaa_limit), (true, false, 0));
    let out = apply(&mut settings, &model_with(games.to_vec()), &Request::set("hand_controllers", "off"));
    assert!(out.restart && !settings.hand_controllers);
    // The same value again changes nothing and restarts nothing
    let out = apply(&mut settings, &model_with(games.to_vec()), &Request::set("hand_controllers", "off"));
    assert_eq!((out.changed, out.restart), (false, false));
    // A value that the setting does not offer, and a setting that does not exist, change nothing
    let before = settings.clone();
    for request in [Request::set("fps", "100"), Request::set("game_audio", "maybe"), Request::set("volume", "200"), Request::run("format.disk")] {
        let out = apply(&mut settings, &model_with(games.to_vec()), &request);
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
    let out = apply(&mut settings, &model_with(games.to_vec()), &Request::set("game.openvr", "default").of(program.clone()));
    assert!(out.changed && !out.restart);
    assert!(!settings.launch.contains_key(&program), "{:?}", settings.launch);
    // The two eye options ask for opposite things
    apply(&mut settings, &model_with(games.to_vec()), &Request::set("game.eye_per_pass", "on").of(program.clone()));
    apply(&mut settings, &model_with(games.to_vec()), &Request::set("game.eye_one_pass", "on").of(program.clone()));
    let options = &settings.launch[&program];
    assert!(options.eye_one_pass && !options.eye_per_pass && !options.eye_one_pass_declined);
    // A word that is no VR system, and a game that is not installed, change nothing
    let before = settings.clone();
    assert!(apply(&mut settings, &model_with(games.to_vec()), &Request::set("game.openvr", "oculus").of(program.clone())).refused.is_some());
    assert!(apply(&mut settings, &model_with(games.to_vec()), &Request::set("game.openvr", "off").of("C:\\Gone\\gone.exe")).refused.is_some());
    assert_eq!(settings, before);
}

#[test]
fn play_starts_a_game_by_its_program_and_stop_ends_steam_and_games() {
    let games = [lab(), added()];
    let program = steam::program(&lab()).unwrap();
    let mut settings = Settings::default();
    let out = apply(&mut settings, &model_with(games.to_vec()), &Request::run("game.play").of(program.clone()));
    assert_eq!(out.effects, [Effect::Play(lab())]);
    assert_eq!(out.pending, Some(pending::Request::Play(program.clone())));
    assert!(!out.changed);
    let out = apply(&mut settings, &model_with(games.to_vec()), &Request::run("steam.stop"));
    assert_eq!((out.effects, out.pending), (vec![Effect::StopAll], Some(pending::Request::StopSteam)));
    assert!(apply(&mut settings, &model_with(games.to_vec()), &Request::run("game.play").of("C:\\Gone\\gone.exe")).refused.is_some());
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
    assert!(apply(&mut settings.clone(), &model_with(games.to_vec()), &Request::run("game.remove").of(lab_program)).refused.is_some());
    let out = apply(&mut settings, &model_with(games.to_vec()), &Request::run("game.remove").of(added_program));
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
    let out = apply(&mut settings, &Model::default(), &Request::set("controls", serde_json::to_string(&never).unwrap()));
    assert!(out.changed && !out.restart);
    assert_eq!(settings.controls, never);
    // A value that is no mapping changes nothing
    assert!(apply(&mut settings, &Model::default(), &Request::set("controls", "5")).refused.is_some());
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
    assert_eq!(apply(&mut settings, &Model::default(), &Request::run("setup.start")).effects, [Effect::Setup]);
    let out = apply(&mut settings, &Model::default(), &Request::run("steam.open"));
    assert_eq!((out.effects, out.pending), (vec![Effect::OpenSteam], Some(pending::Request::OpenSteam)));
    assert!(!out.changed);
}

#[test]
fn every_setting_stands_on_one_page_only() {
    let model = model_with(vec![lab()]);
    let pages = pages(&Settings { virtual_display: Some((1920, 1080)), ..Settings::default() }, &model);
    let mut seen = std::collections::BTreeMap::new();
    for page in &pages {
        for item in &page.items {
            let id = match item {
                Item::Choice(choice) => (choice.id.clone(), choice.subject.clone()),
                Item::Toggle(toggle) => (toggle.id.clone(), toggle.subject.clone()),
                _ => continue,
            };
            assert!(seen.insert(id.clone(), page.id.clone()).is_none(), "{id:?} stands twice");
        }
    }
    // Each setting of the table has a page
    for setting in settings_table() {
        assert!(seen.contains_key(&(setting.id.to_string(), None)), "{} has no page", setting.id);
    }
    let listed: Vec<&str> = pages.iter().filter(|page| page.listed).map(|page| page.short.as_str()).collect();
    assert_eq!(listed, ["Play", "Steam", "Stream", "Connection", "Desktop", "Sound", "Wine", "Headset", "Mac"]);
}

#[test]
fn the_connection_choice_reaches_the_running_host_and_restarts_nothing() {
    let mut settings = Settings::default();
    let model = Model::default();
    assert_eq!(page(&pages(&settings, &model), CONNECTION).find("connection", None).and_then(Item::value), Some("automatic"));
    let out = apply(&mut settings, &model, &Request::set("connection", "wifi"));
    assert_eq!((out.changed, out.restart, out.effects), (true, false, vec![Effect::TellConnection(Connection::WiFi)]));
    assert_eq!(settings.connection, Connection::WiFi);
    // The same choice again tells nothing
    assert!(apply(&mut settings, &model, &Request::set("connection", "wifi")).effects.is_empty());
    assert!(apply(&mut settings, &model, &Request::set("connection", "bluetooth")).refused.is_some());
}

#[test]
fn the_connection_page_offers_the_cables_tool_when_the_mac_lacks_it() {
    let settings = Settings::default();
    let mut model = Model::default();
    model.usb.looked = true;
    let buttons = |model: &Model| page(&pages(&settings, model), CONNECTION).items.iter().filter_map(|item| item.button("usb.download", None).map(|button| button.label.clone())).collect::<Vec<_>>();
    assert_eq!(buttons(&model), ["Add USB support"]);
    model.usb.download = Download::Failed("The download stopped".into());
    assert_eq!(buttons(&model), ["Try again"]);
    model.usb.adb = Some("/usr/bin/adb".into());
    assert!(buttons(&model).is_empty());
    assert_eq!(apply(&mut settings.clone(), &model, &Request::run("usb.download")).effects, [Effect::DownloadUsb]);
}

#[test]
fn the_desktop_page_shows_the_added_displays_settings_while_there_is_one() {
    let mut settings = Settings::default();
    let model = Model::default();
    let has = |settings: &Settings, id: &str| page(&pages(settings, &model), DESKTOP).find(id, None).is_some();
    assert!(!has(&settings, "vr_main_display") && !has(&settings, "window_in_vr"));
    let out = apply(&mut settings, &model, &Request::set("virtual_display", "2560x1440"));
    assert!(out.changed && out.restart);
    assert_eq!(settings.virtual_display, Some((2560, 1440)));
    assert!(has(&settings, "vr_main_display") && has(&settings, "window_in_vr"));
    assert_eq!(page(&pages(&settings, &model), DESKTOP).find("virtual_display", None).and_then(Item::value), Some("2560x1440"));
    // The window's move into VR is the window's own: the host keeps running
    assert!(!apply(&mut settings, &model, &Request::set("window_in_vr", "on")).restart);
    assert!(apply(&mut settings, &model, &Request::set("desktop_fps", "60")).restart);
}

#[test]
fn a_missing_permission_has_ask_and_open_settings_on_the_mac() {
    let settings = Settings::default();
    let model = Model { permissions: [true, false, true], ..Default::default() };
    let desktop = page(&pages(&settings, &model), DESKTOP).clone();
    let rows: Vec<&Buttons> = desktop.items.iter().filter_map(|item| if let Item::Buttons(row) = item { Some(row) } else { None }).collect();
    assert_eq!(rows.iter().map(|row| row.title.as_str()).collect::<Vec<_>>(), ["Screen Recording: allowed", "System Audio Recording: not allowed", "Accessibility: allowed"]);
    assert!(rows[0].buttons.is_empty());
    assert!(rows[1].buttons[0].runs("permission.ask", Some("system_audio")));
    let out = apply(&mut settings.clone(), &model, &Request::run("permission.ask").of("system_audio"));
    assert_eq!(out.effects, [Effect::AskPermission(Permission::SystemAudio)]);
    let out = apply(&mut settings.clone(), &model, &Request::run("permission.settings").of("accessibility"));
    assert_eq!(out.effects, [Effect::PermissionSettings(Permission::Accessibility)]);
    assert!(apply(&mut settings.clone(), &model, &Request::run("permission.ask").of("camera")).refused.is_some());
}

#[test]
fn the_microphone_offers_the_macs_inputs_and_keeps_a_device_that_is_gone() {
    use crate::microphone::InputDevice;
    let mut settings = Settings::default();
    let mut model = Model { input_devices: vec![InputDevice { uid: "BuiltIn".into(), name: "MacBook Microphone".into() }], ..Default::default() };
    let options = |settings: &Settings, model: &Model| match page(&pages(settings, model), SOUND).find("microphone", None) {
        Some(Item::Choice(choice)) => choice.options.iter().map(|option| (option.value.clone(), option.label.clone())).collect::<Vec<_>>(),
        _ => panic!("no microphone"),
    };
    assert_eq!(options(&settings, &model)[1], ("mac:BuiltIn".to_string(), "MacBook Microphone".to_string()));
    let out = apply(&mut settings, &model, &Request::set("microphone", "mac:BuiltIn"));
    assert!(out.changed && !out.restart);
    assert_eq!(settings.microphone, MicrophoneSource::Mac { uid: "BuiltIn".into(), name: "MacBook Microphone".into() });
    // The device goes away: it stays a choice with its name, and the page says so
    model.input_devices.clear();
    assert_eq!(options(&settings, &model)[1], ("mac:BuiltIn".to_string(), "MacBook Microphone (not connected)".to_string()));
    assert!(page(&pages(&settings, &model), SOUND).items.iter().any(|item| matches!(item, Item::Text { text, .. } if text.starts_with("MacBook Microphone is not connected"))));
    assert!(apply(&mut settings, &model, &Request::set("microphone", "mac:Unknown")).refused.is_some());
    apply(&mut settings, &model, &Request::set("microphone", "off"));
    assert_eq!(settings.microphone, MicrophoneSource::Off);
}

#[test]
fn the_wine_page_has_the_options_of_every_program_and_the_mac_page_restarts_the_host() {
    let mut settings = Settings::default();
    settings.wine.dll_overrides = "winhttp=n,b".into();
    let model = Model { running: true, ..Default::default() };
    let pages = pages(&settings, &model);
    let wine = page(&pages, WINE);
    assert_eq!(wine.find("wine.openvr", None).and_then(Item::value), Some("opencomposite"));
    assert!(wine.items.iter().any(|item| matches!(item, Item::Text { text, .. } if text == "DLL overrides for every program: winhttp=n,b")));
    let out = apply(&mut settings, &model, &Request::set("wine.mac_window", "on"));
    assert!(out.changed && !out.restart && settings.wine.mac_window);
    let mac = page(&pages, MAC);
    assert_eq!(mac.items.iter().find_map(|item| item.button("host.restart", None)).map(|b| b.label.as_str()), Some("Restart the host"));
    assert_eq!(apply(&mut settings, &model, &Request::run("host.restart")).effects, [Effect::RestartHost]);
}

#[test]
fn the_headsets_room_and_volume_wait_for_the_headsets_own_choice_and_travel_with_the_menu() {
    let mut settings = Settings::default();
    let model = Model::default();
    let read = |settings: &Settings| serde_json::from_str::<Value>(&vr_menu(settings, &Model::default())).unwrap();
    // A Mac that never heard the headset's choice has none: the menu says null, and the page selects nothing
    assert_eq!(read(&settings)["headset"], serde_json::json!({"room": null, "volume": null}));
    assert_eq!(page(&pages(&settings, &model), HEADSET).find("headset_room", None).and_then(Item::value), Some(""));
    // The headset tells its choice, and later the user changes it in either app
    assert!(apply(&mut settings, &model, &Request::set("headset_room", "scene")).changed);
    assert!(apply(&mut settings, &model, &Request::set("headset_volume", "200")).changed);
    assert_eq!(read(&settings)["headset"], serde_json::json!({"room": "scene", "volume": 200}));
    assert!(apply(&mut settings, &model, &Request::set("headset_volume", "175")).refused.is_some());
    // The values read back from the file
    let text = settings.file_text(None).unwrap();
    let back = Settings::read(&text).settings;
    assert_eq!((back.headset_room.as_deref(), back.headset_volume), (Some("scene"), Some(200)));
}

#[test]
fn a_press_on_the_headsets_own_page_goes_to_the_headset() {
    let mut settings = Settings::default();
    let request = Request::run("headset.recenter");
    let out = apply(&mut settings, &Model::default(), &request);
    assert_eq!(out.effects, [Effect::ToHeadset(request)]);
    assert!(!out.changed);
    let toggle = Request::run("headset.panel").of("stats");
    assert_eq!(apply(&mut settings, &Model::default(), &toggle).effects, [Effect::ToHeadset(toggle)]);
    // The headset's page reaches the model with its status line, and goes with the headset
    let mut model = Model::default();
    model.apply_status_for_test(&serde_json::json!({"type": "headset_page", "page": {"id": "headset.own", "title": "In the headset", "items": [{"kind": "heading", "text": "Panels"}]}}));
    assert_eq!(model.headset_page.as_ref().map(|page| page.title.as_str()), Some("In the headset"));
    model.apply_status_for_test(&serde_json::json!({"type": "headset", "connected": false}));
    assert!(model.headset_page.is_none());
}
