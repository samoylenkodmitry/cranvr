//! The console as a Cranpose panel: the games to play and the main settings, over the host's
//! menu (see `console.rs`). The Console row of the hand menu shows or hides it, and so does a button
//! that the mapping gives the console. A press on a button
//! sends the request to the host.
//!
//! Each opening of the console puts it in front of the head, in a `FollowingSubspace` of its own
//! that the panel's composition creates. The panel starts below and ahead of the head, tilted up like
//! a desk console. The user can carry it with a controller. The pose of the last carry is relative to
//! that subspace's origin, so the next opening puts the console at the same place relative to the
//! head.

use crate::{
    console::{self, size, Pending, Press, Shown, View},
    hub::{hub, LinkState, Snapshot},
    page_model::{self, Key, Line, Local, Look, Name, Row, LABEL},
    place, settings,
    tracking::Read,
    ui::{text_style, ACCENT_COLOR, GOOD_COLOR, KEY_COLOR, MUTED_COLOR, PANEL_COLOR, STOP_COLOR, TEXT_COLOR, WARN_COLOR},
};
use cranvr_common::menu::Tone;
use cranpose::prelude::*;
use cranpose::text::FontWeight;
use cranpose_xr::{FollowMode, FollowTarget, TrackedDimensions, FollowingSubspace, Hand, Pose, SpatialPanel, SubspaceModifier, Vibration};
use cranvr_common::controls::{Control, Controls, Hand as ControlHand, MenuHand, ModelsInGame};
use cranvr_proto::wire::Nanos;
use std::time::{Duration, Instant};

/// The face of the key that a page is for, such as Play
const PRIMARY_COLOR: Color = Color(0.2, 0.5, 0.33, 1.0);
/// The width of an input's name on the Controls page, and of the buttons that step through its actions
const CONTROL_LABEL: f32 = 210.0;
const STEP: f32 = 48.0;

/// Where the console starts, relative to the origin of its subspace, which lies 1 m ahead of the
/// head: 0.6 m ahead of the head and 0.4 m below it, tilted up toward it by 34 degrees. The offset
/// is in dp, a millimeter each.
const START_OFFSET: [f32; 3] = [0.0, -400.0, 400.0];
const START_TILT_DEGREES: f32 = 34.0;

/// Gives the buttons of the controllers one frame of the headset (see `controls.rs`). A button that
/// the mapping gives the console opens it here. The frame loop calls it. A headset action that fires
/// gives the hand that fired it a click, so the user knows that the press took.
pub fn on_frame(read: &Read, now: Nanos) {
    let input = read.frame.as_ref().map_or_else(Default::default, |frame| frame.input);
    #[cfg(all(feature = "scripted-hand", target_os = "android"))]
    {
        crate::scripted::note_input(&input);
        if let Some(frame) = &read.frame {
            crate::scripted::note_poses(frame);
        }
    }
    for fire in hub().controls_frame(&input, now) {
        if let Some(haptics) = hub().headset().and_then(|headset| headset.haptics) {
            let hand = match fire.hand {
                ControlHand::Left => Hand::Left,
                ControlHand::Right => Hand::Right,
            };
            haptics.vibrate(hand, Vibration::new(0.4, Duration::from_millis(40)));
        }
    }
}

/// Where the console starts in its subspace: the place of the user's last carry, else the default.
pub fn start_place() -> Pose {
    let default = SubspaceModifier::new().offset(START_OFFSET[0], START_OFFSET[1], START_OFFSET[2]).rotate(-START_TILT_DEGREES, 0.0, 0.0).pose();
    place::start_pose("console", default)
}

/// The console, while it is open.
#[composable]
pub fn ConsolePanel(view: MutableState<View>, snapshot: MutableState<Snapshot>) {
    let view = view.get();
    let state = snapshot.get().state;
    if view.open {
        // The subspace leaves the composition with the console, so the next opening snaps it to the
        // head again
        FollowingSubspace(FollowTarget::View(FollowMode::Snap(TrackedDimensions::TranslationAndYaw)), move || OpenConsole(view.clone(), state.clone()));
    }
}

