//! cranvr's client for headsets, as a Cranpose app. In a Quest its panels stand in the room. The
//! game's picture is a streamed scene under them. Elsewhere the panels draw flat.
//!
//! The app uses Cranpose's public API and plain Rust crates. The protocol is `cranvr-proto`.
//! `connection.rs` runs the link on its own threads. `hub.rs` hands the results to the UI.
//! `tracking.rs` turns the headset's frames into the tracking that the host takes.

// The app has no unsafe code. The Android entry points that `android_main!` expands to carry
// their own expectation of this lint.
#![deny(unsafe_code)]

mod audio;
mod connection;
mod console;
mod console_panel;
mod desktop;
mod hub;
mod place;
mod settings;
mod sizing;
mod tracking;
mod ui;

pub use ui::ClientApp;

cranpose::app_capabilities!();

#[cfg(all(feature = "android", target_os = "android"))]
fn launcher() -> cranpose::AppLauncher {
    cranpose::AppLauncher::new().with_capabilities(&CAPABILITIES)
}

cranpose_xr::android_main! {
    launcher: launcher(),
    content: ClientApp,
}
