//! A display of this Mac that exists only for the headset. macOS treats it as one more monitor:
//! windows move onto it. The desktop view captures it. It lasts as long as the host.
//!
//! macOS has no public call for this. The classes are CoreGraphics' CGVirtualDisplay family,
//! which DeskPad and BetterDisplay use too; the host reaches them through the Objective-C
//! runtime, so a macOS without them only loses this feature.
//!
//! While a headset streams, the virtual display can be the main one. macOS then moves the menu
//! bar, the Dock and the windows of the main display into VR. New windows open there too. The change applies to this process only
//! (kCGConfigureForAppOnly), so macOS undoes it when the host ends.

use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::{
    msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject},
};
use objc2_foundation::{NSArray, NSSize, NSString};
use std::ffi::{c_void, CStr};

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGPoint,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGBeginDisplayConfiguration(config: *mut *mut c_void) -> i32;
    fn CGConfigureDisplayOrigin(config: *mut c_void, display: u32, x: i32, y: i32) -> i32;
    fn CGConfigureDisplayMirrorOfDisplay(config: *mut c_void, display: u32, master: u32) -> i32;
    fn CGCompleteDisplayConfiguration(config: *mut c_void, option: u32) -> i32;
    fn CGCancelDisplayConfiguration(config: *mut c_void) -> i32;
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayPixelsWide(display: u32) -> usize;
    fn CGDisplayPixelsHigh(display: u32) -> usize;
    fn CGDisplayCopyDisplayMode(display: u32) -> *const c_void;
    fn CGDisplayModeGetWidth(mode: *const c_void) -> usize;
    fn CGDisplayModeGetHeight(mode: *const c_void) -> usize;
    fn CGDisplayModeGetPixelWidth(mode: *const c_void) -> usize;
    fn CGDisplayModeGetPixelHeight(mode: *const c_void) -> usize;
    fn CGDisplayModeRelease(mode: *const c_void);
    fn CGConfigureDisplayWithDisplayMode(config: *mut c_void, display: u32, mode: *const c_void, options: *const c_void) -> i32;
}

/// A display's mode as macOS gave it.
struct DisplayMode(*const c_void);

impl DisplayMode {
    fn current(display: u32) -> Option<Self> {
        let mode = unsafe { CGDisplayCopyDisplayMode(display) };
        (!mode.is_null()).then_some(Self(mode))
    }

    /// Points and pixels
    fn size(&self) -> [usize; 4] {
        unsafe {
            [
                CGDisplayModeGetWidth(self.0),
                CGDisplayModeGetHeight(self.0),
                CGDisplayModeGetPixelWidth(self.0),
                CGDisplayModeGetPixelHeight(self.0),
            ]
        }
    }
}

impl Drop for DisplayMode {
    fn drop(&mut self) {
        unsafe { CGDisplayModeRelease(self.0) };
    }
}

const CONFIGURE_FOR_APP_ONLY: u32 = 0;
/// An arrangement that outlives the process. A helper's arrangement must: macOS undoes an
/// arrangement for the app only when the app ends, and while it removes the helper's display it
/// mirrored the main display onto it, at the virtual display's size.
const CONFIGURE_FOR_SESSION: u32 = 1;

/// The display's modes, in pixels. With HiDPI macOS also offers each one at half the size in
/// points, so 3840 x 2160 can look like 1920 x 1080 with sharp text.
const MODES: [(u32, u32); 4] = [(3840, 2160), (2560, 1440), (1920, 1080), (1280, 720)];

pub struct VirtualDisplay {
    _display: Retained<AnyObject>,
    _queue: DispatchRetained<DispatchQueue>,
    pub id: u32,
    /// Where each display sat before this one became the main display
    saved_origins: Option<Vec<(u32, i32, i32)>>,
}

// The display object is only kept alive; no call goes to it after creation.
unsafe impl Send for VirtualDisplay {}

impl VirtualDisplay {
    /// A display of `width` x `height` points, HiDPI, at `refresh_rate`. It goes to the right of
    /// the displays there are, as an extra screen: macOS by itself made a new display the main
    /// one and mirrored the others to it, which changed their resolution.
    pub fn create(width: u32, height: u32, refresh_rate: f64) -> Result<Self, String> {
        Self::create_numbered(width, height, refresh_rate, 1, CONFIGURE_FOR_APP_ONLY)
    }

