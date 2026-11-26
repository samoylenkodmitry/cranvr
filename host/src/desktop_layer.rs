//! What the frame loop shares with the headset link's desktop panel, and the console's menu.
//!
//! The headset shows the Mac's displays as panels (`headset/native/desktop.rs`). The eye stream
//! then leaves its own copy of the screen out. `LayerState` carries this fact and whether a game
//! runs between the frame loop and the link.

use crate::status;
use std::sync::atomic::{AtomicBool, AtomicU32};

/// What the frame loop and the panel's link share.
#[derive(Default)]
pub struct LayerState {
    /// A game runs: the link tells the client, which decides which panels to show
    pub game: AtomicBool,
    /// The headset shows the panel: the eye stream leaves the screen out
    pub active: AtomicBool,
    /// Pictures that the link sent since the last stats line
    pub frames: AtomicU32,
}

/// The console's menu: the cranvr app's vr-menu.json, when it holds a JSON object. None without the
/// app's file: the console then stays away.
pub(crate) fn menu() -> Option<String> {
    let text = std::fs::read_to_string(status::menu_file()?).ok()?;
    matches!(serde_json::from_str::<serde_json::Value>(&text), Ok(serde_json::Value::Object(_))).then_some(text)
}

/// The stream's state for the console (`HostMsg::MenuStatus`): the headset's refresh rate, the
/// frames a second that the host sends and that the game makes, the latency, the eye stream's
/// bitrate now and as the user set it (Mbps), and the game's program.
pub(crate) fn menu_status() -> String {
    use serde_json::{json, Value};
    let headset = status::latest("headset").filter(|h| h["connected"] == true);
    let stats = status::latest("stats");
    let game = status::latest("game").filter(|g| g["connected"] == true);
    let round = |v: &Value| v.as_f64().map(|n| (n * 10.0).round() / 10.0);
    json!({
        "refresh": headset.as_ref().and_then(|h| round(&h["fps"])),
        "fps": stats.as_ref().and_then(|s| round(&s["fps"])),
        "game_fps": stats.as_ref().and_then(|s| s["game_frames"].as_f64()).map(|f| f / 5.0),
        "latency_ms": stats.as_ref().and_then(|s| round(&s["latency"]["total_ms"])),
        "mbps": stats.as_ref().and_then(|s| s["mbps"].as_u64()),
        "mbps_set": stats.as_ref().and_then(|s| s["mbps_set"].as_u64()),
        // The game's program file, by which the console finds its name
        "game": game.as_ref().map(|g| match g["program"].as_str() {
            Some(path) => path.rsplit(['\\', '/']).next().unwrap_or(path),
            None => g["name"].as_str().unwrap_or("a game"),
        }),
    })
    .to_string()
}
