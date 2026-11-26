//! The pages of settings and actions that both apps show (see `cranvr_common::menu`), and the one
//! function that applies a press of either app.
//!
//! The window draws these pages with `ui::PageItems`. The headset's console draws them from
//! `vr-menu.json`. Every setting stands once in a table here: its id, its words, its options, how it
//! reads from `Settings` and how a request changes it. A request names a setting or an action by its
//! id. [`apply`] finds it here, checks the value against the setting's options and changes the
//! settings. It returns what the caller must do besides (`Outcome`): save, restart the host, start a
//! game. Nothing here holds Cranpose state, so the tests run it directly.

use crate::{
    backend::{Model, Settings, StreamPath},
    launch::{self, GameOptions, OpenVr},
    microphone::MicrophoneSource,
    pending,
    setup::{self, Progress, Setup},
    steam::{self, Game, SteamState},
    system::Permission,
    usb::{Connection, Download},
};
use cranvr_common::{
    foveation::FoveationLevel,
    menu::{Button, Buttons, Choice, ChoiceShape, Item, Opt, Page, Request, Style, Toggle, Tone, OFF, ON},
};
use std::time::Instant;

/// The id of the page of games, which the console shows first
pub const PLAY: &str = "play";
pub const STEAM: &str = "steam";
pub const STREAM: &str = "stream";
pub const CONNECTION: &str = "connection";
pub const DESKTOP: &str = "desktop";
pub const SOUND: &str = "sound";
pub const WINE: &str = "wine";
pub const MAC: &str = "mac";
pub const HEADSET: &str = "headset";
pub const PROGRAMS: &str = "programs";
pub const LOG: &str = "log";
/// The most programs that the page of found programs lists
pub const FOUND_PROGRAMS: usize = 24;
/// How long the log page carries the newest lines after the headset asked for them
pub const LOG_TIME: std::time::Duration = std::time::Duration::from_secs(60);
/// The lines that the log page carries
const LOG_LINES: usize = 14;
/// The id of a game's page: this prefix and the game's program
pub const GAME_PAGE: &str = "game:";

/// The id of the page of a game's options.
pub fn game_page(program: &str) -> String {
    format!("{GAME_PAGE}{program}")
}

/// A setting of the app, as it stands in the pages.
struct Setting {
    id: &'static str,
    label: &'static str,
    /// A change restarts the host
    restarts: bool,
    hint: fn(&Settings) -> &'static str,
    kind: Kind,
}

enum Kind {
    /// The options can depend on the Mac, such as its input devices
    Choice { options: fn(&Settings, &Model) -> Vec<Opt>, get: fn(&Settings) -> String, set: fn(&mut Settings, &Model, &str) },
    Toggle { get: fn(&Settings) -> bool, set: fn(&mut Settings, bool) },
}

impl Setting {
    fn item(&self, settings: &Settings, model: &Model) -> Item {
        let hint = (self.hint)(settings).to_string();
        match &self.kind {
            Kind::Choice { options, get, .. } => {
                let shape = shape_of(self.id);
                Item::Choice(Choice { id: self.id.into(), subject: None, label: self.label.into(), options: options(settings, model), value: get(settings), hint, restarts: self.restarts, shape })
            }
            Kind::Toggle { get, .. } => Item::Toggle(Toggle { id: self.id.into(), subject: None, label: self.label.into(), on: get(settings), hint, restarts: self.restarts }),
        }
    }

    /// Sets the value. False for a value that the setting does not offer.
    fn set(&self, settings: &mut Settings, model: &Model, value: &str) -> bool {
        match &self.kind {
            Kind::Choice { options, set, .. } => {
                if !options(settings, model).iter().any(|option| option.value == value) {
                    return false;
                }
                set(settings, model, value);
                true
            }
            Kind::Toggle { set, .. } => match value {
                ON | OFF => {
                    set(settings, value == ON);
                    true
                }
                _ => false,
            },
        }
    }
}

fn no_hint(_: &Settings) -> &'static str {
    ""
}

fn numbers<T: ToString>(values: &[T], unit: &str) -> Vec<Opt> {
    values.iter().map(|value| Opt::new(value.to_string(), format!("{}{unit}", value.to_string()))).collect()
}

const FPS_CHOICES: [u32; 3] = [72, 90, 120];
const MBPS_CHOICES: [u32; 4] = [50, 100, 150, 200];
const GAME_SCALE_CHOICES: [f32; 5] = [0.5, 0.6, 0.7, 0.85, 1.0];

/// A render size as a request names it: its percent, such as "85"
fn scale_value(scale: f32) -> String {
    format!("{:.0}", scale * 100.0)
}

fn foveation_hint(settings: &Settings) -> &'static str {
    match settings.foveation_level {
        FoveationLevel::Off => "Every pixel at the same detail. At 120 Hz the eye is smaller, so the center is less sharp.",
        FoveationLevel::Low => "The center is sharp and the edges lose a little detail. Each frame takes about 2 ms longer to encode and decode than at Medium.",
        FoveationLevel::Medium => "The center is sharp and the edges lose detail. This is the standard level.",
        FoveationLevel::High => "The center gets the most bits and the edges lose the most detail. The sharp region around straight ahead is smaller.",
    }
}

/// The objects of a setting's options in the headset's 3D console (#111): each shows by its form
/// what its options mean. The other settings draw keys.
fn shape_of(id: &str) -> ChoiceShape {
    match id {
        "fps" => ChoiceShape::Arc,
        "mbps" => ChoiceShape::Gems,
        "foveation_level" => ChoiceShape::Petals,
        "game_scale" => ChoiceShape::Cubes,
        "game_msaa_limit" => ChoiceShape::Stepper,
        _ => ChoiceShape::Keys,
    }
}

