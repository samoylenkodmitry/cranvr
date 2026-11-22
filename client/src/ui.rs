//! The app's composables: the connect panel, the stats panel, the streamed scene and the frame
//! effect that feeds the host with the headset's tracking.
//!
//! In a headset each panel is a layer of the compositor and a controller's trigger presses its
//! buttons. The game's picture is a streamed scene under the panels. In a flat window the panels
//! draw side by side. On a phone or a tablet (`PhoneApp`) the picture fills the screen and a tap
//! opens the same two panels over it. Every composable here is plain Cranpose. The state that the
//! network threads produce arrives as a [`Snapshot`], which a `LaunchedEffect` copies into Cranpose
//! state a few times a second.

use crate::{
    console::Backdrop,
    controls::{beams_wanted, pointer_mapping},
    console_panel::{self, ConsolePanel},
    discovery::FoundHost,
    display_panel::RoomDisplay,
    displays::displays,
    hand_menu::{self, HandPanels, HandState},
    hands,
    headset_page::Remote,
    menu_model::{self, MenuInput},
    hold::{HoldFrame, PanelHold},
    hover,
    menu_place::{self, PlaceFrame, Placer},
    palm::{PalmFrame, PalmWatch},
    hub::{hub, DecoderView, Device, LinkState, Snapshot, Stats},
    panel_layout::{layers, Layout, PanelId},
    phone::{self, PhoneFrames},
    place, settings,
    usb::{self, Line, Path, UsbMac, CABLE_NAME},
};
use crate::tracking::FrameReader;
use cranpose::prelude::*;
use cranpose::text::{FontWeight, TextUnit};
use cranpose::DecodedFrames;
use cranpose_xr::{
    local_controller_models, local_haptics, local_headset_properties, local_pointer_settings, local_spatial_capabilities, local_spatial_environment, FollowMode, FollowTarget, TrackedDimensions,
    FollowingSubspace, Hand as XrHand, InputSource, Pose, SpatialFrameEffect, SpatialPanel, StreamedScene, SubspaceModifier,
};
use cranvr_common::controls::Action as ControlAction;
use cranvr_proto::clock::now_ns;
use std::{cell::RefCell, rc::Rc, time::Duration};

/// The sizes in dp that the connect and stats panels start at. The user can make them larger.
pub(crate) const CONNECT_SIZE: [f32; 2] = [760.0, 840.0];
const STATS_SIZE: [f32; 2] = [700.0, 840.0];
/// How often the UI copies the link's numbers into its state
const POLL: Duration = Duration::from_millis(250);
/// How often the UI looks for a new decoder. The decoder drops pictures that nobody takes, so the
/// streamed scene must start soon after the decoder does.
const STREAM_POLL: Duration = Duration::from_millis(16);
/// How often the UI looks at the console's state and the hand menu's requests. A hold of a button
/// should show them soon.
const CONSOLE_POLL: Duration = Duration::from_millis(50);
/// How often the UI tells the displays what it wishes
const WISH_POLL: Duration = Duration::from_millis(100);

pub(crate) const PANEL_COLOR: Color = Color(0.09, 0.1, 0.13, 0.96);
pub(crate) const FIELD_COLOR: Color = Color(0.16, 0.18, 0.23, 1.0);
pub(crate) const KEY_COLOR: Color = Color(0.2, 0.23, 0.3, 1.0);
pub(crate) const TEXT_COLOR: Color = Color(0.95, 0.96, 0.98, 1.0);
pub(crate) const MUTED_COLOR: Color = Color(0.62, 0.66, 0.73, 1.0);
pub(crate) const GOOD_COLOR: Color = Color(0.4, 0.82, 0.55, 1.0);
pub(crate) const WARN_COLOR: Color = Color(0.95, 0.72, 0.3, 1.0);
pub(crate) const BAD_COLOR: Color = Color(0.95, 0.42, 0.4, 1.0);
pub(crate) const ACCENT_COLOR: Color = Color(0.25, 0.45, 0.95, 1.0);
pub(crate) const STOP_COLOR: Color = Color(0.75, 0.3, 0.3, 1.0);

// The side panels stand on a circle of about a meter around the viewer, at 47 degrees to the left
// and to the right of the stream, and face the viewer.
const SIDE_OFFSET: [f32; 3] = [760.0, 0.0, 300.0];
const SIDE_TURN: f32 = 45.0;

/// The pose of a side panel before the user carries it, relative to the origin of the subspace, which
/// lies a meter ahead of the head: `side` is -1 for the left and 1 for the right.
pub(crate) fn side_pose(side: f32) -> Pose {
    SubspaceModifier::new().offset(side * SIDE_OFFSET[0], SIDE_OFFSET[1], SIDE_OFFSET[2]).rotate(0.0, -side * SIDE_TURN, 0.0).pose()
}

/// What a control shows while a ray or the mouse rests on it and while a press holds it: a white veil
/// over its face, which shows on the dark panels. Cranpose's clickables draw it, so every button, row
/// and ball of the app gets it.
const INDICATION: Indication = Indication::overlay(Color(1.0, 1.0, 1.0, 0.14), Color(1.0, 1.0, 1.0, 0.34));

/// The app, with the look of its controls under a pointer.
#[composable]
pub fn ClientApp() {
    CompositionLocalProvider([local_indication().provides(INDICATION)], Client);
}

