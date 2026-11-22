//! Discovery over real UDP sockets on this machine: a responder answers a finder, two responders
//! share one port, and the finder takes only answers to its own query. The tests use their own
//! ports and no broadcast except the one that checks that two hosts on a Mac both answer it.

use cranvr_proto::{
    discovery::{find, find_with, Answer, Finder, Responder},
    identity::{fingerprint, Identity},
    PROTOCOL_VERSION,
};
use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6, UdpSocket},
    sync::{atomic::AtomicBool, Arc},
    thread,
    time::Duration,
};

fn answer(name: &str, port: u16, seed: u8) -> Answer {
    Answer {
        version: PROTOCOL_VERSION,
        port,
        fingerprint: fingerprint(&Identity::from_seed([seed; 32]).public_key()),
        nonce: 0,
        name: name.into(),
    }
}

/// Runs a responder on its own thread. Dropping the guard stops it.
struct Running {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    port: u16,
}

impl Running {
    fn start(port: u16, answer: Answer) -> Running {
        Self::run(Responder::bind(port).unwrap(), answer)
    }

    fn start_ipv6(port: u16, answer: Answer) -> Running {
        Self::run(Responder::bind_ipv6(port).unwrap(), answer)
    }

    fn run(responder: Responder, answer: Answer) -> Running {
        let port = responder.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            thread::spawn(move || responder.run(&stop, &answer))
        };
        Running { stop, thread: Some(thread), port }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn loopback(port: u16) -> SocketAddr {
    (Ipv4Addr::LOCALHOST, port).into()
}

#[test]
fn a_finder_hears_the_name_the_port_and_the_fingerprint_of_a_host() {
    let host = Running::start(0, answer("Dmitry's MacBook", 9961, 7));
    let found = find(vec![loopback(host.port)], Duration::from_millis(600)).unwrap();
    assert_eq!(found.len(), 1, "one host, listed once although the finder asked several times");
    let found = &found[0];
    assert_eq!(found.from.ip(), Ipv4Addr::LOCALHOST);
    assert_eq!(found.socket_addr(), loopback(9961));
    assert_eq!(found.answer.name, "Dmitry's MacBook");
    assert_eq!(found.answer.fingerprint, fingerprint(&Identity::from_seed([7; 32]).public_key()));
    assert_eq!(found.answer.version, PROTOCOL_VERSION);
}

#[test]
fn a_finder_hears_nobody_where_no_host_listens() {
    // A port that nothing uses
    let free = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port();
    assert!(find(vec![loopback(free)], Duration::from_millis(400)).unwrap().is_empty());
}

#[test]
fn a_finder_ignores_an_answer_to_another_query_and_noise() {
    let finder = Finder::new(vec![]).unwrap();
    let stranger = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    // An answer with a nonce that this finder never sent, then noise
    let stale = Answer { nonce: 12345, ..answer("Stale", 9950, 1) };
    stranger.send_to(&stale.to_bytes(), loopback(finder.local_port())).unwrap();
    stranger.send_to(b"hello", loopback(finder.local_port())).unwrap();
    assert_eq!(finder.receive(Duration::from_millis(300)), None);
}

#[test]
fn two_hosts_on_one_port_both_hear_a_broadcast() {
    let first = Running::start(0, answer("First", 9961, 1));
    let second = Running::start(first.port, answer("Second", 9962, 2));
    assert_eq!(first.port, second.port, "the second host shares the port of the first");
    let found = find(vec![(Ipv4Addr::BROADCAST, first.port).into()], Duration::from_millis(800)).unwrap();
    let mut names: Vec<_> = found.iter().map(|host| host.answer.name.clone()).collect();
    names.sort();
    assert_eq!(names, ["First", "Second"], "{found:?}");
}

#[test]
fn an_ipv6_responder_shares_the_port_of_the_ipv4_one_and_answers_on_ipv6() {
    let v4 = Running::start(0, answer("Mac", 9961, 3));
    let v6 = Running::start_ipv6(v4.port, answer("Mac", 9961, 3));
    assert_eq!(v4.port, v6.port);
    let socket = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).unwrap();
    let finder = Finder::on(socket, vec![(Ipv6Addr::LOCALHOST, v6.port).into()]).unwrap();
    let found = find_with(&finder, Duration::from_millis(600));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].socket_addr(), (Ipv6Addr::LOCALHOST, 9961).into());
}

/// The interface index of macOS's loopback, which has the link-local address fe80::1
#[cfg(target_os = "macos")]
const LOOPBACK_SCOPE: u32 = 1;

// A query to the group of all nodes on a link reaches a responder that joined no group, as the
// headset's query on the USB network reaches the Mac. The answer keeps the link as its scope.
#[cfg(target_os = "macos")]
#[test]
fn a_query_to_all_nodes_on_a_link_finds_the_host_by_its_link_local_address() {
    let host = Running::start_ipv6(0, answer("Mac", 9962, 4));
    let all_nodes = SocketAddrV6::new(Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1), host.port, 0, LOOPBACK_SCOPE);
    let socket = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).unwrap();
    let finder = Finder::on(socket, vec![all_nodes.into()]).unwrap();
    let found = find_with(&finder, Duration::from_millis(600));
    assert_eq!(found.len(), 1, "{found:?}");
    let SocketAddr::V6(address) = found[0].socket_addr() else { panic!("an IPv6 answer: {found:?}") };
    assert!(address.ip().is_unicast_link_local(), "{address}");
    assert_eq!(address.scope_id(), LOOPBACK_SCOPE);
    assert_eq!(address.port(), 9962);
}
