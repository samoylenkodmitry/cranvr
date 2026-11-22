//! cranvr's client for headsets, as a Cranpose app. In a Quest its panels stand in the room. The
//! game's picture is a streamed scene under them. Elsewhere the panels draw flat. The `phone`
//! feature builds the app for an Android phone or tablet instead: the screen shows the picture flat
//! and the motion sensors move the host's camera (`phone.rs`).
//!
//! The app uses Cranpose's public API and plain Rust crates. The protocol is `cranvr-proto`.
//! `connection.rs` runs the link on its own threads. `hub.rs` hands the results to the UI.
//! `tracking.rs` turns the headset's frames into the tracking that the host takes. `tracked.rs` does
//! the same for the user's hands, body, face and eyes. `focus.rs` maps the app's lifecycle to the
//! focus that the host hears of: an app without the input focus sends no tracking and no input.
//! `hands.rs` holds the rule for the user's hands between the panels and the game.

// The app has no unsafe code. The Android entry points that `android_main!` expands to carry
// their own expectation of this lint.
#![deny(unsafe_code)]

mod audio;
mod connection;
mod console;
mod console_panel;
mod controls;
mod display_panel;
mod display_wish;
mod displays;
mod discovery;
mod focus;
mod hand_menu;
mod hands;
mod hold;
mod hover;
mod menu_model;
mod microphone;
mod models;
mod hub;
mod panel_layout;
mod phone;
mod place;
#[cfg(all(feature = "scripted-hand", target_os = "android"))]
mod scripted;
mod settings;
mod sizing;
mod tracked;
mod tracking;
mod ui;
mod usb;
mod usb_network;

pub use ui::{ClientApp, PhoneApp};

cranpose::app_capabilities!();

#[cfg(all(feature = "android", target_os = "android"))]
fn launcher() -> cranpose::AppLauncher {
    cranpose::AppLauncher::new().with_capabilities(&CAPABILITIES)
}

#[cfg(all(feature = "phone", target_os = "android"))]
fn phone_launcher() -> cranpose::AppLauncher {
    cranpose::AppLauncher::new().with_capabilities(&CAPABILITIES)
}

// The Quest's APK starts an immersive app. The phone's APK starts a flat one. Each macro exports
// the entry symbols, so a build has exactly one of them.
#[cfg(not(feature = "phone"))]
cranpose_xr::android_main! {
    launcher: launcher(),
    content: ClientApp,
}

#[cfg(feature = "phone")]
cranpose::android_main! {
    launcher: phone_launcher(),
    content: PhoneApp,
}
