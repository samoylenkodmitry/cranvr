use crate::{
    backend::{self, HeadsetApp, Model, Settings, StreamPath, StreamProtocol},
    black_eye,
    launch::{self, GameOptions},
    setup::{self, Progress, Setup},
    steam::{self, SteamState},
    system::{self, Permission},
    usb::{self, Connection, Download},
};
use cranpose::prelude::*;
use cranpose::text::{FontWeight, TextUnit};
use cranpose::widgets::{FlowRow, FlowSpec};
use std::{
    thread,
    time::{Duration, Instant},
};

const FPS_CHOICES: [u32; 3] = [72, 90, 120];
const MBPS_CHOICES: [u32; 4] = [50, 100, 150, 200];
const GAME_SCALE_CHOICES: [f32; 5] = [0.5, 0.6, 0.7, 0.85, 1.0];
const PERMISSIONS: [Permission; 2] = [Permission::ScreenRecording, Permission::Accessibility];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Play,
    Desktop,
    Settings,
    Log,
}

fn sized(size: f32, bold: bool) -> TextStyle {
    TextStyle {
        span_style: SpanStyle {
            font_size: TextUnit::Sp(size),
            font_weight: bold.then_some(FontWeight::BOLD),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn dim_style() -> TextStyle {
    colored(130, 136, 148)
}

fn colored(r: u8, g: u8, b: u8) -> TextStyle {
    TextStyle { span_style: SpanStyle { color: Some(Color::from_rgb_u8(r, g, b)), ..Default::default() }, ..Default::default() }
}

fn flow() -> FlowSpec {
    FlowSpec::new().main_axis_spacing(8.0).cross_axis_spacing(8.0)
}

#[composable]
fn Label(text: String) {
    Text(text, Modifier::empty(), TextStyle::default());
}

#[composable]
fn Hint(text: String) {
    Text(text, Modifier::empty(), dim_style());
}

#[composable]
fn Heading(text: String) {
    Text(text, Modifier::empty(), sized(18.0, true));
}

const BUTTON: (u8, u8, u8) = (52, 58, 70);
const SELECTED: (u8, u8, u8) = (38, 98, 190);
const PRIMARY: (u8, u8, u8) = (30, 130, 80);

#[composable]
fn Action(label: String, on_click: impl Fn() + 'static) {
    Styled(label, BUTTON, on_click);
}

#[composable]
fn Primary(label: String, on_click: impl Fn() + 'static) {
    Styled(label, PRIMARY, on_click);
}

/// A button; a selected one has a blue background.
#[composable]
fn Choice(label: String, selected: bool, on_click: impl Fn() + 'static) {
    Styled(label, if selected { SELECTED } else { BUTTON }, on_click);
}

#[composable]
fn Styled(label: String, color: (u8, u8, u8), on_click: impl Fn() + 'static) {
    Button(
        Modifier::empty()
            .rounded_corners(8.0)
            .background(Color::from_rgb_u8(color.0, color.1, color.2))
            .height_in(36.0, f32::INFINITY)
            .padding(8.0),
        ButtonSpec::default(),
        on_click,
        move || {
            Text(label.clone(), Modifier::empty(), TextStyle::default());
        },
    );
}

/// A rounded panel around a group of rows.
#[composable]
fn Card(content: impl FnMut() + 'static) {
    Column(
        Modifier::empty()
            .fill_max_width()
            .rounded_corners(12.0)
            .background(Color::from_rgb_u8(34, 38, 47))
            .padding(16.0),
        ColumnSpec::default().vertical_arrangement(LinearArrangement::SpacedBy(10.0)),
        content,
    );
}

/// Saves new settings and restarts a running host with them.
fn change(settings: MutableState<Settings>, model: MutableState<Model>, edit: impl FnOnce(&mut Settings)) {
    settings.update(edit);
    let s = settings.with(Clone::clone);
    s.save();
    if backend::host_running() {
        let result = backend::start_host(&s, model.with(|m| m.tcp_fallback), None);
        model.update(|m| m.message = result.err());
    }
}

// A start by hand begins a fresh check of the network: old notices go away. A pairing code opens
// the pairing window of the native protocol for this start only. It serves a headset with the older
// cranvr app. The window's field for it is gone: a newer headset asks the user with a card.
fn start_host(settings: MutableState<Settings>, model: MutableState<Model>, pair_code: Option<&str>) {
    model.update(|m| {
        m.network.clear();
        m.audio = None;
    });
    let result = backend::start_host(&settings.with(Clone::clone), model.with(|m| m.tcp_fallback), pair_code);
    model.update(|m| {
        m.running = result.is_ok();
        m.message = result.err();
    });
}

/// Games see the headset only while the host runs.
fn ensure_host(settings: MutableState<Settings>, model: MutableState<Model>) {
    if !backend::host_running() {
        start_host(settings, model, None);
    }
}

/// Reads what changes outside the window: host output every 150 ms, the rest every 2 s.
fn refresh(settings: MutableState<Settings>, model: MutableState<Model>, slow: bool) {
    let mut next = model.with(Clone::clone);
    backend::pump(&mut next);
    // The cable counts for the cranvr client only: the ALVR client does not use it
    usb::set_choice(settings.with(|s| if s.headset_app == HeadsetApp::Cranvr { s.connection } else { Connection::WiFi }));
    next.usb = usb::status();
    let requests = std::mem::take(&mut next.vr_requests);
    vr_requests(settings, &mut next, requests);
    let reports = std::mem::take(&mut next.black_eyes);
    black_eye_reports(settings, &mut next, reports);
    // In Auto mode the stream moves to TCP once, when the host finds that UDP data does not
    // arrive. A problem over TCP stays on screen: switching back would only repeat the first one.
    // The cranvr client picks its media path itself.
    if std::mem::take(&mut next.udp_blocked)
        && !next.tcp_fallback
        && settings.with(|s| s.headset_app == HeadsetApp::Alvr && s.stream_protocol == StreamProtocol::Auto)
    {
        next.tcp_fallback = true;
        next.network.push(
            "cranvr switched the stream to TCP. TCP gets through such a VPN; UDP has less delay on a clean network."
                .into(),
        );
        if let Err(e) = backend::start_host(&settings.with(Clone::clone), true, None) {
            next.message = Some(e);
        }
    }
    // A headset that asks to connect has a user who may not look at the Mac: the window comes up
    let waiting = |m: &Model| m.native.iter().flat_map(|n| n.pending.iter().map(|r| r.key.clone())).collect::<Vec<_>>();
    let known = model.with(|m| waiting(m));
    if waiting(&next).iter().any(|key| !known.contains(key)) {
        crate::vr_window::bring_to_front();
    }
    // A finished setup changed what the Play tab shows: read the state now, not in 2 s
    let setup_ended = model.with(|m| m.setup.as_ref().is_some_and(Setup::running)) && !next.setup.as_ref().is_some_and(Setup::running);
    if slow || setup_ended {
        next.clients = backend::read_clients();
        next.lan = system::lan_addresses();
        next.permissions = PERMISSIONS.map(Permission::granted);
        next.steam = steam::state();
        next.rosetta = steam::rosetta_ready();
        next.game_support = steam::game_support_installed();
        next.game_support_outdated = steam::game_support_outdated();
        next.steam_running = next.steam == SteamState::Ready && steam::running();
        next.steam_signed_in = steam::signed_in();
        next.games = steam::games();
    }
    // The window follows the headset into VR and back
    // The window follows a streaming headset into VR and back. With the main display switch the
    // host's report of the switch decides; without it, the first stats of a stream do.
    let switch = settings.with(|s| s.vr_main_display);
    let in_vr = |m: &Model| if switch { m.vr_main } else { m.headset.is_some() && m.stats.is_some() };
    let (was_in_vr, had_display) = model.with(|m| (in_vr(m), m.vr_display));
    if let Some(display) = next.vr_display {
        match (was_in_vr, in_vr(&next)) {
            (false, true) if settings.with(|s| s.window_in_vr) => crate::vr_window::move_into_vr(display),
            (true, false) => crate::vr_window::move_out_of_vr(display),
            // A new virtual display without a headset: the window must not sit on it
            (false, false) if had_display != Some(display) => crate::vr_window::move_out_of_vr(display),
            _ => (),
        }
    }
    if next.notice.as_ref().is_some_and(|(_, until)| *until <= Instant::now()) {
        next.notice = None;
    }
    backend::write_vr_menu(&settings.with(Clone::clone), &next.games, next.notice.as_ref().map(|(notice, _)| notice.headset.as_str()));
    if model.with(|m| *m != next) {
        model.set(next);
    }
}

/// Answers the host's reports of a black right eye (see black_eye.rs): the program's option "Each
/// eye in its own pass" turns on once, and the game starts again with it. A report with the option
/// on already only shows a notice.
fn black_eye_reports(settings: MutableState<Settings>, next: &mut Model, reports: Vec<serde_json::Value>) {
    for report in reports {
        let mut response = None;
        settings.update(|s| response = Some(black_eye::respond(&mut s.launch, &next.games, report["program"].as_str())));
        let Some(black_eye::Response { notice, restart }) = response else { continue };
        next.notice = Some((notice, Instant::now() + black_eye::NOTICE_TIME));
        if let Some(program) = restart {
            // The new start reads the saved options
            settings.with(Settings::save);
            thread::spawn(move || {
                if let Err(e) = steam::restart(&program) {
                    backend::notify(e);
                }
            });
        }
    }
}

/// Runs the requests of the console in VR, as the window's own buttons would.
fn vr_requests(settings: MutableState<Settings>, next: &mut Model, requests: Vec<serde_json::Value>) {
    for request in requests {
        match request["action"].as_str() {
            Some("play") => {
                let name = request["game"].as_str().unwrap_or_default().to_string();
                let games = if next.games.is_empty() { steam::games() } else { next.games.clone() };
                match games.into_iter().find(|g| g.name == name) {
                    // Steam may need seconds to start: keep the window responsive
                    Some(game) => {
                        thread::spawn(move || {
                            if let Err(e) = steam::play(&game) {
                                backend::notify(e);
                            }
                        });
                    }
                    None => next.message = Some(format!("The console asked for {name}, which is not installed")),
                }
            }
            Some("stop") => {
                thread::spawn(steam::stop_all);
            }
            Some("set") => {
                let before = settings.with(Clone::clone);
                let mut after = before.clone();
                if let Some(fps) = request["fps"].as_u64() {
                    after.fps = fps as u32;
                }
                if let Some(scale) = request["game_scale"].as_f64() {
                    after.game_scale = scale as f32;
                }
                if let Some(samples) = request["msaa"].as_u64() {
                    after.game_msaa_limit = samples as u32;
                }
                if let Some(on) = request["low_latency"].as_bool() {
                    after.low_latency = on;
                }
                if after == before {
                    continue;
                }
                after.save();
                settings.set(after.clone());
                // The anti-aliasing limit applies at a game's start; the rest are the host's options
                let host_options = |s: &Settings| (s.fps, s.game_scale.to_bits(), s.low_latency);
                if host_options(&before) != host_options(&after) && backend::host_running() {
                    if let Err(e) = backend::start_host(&after, next.tcp_fallback, None) {
                        next.message = Some(e);
                    }
                }
            }
            _ => {}
        }
    }
}

#[composable]
pub fn CranvrApp() {
    let settings = rememberMutableStateOf(Settings::load);
    let model = rememberMutableStateOf(Model::default);
    let tab = rememberMutableStateOf(|| Tab::Play);

    LaunchedEffect((), async move |_| {
        usb::start(backend::native_port());
        refresh(settings, model, true);
        // macOS asks for each missing permission once, when cranvr starts
        if system::in_app_bundle() {
            for (permission, granted) in PERMISSIONS.into_iter().zip(model.with(|m| m.permissions)) {
                if !granted {
                    permission.prompt();
                }
            }
        }
        if settings.with(|s| s.autostart) && !backend::host_running() {
            start_host(settings, model, None);
        }
        let mut last_slow = Instant::now();
        loop {
            delay(Duration::from_millis(150)).await;
            let slow = last_slow.elapsed() >= Duration::from_secs(2);
            if slow {
                last_slow = Instant::now();
            }
            refresh(settings, model, slow);
        }
    });

    let scroll = rememberScrollState!(0.0);
    Column(
        Modifier::empty().fill_max_size().vertical_scroll(scroll, false).padding(24.0),
        ColumnSpec::default().vertical_arrangement(LinearArrangement::SpacedBy(14.0)),
        move || {
            Header(settings, model);
            PermissionCard(model);
            PairingCard(model);
            HeadsetCard(settings, model);
            if let Some(message) = model.with(|m| m.message.clone()) {
                Text(message, Modifier::empty(), colored(240, 170, 90));
            }
            if let Some((notice, _)) = model.with(|m| m.notice.clone()) {
                Text(notice.window, Modifier::empty(), colored(240, 170, 90));
            }
            let current = tab.get();
            FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                for (t, name) in [(Tab::Play, "Play"), (Tab::Desktop, "Desktop"), (Tab::Settings, "Settings"), (Tab::Log, "Log")] {
                    Choice(name.into(), current == t, move || tab.set(t));
                }
            });
            match current {
                Tab::Play => PlayTab(settings, model),
                Tab::Desktop => DesktopTab(settings, model),
                Tab::Settings => SettingsTab(settings, model),
                Tab::Log => LogTab(model),
            }
        },
    );
}

#[composable]
fn Header(settings: MutableState<Settings>, model: MutableState<Model>) {
    let running = model.with(|m| m.running);
    FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
        Text("cranvr", Modifier::empty(), sized(26.0, true));
        if running {
            Text("Host running".to_string(), Modifier::empty().padding(8.0), colored(110, 200, 140));
            Action("Stop".into(), move || {
                backend::stop_host();
                model.update(|m| m.running = false);
            });
        } else {
            Text("Host stopped".to_string(), Modifier::empty().padding(8.0), dim_style());
            Primary("Start".into(), move || start_host(settings, model, None));
        }
    });
}