/// The app. In a headset it shows the game's picture around the viewer while a game runs, and the
/// room otherwise. The connect panel stands to the left and the stats panel to the right, and the
/// displays of the Mac stand around the middle. The ball above the left controller opens the hand
/// menu, which shows and hides each panel (see `hand_menu.rs`). While a game runs no panel shows
/// until the user picks one.
#[composable]
fn Client() {
    // The connection reads what the headset is from the hub. The values do not change in a
    // session, so this runs before any panel can start a connection.
    hub().attach(local_headset_properties().current(), local_haptics().current());
    #[cfg(all(feature = "scripted-hand", target_os = "android"))]
    crate::scripted::install_once();
    // The saved Room and Volume settings apply before any panel or link needs them
    hub().load_settings();
    let Link { snapshot, frames } = link();
    let console = rememberMutableStateOf(|| hub().console());
    let backdrop = rememberMutableStateOf(|| hub().backdrop());
    // Which panels show: the room's choice, saved, or the game's, which ends with the game
    let layout = rememberMutableStateOf(Layout::load);
    let menu_open = rememberMutableStateOf(hand_menu::open_on_launch);
    // The address field of the connect panel is open. The Keyboard action of a button opens and
    // closes it too, so the state belongs to the app and not to the panel.
    let typing = rememberMutableStateOf(settings::address_entry_on_launch);
    // Counts the Recenter actions, from a button or from the hand menu. Each count makes the panels'
    // subspace snap in front of the head again.
    let recenters = rememberMutableStateOf(|| 0u32);
    // Where the hand menu stands: the frame loop follows the controllers (see `menu_place.rs`)
    let place = rememberMutableStateOf(hand_menu::current_place);
    let present = rememberMutableStateOf(|| [true; 2]);
    // The line under the ball shows at the first start and when the tracking falls to one
    // controller. It goes when the menu opens (see `menu_hint.rs`).
    let hint = rememberMutableStateOf(crate::menu_hint::wanted_at_start);
    // An open palm that faces the eyes carries a ball (see `palm.rs`)
    let palm = rememberMutableStateOf(|| None);
    let hands_seen = rememberMutableStateOf(|| false);
    let gaze = rememberMutableStateOf(|| false);
    let looking = rememberMutableStateOf(|| false);
    let hand = HandState { layout, menu_open, place, present, hint, palm, hands: hands_seen, gaze, looking, recenter: recenters };
    let opened = menu_open.get();
    LaunchedEffect(opened, async move |_| {
        if opened && hint.get_non_reactive() {
            log::info!("the menu hint goes: the menu opened");
            hint.set(false);
            crate::menu_hint::mark_seen();
        }
    });
    if hint.get() {
        LaunchedEffect((), async move |_| log::info!("the menu hint shows"));
    }
    // The hands belong to the game while the host asks for them (see `hands.rs`): then no panel
    // stands where a pinch could press it
    let hands_in_game = rememberMutableStateOf(|| false);
    // The address and the connect at launch belong to the app, not to the connect panel: a user who
    // hid the panels still connects at launch
    let address = address_field();
    // Cranpose's pointer: the buttons of the controllers and the beams. `None` outside a headset.
    let pointer_settings = local_pointer_settings().current();
    // The models of the controllers. A game draws its own controllers, so the Controls setting decides
    // what shows in a game (see `models.rs`).
    let controller_models = local_controller_models().current();
    LaunchedEffect((), async move |_| {
        for address in settings::host_keys_to_forget_on_launch() {
            log::info!("the launch forgets the host key of {address}");
            settings::forget_host_key(&crate::connection::host_name(&address));
        }
        if settings::console_on_launch() {
            hub().pin_console();
        }
        // What the pointer was last given, so a change of the mapping or of the panels is the only
        // thing that calls it
        let mut given = None;
        // The headset's own page for the Mac is looked at once a second (`headset_page.rs`). It is
        // made only when what it shows changed, or for a new link.
        let mut turns = 0u32;
        let mut page_read = None;
        loop {
            console.set(hub().console());
            let now = hub().backdrop();
            backdrop.set(now);
            hands_in_game.set(hands::belong_to_game(hub().feed.wish(), hub().feed.hand_points()));
            // A game starts with no panel shown, and the room's choice comes back when it ends
            let current = layout.get_non_reactive();
            if current.in_game() != now.game_runs {
                let mut next = current;
                next.set_in_game(now.game_runs);
                log::info!("{}: {} displays shown", if now.game_runs { "a game runs" } else { "no game runs" }, next.displays_shown().len());
                layout.set(next);
            }
            for fire in hub().take_fires() {
                run_fire(fire.action, layout, typing, recenters, menu_open);
            }
            if hub().take_menu_request() && !menu_open.get_non_reactive() {
                log::info!("the Mac opens the hand menu {}", hand_menu::current_place().label());
                menu_open.set(true);
            }
            // The Mac's presses on the headset's own page do what the hand menu's buttons do
            for request in hub().take_headset_requests() {
                match crate::headset_page::remote(&request) {
                    Some(Remote::Menu(action)) => hand_menu::perform_remote(action, hand),
                    Some(Remote::OpenMenu) => {
                        log::info!("the Mac opens the hand menu {}", hand_menu::current_place().label());
                        menu_open.set(true);
                    }
                    Some(Remote::Disconnect) => hub().disconnect(),
                    Some(Remote::Exit) => {
                        hub().disconnect();
                        cranpose::request_exit();
                    }
                    None => log::warn!("the Mac asked the headset for {}, which it does not know", request.to_json()),
                }
            }
            turns = turns.wrapping_add(1);
            if turns.is_multiple_of(20) {
                let read = (layout.get_non_reactive(), console.get_non_reactive().open, displays().menu_view(), place.get_non_reactive());
                if hub().headset_page_due(page_read.as_ref() != Some(&read)) {
                    let (shown, console_open, listed, at) = &read;
                    let input = MenuInput { layout: shown, console_open: *console_open, displays: listed, wishes: Default::default(), place: *at, adding: false, pages: &[], costs: false };
                    hub().send_headset_page(&crate::headset_page::page(&menu_model::rows(&input)));
                }
                page_read = Some(read);
            }
            if let Some(pointer) = &pointer_settings {
                let streaming = snapshot.get_non_reactive().state == LinkState::Streaming;
                let shown = layout.get_non_reactive();
                // A panel that the hand menu opens, a display in the room and the display on the hand are panels
                // too: the beams show while any of them does
                let any_display = shown.displays_shown().iter().any(|stream| displays().view(*stream).visible);
                let beams = beams_wanted(
                    !hands_in_game.get_non_reactive(),
                    shown.shown(PanelId::Connection) || shown.shown(PanelId::Stats),
                    streaming,
                    console.get_non_reactive().open || menu_open.get_non_reactive(),
                    any_display,
                );
                let want = (pointer_mapping(&hub().controls()), beams);
                if given != Some(want) {
                    log::info!("the pointer's beams {}, scrolling {}", if beams { "show" } else { "hide" }, if want.0.scroll_speed() > 0.0 { "on" } else { "off" });
                    pointer.set_mapping(want.0);
                    pointer.set_beams_visible(want.1);
                    given = Some(want);
                }
                // With the models off the headset draws no projection layer for them, so a game with
                // every panel hidden costs what it cost before the models
                if let Some(models) = &controller_models {
                    let hands = [XrHand::Left, XrHand::Right].map(|hand| models.hand(hand));
                    let before = hands.map(|hand| hand.is_enabled());
                    let scene = crate::models::Scene { game_runs: now.game_runs, beams, pointing: crate::hover::hands(), carrying: crate::hover::handle_held() };
                    let wanted = crate::models::shown(hub().controls().models_in_game, scene, before);
                    for (((name, hand), want), had) in ["left", "right"].into_iter().zip(hands).zip(wanted).zip(before) {
                        if want != had {
                            log::info!("the {name} controller's model {}", if want { "shows" } else { "hides" });
                            hand.set_enabled(want);
                        }
                    }
                }
            }
            delay(CONSOLE_POLL).await;
        }
    });
    // The client asks the host for the displays that show and for no other: a display that is not shown
    // costs the Mac and the headset nothing. The list of displays also shows a display that the user
    // adds at once. A display that the host does not offer keeps its place (see `panel_layout.rs`).
    LaunchedEffect((), async move |_| {
        loop {
            let mut current = layout.get_non_reactive();
            if current.follow_host(displays().listed_streams()) {
                layout.set(current.clone());
            }
            let focused = crate::focus::current().is_focused();
            let streaming = hub().snapshot().state == LinkState::Streaming;
            displays().set_wanted(if focused && streaming { displays().existing(crate::displays::wishes_of(&current)) } else { Default::default() });
            delay(WISH_POLL).await;
        }
    });
    // What the app composes, in the log: a game with nothing shown carries the ball only
    let shown_layers = {
        let layout = layout.get();
        let streaming = snapshot.get().state == LinkState::Streaming;
        let with_video: Vec<u8> = displays().menu_view().displays.iter().filter(|d| d.streaming).map(|d| d.info.stream).collect();
        layers(&layout, streaming, console.get().open, menu_open.get(), !hands_in_game.get(), &with_video)
    };
    LaunchedEffect(shown_layers, async move |_| {
        log::info!("layers composed by the app: {} panels and {} display surfaces", shown_layers.panels, shown_layers.surfaces);
    });
    // The origin of the panels' frame stands a meter ahead of the head when the first frame tracks it.
    // It moves there again after each recenter. A panel that the user never carried starts at its
    // default pose in that frame, so the first start opens in front of the user. A saved pose is in
    // the same frame (see `place.rs`).
    FollowingSubspace(FollowTarget::View(FollowMode::Snap(TrackedDimensions::TranslationAndYaw)), move || {
        HeadsetFrames(hand);
        // A flat window shows the panels in this order, side by side. In a headset each panel has
        // its own place. The hand menu shows and hides them. While no link runs the connect panel
        // shows anyway, so a user always sees how to connect and sees the code that the Mac asks
        // them to compare.
        let streaming = snapshot.get().state == LinkState::Streaming;
        let room_for_panels = !hands_in_game.get();
        let layout = layout.get();
        // The panels stand in a subspace of their own, which leaves the composition at a recenter
        // and snaps in front of the head again when it comes back. The game's picture and the
        // frame effect stay in the outer one, so a recenter does not touch the stream.
        key(recenters.get(), move || {
            FollowingSubspace(FollowTarget::View(FollowMode::Snap(TrackedDimensions::TranslationAndYaw)), move || {
                if room_for_panels && (layout.shown(PanelId::Connection) || !streaming) {
                    ConnectPanel(snapshot, address, typing);
                }
                if room_for_panels && layout.shown(PanelId::Stats) {
                    StatsPanel(snapshot);
                }
                // Each shown display stands in the room, unless it rides on the left hand. Each has its
                // own slot of the composition: hiding one must not hand its place, its pose and its
                // surface to another.
                for stream in layout.displays_shown().into_iter().filter(|stream| layout.pinned() != Some(*stream)) {
                    key(stream, move || RoomDisplay(stream));
                }
            });
        });
        ConsolePanel(console, snapshot);
        if hand_menu::flat_menu() {
            hand_menu::Menu(hand, console.get().open, hand_menu::ON_LEFT);
        }
        Behind(backdrop, frames);
    });
    // A menu that a button opened shows while the hands belong to the game too: the user asked for it
    if !hands_in_game.get() || menu_open.get() {
        let view = console.get();
        HandPanels(hand, view.open, view.controls, snapshot.get().state == LinkState::Streaming);
    }
}

