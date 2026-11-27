use super::{local_stamp, View};
use std::time::Instant;

// The headset's gem learns what a press does: start, stop, or nothing with the reason
#[test]
fn the_headset_hears_the_reason_only_while_a_press_cannot_start_or_stop() {
    let blocked = View { blocked: Some("Recording needs the headset on the USB cable with USB debugging".into()), ..Default::default() };
    assert_eq!(blocked.to_headset()["running"], false);
    assert_eq!(blocked.to_headset()["blocked"], "Recording needs the headset on the USB cable with USB debugging");
    // A running recording always stops at a press, whatever the cable says now
    let running = View { since: Some(Instant::now()), ..blocked.clone() };
    assert_eq!(running.to_headset()["running"], true);
    assert!(running.to_headset()["blocked"].is_null());
    let saving = View { saving: true, ..Default::default() };
    assert_eq!(saving.to_headset()["blocked"], "Saving the last recording");
    let noted = View { note: Some("Saved: cranvr-2026-11-27-101500.mp4".into()), note_id: 3, ..Default::default() };
    assert_eq!(noted.to_headset()["note"], "Saved: cranvr-2026-11-27-101500.mp4");
    assert_eq!(noted.to_headset()["note_id"], 3);
}

// The final file's name holds the local time of the start: cranvr-YYYY-MM-DD-HHMMSS.mp4
#[test]
fn the_stamp_is_the_local_date_and_time() {
    let stamp = local_stamp();
    assert_eq!(stamp.len(), 17, "{stamp}");
    let parts: Vec<&str> = stamp.split('-').collect();
    assert_eq!(parts.iter().map(|part| part.len()).collect::<Vec<_>>(), vec![4, 2, 2, 6], "{stamp}");
    assert!(stamp.chars().all(|c| c.is_ascii_digit() || c == '-'), "{stamp}");
}
