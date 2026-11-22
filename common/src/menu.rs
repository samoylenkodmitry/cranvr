//! The settings and the actions of cranvr as data, for both apps: the window on the Mac and the
//! console in the headset.
//!
//! The cranvr app on the Mac owns every setting (`app.json`) and every action, such as a game's start
//! or Steam's setup. It describes them as pages of items. Its window draws the pages. It also writes
//! them into `vr-menu.json`, which the host sends to the headset as `HostMsg::Menu`, and the headset's
//! console draws the same pages. A press in either app becomes a [`Request`]. The headset sends its
//! request as `ClientMsg::Console`. The host hands it to the app, and the app applies it with the
//! function that its window's buttons call. Neither drawing knows what an item means, so a new
//! setting shows in both apps with no change of the headset's code.
//!
//! An item names its setting or action by `id`. A per-game item also names its game by `subject`:
//! the program's Windows path, which keys the game's launch options in `app.json`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// A page of items, such as the stream's settings or one game's options.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Page {
    /// The page's name in a link, such as `stream` or `game:C:\Games\Beat Saber\Beat Saber.exe`
    pub id: String,
    pub title: String,
    /// The words of a button that opens the page, such as "Steam"
    #[serde(default)]
    pub short: String,
    /// The menus list the page among the pages. A game's page is reached from the game's row.
    #[serde(default)]
    pub listed: bool,
    pub items: Vec<Item>,
}

impl Page {
    /// The item with this id and subject, if the page has it.
    pub fn find(&self, id: &str, subject: Option<&str>) -> Option<&Item> {
        self.items.iter().find(|item| item.names(id, subject))
    }
}

/// One thing on a page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    /// The title of the items that follow it
    Heading { text: String },
    /// A line of words: a state, a number or a reason
    Text {
        text: String,
        #[serde(default)]
        tone: Tone,
    },
    Choice(Choice),
    Toggle(Toggle),
    Buttons(Buttons),
}

impl Item {
    /// Whether this item is the setting or holds the button of `id` and `subject`.
    pub fn names(&self, id: &str, subject: Option<&str>) -> bool {
        match self {
            Item::Choice(choice) => choice.id == id && choice.subject.as_deref() == subject,
            Item::Toggle(toggle) => toggle.id == id && toggle.subject.as_deref() == subject,
            Item::Buttons(row) => row.buttons.iter().any(|button| button.runs(id, subject)),
            Item::Heading { .. } | Item::Text { .. } => false,
        }
    }

    /// The value of a choice or a toggle ("on" or "off"), as a request sets it.
    pub fn value(&self) -> Option<&str> {
        match self {
            Item::Choice(choice) => Some(&choice.value),
            Item::Toggle(toggle) => Some(toggle.value()),
            _ => None,
        }
    }

    /// The button that runs `id` for `subject`.
    pub fn button(&self, id: &str, subject: Option<&str>) -> Option<&Button> {
        match self {
            Item::Buttons(row) => row.buttons.iter().find(|button| button.runs(id, subject)),
            _ => None,
        }
    }
}

/// How a line of words looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    #[default]
    Normal,
    /// Less important, such as a path or a note
    Muted,
    /// Something that works, such as "Steam is running"
    Good,
    /// Something that asks for the user, such as a missing permission
    Warn,
}

/// One value of several, such as the refresh rate.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub label: String,
    pub options: Vec<Opt>,
    /// The value of the option that holds now
    pub value: String,
    /// What the setting does, for the Mac's window. The headset shows no hints.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hint: String,
    /// A change restarts the host, so the stream stops for a moment
    #[serde(default, skip_serializing_if = "is_false")]
    pub restarts: bool,
}

/// One option of a choice.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opt {
    pub value: String,
    pub label: String,
}

impl Opt {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self { value: value.into(), label: label.into() }
    }
}

/// A setting that is on or off. A request sets it with the value "on" or "off".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Toggle {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub label: String,
    pub on: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hint: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub restarts: bool,
}

