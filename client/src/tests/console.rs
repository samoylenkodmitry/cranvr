use super::*;

const MENU: &str = r#"{"games":[{"name":"Beat Saber","program":"Beat Saber.exe","vr":"opencomposite"},{"name":"Half-Life: Alyx","program":"hlvr.exe","vr":"steamvr"}],
    "fps":120,"game_scale":0.6,"msaa":2,"controls":{},
    "status":{"refresh":120,"fps":119.4,"game_fps":117,"latency_ms":46.2,"game":"Beat Saber.exe"}}"#;

/// A menu with the games and the settings only, as the app writes it before a headset streams
const BARE: &str = r#"{"games":[],"fps":90,"game_scale":1.0,"msaa":0,"controls":{}}"#;

fn menu(game: Option<&str>) -> Menu {
    let mut menu = Menu::parse(MENU).unwrap();
    menu.status.game = game.map(str::to_string);
    menu
}

#[test]
fn the_menu_comes_from_the_hosts_json() {
    let menu = Menu::parse(MENU).unwrap();
    assert_eq!(
        menu.games,
        [
            Game { name: "Beat Saber".into(), program: "Beat Saber.exe".into(), vr: VrSystem::OpenComposite },
            Game { name: "Half-Life: Alyx".into(), program: "hlvr.exe".into(), vr: VrSystem::SteamVr }
        ]
    );
    assert_eq!((menu.fps, menu.msaa), (120, 2));
    assert!((menu.game_scale - 0.6).abs() < 1e-6);
    assert_eq!(menu.status, Status { refresh: 120.0, fps: 119.4, game_fps: 117.0, latency_ms: Some(46.2), game: Some("Beat Saber.exe".into()) });
    assert!(menu.runs(&menu.games[0]) && !menu.runs(&menu.games[1]));
    // The host sends the file without a status while no headset streams
    let bare = Menu::parse(BARE).unwrap();
    assert_eq!((bare.fps, bare.game_scale, bare.status.game), (90, 1.0, None));
    assert_eq!(Menu::parse("not json"), None);
    assert_eq!(Menu::parse(r#"{"fps":90}"#), None, "a text without games is no menu");
    // Every setting and the controls belong to the menu
    for lacking in [r#"{"games":[],"game_scale":1.0,"msaa":0,"controls":{}}"#, r#"{"games":[],"fps":90,"msaa":0,"controls":{}}"#, r#"{"games":[],"fps":90,"game_scale":1.0,"controls":{}}"#, r#"{"games":[],"fps":90,"game_scale":1.0,"msaa":0}"#, r#"{"games":[],"fps":90,"game_scale":1.0,"msaa":0,"controls":5}"#] {
        assert_eq!(Menu::parse(lacking), None, "{lacking}");
    }
    // Each game names its VR system with a word that the console knows
    for game in [r#"{"name":"A","program":"a.exe"}"#, r#"{"name":"A","program":"a.exe","vr":"oculus"}"#] {
        assert_eq!(Menu::parse(&format!(r#"{{"games":[{game}],"fps":90,"game_scale":1.0,"msaa":0,"controls":{{}}}}"#)), None, "{game}");
    }
}

#[test]
fn a_press_on_a_game_s_vr_system_asks_for_the_next_one() {
    assert_eq!(VrSystem::OpenComposite.next(), VrSystem::SteamVr);
    assert_eq!(VrSystem::SteamVr.next(), VrSystem::Off);
    assert_eq!(VrSystem::Off.next(), VrSystem::OpenComposite);
    for system in VrSystem::ALL {
        assert_eq!(VrSystem::from_key(system.key()), Some(system));
    }
    let request = Action::VrSystem("The Lab".into(), VrSystem::SteamVr).request();
    assert_eq!(serde_json::from_str::<Value>(&request).unwrap(), json!({"action": "set", "game": "The Lab", "openvr": "steamvr"}));
}

#[test]
fn the_apps_notice_shows_in_the_menu_and_adds_a_line_to_the_height() {
    let mut with = Menu::parse(r#"{"games":[],"fps":90,"game_scale":1.0,"msaa":0,"controls":{},"notice":"The right eye stayed black. The game restarts."}"#).unwrap();
    assert_eq!(with.notice.as_deref(), Some("The right eye stayed black. The game restarts."));
    let without = Menu::parse(r#"{"games":[],"fps":90,"game_scale":1.0,"msaa":0,"controls":{},"notice":""}"#).unwrap();
    assert_eq!(without.notice, None);
    assert_eq!(Menu::parse(BARE).unwrap().notice, None);
    assert!((height(Some(&with)) - height(Some(&without)) - (size::TEXT + size::GAP)).abs() < 1e-3);
    with.notice = None;
    assert!((height(Some(&with)) - height(Some(&without))).abs() < 1e-3);
}

#[test]
fn requests_are_the_json_that_the_app_reads() {
    let json = |action: Action| serde_json::from_str::<Value>(&action.request()).unwrap();
    assert_eq!(json(Action::Play("Beat Saber".into())), json!({"action": "play", "game": "Beat Saber"}));
    assert_eq!(json(Action::Stop), json!({"action": "stop"}));
    assert_eq!(json(Action::Fps(90)), json!({"action": "set", "fps": 90}));
    assert_eq!(json(Action::Msaa(0)), json!({"action": "set", "msaa": 0}));
    assert!((json(Action::Scale(0.7))["game_scale"].as_f64().unwrap() - 0.7).abs() < 1e-6);
}

#[test]
fn the_height_grows_with_the_games_up_to_the_limit() {
    let mut menu = menu(None);
    let two = height(Some(&menu));
    menu.games.push(Game { name: "Third".into(), program: "third.exe".into(), ..Default::default() });
    assert!((height(Some(&menu)) - two - (size::BUTTON + size::GAP)).abs() < 1e-3);
    menu.games.extend((0..10).map(|i| Game { name: format!("Game {i}"), program: format!("g{i}.exe"), ..Default::default() }));
    assert_eq!(menu.listed().len(), MAX_GAMES);
    assert!(height(Some(&menu)) < 1000.0, "{}", height(Some(&menu)));
    // A console without a menu is short, and a menu without games keeps a line for the note
    assert!(height(None) < 400.0);
    menu.games.clear();
    assert!(height(Some(&menu)) > two - (size::BUTTON + size::GAP) - 1e-3);
}

#[test]
fn the_first_menu_opens_the_console_and_a_game_closes_it() {
    let mut console = Console::default();
    assert!(!console.view().open);
    console.on_menu(menu(None));
    assert!(console.view().open);
    // The next menus change nothing while no game starts
    console.on_menu(menu(None));
    assert!(console.view().open);
    // The user hides it. A menu without a change of the game keeps it hidden.
    console.close();
    console.on_menu(menu(None));
    assert!(!console.view().open);
    // A game starts: hidden. The game ends: shown again
    console.on_menu(menu(Some("Beat Saber.exe")));
    assert!(!console.view().open);
    console.on_menu(menu(Some("Beat Saber.exe")));
    console.on_menu(menu(None));
    assert!(console.view().open);
    // The link ends: no menu, no console
    console.clear();
    assert!(!console.view().open && console.view().menu.is_none());
}

#[test]
fn the_action_of_a_button_shows_or_hides_the_console() {
    let mut console = Console::default();
    assert!(!console.view().open);
    console.toggle_open();
    assert!(console.view().open);
    console.toggle_open();
    assert!(!console.view().open);
}

fn with_controls(controls: &Controls) -> Menu {
    let mut menu = menu(None);
    menu.controls = *controls;
    menu
}

#[test]
fn the_menu_carries_the_controls() {
    let mut mapped = Controls::default();
    mapped.set(Hand::Right, Control::Secondary, ControlAction::Keyboard);
    let text = format!(r#"{{"games":[],"fps":90,"game_scale":1.0,"msaa":0,"controls":{}}}"#, mapped.to_json());
    assert_eq!(Menu::parse(&text).unwrap().controls, mapped);
    assert_eq!(Menu::parse(BARE).unwrap().controls, Controls::default());
}

#[test]
fn the_mac_sets_the_controls_and_the_headset_keeps_them_without_a_link() {
    let mut console = Console::default();
    assert_eq!(console.controls(), Controls::default());
    let mut mapped = Controls::default();
    mapped.set(Hand::Left, Control::Grip, ControlAction::Recenter);
    let now = Instant::now();
    // The menu changes the mapping, and says so once
    assert_eq!(console.on_menu_at(with_controls(&mapped), now), Some(mapped));
    assert_eq!(console.on_menu_at(with_controls(&mapped), now), None);
    assert_eq!(console.view().controls, mapped);
    // The link ends: the mapping stays, and the page cannot change it
    console.clear();
    assert_eq!(console.controls(), mapped);
    assert!(!console.view().controls_editable);
    assert_eq!(console.step_control(Hand::Left, Control::Trigger, 1, now), None);
    assert_eq!(console.reset_controls(now), None);
    assert_eq!(console.controls(), mapped, "a refused change shows nothing");
}

#[test]
fn a_change_of_the_user_shows_at_once_and_a_menu_that_was_on_its_way_does_not_undo_it() {
    let mut console = Console::default();
    let standard = Controls::default();
    let start = Instant::now();
    console.on_menu_at(with_controls(&standard), start);
    assert!(console.view().controls_editable);
    // The trigger's action steps from the left click to the right click and back
    let sent = console.step_control(Hand::Right, Control::Trigger, 1, start).unwrap();
    assert_eq!(sent.right.trigger, ControlAction::RightClick);
    assert_eq!(console.controls(), sent);
    let back = console.step_control(Hand::Right, Control::Trigger, -1, start).unwrap();
    assert_eq!(back, standard);
    let again = console.step_control(Hand::Right, Control::Trigger, 1, start).unwrap();
    // The Mac sends the mapping that it had before it saw the change
    let later = start + Duration::from_millis(500);
    assert_eq!(console.on_menu_at(with_controls(&standard), later), None);
    assert_eq!(console.controls(), again);
    // The Mac repeats the change: the wait ends
    assert_eq!(console.on_menu_at(with_controls(&again), later), None);
    // A change at the Mac's window shows now
    let mut at_the_mac = again;
    at_the_mac.set(Hand::Left, Control::Primary, ControlAction::MiddleClick);
    assert_eq!(console.on_menu_at(with_controls(&at_the_mac), later), Some(at_the_mac));
}

#[test]
fn a_change_that_the_mac_never_repeats_gives_way_after_three_seconds() {
    let mut console = Console::default();
    let start = Instant::now();
    console.on_menu_at(with_controls(&Controls::default()), start);
    let asked = console.step_control(Hand::Left, Control::Menu, 1, start).unwrap();
    assert_ne!(asked, Controls::default());
    // No answer from the Mac: its own mapping returns once the wait is over
    assert_eq!(console.on_menu_at(with_controls(&Controls::default()), start + Duration::from_secs(2)), None);
    assert_eq!(console.controls(), asked);
    assert_eq!(console.on_menu_at(with_controls(&Controls::default()), start + Duration::from_secs(4)), Some(Controls::default()));
}

#[test]
fn stepping_through_the_actions_of_an_input_visits_each_one_it_offers_and_comes_round() {
    let mut console = Console::default();
    let now = Instant::now();
    console.on_menu_at(with_controls(&Controls::default()), now);
    for (control, hand) in [(Control::Primary, Hand::Left), (Control::StickVertical, Hand::Right), (Control::Menu, Hand::Left)] {
        let offers = control.offers();
        let mut seen = vec![console.controls().action(hand, control)];
        for _ in 0..offers.len() {
            let next = console.step_control(hand, control, 1, now).unwrap();
            seen.push(next.action(hand, control));
        }
        assert_eq!(seen.first(), seen.last(), "{control:?} comes round after one turn");
        let mut distinct = seen[..seen.len() - 1].to_vec();
        distinct.sort_by_key(|action| action.key());
        distinct.dedup();
        assert_eq!(distinct.len(), offers.len(), "{control:?} offers {offers:?} and the steps saw {seen:?}");
        // No step ever lands on an action that the input does not offer
        assert!(seen.iter().all(|action| offers.contains(action)));
    }
}

#[test]
fn the_choice_of_the_controller_models_goes_to_the_mac_with_the_controls_and_needs_a_link() {
    let mut console = Console::default();
    let now = Instant::now();
    assert_eq!(console.pick_models(ModelsInGame::Never, now), None);
    assert_eq!(console.controls().models_in_game, ModelsInGame::WhilePointing);
    console.on_menu_at(with_controls(&Controls::default()), now);
    let sent = console.pick_models(ModelsInGame::Always, now).unwrap();
    assert_eq!(sent, Controls { models_in_game: ModelsInGame::Always, ..Controls::default() });
    assert_eq!(console.view().controls.models_in_game, ModelsInGame::Always);
    // The Mac sends the mapping that it had before it saw the change: the choice stays
    assert_eq!(console.on_menu_at(with_controls(&Controls::default()), now + Duration::from_millis(500)), None);
    assert_eq!(console.controls().models_in_game, ModelsInGame::Always);
}

#[test]
fn the_standard_button_gives_the_standard_mapping_back() {
    let mut console = Console::default();
    let now = Instant::now();
    let mut odd = Controls::default();
    odd.set(Hand::Left, Control::Trigger, ControlAction::Nothing);
    console.on_menu_at(with_controls(&odd), now);
    assert_eq!(console.reset_controls(now), Some(Controls::default()));
    assert_eq!(console.controls(), Controls::default());
}

#[test]
fn the_controls_page_fits_a_panel_that_a_person_can_reach() {
    // Two lines, the title, two tabs, eight inputs and the buttons: about 740 dp, under a meter
    let tall = controls_height();
    assert!(tall > 600.0 && tall < 900.0, "{tall}");
}

#[test]
fn the_room_and_the_volume_come_back_from_their_saved_text() {
    assert_eq!(Room::default(), Room::Passthrough);
    assert_eq!(Volume::default(), Volume::P150);
    for room in Room::ALL {
        assert_eq!(Room::from_key(room.key()), Some(room));
    }
    for volume in Volume::ALL {
        assert_eq!(Volume::from_key(&volume.key()), Some(volume));
    }
    // Text that no choice wrote gives no choice, so the setting falls back to its default
    assert_eq!(Room::from_key("sky"), None);
    assert_eq!(Volume::from_key("175"), None);
    assert_eq!(Volume::from_key("loud"), None);
    assert_eq!(Room::from_key(" scene "), Some(Room::Scene));
    assert_eq!(Volume::ALL.map(Volume::percent), [100, 150, 200, 300]);
    assert_eq!(Volume::ALL.map(Volume::gain), [1.0, 1.5, 2.0, 3.0]);
    assert_eq!(Volume::P200.label(), "200%");
    assert_eq!((Room::Passthrough.label(), Room::Scene.label()), ("Passthrough", "Scene"));
}

#[test]
fn the_hosts_hiding_of_the_desktop_tells_a_game_and_the_menu_tells_it_without_a_desktop() {
    let idle = menu(None);
    let playing = menu(Some("Beat Saber.exe"));
    // The host offers a desktop and shows it: no game, whatever a late menu says
    assert!(!game_runs(None, Some(false)));
    assert!(!game_runs(Some(&idle), Some(false)));
    assert!(!game_runs(Some(&playing), Some(false)), "the game ended and the menu is up to a second late");
    // The host hides it for a game, also before its menu names the game
    assert!(game_runs(None, Some(true)));
    assert!(game_runs(Some(&idle), Some(true)));
    assert!(game_runs(Some(&playing), Some(true)));
    // The host offers no desktop: the menu says
    assert!(!game_runs(None, None), "no menu and no desktop: no game");
    assert!(!game_runs(Some(&idle), None));
    assert!(game_runs(Some(&playing), None));
}

#[test]
fn the_room_shows_only_while_no_game_runs_and_the_setting_asks_for_it() {
    let backdrop = |room, game_runs| Backdrop { room, game_runs };
    // The setting is Passthrough, which is the default
    assert!(backdrop(Room::default(), false).shows_room(true));
    assert!(!backdrop(Room::Passthrough, true).shows_room(true), "a game fills the view");
    // The setting is Scene: the host's scene stays, with or without a game
    assert!(!backdrop(Room::Scene, false).shows_room(true));
    assert!(!backdrop(Room::Scene, true).shows_room(true));
    // A headset or a window that cannot show the room keeps the stream
    assert!(!backdrop(Room::Passthrough, false).shows_room(false));
}

#[test]
fn the_console_follows_the_game_and_the_setting_into_its_backdrop() {
    let mut console = Console::default();
    // No link and no menu: no game, so the room shows
    assert!(console.backdrop(None).shows_room(true));
    console.on_menu(menu(None));
    assert!(console.backdrop(None).shows_room(true));
    // A game starts. The desktop's hiding tells it first, the menu later.
    assert!(!console.backdrop(Some(true)).shows_room(true));
    console.on_menu(menu(Some("Beat Saber.exe")));
    assert!(!console.backdrop(Some(true)).shows_room(true));
    // The host has no desktop: the menu alone keeps the game on screen
    assert!(!console.backdrop(None).shows_room(true));
    // The game ends: the desktop's visibility brings the room back before the next menu
    assert!(console.backdrop(Some(false)).shows_room(true));
    // The user keeps the host's scene
    console.set_room(Room::Scene);
    assert!(!console.backdrop(Some(false)).shows_room(true));
    assert_eq!(console.view().room, Room::Scene);
    console.set_room(Room::Passthrough);
    assert!(console.backdrop(Some(false)).shows_room(true));
    // The link ends while a game ran: no menu and no desktop, so the game is gone
    console.clear();
    assert!(console.backdrop(None).shows_room(true));
    // The choices survive the end of a link
    console.set_volume(Volume::P300);
    console.clear();
    assert_eq!(console.view().volume, Volume::P300);
}

#[test]
fn the_new_settings_fit_the_consoles_height() {
    let mut menu = menu(None);
    menu.games.extend((0..10).map(|i| Game { name: format!("Game {i}"), program: format!("g{i}.exe"), ..Default::default() }));
    assert_eq!(menu.listed().len(), MAX_GAMES);
    // Five games, six settings, the note and the buttons: about 990 dp, a little under a meter
    let tall = height(Some(&menu));
    assert!(tall > 900.0 && tall < 1000.0, "{tall}");
}