/// The settings of the stream page, in the order of the page. Each one restarts the host but the
/// anti-aliasing limit, which a game reads when it starts, and the host's start.
const STREAM_SETTINGS: &[Setting] = &[
    Setting {
        id: "fps",
        label: "Refresh rate",
        restarts: true,
        hint: no_hint,
        kind: Kind::Choice {
            options: |_, _| numbers(&FPS_CHOICES, " Hz"),
            get: |s| s.fps.to_string(),
            set: |s, _, v| s.fps = v.parse().unwrap_or(s.fps),
        },
    },
    Setting {
        id: "mbps",
        label: "Bitrate",
        restarts: true,
        hint: no_hint,
        kind: Kind::Choice {
            options: |_, _| numbers(&MBPS_CHOICES, " Mbps"),
            get: |s| s.mbps.to_string(),
            set: |s, _, v| s.mbps = v.parse().unwrap_or(s.mbps),
        },
    },
    Setting {
        id: "foveation_level",
        label: "Foveated encoding",
        restarts: true,
        hint: foveation_hint,
        kind: Kind::Choice {
            options: |_, _| FoveationLevel::ALL.iter().map(|level| Opt::new(level.name(), level_label(*level))).collect(),
            get: |s| s.foveation_level.name().to_string(),
            set: |s, _, v| s.foveation_level = FoveationLevel::parse(v).unwrap_or(s.foveation_level),
        },
    },
    Setting {
        id: "game_scale",
        label: "Game render size",
        restarts: true,
        hint: |_| "A game that makes fewer frames than the refresh rate adds 10 to 18 ms of latency. The Log tab then names the game's rate. A smaller size or lower anti-aliasing in the game helps.",
        kind: Kind::Choice {
            options: |_, _| GAME_SCALE_CHOICES.iter().map(|scale| Opt::new(scale_value(*scale), format!("{}%", scale_value(*scale)))).collect(),
            get: |s| GAME_SCALE_CHOICES.iter().find(|scale| (s.game_scale - **scale).abs() < 0.01).map_or_else(|| scale_value(s.game_scale), |scale| scale_value(*scale)),
            set: |s, _, v| s.game_scale = v.parse::<f32>().map_or(s.game_scale, |percent| percent / 100.0),
        },
    },
    Setting {
        id: "game_msaa_limit",
        label: "Game anti-aliasing",
        restarts: false,
        hint: |_| "Anti-aliasing costs a Windows game far more on the Mac: Beat Saber made 91 frames a second at 4x and 119 at 2x. With the limit, cranvr tells each game that more than 2x does not exist. It applies when a game starts. A game started from the Steam window gets it after a restart of Steam.",
        kind: Kind::Choice {
            options: |_, _| vec![Opt::new("2", "At most 2x"), Opt::new("0", "As the game sets it")],
            get: |s| s.game_msaa_limit.to_string(),
            set: |s, _, v| s.game_msaa_limit = v.parse().unwrap_or(s.game_msaa_limit),
        },
    },
    Setting {
        id: "unscale_displays",
        label: "Panel pixels while a game runs",
        restarts: true,
        hint: |_| "A display in a scaled mode, such as 3008 x 1692 on a 4K TV, makes macOS draw every change at 6016 x 3384 pixels. While a game runs, cranvr gives such a display its panel's own pixels with at least as much room, so windows stay where they are. The mode comes back when the game ends. In a test the Mac's desktop then took about a third of the GPU time.",
        kind: Kind::Toggle { get: |s| s.unscale_displays, set: |s, on| s.unscale_displays = on },
    },
    Setting {
        id: "hand_controllers",
        label: "Controllers from your hands",
        restarts: true,
        hint: |_| "A game that has no hand tracking gets controllers made of your hands, while the headset tracks no controller. A pinch of thumb and index finger pulls the trigger. A fist presses the grip button. A game that tracks hands itself gets your hands and no controllers.",
        kind: Kind::Toggle { get: |s| s.hand_controllers, set: |s, on| s.hand_controllers = on },
    },
    Setting {
        id: "autostart",
        label: "Start the host when cranvr opens",
        restarts: false,
        hint: no_hint,
        kind: Kind::Toggle { get: |s| s.autostart, set: |s, on| s.autostart = on },
    },
];

/// The Connection choice: it reaches a running host at once and counts from the headset's next
/// connection, so the host keeps running.
const CONNECTION_SETTINGS: &[Setting] = &[Setting {
    id: "connection",
    label: "Stream by",
    restarts: false,
    hint: |s| match s.connection {
        Connection::Automatic => "The stream uses the USB cable while the headset is plugged into this Mac. Otherwise it uses Wi-Fi. A change counts from the next connection of the headset.",
        Connection::WiFi => "The stream always uses Wi-Fi. This Mac gives the headset no link over the cable. A change counts from the next connection of the headset.",
        Connection::Usb => "The stream uses the USB cable. The window says when the headset is not on it. A change counts from the next connection of the headset.",
    },
    kind: Kind::Choice {
        options: |_, _| vec![Opt::new("automatic", "Automatic"), Opt::new("wifi", "Wi-Fi"), Opt::new("usb", "USB cable")],
        get: |s| connection_key(s.connection).into(),
        set: |s, _, v| {
            s.connection = match v {
                "wifi" => Connection::WiFi,
                "usb" => Connection::Usb,
                _ => Connection::Automatic,
            }
        },
    },
}];

fn connection_key(connection: Connection) -> &'static str {
    match connection {
        Connection::Automatic => "automatic",
        Connection::WiFi => "wifi",
        Connection::Usb => "usb",
    }
}

/// A choice of the display for the desktop view: its value in a request, its words and the size of
/// the display that the host adds, or none for the main display
type VirtualDisplay = (&'static str, &'static str, Option<(u32, u32)>);

/// The sizes of the display that the host adds for the desktop view
const VIRTUAL_DISPLAYS: [VirtualDisplay; 3] =
    [("main", "The main display", None), ("1920x1080", "Add a display 1920 x 1080", Some((1920, 1080))), ("2560x1440", "Add a display 2560 x 1440", Some((2560, 1440)))];

/// The desktop view. Each one restarts the host but the window's move into VR, which the window does.
const DESKTOP_SETTINGS: &[Setting] = &[
    Setting {
        id: "desktop",
        label: "Desktop view",
        restarts: true,
        hint: |_| "A screen in front of you shows the Mac while no game runs.",
        kind: Kind::Toggle { get: |s| s.desktop, set: |s, on| s.desktop = on },
    },
    Setting {
        id: "virtual_display",
        label: "Screen in VR",
        restarts: true,
        hint: |_| "The headset shows your main display as it is. An added display is one more monitor that only the headset shows: drag windows onto it.",
        kind: Kind::Choice {
            options: |_, _| VIRTUAL_DISPLAYS.iter().map(|(value, label, _)| Opt::new(*value, *label)).collect(),
            get: |s| VIRTUAL_DISPLAYS.iter().find(|(_, _, size)| *size == s.virtual_display).map_or("main", |(value, _, _)| value).into(),
            set: |s, _, v| s.virtual_display = VIRTUAL_DISPLAYS.iter().find(|(value, _, _)| *value == v).and_then(|(_, _, size)| *size),
        },
    },
    Setting {
        id: "vr_main_display",
        label: "Main display while the headset is on",
        restarts: true,
        hint: |_| "As the main display it gets the menu bar, the Dock and new windows, Steam's too. Your other displays go back when the headset leaves.",
        kind: Kind::Toggle { get: |s| s.vr_main_display, set: |s, on| s.vr_main_display = on },
    },
    Setting {
        id: "window_in_vr",
        label: "Bring the cranvr window into VR",
        restarts: false,
        hint: no_hint,
        kind: Kind::Toggle { get: |s| s.window_in_vr, set: |s, on| s.window_in_vr = on },
    },
    Setting {
        id: "desktop_fps",
        label: "Screen updates",
        restarts: true,
        hint: |_| "60 updates per second move the cursor smoother, but cost frames: each capture of a large display takes GPU time.",
        kind: Kind::Choice { options: |_, _| numbers(&[30, 60], " a second"), get: |s| s.desktop_fps.to_string(), set: |s, _, v| s.desktop_fps = v.parse().unwrap_or(s.desktop_fps) },
    },
];