/// The permissions macOS has not given yet, each with a button that asks for it.
#[composable]
fn PermissionCard(model: MutableState<Model>) {
    let permissions = model.with(|m| m.permissions);
    if permissions.iter().all(|g| *g) {
        return;
    }
    Card(move || {
        Text("cranvr needs your permission".to_string(), Modifier::empty(), {
            let mut style = sized(18.0, true);
            style.span_style.color = Some(Color::from_rgb_u8(240, 170, 90));
            style
        });
        for (permission, granted) in PERMISSIONS.into_iter().zip(permissions) {
            if granted {
                continue;
            }
            Label(format!("{}: {}.", permission.title(), permission.reason()));
            FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                Primary(format!("Allow {}", permission.title()), move || permission.request());
                Action("Open System Settings".into(), move || permission.open_settings());
            });
        }
        Hint("In System Settings, turn on the switch next to cranvr. This card goes away by itself.".into());
    });
}

/// A headset asks to connect. The headset shows a code, and the card shows the same one. The user
/// allows the headset only when the two codes match: a device that poses as this Mac to the
/// headset shows another code.
#[composable]
fn PairingCard(model: MutableState<Model>) {
    let waiting = model.with(|m| m.native.as_ref().map(|n| n.pending.clone()).unwrap_or_default());
    for request in waiting {
        Card(move || {
            Text(format!("{} wants to connect.", request.name), Modifier::empty(), {
                let mut style = sized(18.0, true);
                style.span_style.color = Some(Color::from_rgb_u8(240, 170, 90));
                style
            });
            Text(format!("It shows the code {}.", request.code), Modifier::empty(), sized(28.0, true));
            Hint(format!("Press Allow only when the headset shows the same code. Its address is {}.", request.ip));
            let key = request.key.clone();
            FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                let (allow, deny) = (key.clone(), key.clone());
                Primary("Allow".into(), move || backend::answer_pairing(&allow, true));
                Action("Deny".into(), move || backend::answer_pairing(&deny, false));
            });
        });
    }
}