#[composable]
fn OpenConsole(view: View, state: LinkState) {
    // The request to the Mac that runs, from its press until the menu shows its result
    let pending = rememberMutableStateOf(|| None::<Pending>);
    let page = rememberMutableStateOf(|| console::take_page_request().or_else(|| settings::controls_page_on_launch().map(Shown::Controls)).unwrap_or_default());
    // The time moves a request that runs to "no answer" or to its end: a tick composes the rows
    // again while one runs
    let tick = rememberMutableStateOf(|| 0u32);
    // A press on a page's button in the hand menu while the console is open turns the page
    LaunchedEffect((), async move |_| loop {
        if let Some(shown) = console::take_page_request() {
            page.set(shown);
        }
        if pending.get_non_reactive().is_some() {
            tick.set(tick.get_non_reactive().wrapping_add(1));
        }
        delay(Duration::from_millis(250)).await;
    });
    let _ = tick.get();
    let shown = page.get();
    let local = Local { link: matches!(state, LinkState::Streaming | LinkState::Connecting), interface: view.interface };
    let no_menu = match state {
        LinkState::Streaming => "The host offers no menu. Start the cranvr app on the Mac.",
        _ => "Not connected to a host.",
    };
    let rows = match &shown {
        Shown::Controls(_) => Vec::new(),
        _ => page_model::console_rows(view.menu.as_ref(), &shown, local, no_menu, pending.get().as_ref(), Instant::now()),
    };
    let height = match shown {
        Shown::Controls(_) => console::controls_height(),
        _ => page_model::height(&rows),
    };
    // The log says each page that shows, with its size, so a test reads what the console drew
    let said = match &shown {
        Shown::Main => "the main page".to_string(),
        Shown::Host(id) => format!("the page {id}"),
        Shown::Controls(hand) => format!("the controls of the {}", hand.label()),
    };
    let keys = rows.iter().map(|row| match &row.line {
        Line::Keys { keys, .. } => keys.len(),
        _ => 0,
    });
    let summary = format!("{said}: {} rows, {} keys, {height:.0} dp tall", rows.len(), keys.sum::<usize>());
    LaunchedEffect(said, async move |_| log::info!("the console shows {summary}"));
    let start = remember(start_place).with(|pose: &Pose| *pose);
    let modifier = SubspaceModifier::new().size(size::WIDTH, height).layer_priority(crate::panel_layout::layer_rank::TEXT);
    let (controls, editable) = (view.controls, view.controls_editable);
    let notice = view.menu.as_ref().and_then(|menu| menu.notice.clone());
    // The 3D form draws the rows of the pages on a tray. The Controls page keeps its flat form.
    if view.interface.is_3d() && !matches!(shown, Shown::Controls(_)) {
        crate::console_tray::ConsoleTray(place::movable("console", modifier, start, |_| {}), rows, page, (pending, notice));
        return;
    }
    SpatialPanel(place::movable("console", modifier, start, |_| {}), move || {
        // The pointer over the console holds the pointing hand back from the game (see `hold.rs`)
        let hovered = crate::hover::hover_modifier(crate::hover::CONSOLE);
        let shown = shown.clone();
        let rows = rows.clone();
        let notice = notice.clone();
        Column(
            Modifier::empty().fill_max_size().then(hovered).background(PANEL_COLOR).rounded_corners(24.0).padding(size::PADDING),
            ColumnSpec::default().vertical_arrangement(LinearArrangement::spaced_by(size::GAP)),
            move || match &shown {
                Shown::Controls(hand) => ControlsPage(page, *hand, controls, editable),
                _ => {
                    for row in rows.clone() {
                        RowView(row, page, pending, notice.clone());
                    }
                }
            },
        );
    });
}

pub(crate) fn bold(size: f32) -> TextStyle {
    text_style(size, TEXT_COLOR, Some(FontWeight::BOLD))
}

