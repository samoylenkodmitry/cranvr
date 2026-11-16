// cranvr: the console, a panel of its own in VR with what the cranvr window on the Mac offers:
// the stream's state, the installed games and the main settings.
//
// The console stands apart from the screens, tilted up like a desk console in front of the user.
// It is carried by its handle like a screen (see panel.rs). Its face is one sharp picture, drawn
// like a screen's bar (see bar.rs); boxes in the overlay frame it, so it reads as an object with a
// depth.
//
// The cranvr app on the Mac writes the games and the settings into vr-menu.json. The host adds
// the stream's state and sends it as MENU over the desktop layer's link (desktop_layer.rs). A
// press on a button sends CONSOLE with a request as JSON. The host hands it to the app as a
// status line; the app plays the game or changes the setting. A changed setting restarts the
// host, so the stream stops for a few seconds and the headset connects again by itself.

use crate::panel::Panel;
use alvr_common::glam::{Quat, Vec3};
use serde_json::{json, Value};

/// The console's number among the panels; screens count up from 0
pub const CONSOLE_ID: u8 = 250;
/// The console's width in meters
pub const WIDTH: f32 = 0.6;
/// The margin around the console's content, and the gap between items
const MARGIN: f32 = 0.022;
const GAP: f32 = 0.008;
/// Rows: a button's height, a text row's height, a title's height
const BUTTON: f32 = 0.042;
const TEXT: f32 = 0.032;
const TITLE: f32 = 0.04;
/// The handle's row at the top
const HANDLE_ROW: f32 = 0.036;
pub const HANDLE_WIDTH: f32 = 0.2;
pub const HANDLE_HEIGHT: f32 = 0.014;
/// The labels' column in a settings row
const LABEL: f32 = 0.15;
/// The most games that the console lists
const MAX_GAMES: usize = 5;
/// The default place: this far ahead of the head and this far below it, tilted up by TILT radians
const AHEAD: f32 = 0.55;
const BELOW: f32 = 0.38;
const TILT: f32 = 0.6;
const RATES: [u32; 3] = [72, 90, 120];
const SCALES: [f32; 5] = [0.5, 0.6, 0.7, 0.85, 1.0];

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
    pub low_latency: bool,
    pub status: Status,
}

impl Menu {
    /// The menu from the host's MENU message.
    pub fn parse(json: &[u8]) -> Option<Self> {
        let v: Value = serde_json::from_slice(json).ok()?;
        let s = &v["status"];
        let number = |v: &Value| v.as_f64().map(|n| n as f32);
        Some(Self {
            games: v["games"]
                .as_array()?
                .iter()
                .filter_map(|g| {
                    let name = g["name"].as_str()?.to_string();
                    Some(Game { name, program: g["program"].as_str().unwrap_or_default().to_string() })
                })
                .collect(),
            fps: v["fps"].as_u64().unwrap_or(120) as u32,
            game_scale: number(&v["game_scale"]).unwrap_or(1.0),
            msaa: v["msaa"].as_u64().unwrap_or(0) as u32,
            low_latency: v["low_latency"].as_bool().unwrap_or(false),
            status: Status {
                refresh: number(&s["refresh"]).unwrap_or(0.0),
                fps: number(&s["fps"]).unwrap_or(0.0),
                game_fps: number(&s["game_fps"]).unwrap_or(0.0),
                latency_ms: number(&s["latency_ms"]),
                game: s["game"].as_str().map(str::to_string),
            },
        })
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
    LowLatency(bool),
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
            Action::LowLatency(on) => json!({"action": "set", "low_latency": on}),
        }
        .to_string()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Title,
    Text,
    Button { action: Action, selected: bool },
}

/// One item on the console's face: its center and size in meters from the face's center (+y
/// up), its text and what it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub center: (f32, f32),
    pub size: (f32, f32),
    pub label: String,
    pub kind: Kind,
}

