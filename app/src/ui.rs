use crate::{
    backend::{self, Model, Settings, StreamPath},
    pages,
    pending::{self, Pending, Request},
    run::{self, PERMISSIONS},
    launch::GameOptions,
    setup::{self, Progress, Setup},
    steam::{self, SteamState},
    system,
    usb::{self, Connection, Download},
};
use cranpose::prelude::*;
use cranpose::text::{FontWeight, TextUnit};
use cranvr_common::{
    controls::{Action as ControlAction, Control, Controls, Hand, MenuHand, ModelsInGame},
    menu::{self, Item, Press, Style, Tone},
};
use cranpose::widgets::{FlowRow, FlowSpec};
use std::{
    thread,
    time::{Duration, Instant},
};


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
const DANGER: (u8, u8, u8) = (150, 60, 60);

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
        let result = backend::start_host(&s);
        model.update(|m| m.message = result.err());
    }
}

// A start by hand begins a fresh check of the network: old notices go away.
fn start_host(settings: MutableState<Settings>, model: MutableState<Model>) {
    model.update(|m| {
        m.network.clear();
        m.audio = None;
    });
    let result = backend::start_host(&settings.with(Clone::clone));
    model.update(|m| {
        m.running = result.is_ok();
        m.message = result.err();
    });
}

/// What the cable thread works to: the Connection setting. It does not depend on the headset app
/// or the stream protocol.
fn connection_of(settings: MutableState<Settings>) -> Connection {
    settings.with(|s| s.connection)
}

/// Reads what changes outside the window: host output every 150 ms, the rest every 2 s. The work
/// is `run::tick`, which the app with no window runs too. The window adds what only a window does:
/// it comes to the front for a headset that asks to connect, and it follows the headset into VR.
fn refresh(settings: MutableState<Settings>, model: MutableState<Model>, slow: bool) {
    let mut now = settings.with(Clone::clone);
    let mut next = model.with(Clone::clone);
    run::tick(&mut now, &mut next, slow);
    // A headset that asks to connect has a user who may not look at the Mac: the window comes up
    let waiting = |m: &Model| m.native.iter().flat_map(|n| n.pending.iter().map(|r| r.key.clone())).collect::<Vec<_>>();
    let known = model.with(|m| waiting(m));
    if waiting(&next).iter().any(|key| !known.contains(key)) {
        crate::vr_window::bring_to_front();
    }
    // The window follows a streaming headset into VR and back. With the main display switch the
    // host's report of the switch decides; without it, the first stats of a stream do.
    let switch = now.vr_main_display;
    let in_vr = |m: &Model| if switch { m.vr_main } else { m.headset.is_some() && m.stats.is_some() };
    let (was_in_vr, had_display) = model.with(|m| (in_vr(m), m.vr_display));
    if let Some(display) = next.vr_display {
        match (was_in_vr, in_vr(&next)) {
            (false, true) if now.window_in_vr => crate::vr_window::move_into_vr(display),
            (true, false) => crate::vr_window::move_out_of_vr(display),
            // A new virtual display without a headset: the window must not sit on it
            (false, false) if had_display != Some(display) => crate::vr_window::move_out_of_vr(display),
            _ => (),
        }
    }
    if settings.with(|s| *s != now) {
        settings.set(now);
    }
    if model.with(|m| *m != next) {
        model.set(next);
    }
}

/// Opens the file chooser for a program on a thread of its own. A chooser that runs inside the
/// window's event handler makes winit panic ("tried to handle event while another event is currently
/// being handled") and the app ends. The window takes the choice on its next refresh (`run::tick`).
fn choose_program(title: &'static str, start: std::path::PathBuf, add: bool) {
    thread::spawn(move || {
        let picked = rfd::FileDialog::new().set_title(title).add_filter("Windows program", &["exe"]).set_directory(&start).pick_file();
        if let Some(path) = picked {
            backend::choose(if add { backend::Chosen::Add(path) } else { backend::Chosen::Run(path) });
        }
    });
}

