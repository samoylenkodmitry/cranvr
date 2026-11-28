//! The cable to the Mac. Two ways run over it. The headset's USB network (`usb_network.rs`) needs
//! no Developer Mode: the Mac answers on its link-local address on the cable's link. `adb reverse`
//! needs Developer Mode: the Mac's cranvr app sets it up for a plugged-in headset, so the Mac's port
//! answers on the headset's own address 127.0.0.1. A thread asks both while no link runs. The USB
//! network wins when both answer, because it needs no adb and carries UDP. When the Mac answers,
//! the connect panel lists "This Mac by USB cable" first.
//!
//! An `adb reverse` listener accepts every connection, also when no host runs on the Mac. Only
//! bytes from the host prove that it is there, so the question is the first part of the handshake
//! (`cranvr_proto::link::probe_host`). The answer gives the host's key. Its fingerprint tells the
//! cable's Mac from the Macs that the Wi-Fi search found.
//!
//! The choice between the cable and Wi-Fi is made by the pure functions of this file. `entries`
//! gives the panel's list. `target` says where a connection goes when it starts, and again when a
//! link that streamed has ended. A cable is preferred to the Wi-Fi entry of the same Mac. A running
//! Wi-Fi link never moves to the cable: nothing here looks at a link that runs.
//!
//! The Mac's user may choose Wi-Fi in the Mac's app. The host then says so at the start of each
//! connection, on both ways over the cable (`HostProbe::cable`). Such a Mac still shows its key
//! behind the cable, so an aim at the cable goes to the same Mac's Wi-Fi entry.

use crate::{
    discovery::FoundHost,
    hub::Hub,
    usb_network::{self, NetworkRoute},
};
use crate::launch;
use cranvr_proto::{
    discovery::DEFAULT_PORT,
    identity::{fingerprint, FINGERPRINT_LEN, KEY_LEN},
    link::{probe_host, probe_host_via, AnyRoute, Route},
};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::OnceLock,
    thread,
    time::Duration,
};

/// How long one question to the cable may take. A cable that is out refuses at once. A host that
/// is busy answers within a few milliseconds.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
/// How often the thread asks while no link runs
const ASK_EVERY: Duration = Duration::from_millis(1000);
/// How often the thread looks again while a link runs
const IDLE: Duration = Duration::from_millis(500);

/// The port of the Mac's host behind the cable. It is the host's default port, which the Mac app
/// uses too. The launch argument `usb_port` changes it, for a test host on another port.
pub fn port() -> u16 {
    static PORT: OnceLock<u16> = OnceLock::new();
    *PORT.get_or_init(|| launch::args().int("usb_port").and_then(|port| u16::try_from(port).ok()).unwrap_or(DEFAULT_PORT))
}

/// The address behind the cable
pub fn address() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port())
}

/// The way over the cable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Way {
    /// The headset's USB network: the Mac's link-local address on the cable's link
    UsbNetwork,
    /// `adb reverse`: the Mac's port on the headset's own address
    AdbReverse,
}

/// A Mac that answered through the cable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsbMac {
    pub address: SocketAddr,
    /// The Mac's key. `None` when the host speaks another protocol version than this app.
    pub key: Option<[u8; KEY_LEN]>,
    pub version: u16,
    pub way: Way,
    /// The Mac's user chose Wi-Fi: the Mac takes no link over the cable. The headset connects to
    /// the same Mac over Wi-Fi.
    pub wifi_only: bool,
}

impl UsbMac {
    /// The start of the hash of the host's key, when the host showed its key
    pub fn fingerprint(&self) -> Option<[u8; FINGERPRINT_LEN]> {
        self.key.map(|key| fingerprint(&key))
    }

    /// The Mac when it takes a link over the cable
    pub fn usable(&self) -> Option<&Self> {
        (!self.wifi_only).then_some(self)
    }

    /// Whether the Mac asks its user for a code before it lets this headset stream. A Mac on the
    /// other end of `adb reverse` trusts the headset. A Mac on the USB network asks for a headset that
    /// it does not know, as over Wi-Fi.
    pub fn needs_code(&self) -> bool {
        self.way == Way::UsbNetwork && self.fingerprint().is_none_or(|fingerprint| crate::settings::host_key_by_fingerprint(&fingerprint).is_none())
    }

    /// The Mac as the Wi-Fi search would list it, for the checks that the two kinds share. The
    /// name is the one that the panel shows.
    pub fn as_found(&self) -> FoundHost {
        FoundHost {
            name: CABLE_NAME.into(),
            address: self.address,
            fingerprint: self.fingerprint().unwrap_or([0; FINGERPRINT_LEN]),
            version: self.version,
            paired: false,
        }
    }
}