/// The face's height and its items.
pub fn layout(menu: &Menu) -> (f32, Vec<Item>) {
    let inner = WIDTH - 2.0 * MARGIN;
    let left = -WIDTH / 2.0 + MARGIN;
    // Rows go down from y = 0; the face is centered at the end
    let mut items: Vec<Item> = Vec::new();
    let mut y = -(MARGIN / 2.0 + HANDLE_ROW);
    let mut row = |height: f32, cells: Vec<(f32, String, Kind)>, items: &mut Vec<Item>| {
        let mut x = left;
        for (width, label, kind) in cells {
            items.push(Item { center: (x + width / 2.0, y - height / 2.0), size: (width, height), label, kind });
            x += width + GAP;
        }
        y -= height + GAP;
    };
    let text = |s: &str| (s.to_string(), Kind::Text);
    let button = |label: String, action: Action, selected: bool| (label, Kind::Button { action, selected });

    let s = &menu.status;
    let stream = if s.refresh > 0.0 {
        let latency = s.latency_ms.map_or(String::new(), |l| format!(", {l:.0} ms"));
        format!("{:.0} Hz, {:.0} fps{latency}", s.refresh, s.fps)
    } else {
        "No headset stream".to_string()
    };
    row(TITLE, vec![(0.16, "cranvr".into(), Kind::Title), (inner - 0.16 - GAP, stream, Kind::Text)], &mut items);
    let running = |g: &Game| s.game.as_deref().is_some_and(|p| !g.program.is_empty() && p.eq_ignore_ascii_case(&g.program));
    let game = match (&s.game, menu.games.iter().find(|g| running(g))) {
        (Some(_), Some(g)) => format!("{} makes {:.0} frames a second", g.name, s.game_fps),
        (Some(program), None) => format!("{program} makes {:.0} frames a second", s.game_fps),
        (None, _) => "No game runs".to_string(),
    };
    let (label, kind) = text(&game);
    row(TEXT, vec![(inner, label, kind)], &mut items);

    row(TITLE, vec![(inner, "Play".into(), Kind::Title)], &mut items);
    if menu.games.is_empty() {
        let (label, kind) = text("No games installed in cranvr's Steam");
        row(TEXT, vec![(inner, label, kind)], &mut items);
    }
    for g in menu.games.iter().take(MAX_GAMES) {
        let running = running(g);
        let mut cells = vec![(0.36, g.name.clone(), Kind::Button { action: Action::Play(g.name.clone()), selected: running })];
        if running {
            cells.push((0.14, "Stop".into(), Kind::Button { action: Action::Stop, selected: false }));
        }
        row(BUTTON, cells, &mut items);
    }

    row(TITLE, vec![(inner, "Settings".into(), Kind::Title)], &mut items);
    let setting = |name: &str, buttons: Vec<(String, Action, bool)>, width: f32| {
        let mut cells = vec![(LABEL, name.to_string(), Kind::Text)];
        cells.extend(buttons.into_iter().map(|(label, action, selected)| {
            let (label, kind) = button(label, action, selected);
            (width, label, kind)
        }));
        cells
    };
    row(BUTTON, setting("Refresh", RATES.iter().map(|&r| (format!("{r} Hz"), Action::Fps(r), menu.fps == r)).collect(), 0.1), &mut items);
    let scales = SCALES.iter().map(|&f| (format!("{:.0}%", f * 100.0), Action::Scale(f), (menu.game_scale - f).abs() < 0.01)).collect();
    row(BUTTON, setting("Game size", scales, 0.072), &mut items);
    let aa = vec![
        ("At most 2x".to_string(), Action::Msaa(2), menu.msaa == 2),
        ("As the game sets".to_string(), Action::Msaa(0), menu.msaa == 0),
    ];
    row(BUTTON, setting("Anti-aliasing", aa, 0.17), &mut items);
    let low = vec![("Low latency".to_string(), Action::LowLatency(!menu.low_latency), menu.low_latency)];
    row(BUTTON, setting("Latency", low, 0.17), &mut items);
    let (label, kind) = text("A change restarts the stream for a few seconds");
    row(TEXT, vec![(inner, label, kind)], &mut items);

    let height = -y + MARGIN / 2.0;
    for item in &mut items {
        item.center.1 += height / 2.0;
    }
    (height, items)
}

/// The handle's center on the face, in meters from the face's center.
pub fn handle_center(height: f32) -> (f32, f32) {
    (0.0, height / 2.0 - MARGIN / 2.0 - HANDLE_ROW / 2.0)
}

