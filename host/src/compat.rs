//! A notice when the headset's ALVR app speaks another protocol version than this host. ALVR
//! compares only the major version, so any 20.x client works with this host, which is built on
//! ALVR v20.14.1. A v21 nightly build or an old v19 app does not connect. ALVR only logs a
//! warning about it. This module turns that warning into a `network` status line.

use alvr_common::LogSeverity;
use alvr_events::EventType;
use alvr_server_core::LOGGING_EVENTS_SENDER;
use serde_json::{json, Value};
use std::thread;
use tokio::sync::broadcast::error::RecvError;

/// Starts watching ALVR's warnings. Call it after `init_logging`. The status line comes once:
/// discovery repeats the warning every second.
pub fn start() {
    let mut events = LOGGING_EVENTS_SENDER.subscribe();
    thread::Builder::new()
        .name("compat".into())
        .spawn(move || loop {
            let event = match events.blocking_recv() {
                Ok(event) => event,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            };
            let EventType::Log(entry) = event.event_type else { continue };
            if entry.severity != LogSeverity::Warning {
                continue;
            }
            if let Some(line) = status_line(&entry.content) {
                crate::status::emit(line);
                return;
            }
        })
        .expect("compat thread");
}

/// The status line for one of ALVR's warnings about a client of another version.
fn status_line(warning: &str) -> Option<Value> {
    let lower = warning.to_lowercase();
    let incompatible = lower.contains("incompatible client") || lower.contains("client is incompatible");
    let old = lower.contains("found old client");
    (incompatible || old).then(|| {
        json!({
            "type": "network",
            "problem": "incompatible_client",
            "message": "The headset runs an ALVR app of another version. cranvr works with ALVR 20.x: \
                        install ALVR from the Meta store, or the v20.14.1 APK from the ALVR releases \
                        on GitHub. A v21 nightly build does not connect.",
            "detail": warning,
        })
    })
}

#[cfg(test)]
#[path = "tests/compat.rs"]
mod tests;