pub(crate) fn tone_color(tone: Tone) -> Color {
    match tone {
        Tone::Normal => TEXT_COLOR,
        Tone::Muted => MUTED_COLOR,
        Tone::Good => GOOD_COLOR,
        Tone::Warn => WARN_COLOR,
    }
}

/// The name at the start of a row of keys: a setting's label, or a title with its detail. It gives
/// the width of each key after a title, which stand at the row's end.
#[composable]
pub fn RowName(name: &Name) -> Option<f32> {
    match name {
        Name::None => None,
        Name::Setting(label) => {
            Text(label.clone(), Modifier::empty().width(LABEL).padding_symmetric(0.0, 8.0), text_style(22.0, TEXT_COLOR, None));
            None
        }
        Name::Title { title, detail, width } => {
            let (title, detail) = (title.clone(), detail.clone());
            Column(Modifier::empty().weight(1.0), ColumnSpec::default(), move || {
                Text(title.clone(), Modifier::empty(), text_style(22.0, TEXT_COLOR, Some(FontWeight::BOLD)));
                if !detail.is_empty() {
                    Text(detail.clone(), Modifier::empty(), text_style(18.0, MUTED_COLOR, None));
                }
            });
            Some(*width)
        }
    }
}

/// One row of the console, as `page_model.rs` lays it out.
#[composable]
fn RowView(row: Row, page: MutableState<Shown>, pending: MutableState<Option<Pending>>, notice: Option<String>) {
    let height = row.height;
    match row.line {
        Line::Title { text, status } => {
            Row(Modifier::empty().fill_max_width().height(height), RowSpec::default(), move || {
                Text(text.clone(), Modifier::empty().weight(1.0), bold(34.0));
                if !status.is_empty() {
                    Text(status.clone(), Modifier::empty(), text_style(22.0, MUTED_COLOR, None));
                }
            });
        }
        Line::Heading(text) => {
            Text(text, Modifier::empty().height(height), text_style(26.0, MUTED_COLOR, Some(FontWeight::BOLD)));
        }
        Line::Text { text, tone } => {
            Text(text, Modifier::empty().fill_max_width().height(height), text_style(22.0, tone_color(tone), None));
        }
        Line::Keys { name, keys, .. } => {
            Row(
                Modifier::empty().fill_max_width().height(height),
                RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(size::GAP)),
                move || {
                    let width = RowName(&name);
                    for key in keys.clone() {
                        KeyView(key, width, page, pending, notice.clone());
                    }
                },
            );
        }
    }
}

/// A key of a row: its width is `width`, or a share of the row.
#[composable]
fn KeyView(key: Key, width: Option<f32>, page: MutableState<Shown>, pending: MutableState<Option<Pending>>, notice: Option<String>) {
    let color = match key.look {
        Look::Normal => KEY_COLOR,
        Look::Selected => ACCENT_COLOR,
        Look::Primary => PRIMARY_COLOR,
        Look::Danger => STOP_COLOR,
    };
    let modifier = Modifier::empty().height(size::BUTTON).background(color).rounded_corners(12.0);
    let modifier = match width {
        Some(width) => modifier.width(width),
        None => modifier.weight(1.0),
    };
    let press = key.press.clone();
    Button(
        modifier,
        ButtonSpec::default(),
        move || run(&press, page, pending, notice.clone()),
        move || {
            Text(key.label.clone(), Modifier::empty(), bold(20.0));
        },
    );
}

/// Does what a key says: asks the host, opens a page or changes a setting of the client. A request
/// to the host shows on its key until the menu shows its result.
pub(crate) fn run(press: &Press, page: MutableState<Shown>, pending: MutableState<Option<Pending>>, notice: Option<String>) {
    match press {
        Press::Host(request) => {
            pending.set(Some(Pending::new(request.clone(), notice, Instant::now())));
            hub().send_request(request);
        }
        Press::Open(id) => page.set(Shown::Host(id.clone())),
        Press::Back => page.set(Shown::Main),
        Press::Close => hub().close_console(),
        Press::Controls => page.set(Shown::Controls(ControlHand::Right)),
        Press::Disconnect => hub().disconnect(),
        Press::Exit => exit_app(),
    }
}

