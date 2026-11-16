// cranvr: on Android the probe runs as a plain program started by adb. It has no Java VM, so
// the calls of alvr_system_info that go through JNI are replaced here. Other systems use
// alvr_system_info.

use std::net::IpAddr;

#[cfg(target_os = "android")]
pub fn local_ip() -> IpAddr {
    use std::net::{Ipv4Addr, UdpSocket};
    // Connecting a UDP socket sends nothing: it only picks the interface of the default route.
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| {
            socket.connect("192.0.2.1:9")?;
            socket.local_addr()
        })
        .map(|address| address.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

#[cfg(not(target_os = "android"))]
pub fn local_ip() -> IpAddr {
    alvr_system_info::local_ip()
}

#[cfg(target_os = "android")]
pub fn platform_name() -> String {
    "Android (cranvr-probe)".into()
}

#[cfg(not(target_os = "android"))]
pub fn platform_name() -> String {
    alvr_system_info::platform().to_string()
}

/// The battery level from 0 to 1, and whether a charger is plugged in.
#[cfg(target_os = "android")]
pub fn battery() -> (f32, bool) {
    let read = |name: &str| std::fs::read_to_string(format!("/sys/class/power_supply/battery/{name}")).unwrap_or_default();
    let level = read("capacity").trim().parse::<f32>().map_or(1.0, |percent| percent / 100.0);
    let plugged = matches!(read("status").trim(), "Charging" | "Full");
    (level, plugged)
}
