use super::Pacer;
use std::{
    thread,
    time::{Duration, Instant},
};

// Poses three times per refresh must not raise the frame rate past the refresh rate.
#[test]
fn poses_faster_than_the_refresh_rate_keep_the_frame_rate() {
    let fps = 100.0;
    let mut pacer = Pacer::new(fps);
    let start = Instant::now();
    let mut frames = 0;
    while start.elapsed() < Duration::from_millis(500) {
        if pacer.ready() {
            pacer.spend();
            frames += 1;
        }
        thread::sleep(Duration::from_secs_f32(1.0 / fps / 3.0));
    }
    // 50 refreshes in half a second, and the first frame from the starting budget
    assert!((49..=52).contains(&frames), "{frames} frames");
}
