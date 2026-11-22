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
    backend::{Model, Settings},
    launch::{self, GameOptions, OpenVr},
    pending,
    steam::{self, Game},
};
use cranvr_common::{
    foveation::FoveationLevel,
    menu::{Button, Buttons, Choice, Item, Opt, Page, Request, Style, Toggle, Tone, OFF, ON},
};
use std::time::Instant;

/// The id of the page of games, which the console shows first
pub const PLAY: &str = "play";
pub const STREAM: &str = "stream";
/// The id of a game's page: this prefix and the game's program
pub const GAME_PAGE: &str = "game:";

/// The id of the page of a game's options.
pub fn game_page(program: &str) -> String {
    format!("{GAME_PAGE}{program}")
}

/// A setting of the stream and of games for every program, as it stands in the pages.
struct Setting {
    id: &'static str,
    label: &'static str,
    /// A change restarts the host
    restarts: bool,
    hint: fn(&Settings) -> &'static str,
    kind: Kind,
}

enum Kind {
    Choice { options: fn() -> Vec<Opt>, get: fn(&Settings) -> String, set: fn(&mut Settings, &str) },
    Toggle { get: fn(&Settings) -> bool, set: fn(&mut Settings, bool) },
}

impl Setting {
    fn item(&self, settings: &Settings) -> Item {
        let hint = (self.hint)(settings).to_string();
        match &self.kind {
            Kind::Choice { options, get, .. } => {
                Item::Choice(Choice { id: self.id.into(), subject: None, label: self.label.into(), options: options(), value: get(settings), hint, restarts: self.restarts })
            }
            Kind::Toggle { get, .. } => Item::Toggle(Toggle { id: self.id.into(), subject: None, label: self.label.into(), on: get(settings), hint, restarts: self.restarts }),
        }
    }

    /// Sets the value. False for a value that the setting does not offer.
    fn set(&self, settings: &mut Settings, value: &str) -> bool {
        match &self.kind {
            Kind::Choice { options, set, .. } => {
                if !options().iter().any(|option| option.value == value) {
                    return false;
                }
                set(settings, value);
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

/// The settings of the stream page, in the order of the page. Each one restarts the host but the
/// anti-aliasing limit, which a game reads when it starts, and the OpenVR choice, which counts at a
/// game's next start.
const STREAM_SETTINGS: &[Setting] = &[
    Setting {
        id: "fps",
        label: "Refresh rate",
        restarts: true,
        hint: no_hint,
        kind: Kind::Choice {
            options: || numbers(&FPS_CHOICES, " Hz"),
            get: |s| s.fps.to_string(),
            set: |s, v| s.fps = v.parse().unwrap_or(s.fps),
        },
    },
    Setting {
        id: "mbps",
        label: "Bitrate",
        restarts: true,
        hint: no_hint,
        kind: Kind::Choice {
            options: || numbers(&MBPS_CHOICES, " Mbps"),
            get: |s| s.mbps.to_string(),
            set: |s, v| s.mbps = v.parse().unwrap_or(s.mbps),
        },
    },
    Setting {
        id: "foveation_level",
        label: "Foveated encoding",
        restarts: true,
        hint: foveation_hint,
        kind: Kind::Choice {
            options: || FoveationLevel::ALL.iter().map(|level| Opt::new(level.name(), level_label(*level))).collect(),
            get: |s| s.foveation_level.name().to_string(),
            set: |s, v| s.foveation_level = FoveationLevel::parse(v).unwrap_or(s.foveation_level),
        },
    },
    Setting {
        id: "game_scale",
        label: "Game render size",
        restarts: true,
        hint: |_| "A game that makes fewer frames than the refresh rate adds 10 to 18 ms of latency. The Log tab then names the game's rate. A smaller size or lower anti-aliasing in the game helps.",
        kind: Kind::Choice {
            options: || GAME_SCALE_CHOICES.iter().map(|scale| Opt::new(scale_value(*scale), format!("{}%", scale_value(*scale)))).collect(),
            get: |s| GAME_SCALE_CHOICES.iter().find(|scale| (s.game_scale - **scale).abs() < 0.01).map_or_else(|| scale_value(s.game_scale), |scale| scale_value(*scale)),
            set: |s, v| s.game_scale = v.parse::<f32>().map_or(s.game_scale, |percent| percent / 100.0),
        },
    },
    Setting {
        id: "game_msaa_limit",
        label: "Game anti-aliasing",
        restarts: false,
        hint: |_| "Anti-aliasing costs a Windows game far more on the Mac: Beat Saber made 91 frames a second at 4x and 119 at 2x. With the limit, cranvr tells each game that more than 2x does not exist. It applies when a game starts. A game started from the Steam window gets it after a restart of Steam.",
        kind: Kind::Choice {
            options: || vec![Opt::new("2", "At most 2x"), Opt::new("0", "As the game sets it")],
            get: |s| s.game_msaa_limit.to_string(),
            set: |s, v| s.game_msaa_limit = v.parse().unwrap_or(s.game_msaa_limit),
        },
    },
    Setting {
        id: "game_audio",
        label: "Game sound in the headset",
        restarts: true,
        hint: |_| "The headset plays what this Mac plays. The capture takes the sound before the Mac's volume, so you can turn the Mac down. It needs Screen Recording.",
        kind: Kind::Toggle { get: |s| s.game_audio, set: |s, on| s.game_audio = on },
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
        id: "wine.openvr",
        label: "OpenVR games",
        restarts: false,
        hint: |_| OPENVR_EVERY_HINT,
        kind: Kind::Choice {
            options: || OpenVr::ALL.iter().map(|choice| Opt::new(choice.key(), choice.label())).collect(),
            get: |s| s.wine.openvr.key().to_string(),
            set: |s, v| s.wine.openvr = OpenVr::from_key(v).unwrap_or(s.wine.openvr),
        },
    },
    Setting {
        id: "autostart",
        label: "Start the host when cranvr opens",
        restarts: false,
        hint: no_hint,
        kind: Kind::Toggle { get: |s| s.autostart, set: |s, on| s.autostart = on },
    },
];

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
    let mut pages = vec![play_page(settings, model), stream_page(settings, model)];
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

/// Whether the game of `program` runs: the host names its program.
fn runs(model: &Model, program: &str) -> bool {
    model.game_program.as_deref().is_some_and(|running| pending::same_program(running, program))
}

fn stream_page(settings: &Settings, model: &Model) -> Page {
    let mut items = vec![Item::Heading { text: "Stream".into() }];
    items.extend(STREAM_SETTINGS.iter().map(|setting| setting.item(settings)));
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

/// Applies a press of either app to `settings`. `games` is the list of games. The caller saves the
/// settings when the outcome says that they changed, and does its effects.
pub fn apply(settings: &mut Settings, games: &[Game], request: &Request) -> Outcome {
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
            let Some(setting) = STREAM_SETTINGS.iter().find(|setting| setting.id == id) else {
                return Outcome::refused(format!("cranvr has no setting {id}"));
            };
            let before = settings.clone();
            if !setting.set(settings, value) {
                return Outcome::refused(format!("{} has no choice {value}", setting.label));
            }
            let changed = *settings != before;
            Outcome { changed, restart: changed && setting.restarts, ..Default::default() }
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
        Request::Run { id, subject } => run(settings, games, id, subject.as_deref()),
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
