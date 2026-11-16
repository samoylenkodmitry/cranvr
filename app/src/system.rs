//! macOS permissions. cranvr-host runs as a child of this app, and macOS gives a child the
//! permissions of the app that started it. So the app asks for them, and the host inherits them.
//!
//! - Screen Recording: the desktop view captures the display. The host also captures the Mac's
//!   sound for the headset with it.
//! - Accessibility: the controller moves the mouse.
//! - Local Network: macOS asks by itself when the host first talks to the headset (Info.plist
//!   carries the reason).

use std::{ffi::c_void, process::Command};

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
    Accessibility,
}

impl Permission {
    pub fn granted(self) -> bool {
        unsafe {
            match self {
                Permission::ScreenRecording => CGPreflightScreenCaptureAccess(),
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

    /// Shows the system prompt alone. It returns whether the permission is allowed.
    pub fn prompt(self) -> bool {
        unsafe {
            match self {
                Permission::ScreenRecording => CGRequestScreenCaptureAccess(),
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
            Permission::Accessibility => "Accessibility",
        }
    }

    pub fn reason(self) -> &'static str {
        match self {
            Permission::ScreenRecording => "shows your Mac's screen and plays its sound in the headset",
            Permission::Accessibility => "lets the controller move the mouse and click",
        }
    }
}

/// Whether this process runs from an app bundle. Permissions go to the bundle then; from a
/// terminal they go to the terminal app.
pub fn in_app_bundle() -> bool {
    std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
}
