//! The console as a Cranpose panel: the games to play and the main settings, over the host's
//! menu (see `console.rs`). A hold of the left menu button shows or hides it. A press on a button
//! sends the request to the host.
//!
//! Each opening of the console puts it in front of the head, in a `FollowingSubspace` of its own
//! that the panel's composition creates. The panel starts below and ahead of the head, tilted up like
//! a desk console. The user can carry it with a controller. The pose of the last carry is relative to
//! that subspace's origin, so the next opening puts the console at the same place relative to the
//! head.

use crate::{
    console::{self, size, Action, Game, Menu, Press, Room, View, Volume, RATES, SCALES},
    hub::{hub, LinkState, Snapshot},
    place,
    tracking::Read,
    ui::{text_style, ACCENT_COLOR, KEY_COLOR, MUTED_COLOR, PANEL_COLOR, STOP_COLOR, TEXT_COLOR, WARN_COLOR},
};
use cranpose::prelude::*;
use cranpose::text::FontWeight;
use cranpose_xr::{FollowMode, FollowTarget, FollowingSubspace, Hand, Pose, SpatialPanel, SubspaceModifier, Vibration};
use cranvr_proto::{message::buttons, wire::Nanos};
use std::time::Duration;

/// The width of a setting's name, in dp
const LABEL: f32 = 150.0;

/// Where the console starts, relative to the origin of its subspace, which lies 1 m ahead of the
/// head: 0.6 m ahead of the head and 0.4 m below it, tilted up toward it by 34 degrees. The offset
/// is in dp, a millimeter each.
const START_OFFSET: [f32; 3] = [0.0, -400.0, 400.0];
const START_TILT_DEGREES: f32 = 34.0;

/// Gives the console one frame of the headset: the left menu button, whose hold shows or hides it.
/// The frame loop calls it.
pub fn on_frame(read: &Read, now: Nanos) {
    let menu_down = read.frame.as_ref().is_some_and(|frame| frame.input.hands[0].buttons & buttons::MENU != 0);
    if hub().console_frame(menu_down, now) {
        // A click on the left hand tells the user that the hold took
        if let Some(haptics) = hub().headset().and_then(|headset| headset.haptics) {
            haptics.vibrate(Hand::Left, Vibration::new(0.4, Duration::from_millis(40)));
        }
    }
}

/// The console, while it is open.
#[composable]
pub fn ConsolePanel(view: MutableState<View>, snapshot: MutableState<Snapshot>) {
    let view = view.get();
    let state = snapshot.get().state;
    if view.open {
        // The subspace leaves the composition with the console, so the next opening snaps it to the
        // head again
        FollowingSubspace(FollowTarget::View(FollowMode::Snap), move || OpenConsole(view.clone(), state.clone()));
    }
}

#[composable]
fn OpenConsole(view: View, state: LinkState) {
    let height = console::height(view.menu.as_ref());
    let start = remember(|| {
        let default = SubspaceModifier::new().offset(START_OFFSET[0], START_OFFSET[1], START_OFFSET[2]).rotate(-START_TILT_DEGREES, 0.0, 0.0).pose();
        place::start_pose("console", default)
    })
    .with(|pose: &Pose| *pose);
    SpatialPanel(place::movable("console", SubspaceModifier::new().size(size::WIDTH, height), start, |_| {}), move || {
        let menu = view.menu.clone();
        let (room, volume) = (view.room, view.volume);
        let state = state.clone();
        Column(
            Modifier::empty().fill_max_size().background(PANEL_COLOR).rounded_corners(24.0).padding(size::PADDING),
            ColumnSpec::default().vertical_arrangement(LinearArrangement::spaced_by(size::GAP)),
            move || match &menu {
                Some(menu) => MenuRows(menu.clone(), room, volume),
                None => NoMenu(state.clone(), room, volume),
            },
        );
    });
}

fn bold(size: f32) -> TextStyle {
    text_style(size, TEXT_COLOR, Some(FontWeight::BOLD))
}

/// What the console says while the host sends no menu: its app is not running, or no link exists.
#[composable]
fn NoMenu(state: LinkState, room: Room, volume: Volume) {
    Text("cranvr", Modifier::empty().height(size::TITLE), bold(34.0));
    let (text, color) = match state {
        LinkState::Streaming => ("The host offers no menu. Start the cranvr app on the Mac.", WARN_COLOR),
        _ => ("Not connected to a host.", MUTED_COLOR),
    };
    Text(text, Modifier::empty().height(size::TEXT), text_style(22.0, color, None));
    // The settings of the client need no host
    Heading("Settings");
    ClientSettings(room, volume);
    CloseButtons(false);
}

