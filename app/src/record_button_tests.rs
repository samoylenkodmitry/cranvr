//! The window's Record button: a headless shell draws it in each state of the recording and clicks
//! it. A click in a test counts in `recorder::TOGGLES` and starts no recording.

use super::{RecordButton, RecordingNotes};
use crate::{
    backend::{Model, Settings},
    recorder::{View, TOGGLES},
};
use cranpose::prelude::*;
use cranpose_testing::robot::{RobotTestRule, TestRenderer};
use std::{path::PathBuf, sync::atomic::Ordering, time::Instant};

fn window(model: Model) -> RobotTestRule<TestRenderer> {
    let mut robot = RobotTestRule::new(700, 300, TestRenderer::default(), move || {
        let model = model.clone();
        let settings = rememberMutableStateOf(Settings::default);
        let model = rememberMutableStateOf(move || model.clone());
        Column(Modifier::empty(), ColumnSpec::default(), move || {
            RecordButton(settings, model);
            RecordingNotes(model);
        });
    });
    robot.wait_for_idle();
    robot
}

// The one test that clicks: the count of clicks is the process's own
#[test]
fn the_record_button_toggles_the_recording_and_shows_its_state() {
    let before = TOGGLES.load(Ordering::Relaxed);
    let mut idle = window(Model::default());
    assert!(idle.find_by_text("Record what I see").click(), "{:?}", idle.get_all_text());
    idle.wait_for_idle();
    assert_eq!(TOGGLES.load(Ordering::Relaxed), before + 1, "a click on the idle button starts a recording");

    let running = Model { recording: View { since: Some(Instant::now()), ..Default::default() }, recording_seconds: 65, ..Default::default() };
    let mut running = window(running);
    assert!(running.find_by_text("Stop recording (1:05)").click(), "{:?}", running.get_all_text());
    running.wait_for_idle();
    assert_eq!(TOGGLES.load(Ordering::Relaxed), before + 2, "a click on the running button stops it");

    let saving = window(Model { recording: View { saving: true, ..Default::default() }, ..Default::default() }).get_all_text();
    assert!(saving.iter().any(|text| text == "Saving the recording..."), "{saving:?}");
    assert!(!saving.iter().any(|text| text == "Record what I see"), "no button while the file is made: {saving:?}");

    let blocked = View { blocked: Some("Recording needs the headset on the USB cable with USB debugging".into()), ..Default::default() };
    let texts = window(Model { recording: blocked, ..Default::default() }).get_all_text();
    assert!(texts.iter().any(|text| text == "Recording needs the headset on the USB cable with USB debugging"), "{texts:?}");

    let saved = Model { saved: Some((PathBuf::from("/Users/x/Movies/cranvr/cranvr-2026-11-27-140000.mp4"), Instant::now())), ..Default::default() };
    let texts = window(saved).get_all_text();
    assert!(texts.iter().any(|text| text == "Saved: cranvr-2026-11-27-140000.mp4") && texts.iter().any(|text| text == "Show in Finder"), "{texts:?}");
}