/// The value of the microphone's choice in a request: "headset", "off" or "mac:" and a device's UID
fn microphone_key(source: &MicrophoneSource) -> String {
    match source {
        MicrophoneSource::Headset => "headset".into(),
        MicrophoneSource::Off => "off".into(),
        MicrophoneSource::Mac { uid, .. } => format!("mac:{uid}"),
    }
}

/// The game sound and the microphone. The microphone needs no restart: a game's stream reads the
/// choice when it opens.
const SOUND_SETTINGS: &[Setting] = &[
    Setting {
        id: "game_audio",
        label: "Game sound in the headset",
        restarts: true,
        hint: |_| "The headset plays what this Mac plays. The capture takes the sound before the Mac's volume, so you can turn the Mac down. It needs Screen Recording.",
        kind: Kind::Toggle { get: |s| s.game_audio, set: |s, on| s.game_audio = on },
    },
    Setting {
        id: "microphone",
        label: "Microphone in games",
        restarts: false,
        hint: |_| "Games under Wine record from the device \"cranvr microphone\". It takes the sound of this choice. The headset records only while a game does. A game hears a change when it opens its microphone again. With a Mac input device, macOS asks once whether cranvr may use the microphone.",
        kind: Kind::Choice {
            options: |s, m| {
                let mut options = vec![Opt::new("headset", "Headset")];
                options.extend(m.input_devices.iter().map(|device| Opt::new(format!("mac:{}", device.uid), device.name.clone())));
                // A chosen device that is not connected stays a choice, so the setting shows it
                if let MicrophoneSource::Mac { uid, name } = &s.microphone {
                    if !m.input_devices.iter().any(|device| device.uid == *uid) {
                        options.push(Opt::new(format!("mac:{uid}"), format!("{name} (not connected)")));
                    }
                }
                options.push(Opt::new("off", "Off"));
                options
            },
            get: |s| microphone_key(&s.microphone),
            set: |s, m, v| {
                s.microphone = match v.strip_prefix("mac:") {
                    Some(uid) => {
                        let name = m.input_devices.iter().find(|device| device.uid == uid).map(|device| device.name.clone());
                        let kept = match &s.microphone {
                            MicrophoneSource::Mac { uid: own, name } if own == uid => Some(name.clone()),
                            _ => None,
                        };
                        MicrophoneSource::Mac { uid: uid.into(), name: name.or(kept).unwrap_or_else(|| uid.into()) }
                    }
                    None if v == "off" => MicrophoneSource::Off,
                    None => MicrophoneSource::Headset,
                }
            },
        },
    },
];

/// The options of every Windows program that cranvr starts. They count at a program's next start.
const WINE_SETTINGS: &[Setting] = &[
    Setting {
        id: "wine.mac_window",
        label: "Game window on the Mac shows the picture",
        restarts: false,
        hint: |_| "Off: a game's own window on the Mac stays empty and costs no GPU time. The headset shows the game either way.",
        kind: Kind::Toggle { get: |s| s.wine.mac_window, set: |s, on| s.wine.mac_window = on },
    },
    Setting {
        id: "wine.openvr",
        label: "OpenVR games",
        restarts: false,
        hint: |_| OPENVR_EVERY_HINT,
        kind: Kind::Choice {
            options: |_, _| OpenVr::ALL.iter().map(|choice| Opt::new(choice.key(), choice.label())).collect(),
            get: |s| s.wine.openvr.key().to_string(),
            set: |s, _, v| s.wine.openvr = OpenVr::from_key(v).unwrap_or(s.wine.openvr),
        },
    },
];

/// The headset's own settings. The headset applies them, the Mac keeps them. A value that the
/// headset has not told yet shows no option.
const HEADSET_SETTINGS: &[Setting] = &[
    Setting {
        id: "headset_room",
        label: "Room",
        restarts: false,
        hint: |_| "What the headset shows behind its panels while no game runs: your room through its cameras, or the host's scene.",
        kind: Kind::Choice {
            options: |_, _| vec![Opt::new("passthrough", "Passthrough"), Opt::new("scene", "Scene")],
            get: |s| s.headset_room.clone().unwrap_or_default(),
            set: |s, _, v| s.headset_room = Some(v.into()),
        },
    },
    Setting {
        id: "headset_volume",
        label: "Volume",
        restarts: false,
        hint: |_| "How loud the headset plays the sound of the Mac and of games, against the level that the host sends. A soft limiter keeps loud parts clean.",
        kind: Kind::Choice {
            options: |_, _| numbers(&[100, 150, 200, 300], "%"),
            get: |s| s.headset_volume.map(|v| v.to_string()).unwrap_or_default(),
            set: |s, _, v| s.headset_volume = v.parse().ok(),
        },
    },
    Setting {
        id: "headset_interface",
        label: "Interface",
        restarts: false,
        hint: |_| "How the headset draws its hand menu and its console: in 3D, where the hand menu and the Stream page float as glowing glass objects and the other pages stand on trays with keys that sink, or as flat panels.",
        kind: Kind::Choice {
            options: |_, _| vec![Opt::new("3d", "3D"), Opt::new("flat", "Flat")],
            get: |s| s.headset_interface.clone().unwrap_or_default(),
            set: |s, _, v| s.headset_interface = Some(v.into()),
        },
    },
    Setting {
        id: "headset_theme",
        label: "Theme",
        restarts: false,
        hint: |_| "The colors of the 3D interface: cyan glass, or violet glass with gold for the chosen options.",
        kind: Kind::Choice {
            options: |_, _| vec![Opt::new("cyan", "Cyan"), Opt::new("amber", "Amber")],
            get: |s| s.headset_theme.clone().unwrap_or_default(),
            set: |s, _, v| s.headset_theme = Some(v.into()),
        },
    },
];

/// Every setting of the app, of every page.
fn settings_table() -> impl Iterator<Item = &'static Setting> {
    STREAM_SETTINGS.iter().chain(CONNECTION_SETTINGS).chain(DESKTOP_SETTINGS).chain(SOUND_SETTINGS).chain(WINE_SETTINGS).chain(HEADSET_SETTINGS)
}

fn setting(id: &str) -> Option<&'static Setting> {
    settings_table().find(|setting| setting.id == id)
}

fn level_label(level: FoveationLevel) -> &'static str {
    match level {
        FoveationLevel::Off => "Off",
        FoveationLevel::Low => "Low",
        FoveationLevel::Medium => "Medium",
        FoveationLevel::High => "High",
    }
}

