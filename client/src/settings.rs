//! What the app remembers and what it takes from its launch.
//!
//! The host address lives in Cranpose's preferences service. The launch arguments come from
//! Cranpose's `launch_args`: the extras of the launching intent on Android
//! (`adb shell am start ... --es host 192.168.1.20:9950 --ez connect true`) and the command line
//! on the desktop (`--host=192.168.1.20:9950 --connect`).

use crate::hub::MediaChoice;
use cranpose::{launch_args, preferences};

const HOST_KEY: &str = "host";
/// The address that works over USB with `adb reverse tcp:9950 tcp:9950`, and on the same Mac
const DEFAULT_HOST: &str = "127.0.0.1:9950";

/// The address for the text field: the launch argument, else the last address that was used, else
/// the default.
pub fn initial_host() -> String {
    let args = launch_args();
    if let Some(host) = args.string("host") {
        return host.to_owned();
    }
    preferences().get(HOST_KEY).filter(|host| !host.trim().is_empty()).unwrap_or_else(|| DEFAULT_HOST.to_owned())
}

/// Keeps the address for the next launch.
pub fn remember_host(host: &str) {
    if let Err(e) = preferences().set(HOST_KEY, host.trim()) {
        log::warn!("the host address was not saved: {e}");
    }
}

/// Whether the launch asks to connect at once, with `--connect` or the extra `connect`.
pub fn connect_on_launch() -> bool {
    launch_args().boolean("connect").unwrap_or(false)
}

/// How long the sender holds each tracking sample before it sends it, from the launch argument
/// `hold_ms`. The default is 0. A hold moves the frames' arrival to the middle of the headset's
/// frame interval, so fewer arrive in pairs. See the README.
pub fn tracking_hold() -> std::time::Duration {
    let ms = launch_args().string("hold_ms").and_then(|text| text.parse::<f64>().ok()).unwrap_or(0.0);
    std::time::Duration::from_secs_f64(ms.max(0.0) / 1000.0)
}

/// How the media should travel, from the launch argument `media` (`udp` or `tcp`).
pub fn media_choice() -> MediaChoice {
    match launch_args().string("media") {
        Some("udp") => MediaChoice::Udp,
        Some("tcp") => MediaChoice::Tcp,
        _ => MediaChoice::Auto,
    }
}