/// Whether a point on the face, in meters from its center, lies on the handle.
pub fn on_handle(height: f32, (x, y): (f32, f32)) -> bool {
    let (hx, hy) = handle_center(height);
    (x - hx).abs() <= HANDLE_WIDTH / 2.0 + 0.02 && (y - hy).abs() <= HANDLE_ROW / 2.0 + 0.01
}

/// The console's default place: below and ahead of the head, tilted up toward it.
pub fn default_place(head_position: Vec3, head_orientation: Quat, height: f32) -> Panel {
    let forward = head_orientation * Vec3::NEG_Z;
    let flat = Vec3::new(forward.x, 0.0, forward.z).try_normalize().unwrap_or(Vec3::NEG_Z);
    let facing = Quat::from_rotation_y((-flat).x.atan2((-flat).z));
    Panel {
        center: head_position + flat * AHEAD - Vec3::Y * BELOW,
        orientation: facing * Quat::from_rotation_x(-TILT),
        width: WIDTH,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu() -> Menu {
        Menu {
            games: vec![
                Game { name: "Beat Saber".into(), program: "Beat Saber.exe".into() },
                Game { name: "Half-Life: Alyx".into(), program: "hlvr.exe".into() },
            ],
            fps: 120,
            game_scale: 0.6,
            msaa: 2,
            low_latency: false,
            status: Status { refresh: 120.0, fps: 119.4, game_fps: 117.0, latency_ms: Some(46.2), game: Some("Beat Saber.exe".into()) },
        }
    }

    #[test]
    fn the_menu_comes_from_the_hosts_json() {
        let json = br#"{"games":[{"name":"Beat Saber","program":"Beat Saber.exe"},{"name":"Half-Life: Alyx","program":"hlvr.exe"}],
            "fps":120,"game_scale":0.6,"msaa":2,"low_latency":false,
            "status":{"refresh":120,"fps":119.4,"game_fps":117,"latency_ms":46.2,"game":"Beat Saber.exe"}}"#;
        assert_eq!(Menu::parse(json), Some(menu()));
        assert_eq!(Menu::parse(b"not json"), None);
    }

    #[test]
    fn the_layout_fits_the_face_and_marks_the_current_settings() {
        let (height, items) = layout(&menu());
        assert!(height > 0.3 && height < 0.7, "{height}");
        for item in &items {
            let (x, y) = item.center;
            let (w, h) = item.size;
            assert!(x - w / 2.0 >= -WIDTH / 2.0 - 1e-4 && x + w / 2.0 <= WIDTH / 2.0 + 1e-4, "{item:?}");
            assert!(y - h / 2.0 >= -height / 2.0 - 1e-4 && y + h / 2.0 <= height / 2.0 + 1e-4, "{item:?}");
            assert!(!on_handle(height, item.center), "{item:?}");
        }
        let selected: Vec<&str> = items
            .iter()
            .filter(|i| matches!(i.kind, Kind::Button { selected: true, .. }))
            .map(|i| i.label.as_str())
            .collect();
        assert_eq!(selected, ["Beat Saber", "120 Hz", "60%", "At most 2x"]);
        // the running game has a stop button; low latency asks to turn on
        assert!(items.iter().any(|i| i.kind == Kind::Button { action: Action::Stop, selected: false }));
        assert!(items.iter().any(|i| i.kind == Kind::Button { action: Action::LowLatency(true), selected: false }));
    }

    #[test]
    fn requests_are_json_for_the_app() {
        let v: Value = serde_json::from_str(&Action::Play("Beat Saber".into()).request()).unwrap();
        assert_eq!((v["action"].as_str(), v["game"].as_str()), (Some("play"), Some("Beat Saber")));
        let v: Value = serde_json::from_str(&Action::Scale(0.7).request()).unwrap();
        assert_eq!(v["action"], "set");
        assert!((v["game_scale"].as_f64().unwrap() - 0.7).abs() < 1e-6);
    }

    #[test]
    fn the_default_place_is_below_and_ahead_tilted_up() {
        let p = default_place(Vec3::new(0.0, 1.6, 0.0), Quat::IDENTITY, 0.4);
        assert!(p.center.y < 1.3 && p.center.z < -0.4, "{p:?}");
        // its front looks up and back toward the head
        let normal = p.orientation * Vec3::Z;
        assert!(normal.y > 0.4 && normal.z > 0.5, "{normal:?}");
    }
}