/// What the OpenVR choice means, in a game's options and for every program
pub const OPENVR_HINT: &str = "OpenComposite runs an OpenVR game through cranvr's runtime without SteamVR. SteamVR runs the game in SteamVR itself, for a game that needs SteamVR. Install SteamVR from your Steam library first. SteamVR starts with the game and closes after it. Off leaves the game no OpenVR runtime, so a game with an OpenXR mode takes that. An OpenXR game ignores the choice. It applies when the game starts.";
const OPENVR_EVERY_HINT: &str = "OpenComposite runs an OpenVR game through cranvr's runtime without SteamVR. SteamVR runs the game in SteamVR itself, for a game that needs SteamVR. Install SteamVR from your Steam library first. SteamVR starts with the game and closes after it. Off leaves the game no OpenVR runtime, so a game with an OpenXR mode takes that. An OpenXR game ignores the choice. It applies when the game starts. Each game can choose otherwise in its options.";

/// A game's own option, as it stands on the game's page.
struct GameSetting {
    id: &'static str,
    label: &'static str,
    kind: GameKind,
}

enum GameKind {
    Choice { options: fn(&Settings) -> Vec<Opt>, get: fn(&GameOptions) -> String, set: fn(&mut GameOptions, &str) },
    Toggle { get: fn(&GameOptions) -> bool, set: fn(&mut GameOptions, bool) },
}

/// The value of a game's VR system that follows the choice for every program
const AS_SETTINGS: &str = "default";

const GAME_SETTINGS: &[GameSetting] = &[
    GameSetting {
        id: "game.openvr",
        label: "OpenVR",
        kind: GameKind::Choice {
            options: |s| {
                let mut options = vec![Opt::new(AS_SETTINGS, format!("As in Settings ({})", s.wine.openvr.label()))];
                options.extend(OpenVr::ALL.iter().map(|choice| Opt::new(choice.key(), choice.label())));
                options
            },
            get: |o| o.openvr.map_or(AS_SETTINGS, OpenVr::key).to_string(),
            // The choice is the user's now, so cranvr's reason goes
            set: |o, v| {
                o.openvr = OpenVr::from_key(v);
                o.openvr_reason.clear();
            },
        },
    },
    GameSetting {
        id: "game.eye_per_pass",
        label: "Each eye in its own pass",
        kind: GameKind::Toggle {
            get: |o| o.eye_per_pass,
            set: |o, on| {
                o.eye_per_pass = on;
                o.eye_per_pass_declined = !on;
                o.eye_per_pass_reason.clear();
                // The two options ask for opposite things
                if on {
                    o.eye_one_pass = false;
                    o.eye_one_pass_reason.clear();
                }
            },
        },
    },
    GameSetting {
        id: "game.eye_one_pass",
        label: "Both eyes in one pass",
        kind: GameKind::Toggle {
            get: |o| o.eye_one_pass,
            set: |o, on| {
                o.eye_one_pass = on;
                o.eye_one_pass_declined = !on;
                o.eye_one_pass_reason.clear();
                if on {
                    o.eye_per_pass = false;
                    o.eye_per_pass_reason.clear();
                }
            },
        },
    },
];

const EYE_HINT: &str = "For a game whose picture stays black, in the right eye or in both eyes. \"Each eye in its own pass\": the runtime refuses two-eye images, so the game draws each eye apart. This costs frames. \"Both eyes in one pass\": the runtime tells the game that a vertex shader may choose the eye, as a game that draws both eyes in one pass needs, or that draws both eyes into a picture of its own, as a Unity game with the OpenVR provider does.";

/// Whether the request of a button runs now, as the window's pending request says.
fn busy(model: &Model, request: &pending::Request) -> bool {
    model.pending.as_ref().is_some_and(|p| &p.request == request && p.progress(model, Instant::now()) == pending::Progress::Running)
}

/// Every page, in the order of the console's list. The first one is the console's main page.
pub fn pages(settings: &Settings, model: &Model) -> Vec<Page> {
    let mut pages = vec![
        play_page(settings, model),
        steam_page(model),
        stream_page(settings, model),
        connection_page(settings, model),
        desktop_page(settings, model),
        sound_page(settings, model),
        wine_page(settings, model),
        headset_page(settings, model),
        mac_page(model),
        programs_page(model),
        log_page(model),
    ];
    for game in &model.games {
        if let Some(page) = game_page_of(settings, model, game) {
            pages.push(page);
        }
    }
    pages
}

/// The games, each with Play, Options and Stop while it runs.
fn play_page(settings: &Settings, model: &Model) -> Page {
    let mut items = Vec::new();
    // A Mac that cannot run games yet says so first, with the way to the setup
    if let Some(words) = steam_trouble(model) {
        items.push(Item::Buttons(Buttons { title: words.into(), buttons: vec![Button::open("Steam", STEAM)], ..Default::default() }));
    }
    items.push(Item::Heading { text: "Games".into() });
    if model.games.is_empty() {
        items.push(Item::Text { text: "No games installed in cranvr's Steam".into(), tone: Tone::Muted });
    }
    for game in &model.games {
        let Some(program) = steam::program(game) else { continue };
        let runs = runs(model, &program);
        let vr = launch::openvr(&settings.wine, &settings.launch.get(&program).cloned().unwrap_or_default());
        let mut buttons = Vec::new();
        if game.exe.is_some() {
            let label = if runs { "Playing" } else { "Play" };
            buttons.push(Button::run(label, "game.play").of(program.clone()).styled(Style::Primary).running("Starting...", busy(model, &pending::Request::Play(program.clone()))));
        }
        buttons.push(Button::open("Options", game_page(&program)));
        if runs {
            buttons.push(Button::run("Stop", "steam.stop").styled(Style::Danger).running("Stopping...", busy(model, &pending::Request::StopSteam)));
        }
        items.push(Item::Buttons(Buttons { title: game.name.clone(), detail: vr.label().into(), buttons, hint: String::new() }));
    }
    items.push(Item::Buttons(Buttons {
        buttons: vec![Button::run("Stop Steam and games", "steam.stop").styled(Style::Danger).running("Stopping...", busy(model, &pending::Request::StopSteam))],
        ..Default::default()
    }));
    Page { id: PLAY.into(), title: "Play".into(), short: "Play".into(), listed: true, items }
}

/// What keeps this Mac from running games now, in a few words, or `None` when nothing does.
fn steam_trouble(model: &Model) -> Option<&'static str> {
    if model.setup.as_ref().is_some_and(Setup::running) {
        return Some("The setup of games runs");
    }
    if !model.rosetta || model.steam == SteamState::NoWine {
        return Some("This Mac needs a setup for games");
    }
    if model.game_support_outdated {
        return Some("The game support needs an update");
    }
    match model.steam {
        SteamState::NotInstalled => Some("Steam is not installed"),
        SteamState::NeedsSetup => Some("Steam needs a repair"),
        _ => None,
    }
}