impl Toggle {
    pub fn value(&self) -> &'static str {
        if self.on {
            ON
        } else {
            OFF
        }
    }
}

/// The values of a toggle in a request.
pub const ON: &str = "on";
pub const OFF: &str = "off";

/// A row of buttons, with a title and a line of detail beside them, such as a game with Play and
/// Options. Either may be empty.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Buttons {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    pub buttons: Vec<Button>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hint: String,
}

/// A button that runs an action on the Mac or opens another page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Button {
    pub label: String,
    pub press: Press,
    #[serde(default)]
    pub style: Style,
    /// The words of the button while its action runs, such as "Starting...". The app shows them from
    /// the press on, before the Mac says that the action runs.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub running: String,
    /// The Mac works on the action now: the button shows `running`
    #[serde(default, skip_serializing_if = "is_false")]
    pub busy: bool,
}

impl Button {
    /// A button that runs `id` on the Mac.
    pub fn run(label: impl Into<String>, id: impl Into<String>) -> Self {
        Self { label: label.into(), press: Press::Run { id: id.into(), subject: None }, style: Style::Normal, running: String::new(), busy: false }
    }

    /// A button that opens the page `page`.
    pub fn open(label: impl Into<String>, page: impl Into<String>) -> Self {
        Self { label: label.into(), press: Press::Open { page: page.into() }, style: Style::Normal, running: String::new(), busy: false }
    }

    pub fn of(mut self, subject: impl Into<String>) -> Self {
        if let Press::Run { subject: own, .. } = &mut self.press {
            *own = Some(subject.into());
        }
        self
    }

    pub fn styled(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// The words while the action runs, and whether it runs now.
    pub fn running(mut self, words: impl Into<String>, busy: bool) -> Self {
        self.running = words.into();
        self.busy = busy;
        self
    }

    pub fn runs(&self, id: &str, subject: Option<&str>) -> bool {
        matches!(&self.press, Press::Run { id: own, subject: of } if own == id && of.as_deref() == subject)
    }

    /// The words that the button shows now.
    pub fn words(&self) -> &str {
        if self.busy && !self.running.is_empty() {
            &self.running
        } else {
            &self.label
        }
    }
}

/// What a press on a button does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "does", rename_all = "snake_case")]
pub enum Press {
    /// Runs the action `id` on the Mac, for the game `subject` when it names one
    Run {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subject: Option<String>,
    },
    /// Opens the page `page`. It asks nothing of the Mac.
    Open { page: String },
}

impl Press {
    /// The request that the press sends to the Mac. Opening a page sends none.
    pub fn request(&self) -> Option<Request> {
        match self {
            Press::Run { id, subject } => Some(Request::Run { id: id.clone(), subject: subject.clone() }),
            Press::Open { .. } => None,
        }
    }
}

/// How a button looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    #[default]
    Normal,
    /// The action that the page is for, such as Play
    Primary,
    /// An action that ends or removes something, such as Stop
    Danger,
}

/// What a press asks of the Mac's app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Gives the setting `id` the value `value`: an option's value, or "on" or "off" for a toggle
    Set { id: String, subject: Option<String>, value: String },
    /// Runs the action `id`
    Run { id: String, subject: Option<String> },
}

impl Request {
    pub fn set(id: impl Into<String>, value: impl Into<String>) -> Self {
        Request::Set { id: id.into(), subject: None, value: value.into() }
    }

    pub fn run(id: impl Into<String>) -> Self {
        Request::Run { id: id.into(), subject: None }
    }

    pub fn of(self, game: impl Into<String>) -> Self {
        match self {
            Request::Set { id, value, .. } => Request::Set { id, subject: Some(game.into()), value },
            Request::Run { id, .. } => Request::Run { id, subject: Some(game.into()) },
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Request::Set { id, .. } | Request::Run { id, .. } => id,
        }
    }

    pub fn subject(&self) -> Option<&str> {
        match self {
            Request::Set { subject, .. } | Request::Run { subject, .. } => subject.as_deref(),
        }
    }

