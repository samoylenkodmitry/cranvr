//! The console's pages as rows of known heights: what each row draws, its keys and where each key
//! stands. The composables of `console_panel.rs` draw the rows. The scripted controller aims at a key
//! by the same numbers, and #111 can draw the same rows in another form.
//!
//! A page of the host (`cranvr_common::menu`) turns into rows here, item by item:
//! - a heading and a line of text are rows of their own. A long line takes as many rows of text as
//!   its words need;
//! - a choice puts its name and its options in one row when they fit, else its name in a row and the
//!   options in rows of their own, as many as fit side by side;
//! - a toggle puts its name on the left and the keys On and Off on the right;
//! - a row of buttons puts its title on the left and its buttons on the right, or lets the buttons
//!   share the row when it has no title.
//!
//! The headset shows no hints: they are the Mac window's long words.

use crate::console::{size, Interface, Menu, Pending, Press, Progress, Shown};
use crate::spatial_ui::STEP_LABELS;
use cranvr_common::menu::{Buttons, Choice, Item, Page, Press as Link, Request, Style, Toggle, Tone, OFF, ON};
use std::time::Instant;

/// The width of the rows inside the console's padding, in dp
pub const INNER: f32 = size::WIDTH - 2.0 * size::PADDING;
/// The width of a choice's name in front of its options
pub const LABEL: f32 = 150.0;
/// The width of each button beside a title
pub const KEY: f32 = 130.0;
/// The width of the keys On and Off of a toggle
pub const SWITCH: f32 = 96.0;
/// The space that a character of 22 sp text takes on average, and the padding of a key's words, in dp
const CHAR: f32 = 11.5;
const KEY_PAD: f32 = 28.0;
/// The characters of a line of 22 sp text across the console
const LINE_CHARS: usize = 46;
/// The most keys of one row
const ROW_KEYS: usize = 4;
/// The links to the pages stand three in a row
const LINKS: usize = 3;
/// A title with a line of detail under it needs a taller row
const DETAIL_EXTRA: f32 = 10.0;

/// How a key looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Normal,
    /// The option that holds now, or a toggle's state
    Selected,
    Primary,
    Danger,
}

/// A button of a row: its words, its look and what a press does.
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub label: String,
    pub press: Press,
    pub look: Look,
}

/// The name on the left of a row of keys.
#[derive(Clone, Debug, PartialEq)]
pub enum Name {
    None,
    /// A choice's name, `LABEL` wide: the keys share the rest
    Setting(String),
    /// A title that takes the room that the keys leave, with a line of detail under it. The keys are
    /// `width` wide each.
    Title { title: String, detail: String, width: f32 },
}

/// What form a row of keys takes in 3D (`spatial_ui.rs`). The flat form draws keys for all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// Keys side by side
    Keys,
    /// A toggle's On and Off: one switch over both keys
    Switch,
    /// A choice of numbers in one row: a slider over the keys with a stop for each value, and the
    /// values' words under it
    Steps,
}

/// What a row draws.
#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    /// The page's title, with the stream's state on the main page
    Title { text: String, status: String },
    Heading(String),
    Text { text: String, tone: Tone },
    Keys { name: Name, keys: Vec<Key>, form: Form },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub line: Line,
    pub height: f32,
}

impl Row {
    fn new(line: Line) -> Self {
        let height = match &line {
            Line::Title { .. } => size::TITLE,
            Line::Heading(_) => size::HEADING,
            Line::Text { text, .. } => size::TEXT * lines_of(text) as f32,
            Line::Keys { name: Name::Title { detail, .. }, .. } if !detail.is_empty() => size::BUTTON + DETAIL_EXTRA,
            Line::Keys { form: Form::Steps, .. } => size::BUTTON + STEP_LABELS,
            Line::Keys { .. } => size::BUTTON,
        };
        Self { line, height }
    }

