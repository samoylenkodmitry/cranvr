//! What the hand menu lists, as data: its rows, their buttons and the menu's height. The composables
//! of `hand_menu.rs` draw it. Keeping it apart lets the tests cover the words and the choices.
//!
//! The menu lists every panel of the app with a button to show or hide it: the console, the
//! connection panel, the stream's numbers and each display of the Mac. A display has a second
//! button that puts it on the left hand or back in the room, and a third that removes it when the
//! user added it. Under a display stands the cost of showing it. At the bottom the menu adds a
//! display, puts the panels in front of the user again and opens the page of the controls.

use crate::{
    display_wish::{cost_line, Place},
    displays::DisplaysView,
    panel_layout::{Layout, PanelId},
};
use cranvr_proto::message::DesktopWish;

/// What a button of the menu does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    /// Shows the panel, or hides it when it shows
    TogglePanel(PanelId),
    /// Puts the display on the left hand, or back in the room when it is on the hand
    ToggleHand(u8),
    /// Removes a display that the user added
    RemoveDisplay(u8),
    AddDisplay,
    /// Puts the panels in front of the user again
    Recenter,
    /// Opens the console on its page of the controls
    OpenControls,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    Normal,
    /// The button is on: the panel shows, the display rides on the hand
    On,
    Danger,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuButton {
    pub label: String,
    pub action: MenuAction,
    pub kind: ButtonKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuRow {
    pub title: String,
    /// A line of muted text under the row
    pub detail: Option<String>,
    pub buttons: Vec<MenuButton>,
}

/// What the menu needs to know.
pub struct MenuInput<'a> {
    pub layout: &'a Layout,
    pub console_open: bool,
    pub displays: &'a DisplaysView,
    /// What the client asks of each display that shows, by its index
    pub wishes: [Option<DesktopWish>; crate::displays::SLOTS],
    /// Whether the host's protocol has displays: a host of an older version adds none
    pub host_has_displays: bool,
}

/// The sizes of the menu's parts in dp. A dp is a millimeter in the room. The text is 22 dp tall or
/// more: at the 45 cm that a hand holds the menu from the eyes, 22 mm is 2.8 degrees, which reads
/// well in a Quest 3.
pub mod size {
    pub const WIDTH: f32 = 470.0;
    pub const PADDING: f32 = 22.0;
    pub const GAP: f32 = 8.0;
    pub const TITLE: f32 = 40.0;
    pub const ROW: f32 = 52.0;
    pub const DETAIL: f32 = 30.0;
    /// The width of a button on a row
    pub const BUTTON: f32 = 92.0;
}

fn toggle(shown: bool, id: PanelId) -> MenuButton {
    MenuButton { label: if shown { "Hide" } else { "Show" }.into(), action: MenuAction::TogglePanel(id), kind: if shown { ButtonKind::On } else { ButtonKind::Normal } }
}

/// The rows of the menu.
pub fn rows(input: &MenuInput<'_>) -> Vec<MenuRow> {
    let layout = input.layout;
    let panel = |title: &str, id: PanelId, shown: bool| MenuRow { title: title.into(), detail: None, buttons: vec![toggle(shown, id)] };
    let mut rows = vec![
        panel("Console", PanelId::Console, input.console_open),
        panel("Connection", PanelId::Connection, layout.shown(PanelId::Connection)),
        panel("Stream numbers", PanelId::Stats, layout.shown(PanelId::Stats)),
    ];
    for display in &input.displays.displays {
        let stream = display.info.stream;
        let id = PanelId::Display(stream);
        let shown = layout.shown(id);
        let on_hand = layout.pinned() == Some(stream);
        let index = usize::from(stream - cranvr_proto::message::DESKTOP_STREAM);
        let wish = input.wishes.get(index).copied().flatten();
        let mut buttons = vec![
            toggle(shown, id),
            MenuButton { label: if on_hand { "Room" } else { "Hand" }.into(), action: MenuAction::ToggleHand(stream), kind: if on_hand { ButtonKind::On } else { ButtonKind::Normal } },
        ];
        if display.info.removable {
            buttons.push(MenuButton { label: "Remove".into(), action: MenuAction::RemoveDisplay(stream), kind: ButtonKind::Danger });
        }
        let place = if on_hand { "On the left hand" } else { "In the room" };
        let cost = cost_line(shown, &display.measured, wish.as_ref());
        rows.push(MenuRow { title: display.info.name.clone(), detail: Some(format!("{place}. {cost}")), buttons });
    }
    let mut actions = Vec::new();
    if input.displays.can_add {
        actions.push(MenuButton { label: "Add display".into(), action: MenuAction::AddDisplay, kind: ButtonKind::Normal });
    }
    actions.push(MenuButton { label: "Recenter".into(), action: MenuAction::Recenter, kind: ButtonKind::Normal });
    actions.push(MenuButton { label: "Controls".into(), action: MenuAction::OpenControls, kind: ButtonKind::Normal });
    rows.push(MenuRow { title: String::new(), detail: input.displays.notice.clone(), buttons: actions });
    rows
}

/// Where a display's panel stands, for the wish that it asks for.
pub fn place_of(layout: &Layout, stream: u8) -> Place {
    if layout.pinned() == Some(stream) {
        Place::Hand
    } else {
        Place::Room
    }
}