/// Does a headset action that a button fired (see `controls.rs`). The console opens in the frame
/// loop, so it is not here. The frame loop also gave the hand that fired a click.
fn run_fire(action: ControlAction, layout: MutableState<Layout>, typing: MutableState<bool>, recenters: MutableState<u32>, menu_open: MutableState<bool>) {
    match action {
        ControlAction::Menu => {
            let open = !menu_open.get_non_reactive();
            log::info!("a button {} the hand menu", if open { "opens" } else { "closes" });
            menu_open.set(open);
        }
        ControlAction::Panels => {
            // The connection panel and the stream numbers hide together, or show together
            let mut next = layout.get_non_reactive();
            let shown = !(next.shown(PanelId::Connection) || next.shown(PanelId::Stats));
            next.set_shown(PanelId::Connection, shown);
            next.set_shown(PanelId::Stats, shown);
            if !next.in_game() {
                next.save();
            }
            layout.set(next);
        }
        ControlAction::Keyboard => {
            // The connection panel shows for the keyboard, for this run only
            let mut next = layout.get_non_reactive();
            next.set_shown(PanelId::Connection, true);
            layout.set(next);
            typing.set(!typing.get());
        }
        ControlAction::Recenter => recenters.set(recenters.get().wrapping_add(1)),
        _ => {}
    }
}

/// What fills the view behind the panels. While no game runs and the user kept the Room setting at
/// Passthrough, the room shows and the streamed scene stays out of the composition. The host still
/// sends its scene then and the decoder drops the pictures that nobody takes. While a game runs, or
/// with the Room setting at Scene, the streamed scene shows the host's picture and the app asks for
/// no passthrough. A headset that cannot show the room keeps the scene, because the app's own
/// background is black.
#[composable]
fn Behind(backdrop: MutableState<Backdrop>, frames: MutableState<Option<DecodedFrames>>) {
    let environment = local_spatial_environment().current();
    let available = environment.is_some() && local_spatial_capabilities().current().is_passthrough_control_enabled();
    let room = backdrop.get().shows_room(available);
    LaunchedEffect(room, async move |_| {
        if let Some(environment) = environment {
            log::info!("the room behind the panels: {}", if room { "passthrough" } else { "the host's picture" });
            environment.set_preferred_passthrough_opacity(Some(if room { 1.0 } else { 0.0 }));
        }
    });
    if !room {
        Stream(frames);
    }
}

/// The phone's app, with the look of its controls under a press.
#[composable]
pub fn PhoneApp() {
    CompositionLocalProvider([local_indication().provides(INDICATION)], Phone);
}

/// The app on a phone or a tablet. The picture fills the screen. The device's motion sensors move
/// the host's camera. A tap on the picture opens the connect and stats panels over it. The Hide
/// panels button closes them. They open again when the stream ends.
#[composable]
fn Phone() {
    hub().attach_phone();
    let Link { snapshot, frames } = link();
    let address = address_field();
    let panels = rememberMutableStateOf(|| true);
    let streaming = snapshot.get().state == LinkState::Streaming;
    LaunchedEffect(streaming, async move |_| {
        panels.set(!streaming);
        // Nobody touches the screen while the stream runs, and it must not go to sleep
        set_keep_screen_on(streaming);
    });
    Box(Modifier::empty().fill_max_size().background(Color(0.0, 0.0, 0.0, 1.0)), BoxSpec::default(), move || {
        PhoneFrames();
        // A tap on the picture shows or hides the panels: it is no button, so it shows no veil
        BoxWithConstraints(Modifier::empty().fill_max_size().clickable_with_indication(Indication::NONE, move |_| panels.set(!panels.get())), move |scope| {
            // The layout's size is in dp. The views of the Hello need the screen's pixels.
            let (width, height) = (scope.max_width().0, scope.max_height().0);
            let density = density().density();
            if width.is_finite() && height.is_finite() {
                hub().set_screen([(width * density).round() as u32, (height * density).round() as u32]);
            }
            PhonePicture(frames);
            if panels.get() {
                PhonePanels(snapshot, address, panels);
            }
        });
    });
}

/// The picture of the host: the left eye of each frame, fitted to the screen.
#[composable]
fn PhonePicture(frames: MutableState<Option<DecodedFrames>>) {
    Stream(frames);
}

