use super::*;
use crate::console::Status;
use cranvr_common::{
    controls::Controls,
    menu::{Button, Buttons, Choice, Opt, Toggle},
};
use std::time::Duration;

fn local() -> Local {
    Local { link: true, interface: Interface::Flat }
}

fn stream() -> Page {
    Page {
        id: "stream".into(),
        title: "Stream".into(),
        short: "Stream".into(),
        listed: true,
        items: vec![
            Item::Heading { text: "Stream".into() },
            Item::Choice(Choice { id: "fps".into(), label: "Refresh rate".into(), options: vec![Opt::new("72", "72 Hz"), Opt::new("90", "90 Hz"), Opt::new("120", "120 Hz")], value: "120".into(), ..Default::default() }),
            Item::Choice(Choice {
                id: "game.openvr".into(),
                subject: Some("C:\\a.exe".into()),
                label: "OpenVR".into(),
                options: vec![Opt::new("default", "As in Settings (OpenComposite)"), Opt::new("opencomposite", "OpenComposite"), Opt::new("steamvr", "SteamVR"), Opt::new("off", "Off")],
                value: "default".into(),
                ..Default::default()
            }),
            Item::Toggle(Toggle { id: "game_audio".into(), label: "Game sound in the headset".into(), on: true, ..Default::default() }),
            Item::Text { text: "A line of words that is much longer than one line of the console can hold at its size".into(), tone: Tone::Muted },
        ],
    }
}

fn play() -> Page {
    Page {
        id: "play".into(),
        title: "Play".into(),
        short: "Play".into(),
        listed: true,
        items: vec![Item::Buttons(Buttons {
            title: "Beat Saber".into(),
            detail: "OpenComposite".into(),
            buttons: vec![Button::run("Play", "game.play").of("C:\\bs.exe").running("Starting...", false), Button::open("Options", "game:C:\\bs.exe")],
            hint: String::new(),
        })],
    }
}

fn menu() -> Menu {
    Menu { pages: vec![play(), stream()].into(), status: Status::default(), notice: None, controls: Controls::default(), headset: Default::default() }
}

#[test]
fn a_short_choice_takes_one_row_and_a_long_one_puts_its_options_in_rows_of_their_own() {
    let rows = page_rows(&stream(), None, Interface::ThreeD);
    let lines: Vec<&Line> = rows.iter().map(|row| &row.line).collect();
    assert!(matches!(lines[0], Line::Heading(_)));
    assert!(matches!(lines[1], Line::Keys { name: Name::Setting(label), keys, form: Form::Steps } if label == "Refresh rate" && keys.len() == 3));
    // The long first option does not fit beside the name: the name has a line, the options their rows
    assert!(matches!(lines[2], Line::Text { text, .. } if text == "OpenVR"));
    let Line::Keys { name: Name::None, keys, .. } = lines[3] else { panic!("{:?}", lines[3]) };
    assert_eq!(keys[0].label, "As in Settings (OpenComposite)");
    let packed: usize = rows[3..].iter().take_while(|row| matches!(row.line, Line::Keys { name: Name::None, .. })).map(|row| match &row.line {
        Line::Keys { keys, .. } => keys.len(),
        _ => 0,
    }).sum();
    assert_eq!(packed, 4, "every option has a key");
    // A toggle has its name and the keys On and Off, the current one selected
    let toggle = rows.iter().find(|row| matches!(&row.line, Line::Keys { name: Name::Title { title, .. }, .. } if title == "Game sound in the headset")).unwrap();
    let Line::Keys { keys, form, .. } = &toggle.line else { panic!() };
    assert_eq!((keys[0].label.as_str(), keys[0].look, keys[1].look), ("On", Look::Selected, Look::Normal));
    // The 3D form draws the toggle as a switch and the rate as a slider, which needs a line of values
    assert_eq!(*form, Form::Switch);
    assert_eq!(rows[1].height, size::BUTTON + crate::spatial_ui::STEP_LABELS);
    // The flat form keeps the rate's keys in a row of a key's height
    let flat = page_rows(&stream(), None, Interface::Flat);
    assert!(matches!(&flat[1].line, Line::Keys { form: Form::Keys, .. }));
    assert_eq!(flat[1].height, size::BUTTON);
    // A long line takes two lines of text
    assert_eq!(rows.last().unwrap().height, 2.0 * size::TEXT);
}

#[test]
fn the_selected_option_is_the_value_and_a_press_sends_its_value() {
    let rows = page_rows(&stream(), None, Interface::Flat);
    let Line::Keys { keys, .. } = &rows[1].line else { panic!() };
    assert_eq!(keys.iter().map(|key| key.look).collect::<Vec<_>>(), [Look::Normal, Look::Normal, Look::Selected]);
    assert_eq!(keys[1].press, Press::Host(Request::set("fps", "90")));
}

#[test]
fn a_pressed_option_says_changing_and_a_pressed_action_says_its_running_words() {
    let now = Instant::now();
    let menu = menu();
    let change = Pending::new(Request::set("fps", "90"), None, now);
    let rows = page_rows(&stream(), Some((&change, change.progress(&menu, now))), Interface::Flat);
    let Line::Keys { keys, .. } = &rows[1].line else { panic!() };
    assert_eq!(keys[1].label, "Changing...");
    let start = Pending::new(Request::run("game.play").of("C:\\bs.exe"), None, now);
    let rows = page_rows(&play(), Some((&start, start.progress(&menu, now))), Interface::Flat);
    let Line::Keys { keys, .. } = &rows[0].line else { panic!() };
    assert_eq!(keys[0].label, "Starting...");
    // No answer for a long time: the key says so
    let late = now + crate::console::ANSWER_TIME + Duration::from_secs(1);
    let rows = page_rows(&stream(), Some((&change, change.progress(&menu, late))), Interface::Flat);
    let Line::Keys { keys, .. } = &rows[1].line else { panic!() };
    assert_eq!(keys[1].label, "90 Hz: no answer");
}

