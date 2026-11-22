//! The window draws the model's pages (see pages.rs): a headless shell composes every page and the
//! test reads the words on the screen. A made-up item shows that the window draws any item that a
//! page holds, with no code of its own for it. The test presses nothing: a press saves `app.json`.

use super::{PageItems, Settings};
use crate::{backend::Model, pages, steam::Game};
use cranpose::prelude::*;
use cranpose_testing::robot::{RobotTestRule, TestRenderer};
use cranvr_common::menu::{Button, Buttons, Choice, Item, Opt, Page, Style, Toggle};
use std::path::PathBuf;

fn words_of(item: &Item) -> Vec<String> {
    match item {
        Item::Heading { text } | Item::Text { text, .. } => vec![text.clone()],
        Item::Choice(choice) => std::iter::once(choice.label.clone()).chain(choice.options.iter().map(|option| option.label.clone())).collect(),
        Item::Toggle(toggle) => vec![toggle.label.clone()],
        Item::Buttons(row) => std::iter::once(row.title.clone()).chain(row.buttons.iter().map(|button| button.words().to_string())).filter(|words| !words.is_empty()).collect(),
    }
}

fn drawn(pages: Vec<Page>, settings: Settings, model: Model) -> Vec<String> {
    let mut robot = RobotTestRule::new(1400, 6000, TestRenderer::default(), move || {
        let (settings, model) = (settings.clone(), model.clone());
        let settings = rememberMutableStateOf(move || settings.clone());
        let model = rememberMutableStateOf(move || model.clone());
        for page in pages.clone() {
            PageItems(page.items, settings, model);
        }
    });
    robot.get_all_text()
}

#[test]
fn the_window_draws_every_item_of_every_page() {
    let games = vec![
        Game { app_id: "450390".into(), name: "The Lab".into(), exe: Some(PathBuf::from("/Users/x/The Lab/TheLab.exe")), ..Default::default() },
        Game { name: "Plain".into(), exe: Some(PathBuf::from("/Users/x/Plain/plain.exe")), ..Default::default() },
    ];
    let model = Model { games, learned_sizes: vec![("TheLab.exe".into(), 0.85)], ..Default::default() };
    let settings = Settings::default();
    let pages = pages::pages(&settings, &model);
    assert!(pages.len() >= 4, "the play page, the stream page and a page for each game");
    let texts = drawn(pages.clone(), settings, model);
    for page in &pages {
        for item in &page.items {
            for words in words_of(item) {
                assert!(texts.iter().any(|text| text == &words), "{} lacks {words:?} in {texts:?}", page.id);
            }
        }
    }
}

#[test]
fn a_made_up_item_shows_in_the_window_with_no_code_of_its_own() {
    let page = Page {
        id: "new".into(),
        title: "New".into(),
        items: vec![
            Item::Choice(Choice { id: "new.choice".into(), label: "A setting that no window knows".into(), options: vec![Opt::new("a", "First way"), Opt::new("b", "Second way")], value: "a".into(), ..Default::default() }),
            Item::Toggle(Toggle { id: "new.toggle".into(), label: "A new switch".into(), on: true, ..Default::default() }),
            Item::Buttons(Buttons { title: "A new action".into(), buttons: vec![Button::run("Do it", "new.action").styled(Style::Danger)], ..Default::default() }),
        ],
        ..Default::default()
    };
    let texts = drawn(vec![page], Settings::default(), Model::default());
    for words in ["A setting that no window knows", "First way", "Second way", "A new switch", "A new action", "Do it"] {
        assert!(texts.iter().any(|text| text == words), "{words:?} in {texts:?}");
    }
}

#[test]
fn the_settings_tab_holds_the_connection_the_stream_the_sound_the_headset_and_the_mac() {
    let model = Model::default();
    let settings = Settings::default();
    let mut robot = RobotTestRule::new(1400, 6000, TestRenderer::default(), {
        let (settings, model) = (settings.clone(), model.clone());
        move || {
            let (settings, model) = (settings.clone(), model.clone());
            let settings = rememberMutableStateOf(move || settings.clone());
            let model = rememberMutableStateOf(move || model.clone());
            super::GeneralSettings(settings, model);
        }
    });
    let texts = robot.get_all_text();
    let pages = pages::pages(&settings, &model);
    for id in [pages::CONNECTION, pages::STREAM, pages::SOUND, pages::HEADSET, pages::MAC] {
        let page = pages.iter().find(|page| page.id == id).unwrap();
        for item in &page.items {
            for words in words_of(item) {
                assert!(texts.iter().any(|text| text == &words), "the Settings tab lacks {words:?} of {id}");
            }
        }
    }
    // The headset's room and volume, which the Mac keeps for the headset
    assert!(texts.iter().any(|text| text == "Room") && texts.iter().any(|text| text == "Volume"), "{texts:?}");
}