/// The connect and stats panels side by side over a dimmed picture. Each panel scrolls, because a
/// phone's screen is lower than a panel.
#[composable]
fn PhonePanels(snapshot: MutableState<Snapshot>, address: TextFieldState, panels: MutableState<bool>) {
    let typing = rememberMutableStateOf(settings::address_entry_on_launch);
    let left = remember(|| ScrollState::new(0.0)).with(|state| *state);
    let right = remember(|| ScrollState::new(0.0)).with(|state| *state);
    Row(
        Modifier::empty().fill_max_size().background(Color(0.0, 0.0, 0.0, 0.55)).padding(16.0),
        RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(16.0)),
        move || {
            // A panel takes the taps that miss its buttons. They do not reach the picture's tap
            // handler under it. The panel is no button, so it shows no veil.
            Column(
                Modifier::empty().weight(1.0).fill_max_height().vertical_scroll(left, false).clickable_with_indication(Indication::NONE, |_| {}),
                ColumnSpec::default().vertical_arrangement(LinearArrangement::spaced_by(16.0)),
                move || {
                    ConnectBody(snapshot, address, Modifier::empty().fill_max_width(), false, typing);
                    PhoneButtons(panels);
                },
            );
            Column(
                Modifier::empty().weight(1.0).fill_max_height().vertical_scroll(right, false).clickable_with_indication(Indication::NONE, |_| {}),
                ColumnSpec::default(),
                move || StatsBody(snapshot, Modifier::empty().fill_max_width()),
            );
        },
    );
}

/// The buttons of the phone app. Recenter puts the front of the host's room where the screen points.
/// Hide panels closes the panels.
#[composable]
fn PhoneButtons(panels: MutableState<bool>) {
    Row(
        Modifier::empty(),
        RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(16.0)),
        move || {
            Button(
                Modifier::empty().background(KEY_COLOR).rounded_corners(12.0).padding_symmetric(26.0, 14.0),
                ButtonSpec::default(),
                phone::request_recenter,
                move || {
                    Text("Recenter", Modifier::empty(), text_style(24.0, TEXT_COLOR, Some(FontWeight::BOLD)));
                },
            );
            Button(
                Modifier::empty().background(ACCENT_COLOR).rounded_corners(12.0).padding_symmetric(26.0, 14.0),
                ButtonSpec::default(),
                move || panels.set(false),
                move || {
                    Text("Hide panels", Modifier::empty(), text_style(24.0, TEXT_COLOR, Some(FontWeight::BOLD)));
                },
            );
        },
    );
}

/// What the headset's app and the phone's app share. It holds the link's numbers for the UI and the
/// decoder's frames for the scene.
#[derive(Clone, Copy)]
struct Link {
    snapshot: MutableState<Snapshot>,
    frames: MutableState<Option<DecodedFrames>>,
}

/// Starts the effects that copy the hub's state into Cranpose state and keep the link up. The
/// caller composes it once.
fn link() -> Link {
    // The panel lists the Macs that answer. The search runs while no link does.
    crate::discovery::start(hub());
    // and the Mac behind the USB cable, which a headset on the cable shows first
    crate::usb::start(hub());
    let snapshot = rememberMutableStateOf(|| hub().snapshot());
    LaunchedEffect((), async move |_| loop {
        snapshot.set(hub().snapshot());
        delay(POLL).await;
    });
    // A link that streamed and ended without the user's say connects again, as after a restart of
    // the host that a setting of the console asks for
    LaunchedEffect((), async move |_| loop {
        hub().reconnect_if_due();
        delay(POLL).await;
    });
    let frames = rememberMutableStateOf(|| None::<DecodedFrames>);
    LaunchedEffect((), async move |_| {
        let mut seen = None;
        loop {
            let (generation, current) = hub().stream();
            if seen != Some(generation) {
                seen = Some(generation);
                frames.set(current);
            }
            delay(STREAM_POLL).await;
        }
    });
    Link { snapshot, frames }
}

pub(crate) fn text_style(size: f32, color: Color, weight: Option<FontWeight>) -> TextStyle {
    TextStyle {
        span_style: SpanStyle {
            color: Some(color),
            font_size: TextUnit::Sp(size),
            font_weight: weight,
            ..SpanStyle::default()
        },
        ..TextStyle::default()
    }
}

/// Hands each frame of the headset to the sender of the tracking. The callback runs on the frame
/// loop once for each headset frame. It does no network call: the sender thread wakes and sends.
#[composable]
fn HeadsetFrames(hand: HandState) {
    let HandState { menu_open, place, present, hint, palm, hands, gaze, looking, .. } = hand;
    let reader = remember(|| Rc::new(RefCell::new(FrameReader::default()))).with(Clone::clone);
    let hold = remember(|| Rc::new(RefCell::new(PanelHold::default()))).with(Clone::clone);
    let placer = remember(|| Rc::new(RefCell::new(Placer::default()))).with(Clone::clone);
    let palms = remember(|| Rc::new(RefCell::new(PalmWatch::default()))).with(Clone::clone);
    let dwell = remember(|| Rc::new(RefCell::new(crate::gaze::Dwell::default()))).with(Clone::clone);
    let pitch = crate::gaze::pitch();
    SpatialFrameEffect(move |frame| {
        let now = now_ns();
        let mut read = reader.borrow_mut().read(frame, now);
        let open = menu_open.get_non_reactive();
        // A system menu over the app takes the controllers' poses away: that loses no controller
        if frame.focused {
            let mut placer = placer.borrow_mut();
            let next = placer.update(hub().controls().menu_hand, &place_frame(frame, open));
            if next != place.get_non_reactive() {
                let [left, right] = placer.present().map(|present| if present { "tracked" } else { "lost" });
                log::info!("the hand menu moves {}: the left controller is {left}, the right one {right}", next.label());
                hand_menu::set_place(next);
                place.set(next);
            }
            // The palms of the hands that the cameras track, while no controller is in them
            let palm_frames = [XrHand::Left, XrHand::Right].map(|side| palm_frame(frame.hand(side)));
            // A hand counts while Cranpose's hand pointer points with it or its palm is located. A
            // Quest reports a hand as tracking with no located joint while no hand is in view.
            let pointing = [XrHand::Left, XrHand::Right].map(|side| frame.pointer(side) == Some(InputSource::Hands));
            let tracked_hands = [0, 1].map(|i| pointing[i] || palm_frames[i].palm.is_some());
            let tracked = tracked_hands.contains(&true);
            if tracked != hands.get_non_reactive() {
                hands.set(tracked);
            }
            if let Some(eyes) = frame.device_pose.map(|pose| pose.position) {
                let change = palms.borrow_mut().update(frame.predicted_display_time, eyes, palm_frames);
                if change.shown != palm.get_non_reactive() {
                    log::info!("the hand menu's ball {}", match change.shown {
                        Some(cranvr_common::controls::Hand::Left) => "shows on the left palm",
                        Some(cranvr_common::controls::Hand::Right) => "shows on the right palm",
                        None => "leaves the palm",
                    });
                    palm.set(change.shown);
                }
                if change.menu {
                    log::info!("the left palm's pinch {} the hand menu", if open { "closes" } else { "opens" });
                    menu_open.set(!open);
                }
            }
            // No controller and no hand: a look that rests on the ball below opens the menu
            let no_input = crate::gaze::no_input(placer.present(), tracked_hands);
            if no_input != gaze.get_non_reactive() {
                log::info!("{}", if no_input { "no input is tracked: a look at the ball below opens the hand menu" } else { "an input is tracked again: the ball below goes" });
                gaze.set(no_input);
            }
            let on_ball = no_input && frame.device_pose.is_some_and(|head| crate::gaze::looks_at(&head, pitch));
            let mut dwell = dwell.borrow_mut();
            if dwell.update(frame.predicted_display_time, on_ball) {
                log::info!("a look of {} s on the ball {} the hand menu", crate::gaze::DWELL.as_secs(), if open { "closes" } else { "opens" });
                menu_open.set(!open);
            }
            if dwell.resting() != looking.get_non_reactive() {
                looking.set(dwell.resting());
            }
            let now_present = placer.present();
            let before = present.get_non_reactive();
            if now_present != before {
                // The other hand's buttons are gone: the line names what works now
                if before == [true; 2] && now_present.iter().filter(|p| **p).count() == 1 && !open {
                    log::info!("the menu hint shows: the tracking fell to one controller");
                    hint.set(true);
                }
                present.set(now_present);
            }
        }
        // The hands that a panel holds back from the game: the host gives the game rest values for them
        if let Some(sent) = read.frame.as_mut() {
            let workers = if open { menu_place::workers(place.get_non_reactive(), placer.borrow().present()) } else { [false; 2] };
            sent.held = held_hands(&mut hold.borrow_mut(), frame, now, workers);
        }
        // The hands, body, face and eyes go in before the frame, so the sender finds them when it wakes
        reader.borrow_mut().push_tracked(frame, &read, &hub().feed);
        console_panel::on_frame(&read, now);
        hub().feed.push(read);
    });
}

