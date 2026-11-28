//! What the app remembers and what it takes from its launch.
//!
//! The host address lives in Cranpose's preferences service. The launch arguments come from
//! Cranpose's `launch_args`: the extras of the launching intent on Android
//! (`adb shell am start ... --es host 192.168.1.20:9950 --ez connect true`) and the command line
//! on the desktop (`--host=192.168.1.20:9950 --connect`).

use crate::console::{Interface, Room, Theme, Volume};
use crate::hub::MediaChoice;
use cranpose::{launch_args, preferences};
use cranvr_common::controls::{Controls, Hand};
use cranvr_proto::identity::{fingerprint, from_hex, to_hex, Identity, FINGERPRINT_LEN};
use std::sync::{Mutex, OnceLock, PoisonError};

const HOST_KEY: &str = "host";
const IDENTITY_KEY: &str = "identity";
const ROOM_KEY: &str = "room";
const VOLUME_KEY: &str = "volume";
const CONTROLS_KEY: &str = "controls";
const INTERFACE_KEY: &str = "interface";
const THEME_KEY: &str = "theme";
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

/// What shows behind the panels while no game runs. The launch argument `room` (`scene` or
/// `passthrough`) decides for one launch and saves nothing: a test that wants the host's picture on
/// display uses it. Without it the console's Room setting counts, as it saved it. A missing or
/// unknown value gives the passthrough.
pub fn room() -> Room {
    if let Some(room) = launch_args().string("room").and_then(Room::from_key) {
        return room;
    }
    if bare_room() {
        return Room::Scene;
    }
    preferences().get(ROOM_KEY).and_then(|text| Room::from_key(&text)).unwrap_or_default()
}

/// Whether the launch asks for nothing behind the panels (`room=none`), for a screen capture: the
/// app's black background shows, with neither the room nor the host's picture.
pub fn bare_room() -> bool {
    launch_args().string("room") == Some("none")
}

/// Whether the choices that a Mac sends are saved for the time without a link. The launch extra
/// `save_choices=false` keeps the saved ones as they are, for a test with a test host on the user's
/// headset: the user's own host sends the user's own choices.
fn saves_choices() -> bool {
    launch_args().boolean("save_choices").unwrap_or(true)
}

pub fn remember_room(room: Room) {
    if !saves_choices() {
        return;
    }
    if let Err(e) = preferences().set(ROOM_KEY, room.key()) {
        log::warn!("the room setting was not saved: {e}");
    }
}

/// How loud the sound plays, as the console's Volume setting saved it. A missing or unknown value
/// gives 150%.
pub fn volume() -> Volume {
    preferences().get(VOLUME_KEY).and_then(|text| Volume::from_key(&text)).unwrap_or_default()
}

pub fn remember_volume(volume: Volume) {
    if !saves_choices() {
        return;
    }
    if let Err(e) = preferences().set(VOLUME_KEY, &volume.key()) {
        log::warn!("the volume setting was not saved: {e}");
    }
}

/// How the headset draws its menus. The launch argument `interface` (`3d` or `flat`) decides for one
/// launch and saves nothing, for a test. Without it the saved choice counts. A missing or unknown
/// value gives 3D.
pub fn interface() -> Interface {
    if let Some(interface) = launch_args().string("interface").and_then(Interface::from_key) {
        return interface;
    }
    preferences().get(INTERFACE_KEY).and_then(|text| Interface::from_key(&text)).unwrap_or_default()
}

/// Whether the launch fixes the interface for a test.
pub fn interface_fixed() -> bool {
    launch_args().string("interface").is_some()
}

pub fn remember_interface(interface: Interface) {
    if !saves_choices() || interface_fixed() {
        return;
    }
    if let Err(e) = preferences().set(INTERFACE_KEY, interface.key()) {
        log::warn!("the interface setting was not saved: {e}");
    }
}

/// The colors of the 3D menus. The launch argument `theme` (`cyan` or `amber`) decides for one launch
/// and saves nothing, for a test. Without it the saved choice counts. A missing or unknown value
/// gives cyan.
pub fn theme() -> Theme {
    if let Some(theme) = launch_args().string("theme").and_then(Theme::from_key) {
        return theme;
    }
    preferences().get(THEME_KEY).and_then(|text| Theme::from_key(&text)).unwrap_or_default()
}

/// Whether the launch fixes the theme for a test.
pub fn theme_fixed() -> bool {
    launch_args().string("theme").is_some()
}

pub fn remember_theme(theme: Theme) {
    if !saves_choices() || theme_fixed() {
        return;
    }
    if let Err(e) = preferences().set(THEME_KEY, theme.key()) {
        log::warn!("the theme setting was not saved: {e}");
    }
}

/// What the buttons of the controllers did when a Mac last sent the mapping. The Mac's app keeps the
/// mapping (`app.json`). The headset keeps this copy for the time without a link. `None` before any
/// link: the standard mapping then holds.
pub fn controls() -> Option<Controls> {
    preferences().get(CONTROLS_KEY).and_then(|text| Controls::from_json(&text))
}

pub fn remember_controls(controls: &Controls) {
    if !saves_choices() {
        return;
    }
    if let Err(e) = preferences().set(CONTROLS_KEY, &controls.to_json()) {
        log::warn!("the controls were not saved: {e}");
    }
}

/// Whether the launch asks for the connect and stats panels hidden for this launch, with
/// `--panels=hidden` or the extra `panels`. A headset on a desk has no controller that could press
/// the ball, and a test of the beams and the models needs the panels hidden. It saves nothing.
pub fn panels_hidden_on_launch() -> bool {
    launch_args().string("panels") == Some("hidden")
}

