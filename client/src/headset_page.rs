//! The headset's own page for the cranvr app on the Mac: the panels with Show and Hide, each display
//! with its place, Hand or Room and Remove, Add display, Recenter, Controls, the hand menu, Disconnect
//! and Exit. The headset owns them, so the page comes from the hand menu's rows (`menu_model.rs`),
//! with the place of each display in place of its cost, which changes every second.
//!
//! The client sends the page as `ClientMsg::HeadsetPage` when it changes. The Mac's window draws it
//! with the items of its own pages (`cranvr_common::menu`), and a press there comes back as
//! `HostMsg::HeadsetRequest`. [`remote`] reads the request, and the UI does it as the menu's button
//! does.

use crate::{
    menu_model::{ButtonKind, MenuAction, MenuRow},
    panel_layout::PanelId,
};
use cranvr_common::menu::{Button, Buttons, Item, Page, Request, Style};
use cranvr_proto::message::DESKTOP_STREAM;

/// The page's id
pub const PAGE: &str = "in_headset";

/// What a press of the Mac on the headset's page asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remote {
    /// What a button of the hand menu does
    Menu(MenuAction),
    /// Opens the hand menu where it stands now
    OpenMenu,
    Disconnect,
    /// Ends the link and closes the app
    Exit,
}

fn panel_key(id: PanelId) -> String {
    match id {
        PanelId::Console => "console".into(),
        PanelId::Connection => "connection".into(),
        PanelId::Stats => "stats".into(),
        PanelId::Display(stream) => format!("display:{}", stream - DESKTOP_STREAM + 1),
    }
}

/// The id and the subject of the request that a button of the hand menu makes from the Mac. A
/// button that opens a page of the Mac's settings has none: the Mac shows its pages itself.
fn request_of(action: MenuAction) -> Option<(&'static str, Option<String>)> {
    let display = |stream: u8| Some((stream - DESKTOP_STREAM + 1).to_string());
    match action {
        MenuAction::TogglePanel(id) => Some(("headset.panel", Some(panel_key(id)))),
        MenuAction::ToggleHand(stream) => Some(("headset.hand", display(stream))),
        MenuAction::RemoveDisplay(stream) => Some(("headset.remove", display(stream))),
        MenuAction::AddDisplay => Some(("headset.add_display", None)),
        MenuAction::Recenter => Some(("headset.recenter", None)),
        MenuAction::OpenControls => Some(("headset.controls", None)),
        MenuAction::OpenPage(_) => None,
    }
}

/// The headset's page from the hand menu's rows, made with `MenuInput::costs` off.
pub fn page(rows: &[MenuRow]) -> Page {
    let mut items = vec![Item::Heading { text: "In the headset".into() }];
    for row in rows {
        let buttons: Vec<Button> = row
            .buttons
            .iter()
            .filter_map(|button| {
                let (id, subject) = request_of(button.action)?;
                let mut made = Button::run(button.label.clone(), id).styled(match button.kind {
                    ButtonKind::Normal => Style::Normal,
                    ButtonKind::On => Style::Primary,
                    ButtonKind::Danger => Style::Danger,
                });
                if let Some(subject) = subject {
                    made = made.of(subject);
                }
                Some(made)
            })
            .collect();
        if buttons.is_empty() {
            continue;
        }
        items.push(Item::Buttons(Buttons { title: row.title.clone(), detail: row.details.join(" "), buttons, hint: String::new() }));
    }
    items.push(Item::Buttons(Buttons {
        buttons: vec![
            Button::run("Open the hand menu", "headset.menu"),
            Button::run("Disconnect", "headset.disconnect").styled(Style::Danger),
            Button::run("Close the headset app", "headset.exit").styled(Style::Danger),
        ],
        hint: "These buttons work the headset's own panels: the same as the hand menu in the headset. Disconnect ends the stream. The headset connects again only when someone presses its Mac in the headset.".into(),
        ..Default::default()
    }));
    Page { id: PAGE.into(), title: "In the headset".into(), short: "Headset".into(), listed: false, items }
}

/// What a press of the Mac asks for, when the request names a button of the headset's page.
pub fn remote(request: &Request) -> Option<Remote> {
    let Request::Run { id, subject } = request else { return None };
    let display = || subject.as_deref()?.parse::<u8>().ok().filter(|n| (1..=crate::displays::SLOTS as u8).contains(n)).map(|n| DESKTOP_STREAM + n - 1);
    Some(match id.as_str() {
        "headset.panel" => Remote::Menu(MenuAction::TogglePanel(match subject.as_deref()? {
            "console" => PanelId::Console,
            "connection" => PanelId::Connection,
            "stats" => PanelId::Stats,
            other => PanelId::Display(DESKTOP_STREAM + other.strip_prefix("display:")?.parse::<u8>().ok().filter(|n| (1..=crate::displays::SLOTS as u8).contains(n))? - 1),
        })),
        "headset.hand" => Remote::Menu(MenuAction::ToggleHand(display()?)),
        "headset.remove" => Remote::Menu(MenuAction::RemoveDisplay(display()?)),
        "headset.add_display" => Remote::Menu(MenuAction::AddDisplay),
        "headset.recenter" => Remote::Menu(MenuAction::Recenter),
        "headset.controls" => Remote::Menu(MenuAction::OpenControls),
        "headset.menu" => Remote::OpenMenu,
        "headset.disconnect" => Remote::Disconnect,
        "headset.exit" => Remote::Exit,
        _ => return None,
    })
}

#[cfg(test)]
#[path = "tests/headset_page.rs"]
mod tests;