/// What the frame says about one hand that the cameras track, for its palm's ball (see `palm.rs`).
fn palm_frame(state: &cranpose_xr::HandState) -> PalmFrame {
    let joint = state.joint(cranpose_xr::HandJointType::Palm);
    let tracked = state.tracking_state == cranpose_xr::TrackingState::Tracking && joint.valid;
    PalmFrame {
        palm: tracked.then_some(joint.pose),
        pinch: state.aim.map_or(0.0, |aim| aim.index.strength),
        menu_pressed: state.aim.is_some_and(|aim| aim.menu_pressed),
    }
}

/// What the frame says about the controllers, for the hand menu's place (see `menu_place.rs`). A hand
/// that the cameras track is no controller.
fn place_frame(frame: &cranpose_xr::XrFrame, menu_open: bool) -> PlaceFrame {
    let controller = |hand: XrHand| crate::tracking::held(frame.controller(hand)).filter(|controller| controller.aim.is_some());
    let hands = [XrHand::Left, XrHand::Right];
    PlaceFrame {
        time: frame.predicted_display_time,
        tracked: hands.map(|hand| controller(hand).is_some()),
        trigger: hands.map(|hand| controller(hand).map_or(0.0, |controller| controller.trigger.value)),
        on_panel: hover::hands_off_menu(),
        menu_busy: menu_open || hover::on_menu(),
    }
}

/// The hands that the app holds back from the game in this frame (see `hold.rs`). An app without the
/// input focus has no pointer, so nothing is held then. `menu_workers` are the hands that work the
/// open hand menu.
fn held_hands(hold: &mut PanelHold, frame: &cranpose_xr::XrFrame, now: u64, menu_workers: [bool; 2]) -> [bool; 2] {
    if !frame.focused {
        hold.reset();
        hover::clear();
        return [false; 2];
    }
    let hover = hover::hands();
    if hover[0] || hover[1] {
        hover::mark(now / 1_000_000);
    }
    let trigger = |hand: XrHand| match frame.pointer(hand) {
        // A tracked hand presses with the pinch of its index finger
        Some(InputSource::Hands) => frame.hand(hand).aim.map_or(0.0, |aim| aim.index.strength),
        _ => frame.controller(hand).trigger.value,
    };
    let held = hold.update(&HoldFrame {
        time: frame.predicted_display_time,
        hover,
        handle: hover::handle_held(),
        trigger: [trigger(XrHand::Left), trigger(XrHand::Right)],
        menu_workers,
    });
    // Each change goes to the log, so a run shows when a panel held a hand back
    static LAST: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    let bits = u8::from(held[0]) | u8::from(held[1]) << 1;
    if LAST.swap(bits, std::sync::atomic::Ordering::Relaxed) != bits {
        log::info!("a panel holds back {} from the game", match held {
            [false, false] => "no hand",
            [true, false] => "the left hand",
            [false, true] => "the right hand",
            [true, true] => "both hands",
        });
    }
    held
}

/// The game's picture. The decoder outputs frames. The headset's compositor shows each one from
/// the pose that the host rendered it for. It corrects the picture for the head's newer pose.
#[composable]
fn Stream(frames: MutableState<Option<DecodedFrames>>) {
    if let Some(frames) = frames.get() {
        let source = frames.clone();
        StreamedScene(frames, move |metadata| hub().views_of(&source, metadata), hub().stream_stats.clone());
    }
}