/// What the panel calls the Mac behind the cable
pub const CABLE_NAME: &str = "This Mac by USB cable";

/// Asks the cable: the USB network first, then the address of `adb reverse`. `None` when no host
/// answers on either.
pub fn probe() -> Option<UsbMac> {
    probe_usb_network().or_else(|| probe_at(&AnyRoute, address(), Way::AdbReverse))
}

/// Asks the cable as [`probe`] does, for a connection that starts now. A USB network that came up a
/// moment ago may not reach the Mac yet: the question then repeats until the Mac answers on it or
/// the moment is over.
pub fn probe_settled() -> Option<UsbMac> {
    loop {
        let found = probe();
        if found.as_ref().is_some_and(|mac| mac.way == Way::UsbNetwork) || !usb_network::just_up() {
            return found;
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// The host on the USB network that listens on the cable's port
fn probe_usb_network() -> Option<UsbMac> {
    let network = usb_network::network()?;
    let (address, _) = usb_network::find_hosts(&network).into_iter().find(|(address, _)| address.port() == port())?;
    probe_at(&NetworkRoute(network), address, Way::UsbNetwork)
}

fn probe_at(route: &dyn Route, address: SocketAddr, way: Way) -> Option<UsbMac> {
    match probe_host_via(route, address, PROBE_TIMEOUT) {
        Ok(probe) => Some(UsbMac { address, key: probe.key, version: probe.version, way, wifi_only: !probe.cable }),
        Err(e) => {
            log::debug!("nothing answers behind the cable at {address}: {e}");
            None
        }
    }
}

/// The fingerprint of the host at `address`, asked the same way. It tells whether a typed address
/// belongs to the Mac behind the cable.
pub fn identify(address: SocketAddr) -> Option<[u8; FINGERPRINT_LEN]> {
    probe_host(address, PROBE_TIMEOUT).ok()?.key.map(|key| fingerprint(&key))
}

/// Starts the thread that asks the cable, once for the process. It asks only while no link runs and
/// only on a headset or a phone: a flat window beside the host would otherwise find the host on
/// the machine and call it a cable.
pub fn start(hub: &'static Hub) {
    static STARTED: std::sync::Once = std::sync::Once::new();
    // Read the launch argument on the caller's thread, which the launch intent belongs to
    port();
    STARTED.call_once(|| {
        if let Err(e) = thread::Builder::new().name("cranvr-usb".into()).spawn(move || run(hub)) {
            log::warn!("the question to the USB cable did not start: {e}");
        }
    });
}

fn run(hub: &'static Hub) {
    loop {
        if !hub.device().is_live() {
            thread::sleep(IDLE);
            continue;
        }
        // The request lives while a link runs too: a link over the USB network needs its network
        usb_network::keep();
        if !hub.is_searching() {
            thread::sleep(IDLE);
            continue;
        }
        let found = probe();
        if hub.usb() != found {
            match &found {
                Some(mac) if mac.wifi_only => {
                    log::info!("a Mac answers behind the USB cable at {}, and its user chose Wi-Fi", mac.address)
                }
                Some(mac) => log::info!("a Mac answers behind the USB cable at {}", mac.address),
                None => log::info!("the USB cable no longer answers"),
            }
            hub.set_usb(found);
        }
        thread::sleep(ASK_EVERY);
    }
}

/// Which way a link goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    Usb,
    WiFi,
}

/// One line of the connect panel.
#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Cable(UsbMac),
    WiFi(FoundHost),
}

/// The panel's list: the Mac behind the cable first, then the Macs of the Wi-Fi search. The Wi-Fi
/// entry of the cable's Mac stays out, so each Mac has one line. The cable wins for that Mac. The
/// user who wants Wi-Fi sets the Mac's choice to Wi-Fi, which takes the cable away: the Mac then
/// has its Wi-Fi line.
pub fn entries(found: &[FoundHost], usb: Option<&UsbMac>) -> Vec<Line> {
    let usb = usb.and_then(UsbMac::usable);
    let cable = usb.and_then(UsbMac::fingerprint);
    usb.map(|mac| Line::Cable(mac.clone()))
        .into_iter()
        .chain(found.iter().filter(|host| cable != Some(host.fingerprint)).cloned().map(Line::WiFi))
        .collect()
}

/// What a connection aims at: an address and, when known, the fingerprint of the Mac behind it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Aim {
    /// The address as the user pressed or typed it
    pub address: String,
    pub fingerprint: Option<[u8; FINGERPRINT_LEN]>,
}