/// Steam and the game support: their state, the setup's steps and the buttons that set up, update,
/// repair, open and stop. The Rosetta step and the Steam sign-in need the Mac: the page says so.
fn steam_page(model: &Model) -> Page {
    let mut items = vec![Item::Heading { text: "Game support".into() }];
    let newest = steam::game_support_expected();
    items.push(match (&model.game_support, model.game_support_outdated, model.steam) {
        (_, _, SteamState::NoWine) => Item::Text { text: "No game support yet: Wine, DXMT and cranvr's runtime are missing".into(), tone: Tone::Warn },
        (Some(version), false, _) => Item::Text { text: format!("Game support version {version}"), tone: Tone::Good },
        (Some(version), true, _) => Item::Text { text: format!("Game support version {version}. Version {newest} is available."), tone: Tone::Warn },
        (None, true, _) => Item::Text { text: format!("The game support is older than version {newest}."), tone: Tone::Warn },
        (None, false, _) => Item::Text { text: "Game support without a version stamp".into(), tone: Tone::Muted },
    });
    if !model.rosetta {
        items.push(Item::Text { text: "Rosetta 2 is missing. Wine needs it. Its install asks for the Mac's password on the Mac.".into(), tone: Tone::Warn });
    }
    items.push(Item::Heading { text: "Steam".into() });
    let (state, tone) = match model.steam {
        SteamState::NoWine => ("Steam waits for the game support", Tone::Muted),
        SteamState::NotInstalled => ("Steam for Windows is not installed", Tone::Warn),
        SteamState::NeedsSetup => ("Steam's web helper is missing or unknown, so Steam needs a repair", Tone::Warn),
        SteamState::Ready if model.steam_running => ("Steam is running", Tone::Good),
        SteamState::Ready => ("Steam is installed and ready", Tone::Good),
    };
    items.push(Item::Text { text: state.into(), tone });
    if model.steam == SteamState::Ready && !model.steam_signed_in {
        items.push(Item::Text { text: "Sign in to Steam once, in the Steam window on the Mac. Steam remembers you after that.".into(), tone: Tone::Warn });
    }
    // The steps of the setup: what each does before it runs, its progress while it runs
    let setup = model.setup.clone().filter(|setup| setup.running() || setup.failed());
    let plan = setup::plan(model.rosetta, model.steam, model.game_support_outdated);
    if setup.is_some() || !plan.is_empty() {
        items.push(Item::Heading { text: "Setup".into() });
    }
    match &setup {
        Some(setup) => {
            for (step, progress) in &setup.steps {
                let (words, tone) = match progress {
                    Progress::Waiting => ("Waiting".to_string(), Tone::Muted),
                    Progress::Working(text) => (text.clone(), Tone::Normal),
                    Progress::Done => ("Done".to_string(), Tone::Good),
                    Progress::Failed(message) => (format!("Failed: {message}"), Tone::Warn),
                };
                items.push(Item::Text { text: format!("{}: {words}", step.title()), tone });
            }
        }
        None => {
            for step in &plan {
                items.push(Item::Text { text: format!("{}: {}", step.title(), step.describe(model.steam)), tone: Tone::Muted });
            }
        }
    }
    let running = setup.as_ref().is_some_and(Setup::running);
    let failed = setup.as_ref().is_some_and(Setup::failed);
    if running || !plan.is_empty() {
        let label = if failed {
            "Retry"
        } else if model.steam == SteamState::NoWine || !model.rosetta {
            "Set up games"
        } else if model.game_support_outdated {
            "Update game support"
        } else if model.steam == SteamState::NeedsSetup {
            "Repair Steam"
        } else {
            "Install Steam"
        };
        let hint = if plan.contains(&setup::Step::Steam) {
            "Keep cranvr open. This takes a few minutes. Steam opens at the end: sign in there once."
        } else {
            "Keep cranvr open. Steam and running games stop while this runs. Your games and your Steam login stay."
        };
        items.push(Item::Buttons(Buttons { buttons: vec![Button::run(label, "setup.start").styled(Style::Primary).running("Setting up...", running)], hint: hint.into(), ..Default::default() }));
    }
    if model.steam == SteamState::Ready && !running {
        let mut buttons = Vec::new();
        if !model.steam_running {
            buttons.push(Button::run("Open Steam", "steam.open").styled(Style::Primary).running("Opening Steam...", busy(model, &pending::Request::OpenSteam)));
        }
        buttons.push(Button::run("Stop Steam and games", "steam.stop").styled(Style::Danger).running("Stopping...", busy(model, &pending::Request::StopSteam)));
        let hint = "Games on another disk: in the Steam window, open Settings, then Storage, and add a folder on that disk (it sits under Z:\\Volumes). The games installed there show up in the list.";
        items.push(Item::Buttons(Buttons { buttons, hint: hint.into(), ..Default::default() }));
    }
    Page { id: STEAM.into(), title: "Steam and game support".into(), short: "Steam".into(), listed: true, items }
}

/// Whether the game of `program` runs: the host names its program.
fn runs(model: &Model, program: &str) -> bool {
    model.game_program.as_deref().is_some_and(|running| pending::same_program(running, program))
}

fn stream_page(settings: &Settings, model: &Model) -> Page {
    let mut items = vec![Item::Heading { text: "Stream".into() }];
    items.extend(STREAM_SETTINGS.iter().map(|setting| setting.item(settings, model)));
    // The sizes that the host learned for games that stayed below the refresh rate
    let sizes = model.learned_sizes.iter().map(|(name, share)| format!("{name} {:.0}%", share * 100.0)).collect::<Vec<_>>();
    items.push(Item::Buttons(Buttons {
        title: if sizes.is_empty() { "No learned game sizes".into() } else { format!("Learned: {}", sizes.join(", ")) },
        detail: String::new(),
        buttons: if sizes.is_empty() { Vec::new() } else { vec![Button::run("Forget learned sizes", "sizes.forget")] },
        hint: "A game that stays below the refresh rate for a minute renders smaller at its next start, as a share of the size above. A game reads its size only when it starts.".into(),
    }));
    Page { id: STREAM.into(), title: "Stream".into(), short: "Stream".into(), listed: true, items }
}