/// What the user presses on the keypad.
#[derive(Clone, Copy, PartialEq)]
enum KeyPress {
    Text(&'static str),
    Back,
    Clear,
}

/// Starts or ends the connection, as the button of the panel says. A start connects to the typed
/// address.
fn toggle_connection(state: &LinkState, address: TextFieldState) {
    match state {
        LinkState::Idle | LinkState::Failed(_) => {
            let host = address.text();
            settings::remember_host(&host);
            hub().connect(host, None, settings::media_choice());
        }
        LinkState::Connecting | LinkState::Waiting { .. } | LinkState::Streaming => hub().disconnect(),
    }
}

/// Connects to the Mac behind the USB cable. It leaves the address field alone: the field belongs
/// to the Wi-Fi entry and to the typed address.
fn connect_cable(mac: &UsbMac) {
    hub().connect(mac.address.to_string(), mac.fingerprint(), settings::media_choice());
}

/// Connects to a Mac that the search found. The address field takes the Mac's address, so the next
/// launch starts with it and Forget host key knows it.
fn connect_found(host: &FoundHost, address: TextFieldState) {
    let text = host.address_text();
    address.edit(|buffer| {
        buffer.clear();
        buffer.insert(&text);
    });
    settings::remember_host(&text);
    hub().connect(text, Some(host.fingerprint), settings::media_choice());
}

/// The field's state for the host address. The caller composes it once. A launch with `connect`
/// starts the connection.
fn address_field() -> TextFieldState {
    let address = remember(|| TextFieldState::new(settings::initial_host())).with(|state| *state);
    LaunchedEffect((), async move |_| {
        if settings::connect_on_launch() {
            toggle_connection(&LinkState::Idle, address);
        }
    });
    address
}

/// The connect panel: the Macs that the search found, and the address field with its keypad behind a
/// button. The app owns the address field (`address_field`).
#[composable]
fn ConnectPanel(snapshot: MutableState<Snapshot>, address: TextFieldState, typing: MutableState<bool>) {
    let pose = remember(|| place::start_pose("connect", side_pose(-1.0))).with(|pose| *pose);
    let size = rememberMutableStateOf(|| place::start_size("connect", CONNECT_SIZE));
    let modifier = place::resizable("connect", size, CONNECT_SIZE).layer_priority(crate::panel_layout::layer_rank::TEXT);
    SpatialPanel(place::movable("connect", modifier, pose, |_| {}), move || {
        // The pointer over the panel holds the pointing hand back from the game (see `hold.rs`)
        let hovered = hover::hover_modifier(hover::CONNECTION);
        ConnectBody(snapshot, address, Modifier::empty().fill_max_size().then(hovered), true, typing);
    });
}

/// The content of the connect panel. `modifier` gives its size. While no link runs, it lists the
/// Macs that answered the search. A press on one connects. The address field stays
/// behind the small button "Enter an address", for a headset on USB and for another network. It
/// replaces the list while it shows. `keypad` adds the keypad of a headset.
#[composable]
fn ConnectBody(snapshot: MutableState<Snapshot>, address: TextFieldState, modifier: Modifier, keypad: bool, typing: MutableState<bool>) {
    let snap = snapshot.get();
    let state = snap.state.clone();
    let found = usb::entries(&snap.found, snap.usb.as_ref());
    let path = snap.path;
    Column(
        modifier.background(PANEL_COLOR).rounded_corners(24.0).padding(36.0),
        ColumnSpec::default().vertical_arrangement(LinearArrangement::spaced_by(20.0)),
        move || {
            Text("cranvr", Modifier::empty(), text_style(44.0, TEXT_COLOR, Some(FontWeight::BOLD)));
            match state.clone() {
                LinkState::Idle | LinkState::Failed(_) => {
                    // The address entry takes the place of the list: both together would not fit the panel
                    if typing.get() {
                        AddressEntry(address, state.clone(), keypad);
                    } else {
                        HostList(found.clone(), address);
                    }
                    StatusLine(state.clone(), address, path);
                    AddressButtons(typing, address);
                }
                LinkState::Waiting { code, host } => {
                    WaitingBody(code, host);
                    StopButton(state.clone());
                }
                LinkState::Connecting | LinkState::Streaming => {
                    StatusLine(state.clone(), address, path);
                    StopButton(state.clone());
                }
            }
        },
    );
}

/// The Macs that answered, the cable's first, each as a button, or the words that say what to do
/// when none answered.
#[composable]
fn HostList(found: Vec<Line>, address: TextFieldState) {
    if found.is_empty() {
        Text("Looking for a Mac that runs cranvr...", Modifier::empty(), text_style(28.0, TEXT_COLOR, None));
        Text(
            "Open cranvr on the Mac. Put the Mac and this headset on the same Wi-Fi.",
            Modifier::empty(),
            text_style(22.0, MUTED_COLOR, None),
        );
        return;
    }
    Text("Press your Mac to connect.", Modifier::empty(), text_style(26.0, MUTED_COLOR, None));
    for entry in found {
        match entry {
            Line::Cable(mac) => CableButton(mac),
            Line::WiFi(host) => FoundHostButton(host, address),
        }
    }
}

/// The Mac behind the USB cable. `adb reverse` needs no code, because the Mac trusts a headset that
/// reaches it from its own address. On the USB network the Mac asks once for a headset that it does
/// not know, as over Wi-Fi.
#[composable]
fn CableButton(mac: UsbMac) {
    let host = mac.as_found();
    let detail = if mac.needs_code() { "The Mac shows a code the first time" } else { "No code needed" };
    if let Some(advice) = host.update_advice() {
        Column(
            Modifier::empty().fill_max_width().background(KEY_COLOR).rounded_corners(16.0).padding_symmetric(28.0, 18.0),
            ColumnSpec::default(),
            move || {
                Text(CABLE_NAME, Modifier::empty(), text_style(30.0, MUTED_COLOR, Some(FontWeight::BOLD)));
                Text(advice, Modifier::empty(), text_style(22.0, WARN_COLOR, None));
            },
        );
        return;
    }
    Button(
        Modifier::empty().fill_max_width().background(ACCENT_COLOR).rounded_corners(16.0).padding_symmetric(28.0, 18.0),
        ButtonSpec::default(),
        move || connect_cable(&mac),
        move || {
            Column(Modifier::empty(), ColumnSpec::default(), move || {
                Text(CABLE_NAME, Modifier::empty(), text_style(32.0, TEXT_COLOR, Some(FontWeight::BOLD)));
                Text(detail, Modifier::empty(), text_style(20.0, TEXT_COLOR, None));
            });
        },
    );
}

#[composable]
fn FoundHostButton(host: FoundHost, address: TextFieldState) {
    let detail = format!("{}{}", host.address.ip(), if host.paired { ", paired" } else { "" });
    let name = host.name.clone();
    if let Some(advice) = host.update_advice() {
        // The two apps speak no common protocol: a press would only fail
        Column(
            Modifier::empty().fill_max_width().background(KEY_COLOR).rounded_corners(16.0).padding_symmetric(28.0, 18.0),
            ColumnSpec::default(),
            move || {
                Text(name.clone(), Modifier::empty(), text_style(30.0, MUTED_COLOR, Some(FontWeight::BOLD)));
                Text(advice, Modifier::empty(), text_style(22.0, WARN_COLOR, None));
            },
        );
        return;
    }
    let pressed = host.clone();
    Button(
        Modifier::empty().fill_max_width().background(ACCENT_COLOR).rounded_corners(16.0).padding_symmetric(28.0, 18.0),
        ButtonSpec::default(),
        move || connect_found(&pressed, address),
        move || {
            Column(Modifier::empty(), ColumnSpec::default(), {
                let (name, detail) = (name.clone(), detail.clone());
                move || {
                    Text(name.clone(), Modifier::empty(), text_style(32.0, TEXT_COLOR, Some(FontWeight::BOLD)));
                    Text(detail.clone(), Modifier::empty(), text_style(20.0, TEXT_COLOR, None));
                }
            });
        },
    );
}

/// What the headset shows while the Mac's user decides: the code, which the Mac shows too.
#[composable]
fn WaitingBody(code: String, host: String) {
    Text("Allow it on the Mac", Modifier::empty(), text_style(38.0, TEXT_COLOR, Some(FontWeight::BOLD)));
    Text(code, Modifier::empty(), text_style(96.0, GOOD_COLOR, Some(FontWeight::BOLD)));
    Text(
        format!("{host} shows the same code. Press Allow on the Mac only if the two codes match."),
        Modifier::empty(),
        text_style(24.0, MUTED_COLOR, None),
    );
}

/// The small buttons under the list.
#[composable]
fn AddressButtons(typing: MutableState<bool>, address: TextFieldState) {
    let shown = typing.get();
    Row(
        Modifier::empty(),
        RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(12.0)),
        move || {
            Button(
                Modifier::empty().background(KEY_COLOR).rounded_corners(12.0).padding_symmetric(24.0, 12.0),
                ButtonSpec::default(),
                move || typing.set(!shown),
                move || {
                    Text(if shown { "Show the Macs" } else { "Enter an address" }, Modifier::empty(), text_style(22.0, MUTED_COLOR, None));
                },
            );
            ForgetHostButton(address);
        },
    );
}

