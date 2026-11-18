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
    assert!(height(None) < 250.0);
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
