//! macOS permissions. cranvr-host runs as a child of this app, and macOS gives a child the
//! permissions of the app that started it. So the app asks for them, and the host inherits them.
//!
//! - Screen Recording: the desktop view captures the display.
//! - System Audio Recording: the host captures the Mac's sound for the headset with a Core Audio tap
//!   (`cranvr_common::system_audio`).
//! - Accessibility: the controller moves the mouse.
//! - Local Network: macOS asks by itself when the host first talks to the headset (Info.plist
//!   carries the reason).

use cranvr_common::system_audio::{self, Access};
use std::{
    ffi::c_void,
    net::{IpAddr, Ipv4Addr},
    process::Command,
};

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFBooleanTrue: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    ScreenRecording,
    SystemAudio,
    Accessibility,
}

impl Permission {
    pub fn granted(self) -> bool {
        unsafe {
            match self {
                Permission::ScreenRecording => CGPreflightScreenCaptureAccess(),
                Permission::SystemAudio => system_audio::access() == Access::Granted,
                Permission::Accessibility => AXIsProcessTrusted(),
            }
        }
    }

    /// Shows the system prompt. macOS shows it once per app; after a "Don't Allow" only the
    /// switch in System Settings helps, so this also opens the right pane then.
    pub fn request(self) {
        if !self.prompt() {
            self.open_settings();
        }
    }

    /// Shows the system prompt alone. It returns false when the permission stays off: then only
    /// System Settings helps. System Audio Recording returns true while its prompt waits for an answer.
    pub fn prompt(self) -> bool {
        unsafe {
            match self {
                Permission::ScreenRecording => CGRequestScreenCaptureAccess(),
                Permission::SystemAudio => match system_audio::access() {
                    Access::Granted => true,
                    Access::Denied => false,
                    // The prompt answers later: the permission list reads the state again
                    Access::Unknown => {
                        system_audio::request();
                        true
                    }
                },
                Permission::Accessibility => {
                    let keys = [kAXTrustedCheckOptionPrompt];
                    let values = [kCFBooleanTrue];
                    let options = CFDictionaryCreate(
                        std::ptr::null(),
                        keys.as_ptr(),
                        values.as_ptr(),
                        1,
                        &kCFTypeDictionaryKeyCallBacks as *const c_void,
                        &kCFTypeDictionaryValueCallBacks as *const c_void,
                    );
                    let trusted = AXIsProcessTrustedWithOptions(options);
                    CFRelease(options);
                    trusted
                }
            }
        }
    }

    pub fn open_settings(self) {
        let pane = match self {
            Permission::ScreenRecording => "Privacy_ScreenCapture",
            Permission::SystemAudio => "Privacy_AudioCapture",
            Permission::Accessibility => "Privacy_Accessibility",
        };
        Command::new("open")
            .arg(format!("x-apple.systempreferences:com.apple.preference.security?{pane}"))
            .status()
            .ok();
    }

    pub fn title(self) -> &'static str {
        match self {
            Permission::ScreenRecording => "Screen Recording",
            Permission::SystemAudio => "System Audio Recording",
            Permission::Accessibility => "Accessibility",
        }
    }

    pub fn reason(self) -> &'static str {
        match self {
            Permission::ScreenRecording => "shows your Mac's screen in the headset",
            Permission::SystemAudio => "plays your Mac's sound in the headset",
            Permission::Accessibility => "lets the controller move the mouse and click",
        }
    }
}

/// Whether this process runs from an app bundle. Permissions go to the bundle then; from a
/// terminal they go to the terminal app.
pub fn in_app_bundle() -> bool {
    std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
}

/// This Mac's addresses on the local network, to type into the headset: the IPv4 addresses of
/// the Wi-Fi and Ethernet interfaces (en0, en1 and so on). Loopback and link-local addresses
/// stay out. So do the tunnels of VPNs (utun) and the bridges of Docker and virtual machines.
pub fn lan_addresses() -> Vec<Ipv4Addr> {
    lan_of(local_ip_address::list_afinet_netifas().unwrap_or_default())
}

fn lan_of(mut interfaces: Vec<(String, IpAddr)>) -> Vec<Ipv4Addr> {
    interfaces.sort();
    let mut found: Vec<Ipv4Addr> = interfaces
        .into_iter()
        .filter_map(|(name, ip)| match ip {
            IpAddr::V4(ip) if name.starts_with("en") && !ip.is_loopback() && !ip.is_link_local() => Some(ip),
            _ => None,
        })
        .collect();
    found.dedup();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_headset_gets_the_addresses_of_wifi_and_ethernet_only() {
        let interfaces = ["lo0 127.0.0.1", "en1 10.0.0.7", "en0 192.168.1.20", "utun4 100.101.102.103", "bridge100 192.168.64.1", "en0 169.254.9.9", "en0 fe80::1", "awdl0 fe80::2"];
        let interfaces = interfaces.map(|line| {
            let (name, ip) = line.split_once(' ').unwrap();
            (name.to_string(), ip.parse().unwrap())
        });
        assert_eq!(lan_of(interfaces.to_vec()), [Ipv4Addr::new(192, 168, 1, 20), Ipv4Addr::new(10, 0, 0, 7)]);
    }
}
