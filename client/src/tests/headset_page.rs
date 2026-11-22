use super::*;
use crate::{
    displays::{DisplayView, DisplaysView},
    display_wish::Measured,
    menu_model::{rows, MenuInput},
    menu_place::MenuPlace,
    panel_layout::Layout,
};
use cranvr_common::menu::Item;
use cranvr_proto::message::DisplayInfo;

fn input_rows(layout: &Layout, displays: &DisplaysView) -> Vec<MenuRow> {
    let pages = [("stream".to_string(), "Stream".to_string())];
    rows(&MenuInput { layout, console_open: true, displays, wishes: Default::default(), place: MenuPlace::Hand(cranvr_common::controls::Hand::Left), adding: false, pages: &pages, costs: false })
}

fn displays() -> DisplaysView {
    let info = |stream: u8, name: &str, removable: bool| DisplayInfo { stream, name: name.into(), points: [1920, 1080], removable };
    let measured = Measured { size: Some([3840, 2160]), received_fps: 58.0, decoded_fps: 58.0, mbps: 1.2, decode_ms: 2.0 };
    DisplaysView {
        displays: vec![DisplayView { info: info(DESKTOP_STREAM, "Mac display", false), measured, streaming: true }, DisplayView { info: info(DESKTOP_STREAM + 1, "Display 2", true), measured: Measured::default(), streaming: false }],
        can_add: true,
        notice: None,
    }
}

#[test]
fn the_headsets_page_has_its_panels_its_displays_with_their_place_and_its_actions() {
    let layout = Layout::default();
    let displays = displays();
    let page = page(&input_rows(&layout, &displays));
    let rows: Vec<&Buttons> = page.items.iter().filter_map(|item| if let Item::Buttons(row) = item { Some(row) } else { None }).collect();
    let titles: Vec<&str> = rows.iter().map(|row| row.title.as_str()).collect();
    // The pages of the Mac are the Mac's own, so their row stays out
    assert_eq!(titles, ["Console", "Connection", "Stream numbers", "Mac display", "Display 2", "", ""]);
    // A display says where it stands and whether it shows, not its cost, which changes every second
    assert_eq!(rows[3].detail, "In the room. Shown");
    assert_eq!(rows[4].detail, "In the room. Hidden");
    assert!(rows[0].buttons[0].runs("headset.panel", Some("console")));
    assert_eq!(rows[0].buttons[0].label, "Hide", "the console is open");
    assert!(rows[4].buttons[2].runs("headset.remove", Some("2")));
    assert!(rows[5].buttons[0].runs("headset.add_display", None));
    assert!(rows[6].buttons.iter().any(|button| button.runs("headset.exit", None)));
}

#[test]
fn a_press_of_the_mac_does_what_the_menus_button_does() {
    let layout = Layout::default();
    let displays = displays();
    // Every button of the page reads back as the action of the menu's button that made it
    for row in input_rows(&layout, &displays) {
        for button in row.buttons {
            let Some((id, subject)) = request_of(button.action) else {
                assert!(matches!(button.action, MenuAction::OpenPage(_)));
                continue;
            };
            let request = Request::Run { id: id.into(), subject };
            assert_eq!(remote(&request), Some(Remote::Menu(button.action)), "{request:?}");
        }
    }
    assert_eq!(remote(&Request::run("headset.menu")), Some(Remote::OpenMenu));
    assert_eq!(remote(&Request::run("headset.exit")), Some(Remote::Exit));
    // A display that cannot exist, a panel that is no panel and a change of a setting are no presses
    for wrong in [Request::run("headset.hand").of("9"), Request::run("headset.panel").of("display:0"), Request::run("headset.panel").of("sky"), Request::run("headset.format"), Request::set("headset.recenter", "on")] {
        assert_eq!(remote(&wrong), None, "{wrong:?}");
    }
}

#[test]
fn the_cost_of_a_display_does_not_change_what_the_headsets_page_reads() {
    let before = displays();
    let mut after = displays();
    after.displays[0].measured.received_fps = 31.0;
    after.displays[0].measured.mbps = 4.5;
    after.displays[1].streaming = true;
    assert_ne!(before, after);
    assert_eq!(before.clone().without_costs(), after.clone().without_costs());
    // The page from the view without costs is the page from the full view
    let layout = Layout::default();
    assert_eq!(page(&input_rows(&layout, &before)), page(&input_rows(&layout, &after.without_costs())));
    // A display that the user hides changes what the page reads
    let mut hidden = Layout::default();
    hidden.set_shown(PanelId::Display(DESKTOP_STREAM), false);
    assert_ne!(page(&input_rows(&layout, &before)), page(&input_rows(&hidden, &before)));
}