#[composable]
fn MenuRows(menu: Menu, room: Room, volume: Volume) {
    let status = menu.status.clone();
    let stream = if status.refresh > 0.0 {
        let latency = status.latency_ms.map_or(String::new(), |ms| format!(", {ms:.0} ms"));
        format!("{:.0} Hz, {:.0} fps{latency}", status.refresh, status.fps)
    } else {
        "No headset stream".to_string()
    };
    Row(Modifier::empty().fill_max_width().height(size::TITLE), RowSpec::default(), move || {
        Text("cranvr", Modifier::empty().width(LABEL), bold(34.0));
        Text(stream.clone(), Modifier::empty().weight(1.0), text_style(22.0, MUTED_COLOR, None));
    });
    let running = menu.games.iter().find(|game| menu.runs(game)).map(|game| game.name.clone()).or(status.game.clone());
    let line = match running {
        Some(name) => format!("{name} makes {:.0} frames a second", status.game_fps),
        None => "No game runs".to_string(),
    };
    Text(line, Modifier::empty().height(size::TEXT), text_style(22.0, TEXT_COLOR, None));

    Heading("Play");
    if menu.games.is_empty() {
        Text("No games installed in cranvr's Steam", Modifier::empty().height(size::BUTTON), text_style(22.0, MUTED_COLOR, None));
    }
    for game in menu.listed() {
        GameRow(game.clone(), menu.runs(game));
    }

    Heading("Settings");
    let rates = RATES.iter().map(|&rate| (format!("{rate} Hz"), Press::Host(Action::Fps(rate)), menu.fps == rate)).collect();
    Setting("Refresh", rates);
    let sizes = SCALES.iter().map(|&scale| (format!("{:.0}%", scale * 100.0), Press::Host(Action::Scale(scale)), (menu.game_scale - scale).abs() < 0.01)).collect();
    Setting("Game size", sizes);
    Setting(
        "Anti-aliasing",
        vec![("Limit 2x".into(), Press::Host(Action::Msaa(2)), menu.msaa == 2), ("No limit".into(), Press::Host(Action::Msaa(0)), menu.msaa == 0)],
    );
    ClientSettings(room, volume);
    Text(
        "Refresh and game size restart the stream",
        Modifier::empty().height(size::TEXT),
        text_style(20.0, MUTED_COLOR, None),
    );
    CloseButtons(true);
}

/// The two settings that the client keeps for itself: what shows behind the panels while no game
/// runs, and how loud the sound plays.
#[composable]
fn ClientSettings(room: Room, volume: Volume) {
    Setting("Room", Room::ALL.iter().map(|&choice| (choice.label().into(), Press::Room(choice), room == choice)).collect());
    Setting("Volume", Volume::ALL.iter().map(|&choice| (choice.label(), Press::Volume(choice), volume == choice)).collect());
}

#[composable]
fn Heading(text: &'static str) {
    Text(text, Modifier::empty().height(size::HEADING), text_style(26.0, MUTED_COLOR, Some(FontWeight::BOLD)));
}

/// A game's button, with a Stop button beside it while it runs.
#[composable]
fn GameRow(game: Game, running: bool) {
    Row(
        Modifier::empty().fill_max_width(),
        RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(size::GAP)),
        move || {
            Choice(game.name.clone(), running, Press::Host(Action::Play(game.name.clone())), ACCENT_COLOR, true);
            if running {
                Choice("Stop".into(), false, Press::Host(Action::Stop), STOP_COLOR, false);
            }
        },
    );
}

/// A setting's name and its choices, the current one marked.
#[composable]
fn Setting(name: &'static str, choices: Vec<(String, Press, bool)>) {
    Row(
        Modifier::empty().fill_max_width(),
        RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(size::GAP)),
        move || {
            Text(name, Modifier::empty().width(LABEL).padding_symmetric(0.0, 8.0), text_style(22.0, TEXT_COLOR, None));
            for (label, press, selected) in choices.clone() {
                Choice(label, selected, press, ACCENT_COLOR, true);
            }
        },
    );
}

/// A button that does `press`. `fill` makes it share the row's width with its neighbors.
#[composable]
fn Choice(label: String, selected: bool, press: Press, selected_color: Color, fill: bool) {
    let mut modifier = Modifier::empty().height(size::BUTTON).background(if selected { selected_color } else { KEY_COLOR }).rounded_corners(12.0);
    modifier = if fill { modifier.weight(1.0) } else { modifier.padding_symmetric(24.0, 0.0) };
    Button(
        modifier,
        ButtonSpec::default(),
        move || run(&press),
        move || {
            Text(label.clone(), Modifier::empty(), bold(22.0));
        },
    );
}

/// Does what a button says: asks the host, or changes a setting of the client.
fn run(press: &Press) {
    match press {
        Press::Host(action) => hub().press(action),
        Press::Room(room) => hub().choose_room(*room),
        Press::Volume(volume) => hub().choose_volume(*volume),
    }
}

/// The buttons at the bottom. Close hides the console. Disconnect ends the link while it runs. Exit
/// closes the app. The host's menu has no Exit item, so the client adds its own.
#[composable]
fn CloseButtons(link: bool) {
    Row(
        Modifier::empty().fill_max_width(),
        RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(size::GAP)),
        move || {
            Button(
                Modifier::empty().height(size::BUTTON).background(KEY_COLOR).rounded_corners(12.0).weight(1.0),
                ButtonSpec::default(),
                || hub().close_console(),
                || {
                    Text("Close", Modifier::empty(), bold(22.0));
                },
            );
            if link {
                Button(
                    Modifier::empty().height(size::BUTTON).background(STOP_COLOR).rounded_corners(12.0).weight(1.0),
                    ButtonSpec::default(),
                    || hub().disconnect(),
                    || {
                        Text("Disconnect", Modifier::empty(), bold(22.0));
                    },
                );
            }
            Button(
                Modifier::empty().height(size::BUTTON).background(STOP_COLOR).rounded_corners(12.0).weight(1.0),
                ButtonSpec::default(),
                exit_app,
                || {
                    Text("Exit", Modifier::empty(), bold(22.0));
                },
            );
        },
    );
}

/// Ends the link, then closes the app. The hub outlives the activity, so a link that stayed open
/// would go on sending the headset's tracking to the host.
fn exit_app() {
    hub().disconnect();
    cranpose::request_exit();
}
