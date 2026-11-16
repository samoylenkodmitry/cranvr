//! Status lines for a GUI that runs the host as a child process (`--status`). Each line on
//! stdout reads `@cranvr ` followed by one JSON object with a "type" field:
//! - `headset`: connected, and when connected: eye [w, h], fps, foveated
//! - `game`: connected, and when connected: name (the game's renderer) and program (its
//!   program's path, or null)
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
//! - `vr_request`: a button of the console in VR (see desktop_layer.rs): action "play" with game,
//!   "stop", or "set" with one of fps, game_scale, msaa, low_latency
//!
//! The host keeps the newest line of each type for the console, also without `--status`.

use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::Write,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
};

/// The newest line of each type
static LATEST: Mutex<BTreeMap<String, Value>> = Mutex::new(BTreeMap::new());
/// vr-menu.json in the config dir: the games and settings that the cranvr app offers in VR
static MENU_FILE: OnceLock<PathBuf> = OnceLock::new();

/// The newest status line of this type.
pub fn latest(kind: &str) -> Option<Value> {
    LATEST.lock().unwrap().get(kind).cloned()
}

pub fn set_menu_file(path: PathBuf) {
    MENU_FILE.set(path).ok();
}

pub fn menu_file() -> Option<&'static PathBuf> {
    MENU_FILE.get()
}

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
    if let Some(kind) = value["type"].as_str() {
        LATEST.lock().unwrap().insert(kind.to_string(), value.clone());
    }
    if ENABLED.load(Ordering::Relaxed) {
        let mut out = std::io::stdout().lock();
        writeln!(out, "@cranvr {value}").ok();
        out.flush().ok();
    }
}
