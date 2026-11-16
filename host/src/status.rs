//! Status lines for a GUI that runs the host as a child process (`--status`). Each line on
//! stdout reads `@cranvr ` followed by one JSON object with a "type" field:
//! - `headset`: connected, and when connected: eye [w, h], fps, foveated
//! - `game`: connected, and when connected: name
//! - `desktop`: ok, message, and when ok: display (its ID) and virtual (whether it is the
//!   virtual display)
//! - `virtual_display`: ok false and message, when macOS refused the virtual display
//! - `vr_display`: main (true when the virtual display became the main display at the first
//!   pose of a connection, false when it stopped at the end), and display
//! - `audio`: ok, and message when not ok: the capture of the Mac's sound for the headset
//! - `stats`: fps, game_frames, render_ms, encode_ms, mbps, audio_s: seconds of the Mac's sound
//!   captured for the headset (every 5 seconds), and latency: the
//!   headset's reports since the last line, see `latency::take`, or null
//! - `network`: problem ("no_poses" with protocol "udp" or "tcp", "vpn_route", or
//!   "incompatible_client" with detail: ALVR's warning), and message

use serde_json::Value;
use std::{
    io::Write,
    sync::atomic::{AtomicBool, Ordering},
};

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Turns the status lines on. The host then runs as the cranvr app's child, so it also ends
/// when the app ends: macOS gives an orphaned process to launchd (pid 1), and the app cannot
/// stop its host when the user quits it from the Dock.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
    let parent = std::os::unix::process::parent_id();
    std::thread::Builder::new()
        .name("parent-watch".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if std::os::unix::process::parent_id() != parent {
                std::process::exit(0);
            }
        })
        .ok();
}

pub fn emit(value: Value) {
    if ENABLED.load(Ordering::Relaxed) {
        let mut out = std::io::stdout().lock();
        writeln!(out, "@cranvr {value}").ok();
        out.flush().ok();
    }
}