    /// The request as the headset sends it: `{"action": "set", "id": "fps", "value": "90"}` or
    /// `{"action": "run", "id": "game.play", "subject": "C:\\..."}`.
    pub fn to_json(&self) -> Value {
        let mut value = match self {
            Request::Set { id, value, .. } => json!({"action": "set", "id": id, "value": value}),
            Request::Run { id, .. } => json!({"action": "run", "id": id}),
        };
        if let Some(subject) = self.subject() {
            value["subject"] = subject.into();
        }
        value
    }

    /// The request in a JSON object. The host's status line adds a "type", which counts for nothing
    /// here. `None` for anything else.
    pub fn from_json(value: &Value) -> Option<Self> {
        let id = value["id"].as_str()?.to_string();
        let subject = value["subject"].as_str().map(str::to_string);
        match value["action"].as_str()? {
            "set" => Some(Request::Set { id, subject, value: value["value"].as_str()?.to_string() }),
            "run" => Some(Request::Run { id, subject }),
            _ => None,
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Page {
        Page {
            id: "stream".into(),
            title: "Stream".into(),
            short: "Stream".into(),
            listed: true,
            items: vec![
                Item::Heading { text: "Picture".into() },
                Item::Choice(Choice {
                    id: "fps".into(),
                    label: "Refresh rate".into(),
                    options: vec![Opt::new("90", "90 Hz"), Opt::new("120", "120 Hz")],
                    value: "120".into(),
                    restarts: true,
                    ..Default::default()
                }),
                Item::Toggle(Toggle { id: "game_audio".into(), label: "Game sound".into(), on: true, ..Default::default() }),
                Item::Text { text: "Steam is running".into(), tone: Tone::Good },
                Item::Buttons(Buttons {
                    title: "Beat Saber".into(),
                    buttons: vec![Button::run("Play", "game.play").of("C:\\Beat Saber.exe").styled(Style::Primary).running("Starting...", true), Button::open("Options", "game:C:\\Beat Saber.exe")],
                    ..Default::default()
                }),
            ],
        }
    }

    #[test]
    fn a_page_goes_through_json_unchanged() {
        let page = sample();
        let text = serde_json::to_string(&page).unwrap();
        assert_eq!(serde_json::from_str::<Page>(&text).unwrap(), page);
        // Empty hints and false flags stay out of the text, which the host sends with every change
        assert!(!text.contains("hint") && !text.contains("\"busy\":false"), "{text}");
    }

    #[test]
    fn an_item_is_found_by_its_id_and_its_game() {
        let page = sample();
        assert_eq!(page.find("fps", None).and_then(Item::value), Some("120"));
        assert_eq!(page.find("game_audio", None).and_then(Item::value), Some(ON));
        let play = page.find("game.play", Some("C:\\Beat Saber.exe")).and_then(|item| item.button("game.play", Some("C:\\Beat Saber.exe")));
        assert_eq!(play.map(Button::words), Some("Starting..."));
        // A per-game item needs its game
        assert!(page.find("game.play", None).is_none());
    }

    #[test]
    fn a_request_reads_back_from_its_json_and_from_the_hosts_status_line() {
        for request in [Request::set("fps", "90"), Request::set("game.openvr", "steamvr").of("C:\\a.exe"), Request::run("steam.open"), Request::run("game.play").of("C:\\a.exe")] {
            assert_eq!(Request::from_json(&request.to_json()), Some(request.clone()));
            let mut line = request.to_json();
            line["type"] = "vr_request".into();
            assert_eq!(Request::from_json(&line), Some(request));
        }
        assert_eq!(Request::from_json(&json!({"action": "set", "id": "fps"})), None, "a change needs its value");
        assert_eq!(Request::from_json(&json!({"action": "play", "game": "Beat Saber"})), None, "the old requests are gone");
        assert_eq!(Button::open("Options", "game:x").press.request(), None);
    }
}