/// Does what a press of the window asks, as a press in the headset does (`run::act`).
fn press(settings: MutableState<Settings>, model: MutableState<Model>, request: menu::Request) {
    let mut now = settings.with(Clone::clone);
    let mut next = model.with(Clone::clone);
    run::act(&mut now, &mut next, &request);
    if settings.with(|s| *s != now) {
        settings.set(now);
    }
    if model.with(|m| *m != next) {
        model.set(next);
    }
}

/// Opens a page that a button of the model links to. The window shows the pages in its tabs, so only
/// a game's page has to open: its options show under its row.
fn open_page(model: MutableState<Model>, page: &str) {
    if let Some(program) = page.strip_prefix(pages::GAME_PAGE) {
        let program = program.to_string();
        model.update(move |m| m.open_options = if m.open_options.as_ref() == Some(&program) { None } else { Some(program) });
    }
}

/// The page of `id` as the model gives it now.
fn page_of(settings: MutableState<Settings>, model: MutableState<Model>, id: &str) -> Option<menu::Page> {
    let pages = settings.with(|s| model.with(|m| pages::pages(s, m)));
    pages.into_iter().find(|page| page.id == id)
}

fn tone_style(tone: Tone) -> TextStyle {
    match tone {
        Tone::Normal => TextStyle::default(),
        Tone::Muted => dim_style(),
        Tone::Good => colored(110, 200, 140),
        Tone::Warn => colored(240, 170, 90),
    }
}

/// The items of a page of the model (see pages.rs), as the window draws them. The headset's console
/// draws the same items from `vr-menu.json`.
#[composable]
fn PageItems(items: Vec<Item>, settings: MutableState<Settings>, model: MutableState<Model>) {
    for item in items {
        ItemView(item, settings, model);
    }
}

#[composable]
fn ItemView(item: Item, settings: MutableState<Settings>, model: MutableState<Model>) {
    match item {
        Item::Heading { text } => Heading(text),
        Item::Text { text, tone } => {
            Text(text, Modifier::empty(), tone_style(tone));
        }
        Item::Choice(choice) => {
            let hint = choice.hint.clone();
            FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                Text(choice.label.clone(), Modifier::empty().padding(8.0), TextStyle::default());
                for option in choice.options.clone() {
                    let request = menu::Request::Set { id: choice.id.clone(), subject: choice.subject.clone(), value: option.value.clone() };
                    Choice(option.label.clone(), option.value == choice.value, move || press(settings, model, request.clone()));
                }
            });
            if !hint.is_empty() {
                Hint(hint);
            }
        }
        Item::Toggle(toggle) => {
            let hint = toggle.hint.clone();
            FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                let value = if toggle.on { menu::OFF } else { menu::ON };
                let request = menu::Request::Set { id: toggle.id.clone(), subject: toggle.subject.clone(), value: value.into() };
                Choice(toggle.label.clone(), toggle.on, move || press(settings, model, request.clone()));
            });
            if !hint.is_empty() {
                Hint(hint);
            }
        }
        Item::Buttons(row) => {
            let hint = row.hint.clone();
            FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                if !row.title.is_empty() {
                    Text(row.title.clone(), Modifier::empty().padding(8.0), TextStyle::default());
                }
                for button in row.buttons.clone() {
                    let color = match button.style {
                        Style::Normal => BUTTON,
                        Style::Primary => PRIMARY,
                        Style::Danger => DANGER,
                    };
                    let press_of = button.press.clone();
                    Styled(button.words().to_string(), color, move || match &press_of {
                        Press::Run { .. } => {
                            if let Some(request) = press_of.request() {
                                press(settings, model, request);
                            }
                        }
                        Press::Open { page } => open_page(model, page),
                    });
                }
                if !row.detail.is_empty() {
                    Text(row.detail.clone(), Modifier::empty().padding(8.0), dim_style());
                }
            });
            if !hint.is_empty() {
                Hint(hint);
            }
        }
    }
}

