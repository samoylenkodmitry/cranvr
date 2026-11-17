//! What the app remembers and what it takes from its launch.
//!
//! The host address lives in Cranpose's preferences service. The launch arguments come from
//! Cranpose's `launch_args`: the extras of the launching intent on Android
//! (`adb shell am start ... --es host 192.168.1.20:9950 --ez connect true`) and the command line
//! on the desktop (`--host=192.168.1.20:9950 --connect`).

use crate::hub::MediaChoice;
use cranpose::{launch_args, preferences};
use cranvr_proto::identity::{from_hex, random_bytes, to_hex, Identity};
use std::sync::OnceLock;

const HOST_KEY: &str = "host";
const IDENTITY_KEY: &str = "identity";
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

/// Whether the launch asks for the console open and kept open, with `--console` or the extra
/// `console`. A headset on a desk has no controller that could hold the menu button.
pub fn console_on_launch() -> bool {
    launch_args().boolean("console").unwrap_or(false)
}

/// How the media should travel, from the launch argument `media` (`udp` or `tcp`).
pub fn media_choice() -> MediaChoice {
    match launch_args().string("media") {
        Some("udp") => MediaChoice::Udp,
        Some("tcp") => MediaChoice::Tcp,
        _ => MediaChoice::Auto,
    }
}

/// The preferences key under which the headset remembers the key of a host, by the name that
/// `connection::host_name` gives.
fn host_key_entry(name: &str) -> String {
    format!("host-key:{name}")
}

/// The key of the host that this headset paired with at `name`, if it did.
pub fn host_key(name: &str) -> Option<[u8; 32]> {
    preferences().get(&host_key_entry(name)).and_then(|text| from_hex(&text))
}

/// Remembers the key of a host. The headset refuses a host at this name that has another key.
pub fn remember_host_key(name: &str, key: &[u8; 32]) {
    if let Err(e) = preferences().set(&host_key_entry(name), &to_hex(key)) {
        log::warn!("the host's key was not saved, so the next connection trusts the host's key again: {e}");
    }
}

/// Forgets the key of the host at `name`. The next connection trusts the key that the host shows
/// and remembers it.
pub fn forget_host_key(name: &str) {
    if let Err(e) = preferences().remove(&host_key_entry(name)) {
        log::warn!("the host's key was not forgotten: {e}");
    }
}

/// The key of this headset, for pairing with a host. The first call reads it from Cranpose's
/// preferences service, or makes a new one and saves it there. The host remembers the public key
/// of a paired headset, so a headset keeps its key for good.
pub fn identity() -> &'static Identity {
    static IDENTITY: OnceLock<Identity> = OnceLock::new();
    IDENTITY.get_or_init(|| {
        if let Some(seed) = preferences().get(IDENTITY_KEY).and_then(|text| from_hex(&text)) {
            return Identity::from_seed(seed);
        }
        let seed = random_bytes().unwrap_or_else(|e| {
            // Without the system's random numbers the key is weak. The host's code check still
            // holds. The headset then pairs, but its key is as strong as the clock.
            log::warn!("no random numbers for the pairing key: {e}");
            let mut seed = [0u8; 32];
            for chunk in seed.chunks_mut(8) {
                let bits = std::hash::BuildHasher::hash_one(&std::collections::hash_map::RandomState::new(), now_nanos());
                chunk.copy_from_slice(&bits.to_le_bytes());
            }
            seed
        });
        if let Err(e) = preferences().set(IDENTITY_KEY, &to_hex(&seed)) {
            log::warn!("the pairing key was not saved, so the host will see a new headset at the next start: {e}");
        }
        Identity::from_seed(seed)
    })
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}
