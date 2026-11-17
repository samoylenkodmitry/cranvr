//! The console as a Cranpose panel: the games to play and the main settings, over the host's
//! menu (see `console.rs`). The panel stands below and ahead of the head, tilted up like a desk
//! console, and the user can carry it with a controller. A hold of the left menu button shows or
//! hides it. A press on a button sends the request to the host.

use crate::{
    console::{self, size, Action, Game, Menu, View, RATES, SCALES},
    hub::{hub, LinkState, Snapshot},
    tracking::Read,
    ui::{text_style, ACCENT_COLOR, KEY_COLOR, MUTED_COLOR, PANEL_COLOR, STOP_COLOR, TEXT_COLOR, WARN_COLOR},
};
use cranpose::prelude::*;
use cranpose::text::FontWeight;
use cranpose_xr::{Hand, SpatialPanel, SubspaceModifier, Vibration};
use cranvr_proto::{message::buttons, wire::Nanos};
use std::time::Duration;

/// The width of a setting's name, in dp
const LABEL: f32 = 150.0;

/// Gives the console one frame of the headset: its head, for the place that the console takes
/// when it opens, and the left menu button, whose hold shows or hides it. The frame loop calls it.
pub fn on_frame(read: &Read, now: Nanos) {
    let (head, menu_down) = match &read.frame {
        Some(frame) => (Some((frame.head.pose.position, frame.head.pose.orientation)), frame.input.hands[0].buttons & buttons::MENU != 0),
        None => (None, false),
    };
    if hub().console_frame(head, menu_down, now) {
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
        let height = console::height(view.menu.as_ref());
        let place = view.place;
        SpatialPanel(
            SubspaceModifier::new()
                .size(size::WIDTH, height)
                .offset(place.offset[0], place.offset[1], place.offset[2])
                .rotate(place.pitch, place.yaw, 0.0)
                .movable(),
            move || {
                let menu = view.menu.clone();
                let state = state.clone();
                Column(
                    Modifier::empty().fill_max_size().background(PANEL_COLOR).rounded_corners(24.0).padding(size::PADDING),
                    ColumnSpec::default().vertical_arrangement(LinearArrangement::spaced_by(size::GAP)),
                    move || match &menu {
                        Some(menu) => MenuRows(menu.clone()),
                        None => NoMenu(state.clone()),
                    },
                );
            },
        );
    }
}

fn bold(size: f32) -> TextStyle {
    text_style(size, TEXT_COLOR, Some(FontWeight::BOLD))
}

/// What the console says while the host sends no menu: its app is not running, or no link exists.
#[composable]
fn NoMenu(state: LinkState) {
    Text("cranvr", Modifier::empty().height(size::TITLE), bold(34.0));
    let (text, color) = match state {
        LinkState::Streaming => ("The host offers no menu. Start the cranvr app on the Mac.", WARN_COLOR),
        _ => ("Not connected to a host.", MUTED_COLOR),
    };
    Text(text, Modifier::empty().height(size::TEXT), text_style(22.0, color, None));
    CloseButtons(false);
}

#[composable]
fn MenuRows(menu: Menu) {
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
    let rates = RATES.iter().map(|&rate| (format!("{rate} Hz"), Action::Fps(rate), menu.fps == rate)).collect();
    Setting("Refresh", rates);
    let sizes = SCALES.iter().map(|&scale| (format!("{:.0}%", scale * 100.0), Action::Scale(scale), (menu.game_scale - scale).abs() < 0.01)).collect();
    Setting("Game size", sizes);
    Setting("Anti-aliasing", vec![("Limit 2x".into(), Action::Msaa(2), menu.msaa == 2), ("No limit".into(), Action::Msaa(0), menu.msaa == 0)]);
    Setting("Latency", vec![("Low latency".into(), Action::LowLatency(!menu.low_latency), menu.low_latency)]);
    Text(
        "Refresh, game size and latency restart the stream",
        Modifier::empty().height(size::TEXT),
        text_style(20.0, MUTED_COLOR, None),
    );
    CloseButtons(true);
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
            Choice(game.name.clone(), running, Action::Play(game.name.clone()), ACCENT_COLOR, true);
            if running {
                Choice("Stop".into(), false, Action::Stop, STOP_COLOR, false);
            }
        },
    );
}

/// A setting's name and its choices, the current one marked.
#[composable]
fn Setting(name: &'static str, choices: Vec<(String, Action, bool)>) {
    Row(
        Modifier::empty().fill_max_width(),
        RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(size::GAP)),
        move || {
            Text(name, Modifier::empty().width(LABEL).padding_symmetric(0.0, 8.0), text_style(22.0, TEXT_COLOR, None));
            for (label, action, selected) in choices.clone() {
                Choice(label, selected, action, ACCENT_COLOR, true);
            }
        },
    );
}

/// A button that sends `action`. `fill` makes it share the row's width with its neighbors.
#[composable]
fn Choice(label: String, selected: bool, action: Action, selected_color: Color, fill: bool) {
    let mut modifier = Modifier::empty().height(size::BUTTON).background(if selected { selected_color } else { KEY_COLOR }).rounded_corners(12.0);
    modifier = if fill { modifier.weight(1.0) } else { modifier.padding_symmetric(24.0, 0.0) };
    Button(
        modifier,
        ButtonSpec::default(),
        move || hub().press(&action),
        move || {
            Text(label.clone(), Modifier::empty(), bold(22.0));
        },
    );
}

/// The Close button, and the Disconnect button when the link runs.
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
        },
    );
}