    /// Like `create`. macOS tells displays apart by `serial`, so each display at a time needs its
    /// own. `option` says how long the arrangement lasts.
    fn create_numbered(width: u32, height: u32, refresh_rate: f64, serial: u32, option: u32) -> Result<Self, String> {
        let before: Vec<(u32, CGRect)> = active_displays().into_iter().map(|d| (d, unsafe { CGDisplayBounds(d) })).collect();
        let modes: Vec<(u32, DisplayMode)> = before.iter().filter_map(|&(d, _)| DisplayMode::current(d).map(|m| (d, m))).collect();
        let display = Self::create_display(width, height, refresh_rate, serial)?;
        // The display comes online a moment after its settings
        for _ in 0..40 {
            if active_displays().contains(&display.id) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // macOS keeps settings for each set of displays. For a set with this display it may bring
        // back an old mode of a display that was there: a TV set to 3840 x 2160 at 1x went back
        // to the scaled 3008 x 1692 mode it had with this display before. Each display gets its
        // mode back.
        let changed = |modes: &[(u32, DisplayMode)]| -> Vec<(u32, *const c_void)> {
            modes
                .iter()
                .filter(|(d, m)| DisplayMode::current(*d).is_some_and(|now| now.size() != m.size()))
                .map(|(d, m)| (*d, m.0))
                .collect()
        };
        for _ in 0..10 {
            if !changed(&modes).is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let restore = changed(&modes);
        let right = before.iter().map(|(_, b)| (b.origin.x + b.size.x) as i32).max().unwrap_or(0);
        let mut layout: Vec<(u32, i32, i32)> = before.iter().map(|(d, b)| (*d, b.origin.x as i32, b.origin.y as i32)).collect();
        layout.push((display.id, right, 0));
        // A display that macOS does not let us arrange may mirror the main display, and the main
        // display then drops to the virtual display's size: the display goes again
        // A mode that macOS refuses must not cost the new display
        apply(&layout, true, &restore, option).or_else(|_| apply(&layout, true, &[], option))?;
        Ok(display)
    }

    fn create_display(width: u32, height: u32, refresh_rate: f64, serial: u32) -> Result<Self, String> {
        let class = |name: &CStr| AnyClass::get(name).ok_or_else(|| format!("this macOS has no {}", name.to_string_lossy()));
        let descriptor_class = class(c"CGVirtualDisplayDescriptor")?;
        let display_class = class(c"CGVirtualDisplay")?;
        let settings_class = class(c"CGVirtualDisplaySettings")?;
        let mode_class = class(c"CGVirtualDisplayMode")?;
        let queue = DispatchQueue::new("cranvr.virtual-display", None);
        let (pixels_wide, pixels_high) = (width * 2, height * 2);
        unsafe {
            let descriptor: Retained<AnyObject> = msg_send![descriptor_class, new];
            let queue_object = DispatchRetained::as_ptr(&queue).as_ptr() as *mut AnyObject;
            // The setter's name differs between macOS versions
            let set_queue: bool = msg_send![&*descriptor, respondsToSelector: objc2::sel!(setQueue:)];
            if set_queue {
                let _: () = msg_send![&*descriptor, setQueue: queue_object];
            } else {
                let _: () = msg_send![&*descriptor, setDispatchQueue: queue_object];
            }
            let name = NSString::from_str("cranvr");
            let _: () = msg_send![&*descriptor, setName: &*name];
            let _: () = msg_send![&*descriptor, setMaxPixelsWide: pixels_wide];
            let _: () = msg_send![&*descriptor, setMaxPixelsHigh: pixels_high];
            // A 27-inch monitor's size: macOS uses it for the default scale
            let _: () = msg_send![&*descriptor, setSizeInMillimeters: NSSize::new(597.0, 336.0)];
            let _: () = msg_send![&*descriptor, setVendorID: 0x6372u32];
            let _: () = msg_send![&*descriptor, setProductID: 0x7672u32];
            let _: () = msg_send![&*descriptor, setSerialNum: serial];

            let allocated: *mut AnyObject = msg_send![display_class, alloc];
            let display: *mut AnyObject = msg_send![allocated, initWithDescriptor: &*descriptor];
            let display = Retained::from_raw(display).ok_or("macOS did not create the virtual display")?;

            let mut modes = vec![(pixels_wide, pixels_high)];
            modes.extend(MODES.iter().copied().filter(|&(w, h)| w <= pixels_wide && h <= pixels_high && (w, h) != (pixels_wide, pixels_high)));
            let modes: Vec<Retained<AnyObject>> = modes
                .into_iter()
                .filter_map(|(w, h)| {
                    let allocated: *mut AnyObject = msg_send![mode_class, alloc];
                    let mode: *mut AnyObject = msg_send![allocated, initWithWidth: w, height: h, refreshRate: refresh_rate];
                    Retained::from_raw(mode)
                })
                .collect();
            let settings: Retained<AnyObject> = msg_send![settings_class, new];
            let _: () = msg_send![&*settings, setHiDPI: 1u32];
            let modes = NSArray::from_retained_slice(&modes);
            let _: () = msg_send![&*settings, setModes: &*modes];
            let applied: bool = msg_send![&*display, applySettings: &*settings];
            if !applied {
                return Err("macOS refused the virtual display's modes".into());
            }
            let id: u32 = msg_send![&*display, displayID];
            if id == 0 {
                return Err("the virtual display has no ID".into());
            }
            Ok(Self { _display: display, _queue: queue, id, saved_origins: None })
        }
    }

    /// The display's size in points and in pixels, as macOS set it up.
    pub fn describe(&self) -> String {
        let bounds = unsafe { CGDisplayBounds(self.id) };
        let (w, h) = unsafe { (CGDisplayPixelsWide(self.id), CGDisplayPixelsHigh(self.id)) };
        format!("{} x {} points at ({}, {}), {w} x {h} pixels", bounds.size.x, bounds.size.y, bounds.origin.x, bounds.origin.y)
    }

    /// Whether `make_main` moved the displays and `restore` has not put them back yet.
    pub fn is_main(&self) -> bool {
        self.saved_origins.is_some()
    }

    /// Makes this display the main one: it moves to the origin, the others keep their places
    /// around it.
    pub fn make_main(&mut self) -> Result<(), String> {
        if unsafe { CGMainDisplayID() } == self.id {
            return Ok(());
        }
        let displays = active_displays();
        let own = unsafe { CGDisplayBounds(self.id) }.origin;
        let saved: Vec<(u32, i32, i32)> = displays
            .iter()
            .map(|&d| {
                let o = unsafe { CGDisplayBounds(d) }.origin;
                (d, o.x as i32, o.y as i32)
            })
            .collect();
        let moved: Vec<(u32, i32, i32)> = saved.iter().map(|&(d, x, y)| (d, x - own.x as i32, y - own.y as i32)).collect();
        configure(&moved)?;
        self.saved_origins.get_or_insert(saved);
        Ok(())
    }

    /// Puts every display back where it was before `make_main`.
    pub fn restore(&mut self) -> Result<(), String> {
        match self.saved_origins.take() {
            Some(saved) => configure(&saved),
            None => Ok(()),
        }
    }
}

fn active_displays() -> Vec<u32> {
    let mut ids = [0u32; 16];
    let mut count = 0u32;
    unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    ids[..count as usize].to_vec()
}

fn configure(origins: &[(u32, i32, i32)]) -> Result<(), String> {
    apply(origins, false, &[], CONFIGURE_FOR_APP_ONLY)
}

/// Moves the displays to `origins` and sets the `modes`; with `unmirror` every display also stops
/// mirroring.
fn apply(origins: &[(u32, i32, i32)], unmirror: bool, modes: &[(u32, *const c_void)], option: u32) -> Result<(), String> {
    unsafe {
        let mut config = std::ptr::null_mut();
        if CGBeginDisplayConfiguration(&mut config) != 0 {
            return Err("macOS refused a display configuration".into());
        }
        for &(display, mode) in modes {
            if CGConfigureDisplayWithDisplayMode(config, display, mode, std::ptr::null()) != 0 {
                CGCancelDisplayConfiguration(config);
                return Err(format!("macOS refused the earlier mode of display {display}"));
            }
        }
        if unmirror {
            for &(display, _, _) in origins {
                // kCGNullDirectDisplay as the master: no mirroring
                CGConfigureDisplayMirrorOfDisplay(config, display, 0);
            }
        }
        for &(display, x, y) in origins {
            if CGConfigureDisplayOrigin(config, display, x, y) != 0 {
                CGCancelDisplayConfiguration(config);
                return Err(format!("macOS refused to move display {display}"));
            }
        }
        match CGCompleteDisplayConfiguration(config, option) {
            0 => Ok(()),
            e => Err(format!("macOS refused the display arrangement ({e})")),
        }
    }
}


/// A virtual display in a helper process: `cranvr-host --display-helper W H SERIAL`.
///
/// A process sees the displays as they were when it first asked, and macOS does not update that
/// view in the host, whose main thread runs no event loop. The host's second display then failed
/// to arrange, and macOS mirrored the main display onto it at the virtual display's size. A new
/// process sees the current displays, so each added display gets its own. The display lives as
/// long as the helper: dropping this ends the helper, and macOS removes the display at once.
pub struct DisplayProcess {
    child: std::process::Child,
    pub id: u32,
}

impl DisplayProcess {
    pub fn start(width: u32, height: u32, serial: u32) -> Result<Self, String> {
        use std::{
            io::{BufRead, BufReader},
            process::{Command, Stdio},
        };
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut child = Command::new(exe)
            .args(["--display-helper", &width.to_string(), &height.to_string(), &serial.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("no display helper: {e}"))?;
        let stdout = child.stdout.take().ok_or("the display helper has no output")?;
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(stdout).read_line(&mut line).ok();
            sender.send(line).ok();
        });
        let line = receiver.recv_timeout(std::time::Duration::from_secs(10)).unwrap_or_default();
        match line.trim().strip_prefix("display ").and_then(|id| id.parse().ok()) {
            Some(id) => Ok(Self { child, id }),
            None => {
                child.kill().ok();
                child.wait().ok();
                Err(line.trim().strip_prefix("error ").unwrap_or("the display helper did not answer").to_string())
            }
        }
    }

    /// The display's size in points and in pixels, as macOS set it up.
    pub fn describe(&self) -> String {
        let bounds = unsafe { CGDisplayBounds(self.id) };
        format!("{} x {} points at ({}, {})", bounds.size.x, bounds.size.y, bounds.origin.x, bounds.origin.y)
    }
}

impl Drop for DisplayProcess {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// The helper's side: makes the display, says its number on stdout and keeps it until stdin
/// closes, as when the host ends.
pub fn helper_main(args: &[String]) -> ! {
    use std::io::{Read, Write};
    let number = |i: usize| args.get(i).and_then(|a| a.parse::<u32>().ok());
    let (Some(width), Some(height), Some(serial)) = (number(0), number(1), number(2)) else {
        println!("error usage: --display-helper WIDTH HEIGHT SERIAL");
        std::process::exit(2);
    };
    match VirtualDisplay::create_numbered(width, height, 60.0, serial, CONFIGURE_FOR_SESSION) {
        Ok(display) => {
            println!("display {}", display.id);
            std::io::stdout().flush().ok();
            let mut rest = Vec::new();
            std::io::stdin().read_to_end(&mut rest).ok();
            drop(display);
            std::process::exit(0)
        }
        Err(e) => {
            println!("error {e}");
            std::process::exit(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Adds a display to this Mac for a moment: run it by hand. The host asks for the bounds of a
    /// display made by a helper after the host's own first look at the displays.
    #[test]
    #[ignore]
    fn the_host_sees_a_display_made_by_a_helper() {
        let before = active_displays();
        // The test binary is not cranvr-host: the helper is the release build of the host
        let host = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/release/cranvr-host");
        let mut child = std::process::Command::new(host)
            .args(["--display-helper", "1920", "1080", "402"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(child.stdout.take().unwrap()), &mut line).unwrap();
        let id: u32 = line.trim().strip_prefix("display ").unwrap().parse().unwrap();
        let bounds = unsafe { CGDisplayBounds(id) };
        child.kill().ok();
        child.wait().ok();
        assert!(!before.contains(&id));
        assert_eq!((bounds.size.x, bounds.size.y), (1920.0, 1080.0));
    }
}