#[composable]
fn HeadsetCard(settings: MutableState<Settings>, model: MutableState<Model>) {
    Card(move || {
        let native = settings.with(|s| s.headset_app == HeadsetApp::Cranvr);
        match model.with(|m| m.headset.clone()) {
            Some(h) => {
                Text(
                    format!(
                        "Headset connected: {}x{} per eye at {:.0} Hz{}",
                        h.eye.0,
                        h.eye.1,
                        h.fps,
                        if h.foveated { ", foveated" } else { "" }
                    ),
                    Modifier::empty(),
                    colored(110, 200, 140),
                );
                // The way of the stream: the user who chose the cable sees when it is not used
                if let Some(path) = h.path {
                    let (text, wanted_cable) = (
                        match path {
                            StreamPath::Usb => "Connected by USB cable",
                            StreamPath::WiFi => "Connected over Wi-Fi",
                        },
                        settings.with(|s| s.connection == Connection::Usb),
                    );
                    Text(text.to_string(), Modifier::empty(), colored(110, 200, 140));
                    if wanted_cable && path == StreamPath::WiFi {
                        Hint("The USB cable is chosen, but this headset is not on it. Plug it into this Mac.".into());
                    }
                }
                if let Some(st) = model.with(|m| m.stats.clone()) {
                    let latency = st.latency_ms.map(|ms| format!(", {ms:.0} ms from motion to display")).unwrap_or_default();
                    Hint(format!(
                        "{:.0} fps, render {:.1} ms, encode {:.1} ms, {} Mbps{latency}",
                        st.fps, st.render_ms, st.encode_ms, st.mbps
                    ));
                }
            }
            None => {
                Label("No headset connected".into());
                if native {
                    NativeConnect(settings, model);
                } else {
                    Hint("On the Quest, open the ALVR app (version 20.14, from the Meta store). Use the same Wi-Fi as this Mac, on 5 GHz.".into());
                }
            }
        }
        if let Some((false, why)) = model.with(|m| m.audio.clone()) {
            Text(format!("No game sound in the headset: {why}"), Modifier::empty(), colored(230, 180, 90));
        }
        for note in model.with(|m| m.network.clone()) {
            Text(note, Modifier::empty(), colored(230, 180, 90));
        }
        // The list comes from ALVR's session file: the cranvr client does not use it
        let clients = if native { vec![] } else { model.with(|m| m.clients.clone()) };
        for client in clients {
            let paired = client.trusted || settings.with(|s| s.trusted.contains(&client.hostname));
            let name = client.hostname.clone();
            FlowRow(Modifier::empty(), flow(), move || {
                Text(name.clone(), Modifier::empty().padding(8.0), TextStyle::default());
                if paired {
                    Text("paired".to_string(), Modifier::empty().padding(8.0), dim_style());
                } else {
                    let name = name.clone();
                    Primary(format!("Pair with {name}"), move || {
                        let name = name.clone();
                        change(settings, model, move |s| {
                            if !s.trusted.contains(&name) {
                                s.trusted.push(name);
                            }
                        });
                        ensure_host(settings, model);
                    });
                }
            });
        }
    });
}

