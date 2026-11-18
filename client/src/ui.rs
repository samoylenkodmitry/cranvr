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
    console_panel::{self, ConsolePanel},
    hub::{hub, DecoderView, Device, LinkState, Snapshot, Stats},
    phone::{self, PhoneFrames},
    place, settings,
};
use crate::tracking::FrameReader;
use cranpose::prelude::*;
use cranpose::text::{FontWeight, TextUnit};
use cranpose::DecodedFrames;
use cranpose_xr::{
    local_haptics, local_headset_properties, local_spatial_capabilities, FollowMode, FollowTarget, FollowingSubspace, Pose,
    SpatialFrameEffect, SpatialPanel, StreamedScene, SubspaceModifier,
};
use cranvr_proto::{clock::now_ns, identity::format_code};
use std::{cell::RefCell, rc::Rc, time::Duration};

/// How often the UI copies the link's numbers into its state
const POLL: Duration = Duration::from_millis(250);
/// How often the UI looks for a new decoder. The decoder drops pictures that nobody takes, so the
/// streamed scene must start soon after the decoder does.
const STREAM_POLL: Duration = Duration::from_millis(16);
/// How often the UI looks at the console's state. A hold of the menu button should show it soon.
const CONSOLE_POLL: Duration = Duration::from_millis(50);

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
fn side_pose(side: f32) -> Pose {
    SubspaceModifier::new().offset(side * SIDE_OFFSET[0], SIDE_OFFSET[1], SIDE_OFFSET[2]).rotate(0.0, -side * SIDE_TURN, 0.0).pose()
}

/// The app. In a headset it shows the game's picture around the viewer, the connect panel to the
/// left and the stats panel to the right.
#[composable]
pub fn ClientApp() {
    // The connection reads what the headset is from the hub. The values do not change in a
    // session, so this runs before any panel can start a connection.
    hub().attach(local_headset_properties().current(), local_haptics().current());
    let Link { snapshot, frames } = link();
    let console = rememberMutableStateOf(|| hub().console());
    LaunchedEffect((), async move |_| {
        if settings::console_on_launch() {
            hub().pin_console();
        }
        loop {
            console.set(hub().console());
            delay(CONSOLE_POLL).await;
        }
    });
    // The origin of the panels' frame stands a meter ahead of the head when the first frame tracks it.
    // It moves there again after each recenter. A panel that the user never carried starts at its
    // default pose in that frame, so the first start opens in front of the user. A saved pose is in
    // the same frame (see `place.rs`).
    FollowingSubspace(FollowTarget::View(FollowMode::Snap), move || {
        HeadsetFrames();
        // A flat window shows the panels in this order, side by side. In a headset each panel has
        // its own place.
        ConnectPanel(snapshot);
        StatsPanel(snapshot);
        crate::desktop::DesktopPanel();
        ConsolePanel(console, snapshot);
        Stream(frames);
    });
}