/// How the stream reaches the headset: the choice, the path that it takes now, the cable's state and
/// the download of the tool that the cable needs.
fn connection_page(settings: &Settings, model: &Model) -> Page {
    let mut items = vec![Item::Heading { text: "Connection".into() }];
    items.extend(CONNECTION_SETTINGS.iter().map(|setting| setting.item(settings, model)));
    if let Some(path) = model.headset.as_ref().and_then(|headset| headset.path) {
        let words = match path {
            StreamPath::Usb => "The headset is connected by USB cable",
            StreamPath::WiFi => "The headset is connected over Wi-Fi",
        };
        items.push(Item::Text { text: words.into(), tone: Tone::Good });
        if settings.connection == Connection::Usb && path == StreamPath::WiFi {
            items.push(Item::Text { text: "The USB cable is chosen, but this headset is not on it. Plug it into this Mac.".into(), tone: Tone::Warn });
        }
    }
    let usb = &model.usb;
    if settings.connection.allows_usb() {
        if let Some(blocked) = &usb.blocked {
            items.push(Item::Text { text: blocked.clone(), tone: Tone::Warn });
        }
        if usb.unauthorized {
            items.push(Item::Text { text: "Put on the headset and allow USB debugging.".into(), tone: Tone::Warn });
        }
        if usb.adb.is_none() && usb.looked {
            items.push(Item::Text { text: "The USB cable needs a tool from Google that this Mac does not have.".into(), tone: Tone::Warn });
            let hint = format!("The button downloads Google's Android platform-tools for macOS ({}) from dl.google.com into {}.", crate::usb::TOOLS_SIZE, crate::backend::config_dir().display());
            match &usb.download {
                Download::Working(text) => items.push(Item::Text { text: text.clone(), tone: Tone::Normal }),
                Download::Failed(text) => {
                    items.push(Item::Text { text: text.clone(), tone: Tone::Warn });
                    items.push(Item::Buttons(Buttons { buttons: vec![Button::run("Try again", "usb.download").styled(Style::Primary)], hint, ..Default::default() }));
                }
                Download::Idle => items.push(Item::Buttons(Buttons { buttons: vec![Button::run("Add USB support", "usb.download").styled(Style::Primary)], hint, ..Default::default() })),
            }
        }
    }
    items.push(Item::Text { text: "The cable needs developer mode on the headset. Turn it on in the Meta Horizon app on your phone.".into(), tone: Tone::Muted });
    for note in &model.network {
        items.push(Item::Text { text: note.clone(), tone: Tone::Warn });
    }
    Page { id: CONNECTION.into(), title: "Connection".into(), short: "Connection".into(), listed: true, items }
}

/// The Mac's desktop in the headset, and the permissions that it needs.
fn desktop_page(settings: &Settings, model: &Model) -> Page {
    let mut items = vec![Item::Heading { text: "Mac desktop in VR".into() }];
    for setting in DESKTOP_SETTINGS {
        // The added display's own settings show while there is one
        if matches!(setting.id, "vr_main_display" | "window_in_vr") && settings.virtual_display.is_none() {
            continue;
        }
        items.push(setting.item(settings, model));
    }
    if let Some((ok, message)) = &model.desktop {
        if !ok && !message.is_empty() {
            items.push(Item::Text { text: message.clone(), tone: Tone::Warn });
        }
    }
    items.push(Item::Heading { text: "Permissions".into() });
    for (permission, granted) in crate::run::PERMISSIONS.into_iter().zip(model.permissions) {
        let state = if granted { "allowed" } else { "not allowed" };
        let mut row = Buttons { title: format!("{}: {state}", permission.title()), detail: permission.reason().into(), ..Default::default() };
        if !granted {
            let key = permission_key(permission);
            row.buttons = vec![Button::run("Ask", "permission.ask").of(key).styled(Style::Primary), Button::run("Open Settings", "permission.settings").of(key)];
            row.hint = "macOS asks on the Mac's screen. In System Settings, turn on the switch next to cranvr. After that macOS may ask to quit and reopen cranvr.".into();
        }
        items.push(Item::Buttons(row));
    }
    Page { id: DESKTOP.into(), title: "Mac desktop".into(), short: "Desktop".into(), listed: true, items }
}

fn sound_page(settings: &Settings, model: &Model) -> Page {
    let mut items = vec![Item::Heading { text: "Sound".into() }];
    items.extend(SOUND_SETTINGS.iter().map(|setting| setting.item(settings, model)));
    if let Some((false, why)) = &model.audio {
        items.push(Item::Text { text: format!("No game sound in the headset: {why}"), tone: Tone::Warn });
    }
    if let MicrophoneSource::Mac { uid, name } = &settings.microphone {
        if !model.input_devices.iter().any(|device| device.uid == *uid) {
            items.push(Item::Text { text: format!("{name} is not connected. Games hear silence until it is."), tone: Tone::Warn });
        }
    }
    Page { id: SOUND.into(), title: "Sound".into(), short: "Sound".into(), listed: true, items }
}

/// The Wine that runs the games and the options of every program that cranvr starts. The text
/// fields stay in the window: the page shows what they hold.
fn wine_page(settings: &Settings, model: &Model) -> Page {
    let mut items = vec![Item::Heading { text: "Wine".into() }];
    items.push(Item::Text { text: format!("Wine: {}", steam::engine().display()), tone: Tone::Muted });
    items.push(Item::Text { text: format!("Prefix: {}", steam::prefix().display()), tone: Tone::Muted });
    items.extend(WINE_SETTINGS.iter().map(|setting| setting.item(settings, model)));
    for (name, value) in [("DLL overrides for every program", &settings.wine.dll_overrides), ("Environment for every program", &settings.wine.env)] {
        if !value.is_empty() {
            items.push(Item::Text { text: format!("{name}: {value}"), tone: Tone::Muted });
        }
    }
    Page { id: WINE.into(), title: "Wine".into(), short: "Wine".into(), listed: true, items }
}

/// The headset's room and volume, which the headset applies and the Mac keeps.
fn headset_page(settings: &Settings, model: &Model) -> Page {
    let mut items = vec![Item::Heading { text: "Headset".into() }];
    items.extend(HEADSET_SETTINGS.iter().map(|setting| setting.item(settings, model)));
    if settings.headset_room.is_none() || settings.headset_volume.is_none() || settings.headset_interface.is_none() || settings.headset_theme.is_none() {
        items.push(Item::Text { text: "The headset tells its own choice when it connects.".into(), tone: Tone::Muted });
    }
    Page { id: HEADSET.into(), title: "Headset".into(), short: "Headset".into(), listed: true, items }
}

/// The Windows programs that the Mac found, with Add and Run once: the headset has no file chooser,
/// so it adds a game from this list. The window lists them too, beside its file chooser.
fn programs_page(model: &Model) -> Page {
    let mut items = vec![Item::Heading { text: "Programs on this Mac".into() }];
    if model.found_programs.is_empty() {
        items.push(Item::Text { text: "No other Windows program in the Steam libraries or in C:\\Games.".into(), tone: Tone::Muted });
    }
    for path in &model.found_programs {
        let folder = path.parent().and_then(|folder| folder.file_name()).map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let file = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let subject = path.display().to_string();
        items.push(Item::Buttons(Buttons {
            title: folder,
            detail: file,
            buttons: vec![Button::run("Add", "programs.add").of(subject.clone()).styled(Style::Primary), Button::run("Run once", "programs.run").of(subject)],
            hint: String::new(),
        }));
    }
    items.push(Item::Text { text: "Add puts the program in the list of games with its own options. Run once starts it as it is.".into(), tone: Tone::Muted });
    Page { id: PROGRAMS.into(), title: "Add a game".into(), short: "Add game".into(), listed: true, items }
}