/// The tab that the window opens on: the Play tab, or the one that `CRANVR_TAB` names (`play`,
/// `desktop`, `settings`, `controls` or `log`). A test that takes a screen capture of a page uses it,
/// because it cannot press the tab's button.
fn start_tab() -> Tab {
    match std::env::var("CRANVR_TAB").as_deref() {
        Ok("desktop") => Tab::Desktop,
        Ok("settings" | "controls") => Tab::Settings,
        Ok("log") => Tab::Log,
        _ => Tab::Play,
    }
}

/// What a control shows while the mouse rests on it and while a press holds it: a white veil over
/// its face, which shows on the dark window. Cranpose's clickables draw it, so every button gets it.
const INDICATION: Indication = Indication::overlay(Color(1.0, 1.0, 1.0, 0.12), Color(1.0, 1.0, 1.0, 0.3));

/// The window, with the look of its controls under the mouse.
#[composable]
pub fn CranvrApp() {
    CompositionLocalProvider([local_indication().provides(INDICATION)], Window);
}

/// Notes that the button of `request` asked for it now: the button shows that it runs.
fn note(model: MutableState<Model>, request: Request) {
    model.update(|m| m.pending = Some(Pending::new(request, m, Instant::now())));
}

/// The words of the button of `request`.
fn label_of(model: MutableState<Model>, request: &Request, own: &str) -> String {
    model.with(|m| pending::label(m.pending.as_ref(), m, request, own))
}

