use super::*;
use cranvr_common::menu::{Button, Buttons, Choice, Opt, Style};
use serde_json::json;

/// The pages as the app writes them: the games first, then the stream's settings
fn pages() -> Value {
    json!([
        {"id": "play", "title": "Play", "short": "Play", "listed": true, "items": [
            {"kind": "buttons", "title": "Beat Saber", "detail": "OpenComposite", "buttons": [
                {"label": "Play", "press": {"does": "run", "id": "game.play", "subject": "C:\\BS\\Beat Saber.exe"}, "style": "primary", "running": "Starting..."},
                {"label": "Options", "press": {"does": "open", "page": "game:C:\\BS\\Beat Saber.exe"}}
            ]}
        ]},
        {"id": "stream", "title": "Stream", "short": "Stream", "listed": true, "items": [
            {"kind": "choice", "id": "fps", "label": "Refresh rate", "options": [{"value": "90", "label": "90 Hz"}, {"value": "120", "label": "120 Hz"}], "value": "120", "restarts": true}
        ]},
        {"id": "game:C:\\BS\\Beat Saber.exe", "title": "Beat Saber", "short": "Options", "items": []}
    ])
}

fn menu_text() -> String {
    json!({"pages": pages(), "controls": {}}).to_string()
}

/// A menu with the pages only, as the app writes it before a headset streams
const BARE: &str = r#"{"pages":[],"controls":{}}"#;

/// A menu that knows whether `game` runs, as the console's menu does once the stream's state came
fn menu(game: Option<&str>) -> Menu {
    let mut menu = Menu::parse(&menu_text()).unwrap();
    menu.status.game = game.map(str::to_string);
    menu
}

/// The host's state with `game` and then its menu, as the host sends them
fn show(console: &mut Console, game: Option<&str>) {
    console.on_status(Status { game: game.map(str::to_string), ..Status::default() });
    console.on_menu(menu(None));
}

