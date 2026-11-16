//! The ScreenCaptureKit calls that the desktop view and the game sound share: a display to
//! capture and the start of a stream. Both need Screen Recording, which macOS asks for on first use.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{SCDisplay, SCShareableContent, SCStream};
use std::{sync::mpsc, time::Duration};

/// Lets ScreenCaptureKit objects cross threads. They are thread safe for the calls made here.
pub struct SendBox<T>(pub T);
unsafe impl<T> Send for SendBox<T> {}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGMainDisplayID() -> u32;
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
}

/// The main display's ID.
pub fn main_display_id() -> u32 {
    unsafe { CGMainDisplayID() }
}

/// The IDs of the displays that draw: a display mirrored onto another is not one of them.
pub fn active_displays() -> Vec<u32> {
    let mut ids = [0u32; 32];
    let mut count = 0;
    if unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) } != 0 {
        return Vec::new();
    }
    ids[..count as usize].to_vec()
}

/// The main display, or the first one when ScreenCaptureKit does not list the main display.
pub fn main_display() -> Result<Retained<SCDisplay>, String> {
    display(None)
}

/// The display with this ID, or the main one. A display that macOS just added can take a moment
/// to show up in ScreenCaptureKit's list, so the lookup tries for a few seconds.
pub fn display(id: Option<u32>) -> Result<Retained<SCDisplay>, String> {
    for _ in 0..30 {
        match find_display(id)? {
            Some(display) => return Ok(display),
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    Err(format!("no display {} to capture", id.unwrap_or(0)))
}

fn find_display(id: Option<u32>) -> Result<Option<Retained<SCDisplay>>, String> {
    let (sender, receiver) = mpsc::channel();
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        let result = match unsafe { Retained::retain(content) } {
            Some(c) => Ok(SendBox(c)),
            None => Err(describe(error)),
        };
        sender.send(result).ok();
    });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
    let content = receiver
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| "ScreenCaptureKit did not answer".to_string())?
        .map_err(|e| format!("no screen access ({e}); allow cranvr under Screen Recording in System Settings"))?
        .0;
    let displays = unsafe { content.displays() };
    Ok(match id {
        Some(id) => displays.iter().find(|d| unsafe { d.displayID() } == id),
        None => {
            let main_id = unsafe { CGMainDisplayID() };
            displays.iter().find(|d| unsafe { d.displayID() } == main_id).or_else(|| displays.firstObject())
        }
    })
}

/// Starts `stream` and waits until it runs.
pub fn start(stream: &SCStream) -> Result<(), String> {
    let (sender, receiver) = mpsc::channel();
    let started = RcBlock::new(move |error: *mut NSError| {
        sender.send(if error.is_null() { Ok(()) } else { Err(describe(error)) }).ok();
    });
    unsafe { stream.startCaptureWithCompletionHandler(Some(&started)) };
    receiver
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| "the capture did not start".to_string())?
        .map_err(|e| format!("the capture did not start: {e}"))
}

pub fn describe(error: *mut NSError) -> String {
    match unsafe { error.as_ref() } {
        Some(e) => e.localizedDescription().to_string(),
        None => "unknown error".into(),
    }
}
