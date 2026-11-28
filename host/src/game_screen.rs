//! A game's loading screen and its message to the user (`HostMsg::GameScreen`), from the game's
//! mailbox messages.
//!
//! A game that speaks OpenVR tells SteamVR what to show between its scenes through OpenVR's mailbox
//! (`IVRMailbox`), and SteamVR draws it as the game's loading screen. Under cranvr, OpenComposite hands
//! each message to the runtime, and the runtime hands it to the host (`runtime_link.rs`, message 25).
//! This file is the one place that knows what such messages mean. It knows them by their own words,
//! never by the game or by the address of its mailbox: a message is a JSON object whose `type` names
//! what it does. docs/games.md lists the types and the games that send them.
//!
//! - `begin_loading`: a load begins. `loadingText` holds the words of the load, such as "LOADING".
//!   `tipText` and `description` hold lines under them, and `facingAngle` the screen's direction in
//!   degrees. A new load drops the message of the last one.
//! - `show_message`: `text` is a message to the user during the load, such as "Press Trigger To
//!   Start". The game's own page shows it as HTML, so the screen drops its tags. An empty text hides
//!   the message. A message outside a load shows nothing, as on the game's own page.
//! - `end_loading`: the load ended, and the screen hides.
//! - `set_paths`: the folders of the game's own page for SteamVR. Nothing shows.
//!
//! A message of another type, or one that is no JSON object, changes nothing.

use cranvr_proto::message::GameScreen;
use serde_json::Value;

/// The words of a load whose message names none
const LOADING: &str = "Loading";

/// What one mailbox message does to the screen.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// A load begins, and the screen shows this
    Begin(GameScreen),
    /// The message to the user during the load, or `None` to hide it
    Message(Option<String>),
    /// The load ended
    End,
    /// The message holds nothing that shows: the folders of the game's page, or a type that this
    /// file does not know
    Nothing,
}

/// What a mailbox message does to the screen.
pub fn read(message: &str) -> Change {
    let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(message) else { return Change::Nothing };
    let text = |name: &str| fields.get(name).and_then(Value::as_str).map(plain).filter(|text| !text.is_empty());
    match fields.get("type").and_then(Value::as_str) {
        Some("begin_loading") => Change::Begin(GameScreen {
            loading: Some(text("loadingText").unwrap_or_else(|| LOADING.to_owned())),
            lines: ["tipText", "description"].into_iter().filter_map(text).collect(),
            message: None,
            facing_deg: fields.get("facingAngle").and_then(Value::as_f64).map(|angle| angle as f32).filter(|angle| angle.is_finite()),
        }),
        Some("show_message") => Change::Message(text("text")),
        Some("end_loading") => Change::End,
        _ => Change::Nothing,
    }
}

/// The text of `html` without its tags and with its character references read. A line break
/// stays a line break.
fn plain(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        text.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else {
            // A lone `<` is text
            text.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let tag = rest[start + 1..start + end].trim().to_ascii_lowercase();
        if tag == "br" || tag.starts_with("br ") || tag.starts_with("br/") {
            text.push('\n');
        }
        rest = &rest[start + end + 1..];
    }
    text.push_str(rest);
    for (reference, character) in [("&lt;", "<"), ("&gt;", ">"), ("&quot;", "\""), ("&#39;", "'"), ("&nbsp;", " "), ("&amp;", "&")] {
        if text.contains(reference) {
            text = text.replace(reference, character);
        }
    }
    text.trim().to_owned()
}

/// What shows of the game that runs, as its messages left it.
#[derive(Debug, Default)]
pub struct Screen(Option<GameScreen>);

impl Screen {
    /// Applies `change`. True when what shows changed.
    pub fn apply(&mut self, change: Change) -> bool {
        let next = match change {
            Change::Begin(screen) => Some(screen),
            Change::Message(message) => self.0.as_ref().map(|shown| GameScreen { message, ..shown.clone() }),
            Change::End => None,
            Change::Nothing => return false,
        };
        let changed = next != self.0;
        self.0 = next;
        changed
    }

    /// Hides the screen, as at the end of the game. True when a screen showed.
    pub fn clear(&mut self) -> bool {
        self.0.take().is_some()
    }

    pub fn shown(&self) -> Option<&GameScreen> {
        self.0.as_ref()
    }
}

/// A line for the host's log about what shows now.
pub fn describe(screen: Option<&GameScreen>) -> String {
    let Some(screen) = screen else { return "nothing shows".to_owned() };
    let mut words = vec![format!("\"{}\"", screen.loading.as_deref().unwrap_or(""))];
    if !screen.lines.is_empty() {
        words.push(format!("{} lines", screen.lines.len()));
    }
    if let Some(message) = &screen.message {
        words.push(format!("the message \"{message}\""));
    }
    words.push(match screen.facing_deg {
        Some(angle) => format!("at {angle:.0} degrees"),
        None => "in front of the user".to_owned(),
    });
    words.join(", ")
}

#[cfg(test)]
#[path = "tests/game_screen.rs"]
mod tests;