#[test]
fn the_menu_comes_from_the_hosts_json() {
    let menu = menu(None);
    assert_eq!(menu.pages.len(), 3);
    assert_eq!(menu.main().map(|page| page.id.as_str()), Some("play"));
    // The stream's state comes apart
    let status = Status::parse(r#"{"refresh":120,"fps":119.4,"game_fps":117,"latency_ms":46.2,"game":"Beat Saber.exe"}"#).unwrap();
    assert_eq!(status, Status { refresh: 120.0, fps: 119.4, game_fps: 117.0, latency_ms: Some(46.2), game: Some("Beat Saber.exe".into()) });
    assert_eq!(Status::parse(r#"{"refresh":null}"#).unwrap(), Status::default(), "no headset streams");
    assert_eq!(Status::parse("[1]"), None);
    // The menus list the pages after the main page that ask for it: a game's page is not listed
    assert_eq!(menu.listed(), [("stream".to_string(), "Stream".to_string())]);
    assert_eq!(menu.item(&Request::set("fps", "90")).and_then(Item::value), Some("120"));
    let bare = Menu::parse(BARE).unwrap();
    assert_eq!((bare.pages.len(), bare.status.game), (0, None));
    assert_eq!(Menu::parse("not json"), None);
    assert_eq!(Menu::parse(r#"{"games":[],"fps":90,"game_scale":1.0,"msaa":0,"controls":{}}"#), None, "the old menu is no menu");
    assert_eq!(Menu::parse(r#"{"pages":[{"id":"x"}],"controls":{}}"#), None, "a page that does not read");
    assert_eq!(Menu::parse(r#"{"pages":[]}"#), None, "the controls belong to the menu");
}

#[test]
fn the_apps_notice_shows_in_the_menu_only_while_there_is_one() {
    let with = Menu::parse(r#"{"pages":[],"controls":{},"notice":"The right eye stayed black. The game restarts."}"#).unwrap();
    assert_eq!(with.notice.as_deref(), Some("The right eye stayed black. The game restarts."));
    let without = Menu::parse(r#"{"pages":[],"controls":{},"notice":""}"#).unwrap();
    assert_eq!(without.notice, None);
}

#[test]
fn a_change_runs_until_the_menu_shows_its_value_or_a_new_notice() {
    let start = Instant::now();
    let menu = menu(None);
    let pending = Pending::new(Request::set("fps", "90"), None, start);
    assert_eq!(pending.progress(&menu, start + Duration::from_secs(10)), Progress::Running);
    assert_eq!(pending.progress(&menu, start + ANSWER_TIME + Duration::from_secs(1)), Progress::NoAnswer);
    // The Mac took it: the menu shows the new value
    let mut taken = menu.clone();
    if let Some(Item::Choice(choice)) = Arc::make_mut(&mut taken.pages)[1].items.first_mut() {
        choice.value = "90".into();
    }
    assert_eq!(pending.progress(&taken, start), Progress::Done);
    // The Mac says why not
    let mut refused = menu;
    refused.notice = Some("cranvr has no setting fps".into());
    assert_eq!(pending.progress(&refused, start), Progress::Done);
}

#[test]
fn an_action_runs_while_the_mac_marks_its_button_and_briefly_before_the_mac_can_say_so() {
    let start = Instant::now();
    let play = Request::run("game.play").of("C:\\BS\\Beat Saber.exe");
    let mut menu = menu(None);
    let pending = Pending::new(play.clone(), None, start);
    assert_eq!(pending.progress(&menu, start + Duration::from_secs(1)), Progress::Running, "the Mac's menu is on its way");
    assert_eq!(pending.progress(&menu, start + ECHO_TIME + Duration::from_millis(1)), Progress::Done, "a quick action is done");
    let Some(Item::Buttons(row)) = Arc::make_mut(&mut menu.pages)[0].items.first_mut() else { panic!() };
    row.buttons[0].busy = true;
    assert_eq!(pending.progress(&menu, start + Duration::from_secs(30)), Progress::Running, "the Mac starts the game");
    assert_eq!(pending.progress(&menu, start + ANSWER_TIME + Duration::from_secs(1)), Progress::NoAnswer);
    assert!(pending.is(&play) && !pending.is(&Request::run("steam.stop")));
}

#[test]
fn a_request_is_the_json_that_the_app_reads() {
    let request = Request::set("fps", "90");
    assert_eq!(request.to_json(), json!({"action": "set", "id": "fps", "value": "90"}));
    // The types that the app writes read back here
    let row = Buttons { title: "A".into(), buttons: vec![Button::run("Play", "game.play").of("C:\\a.exe").styled(Style::Primary)], ..Default::default() };
    let choice = Choice { id: "fps".into(), label: "Refresh".into(), options: vec![Opt::new("90", "90 Hz")], value: "90".into(), ..Default::default() };
    let page = Page { id: "x".into(), title: "X".into(), items: vec![Item::Buttons(row), Item::Choice(choice)], ..Default::default() };
    let text = json!({"pages": [page], "controls": {}}).to_string();
    assert_eq!(Menu::parse(&text).unwrap().pages[0].items.len(), 2);
}

#[test]
fn the_first_menu_opens_the_console_and_a_game_closes_it() {
    let mut console = Console::default();
    assert!(!console.view().open);
    show(&mut console, None);
    assert!(console.view().open);
    // The next menus change nothing while no game starts
    show(&mut console, None);
    assert!(console.view().open);
    // The user hides it. A menu without a change of the game keeps it hidden.
    console.close();
    show(&mut console, None);
    assert!(!console.view().open);
    // A game starts: hidden. The game ends: shown again
    show(&mut console, Some("Beat Saber.exe"));
    assert!(!console.view().open);
    show(&mut console, Some("Beat Saber.exe"));
    show(&mut console, None);
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
    let text = format!(r#"{{"pages":[],"controls":{}}}"#, mapped.to_json());
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
    // The title, two tabs, eight inputs, the menu hand, the controller models, two lines and the
    // buttons: about 930 dp, under a meter
    let tall = controls_height();
    assert!(tall > 800.0 && tall < 1000.0, "{tall}");
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
    show(&mut console, None);
    assert!(console.backdrop(None).shows_room(true));
    // A game starts. The desktop's hiding tells it first, the menu later.
    assert!(!console.backdrop(Some(true)).shows_room(true));
    show(&mut console, Some("Beat Saber.exe"));
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
fn a_notice_from_before_the_press_does_not_answer_it() {
    let start = Instant::now();
    let mut old = menu(None);
    old.notice = Some("An old notice".into());
    let change = Pending::new(Request::set("fps", "90"), old.notice.clone(), start);
    assert_eq!(change.progress(&old, start), Progress::Running);
    let mut newer = old.clone();
    newer.notice = Some("A new notice".into());
    assert_eq!(change.progress(&newer, start), Progress::Done);
}

#[test]
fn the_state_comes_apart_and_a_game_that_runs_at_the_connection_keeps_the_console_hidden() {
    let mut console = Console::default();
    // The host sends the state first
    console.on_status(Status { game: Some("Beat Saber.exe".into()), ..Status::default() });
    assert!(!console.view().open, "no menu yet");
    console.on_menu(menu(None));
    assert!(!console.view().open, "a game runs");
    assert_eq!(console.view().menu.unwrap().status.game.as_deref(), Some("Beat Saber.exe"));
    // A new menu keeps the state, and the game's end alone shows the console
    console.on_menu(menu(None));
    assert!(!console.view().open);
    console.on_status(Status::default());
    assert!(console.view().open, "the game ended");
}

#[test]
fn the_mac_keeps_the_room_and_the_volume_and_says_null_until_the_headset_told_its_own() {
    let unknown = Menu::parse(r#"{"pages":[],"controls":{},"headset":{"room":null,"volume":null}}"#).unwrap();
    assert_eq!(unknown.headset, HeadsetChoices::default());
    let known = Menu::parse(r#"{"pages":[],"controls":{},"headset":{"room":"scene","volume":200,"interface":"flat","theme":"amber"}}"#).unwrap();
    let expected = HeadsetChoices { room: Some(Room::Scene), volume: Some(Volume::P200), interface: Some(Interface::Flat), theme: Some(Theme::Amber) };
    assert_eq!(known.headset, expected);
    // A value that no choice has counts as none
    let odd = Menu::parse(r#"{"pages":[],"controls":{},"headset":{"room":"sky","volume":175}}"#).unwrap();
    assert_eq!(odd.headset, HeadsetChoices::default());
}