#[composable]
fn Window() {
    let settings = rememberMutableStateOf(Settings::load);
    let model = rememberMutableStateOf(Model::default);
    let tab = rememberMutableStateOf(|| start_tab());

    LaunchedEffect((), async move |_| {
        // The choice comes first: a Mac set to Wi-Fi must not get a link in the thread's first look
        usb::set_choice(connection_of(settings));
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
            start_host(settings, model);
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
            if let Some(problem) = model.with(|m| m.settings_problem.clone()) {
                Text(problem, Modifier::empty(), colored(240, 170, 90));
            }
            if let Some(message) = model.with(|m| m.message.clone()) {
                Text(message, Modifier::empty(), colored(240, 170, 90));
            }
            // A notice that only the console in VR shows has no text for the window
            if let Some((notice, _)) = model.with(|m| m.notice.clone()).filter(|(notice, _)| !notice.window.is_empty()) {
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
            Primary(label_of(model, &Request::StartHost, "Start"), move || {
                note(model, Request::StartHost);
                start_host(settings, model);
            });
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
        match model.with(|m| m.headset.clone()) {
            Some(h) => {
                Text(
                    format!(
                        "Headset connected: {}x{} per eye at {:.0} Hz{}",
                        h.eye.0,
                        h.eye.1,
                        h.fps,
                        h.foveation.map(|level| format!(", foveation {}", level.name())).unwrap_or_default()
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
                    Hint(format!("{:.0} fps, render {:.1} ms, encode {:.1} ms, {}{latency}", st.fps, st.render_ms, st.encode_ms, bitrate_text(&st)));
                }
                // A user whose controllers do not reach the hand menu opens it from here
                FlowRow(Modifier::empty().fill_max_width(), flow(), || {
                    Action("Open the menu in the headset".into(), backend::open_headset_menu);
                });
            }
            None => {
                Label("No headset connected".into());
                NativeConnect(settings, model);
            }
        }
        if let Some((false, why)) = model.with(|m| m.audio.clone()) {
            Text(format!("No game sound in the headset: {why}"), Modifier::empty(), colored(230, 180, 90));
        }
        for note in model.with(|m| m.network.clone()) {
            Text(note, Modifier::empty(), colored(230, 180, 90));
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
        Text("The headset is on the USB cable. It connects over the cable.".to_string(), Modifier::empty(), colored(110, 200, 140));
    } else if choice == Connection::Usb && !status.unauthorized {
        Text(
            if status.adb.is_none() && status.looked {
                "The USB cable needs a tool from Google. Press Add USB support in the Settings tab."
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
    // Steam cannot start before the setup. An outdated game support starts nothing: the library
    // shows the Update button alone then.
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
        // The games that the user added from a folder: each has Play and Options like a Steam game
        let added: Vec<steam::Game> = model.with(|m| m.games.iter().filter(|game| game.app_id.is_empty()).cloned().collect());
        for game in added {
            GameRow(settings, model, game);
        }
        FlowRow(Modifier::empty(), flow(), move || {
            Primary("Add a game from a folder".into(), move || {
                let steam_games = steam::steam_dir().join("steamapps/common");
                let start = if steam_games.exists() { steam_games } else { steam::prefix().join("drive_c") };
                choose_program("Choose the program of the game", start, true);
            });
            Action("Choose a program".into(), move || choose_program("Choose a Windows program", steam::prefix().join("drive_c"), false));
            Action(label_of(model, &Request::StopSteam, "Stop Steam and games"), move || {
                note(model, Request::StopSteam);
                steam::stop_all();
            });
        });
        Hint("Add a game: choose the program of a game that Steam does not list, such as a copy of a game with mods. cranvr reads the game's folder and fills in what the game needs. Options shows each value with its reason. Choose a program starts a program once.".into());
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
                LaunchOptions(settings, model, program);
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
    WineCard(settings, model);
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
    let (running, signed_in, games, outdated, installed) = model.with(|m| {
        // A game that the user added has no manifest: the card of other programs lists it
        let steam_games: Vec<steam::Game> = m.games.iter().filter(|game| !game.app_id.is_empty()).cloned().collect();
        (m.steam_running, m.steam_signed_in, steam_games, m.game_support_outdated, m.game_support.clone())
    });
    if outdated {
        GameSupportUpdate(installed, last);
        return;
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
            Primary(label_of(model, &Request::OpenSteam, "Open Steam"), move || {
                note(model, Request::OpenSteam);
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
        GameRow(settings, model, game);
    }
}

/// One game of a list: its row of the Play page (see pages.rs) with Play, Options and Stop. The
/// model holds the program whose launch options are open.
#[composable]
fn GameRow(settings: MutableState<Settings>, model: MutableState<Model>, game: steam::Game) {
    let Some(program) = steam::program(&game) else {
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            Text(game.name.clone(), Modifier::empty().padding(8.0), TextStyle::default());
            Hint("no program found in its folder".into());
        });
        return;
    };
    let shown = model.with(|m| m.open_options.as_ref() == Some(&program));
    let row = page_of(settings, model, pages::PLAY).and_then(|page| page.items.into_iter().find(|item| item.names("game.play", Some(&program)) || matches!(item, Item::Buttons(row) if row.title == game.name)));
    if let Some(row) = row {
        ItemView(row, settings, model);
    }
    if shown {
        LaunchOptions(settings, model, program);
    }
}

/// The launch options of one program, under its row: what cranvr adds when the program starts.
/// The model's page of the game gives the choices (see pages.rs), which the headset shows too. The
/// raw fields sit behind "Advanced", which starts open for a program that already has values in
/// them: the headset shows no text field.
#[composable]
fn LaunchOptions(settings: MutableState<Settings>, model: MutableState<Model>, program: String) {
    let options = settings.with(|s| s.launch.get(&program).cloned().unwrap_or_default());
    let raw_fields_set =
        !(options.dll_overrides.is_empty() && options.env.is_empty() && options.args.is_empty() && options.steam_app_id.is_empty());
    let advanced = rememberMutableStateOf(move || raw_fields_set);
    let dll = cranpose_core::remember(|| TextFieldState::new(&options.dll_overrides)).with(|state| *state);
    let steam_id = cranpose_core::remember(|| TextFieldState::new(&options.steam_app_id)).with(|state| *state);
    let env = cranpose_core::remember(|| TextFieldState::new(&options.env)).with(|state| *state);
    let args = cranpose_core::remember(|| TextFieldState::new(&options.args)).with(|state| *state);
    // The game's page without the buttons that its row in the list has already
    let items: Vec<Item> = settings
        .with(|s| model.with(|m| pages::options_page(s, m, &program)))
        .items
        .into_iter()
        .filter_map(|item| match item {
            Item::Buttons(mut row) => {
                row.buttons.retain(|button| !button.runs("game.play", Some(&program)) && !button.runs("steam.stop", None));
                (!row.buttons.is_empty()).then_some(Item::Buttons(row))
            }
            other => Some(other),
        })
        .collect();
    Column(
        Modifier::empty().fill_max_width().padding_symmetric(16.0, 0.0),
        ColumnSpec::default().vertical_arrangement(LinearArrangement::SpacedBy(6.0)),
        move || {
            PageItems(items.clone(), settings, model);
            let shown = advanced.get();
            FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                Choice("Advanced".into(), shown, move || advanced.set(!shown));
            });
            if shown {
                OptionField("Wine DLL overrides", dll);
                Hint("As WINEDLLOVERRIDES takes them, such as winhttp=n,b. The program then loads a winhttp.dll from its own folder before Wine's. Use it only for a mod loader that does not start without it: a loader that patched the game's files already starts twice with it.".into());
                OptionField("Steam app ID", steam_id);
                if !options.steam_app_id.is_empty() && !options.steam_app_id_reason.is_empty() {
                    Hint(options.steam_app_id_reason.clone());
                }
                Hint("The ID that the program's Steam check asks for. A copy of a Steam game needs the ID of the original. Empty: a program in a Steam library gets the ID of its folder's manifest, and any other program gets none. A program with an ID starts Steam first.".into());
                OptionField("Environment", env);
                Hint("NAME=value entries, separated by spaces.".into());
                OptionField("Arguments", args);
                let key = program.clone();
                FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                    let key = key.clone();
                    Primary("Save options".into(), move || {
                        edit_launch(settings, &key, |o| {
                            // A value that the user changed has no reason of cranvr's any more
                            let steam_text = steam_id.text().trim().to_owned();
                            if steam_text != o.steam_app_id {
                                o.steam_app_id_reason.clear();
                            }
                            o.dll_overrides = dll.text().trim().to_owned();
                            o.steam_app_id = steam_text;
                            o.env = env.text().trim().to_owned();
                            o.args = args.text().trim().to_owned();
                        })
                    });
                });
            }
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

/// The choice for every program: the OpenVR runtime of OpenVR games (see openvr.rs), from the model's
/// stream page. A program can set its own in its options. The Wine card and the Settings tab both
/// show it.
#[composable]
fn OpenVrSetting(settings: MutableState<Settings>, model: MutableState<Model>) {
    if let Some(item) = page_of(settings, model, pages::STREAM).and_then(|page| page.find("wine.openvr", None).cloned()) {
        ItemView(item, settings, model);
    }
}

/// The Wine that runs the games and the options of every program that cranvr starts. The raw
/// fields sit behind "Advanced", which starts open when they hold values.
#[composable]
fn WineCard(settings: MutableState<Settings>, model: MutableState<Model>) {
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
        OpenVrSetting(settings, model);
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

/// The two pages of the Settings tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsPage {
    General,
    Controls,
}

#[composable]
fn SettingsTab(settings: MutableState<Settings>, model: MutableState<Model>) {
    let page = rememberMutableStateOf(|| {
        if std::env::var("CRANVR_TAB").is_ok_and(|tab| tab == "controls") {
            SettingsPage::Controls
        } else {
            SettingsPage::General
        }
    });
    let current = page.get();
    FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
        for (choice, name) in [(SettingsPage::General, "General"), (SettingsPage::Controls, "Controls")] {
            Choice(name.into(), current == choice, move || page.set(choice));
        }
    });
    match current {
        SettingsPage::General => GeneralSettings(settings, model),
        SettingsPage::Controls => ControlsPage(settings),
    }
}

/// Changes one button of one controller. The headset gets the new mapping through `vr-menu.json`.
/// The host keeps running: the mapping is the headset's business.
fn set_control(settings: MutableState<Settings>, hand: Hand, control: Control, action: ControlAction) {
    settings.update(|s| {
        s.controls.set(hand, control, action);
    });
    settings.with(Settings::save);
}

/// Changes when the headset draws its models of the controllers in a game. The headset gets the
/// choice with the mapping, through `vr-menu.json`.
fn set_models(settings: MutableState<Settings>, choice: ModelsInGame) {
    settings.update(|s| s.controls.models_in_game = choice);
    settings.with(Settings::save);
}

/// Changes the hand that carries the hand menu while the headset tracks both controllers. The headset
/// gets the choice with the mapping, through `vr-menu.json`.
fn set_menu_hand(settings: MutableState<Settings>, choice: MenuHand) {
    settings.update(|s| s.controls.menu_hand = choice);
    settings.with(Settings::save);
}

fn reset_controls(settings: MutableState<Settings>) {
    settings.update(|s| s.controls = Controls::default());
    settings.with(Settings::save);
}

/// What each button of each controller does in the headset. A row lists the actions that the
/// button can have and marks the current one. The headset's console has the same page.
#[composable]
fn ControlsPage(settings: MutableState<Settings>) {
    let controls = settings.with(|s| s.controls);
    Card(move || {
        Heading("Controls".into());
        Hint("Each button of each controller does one thing in the headset's own panels. A game that runs still gets every button. The Meta button belongs to the headset's system and is not listed.".into());
        for hand in Hand::BOTH {
            Heading(hand.label().to_string());
            for control in Control::ALL.into_iter().filter(|control| control.exists(hand)) {
                let current = controls.action(hand, control);
                FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
                    Text(control.label(hand).to_string(), Modifier::empty().width(230.0).padding(8.0), TextStyle::default());
                    for action in control.offers() {
                        let action = *action;
                        Choice(action.label().into(), current == action, move || set_control(settings, hand, control, action));
                    }
                });
            }
        }
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            Action("Use the standard controls".into(), move || reset_controls(settings));
        });
    });
    let menu_hand = controls.menu_hand;
    Card(move || {
        Heading("Hand menu".into());
        Hint("The hand menu shows and hides every panel and opens the console. A button opens it, as the controls above say, or a press on the ball, or the button \"Open the menu in the headset\" at the top of this window while a headset is connected. With both controllers the ball and the menu ride on one hand, and the other hand points at them. Auto puts them on the hand that did not last press a panel. With one controller, or none, they stand in front of you, and that controller points at them. A controller that stops tracking for 2 seconds gives them up.".into());
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            for choice in MenuHand::ALL {
                Choice(choice.label().into(), menu_hand == choice, move || set_menu_hand(settings, choice));
            }
        });
    });
    let models = controls.models_in_game;
    Card(move || {
        Heading("Controller models during a game".into());
        Hint("The headset draws a model of each controller. A game draws its own controllers, so a second model can stand at the same place. This choice decides when the headset draws its models while a game runs. The console in the headset has the same choice.".into());
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            for choice in ModelsInGame::ALL {
                Choice(choice.label().into(), models == choice, move || set_models(settings, choice));
            }
        });
    });
    Card(move || {
        Heading("What the actions do".into());
        for action in ControlAction::ALL {
            Hint(format!("{}: {}", action.label(), action.describe()));
        }
        Hint("A mouse action works on the Mac's desktop panel, where the controller points. The headset's own panels take the trigger as their click, whatever it is set to here. Open the menu, Show or hide panels, Open the console, Recenter and Keyboard work wherever the controller points. While the headset streams they need a hold of 0.8 seconds, so a short press still belongs to the game.".into());
        Hint("The Mac has one mouse. The controller that holds a button keeps it, and the controller that presses next takes it. A change reaches the headset within a second.".into());
    });
}