/// What the user does to connect the cranvr client: open it on the headset. The headset finds this
/// Mac. The Mac's address is the way for a headset that finds nothing.
#[composable]
fn NativeConnect(settings: MutableState<Settings>, model: MutableState<Model>) {
    let (native, lan) = model.with(|m| (m.native.clone(), m.lan.clone()));
    let port = native.as_ref().map_or(backend::native_port(), |n| n.port);
    UsbNote(settings, model);
    Label("Open cranvr on the headset. It finds this Mac.".into());
    Hint("Put the headset on the same Wi-Fi as this Mac, on 5 GHz. The first time, this window asks you to allow the headset.".into());
    match &native {
        None => Hint("The host tells which headsets are paired while it runs.".into()),
        Some(n) if n.paired.is_empty() => Hint("No headset is paired yet.".into()),
        Some(n) => Hint(format!("Paired headsets: {}", n.paired.join(", "))),
    }
    if lan.is_empty() {
        Hint("This Mac has no address on a local network. Connect it to Wi-Fi or Ethernet.".into());
    } else {
        let addresses: Vec<String> = lan.iter().map(|ip| format!("{ip}:{port}")).collect();
        Hint(format!("If the headset does not find this Mac, enter its address in the headset: {}", addresses.join(" or ")));
    }
}

/// What the cable does, in words, while no headset streams. A Mac whose user never plugs a headset
/// in sees nothing here: the lines need a device on the cable or the choice of the cable.
#[composable]
fn UsbNote(settings: MutableState<Settings>, model: MutableState<Model>) {
    let choice = settings.with(|s| s.connection);
    if !choice.allows_usb() {
        return;
    }
    let status = model.with(|m| m.usb.clone());
    if let Some(blocked) = &status.blocked {
        Text(blocked.clone(), Modifier::empty(), colored(240, 170, 90));
        return;
    }
    if status.unauthorized {
        Text("Put on the headset and allow USB debugging.".to_string(), Modifier::empty(), colored(240, 170, 90));
    }
    if status.headsets.iter().any(|h| h.reversed) {
        Text("The headset is on the USB cable. Open cranvr on the headset to stream over it.".to_string(), Modifier::empty(), colored(110, 200, 140));
    } else if choice == Connection::Usb && !status.unauthorized {
        Text(
            if status.adb.is_none() && status.looked {
                "USB needs a tool that cranvr does not have yet. Press Add USB support in the Settings tab."
            } else {
                "Plug the headset into this Mac with a USB cable."
            }
            .to_string(),
            Modifier::empty(),
            colored(230, 180, 90),
        );
    }
}

#[composable]
fn PlayTab(settings: MutableState<Settings>, model: MutableState<Model>) {
    let (state, rosetta, outdated, setup) = model.with(|m| (m.steam, m.rosetta, m.game_support_outdated, m.setup.clone()));
    let open_program = rememberMutableStateOf(|| false);
    let plan = setup::plan(rosetta, state, outdated);
    // Steam cannot start before the setup. An outdated game support alone leaves it working: the
    // library shows an Update button then.
    let needs_setup = !rosetta || state != SteamState::Ready;
    let running = setup.as_ref().is_some_and(Setup::running);
    Card(move || {
        Heading("Steam".into());
        if running {
            let setup = setup.clone().unwrap_or(Setup { steps: vec![] });
            let steam_opens = setup.steps.iter().any(|(step, _)| *step == setup::Step::Steam);
            SetupProgress(setup);
            Hint(if steam_opens {
                "Keep cranvr open. This takes a few minutes. Steam opens at the end: sign in there once."
            } else {
                "Keep cranvr open. Steam and running games stop while this runs."
            }
            .into());
        } else if needs_setup {
            SetupStart(model, plan.clone(), setup.clone());
        } else {
            SteamLibrary(settings, model, setup.clone());
        }
    });
    // Before the game support step there is no Wine to start a program with or to set up. While a
    // setup runs, its card is the one thing on the tab.
    if running || state == SteamState::NoWine {
        return;
    }
    Card(move || {
        Heading("Other Windows programs".into());
        Hint("Any Windows VR program that uses OpenXR with Direct3D 11.".into());
        FlowRow(Modifier::empty(), flow(), move || {
            Action("Choose a program".into(), move || {
                let start = steam::prefix().join("drive_c");
                let picked = rfd::FileDialog::new()
                    .set_title("Choose a Windows program")
                    .add_filter("Windows program", &["exe"])
                    .set_directory(&start)
                    .pick_file();
                if let Some(path) = picked {
                    ensure_host(settings, model);
                    let result = steam::run_program(&path);
                    let shown = path.display().to_string();
                    change(settings, model, move |s| s.game = shown);
                    model.update(|m| m.message = result.err());
                }
            });
            Action("Stop Steam and games".into(), steam::stop_all);
        });
        // The options of the program chosen last, which the button above starts
        let last = settings.with(|s| s.game.clone());
        if last.starts_with('/') {
            let program = steam::mac_to_windows(std::path::Path::new(&last));
            let name = std::path::Path::new(&last).file_name().map_or(last.clone(), |name| name.to_string_lossy().into_owned());
            let own = settings.with(|s| s.launch.contains_key(&program));
            let shown = open_program.get();
            FlowRow(Modifier::empty(), flow(), move || {
                Choice(format!("Options of {name}{}", if own { " (own)" } else { "" }), shown, move || open_program.set(!shown));
            });
            if shown {
                LaunchOptions(settings, program);
            }
        }
        match model.with(|m| m.game.clone()) {
            Some(name) => {
                Text(format!("A game is connected ({name})."), Modifier::empty(), colored(110, 200, 140));
            }
            None => {
                Text("No game connected.".to_string(), Modifier::empty(), dim_style());
            }
        }
    });
    WineCard(settings);
}