    /// The middle of each key across the row, in dp from the row's left edge.
    #[cfg_attr(not(any(test, all(feature = "scripted-hand", target_os = "android"))), allow(dead_code))]
    pub fn key_centers(&self) -> Vec<f32> {
        let Line::Keys { name, keys, .. } = &self.line else { return Vec::new() };
        let n = keys.len() as f32;
        let gap = size::GAP;
        match name {
            Name::Title { width, .. } => {
                let start = INNER - n * width - (n - 1.0) * gap;
                (0..keys.len()).map(|i| start + i as f32 * (width + gap) + width / 2.0).collect()
            }
            Name::Setting(_) => {
                let each = (INNER - LABEL - n * gap) / n;
                (0..keys.len()).map(|i| LABEL + gap + i as f32 * (each + gap) + each / 2.0).collect()
            }
            Name::None => {
                let each = (INNER - (n - 1.0) * gap) / n;
                (0..keys.len()).map(|i| i as f32 * (each + gap) + each / 2.0).collect()
            }
        }
    }
}

/// How many lines of 22 sp text the words take across the console.
fn lines_of(text: &str) -> usize {
    let mut lines = 1;
    let mut used = 0;
    for word in text.split_whitespace() {
        let need = if used == 0 { word.len() } else { used + 1 + word.len() };
        if need > LINE_CHARS && used > 0 {
            lines += 1;
            used = word.len();
        } else {
            used = need;
        }
    }
    lines
}

/// The width of a key with these words, as a guess from their length
fn key_width(label: &str) -> f32 {
    label.chars().count() as f32 * CHAR + KEY_PAD
}

/// The words of a key: its own, or what the Mac does with its request now.
fn label_of(request: &Request, own: &str, running: &str, busy: bool, pending: Option<(&Pending, Progress)>) -> String {
    match pending.filter(|(pending, _)| pending.is(request)) {
        Some((_, Progress::Running)) => match request {
            Request::Set { .. } => "Changing...".into(),
            Request::Run { .. } if !running.is_empty() => running.into(),
            Request::Run { .. } => own.into(),
        },
        Some((_, Progress::NoAnswer)) => format!("{own}: no answer"),
        _ if busy && !running.is_empty() => running.into(),
        _ => own.into(),
    }
}

/// The rows of one item of a host's page.
fn item_rows(item: &Item, pending: Option<(&Pending, Progress)>, interface: Interface, rows: &mut Vec<Row>) {
    match item {
        Item::Heading { text } => rows.push(Row::new(Line::Heading(text.clone()))),
        Item::Text { text, tone } => rows.push(Row::new(Line::Text { text: text.clone(), tone: *tone })),
        Item::Choice(choice) => choice_rows(choice, pending, interface, rows),
        Item::Toggle(toggle) => rows.push(toggle_row(toggle, pending)),
        Item::Buttons(row) => buttons_rows(row, pending, rows),
    }
}

fn choice_rows(choice: &Choice, pending: Option<(&Pending, Progress)>, interface: Interface, rows: &mut Vec<Row>) {
    let keys: Vec<Key> = choice
        .options
        .iter()
        .map(|option| {
            let request = Request::Set { id: choice.id.clone(), subject: choice.subject.clone(), value: option.value.clone() };
            let label = label_of(&request, &option.label, "", false, pending);
            Key { label, press: Press::Host(request), look: if option.value == choice.value { Look::Selected } else { Look::Normal } }
        })
        .collect();
    // The name and the options in one row, when every option's words fit its share
    let n = keys.len().max(1) as f32;
    let share = (INNER - LABEL - n * size::GAP) / n;
    if keys.len() <= ROW_KEYS && choice.options.iter().all(|option| key_width(&option.label) <= share) {
        let form = if interface.is_3d() && steps(choice) { Form::Steps } else { Form::Keys };
        rows.push(Row::new(Line::Keys { name: Name::Setting(choice.label.clone()), keys, form }));
        return;
    }
    rows.push(Row::new(Line::Text { text: choice.label.clone(), tone: Tone::Normal }));
    pack(keys, rows);
}

/// Whether a choice reads as values in order: three or more options whose words start with a
/// number, such as rates and percents. The 3D form draws them as a slider.
fn steps(choice: &Choice) -> bool {
    choice.options.len() >= 3 && choice.options.iter().all(|option| option.label.trim_start().starts_with(|c: char| c.is_ascii_digit()))
}