/// The hand that carries the hand menu while both controllers are tracked. The Mac keeps the choice
/// with the controls.
#[composable]
fn MenuHandSetting(current: MenuHand) {
    Text("Hand menu with two controllers", Modifier::empty().height(size::TEXT), text_style(22.0, TEXT_COLOR, None));
    Row(
        Modifier::empty().fill_max_width(),
        RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(size::GAP)),
        move || {
            for choice in MenuHand::ALL {
                let color = if choice == current { ACCENT_COLOR } else { KEY_COLOR };
                Button(
                    Modifier::empty().height(size::BUTTON).background(color).rounded_corners(12.0).weight(1.0),
                    ButtonSpec::default(),
                    move || hub().choose_menu_hand(choice),
                    move || {
                        Text(choice.label(), Modifier::empty(), bold(18.0));
                    },
                );
            }
        },
    );
}

/// When the controllers' models show in a game. The Mac keeps the choice with the controls. The
/// long middle choice takes the most room.
#[composable]
fn ModelsSetting(current: ModelsInGame) {
    Text("Controller models during a game", Modifier::empty().height(size::TEXT), text_style(22.0, TEXT_COLOR, None));
    Row(
        Modifier::empty().fill_max_width(),
        RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(size::GAP)),
        move || {
            for choice in ModelsInGame::ALL {
                let share = if choice == ModelsInGame::WhilePointing { 3.0 } else { 1.0 };
                let color = if choice == current { ACCENT_COLOR } else { KEY_COLOR };
                Button(
                    Modifier::empty().height(size::BUTTON).background(color).rounded_corners(12.0).weight(share),
                    ButtonSpec::default(),
                    move || hub().choose_models(choice),
                    move || {
                        Text(choice.label(), Modifier::empty(), bold(18.0));
                    },
                );
            }
        },
    );
}

/// The page for the buttons of the controllers: the two controllers as tabs, and for each input of
/// the shown controller the action that it does, with a button on each side that picks the previous
/// or the next action. The Mac's app keeps the mapping, so a change goes to it. Without a link the
/// page shows the mapping that the headset kept and changes nothing.
#[composable]
fn ControlsPage(page: MutableState<Shown>, hand: ControlHand, controls: Controls, editable: bool) {
    Text("Controls", Modifier::empty().height(size::TITLE), bold(34.0));
    Row(
        Modifier::empty().fill_max_width(),
        RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(size::GAP)),
        move || {
            for tab in ControlHand::BOTH {
                Button(
                    Modifier::empty().height(size::BUTTON).background(if tab == hand { ACCENT_COLOR } else { KEY_COLOR }).rounded_corners(12.0).weight(1.0),
                    ButtonSpec::default(),
                    move || page.set(Shown::Controls(tab)),
                    move || {
                        Text(tab.label(), Modifier::empty(), bold(22.0));
                    },
                );
            }
        },
    );
    for control in Control::ALL {
        ControlRow(hand, control, controls, editable);
    }
    MenuHandSetting(controls.menu_hand);
    ModelsSetting(controls.models_in_game);
    let (first, second) = if editable {
        ("A game still gets every button.", "A mouse action works on the Mac's desktop.")
    } else {
        ("Connect to the Mac to change the buttons.", "A game still gets every button.")
    };
    Text(first, Modifier::empty().height(size::TEXT), text_style(20.0, MUTED_COLOR, None));
    Text(second, Modifier::empty().height(size::TEXT), text_style(20.0, MUTED_COLOR, None));
    Row(
        Modifier::empty().fill_max_width(),
        RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(size::GAP)),
        move || {
            if editable {
                Button(
                    Modifier::empty().height(size::BUTTON).background(KEY_COLOR).rounded_corners(12.0).weight(1.0),
                    ButtonSpec::default(),
                    || hub().reset_controls(),
                    || {
                        Text("Standard", Modifier::empty(), bold(22.0));
                    },
                );
            }
            Button(
                Modifier::empty().height(size::BUTTON).background(KEY_COLOR).rounded_corners(12.0).weight(1.0),
                ButtonSpec::default(),
                move || page.set(Shown::Main),
                || {
                    Text("Back", Modifier::empty(), bold(22.0));
                },
            );
        },
    );
}