#[composable]
fn GeneralSettings(settings: MutableState<Settings>, model: MutableState<Model>) {
    // The way of the stream comes first: the cable is the choice that a user looks for
    ConnectionCard(settings, model);
    // The stream's settings are the model's (see pages.rs): the headset's console shows the same page
    let stream = page_of(settings, model, pages::STREAM).map(|page| page.items).unwrap_or_default();
    Card(move || {
        PageItems(stream.clone(), settings, model);
        Hint("A change of the refresh rate, the bitrate, the foveation, the game size, the sound, the panel pixels or the hand controllers restarts the host; the headset reconnects by itself.".into());
    });
    MicrophoneCard(settings, model);
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
}

/// What games under Wine hear from the device "cranvr microphone": the headset's microphone, an input
/// device of this Mac, or nothing. The choice needs no restart of the host: a game's stream reads it
/// when it opens.
#[composable]
fn MicrophoneCard(settings: MutableState<Settings>, model: MutableState<Model>) {
    use crate::microphone::MicrophoneSource;
    let current = settings.with(|s| s.microphone.clone());
    let devices = model.with(|m| m.input_devices.clone());
    let choose = move |source: MicrophoneSource| {
        settings.update(|s| s.microphone = source);
        settings.with(Settings::save);
    };
    Card(move || {
        Heading("Microphone in games".into());
        let (shown, listed) = (current.clone(), devices.clone());
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            let current = shown.clone();
            Choice("Headset".into(), current == MicrophoneSource::Headset, move || choose(MicrophoneSource::Headset));
            for device in listed.clone() {
                let chosen = matches!(&current, MicrophoneSource::Mac { uid, .. } if *uid == device.uid);
                let source = MicrophoneSource::Mac { uid: device.uid.clone(), name: device.name.clone() };
                Choice(device.name.clone(), chosen, move || choose(source.clone()));
            }
            Choice("Off".into(), current == MicrophoneSource::Off, move || choose(MicrophoneSource::Off));
        });
        if let MicrophoneSource::Mac { uid, name } = &current {
            if !devices.iter().any(|device| device.uid == *uid) {
                Hint(format!("{name} is not connected. Games hear silence until it is."));
            }
        }
        Hint("Games under Wine record from the device \"cranvr microphone\". It takes the sound of this choice. The headset records only while a game does. A game hears a change when it opens its microphone again. With a Mac input device, macOS asks once whether cranvr may use the microphone.".into());
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
                    // The host keeps running and a stream keeps its path. The adb thread and the
                    // host's next connection follow the choice.
                    settings.update(|s| s.connection = option);
                    settings.with(Settings::save);
                    backend::tell_host_connection(option);
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
            Label("The USB cable needs a tool from Google that this Mac does not have.".into());
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
            Hint(format!("The button downloads Google's Android platform-tools for macOS ({}) from dl.google.com into {}.", usb::TOOLS_SIZE, backend::config_dir().display()));
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

/// What the eye stream sends against its bitrate. The link may carry less than the user set: the host
/// then names its limit.
fn bitrate_text(stats: &backend::Stats) -> String {
    if stats.mbps_set > stats.mbps {
        format!("{:.0} Mbps sent, at most {} Mbps on this link", stats.sent_mbps, stats.mbps)
    } else {
        format!("{:.0} Mbps sent of {}", stats.sent_mbps, stats.mbps)
    }
}

#[cfg(test)]
mod bitrate_tests {
    use super::*;

    #[test]
    fn the_hint_names_what_the_eye_stream_sends_and_the_links_limit() {
        let stats = |mbps, mbps_set| backend::Stats { mbps, mbps_set, sent_mbps: 27.6, ..Default::default() };
        assert_eq!(bitrate_text(&stats(40, 100)), "28 Mbps sent, at most 40 Mbps on this link");
        assert_eq!(bitrate_text(&stats(50, 50)), "28 Mbps sent of 50");
    }
}

#[cfg(test)]
#[path = "hover_tests.rs"]
mod hover_tests;

#[cfg(test)]
#[path = "pages_render_tests.rs"]
mod pages_render_tests;