/// The app on a phone or a tablet. The picture fills the screen. The device's motion sensors move
/// the host's camera. A tap on the picture opens the connect and stats panels over it. The Hide
/// panels button closes them. They open again when the stream ends.
#[composable]
pub fn PhoneApp() {
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
        BoxWithConstraints(Modifier::empty().fill_max_size().clickable(move |_| panels.set(!panels.get())), move |scope| {
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
    let left = remember(|| ScrollState::new(0.0)).with(|state| *state);
    let right = remember(|| ScrollState::new(0.0)).with(|state| *state);
    Row(
        Modifier::empty().fill_max_size().background(Color(0.0, 0.0, 0.0, 0.55)).padding(16.0),
        RowSpec::default().horizontal_arrangement(LinearArrangement::spaced_by(16.0)),
        move || {
            // A panel takes the taps that miss its buttons. They do not reach the picture's tap
            // handler under it.
            Column(
                Modifier::empty().weight(1.0).fill_max_height().vertical_scroll(left, false).clickable(|_| {}),
                ColumnSpec::default().vertical_arrangement(LinearArrangement::spaced_by(16.0)),
                move || {
                    ConnectBody(snapshot, address, Modifier::empty().fill_max_width(), Entry::Keyboard);
                    PhoneButtons(panels);
                },
            );
            Column(
                Modifier::empty().weight(1.0).fill_max_height().vertical_scroll(right, false).clickable(|_| {}),
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
fn HeadsetFrames() {
    let reader = remember(|| Rc::new(RefCell::new(FrameReader::default()))).with(Clone::clone);
    SpatialFrameEffect(move |frame| {
        let now = now_ns();
        let read = reader.borrow_mut().read(frame, now);
        console_panel::on_frame(&read, now);
        hub().feed.push(read);
    });
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

/// Starts or ends the connection, as the button of the panel says.
fn toggle_connection(state: &LinkState, address: TextFieldState) {
    match state {
        LinkState::Idle | LinkState::Failed(_) => {
            let host = address.text();
            settings::remember_host(&host);
            hub().connect(host, settings::media_choice());
        }
        LinkState::Connecting | LinkState::Streaming => hub().disconnect(),
    }
}

/// How the connect panel takes the address.
#[derive(Clone, Copy, PartialEq)]
enum Entry {
    /// A keypad in the panel, because a headset has no keyboard in the room. A focused field asks
    /// for the system keyboard too, where `system_keyboard` says so.
    Keypad { system_keyboard: bool },
    /// The system keyboard only, as on a phone
    Keyboard,
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

/// The host address, a keypad for it and the Connect button. A headset has no keyboard in the
/// room, so the keypad edits the same text as the field does.
#[composable]
fn ConnectPanel(snapshot: MutableState<Snapshot>) {
    let address = address_field();
    // In a headset a focused field must not ask for a software keyboard
    let in_headset = local_spatial_capabilities().current().is_spatial_ui_enabled();
    let pose = remember(|| place::start_pose("connect", side_pose(-1.0))).with(|pose| *pose);
    SpatialPanel(
        place::movable("connect", SubspaceModifier::new().size(760.0, 840.0), pose, |_| {}),
        move || ConnectBody(snapshot, address, Modifier::empty().fill_max_size(), Entry::Keypad { system_keyboard: !in_headset }),
    );
}

/// The content of the connect panel. `modifier` gives its size.
#[composable]
fn ConnectBody(snapshot: MutableState<Snapshot>, address: TextFieldState, modifier: Modifier, entry: Entry) {
    let snap = snapshot.get();
    let state = snap.state.clone();
    Column(
        modifier.background(PANEL_COLOR).rounded_corners(24.0).padding(36.0),
        ColumnSpec::default().vertical_arrangement(LinearArrangement::spaced_by(20.0)),
        move || {
            Text("cranvr", Modifier::empty(), text_style(44.0, TEXT_COLOR, Some(FontWeight::BOLD)));
            Text(
                "Address of the Mac that runs cranvr-host --protocol native",
                Modifier::empty(),
                text_style(22.0, MUTED_COLOR, None),
            );
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
                            show_keyboard_on_focus: entry != (Entry::Keypad { system_keyboard: false }),
                            ..BasicTextFieldOptions::default()
                        },
                    );
                },
            );
            if matches!(entry, Entry::Keypad { .. }) {
                Keypad(address);
            }
            ConnectButton(state.clone(), address);
            StatusLine(state.clone(), address);
            ForgetHostButton(address);
            Text(
                format!("Pairing code {}", format_code(settings::identity().code())),
                Modifier::empty(),
                text_style(22.0, MUTED_COLOR, None),
            );
        },
    );
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

#[composable]
fn ConnectButton(state: LinkState, address: TextFieldState) {
    let (label, color) = match state {
        LinkState::Idle | LinkState::Failed(_) => ("Connect", ACCENT_COLOR),
        LinkState::Connecting => ("Cancel", STOP_COLOR),
        LinkState::Streaming => ("Disconnect", STOP_COLOR),
    };
    Button(
        Modifier::empty().background(color).rounded_corners(16.0).padding_symmetric(44.0, 20.0),
        ButtonSpec::default(),
        move || toggle_connection(&state, address),
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
fn StatusLine(state: LinkState, address: TextFieldState) {
    let (text, color) = state_text(&state, &address.text());
    Text(text, Modifier::empty(), text_style(24.0, color, None));
}

fn state_text(state: &LinkState, address: &str) -> (String, Color) {
    match state {
        LinkState::Idle => ("Not connected".into(), MUTED_COLOR),
        LinkState::Connecting => (format!("Connecting to {}...", address.trim()), WARN_COLOR),
        LinkState::Streaming => ("Connected".into(), GOOD_COLOR),
        LinkState::Failed(message) => (message.clone(), BAD_COLOR),
    }
}

/// The numbers of the link: rates, decoder times, loss and latency.
#[composable]
fn StatsPanel(snapshot: MutableState<Snapshot>) {
    let pose = remember(|| place::start_pose("stats", side_pose(1.0))).with(|pose| *pose);
    SpatialPanel(
        place::movable("stats", SubspaceModifier::new().size(700.0, 840.0), pose, |_| {}),
        move || StatsBody(snapshot, Modifier::empty().fill_max_size()),
    );
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
    let (link, link_color) = state_text(&snap.state, "the host");
    let mut lines = vec![("Link", link, link_color)];
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
    for (label, value) in crate::desktop::stat_lines().into_iter().chain(crate::audio::stat_lines()) {
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
        "round trip {} ms, network {} ms, pose to decode {} ms",
        ms(stats.rtt_ms),
        ms(stats.network_ms),
        ms(stats.sample_to_decode_ms)
    )
}