/// The menu's height in dp for these rows: the title, then each row with its detail and its gaps.
pub fn height(rows: &[MenuRow]) -> f32 {
    use size::*;
    let body: f32 = rows.iter().map(|row| ROW + GAP + if row.detail.is_some() { DETAIL } else { 0.0 }).sum();
    2.0 * PADDING + TITLE + GAP + body
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        display_wish::Measured,
        displays::DisplayView,
    };
    use cranvr_proto::message::DisplayInfo;

    fn info(stream: u8, name: &str, removable: bool) -> DisplayInfo {
        DisplayInfo { stream, name: name.into(), points: [1920, 1080], removable }
    }

    fn view(displays: Vec<DisplayInfo>, can_add: bool) -> DisplaysView {
        DisplaysView { displays: displays.into_iter().map(|info| DisplayView { info, measured: Measured::default(), streaming: false }).collect(), can_add, notice: None }
    }

    fn input<'a>(layout: &'a Layout, displays: &'a DisplaysView) -> MenuInput<'a> {
        MenuInput { layout, console_open: false, displays, wishes: Default::default(), host_has_displays: true }
    }

    #[test]
    fn the_menu_lists_every_panel_each_display_and_the_actions() {
        let layout = Layout::default();
        let displays = view(vec![info(1, "Mac display", false), info(2, "Display 2", true)], true);
        let rows = rows(&input(&layout, &displays));
        let titles: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, vec!["Console", "Connection", "Stream numbers", "Mac display", "Display 2", ""]);
        // The first display cannot be removed, an added one can
        assert_eq!(rows[3].buttons.len(), 2);
        assert_eq!(rows[4].buttons.len(), 3);
        assert_eq!(rows[4].buttons[2].action, MenuAction::RemoveDisplay(2));
        // The last row adds a display, puts the panels back and opens the controls
        let actions: Vec<MenuAction> = rows[5].buttons.iter().map(|b| b.action).collect();
        assert_eq!(actions, vec![MenuAction::AddDisplay, MenuAction::Recenter, MenuAction::OpenControls]);
    }

    #[test]
    fn a_host_that_cannot_add_gets_no_add_button() {
        let layout = Layout::default();
        let displays = view(vec![info(1, "Mac display", false)], false);
        let rows = rows(&input(&layout, &displays));
        assert!(rows.last().unwrap().buttons.iter().all(|b| b.action != MenuAction::AddDisplay));
    }

    #[test]
    fn a_button_says_what_a_press_does() {
        let mut layout = Layout::default();
        let displays = view(vec![info(1, "Mac display", false), info(2, "Display 2", true)], true);
        let rows_before = rows(&input(&layout, &displays));
        // Display 1 shows in the room: Hide and Hand. Display 2 is hidden: Show and Hand.
        assert_eq!((rows_before[3].buttons[0].label.as_str(), rows_before[3].buttons[1].label.as_str()), ("Hide", "Hand"));
        assert_eq!((rows_before[4].buttons[0].label.as_str(), rows_before[4].buttons[1].label.as_str()), ("Show", "Hand"));
        layout.toggle_pinned(2);
        layout.set_shown(PanelId::Display(2), true);
        let rows_after = rows(&input(&layout, &displays));
        assert_eq!((rows_after[4].buttons[0].label.as_str(), rows_after[4].buttons[1].label.as_str()), ("Hide", "Room"));
        assert_eq!(rows_after[4].buttons[1].kind, ButtonKind::On);
    }

    #[test]
    fn a_hidden_display_says_that_it_costs_nothing_and_a_shown_one_says_what_it_costs() {
        let mut layout = Layout::default();
        let mut displays = view(vec![info(1, "Mac display", false), info(2, "Display 2", true)], true);
        displays.displays[0].measured = Measured { size: Some([3840, 2160]), received_fps: 58.0, decoded_fps: 58.0, mbps: 1.2, decode_ms: 2.0 };
        let rows = rows(&input(&layout, &displays));
        assert_eq!(rows[4].detail.as_deref(), Some("In the room. Hidden: it costs nothing"));
        assert!(rows[3].detail.as_deref().unwrap().starts_with("In the room. 3840 x 2160, 58 a second, 1.2 Mbps"));
        layout.toggle_pinned(1);
        assert!(self::rows(&input(&layout, &displays))[3].detail.as_deref().unwrap().starts_with("On the left hand."));
    }

    #[test]
    fn the_console_button_follows_the_console() {
        let layout = Layout::default();
        let displays = view(vec![], false);
        let mut input = input(&layout, &displays);
        assert_eq!(rows(&input)[0].buttons[0].label, "Show");
        input.console_open = true;
        assert_eq!(rows(&input)[0].buttons[0].label, "Hide");
    }

    #[test]
    fn a_notice_of_the_host_stands_under_the_actions() {
        let layout = Layout::default();
        let mut displays = view(vec![info(1, "Mac display", false)], true);
        displays.notice = Some("The Mac made no display".into());
        let rows = rows(&input(&layout, &displays));
        assert_eq!(rows.last().unwrap().detail.as_deref(), Some("The Mac made no display"));
    }

    #[test]
    fn the_height_grows_with_the_rows_and_stays_within_reach() {
        let layout = Layout::default();
        let few = view(vec![info(1, "Mac display", false)], true);
        let many = view((1..=4).map(|s| info(s, "Display", s > 1)).collect(), false);
        let h = |d: &DisplaysView| height(&rows(&input(&layout, d)));
        assert!(h(&few) < h(&many));
        // Four displays still make a menu that is under 80 cm tall
        assert!(h(&many) < 800.0, "{}", h(&many));
        assert_eq!(place_of(&layout, 1), Place::Room);
    }
}
