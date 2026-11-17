//! cranvr's client for headsets, as a Cranpose app. In a Quest its panels stand in the room. The
//! stream is a surface that the hardware decoder writes into. Elsewhere the panels draw flat.
//!
//! The app uses Cranpose's public API and plain Rust crates. The protocol is `cranvr-proto`.
//! `connection.rs` runs the link on its own threads. `hub.rs` hands the results to the UI.
//! `tracking.rs` is the seam for the headset's per-frame data.

// The app has no unsafe code. The Android entry points that `android_main!` expands to carry
// their own expectation of this lint.
#![deny(unsafe_code)]

mod connection;
mod hub;
mod settings;
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