/// One input of a controller with the action that it does. The right controller has no menu button:
/// its row says so and keeps the row's height, so the page does not change its size with the tab.
#[composable]
fn ControlRow(hand: ControlHand, control: Control, controls: Controls, editable: bool) {
    let action = controls.action(hand, control);
    let exists = control.exists(hand);
    Row(
        Modifier::empty().fill_max_width().height(size::BUTTON),
        RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(size::GAP)),
        move || {
            Text(control.label(hand), Modifier::empty().width(CONTROL_LABEL).padding_symmetric(0.0, 8.0), text_style(20.0, TEXT_COLOR, None));
            if !exists {
                Text("This controller has none", Modifier::empty().weight(1.0).padding_symmetric(0.0, 8.0), text_style(20.0, MUTED_COLOR, None));
            } else if editable {
                Button(
                    Modifier::empty().width(STEP).height(size::BUTTON).background(KEY_COLOR).rounded_corners(12.0),
                    ButtonSpec::default(),
                    move || hub().step_control(hand, control, -1),
                    || {
                        Text("<", Modifier::empty(), bold(22.0));
                    },
                );
                Text(action.label(), Modifier::empty().weight(1.0).padding_symmetric(0.0, 8.0), bold(20.0));
                Button(
                    Modifier::empty().width(STEP).height(size::BUTTON).background(KEY_COLOR).rounded_corners(12.0),
                    ButtonSpec::default(),
                    move || hub().step_control(hand, control, 1),
                    || {
                        Text(">", Modifier::empty(), bold(22.0));
                    },
                );
            } else {
                Text(action.label(), Modifier::empty().weight(1.0).padding_symmetric(0.0, 8.0), bold(20.0));
            }
        },
    );
}

/// Ends the link, then closes the app. The hub outlives the activity, so a link that stayed open
/// would go on sending the headset's tracking to the host.
fn exit_app() {
    hub().disconnect();
    cranpose::request_exit();
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranpose_testing::robot::{RobotTestRule, TestRenderer};
    use cranvr_common::menu::{Button, Buttons, Choice, Item, Opt, Page, Style, Toggle};

    /// The console draws a page that the Mac's app made up, with no code of its own for its items:
    /// a headless shell composes the rows and the test reads the words on the screen.
    #[test]
    fn a_made_up_item_shows_in_the_console_with_no_code_of_its_own() {
        let page = Page {
            id: "new".into(),
            title: "New".into(),
            items: vec![
                Item::Choice(Choice { id: "new.choice".into(), label: "A new one".into(), options: vec![Opt::new("a", "First"), Opt::new("b", "Second")], value: "a".into(), ..Default::default() }),
                Item::Toggle(Toggle { id: "new.toggle".into(), label: "A new switch".into(), on: true, ..Default::default() }),
                Item::Buttons(Buttons { title: "A new action".into(), buttons: vec![Button::run("Do it", "new.action").styled(Style::Danger)], ..Default::default() }),
            ],
            ..Default::default()
        };
        let rows = page_model::page_rows(&page, None, crate::console::Interface::Flat);
        let mut robot = RobotTestRule::new(700, 1200, TestRenderer::default(), move || {
            let page = rememberMutableStateOf(Shown::default);
            let pending = rememberMutableStateOf(|| None::<Pending>);
            for row in rows.clone() {
                RowView(row, page, pending, None);
            }
        });
        let texts = robot.get_all_text();
        for words in ["A new one", "First", "Second", "A new switch", "On", "Off", "A new action", "Do it"] {
            assert!(texts.iter().any(|text| text == words), "{words:?} in {texts:?}");
        }
    }
}