/// The steps of a setup, each with its progress line.
#[composable]
fn SetupProgress(setup: Setup) {
    for (step, progress) in setup.steps {
        let (text, style) = match progress {
            Progress::Waiting => ("Waiting".to_string(), dim_style()),
            Progress::Working(text) => (text, TextStyle::default()),
            Progress::Done => ("Done".to_string(), colored(110, 200, 140)),
            Progress::Failed(message) => (format!("Failed: {message}"), colored(240, 170, 90)),
        };
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            Text(step.title().to_string(), Modifier::empty().width(170.0).padding(8.0), sized(16.0, true));
            Text(text.clone(), Modifier::empty().padding(8.0), style.clone());
        });
    }
}

/// Before the setup of a Mac that cannot run games yet: what the one button does, and what
/// stays for the user. After a failed run the failure stays on screen above the button.
#[composable]
fn SetupStart(model: MutableState<Model>, plan: Vec<setup::Step>, last: Option<Setup>) {
    let failed = last.as_ref().is_some_and(Setup::failed);
    let state = model.with(|m| m.steam);
    let steam_opens = plan.contains(&setup::Step::Steam);
    Label(if state == SteamState::NoWine {
        "This Mac needs a one-time setup before it can run Windows VR games. One button does it."
    } else {
        "Steam needs a setup step. One button does it."
    }
    .into());
    match last.filter(|_| failed) {
        Some(last) => SetupProgress(last),
        None => {
            for step in plan {
                FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                    Text(step.title().to_string(), Modifier::empty().width(170.0).padding(8.0), sized(16.0, true));
                    Text(step.describe(state).to_string(), Modifier::empty().padding(8.0), dim_style());
                });
            }
        }
    }
    FlowRow(Modifier::empty(), flow(), move || {
        Primary(if failed { "Retry".into() } else { "Set up games".into() }, setup::start);
    });
    if steam_opens {
        Hint("When it ends, Steam opens. Sign in to Steam there once. Your installed games show up here afterwards.".into());
    }
}

/// The notice of a game support that is older than this app expects, with its one button.
#[composable]
fn GameSupportUpdate(installed: Option<String>, last: Option<Setup>) {
    let newest = steam::game_support_expected();
    Label(match installed {
        Some(version) => format!("Your game support is version {version}. Version {newest} is available."),
        None => format!("Your game support is older than version {newest}."),
    });
    if let Some(last) = last.filter(Setup::failed) {
        SetupProgress(last);
    }
    FlowRow(Modifier::empty(), flow(), move || {
        Primary("Update game support".into(), setup::start);
    });
    Hint("The update replaces the OpenXR runtime and DXMT in Wine. It stops Steam and running games first. Your games and your Steam login stay.".into());
}

#[composable]
fn SteamLibrary(settings: MutableState<Settings>, model: MutableState<Model>, last: Option<Setup>) {
    let (running, signed_in, games, outdated, installed) =
        model.with(|m| (m.steam_running, m.steam_signed_in, m.games.clone(), m.game_support_outdated, m.game_support.clone()));
    // The game whose launch options are open, by its program
    let open = rememberMutableStateOf(|| None::<String>);
    if outdated {
        GameSupportUpdate(installed, last);
    }
    if !signed_in {
        Label(if running {
            "Sign in to Steam in the Steam window. Steam remembers you after that.".into()
        } else {
            "Open Steam and sign in once. Steam remembers you after that.".into()
        });
    }
    FlowRow(Modifier::empty(), flow(), move || {
        if running {
            Text("Steam is running.".to_string(), Modifier::empty().padding(8.0), colored(110, 200, 140));
        } else {
            Primary("Open Steam".into(), move || {
                let result = steam::start();
                model.update(|m| m.message = result.err());
            });
        }
    });
    Hint("Games on another disk: in the Steam window, open Settings, then Storage, and add a folder on that disk (it sits under Z:\\Volumes). The games installed there show up here.".into());
    if games.is_empty() {
        Hint(if signed_in {
            "No games installed yet. Install them in the Steam window; they show up here.".into()
        } else {
            "Your installed games show up here.".into()
        });
        return;
    }
    Hint("Play starts Steam if needed, then the game. Put on the headset first. Options sets what the game starts with.".into());
    for game in games {
        let g = game.clone();
        let program = steam::program(&game);
        let shown = program.as_ref().is_some_and(|program| open.with(|open| open.as_ref() == Some(program)));
        let own = program.as_ref().is_some_and(|program| settings.with(|s| s.launch.contains_key(program)));
        let toggled = program.clone();
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            Text(g.name.clone(), Modifier::empty().padding(8.0), TextStyle::default());
            if let Some(program) = toggled.clone() {
                Choice(if own { "Options (own)".into() } else { "Options".into() }, shown, move || {
                    let program = program.clone();
                    open.update(move |open| *open = if open.as_ref() == Some(&program) { None } else { Some(program) });
                });
            }
            if g.exe.is_some() {
                let game = g.clone();
                Primary("Play".into(), move || {
                    ensure_host(settings, model);
                    let game = game.clone();
                    // Steam may need seconds to start: keep the window responsive
                    thread::spawn(move || {
                        if let Err(e) = steam::play(&game) {
                            backend::notify(e);
                        }
                    });
                });
            } else {
                Hint("no program found in its folder".into());
            }
        });
        if let Some(program) = program.filter(|_| shown) {
            LaunchOptions(settings, program);
        }
    }
}