/// The address of a Mac that the search does not find: a headset on USB (`127.0.0.1`) or on another
/// network. A press on the field opens the keyboard. In a Quest it is the keyboard that Cranpose
/// draws under the panel, and a press on its keys types. On a phone it is the system's keyboard.
/// A headset also gets a keypad in the panel, which edits the same text. It stays until the drawn
/// keyboard has run on a Quest 3, because Cranpose tested it in a window and an emulator only.
#[composable]
fn AddressEntry(address: TextFieldState, state: LinkState, keypad: bool) {
    Text("Address of the Mac", Modifier::empty(), text_style(22.0, MUTED_COLOR, None));
    Box(
        Modifier::empty().fill_max_width().background(FIELD_COLOR).rounded_corners(14.0).padding(18.0),
        BoxSpec::default(),
        move || {
            BasicTextFieldWithOptions(
                address,
                Modifier::empty().fill_max_width(),
                BasicTextFieldOptions {
                    text_style: text_style(34.0, TEXT_COLOR, None),
                    cursor_color: TEXT_COLOR,
                    line_limits: TextFieldLineLimits::SingleLine,
                    show_keyboard_on_focus: true,
                    ..BasicTextFieldOptions::default()
                },
            );
        },
    );
    if keypad {
        Keypad(address);
    }
    ConnectButton(state, address);
}

#[composable]
fn Keypad(address: TextFieldState) {
    let rows: [&[KeyPress]; 3] = [
        &[KeyPress::Text("1"), KeyPress::Text("2"), KeyPress::Text("3"), KeyPress::Text("4"), KeyPress::Text("5")],
        &[KeyPress::Text("6"), KeyPress::Text("7"), KeyPress::Text("8"), KeyPress::Text("9"), KeyPress::Text("0")],
        &[KeyPress::Text("."), KeyPress::Text(":"), KeyPress::Back, KeyPress::Clear],
    ];
    Column(
        Modifier::empty(),
        ColumnSpec::default().vertical_arrangement(LinearArrangement::spaced_by(10.0)),
        move || {
            for row in rows {
                Row(
                    Modifier::empty(),
                    RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(10.0)),
                    move || {
                        for press in row {
                            Key(*press, address);
                        }
                    },
                );
            }
        },
    );
}

#[composable]
fn Key(press: KeyPress, address: TextFieldState) {
    let label = match press {
        KeyPress::Text(text) => text,
        KeyPress::Back => "<-",
        KeyPress::Clear => "clear",
    };
    let wide = !matches!(press, KeyPress::Text(_));
    Button(
        Modifier::empty()
            .background(KEY_COLOR)
            .rounded_corners(12.0)
            .padding_symmetric(if wide { 26.0 } else { 30.0 }, 14.0),
        ButtonSpec::default(),
        move || {
            address.edit(|buffer| match press {
                KeyPress::Text(text) => buffer.insert(text),
                KeyPress::Back => buffer.delete_before_cursor(),
                KeyPress::Clear => buffer.clear(),
            });
        },
        move || {
            Text(label, Modifier::empty(), text_style(30.0, TEXT_COLOR, Some(FontWeight::BOLD)));
        },
    );
}

/// Connects to the typed address.
#[composable]
fn ConnectButton(state: LinkState, address: TextFieldState) {
    Button(
        Modifier::empty().background(ACCENT_COLOR).rounded_corners(16.0).padding_symmetric(44.0, 20.0),
        ButtonSpec::default(),
        move || toggle_connection(&state, address),
        move || {
            Text("Connect", Modifier::empty(), text_style(32.0, TEXT_COLOR, Some(FontWeight::BOLD)));
        },
    );
}

/// Ends the connection that runs or that waits for the Mac.
#[composable]
fn StopButton(state: LinkState) {
    let label = if state == LinkState::Streaming { "Disconnect" } else { "Cancel" };
    Button(
        Modifier::empty().background(STOP_COLOR).rounded_corners(16.0).padding_symmetric(44.0, 20.0),
        ButtonSpec::default(),
        move || hub().disconnect(),
        move || {
            Text(label, Modifier::empty(), text_style(32.0, TEXT_COLOR, Some(FontWeight::BOLD)));
        },
    );
}

/// Makes this headset forget the key of the host at the typed address. The headset then trusts the
/// key that the host shows at the next connection. The user needs it after a new install of the
/// host, which makes a new key.
#[composable]
fn ForgetHostButton(address: TextFieldState) {
    Button(
        Modifier::empty().background(KEY_COLOR).rounded_corners(12.0).padding_symmetric(26.0, 12.0),
        ButtonSpec::default(),
        move || settings::forget_host_key(&crate::connection::host_name(&address.text())),
        move || {
            Text("Forget host key", Modifier::empty(), text_style(22.0, MUTED_COLOR, None));
        },
    );
}

/// The state of the link in words: connecting, streaming or the error.
#[composable]
fn StatusLine(state: LinkState, address: TextFieldState, path: Option<Path>) {
    let (text, color) = state_text(&state, &address.text(), path);
    Text(text, Modifier::empty(), text_style(24.0, color, None));
}

/// The path shows in the words of the Mac's Headset card.
fn state_text(state: &LinkState, address: &str, path: Option<Path>) -> (String, Color) {
    match state {
        LinkState::Idle => ("Not connected".into(), MUTED_COLOR),
        LinkState::Connecting if path == Some(Path::Usb) => ("Connecting by USB cable...".into(), WARN_COLOR),
        LinkState::Connecting => (format!("Connecting to {}...", address.trim()), WARN_COLOR),
        LinkState::Waiting { host, .. } => (format!("Waiting for {host} to allow this headset"), WARN_COLOR),
        LinkState::Streaming => (
            match path {
                Some(Path::Usb) => "Connected by USB cable",
                Some(Path::WiFi) => "Connected over Wi-Fi",
                None => "Connected",
            }
            .into(),
            GOOD_COLOR,
        ),
        LinkState::Failed(message) => (message.clone(), BAD_COLOR),
    }
}

/// The numbers of the link: rates, decoder times, loss and latency.
#[composable]
fn StatsPanel(snapshot: MutableState<Snapshot>) {
    let pose = remember(|| place::start_pose("stats", side_pose(1.0))).with(|pose| *pose);
    let size = rememberMutableStateOf(|| place::start_size("stats", STATS_SIZE));
    SpatialPanel(place::movable("stats", place::resizable("stats", size, STATS_SIZE), pose, |_| {}), move || {
        let hovered = hover::hover_modifier(hover::STATS);
        StatsBody(snapshot, Modifier::empty().fill_max_size().then(hovered));
    });
}