#[test]
fn the_main_page_has_the_games_the_links_to_the_pages_the_clients_settings_and_the_buttons() {
    let menu = menu();
    let rows = console_rows(Some(&menu), &Shown::Main, local(), "", None, Instant::now());
    assert!(matches!(&rows[0].line, Line::Title { text, .. } if text == "cranvr"));
    assert!(find_key(&rows, &Press::Host(Request::run("game.play").of("C:\\bs.exe"))).is_some());
    let (row, key) = find_key(&rows, &Press::Open("stream".into())).expect("a link to the stream page");
    assert_eq!(key, 0);
    assert!(matches!(&rows[row - 1].line, Line::Heading(text) if text == "Settings"));
    assert!(find_key(&rows, &Press::Disconnect).is_some());
    // Without a link the bottom has no Disconnect
    let rows = console_rows(Some(&menu), &Shown::Main, Local { link: false, interface: Interface::Flat }, "", None, Instant::now());
    assert!(find_key(&rows, &Press::Disconnect).is_none());
}

#[test]
fn a_page_of_the_host_has_its_title_its_items_and_back() {
    let menu = menu();
    let rows = console_rows(Some(&menu), &Shown::Host("stream".into()), local(), "", None, Instant::now());
    assert!(matches!(&rows[0].line, Line::Title { text, .. } if text == "Stream"));
    // The page's heading of its own title shows once, as the title
    assert!(!rows.iter().any(|row| matches!(&row.line, Line::Heading(text) if text == "Stream")));
    assert!(find_key(&rows, &Press::Back).is_some());
    assert!(find_key(&rows, &Press::Host(Request::set("fps", "72"))).is_some());
    // A page that the menu lacks shows the main page
    let rows = console_rows(Some(&menu), &Shown::Host("gone".into()), local(), "", None, Instant::now());
    assert!(matches!(&rows[0].line, Line::Title { text, .. } if text == "cranvr"));
}

#[test]
fn the_height_and_the_place_of_each_key_follow_from_the_rows() {
    let menu = menu();
    let rows = console_rows(Some(&menu), &Shown::Host("stream".into()), local(), "", None, Instant::now());
    let total = height(&rows);
    let sum: f32 = rows.iter().map(|row| row.height).sum::<f32>() + (rows.len() - 1) as f32 * size::GAP + 2.0 * size::PADDING;
    assert!((total - sum).abs() < 1e-3);
    // The refresh rate's row is the second: the title, then the choice, as the page's heading of its
    // own title shows as the title. Its keys share the room right of the name.
    let (row, key) = find_key(&rows, &Press::Host(Request::set("fps", "72"))).unwrap();
    assert_eq!((row, key), (1, 0));
    let [x, y] = key_point(&rows, row, key).unwrap();
    let each = (INNER - LABEL - 3.0 * size::GAP) / 3.0;
    assert!((x - (size::PADDING + LABEL + size::GAP + each / 2.0)).abs() < 1e-3, "{x}");
    let top = size::PADDING + size::TITLE + size::GAP;
    assert!((y - (top + size::BUTTON / 2.0)).abs() < 1e-3, "{y}");
    // A title's keys stand at the right end
    let main = console_rows(Some(&menu), &Shown::Main, local(), "", None, Instant::now());
    let (row, key) = find_key(&main, &Press::Open("game:C:\\bs.exe".into())).unwrap();
    let [x, _] = key_point(&main, row, key).unwrap();
    assert!((x - (size::PADDING + INNER - KEY / 2.0)).abs() < 1e-3, "{x}");
    assert!(total < 900.0, "{total}");
}

#[test]
fn the_console_without_a_menu_says_why_and_keeps_its_buttons() {
    let rows = console_rows(None, &Shown::Main, Local { link: false, interface: Interface::Flat }, "Not connected to a host.", None, Instant::now());
    assert!(matches!(&rows[1].line, Line::Text { text, .. } if text == "Not connected to a host."));
    assert!(find_key(&rows, &Press::Exit).is_some());
    assert!(find_key(&rows, &Press::Disconnect).is_none());
    assert!(height(&rows) < 500.0);
}

#[test]
fn a_long_line_breaks_at_words() {
    assert_eq!(lines_of("short"), 1);
    assert_eq!(lines_of(&"word ".repeat(20)), 3);
    assert_eq!(lines_of(""), 1);
}

#[test]
fn a_toggle_whose_name_fits_beside_its_keys_takes_one_row_and_a_longer_one_takes_a_row_above_them() {
    let toggle = |label: &str| Item::Toggle(Toggle { id: "t".into(), label: label.into(), on: true, ..Default::default() });
    let page = Page { id: "p".into(), title: "P".into(), short: "P".into(), listed: true, items: vec![toggle("Game sound in the headset"), toggle("Panel pixels while a game runs")] };
    let rows = page_rows(&page, None, Interface::ThreeD);
    let lines: Vec<&Line> = rows.iter().map(|row| &row.line).collect();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(matches!(lines[0], Line::Keys { name: Name::Title { title, .. }, form: Form::Switch, .. } if title == "Game sound in the headset"));
    assert!(matches!(lines[1], Line::Text { text, .. } if text == "Panel pixels while a game runs"));
    assert!(matches!(lines[2], Line::Keys { name: Name::Title { title, width, .. }, keys, form: Form::Switch } if title.is_empty() && *width == SWITCH && keys.len() == 2));
}