/// Where a connection goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub address: String,
    pub path: Path,
}

/// Whether an address as text names this machine: a loopback IP, with or without a port, or
/// `localhost`. Behind the cable the Mac answers there.
pub fn is_loopback(address: &str) -> bool {
    let text = address.trim();
    if let Ok(socket) = text.parse::<SocketAddr>() {
        return socket.ip().is_loopback();
    }
    if let Ok(ip) = text.parse::<IpAddr>() {
        return ip.is_loopback();
    }
    text.rsplit_once(':').map_or(text, |(host, _)| host).eq_ignore_ascii_case("localhost")
}

/// The port of an address as text, or the host's default port when the text names none
fn port_of(address: &str) -> u16 {
    let text = address.trim();
    if let Ok(socket) = text.parse::<SocketAddr>() {
        return socket.port();
    }
    text.rsplit_once(':').and_then(|(_, port)| port.parse().ok()).unwrap_or(DEFAULT_PORT)
}

/// Where a connection to `aim` goes now, given what the cable and the Wi-Fi search show.
///
/// An aim at this device's own address means the cable: the way that answers on the cable's port
/// takes it, the USB network first. Any other aim without a fingerprint goes where it says: nothing
/// tells which Mac it means. With a
/// fingerprint, the cable wins when the Mac behind it has the same one. Otherwise the aim goes to
/// the Wi-Fi entry with that fingerprint, which a USB link falls back to when the cable is out. When
/// no such entry exists, the aim keeps its address.
///
/// A Mac whose user chose Wi-Fi counts as no cable. An aim at the cable then means that Mac: its
/// key behind the cable finds its Wi-Fi entry.
pub fn target(aim: &Aim, usb: Option<&UsbMac>, found: &[FoundHost]) -> Target {
    let aim = &Aim { address: aim.address.clone(), fingerprint: aim.fingerprint.or_else(|| mac_set_to_wifi(&aim.address, usb)) };
    let usb = usb.and_then(UsbMac::usable);
    let as_aimed = || {
        if !is_loopback(&aim.address) {
            return Target { address: aim.address.clone(), path: Path::WiFi };
        }
        // An aim at this device's own address means the cable: the way that answers on it takes
        // the aim, the USB network first
        match usb.filter(|cable| port_of(&aim.address) == cable.address.port()) {
            Some(cable) => Target { address: cable.address.to_string(), path: Path::Usb },
            None => Target { address: aim.address.clone(), path: Path::Usb },
        }
    };
    let Some(mac) = aim.fingerprint else { return as_aimed() };
    if let Some(cable) = usb.filter(|cable| cable.fingerprint() == Some(mac)) {
        return Target { address: cable.address.to_string(), path: Path::Usb };
    }
    match found.iter().find(|host| host.fingerprint == mac && !host.address.ip().is_loopback()) {
        Some(host) => Target { address: host.address.to_string(), path: Path::WiFi },
        None => as_aimed(),
    }
}

