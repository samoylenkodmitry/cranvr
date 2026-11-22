//! The headset's USB network to the Mac. The app asks the system for a network of the USB transport
//! without the internet (Cranpose's `request_network`). Horizon OS then switches the headset's USB
//! functions to a network over the cable. The Mac makes an interface for it with an IPv6 link-local
//! address. The app finds the Mac's host by the discovery query to every node on that link
//! (`cranvr_proto::discovery`), and it binds its sockets to the network. This way needs no
//! Developer Mode and no adb.
//!
//! Horizon OS serves the request only while the app is in front. A request that the system drops
//! is made again once the app has the focus. The request lives as long as the app does, so a link
//! over the network keeps its network.
//!
//! The first time, macOS asks the user whether to allow the headset's new USB functions. Until
//! the user allows them, the network stays pending and the cable's `adb reverse` or Wi-Fi carry
//! the link.

use crate::focus;
use cranpose::{launch_args, request_network, Network, NetworkRequest, NetworkRequestState, NetworkTransport, RequestedNetwork};
use cranvr_proto::{
    discovery::{find_with, Finder, DEFAULT_PORT},
    link::Route,
    message::Focus,
};
use std::{
    io,
    net::{Ipv6Addr, SocketAddr, TcpStream, UdpSocket},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

/// How long the client waits for an answer to the query on the USB network. The link is a cable
/// to one Mac: the answer comes within a few milliseconds.
const SEARCH: Duration = Duration::from_millis(300);
/// A request that the system dropped is made again after this long
const RETRY: Duration = Duration::from_secs(10);
/// How long a first connection waits for a request without an answer. The headset needs about a
/// second to switch its USB functions. A headset without a cable never answers, so the wait ends.
const SETTLE: Duration = Duration::from_secs(4);

/// The request and what the log said about it last.
struct Held {
    request: RequestedNetwork,
    since: Instant,
    told: Option<NetworkRequestState>,
}

static HELD: Mutex<Option<Held>> = Mutex::new(None);

/// Whether this launch asks for the USB network. `--ez usb_network false` keeps the headset's USB
/// functions as they are, for a test of `adb reverse` that a change of the functions would cut.
pub fn wanted() -> bool {
    static WANTED: OnceLock<bool> = OnceLock::new();
    *WANTED.get_or_init(|| launch_args().boolean("usb_network").unwrap_or(true))
}

/// Keeps a request for the USB network while the app has the focus. The cable's thread calls it
/// about once a second.
pub fn keep() {
    if !wanted() {
        return;
    }
    let mut held = HELD.lock().unwrap();
    if held.as_ref().is_some_and(|held| held.request.state() == NetworkRequestState::Unavailable && held.since.elapsed() >= RETRY) {
        *held = None;
    }
    if held.is_none() && focus::current() == Focus::Focused {
        *held = Some(Held { request: request_network(NetworkRequest::local(NetworkTransport::Usb)), since: Instant::now(), told: None });
    }
    if let Some(held) = held.as_mut() {
        let state = held.request.state();
        if held.told.as_ref() != Some(&state) {
            tell(&state);
            held.told = Some(state);
        }
    }
}

fn tell(state: &NetworkRequestState) {
    match state {
        NetworkRequestState::Pending => log::info!("the USB network is asked for"),
        NetworkRequestState::Available(network) => log::info!(
            "the USB network is up on {} (index {}) with {:?}, {:?} kbps down and {:?} up",
            network.interface_name,
            network.interface_index,
            network.addresses,
            network.link_speed.downstream_kbps,
            network.link_speed.upstream_kbps
        ),
        NetworkRequestState::Lost => log::info!("the USB network went away"),
        NetworkRequestState::Unavailable => log::info!("the system does not hand out the USB network now"),
    }
}

/// The USB network while the system hands one out
pub fn network() -> Option<Network> {
    HELD.lock().unwrap().as_ref().and_then(|held| held.request.network())
}

/// Whether a request is a moment old and has no answer yet. The headset switches its USB functions
/// then, which cuts `adb` for about a second, and the USB network may come up right after it.
pub fn settling() -> bool {
    HELD.lock().unwrap().as_ref().is_some_and(|held| held.since.elapsed() < SETTLE && held.request.state() == NetworkRequestState::Pending)
}

/// The network of `address` when it is the Mac's link-local address on the USB network: an IPv6
/// address whose scope is the network's interface.
pub fn network_of(address: &SocketAddr) -> Option<Network> {
    let SocketAddr::V6(address) = address else { return None };
    network().filter(|network| address.ip().is_unicast_link_local() && address.scope_id() == network.interface_index)
}

/// The hosts that answer the discovery query on the USB network, by the address to connect to.
pub fn find_hosts(network: &Network) -> Vec<(SocketAddr, cranvr_proto::discovery::Answer)> {
    let finder = network
        .bind_udp((Ipv6Addr::UNSPECIFIED, 0).into())
        .and_then(|socket| Finder::on(socket, vec![network.all_nodes(DEFAULT_PORT)]));
    match finder {
        Ok(finder) => find_with(&finder, SEARCH).into_iter().map(|found| (found.socket_addr(), found.answer)).collect(),
        Err(e) => {
            log::warn!("no socket on the USB network: {e}");
            Vec::new()
        }
    }
}

/// The sockets of a link over the USB network: bound to the network, so they use the cable
/// whatever the headset's default network is.
pub struct NetworkRoute(pub Network);

impl Route for NetworkRoute {
    fn connect_tcp(&self, host: SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
        self.0.connect_tcp(host, timeout)
    }

    fn bind_udp(&self, local: SocketAddr) -> io::Result<UdpSocket> {
        self.0.bind_udp(local)
    }
}