/// The launch options of one program, under its row: what cranvr adds when the program starts.
/// The common switch is in view. The raw fields sit behind "Advanced", which starts open for a
/// program that already has values in them.
#[composable]
fn LaunchOptions(settings: MutableState<Settings>, program: String) {
    let (options, wine) = settings.with(|s| (s.launch.get(&program).cloned().unwrap_or_default(), s.wine.clone()));
    let raw_fields_set = !(options.dll_overrides.is_empty() && options.env.is_empty() && options.args.is_empty());
    let advanced = rememberMutableStateOf(move || raw_fields_set);
    let dll = cranpose_core::remember(|| TextFieldState::new(&options.dll_overrides)).with(|state| *state);
    let env = cranpose_core::remember(|| TextFieldState::new(&options.env)).with(|state| *state);
    let args = cranpose_core::remember(|| TextFieldState::new(&options.args)).with(|state| *state);
    Column(
        Modifier::empty().fill_max_width().padding_symmetric(16.0, 0.0),
        ColumnSpec::default().vertical_arrangement(LinearArrangement::SpacedBy(6.0)),
        move || {
            let key = program.clone();
            let shown = advanced.get();
            FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                let key = key.clone();
                Choice("Each eye in its own pass".into(), options.eye_per_pass, move || {
                    edit_launch(settings, &key, |o| {
                        o.eye_per_pass = !o.eye_per_pass;
                        o.eye_per_pass_declined = !o.eye_per_pass;
                        o.eye_per_pass_reason.clear();
                    })
                });
                Choice("Advanced".into(), shown, move || advanced.set(!shown));
            });
            if options.eye_per_pass && !options.eye_per_pass_reason.is_empty() {
                Hint(options.eye_per_pass_reason.clone());
            }
            Hint("For a game whose right eye stays black. The runtime then refuses two-eye images, so the game draws each eye apart. This costs frames.".into());
            if shown {
                OptionField("Wine DLL overrides", dll);
                Hint("As WINEDLLOVERRIDES takes them, such as winhttp=n,b to load a winhttp.dll from the game's folder before Wine's own. Beat Saber's mod loader BSIPA ran without it in cranvr's Wine.".into());
                OptionField("Environment", env);
                Hint("NAME=value entries, separated by spaces.".into());
                OptionField("Arguments", args);
                let key = program.clone();
                FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                    let key = key.clone();
                    Primary("Save options".into(), move || {
                        edit_launch(settings, &key, |o| {
                            o.dll_overrides = dll.text().trim().to_owned();
                            o.env = env.text().trim().to_owned();
                            o.args = args.text().trim().to_owned();
                        })
                    });
                });
            }
            let summary = launch::summary(&wine, &options);
            Hint(if summary.is_empty() { "The game starts with nothing added.".into() } else { format!("The game starts with: {summary}") });
        },
    );
}

/// A labeled text field of the launch options.
#[composable]
fn OptionField(label: &'static str, state: TextFieldState) {
    FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
        Text(label.to_string(), Modifier::empty().width(170.0).padding(8.0), TextStyle::default());
        BasicTextField(
            state,
            Modifier::empty()
                .width(420.0)
                .rounded_corners(8.0)
                .background(Color::from_rgb_u8(BUTTON.0, BUTTON.1, BUTTON.2))
                .padding(8.0)
                .content_description(label),
            TextStyle::default(),
        );
    });
}

/// Saves a change of the launch options of `program`. They apply when the program starts next, so
/// the host keeps running. Options equal to the defaults leave no entry.
fn edit_launch(settings: MutableState<Settings>, program: &str, edit: impl FnOnce(&mut GameOptions)) {
    settings.update(|s| {
        let options = s.launch.entry(program.to_owned()).or_default();
        edit(options);
        if options.is_default() {
            s.launch.remove(program);
        }
    });
    settings.with(Settings::save);
}

/// The Wine that runs the games and the options of every program that cranvr starts. The raw
/// fields sit behind "Advanced", which starts open when they hold values.
#[composable]
fn WineCard(settings: MutableState<Settings>) {
    let wine = settings.with(|s| s.wine.clone());
    let raw_fields_set = !(wine.dll_overrides.is_empty() && wine.env.is_empty());
    let advanced = rememberMutableStateOf(move || raw_fields_set);
    let dll = cranpose_core::remember(|| TextFieldState::new(&wine.dll_overrides)).with(|state| *state);
    let env = cranpose_core::remember(|| TextFieldState::new(&wine.env)).with(|state| *state);
    Card(move || {
        Heading("Wine".into());
        Label(format!("Wine: {}", steam::engine().display()));
        Label(format!("Prefix: {}", steam::prefix().display()));
        Hint("The game support step installs this Wine with DXMT and cranvr's OpenXR runtime into this prefix. Another Wine or prefix lacks them, so cranvr uses these.".into());
        let (mac_window, shown) = (wine.mac_window, advanced.get());
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            Choice("Game window on the Mac shows the picture".into(), mac_window, move || {
                settings.update(|s| s.wine.mac_window = !s.wine.mac_window);
                settings.with(Settings::save);
            });
            Choice("Advanced".into(), shown, move || advanced.set(!shown));
        });
        Hint("Off: a game's own window on the Mac stays empty and costs no GPU time. The headset shows the game either way.".into());
        if shown {
            OptionField("DLL overrides", dll);
            OptionField("Environment", env);
            Hint("For every program that cranvr starts, before its own options. Steam does not get them.".into());
            FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                Primary("Save Wine options".into(), move || {
                    settings.update(|s| {
                        s.wine.dll_overrides = dll.text().trim().to_owned();
                        s.wine.env = env.text().trim().to_owned();
                    });
                    settings.with(Settings::save);
                });
            });
            Hint(format!(
                "cranvr also always sets WINEPREFIX, WINEARCH=win64, WINEDEBUG=-all, DXMT_BASALT_IOSURFACE=1 and BASALT_CONTROL, which its runtime needs, and DXMT_MAX_SAMPLE_COUNT from the anti-aliasing setting. Steam starts with {}.",
                steam::STEAM_FLAGS.join(" ")
            ));
        }
    });
}