/// The fingerprint of the Mac behind the cable when `address` aims at the cable and that Mac's user
/// chose Wi-Fi
pub fn mac_set_to_wifi(address: &str, usb: Option<&UsbMac>) -> Option<[u8; FINGERPRINT_LEN]> {
    usb.filter(|mac| mac.wifi_only && is_loopback(address) && port_of(address) == mac.address.port())?.fingerprint()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranvr_proto::PROTOCOL_VERSION;

    const B: [u8; FINGERPRINT_LEN] = [2; FINGERPRINT_LEN];

    fn wifi(name: &str, ip: [u8; 4], fingerprint: [u8; FINGERPRINT_LEN]) -> FoundHost {
        FoundHost { name: name.into(), address: (ip, 9950).into(), fingerprint, version: PROTOCOL_VERSION, paired: false }
    }

    /// A Mac behind the cable whose key has the fingerprint of the first byte's key
    fn cable(key_byte: u8) -> UsbMac {
        UsbMac { address: address(), key: Some([key_byte; KEY_LEN]), version: PROTOCOL_VERSION, way: Way::AdbReverse, wifi_only: false }
    }

    /// The same Mac on the headset's USB network: its link-local address on the cable's link
    fn usb_network(key_byte: u8) -> UsbMac {
        let address = std::net::SocketAddrV6::new("fe80::20:c91:c353:87c3".parse().unwrap(), port(), 0, 17);
        UsbMac { address: address.into(), way: Way::UsbNetwork, ..cable(key_byte) }
    }

    fn aim(address: &str, fingerprint: Option<[u8; FINGERPRINT_LEN]>) -> Aim {
        Aim { address: address.into(), fingerprint }
    }

    #[test]
    fn the_cable_comes_first_and_hides_the_wifi_entry_of_its_mac() {
        let mac = cable(1);
        let same = wifi("Mac", [192, 168, 1, 20], mac.fingerprint().unwrap());
        let other = wifi("Other", [192, 168, 1, 21], B);
        assert_eq!(entries(&[other.clone(), same.clone()], Some(&mac)), [Line::Cable(mac.clone()), Line::WiFi(other.clone())]);
        // No cable: the Wi-Fi list as it is
        assert_eq!(entries(&[other.clone(), same.clone()], None), [Line::WiFi(other), Line::WiFi(same)]);
        assert!(entries(&[], None).is_empty());
    }

    #[test]
    fn a_cable_with_no_known_key_hides_no_wifi_entry() {
        // A host of another protocol version shows no key: it cannot be matched with a Wi-Fi entry
        let unknown = UsbMac { key: None, version: PROTOCOL_VERSION + 1, ..cable(1) };
        let host = wifi("Mac", [192, 168, 1, 20], [0; FINGERPRINT_LEN]);
        assert_eq!(entries(std::slice::from_ref(&host), Some(&unknown)), [Line::Cable(unknown), Line::WiFi(host)]);
    }

    #[test]
    fn the_cable_wins_over_the_wifi_entry_of_the_same_mac() {
        let mac = cable(1);
        let fp = mac.fingerprint().unwrap();
        let found = [wifi("Mac", [192, 168, 1, 20], fp)];
        // The Wi-Fi entry was pressed, or typed with the key of the Mac known: the cable takes it
        let t = target(&aim("192.168.1.20:9950", Some(fp)), Some(&mac), &found);
        assert_eq!(t, Target { address: "127.0.0.1:9950".into(), path: Path::Usb });
        // A press on the cable's entry stays on the cable
        assert_eq!(target(&aim("127.0.0.1:9950", Some(fp)), Some(&mac), &found).path, Path::Usb);
    }

    #[test]
    fn another_mac_stays_on_wifi_while_the_cable_runs_to_a_different_one() {
        let mac = cable(1);
        let found = [wifi("Other", [192, 168, 1, 21], B)];
        let t = target(&aim("192.168.1.21:9950", Some(B)), Some(&mac), &found);
        assert_eq!(t, Target { address: "192.168.1.21:9950".into(), path: Path::WiFi });
    }

    #[test]
    fn a_dropped_cable_falls_back_to_the_wifi_entry_of_the_same_mac() {
        let fp = cable(1).fingerprint().unwrap();
        let found = [wifi("Mac", [192, 168, 1, 20], fp), wifi("Other", [192, 168, 1, 21], B)];
        // The aim was the cable's entry: its address is loopback. With the cable gone it goes to Wi-Fi.
        let t = target(&aim("127.0.0.1:9950", Some(fp)), None, &found);
        assert_eq!(t, Target { address: "192.168.1.20:9950".into(), path: Path::WiFi });
        // The cable answers again: the next connect prefers it, also for an aim that was Wi-Fi
        let t = target(&aim("192.168.1.20:9950", Some(fp)), Some(&cable(1)), &found);
        assert_eq!(t.path, Path::Usb);
    }

    #[test]
    fn with_neither_a_cable_nor_a_wifi_entry_the_aim_keeps_its_address() {
        let fp = cable(1).fingerprint().unwrap();
        assert_eq!(target(&aim("127.0.0.1:9950", Some(fp)), None, &[]), Target { address: "127.0.0.1:9950".into(), path: Path::Usb });
        assert_eq!(target(&aim("192.168.1.20:9950", Some(fp)), None, &[]), Target { address: "192.168.1.20:9950".into(), path: Path::WiFi });
    }

    #[test]
    fn an_address_without_a_known_mac_goes_where_it_says() {
        let mac = cable(1);
        let found = [wifi("Mac", [192, 168, 1, 20], mac.fingerprint().unwrap())];
        assert_eq!(target(&aim("192.168.1.20:9950", None), Some(&mac), &found).path, Path::WiFi);
        assert_eq!(target(&aim("127.0.0.1:9962", None), None, &found), Target { address: "127.0.0.1:9962".into(), path: Path::Usb });
        assert_eq!(target(&aim("my-mac.local:9950", None), Some(&mac), &found).path, Path::WiFi);
    }

    // The cable is real only when the host answers. `adb reverse` accepts a connection on the
    // headset whether or not a host runs on the Mac, and closes it at once when none does.
    #[test]
    fn a_host_behind_the_cable_shows_its_key_and_a_dead_link_shows_nothing() {
        use cranvr_proto::{identity::Identity, link::HostListener};
        let identity = Identity::from_seed([8; 32]);
        let listener = HostListener::bind("127.0.0.1:0", identity.clone()).unwrap();
        let address = listener.local_addr().unwrap();
        // The host's accept loop sees the probe end before the client's key
        let host = thread::spawn(move || listener.accept().err());
        let mac = probe_at(&AnyRoute, address, Way::AdbReverse).expect("the host answers");
        assert_eq!(mac.key, Some(identity.public_key()));
        assert_eq!(mac.fingerprint(), Some(fingerprint(&identity.public_key())));
        assert_eq!(mac.address, address);
        assert!(host.join().unwrap().is_some());

        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = dead.local_addr().unwrap();
        let acceptor = thread::spawn(move || drop(dead.accept().unwrap()));
        assert_eq!(probe_at(&AnyRoute, address, Way::AdbReverse), None);
        acceptor.join().unwrap();
        // Nothing listens: the cable is out
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert_eq!(probe_at(&AnyRoute, free, Way::AdbReverse), None);
    }

    #[test]
    fn an_aim_at_this_device_takes_the_usb_network_when_it_answers() {
        let network = usb_network(1);
        let found = [wifi("Mac", [192, 168, 1, 20], network.fingerprint().unwrap())];
        // The launch's address or the cable's line, with or without the Mac's key known
        for fingerprint in [None, network.fingerprint()] {
            let t = target(&aim("127.0.0.1:9950", fingerprint), Some(&network), &found);
            assert_eq!(t, Target { address: "[fe80::20:c91:c353:87c3%17]:9950".into(), path: Path::Usb });
        }
        // The address of the USB network parses back with its scope
        assert_eq!("[fe80::20:c91:c353:87c3%17]:9950".parse::<SocketAddr>().unwrap(), network.address);
        // A test host on another port is not the cable's host
        assert_eq!(target(&aim("127.0.0.1:9962", None), Some(&network), &found), Target { address: "127.0.0.1:9962".into(), path: Path::Usb });
        // The Wi-Fi entry of the same Mac shows once, as the cable
        assert_eq!(entries(&found, Some(&network)), [Line::Cable(network)]);
    }

    // The Mac's user chose Wi-Fi: the Mac shows its key behind the cable on either way and takes
    // no link there. The aim at the cable and the press on the Mac's Wi-Fi line both go to its
    // Wi-Fi address. The panel lists that Mac by Wi-Fi.
    #[test]
    fn a_mac_set_to_wifi_takes_the_aim_at_the_cable_over_wifi() {
        for way in [usb_network(1), cable(1)] {
            let mac = UsbMac { wifi_only: true, ..way };
            let fp = mac.fingerprint().unwrap();
            let found = [wifi("Mac", [192, 168, 1, 20], fp), wifi("Other", [192, 168, 1, 21], B)];
            let over_wifi = Target { address: "192.168.1.20:9950".into(), path: Path::WiFi };
            assert_eq!(target(&aim("127.0.0.1:9950", None), Some(&mac), &found), over_wifi);
            assert_eq!(target(&aim("192.168.1.20:9950", Some(fp)), Some(&mac), &found), over_wifi);
            assert_eq!(entries(&found, Some(&mac)), [Line::WiFi(found[0].clone()), Line::WiFi(found[1].clone())]);
            // A test host on another port is another Mac
            assert_eq!(mac_set_to_wifi("127.0.0.1:9962", Some(&mac)), None);
            // The Wi-Fi search has not listed the Mac yet: the aim keeps its address
            assert_eq!(target(&aim("127.0.0.1:9950", None), Some(&mac), &[]).address, "127.0.0.1:9950");
        }
        // The user chooses the cable again: the cable wins
        let t = target(&aim("127.0.0.1:9950", None), Some(&usb_network(1)), &[wifi("Mac", [192, 168, 1, 20], usb_network(1).fingerprint().unwrap())]);
        assert_eq!(t.path, Path::Usb);
    }

    #[test]
    fn loopback_is_told_from_the_text() {
        for loopback in ["127.0.0.1:9950", "127.0.0.1", " 127.0.0.1:9962 ", "[::1]:9950", "::1", "localhost:9950", "LOCALHOST"] {
            assert!(is_loopback(loopback), "{loopback}");
        }
        for other in ["192.168.1.20:9950", "mac.local:9950", "localhostile:9950", ""] {
            assert!(!is_loopback(other), "{other}");
        }
    }
}