/// The newest lines of the log, for a minute after the headset asked: the lines change all the
/// time, and the menu goes to the headset at each change.
fn log_page(model: &Model) -> Page {
    let mut items = vec![Item::Heading { text: "Log".into() }];
    if model.log_until.is_some() {
        let lines: Vec<&String> = model.log.iter().rev().take(LOG_LINES).collect();
        for line in lines.into_iter().rev() {
            items.push(Item::Text { text: line.chars().take(140).collect(), tone: Tone::Muted });
        }
    } else {
        items.push(Item::Text { text: "The newest lines of the log show here for a minute.".into(), tone: Tone::Muted });
    }
    items.push(Item::Buttons(Buttons { buttons: vec![Button::run("Show the newest lines", "log.watch")], ..Default::default() }));
    Page { id: LOG.into(), title: "Log".into(), short: "Log".into(), listed: true, items }
}

/// The host, the settings file, the paired headsets and the Mac's addresses.
fn mac_page(model: &Model) -> Page {
    let mut items = vec![Item::Heading { text: "This Mac".into() }];
    items.push(Item::Buttons(Buttons {
        title: if model.running { "The host runs".into() } else { "The host is stopped".into() },
        buttons: vec![Button::run(if model.running { "Restart the host" } else { "Start the host" }, "host.restart")],
        hint: "A new start of the host stops the stream for a moment. The headset reconnects by itself.".into(),
        ..Default::default()
    }));
    if let Some(problem) = &model.settings_problem {
        items.push(Item::Text { text: problem.clone(), tone: Tone::Warn });
    }
    let paired = match &model.native {
        None => "The host tells which headsets are paired while it runs.".to_string(),
        Some(native) if native.paired.is_empty() => "No headset is paired yet.".to_string(),
        Some(native) => format!("Paired headsets: {}", native.paired.join(", ")),
    };
    items.push(Item::Text { text: paired, tone: Tone::Normal });
    if model.native.as_ref().is_some_and(|native| !native.paired.is_empty()) {
        items.push(Item::Text { text: format!("To forget a headset, delete its line in paired-headsets.txt in {} and restart the host.", crate::backend::config_dir().display()), tone: Tone::Muted });
    }
    let port = model.native.as_ref().map_or(crate::backend::native_port(), |native| native.port);
    let addresses: Vec<String> = model.lan.iter().map(|ip| format!("{ip}:{port}")).collect();
    items.push(Item::Text {
        text: if addresses.is_empty() { "This Mac has no address on a local network.".into() } else { format!("Addresses of this Mac: {}", addresses.join(", ")) },
        tone: Tone::Muted,
    });
    Page { id: MAC.into(), title: "This Mac".into(), short: "Mac".into(), listed: true, items }
}

/// A game's page: its VR system, the eye options and what it starts with. `None` for a game whose
/// program cranvr does not know.
fn game_page_of(settings: &Settings, model: &Model, game: &Game) -> Option<Page> {
    let program = steam::program(game)?;
    let mut page = options_page(settings, model, &program);
    page.title = game.name.clone();
    let runs = runs(model, &program);
    let mut buttons = Vec::new();
    if game.exe.is_some() {
        buttons.push(Button::run(if runs { "Playing" } else { "Play" }, "game.play").of(program.clone()).styled(Style::Primary).running("Starting...", busy(model, &pending::Request::Play(program.clone()))));
    }
    if runs {
        buttons.push(Button::run("Stop", "steam.stop").styled(Style::Danger).running("Stopping...", busy(model, &pending::Request::StopSteam)));
    }
    // A game that the user added has no Steam manifest: only the list holds it
    if game.app_id.is_empty() {
        buttons.push(Button::run("Remove", "game.remove").of(program.clone()).styled(Style::Danger));
    }
    if !buttons.is_empty() {
        page.items.push(Item::Buttons(Buttons { buttons, ..Default::default() }));
    }
    Some(page)
}

/// The launch options of the program `program` as a page: its VR system, the eye options, cranvr's
/// reasons and what the program starts with. A game's page has them, and so has the program that the
/// window started last with "Choose a program".
pub fn options_page(settings: &Settings, _model: &Model, program: &str) -> Page {
    let options = settings.launch.get(program).cloned().unwrap_or_default();
    let mut items = Vec::new();
    for setting in GAME_SETTINGS {
        items.push(match &setting.kind {
            GameKind::Choice { options: offered, get, .. } => Item::Choice(Choice {
                id: setting.id.into(),
                subject: Some(program.to_string()),
                label: setting.label.into(),
                options: offered(settings),
                value: get(&options),
                hint: if setting.id == "game.openvr" { OPENVR_HINT.into() } else { String::new() },
                restarts: false,
                shape: ChoiceShape::Keys,
            }),
            GameKind::Toggle { get, .. } => Item::Toggle(Toggle {
                id: setting.id.into(),
                subject: Some(program.to_string()),
                label: setting.label.into(),
                on: get(&options),
                hint: if setting.id == "game.eye_one_pass" { EYE_HINT.into() } else { String::new() },
                restarts: false,
            }),
        });
        // cranvr's reason for a value that it chose stands under the setting
        let reason = match setting.id {
            "game.openvr" if options.openvr.is_some() => &options.openvr_reason,
            "game.eye_per_pass" if options.eye_per_pass => &options.eye_per_pass_reason,
            "game.eye_one_pass" if options.eye_one_pass => &options.eye_one_pass_reason,
            _ => "",
        };
        if !reason.is_empty() {
            items.push(Item::Text { text: reason.to_string(), tone: Tone::Muted });
        }
    }
    let summary = launch::summary(&settings.wine, &options);
    items.push(Item::Text { text: if summary.is_empty() { "The game starts with nothing added.".into() } else { format!("The game starts with: {summary}") }, tone: Tone::Muted });
    Page { id: game_page(program), title: program.rsplit('\\').next().unwrap_or(program).to_string(), short: "Options".into(), listed: false, items }
}

/// Whether `program` has launch options that a request may change: a game's program, or the program
/// that the window started last.
fn known_program(settings: &Settings, games: &[Game], program: &str) -> bool {
    games.iter().any(|game| steam::program(game).as_deref() == Some(program))
        || (settings.game.starts_with('/') && steam::mac_to_windows(std::path::Path::new(&settings.game)) == program)
}

/// What a request asks of the caller besides the new settings.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Starts this game, as its Play button does
    Play(Game),
    /// Ends every game and Steam
    StopAll,
    /// Forgets the render sizes that the host learned
    ForgetSizes,
    /// Runs the steps of the setup that this Mac needs: Rosetta, the game support, Steam
    Setup,
    /// Starts Steam
    OpenSteam,
    /// Tells a running host the Connection choice
    TellConnection(Connection),
    /// Downloads Google's platform-tools for the USB cable
    DownloadUsb,
    /// Starts the host again with the saved settings: the headset reconnects by itself
    RestartHost,
    /// Shows macOS's prompt for a permission on the Mac's screen
    AskPermission(Permission),
    /// Opens System Settings at a permission's pane on the Mac's screen
    PermissionSettings(Permission),
    /// Hands a press on the headset's own page to the headset
    ToHeadset(Request),
    /// Adds a program that the Mac found to the games
    AddProgram(std::path::PathBuf),
    /// Starts a program that the Mac found once
    RunProgram(std::path::PathBuf),
    /// The log page carries the newest lines for a while
    WatchLog,
}