#[composable]
fn DesktopTab(settings: MutableState<Settings>, model: MutableState<Model>) {
    let s = settings.with(Clone::clone);
    let permissions = model.with(|m| m.permissions);
    Card(move || {
        Heading("Mac desktop in VR".into());
        Hint("A screen in front of you shows the Mac while no game runs.".into());
        let on = s.desktop;
        FlowRow(Modifier::empty(), flow(), move || {
            Choice(
                if on { "Desktop view: on".into() } else { "Desktop view: off".into() },
                on,
                move || change(settings, model, |s| s.desktop = !s.desktop),
            );
        });
        let size = s.virtual_display;
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            Text("Screen in VR".to_string(), Modifier::empty().padding(8.0), TextStyle::default());
            for (label, choice) in [
                ("The main display", None),
                ("Add a display 1920 x 1080", Some((1920, 1080))),
                ("Add a display 2560 x 1440", Some((2560, 1440))),
            ] {
                Choice(label.into(), size == choice, move || change(settings, model, move |s| s.virtual_display = choice));
            }
        });
        Hint("The headset shows your main display as it is. An added display is one more monitor that only the headset shows: drag windows onto it.".into());
        let (vr_main, window_in_vr) = (s.vr_main_display, s.window_in_vr);
        if size.is_some() {
            FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                Choice("Main display while the headset is on".into(), vr_main, move || {
                    change(settings, model, |s| s.vr_main_display = !s.vr_main_display)
                });
                Choice("Bring the cranvr window into VR".into(), window_in_vr, move || {
                    change(settings, model, |s| s.window_in_vr = !s.window_in_vr)
                });
            });
            Hint("As the main display it gets the menu bar, the Dock and new windows, Steam's too. Your other displays go back when the headset leaves.".into());
        }
        for (permission, granted) in PERMISSIONS.into_iter().zip(permissions) {
            FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                Text(
                    format!("{}: {}", permission.title(), if granted { "allowed" } else { "not allowed" }),
                    Modifier::empty().padding(8.0),
                    if granted { colored(110, 200, 140) } else { colored(240, 170, 90) },
                );
                Text(format!("({})", permission.reason()), Modifier::empty().padding(8.0), dim_style());
                if !granted {
                    Primary("Allow".into(), move || permission.request());
                    Action("Open System Settings".into(), move || permission.open_settings());
                }
            });
        }
        if !system::in_app_bundle() {
            Hint("Start cranvr from /Applications/cranvr.app: macOS then gives these permissions to cranvr. From a terminal they go to the terminal app.".into());
        }
        if permissions.iter().any(|g| !g) {
            Hint("After you allow a permission in System Settings, macOS may ask to quit and reopen cranvr.".into());
        }
    });
    let fps = settings.with(|s| s.desktop_fps);
    Card(move || {
        Heading("Controls in the headset".into());
        Label("Point the right controller at the screen to move the mouse.".into());
        Label("Pull the right trigger to click. Push the right thumbstick up or down to scroll.".into());
        Label("Click the right thumbstick to bring the screen in front of you.".into());
        FlowRow(Modifier::empty(), flow(), move || {
            Text("Screen updates".to_string(), Modifier::empty().padding(8.0), TextStyle::default());
            for rate in [30u32, 60] {
                Choice(format!("{rate} per second"), fps == rate, move || change(settings, model, |s| s.desktop_fps = rate));
            }
        });
        Hint("60 updates per second move the cursor smoother, but cost frames: each capture of a large display takes GPU time.".into());
    });
}

#[composable]
fn SettingsTab(settings: MutableState<Settings>, model: MutableState<Model>) {
    let s = settings.with(Clone::clone);
    let s1 = s.clone();
    let native = s.headset_app == HeadsetApp::Cranvr;
    Card(move || {
        Heading("Stream".into());
        let headset_app = s1.headset_app;
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            Text("Headset app".to_string(), Modifier::empty().padding(8.0), TextStyle::default());
            for (label, choice) in [("cranvr client", HeadsetApp::Cranvr), ("ALVR client", HeadsetApp::Alvr)] {
                Choice(label.into(), headset_app == choice, move || {
                    model.update(|m| {
                        m.tcp_fallback = false;
                        m.network.clear();
                        m.native = None;
                    });
                    change(settings, model, |s| s.headset_app = choice);
                });
            }
        });
        Hint(if native {
            "The cranvr client is cranvr's own app on the headset. The host speaks cranvr's protocol. A new headset asks you to allow it once. The Network choice, Low latency and trusted names count for the ALVR client only, so they are hidden."
        } else {
            "The ALVR client is the ALVR app from the Meta store. Choose the cranvr client to use cranvr's own app on the headset."
        }
        .into());
        let current_fps = s1.fps;
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            Text("Refresh rate".to_string(), Modifier::empty().padding(8.0), TextStyle::default());
            for fps in FPS_CHOICES {
                Choice(format!("{fps} Hz"), current_fps == fps, move || change(settings, model, |s| s.fps = fps));
            }
        });
        let current_mbps = s1.mbps;
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            Text("Bitrate".to_string(), Modifier::empty().padding(8.0), TextStyle::default());
            for mbps in MBPS_CHOICES {
                Choice(format!("{mbps} Mbps"), current_mbps == mbps, move || change(settings, model, |s| s.mbps = mbps));
            }
        });
        if !native {
            let protocol = s1.stream_protocol;
            FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                Text("Network".to_string(), Modifier::empty().padding(8.0), TextStyle::default());
                for (label, choice) in [("Auto", StreamProtocol::Auto), ("UDP", StreamProtocol::Udp), ("TCP", StreamProtocol::Tcp)] {
                    Choice(label.into(), protocol == choice, move || {
                        model.update(|m| {
                            m.tcp_fallback = false;
                            m.network.clear();
                        });
                        change(settings, model, |s| s.stream_protocol = choice);
                    });
                }
            });
            Hint("Auto streams over UDP and moves to TCP when UDP data does not reach this Mac, as with a VPN on the headset.".into());
        }
        let (foveation, autostart) = (s1.foveation, s1.autostart);
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            Choice("Foveated encoding".into(), foveation, move || change(settings, model, |s| s.foveation = !s.foveation));
            Choice("Start the host when cranvr opens".into(), autostart, move || {
                change(settings, model, |s| s.autostart = !s.autostart)
            });
        });
        Hint("Foveated encoding sends the edges of the view at lower detail. It makes 120 Hz possible at the full Quest 3 resolution.".into());
        let game_audio = s1.game_audio;
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            Choice("Game sound in the headset".into(), game_audio, move || {
                model.update(|m| m.audio = None);
                change(settings, model, |s| s.game_audio = !s.game_audio)
            });
        });
        Hint("The headset plays what this Mac plays. The capture takes the sound before the Mac's volume, so you can turn the Mac down. It needs Screen Recording.".into());
        let game_scale = s1.game_scale;
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            Text("Game render size".to_string(), Modifier::empty().padding(8.0), TextStyle::default());
            for scale in GAME_SCALE_CHOICES {
                Choice(format!("{:.0}%", scale * 100.0), (game_scale - scale).abs() < 0.01, move || {
                    change(settings, model, |s| s.game_scale = scale)
                });
            }
        });
        Hint("A game that makes fewer frames than the refresh rate adds 10 to 18 ms of latency. The Log tab then names the game's rate. A smaller size or lower anti-aliasing in the game helps.".into());
        if !native {
            let low_latency = s1.low_latency;
            FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                Choice("Low latency".into(), low_latency, move || change(settings, model, |s| s.low_latency = !s.low_latency));
            });
            Hint("The headset keeps one decoded frame instead of two. This saved about 4 ms in a test. A weak Wi-Fi may then stutter.".into());
        }
        let msaa = s1.game_msaa_limit;
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            Text("Game anti-aliasing".to_string(), Modifier::empty().padding(8.0), TextStyle::default());
            for (label, limit) in [("At most 2x", 2u32), ("As the game sets it", 0)] {
                Choice(label.into(), msaa == limit, move || change(settings, model, |s| s.game_msaa_limit = limit));
            }
        });
        Hint("Anti-aliasing costs a Windows game far more on the Mac: Beat Saber made 91 frames a second at 4x and 119 at 2x. With the limit, cranvr tells each game that more than 2x does not exist. It applies when a game starts. A game started from the Steam window gets it after a restart of Steam.".into());
        let unscale = s1.unscale_displays;
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            Choice("Panel pixels while a game runs".into(), unscale, move || {
                change(settings, model, |s| s.unscale_displays = !s.unscale_displays)
            });
        });
        Hint("A display in a scaled mode, such as 3008 x 1692 on a 4K TV, makes macOS draw every change at 6016 x 3384 pixels. While a game runs, cranvr gives such a display its panel's own pixels with at least as much room, so windows stay where they are. The mode comes back when the game ends. In a test the Mac's desktop then took about a third of the GPU time.".into());
        let _ = model.with(|m| m.sizes_version);
        let learned = backend::learned_game_sizes();
        if !learned.is_empty() {
            FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                for (name, share) in learned.clone() {
                    Text(format!("{name}: {:.0}%", share * 100.0), Modifier::empty().padding(8.0), TextStyle::default());
                }
                Action("Forget learned sizes".into(), move || {
                    backend::forget_game_sizes();
                    model.update(|m| m.sizes_version += 1);
                });
            });
        }
        Hint("A game that stays below the refresh rate for a minute renders smaller at its next start, as a share of the size above. A game reads its size only when it starts.".into());
        Hint("Changes restart the host; the headset reconnects by itself.".into());
    });
    if native {
        ConnectionCard(settings, model);
        let paired = model.with(|m| m.native.as_ref().map(|n| n.paired.clone()));
        Card(move || {
            Heading("Paired headsets".into());
            match &paired {
                None => Hint("The host tells which headsets are paired while it runs.".into()),
                Some(names) if names.is_empty() => {
                    Hint("None yet. Open the cranvr app on the headset. This window then asks you to allow it.".into())
                }
                Some(names) => {
                    for name in names {
                        Text(name.clone(), Modifier::empty().padding(8.0), TextStyle::default());
                    }
                    Hint(format!("Delete the line of a headset from paired-headsets.txt in {} and restart the host to forget it.", backend::config_dir().display()));
                }
            }
        });
        return;
    }
    let trusted = s.trusted.clone();
    Card(move || {
        Heading("Paired headsets".into());
        if trusted.is_empty() {
            Hint("None yet. A headset that opens ALVR shows up at the top with a Pair button.".into());
        }
        for name in trusted.clone() {
            FlowRow(Modifier::empty(), flow(), move || {
                Text(name.clone(), Modifier::empty().padding(8.0), TextStyle::default());
                let name = name.clone();
                Action("Forget".into(), move || {
                    let name = name.clone();
                    change(settings, model, move |s| s.trusted.retain(|t| *t != name));
                });
            });
        }
    });
}

