//! The cranvr window in VR. While a headset streams, the window moves onto the virtual
//! display that the host shows in VR, so the user can start Steam and games from the headset.
//! When the headset leaves, the window goes back to where it was.
//!
//! The place to go back to is kept as a display and an offset on it. The host makes the virtual
//! display the main one while a headset streams. That moves the origin of macOS' window
//! coordinates.

use dispatch2::DispatchQueue;
use objc2::{
    msg_send,
    runtime::{AnyClass, AnyObject},
};
use objc2_foundation::{NSPoint, NSRect, NSString};
use std::sync::Mutex;

/// Where the window was before it moved into VR: a display ID and the offset on that display.
static HOME: Mutex<Option<(u32, f64, f64)>> = Mutex::new(None);

/// Moves the window to the middle of the virtual display. Runs on the main thread.
pub fn move_into_vr(vr_display: u32) {
    DispatchQueue::main().exec_async(move || unsafe {
        let Some(window) = app_window() else { return };
        let frame: NSRect = msg_send![window, frame];
        let mut home = HOME.lock().unwrap();
        if home.is_none() {
            // A window that sat on the virtual display already goes back to the main display
            *home = screen_of(frame)
                .filter(|(id, _)| *id != vr_display)
                .map(|(id, screen)| (id, frame.origin.x - screen.origin.x, frame.origin.y - screen.origin.y));
        }
        if let Some(area) = visible_frame(vr_display) {
            center(window, frame, area);
        }
    });
}

/// Moves the window back to where it was before it went into VR, or to the middle of the main
/// display. Without a headset nobody sees the virtual display, so the window must not stay
/// there: macOS also puts it back there at the next start when the app ended in VR.
pub fn move_out_of_vr(vr_display: u32) {
    DispatchQueue::main().exec_async(move || unsafe {
        let Some(window) = app_window() else { return };
        let frame: NSRect = msg_send![window, frame];
        let home = HOME.lock().unwrap().take();
        let on_vr = screen_of(frame).is_some_and(|(id, _)| id == vr_display);
        match home.and_then(|(id, dx, dy)| screen_frame(id).map(|s| NSPoint::new(s.origin.x + dx, s.origin.y + dy))) {
            Some(origin) => {
                let _: () = msg_send![window, setFrameOrigin: origin];
            }
            None if on_vr => {
                if let Some(area) = main_visible_frame() {
                    center(window, frame, area);
                }
            }
            None => (),
        }
    });
}

unsafe fn center(window: &AnyObject, frame: NSRect, area: NSRect) {
    let origin = NSPoint::new(
        area.origin.x + ((area.size.width - frame.size.width) / 2.0).max(0.0),
        area.origin.y + ((area.size.height - frame.size.height) / 2.0).max(0.0),
    );
    let _: () = msg_send![window, setFrameOrigin: origin];
}

/// The visible frame of the screen with the menu bar.
unsafe fn main_visible_frame() -> Option<NSRect> {
    screens().into_iter().next().map(|s| s.2)
}

/// The app's first visible window.
unsafe fn app_window() -> Option<&'static AnyObject> {
    let app_class = AnyClass::get(c"NSApplication")?;
    let app: *mut AnyObject = msg_send![app_class, sharedApplication];
    let windows: *mut AnyObject = msg_send![app.as_ref()?, windows];
    let windows = windows.as_ref()?;
    let count: usize = msg_send![windows, count];
    (0..count).find_map(|i| {
        let window: *mut AnyObject = msg_send![windows, objectAtIndex: i];
        let window = window.as_ref()?;
        let visible: bool = msg_send![window, isVisible];
        visible.then_some(window)
    })
}

/// Every screen as (display ID, frame, visible frame).
unsafe fn screens() -> Vec<(u32, NSRect, NSRect)> {
    let Some(screen_class) = AnyClass::get(c"NSScreen") else { return vec![] };
    let screens: *mut AnyObject = msg_send![screen_class, screens];
    let Some(screens) = screens.as_ref() else { return vec![] };
    let count: usize = msg_send![screens, count];
    let key = NSString::from_str("NSScreenNumber");
    (0..count)
        .filter_map(|i| {
            let screen: *mut AnyObject = msg_send![screens, objectAtIndex: i];
            let screen = screen.as_ref()?;
            let description: *mut AnyObject = msg_send![screen, deviceDescription];
            let number: *mut AnyObject = msg_send![description.as_ref()?, objectForKey: &*key];
            let id: u32 = msg_send![number.as_ref()?, unsignedIntValue];
            let frame: NSRect = msg_send![screen, frame];
            let visible: NSRect = msg_send![screen, visibleFrame];
            Some((id, frame, visible))
        })
        .collect()
}

unsafe fn screen_frame(display: u32) -> Option<NSRect> {
    screens().into_iter().find(|s| s.0 == display).map(|s| s.1)
}

unsafe fn visible_frame(display: u32) -> Option<NSRect> {
    screens().into_iter().find(|s| s.0 == display).map(|s| s.2)
}

/// The display that holds the middle of `frame`.
unsafe fn screen_of(frame: NSRect) -> Option<(u32, NSRect)> {
    let (x, y) = (frame.origin.x + frame.size.width / 2.0, frame.origin.y + frame.size.height / 2.0);
    screens().into_iter().find_map(|(id, f, _)| {
        let inside = x >= f.origin.x && x < f.origin.x + f.size.width && y >= f.origin.y && y < f.origin.y + f.size.height;
        inside.then_some((id, f))
    })
}
