//! Status lines for a GUI that runs the host as a child process (`--status`). Each line on
//! stdout reads `@cranvr ` followed by one JSON object with a "type" field:
//! - `headset`: connected, and when connected: eye [w, h], fps, foveated, ip (the headset's address:
//!   loopback means that the stream runs over the USB cable)
//! - `game`: connected, and when connected: name (the game's renderer) and program (its
//!   program's path, or null)
//! - `desktop`: ok, message, and when ok: display (its ID) and virtual (whether it is the
//!   virtual display)
//! - `virtual_display`: ok false and message, when macOS refused the virtual display
//! - `vr_display`: main (true when the virtual display became the main display at the first
//!   pose of a connection, false when it stopped at the end), and display
//! - `audio`: ok, and message when not ok: the capture of the Mac's sound for the headset
//! - `stats`: fps, game_frames, render_ms, encode_ms, mbps, audio_s: seconds of the Mac's sound
//!   captured for the headset (every 5 seconds), and latency: the averages of the headset's
//!   frame timing reports since the last line (see `Headset::take_stats`), or null. The headset
//!   reports no timing while it shows no stream: with its Room setting at Passthrough and no game
//! - `network`: problem ("no_poses" with protocol "udp" or "tcp", "vpn_route" or
//!   "incompatible_client": a headset whose cranvr app has another protocol version than the
//!   host's, with the side to update in the message), and message
//! - `native`: at the start and after each change of the pairings: port,
//!   paired (the names of the paired headsets) and pending (the headsets that wait for the user:
//!   key in hex, name, ip and code). The user's answer comes back on the host's input as
//!   `allow KEY` or `deny KEY`.
//! - `black_eye`: eye and program (the game's program path, or null). Eye "right": the game's right
//!   eye stayed black for several seconds while its left eye showed a picture. Eye "both": both
//!   eyes stayed black for 90 seconds while the game delivered frames (see eye_check.rs). The host
//!   sends it once for each game run.
//! - `vr_request`: a button of the console in VR (see desktop_layer.rs): action "play" with game,
//!   "stop", or "set" with one of fps, game_scale, msaa
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

/// Whether the host prints status lines, which means that it runs as the cranvr app's child. Such
/// a host owns its input: the app writes the user's answers there.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

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

/// Hands a console button's request, a JSON object, to the cranvr app as a `vr_request` status
/// line. Returns whether the text was such an object.
pub fn console_request(text: &str) -> bool {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(mut request)) => {
            request.insert("type".into(), "vr_request".into());
            emit(Value::Object(request));
            true
        }
        _ => false,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_console_request_becomes_a_status_line() {
        assert!(console_request(r#"{"action":"set","fps":90}"#));
        let line = latest("vr_request").unwrap();
        assert_eq!((line["action"].as_str(), line["fps"].as_u64()), (Some("set"), Some(90)));
        // Anything but an object is no request
        assert!(!console_request("not json") && !console_request("[1]"));
    }
}