/// How the stream reaches the headset (see usb.rs), and the button that adds the tool that the
/// cable needs when this Mac has none.
#[composable]
fn ConnectionCard(settings: MutableState<Settings>, model: MutableState<Model>) {
    let choice = settings.with(|s| s.connection);
    let status = model.with(|m| m.usb.clone());
    Card(move || {
        Heading("Connection".into());
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            Text("Stream by".to_string(), Modifier::empty().padding(8.0), TextStyle::default());
            for (label, option) in [("Automatic", Connection::Automatic), ("Wi-Fi", Connection::WiFi), ("USB cable", Connection::Usb)] {
                Choice(label.into(), choice == option, move || {
                    // The cable is an adb matter: the host keeps running and a stream keeps its path
                    settings.update(|s| s.connection = option);
                    settings.with(Settings::save);
                });
            }
        });
        Hint(
            match choice {
                Connection::Automatic => "The stream uses the USB cable while the headset is plugged into this Mac. Otherwise it uses Wi-Fi.",
                Connection::WiFi => "The stream always uses Wi-Fi. This Mac gives the headset no link over the cable.",
                Connection::Usb => "The stream uses the USB cable. This window says when the headset is not on it.",
            }
            .into(),
        );
        Hint("A change counts from the next connection of the headset. A running stream keeps its path.".into());
        Hint("The cable needs developer mode on the headset. Turn it on in the Meta Horizon app on your phone.".into());
        if choice.allows_usb() && status.adb.is_none() && status.looked {
            Label("This Mac has no adb tool, which the cable needs.".into());
            match status.download.clone() {
                Download::Working(text) => {
                    Text(text, Modifier::empty(), colored(230, 180, 90));
                }
                Download::Failed(text) => {
                    Text(text, Modifier::empty(), colored(240, 170, 90));
                    FlowRow(Modifier::empty(), flow(), || Primary("Try again".into(), usb::download_tools));
                }
                Download::Idle => {
                    FlowRow(Modifier::empty(), flow(), || Primary("Add USB support".into(), usb::download_tools));
                }
            }
            Hint(format!("The button downloads Google's Android platform-tools for macOS ({}) from dl.google.com into {}.", usb::TOOLS_SIZE, backend::config_dir().display()).into());
        }
    });
}

#[composable]
fn LogTab(model: MutableState<Model>) {
    Card(move || {
        let lines: Vec<String> = model.with(|m| m.log.iter().rev().take(60).rev().cloned().collect());
        if lines.is_empty() {
            Hint("Nothing yet.".into());
        }
        for line in lines {
            Text(line, Modifier::empty(), dim_style());
        }
    });
}
