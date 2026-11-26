use crate::{
    backend::{self, Model, Settings, StreamPath},
    pages,
    pending::{self, Pending, Request},
    run::{self, PERMISSIONS},
    launch::GameOptions,
    setup::Setup,
    steam::{self, SteamState},
    system,
    usb::{self, Connection},
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
                // The headset's own page: its panels, its displays and its actions, as it sends them
                if let Some(page) = model.with(|m| m.headset_page.clone()) {
                    PageItems(page.items, settings, model);
                }
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
    let (state, setup) = model.with(|m| (m.steam, m.setup.clone()));
    let open_program = rememberMutableStateOf(|| false);
    let running = setup.as_ref().is_some_and(Setup::running);
    // Steam's state, the setup and its buttons are the model's (see pages.rs): the headset's console
    // shows the same page
    let steam_page = page_of(settings, model, pages::STEAM).map(|page| page.items).unwrap_or_default();
    // The Steam games show while Steam can start them. An outdated game support starts nothing.
    let library = model.with(|m| m.rosetta && m.steam == SteamState::Ready && !m.game_support_outdated) && !running;
    Card(move || {
        PageItems(steam_page.clone(), settings, model);
        if library {
            SteamGames(settings, model);
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
        // The programs that the Mac found, which the headset adds from in place of a file chooser
        let found = page_of(settings, model, pages::PROGRAMS).map(|page| page.items).unwrap_or_default();
        PageItems(found, settings, model);
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

/// The games of the Steam libraries, each with its row of the Play page. A game that the user added
/// stands in the card of other programs.
#[composable]
fn SteamGames(settings: MutableState<Settings>, model: MutableState<Model>) {
    let (signed_in, games) = model.with(|m| (m.steam_signed_in, m.games.iter().filter(|game| !game.app_id.is_empty()).cloned().collect::<Vec<_>>()));
    if games.is_empty() {
        Hint(if signed_in { "No games installed yet. Install them in the Steam window; they show up here.".into() } else { "Your installed games show up here.".into() });
        return;
    }
    Heading("Games".into());
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
    let raw_fields_set = !(options.dll_overrides.is_empty()
        && options.env.is_empty()
        && options.args.is_empty()
        && options.steam_app_id.is_empty()
        && options.vulkan_args.is_empty()
        && options.direct3d_args.is_empty());
    let advanced = rememberMutableStateOf(move || raw_fields_set);
    let dll = cranpose_core::remember(|| TextFieldState::new(&options.dll_overrides)).with(|state| *state);
    let steam_id = cranpose_core::remember(|| TextFieldState::new(&options.steam_app_id)).with(|state| *state);
    let env = cranpose_core::remember(|| TextFieldState::new(&options.env)).with(|state| *state);
    let args = cranpose_core::remember(|| TextFieldState::new(&options.args)).with(|state| *state);
    let vulkan_args = cranpose_core::remember(|| TextFieldState::new(&options.vulkan_args)).with(|state| *state);
    let direct3d_args = cranpose_core::remember(|| TextFieldState::new(&options.direct3d_args)).with(|state| *state);
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
                OptionField("Vulkan arguments", vulkan_args);
                OptionField("Direct3D 11 arguments", direct3d_args);
                Hint("What the Graphics choice adds for this game, such as +gfx_strAPI VLK. Empty: the argument of the game's engine, when cranvr knows the engine.".into());
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
                            o.vulkan_args = vulkan_args.text().trim().to_owned();
                            o.direct3d_args = direct3d_args.text().trim().to_owned();
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

/// The Wine that runs the games and the options of every program that cranvr starts: the model's
/// page (see pages.rs), which the headset shows too, and the raw fields behind "Advanced", which
/// starts open when they hold values. The headset shows no text field.
#[composable]
fn WineCard(settings: MutableState<Settings>, model: MutableState<Model>) {
    let wine = settings.with(|s| s.wine.clone());
    let raw_fields_set = !(wine.dll_overrides.is_empty() && wine.env.is_empty());
    let advanced = rememberMutableStateOf(move || raw_fields_set);
    let dll = cranpose_core::remember(|| TextFieldState::new(&wine.dll_overrides)).with(|state| *state);
    let env = cranpose_core::remember(|| TextFieldState::new(&wine.env)).with(|state| *state);
    // The page's lines of the raw fields stand in the fields here
    let items: Vec<Item> = page_of(settings, model, pages::WINE)
        .map(|page| page.items)
        .unwrap_or_default()
        .into_iter()
        .filter(|item| !matches!(item, Item::Text { text, .. } if text.contains("for every program:")))
        .collect();
    Card(move || {
        PageItems(items.clone(), settings, model);
        Hint("The game support step installs this Wine with DXMT and cranvr's OpenXR runtime into this prefix. Another Wine or prefix lacks them, so cranvr uses these.".into());
        let shown = advanced.get();
        FlowRow(Modifier::empty().fill_max_width(), flow(), move || {
            Choice("Advanced".into(), shown, move || advanced.set(!shown));
        });
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

/// The Mac's desktop in the headset and the permissions that it needs: the model's page (see
/// pages.rs), which the headset shows too.
#[composable]
fn DesktopTab(settings: MutableState<Settings>, model: MutableState<Model>) {
    let items = page_of(settings, model, pages::DESKTOP).map(|page| page.items).unwrap_or_default();
    Card(move || {
        PageItems(items.clone(), settings, model);
        if !system::in_app_bundle() {
            Hint("Start cranvr from /Applications/cranvr.app: macOS then gives these permissions to cranvr. From a terminal they go to the terminal app.".into());
        }
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

/// The connection, the stream, the sound, the headset's room and volume, and the Mac: the model's
/// pages (see pages.rs), which the headset shows too.
#[composable]
fn GeneralSettings(settings: MutableState<Settings>, model: MutableState<Model>) {
    // The way of the stream comes first: the cable is the choice that a user looks for
    for id in [pages::CONNECTION, pages::STREAM, pages::SOUND, pages::HEADSET, pages::MAC] {
        let items = page_of(settings, model, id).map(|page| page.items).unwrap_or_default();
        Card(move || {
            PageItems(items.clone(), settings, model);
        });
    }
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

/// What the eye stream sends against its bitrate. The host follows the link, and it sends the least
/// while the headset shows no stream: while the target lies under the user's setting, the text names
/// both.
fn bitrate_text(stats: &backend::Stats) -> String {
    if stats.mbps_set > stats.mbps {
        format!("{:.0} Mbps sent of {} now, {} set", stats.sent_mbps, stats.mbps, stats.mbps_set)
    } else {
        format!("{:.0} Mbps sent of {}", stats.sent_mbps, stats.mbps)
    }
}

#[cfg(test)]
mod bitrate_tests {
    use super::*;

    #[test]
    fn the_hint_names_what_the_eye_stream_sends_and_the_target_for_the_link() {
        let stats = |mbps, mbps_set| backend::Stats { mbps, mbps_set, sent_mbps: 27.6, ..Default::default() };
        assert_eq!(bitrate_text(&stats(40, 100)), "28 Mbps sent of 40 now, 100 set");
        assert_eq!(bitrate_text(&stats(50, 50)), "28 Mbps sent of 50");
    }
}

#[cfg(test)]
#[path = "hover_tests.rs"]
mod hover_tests;

#[cfg(test)]
#[path = "pages_render_tests.rs"]
mod pages_render_tests;
