//! The console: the menu that the host offers in the headset (its games and settings), the
//! requests that the user sends back, and the rules for when the console shows and where.
//!
//! The cranvr app on the Mac writes the games and the settings into `vr-menu.json`. The host adds
//! the stream's state and sends the text as `HostMsg::Menu`. A press on a button sends a request
//! as `ClientMsg::Console`. The host hands it to the app as a `vr_request` status line.
//!
//! This file holds the data and the rules, with no Cranpose state. `console_panel.rs` draws them
//! and places the panel.

use serde_json::{json, Value};

/// The refresh rates and the render sizes that the console offers. The app accepts any value.
pub const RATES: [u32; 3] = [72, 90, 120];
pub const SCALES: [f32; 5] = [0.5, 0.6, 0.7, 0.85, 1.0];
/// The most games that the console lists
pub const MAX_GAMES: usize = 5;

/// The stream's state, from the host.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// The headset's refresh rate in Hz
    pub refresh: f32,
    /// Frames per second that the host sends
    pub fps: f32,
    /// Frames per second that the game makes
    pub game_fps: f32,
    pub latency_ms: Option<f32>,
    /// The program file of the game that runs, such as "Beat Saber.exe"
    pub game: Option<String>,
}

/// An installed game: its name and its program's file name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Game {
    pub name: String,
    pub program: String,
}

/// What the console shows: the app's games and settings and the host's state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Menu {
    pub games: Vec<Game>,
    pub fps: u32,
    pub game_scale: f32,
    /// The most MSAA samples a game may use; 0 lets the game choose
    pub msaa: u32,
    pub status: Status,
}

impl Menu {
    /// The menu from the host's text. `None` for a text that is no menu.
    pub fn parse(text: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(text).ok()?;
        let s = &v["status"];
        let number = |v: &Value| v.as_f64().map(|n| n as f32);
        Some(Self {
            games: v["games"]
                .as_array()?
                .iter()
                .filter_map(|g| Some(Game { name: g["name"].as_str()?.to_string(), program: g["program"].as_str().unwrap_or_default().to_string() }))
                .collect(),
            fps: v["fps"].as_u64().unwrap_or(120) as u32,
            game_scale: number(&v["game_scale"]).unwrap_or(1.0),
            msaa: v["msaa"].as_u64().unwrap_or(0) as u32,
            status: Status {
                refresh: number(&s["refresh"]).unwrap_or(0.0),
                fps: number(&s["fps"]).unwrap_or(0.0),
                game_fps: number(&s["game_fps"]).unwrap_or(0.0),
                latency_ms: number(&s["latency_ms"]),
                game: s["game"].as_str().map(str::to_string),
            },
        })
    }

    /// Whether `game` is the game that runs.
    pub fn runs(&self, game: &Game) -> bool {
        self.status.game.as_deref().is_some_and(|program| !game.program.is_empty() && program.eq_ignore_ascii_case(&game.program))
    }

    /// The games that the console lists
    pub fn listed(&self) -> &[Game] {
        &self.games[..self.games.len().min(MAX_GAMES)]
    }
}

/// What a console button asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Play(String),
    Stop,
    Fps(u32),
    Scale(f32),
    Msaa(u32),
}

impl Action {
    /// The request for the host, as JSON.
    pub fn request(&self) -> String {
        match self {
            Action::Play(name) => json!({"action": "play", "game": name}),
            Action::Stop => json!({"action": "stop"}),
            Action::Fps(fps) => json!({"action": "set", "fps": fps}),
            Action::Scale(scale) => json!({"action": "set", "game_scale": scale}),
            Action::Msaa(samples) => json!({"action": "set", "msaa": samples}),
        }
        .to_string()
    }
}

/// The sizes of the console's parts in dp. A dp is a millimeter in the room.
pub mod size {
    pub const WIDTH: f32 = 620.0;
    pub const PADDING: f32 = 26.0;
    pub const GAP: f32 = 10.0;
    pub const TITLE: f32 = 40.0;
    pub const TEXT: f32 = 30.0;
    pub const BUTTON: f32 = 46.0;
    pub const HEADING: f32 = 34.0;
}

/// The console's height in dp for a menu: the rows that `console_panel.rs` draws, with their gaps.
pub fn height(menu: Option<&Menu>) -> f32 {
    use size::*;
    let rows = |heights: &[f32]| heights.iter().map(|h| h + GAP).sum::<f32>();
    let Some(menu) = menu else {
        // The title, a line of text and the Close button
        return 2.0 * PADDING + rows(&[TITLE, TEXT, BUTTON]);
    };
    let games = menu.listed().len().max(1) as f32;
    // The title, the game's state, the heading and the games, the heading and four settings, the
    // note and the buttons at the bottom
    2.0 * PADDING + rows(&[TITLE, TEXT, HEADING]) + games * (BUTTON + GAP) + rows(&[HEADING, BUTTON, BUTTON, BUTTON, BUTTON, TEXT, BUTTON])
}

/// How long the left menu button must stay down to show or hide the console. A shorter press goes
/// to the game only: games use the button for their own menu.
pub const HOLD_NS: u64 = 800_000_000;

/// What the UI draws of the console.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub open: bool,
    pub menu: Option<Menu>,
}

/// The console's state: its menu and whether it shows.
#[derive(Default)]
pub struct Console {
    menu: Option<Menu>,
    open: bool,
    /// When the menu button went down, while it is held
    held_since: Option<u64>,
    /// The hold already toggled the console
    fired: bool,
    /// Whether a game ran at the last menu
    game_ran: Option<bool>,
    /// The console stays open when the link ends. A launch option sets it, for a run without a
    /// controller that could open the console.
    keep_open: bool,
}

impl Console {
    pub fn view(&self) -> View {
        View { open: self.open, menu: self.menu.clone() }
    }

    /// Shows the console, or hides it.
    fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Opens the console and keeps it open when the link ends.
    pub fn pin(&mut self) {
        self.keep_open = true;
        self.open = true;
    }

    /// A new menu from the host. The console shows when the first menu comes and while no game
    /// runs, and it hides when a game starts. The user's own toggle holds until the game starts or
    /// stops.
    pub fn on_menu(&mut self, menu: Menu) {
        let runs = menu.status.game.is_some();
        if self.game_ran != Some(runs) {
            self.game_ran = Some(runs);
            self.open = !runs;
        }
        self.menu = Some(menu);
    }

    /// The link ended: the menu is gone.
    pub fn clear(&mut self) {
        self.menu = None;
        self.game_ran = None;
        self.open = self.keep_open;
    }

    /// One frame of the headset: whether the left menu button is down. Returns whether a hold
    /// toggled the console in this frame. `now` is on the client's clock.
    pub fn on_frame(&mut self, menu_down: bool, now: u64) -> bool {
        if !menu_down {
            self.held_since = None;
            self.fired = false;
            return false;
        }
        let since = *self.held_since.get_or_insert(now);
        if self.fired || now.saturating_sub(since) < HOLD_NS {
            return false;
        }
        self.fired = true;
        self.toggle();
        true
    }
}

#[cfg(test)]
#[path = "tests/console.rs"]
mod tests;
