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
    assert_eq!((menu.fps, menu.msaa, menu.low_latency), (120, 2, false));
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
    assert_eq!(json(Action::LowLatency(true)), json!({"action": "set", "low_latency": true}));
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
fn the_console_stands_below_and_ahead_of_the_head_and_faces_it() {
    // A head at 1.6 m above the start that looks ahead
    let place = Place::below_ahead_of([0.0, 1.6, 0.0], [0.0, 0.0, 0.0, 1.0]);
    // 0.6 m ahead of the head is 0.4 m ahead of the subspace's origin: 0.4 m toward the viewer from it
    assert_eq!(place.offset[0], 0.0);
    assert!((place.offset[1] - 1200.0).abs() < 1e-2 && (place.offset[2] - 400.0).abs() < 1e-2, "{place:?}");
    assert_eq!((place.pitch, place.yaw), (-34.0, 0.0));
    // The same head turned 90 degrees to the left stands the console to its left, turned to face it
    let half = std::f32::consts::FRAC_PI_4;
    let left = Place::below_ahead_of([0.0, 1.6, 0.0], [0.0, half.sin(), 0.0, half.cos()]);
    assert!((left.offset[0] + 600.0).abs() < 1e-2 && (left.offset[2] - 1000.0).abs() < 1e-2, "{left:?}");
    assert!((left.yaw - 90.0).abs() < 1e-3, "{left:?}");
    // A head that nods down by 45 degrees still looks ahead for the console's sake
    let nod = std::f32::consts::FRAC_PI_8;
    let down = Place::below_ahead_of([0.0, 1.6, 0.0], [-nod.sin(), 0.0, 0.0, nod.cos()]);
    assert!((down.offset[2] - 400.0).abs() < 1e-2 && down.yaw.abs() < 1e-3, "{down:?}");
    // Straight down has no facing: the console goes ahead of the start
    let straight = std::f32::consts::FRAC_PI_4;
    let floor = Place::below_ahead_of([0.0, 1.6, 0.0], [-straight.sin(), 0.0, 0.0, straight.cos()]);
    assert!((floor.offset[2] - 400.0).abs() < 1e-1 && floor.yaw.abs() < 1e-3, "{floor:?}");
}

#[test]
fn the_first_menu_opens_the_console_and_a_game_closes_it() {
    let mut console = Console::default();
    assert!(!console.view().open);
    console.on_menu(menu(None));
    assert!(console.view().open && console.view().opened == 1);
    // The next menus change nothing while no game starts
    console.on_menu(menu(None));
    assert_eq!(console.view().opened, 1);
    // The user hides it. A menu without a change of the game keeps it hidden.
    console.close();
    console.on_menu(menu(None));
    assert!(!console.view().open);
    // A game starts: hidden. The game ends: shown again
    console.on_menu(menu(Some("Beat Saber.exe")));
    assert!(!console.view().open);
    console.on_menu(menu(Some("Beat Saber.exe")));
    console.on_menu(menu(None));
    assert!(console.view().open && console.view().opened == 2);
    // The link ends: no menu, no console
    console.clear();
    assert!(!console.view().open && console.view().menu.is_none());
}

#[test]
fn a_hold_of_the_menu_button_shows_the_console_where_the_head_is() {
    let mut console = Console::default();
    let head = Some(([1.0, 1.5, 0.0], [0.0, 0.0, 0.0, 1.0]));
    let ms = 1_000_000;
    // A short press does nothing: the game gets it
    assert!(!console.on_frame(head, true, 1_000 * ms));
    assert!(!console.on_frame(head, true, 1_000 * ms + HOLD_NS - 1));
    assert!(!console.on_frame(head, false, 1_000 * ms + HOLD_NS + ms));
    assert!(!console.view().open);
    // A hold toggles once, and the console stands at the head
    assert!(!console.on_frame(head, true, 2_000 * ms));
    assert!(console.on_frame(head, true, 2_000 * ms + HOLD_NS));
    assert!(!console.on_frame(head, true, 2_000 * ms + 2 * HOLD_NS), "one toggle for one hold");
    let view = console.view();
    assert!(view.open && (view.place.offset[0] - 1000.0).abs() < 1e-2, "{:?}", view.place);
    // Another hold hides it
    assert!(!console.on_frame(None, false, 3_000 * ms));
    assert!(!console.on_frame(None, true, 4_000 * ms));
    assert!(console.on_frame(None, true, 4_000 * ms + HOLD_NS));
    assert!(!console.view().open);
}
