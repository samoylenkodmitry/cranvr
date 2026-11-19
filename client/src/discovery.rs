//! The search for Macs that run cranvr. A thread asks the local network a few times a second while
//! no link runs (see `cranvr_proto::discovery`). The hub keeps the Macs that answered, and the
//! connect panel lists them. A Mac that stops answering leaves the list after a few seconds.

use crate::{hub::Hub, settings};
use cranvr_proto::{
    discovery::{broadcast_targets, find, Finder, Found, DEFAULT_PORT},
    message::{VersionRange, UPDATE_HEADSET, UPDATE_MAC},
};
use std::{
    net::SocketAddr,
    sync::Once,
    thread,
    time::{Duration, Instant},
};

/// How long one search round listens, and how often it asks within the round
const ROUND: Duration = Duration::from_millis(1000);
const ASK_EVERY: Duration = Duration::from_millis(400);
/// A Mac that has not answered for this long leaves the list
const FORGET_AFTER: Duration = Duration::from_secs(5);
/// How long the thread waits while a link runs, or when no socket opens
const IDLE: Duration = Duration::from_millis(1000);

/// A Mac that answered.
#[derive(Clone, Debug, PartialEq)]
pub struct FoundHost {
    pub name: String,
    /// Where the host listens for a connection
    pub address: SocketAddr,
    /// The start of the hash of the host's key
    pub fingerprint: [u8; 8],
    /// The protocol versions of the host
    pub versions: VersionRange,
    /// This headset paired with the host before: it remembers the host's key
    pub paired: bool,
}

impl FoundHost {
    /// What the user must do when this headset and the host speak no common protocol version.
    /// `None` when they can talk.
    pub fn update_advice(&self) -> Option<&'static str> {
        let ours = VersionRange::OURS;
        if ours.common(self.versions).is_some() {
            return None;
        }
        Some(if ours.peer_is_older(self.versions) { UPDATE_MAC } else { UPDATE_HEADSET })
    }

    /// The address as the connect panel's field takes it
    pub fn address_text(&self) -> String {
        self.address.to_string()
    }
}

/// Starts the search thread, once for the process.
pub fn start(hub: &'static Hub) {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        if let Err(e) = thread::Builder::new().name("cranvr-search".into()).spawn(move || run(hub)) {
            log::warn!("the search for Macs did not start: {e}");
        }
    });
}

fn host_of(found: &Found) -> FoundHost {
    FoundHost {
        name: found.answer.name.clone(),
        address: found.socket_addr(),
        fingerprint: found.answer.fingerprint,
        versions: found.answer.versions,
        paired: settings::host_key_by_fingerprint(&found.answer.fingerprint).is_some(),
    }
}

/// Asks the network for `wait` and returns the Macs that answered. A connection that lost its
/// cable calls it on its own thread, to find the Mac's Wi-Fi address.
pub fn search_now(wait: Duration) -> Vec<FoundHost> {
    match find(broadcast_targets(DEFAULT_PORT), wait) {
        Ok(found) => found.iter().map(host_of).collect(),
        Err(e) => {
            log::warn!("no socket for the search for Macs: {e}");
            Vec::new()
        }
    }
}

fn run(hub: &'static Hub) {
    let mut seen: Vec<(FoundHost, Instant)> = Vec::new();
    loop {
        if !hub.is_searching() {
            thread::sleep(IDLE);
            continue;
        }
        let finder = match Finder::new(broadcast_targets(DEFAULT_PORT)) {
            Ok(finder) => finder,
            Err(e) => {
                log::warn!("no socket for the search for Macs: {e}");
                thread::sleep(IDLE);
                continue;
            }
        };
        let round_end = Instant::now() + ROUND;
        let mut next_ask = Instant::now();
        while Instant::now() < round_end {
            if Instant::now() >= next_ask {
                finder.ask();
                next_ask = Instant::now() + ASK_EVERY;
            }
            let wait = next_ask.min(round_end).saturating_duration_since(Instant::now());
            let Some(found) = finder.receive(wait) else { continue };
            let host = host_of(&found);
            match seen.iter_mut().find(|(known, _)| known.address == host.address) {
                Some(entry) => *entry = (host, Instant::now()),
                None => seen.push((host, Instant::now())),
            }
        }
        seen.retain(|(_, at)| at.elapsed() < FORGET_AFTER);
        hub.set_found(list(&seen));
    }
}

/// The hosts for the panel, the paired ones first, then by name. A host on this machine answers
/// the query to the machine and the broadcast: the entry with the network address stays, as the
/// one that a headset could use.
fn list(seen: &[(FoundHost, Instant)]) -> Vec<FoundHost> {
    let mut hosts: Vec<FoundHost> = seen
        .iter()
        .map(|(host, _)| host)
        .filter(|host| {
            !host.address.ip().is_loopback()
                || !seen.iter().any(|(other, _)| {
                    !other.address.ip().is_loopback() && other.address.port() == host.address.port() && other.fingerprint == host.fingerprint
                })
        })
        .cloned()
        .collect();
    hosts.sort_by(|a, b| b.paired.cmp(&a.paired).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then(a.address.cmp(&b.address)));
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str, port: u16, paired: bool, versions: VersionRange) -> FoundHost {
        FoundHost { name: name.into(), address: ([192, 168, 1, 20], port).into(), fingerprint: [0; 8], versions, paired }
    }

    #[test]
    fn a_host_of_the_same_protocol_needs_no_update() {
        assert_eq!(host("Mac", 9950, false, VersionRange::OURS).update_advice(), None);
        // A host that speaks only version 4 still talks to this client
        assert_eq!(host("Mac", 9950, false, VersionRange { min: 4, max: 4 }).update_advice(), None);
    }

    #[test]
    fn the_older_side_is_told_to_update() {
        let ours = VersionRange::OURS;
        let old = VersionRange { min: ours.min - 1, max: ours.min - 1 };
        let new = VersionRange { min: ours.max + 1, max: ours.max + 2 };
        assert_eq!(host("Mac", 9950, false, old).update_advice(), Some("Update cranvr on the Mac."));
        assert_eq!(host("Mac", 9950, false, new).update_advice(), Some("Update the cranvr app on the headset."));
    }

    #[test]
    fn a_host_on_this_machine_is_listed_once_by_its_network_address() {
        let now = Instant::now();
        let mut local = host("Mac", 9950, false, VersionRange::OURS);
        local.address = ([127, 0, 0, 1], 9950).into();
        let network = host("Mac", 9950, false, VersionRange::OURS);
        // A host that answered on loopback only stays, and so does another host of the same Mac on another port
        let mut only_local = host("Other", 9962, false, VersionRange::OURS);
        only_local.address = ([127, 0, 0, 1], 9962).into();
        let seen = vec![(local, now), (network.clone(), now), (only_local.clone(), now)];
        assert_eq!(list(&seen), [network, only_local]);
    }

    #[test]
    fn the_list_puts_paired_hosts_first_and_sorts_by_name() {
        let now = Instant::now();
        let seen = vec![
            (host("zeta", 9950, false, VersionRange::OURS), now),
            (host("Beta", 9950, false, VersionRange::OURS), now),
            (host("Omega", 9950, true, VersionRange::OURS), now),
        ];
        let names: Vec<_> = list(&seen).into_iter().map(|host| host.name).collect();
        assert_eq!(names, ["Omega", "Beta", "zeta"]);
    }
}