/// The addresses whose host keys the launch asks to forget, with `--forget_host_key=ADDRESS,ADDRESS` or
/// the extra `forget_host_key`. A test that connected this headset to a test host on a network address
/// leaves the key of that host behind: the headset then refuses the next test host on that address.
/// Each address is as the connect panel shows it, with its port.
pub fn host_keys_to_forget_on_launch() -> Vec<String> {
    launch_args().string("forget_host_key").map(split_addresses).unwrap_or_default()
}

fn split_addresses(text: &str) -> Vec<String> {
    text.split(',').map(str::trim).filter(|address| !address.is_empty()).map(str::to_owned).collect()
}

/// Whether the launch asks to connect at once, with `--connect` or the extra `connect`.
pub fn connect_on_launch() -> bool {
    launch_args().boolean("connect").unwrap_or(false)
}

/// Whether the launch asks for the console open and kept open, with `--console` or the extra
/// `console` true, or closed until a press opens it, with the extra `console` false. A headset on a
/// desk has no controller that could hold the menu button, and a capture of the hand menu needs the
/// console out of the way. `None` without the extra: the console opens with the first menu.
pub fn console_on_launch() -> Option<bool> {
    launch_args().boolean("console")
}

/// The host's page that the console opens on at the launch, with `--console_page=stream` or the
/// extra `console_page`: a test or a capture of a page needs it open with no press.
pub fn console_page_on_launch() -> Option<String> {
    launch_args().string("console_page").map(str::to_owned)
}

/// Whether the launch asks for the console to open on the Controls page, with `--controls_page=left`
/// or `--controls_page=right` or the extra `controls_page`. A test on a desk has no controller that
/// could press the console's Controls button, and a screen capture of the page needs it open.
pub fn controls_page_on_launch() -> Option<Hand> {
    match launch_args().string("controls_page") {
        Some("left") => Some(Hand::Left),
        Some("right") => Some(Hand::Right),
        _ => None,
    }
}

/// Whether the launch asks for the address field open at the start, with `--address_entry` or the
/// extra `address_entry`. A test that types into the field needs it there without a press.
pub fn address_entry_on_launch() -> bool {
    launch_args().boolean("address_entry").unwrap_or(false)
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

/// The preferences key under which the headset finds the key of a host again by its fingerprint,
/// after the Mac's address changed.
fn host_fingerprint_entry(fingerprint: &[u8; FINGERPRINT_LEN]) -> String {
    format!("host-fingerprint:{}", to_hex(fingerprint))
}

/// The key of the host that this headset paired with at `name`, if it did.
pub fn host_key(name: &str) -> Option<[u8; 32]> {
    preferences().get(&host_key_entry(name)).and_then(|text| from_hex(&text))
}

/// The key of the host that this headset paired with and that has this fingerprint, if it did.
/// A key whose fingerprint differs is not returned, so a damaged entry pins nothing.
pub fn host_key_by_fingerprint(fingerprint_of_host: &[u8; FINGERPRINT_LEN]) -> Option<[u8; 32]> {
    let key = preferences().get(&host_fingerprint_entry(fingerprint_of_host)).and_then(|text| from_hex(&text))?;
    (fingerprint(&key) == *fingerprint_of_host).then_some(key)
}

/// Remembers the key of a host under its address and under its fingerprint. The headset refuses a
/// host at this address, or with this fingerprint, that has another key.
pub fn remember_host_key(name: &str, key: &[u8; 32]) {
    let saved = preferences()
        .set(&host_key_entry(name), &to_hex(key))
        .and_then(|()| preferences().set(&host_fingerprint_entry(&fingerprint(key)), &to_hex(key)));
    if let Err(e) = saved {
        log::warn!("the host's key was not saved, so the next connection trusts the host's key again: {e}");
    }
}

/// Forgets the key of the host at `name`, under its address and under its fingerprint. The next
/// connection trusts the key that the host shows and remembers it.
pub fn forget_host_key(name: &str) {
    if let Some(key) = host_key(name) {
        if let Err(e) = preferences().remove(&host_fingerprint_entry(&fingerprint(&key))) {
            log::warn!("the host's fingerprint was not forgotten: {e}");
        }
    }
    if let Err(e) = preferences().remove(&host_key_entry(name)) {
        log::warn!("the host's key was not forgotten: {e}");
    }
}

/// The key of this headset, for pairing with a host. The first call that succeeds reads it from
/// Cranpose's preferences service, or makes a new one from the system's random numbers and saves it
/// there. The host remembers the public key of a paired headset, so a headset keeps its key for
/// good. Without random numbers the call fails and makes no key.
pub fn identity() -> std::io::Result<&'static Identity> {
    static IDENTITY: OnceLock<Identity> = OnceLock::new();
    // Two first calls at once must not make two keys and save the second one
    static MAKING: Mutex<()> = Mutex::new(());
    let _making = MAKING.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(identity) = IDENTITY.get() {
        return Ok(identity);
    }
    let identity = match preferences().get(IDENTITY_KEY).and_then(|text| from_hex(&text)) {
        Some(seed) => Identity::from_seed(seed),
        None => {
            let identity = Identity::generate()?;
            if let Err(e) = preferences().set(IDENTITY_KEY, &to_hex(&identity.seed())) {
                log::warn!("the pairing key was not saved, so the host will see a new headset at the next start: {e}");
            }
            identity
        }
    };
    Ok(IDENTITY.get_or_init(|| identity))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launch_names_the_addresses_to_forget_in_a_comma_list() {
        assert_eq!(split_addresses("192.168.1.20:9961, 192.168.1.20:9962,,"), vec!["192.168.1.20:9961", "192.168.1.20:9962"]);
        assert!(split_addresses("  ").is_empty());
    }
}
