use super::*;

const MENU: &str = r#"{"games":[{"name":"Beat Saber","program":"Beat Saber.exe"},{"name":"Half-Life: Alyx","program":"hlvr.exe"}],
    "fps":120,"game_scale":0.6,"msaa":2,"low_latency":false,
    "status":{"refresh":120,"fps":119.4,"game_fps":117,"latency_ms":46.2,"game":"Beat Saber.exe"}}"#;

fn menu(game: Option<&str>) -> Menu {
    let mut menu = Menu::parse(MENU).unwrap();
    menu.status.game = game.map(str::to_string);
    menu
}

#[test]
fn the_menu_comes_from_the_hosts_json() {
    let menu = Menu::parse(MENU).unwrap();
    assert_eq!(menu.games, [Game { name: "Beat Saber".into(), program: "Beat Saber.exe".into() }, Game { name: "Half-Life: Alyx".into(), program: "hlvr.exe".into() }]);
    assert_eq!((menu.fps, menu.msaa), (120, 2));
    assert!((menu.game_scale - 0.6).abs() < 1e-6);
    assert_eq!(menu.status, Status { refresh: 120.0, fps: 119.4, game_fps: 117.0, latency_ms: Some(46.2), game: Some("Beat Saber.exe".into()) });
    assert!(menu.runs(&menu.games[0]) && !menu.runs(&menu.games[1]));
    // The host sends the file without a status while no headset streams, and an app may leave fields out
    let bare = Menu::parse(r#"{"games":[]}"#).unwrap();
    assert_eq!((bare.fps, bare.game_scale, bare.status.game), (120, 1.0, None));
    assert_eq!(Menu::parse("not json"), None);
    assert_eq!(Menu::parse(r#"{"fps":90}"#), None, "a text without games is no menu");
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
    menu.games.push(Game { name: "Third".into(), program: "third.exe".into() });
    assert!((height(Some(&menu)) - two - (size::BUTTON + size::GAP)).abs() < 1e-3);
    menu.games.extend((0..10).map(|i| Game { name: format!("Game {i}"), program: format!("g{i}.exe") }));
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
fn a_hold_of_the_menu_button_shows_or_hides_the_console() {
    let mut console = Console::default();
    let ms = 1_000_000;
    // A short press does nothing: the game gets it
    assert!(!console.on_frame(true, 1_000 * ms));
    assert!(!console.on_frame(true, 1_000 * ms + HOLD_NS - 1));
    assert!(!console.on_frame(false, 1_000 * ms + HOLD_NS + ms));
    assert!(!console.view().open);
    // A hold toggles once
    assert!(!console.on_frame(true, 2_000 * ms));
    assert!(console.on_frame(true, 2_000 * ms + HOLD_NS));
    assert!(!console.on_frame(true, 2_000 * ms + 2 * HOLD_NS), "one toggle for one hold");
    assert!(console.view().open);
    // Another hold hides it
    assert!(!console.on_frame(false, 3_000 * ms));
    assert!(!console.on_frame(true, 4_000 * ms));
    assert!(console.on_frame(true, 4_000 * ms + HOLD_NS));
    assert!(!console.view().open);
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
    menu.games.extend((0..10).map(|i| Game { name: format!("Game {i}"), program: format!("g{i}.exe") }));
    assert_eq!(menu.listed().len(), MAX_GAMES);
    // Five games, five settings, the note and the buttons: about 940 dp, a little under a meter
    let tall = height(Some(&menu));
    assert!(tall > 900.0 && tall < 1000.0, "{tall}");
}