/// Puts keys into rows of their own, as many side by side as their words fit.
fn pack(keys: Vec<Key>, rows: &mut Vec<Row>) {
    let mut row: Vec<Key> = Vec::new();
    for key in keys {
        let mut next = row.clone();
        next.push(key.clone());
        let share = (INNER - (next.len() as f32 - 1.0) * size::GAP) / next.len() as f32;
        let fits = next.len() <= ROW_KEYS && next.iter().all(|key| key_width(&key.label) <= share);
        if fits || row.is_empty() {
            row = next;
        } else {
            rows.push(Row::new(Line::Keys { name: Name::None, keys: std::mem::take(&mut row), form: Form::Keys }));
            row.push(key);
        }
    }
    if !row.is_empty() {
        rows.push(Row::new(Line::Keys { name: Name::None, keys: row, form: Form::Keys }));
    }
}

fn toggle_row(toggle: &Toggle, pending: Option<(&Pending, Progress)>) -> Row {
    let keys = [(ON, "On", true), (OFF, "Off", false)]
        .into_iter()
        .map(|(value, words, on)| {
            let request = Request::Set { id: toggle.id.clone(), subject: toggle.subject.clone(), value: value.into() };
            let label = label_of(&request, words, "", false, pending);
            Key { label, press: Press::Host(request), look: if toggle.on == on { Look::Selected } else { Look::Normal } }
        })
        .collect();
    Row::new(Line::Keys { name: Name::Title { title: toggle.label.clone(), detail: String::new(), width: SWITCH }, keys, form: Form::Switch })
}

fn buttons_rows(row: &Buttons, pending: Option<(&Pending, Progress)>, rows: &mut Vec<Row>) {
    let keys: Vec<Key> = row
        .buttons
        .iter()
        .map(|button| {
            let look = match button.style {
                Style::Normal => Look::Normal,
                Style::Primary => Look::Primary,
                Style::Danger => Look::Danger,
            };
            match &button.press {
                Link::Run { .. } => {
                    let request = button.press.request().expect("a run has a request");
                    let label = label_of(&request, &button.label, &button.running, button.busy, pending);
                    Key { label, press: Press::Host(request), look }
                }
                Link::Open { page } => Key { label: button.label.clone(), press: Press::Open(page.clone()), look },
            }
        })
        .collect();
    if row.title.is_empty() {
        pack(keys, rows);
    } else if keys.is_empty() {
        rows.push(Row::new(Line::Text { text: row.title.clone(), tone: Tone::Normal }));
    } else {
        rows.push(Row::new(Line::Keys { name: Name::Title { title: row.title.clone(), detail: row.detail.clone(), width: KEY }, keys, form: Form::Keys }));
    }
}

/// The rows of a host's page, in the form of `interface`.
pub fn page_rows(page: &Page, pending: Option<(&Pending, Progress)>, interface: Interface) -> Vec<Row> {
    let mut rows = Vec::new();
    for item in &page.items {
        item_rows(item, pending, interface, &mut rows);
    }
    rows
}

/// What the console needs besides the menu: the link and how the menus draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Local {
    /// A link runs, so Disconnect has a meaning
    pub link: bool,
    /// The 3D form draws a choice of numbers as a slider, with a line of values under it
    pub interface: Interface,
}

fn key(label: &str, press: Press, look: Look) -> Key {
    Key { label: label.into(), press, look }
}

/// The buttons at the bottom of the main page. Close hides the console. Controls opens the page for
/// the buttons of the controllers. Disconnect ends the link while it runs. Exit closes the app.
fn bottom(local: Local) -> Row {
    let mut keys = vec![key("Close", Press::Close, Look::Normal), key("Controls", Press::Controls, Look::Normal)];
    if local.link {
        keys.push(key("Disconnect", Press::Disconnect, Look::Danger));
    }
    keys.push(key("Exit", Press::Exit, Look::Danger));
    Row::new(Line::Keys { name: Name::None, keys, form: Form::Keys })
}