/// What a request did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    /// The settings changed: save them
    pub changed: bool,
    /// The change needs a new start of the host
    pub restart: bool,
    pub effects: Vec<Effect>,
    /// The request that runs from now on, for the button that asked for it
    pub pending: Option<pending::Request>,
    /// Why the app did nothing, for the window and the console
    pub refused: Option<String>,
}

impl Outcome {
    fn refused(why: String) -> Self {
        Self { refused: Some(why), ..Default::default() }
    }
}

/// Applies a press of either app to `settings`. `model` is the app's state: its games and the Mac's
/// devices. The caller saves the settings when the outcome says that they changed, and does its
/// effects.
pub fn apply(settings: &mut Settings, model: &Model, request: &Request) -> Outcome {
    let games = &model.games;
    // The headset's own page belongs to the headset: the press goes there
    if request.id().starts_with("headset.") {
        return Outcome { effects: vec![Effect::ToHeadset(request.clone())], ..Default::default() };
    }
    match request {
        Request::Set { id, subject: None, value } if id == "controls" => match cranvr_common::controls::Controls::from_json(value) {
            Some(controls) => {
                let changed = settings.controls != controls;
                settings.controls = controls;
                Outcome { changed, ..Default::default() }
            }
            None => Outcome::refused("The headset sent controls that cranvr cannot read".into()),
        },
        Request::Set { id, subject: None, value } => {
            let Some(setting) = setting(id) else {
                return Outcome::refused(format!("cranvr has no setting {id}"));
            };
            let before = settings.clone();
            if !setting.set(settings, model, value) {
                return Outcome::refused(format!("{} has no choice {value}", setting.label));
            }
            let changed = *settings != before;
            // The host keeps running: the Connection choice counts from the headset's next connection
            let effects = if changed && setting.id == "connection" { vec![Effect::TellConnection(settings.connection)] } else { Vec::new() };
            Outcome { changed, restart: changed && setting.restarts, effects, ..Default::default() }
        }
        Request::Set { id, subject: Some(program), value } => {
            let Some(setting) = GAME_SETTINGS.iter().find(|setting| setting.id == id) else {
                return Outcome::refused(format!("cranvr has no game setting {id}"));
            };
            if !known_program(settings, games, program) {
                return Outcome::refused(format!("cranvr knows no game with the program {program}"));
            }
            let mut options = settings.launch.get(program).cloned().unwrap_or_default();
            let ok = match &setting.kind {
                GameKind::Choice { options: offered, set, .. } => {
                    let ok = offered(settings).iter().any(|option| option.value == *value);
                    if ok {
                        set(&mut options, value);
                    }
                    ok
                }
                GameKind::Toggle { set, .. } => {
                    let ok = value == ON || value == OFF;
                    if ok {
                        set(&mut options, value == ON);
                    }
                    ok
                }
            };
            if !ok {
                return Outcome::refused(format!("{} has no choice {value}", setting.label));
            }
            let before = settings.launch.clone();
            // Options equal to the defaults leave no entry
            if options.is_default() {
                settings.launch.remove(program);
            } else {
                settings.launch.insert(program.clone(), options);
            }
            Outcome { changed: settings.launch != before, ..Default::default() }
        }
        Request::Run { id, subject } if id.starts_with("programs.") => {
            // Only a program that the Mac found itself: a request names no other path to start
            match model.found_programs.iter().find(|path| Some(path.display().to_string().as_str()) == subject.as_deref()) {
                Some(path) if id == "programs.add" => Outcome { effects: vec![Effect::AddProgram(path.clone())], ..Default::default() },
                Some(path) if id == "programs.run" => Outcome { effects: vec![Effect::RunProgram(path.clone())], ..Default::default() },
                Some(_) => Outcome::refused(format!("cranvr has no action {id}")),
                None => Outcome::refused("cranvr found no such program on this Mac".into()),
            }
        }
        Request::Run { id, subject } => run(settings, games, id, subject.as_deref()),
    }
}

/// The permission that a request names by its key
fn permission_of(key: Option<&str>) -> Option<Permission> {
    crate::run::PERMISSIONS.into_iter().find(|permission| Some(permission_key(*permission)) == key)
}

fn permission_key(permission: Permission) -> &'static str {
    match permission {
        Permission::ScreenRecording => "screen_recording",
        Permission::SystemAudio => "system_audio",
        Permission::Accessibility => "accessibility",
    }
}

fn run(settings: &mut Settings, games: &[Game], id: &str, subject: Option<&str>) -> Outcome {
    let game = || subject.and_then(|program| games.iter().find(|game| steam::program(game).as_deref() == Some(program)));
    match id {
        "game.play" => match game() {
            Some(game) if game.exe.is_some() => {
                let program = steam::program(game).unwrap_or_default();
                Outcome { effects: vec![Effect::Play(game.clone())], pending: Some(pending::Request::Play(program)), ..Default::default() }
            }
            _ => Outcome::refused(format!("The console asked for {}, which is not installed", subject.unwrap_or("a game"))),
        },
        "steam.stop" => Outcome { effects: vec![Effect::StopAll], pending: Some(pending::Request::StopSteam), ..Default::default() },
        "sizes.forget" => Outcome { effects: vec![Effect::ForgetSizes], ..Default::default() },
        "setup.start" => Outcome { effects: vec![Effect::Setup], ..Default::default() },
        "steam.open" => Outcome { effects: vec![Effect::OpenSteam], pending: Some(pending::Request::OpenSteam), ..Default::default() },
        "usb.download" => Outcome { effects: vec![Effect::DownloadUsb], ..Default::default() },
        "host.restart" => Outcome { effects: vec![Effect::RestartHost], ..Default::default() },
        "log.watch" => Outcome { effects: vec![Effect::WatchLog], ..Default::default() },
        "permission.ask" | "permission.settings" => match permission_of(subject) {
            Some(permission) if id == "permission.ask" => Outcome { effects: vec![Effect::AskPermission(permission)], ..Default::default() },
            Some(permission) => Outcome { effects: vec![Effect::PermissionSettings(permission)], ..Default::default() },
            None => Outcome::refused(format!("cranvr has no permission {}", subject.unwrap_or("without a name"))),
        },
        "game.remove" => match game() {
            Some(game) if game.app_id.is_empty() => {
                let exe = game.exe.clone().unwrap_or_default();
                steam::remove_game(settings, &exe);
                Outcome { changed: true, ..Default::default() }
            }
            _ => Outcome::refused("Only a game that you added from a folder can be removed".into()),
        },
        _ => Outcome::refused(format!("cranvr has no action {id}")),
    }
}

#[cfg(test)]
#[path = "pages_tests.rs"]
mod tests;