/// The content of the stats panel. `modifier` gives its size.
#[composable]
fn StatsBody(snapshot: MutableState<Snapshot>, modifier: Modifier) {
    let snap = snapshot.get();
    let lines = stat_lines(&snap);
    Column(
        modifier.background(PANEL_COLOR).rounded_corners(24.0).padding(36.0),
        ColumnSpec::default().vertical_arrangement(LinearArrangement::spaced_by(14.0)),
        move || {
            Text("Stream", Modifier::empty(), text_style(44.0, TEXT_COLOR, Some(FontWeight::BOLD)));
            for (label, value, color) in lines.clone() {
                StatLine(label, value, color);
            }
        },
    );
}

#[composable]
fn StatLine(label: &'static str, value: String, color: Color) {
    Row(
        Modifier::empty().fill_max_width(),
        RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(16.0)),
        move || {
            Text(label, Modifier::empty().width(170.0), text_style(22.0, MUTED_COLOR, None));
            Text(value.clone(), Modifier::empty().weight(1.0), text_style(22.0, color, None));
        },
    );
}

/// The lines of the stats panel: a label, a value and the value's color.
fn stat_lines(snap: &Snapshot) -> Vec<(&'static str, String, Color)> {
    let stats = &snap.stats;
    let (link, link_color) = state_text(&snap.state, "the host", snap.path);
    let mut lines = vec![("Link", link, link_color)];
    // While the app has no input focus the host sends no pictures. The panel says why the numbers stop.
    if !snap.focus.is_focused() {
        lines.push(("Focus", format!("{}. The stream pauses and goes on when the focus returns.", crate::focus::describe(snap.focus)), WARN_COLOR));
    }
    let Some(stream) = &snap.stream else {
        return lines;
    };
    lines.push((
        "Stream",
        format!(
            "{}x{} per eye{}, {} at {} Hz, {} media, {:.0} Mbps asked",
            stream.eye[0],
            stream.eye[1],
            if stream.foveated { format!(", foveated into frames of {}x{}", stream.picture[0], stream.picture[1]) } else { String::new() },
            stream.codec,
            stream.refresh_hz,
            if stream.udp { "UDP" } else { "TCP" },
            stream.target_mbps
        ),
        TEXT_COLOR,
    ));
    lines.push(("Frames", frames_text(stats, stream.refresh_hz), fps_color(stats.decoded_fps, stream.refresh_hz)));
    let (decoder, decoder_color) = decoder_text(stats);
    lines.push(("Decoder", decoder, decoder_color));
    lines.push(("Loss", loss_text(stats), if stats.units_lost > 0 { BAD_COLOR } else { TEXT_COLOR }));
    lines.push((
        "FEC",
        format!("{} units recovered, {} packets restored", stats.units_recovered, stats.packets_recovered),
        TEXT_COLOR,
    ));
    lines.push(("Latency", latency_text(stats), TEXT_COLOR));
    lines.push(("Key frames", format!("{} requested", stats.key_requests), TEXT_COLOR));
    lines.push(("Sealing", crypto_text(stats), if stats.packets_rejected > 0 { WARN_COLOR } else { TEXT_COLOR }));
    lines.push(tracking_line(stats));
    if hub().device() == Device::Phone {
        lines.push((
            "Host's pose",
            stats.rendered_orientation.map_or("no frame yet".to_string(), |q| format!("the newest frame was rendered for looking {:.2?}", phone::forward(q))),
            TEXT_COLOR,
        ));
    }
    lines.push(("Haptics", format!("{} pulses received", stats.haptic_pulses), TEXT_COLOR));
    for (label, value) in displays().stat_lines().into_iter().chain(crate::audio::stat_lines()) {
        lines.push((label, value, TEXT_COLOR));
    }
    lines
}

/// How the head's poses arise: from the headset, from the phone's motion sensors or as placeholders.
fn tracking_line(stats: &Stats) -> (&'static str, String, Color) {
    let (text, color) = match (hub().device(), stats.tracking_live) {
        (Device::Phone, true) => (
            format!(
                "from the phone's motion sensors, looking {}",
                stats.head_orientation.map_or("nowhere".to_string(), |q| format!("{:.2?}", phone::forward(q)))
            ),
            GOOD_COLOR,
        ),
        (Device::Phone, false) => ("no frames from the screen".to_string(), WARN_COLOR),
        (_, true) => ("from the headset".to_string(), GOOD_COLOR),
        (_, false) => ("placeholder values: no frames from a headset".to_string(), WARN_COLOR),
    };
    ("Tracking", text, color)
}

fn crypto_text(stats: &Stats) -> String {
    format!(
        "opening took {:.0} us a frame ({:.2} us a packet, {:.1}% of a core), {} packets refused",
        stats.open_us_per_frame, stats.open_us_per_packet, stats.open_core_pct, stats.packets_rejected
    )
}

fn fps_color(fps: f32, target: f32) -> Color {
    if fps >= target * 0.9 {
        GOOD_COLOR
    } else if fps >= target * 0.6 {
        WARN_COLOR
    } else {
        BAD_COLOR
    }
}

fn frames_text(stats: &Stats, target_hz: f32) -> String {
    let received = format!("{:.1} fps decoded of {:.0}, {:.1} received, {:.1} Mbps", stats.decoded_fps, target_hz, stats.received_fps, stats.received_mbps);
    if hub().device() == Device::Phone {
        // The picture of a phone draws the newest frame. Cranpose counts nothing for it.
        return format!("{received}. The screen runs at {:.1} frames a second", stats.headset_fps);
    }
    format!(
        "{received}. The scene showed {:.1} a second, skipped {:.1} and repeated {:.1} of {:.1} headset frames",
        stats.shown_fps, stats.skipped_fps, stats.repeated_fps, stats.headset_fps
    )
}

fn decoder_text(stats: &Stats) -> (String, Color) {
    match &stats.decoder {
        DecoderView::Idle => ("waiting for the stream".into(), MUTED_COLOR),
        DecoderView::NoHeadset => ("none: this window is not a headset, so nothing decodes".into(), WARN_COLOR),
        DecoderView::Failed(message) => (message.clone(), BAD_COLOR),
        DecoderView::Running => (
            format!(
                "{:.2} ms mean, {:.2} ms last, {:.2} ms longest, {} dropped",
                stats.decode_mean_ms, stats.decode_last_ms, stats.decode_max_ms, stats.decoder_dropped
            ),
            TEXT_COLOR,
        ),
    }
}

fn loss_text(stats: &Stats) -> String {
    let lost = stats.packets_expected.saturating_sub(stats.packets_received);
    let percent = 100.0 * lost as f64 / stats.packets_expected.max(1) as f64;
    format!("{percent:.2}% of {} packets, {} frames lost", stats.packets_expected, stats.units_lost)
}

fn latency_text(stats: &Stats) -> String {
    let ms = |value: Option<f32>| value.map_or("-".to_string(), |v| format!("{v:.1}"));
    format!(
        "round trip {} ms, network {} ms, pose to decode {} ms, pose to display {} ms",
        ms(stats.rtt_ms),
        ms(stats.network_ms),
        ms(stats.sample_to_decode_ms),
        ms(stats.sample_to_display_ms)
    )
}