/// The rows of the console on `shown` with this menu. `status` is the stream's state for the title.
/// Without a menu the console shows that the host offers none, and its buttons.
pub fn console_rows(menu: Option<&Menu>, shown: &Shown, local: Local, no_menu: &str, pending: Option<&Pending>, now: Instant) -> Vec<Row> {
    let Some(menu) = menu else {
        return vec![Row::new(Line::Title { text: "cranvr".into(), status: String::new() }), Row::new(Line::Text { text: no_menu.into(), tone: Tone::Muted }), bottom(local)];
    };
    let pending = pending.map(|pending| (pending, pending.progress(menu, now)));
    let page = match shown {
        Shown::Host(id) => menu.page(id),
        _ => None,
    };
    let mut rows = Vec::new();
    if let Some(page) = page {
        rows.push(Row::new(Line::Title { text: page.title.clone(), status: String::new() }));
        if let Some(notice) = &menu.notice {
            rows.push(Row::new(Line::Text { text: notice.clone(), tone: Tone::Warn }));
        }
        rows.extend(page_rows(page, pending, local.interface));
        rows.push(Row::new(Line::Keys { name: Name::None, keys: vec![key("Back", Press::Back, Look::Normal), key("Close", Press::Close, Look::Normal)], form: Form::Keys }));
        return rows;
    }
    let status = &menu.status;
    let stream = if status.refresh > 0.0 {
        let latency = status.latency_ms.map_or(String::new(), |ms| format!(", {ms:.0} ms"));
        format!("{:.0} Hz, {:.0} fps{latency}", status.refresh, status.fps)
    } else {
        "No headset stream".to_string()
    };
    rows.push(Row::new(Line::Title { text: "cranvr".into(), status: stream }));
    let line = match &status.game {
        Some(program) => format!("{} makes {:.0} frames a second", program.strip_suffix(".exe").unwrap_or(program), status.game_fps),
        None => "No game runs".to_string(),
    };
    rows.push(Row::new(Line::Text { text: line, tone: Tone::Normal }));
    if let Some(notice) = &menu.notice {
        rows.push(Row::new(Line::Text { text: notice.clone(), tone: Tone::Warn }));
    }
    if let Some(main) = menu.main() {
        rows.extend(page_rows(main, pending, local.interface));
    }
    let links: Vec<Key> = menu.listed().into_iter().map(|(id, short)| key(&short, Press::Open(id), Look::Normal)).collect();
    if !links.is_empty() {
        rows.push(Row::new(Line::Heading("Settings".into())));
        for chunk in links.chunks(LINKS) {
            rows.push(Row::new(Line::Keys { name: Name::None, keys: chunk.to_vec(), form: Form::Keys }));
        }
    }
    rows.push(bottom(local));
    rows
}

/// The height of the console for these rows, in dp.
pub fn height(rows: &[Row]) -> f32 {
    2.0 * size::PADDING + rows.iter().map(|row| row.height + size::GAP).sum::<f32>() - size::GAP
}

/// The middle of the key `key` of the row `row`, in dp from the console's top left corner. The
/// scripted controller aims there.
#[cfg_attr(not(any(test, all(feature = "scripted-hand", target_os = "android"))), allow(dead_code))]
pub fn key_point(rows: &[Row], row: usize, key: usize) -> Option<[f32; 2]> {
    let top: f32 = rows[..row].iter().map(|row| row.height + size::GAP).sum();
    let target = rows.get(row)?;
    let x = *target.key_centers().get(key)?;
    Some([size::PADDING + x, size::PADDING + top + target.height / 2.0])
}

/// The row and the key that do `press`, the first of them.
#[cfg_attr(not(any(test, all(feature = "scripted-hand", target_os = "android"))), allow(dead_code))]
pub fn find_key(rows: &[Row], press: &Press) -> Option<(usize, usize)> {
    rows.iter().enumerate().find_map(|(r, row)| match &row.line {
        Line::Keys { keys, .. } => keys.iter().position(|key| key.press == *press).map(|k| (r, k)),
        _ => None,
    })
}

#[cfg(test)]
#[path = "tests/page_model.rs"]
mod tests;
