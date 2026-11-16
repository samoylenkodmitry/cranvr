//! Checks of the network path between this Mac and the headset. The host reports what it finds
//! as `network` status lines, and the cranvr app shows them.
//!
//! A VPN on the headset can route the local network through another device. Tailscale does this
//! for a device that accepts a subnet route of its own network: the headset's UDP then reaches
//! the Mac from the subnet router's address, and ALVR drops every stream packet that does not
//! come from the headset's own address. TCP still works, because the Mac opens those connections
//! and Android answers them on the interface that they came in on.

use serde_json::{json, Value};
use std::{fs, net::IpAddr, path::Path, process::Command};

/// Connections in a row that end without a pose before the host reports a problem. One such
/// connection happens when the headset leaves for other reasons.
const FAILED_CONNECTIONS: u32 = 2;

/// Watches whether each connection gets the headset's poses.
#[derive(Default)]
pub struct StreamWatch {
    poses: bool,
    failures: u32,
}

impl StreamWatch {
    pub fn connected(&mut self) {
        self.poses = false;
    }

    pub fn pose(&mut self) {
        self.poses = true;
    }

    /// A status line when the connection that ended got no pose, and the one before it neither.
    pub fn disconnected(&mut self, tcp: bool) -> Option<Value> {
        if self.poses {
            self.failures = 0;
            return None;
        }
        self.failures += 1;
        (self.failures == FAILED_CONNECTIONS).then(|| {
            let message = if tcp {
                "The headset connected over TCP, but none of its stream data reached this Mac. \
                 Check the firewall and VPN settings on the headset and on this Mac."
            } else {
                "The headset connected, but none of its stream data reached this Mac. A VPN on \
                 the headset can send its UDP packets through another device that changes their \
                 sender address: Tailscale does this when the headset uses a subnet route for its \
                 own network. Exclude the ALVR app from the VPN on the headset, or stream over TCP."
            };
            json!({
                "type": "network",
                "problem": "no_poses",
                "protocol": if tcp { "tcp" } else { "udp" },
                "message": message,
            })
        })
    }
}

/// The address of the headset that is connected now, from ALVR's session file.
pub fn client_ip(session: &Path) -> Option<IpAddr> {
    let json: Value = serde_json::from_str(&fs::read_to_string(session).ok()?).ok()?;
    json["client_connections"].as_object()?.values().find_map(|client| {
        let state = client["connection_state"].as_str()?;
        matches!(state, "Connecting" | "Connected" | "Streaming").then_some(())?;
        client["current_ip"].as_str()?.parse().ok()
    })
}

/// A status line when this Mac reaches `ip` through a tunnel interface, such as a VPN's.
pub fn check_route(ip: IpAddr) -> Option<Value> {
    let output = Command::new("/sbin/route").args(["-n", "get", &ip.to_string()]).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let interface = text.lines().find_map(|line| line.trim().strip_prefix("interface: "))?.to_string();
    interface.starts_with("utun").then(|| {
        json!({
            "type": "network",
            "problem": "vpn_route",
            "interface": interface,
            "message": format!(
                "This Mac reaches the headset at {ip} through a VPN tunnel ({interface}). The stream \
                 takes a longer path. Set the VPN on this Mac to leave the local network alone."
            ),
        })
    })
}

#[cfg(test)]
#[path = "tests/network.rs"]
mod tests;
