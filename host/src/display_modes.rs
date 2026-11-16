//! The Mac's displays in their panels' own pixels while a game runs.
//!
//! A scaled mode, such as 3008 x 1692 points on a 3840 x 2160 TV, makes macOS draw the desktop at
//! 6016 x 3384 pixels and shrink it to the panel, on every change of any window. A Metal trace with
//! Beat Saber showed WindowServer at 442 ms of GPU time per second in that mode and at 157 to 161
//! ms in 3840 x 2160 at 1x; the game's own GPU time per frame fell from about 11 ms to 8 ms.
//!
//! A helper process (`cranvr-host --unscale-displays`) sets each scaled display to a mode with
//! the panel's own pixels and at least as many points, so windows keep their places. It sets the
//! modes for itself only: when it ends, as when its input closes, macOS gives each display its
//! mode back. The host's own view of the displays goes stale (see virtual_display.rs), so a fresh
//! process does the work.

use std::ffi::c_void;

type CFTypeRef = *const c_void;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayCopyDisplayMode(display: u32) -> CFTypeRef;
    fn CGDisplayCopyAllDisplayModes(display: u32, options: CFTypeRef) -> CFTypeRef;
    fn CGDisplayModeGetWidth(mode: CFTypeRef) -> usize;
    fn CGDisplayModeGetHeight(mode: CFTypeRef) -> usize;
    fn CGDisplayModeGetPixelWidth(mode: CFTypeRef) -> usize;
    fn CGDisplayModeGetPixelHeight(mode: CFTypeRef) -> usize;
    fn CGDisplayModeGetRefreshRate(mode: CFTypeRef) -> f64;
    fn CGDisplayModeGetIOFlags(mode: CFTypeRef) -> u32;
    fn CGDisplayModeIsUsableForDesktopGUI(mode: CFTypeRef) -> bool;
    fn CGBeginDisplayConfiguration(config: *mut *mut c_void) -> i32;
    fn CGConfigureDisplayWithDisplayMode(config: *mut c_void, display: u32, mode: CFTypeRef, options: CFTypeRef) -> i32;
    fn CGCompleteDisplayConfiguration(config: *mut c_void, option: u32) -> i32;
    fn CGCancelDisplayConfiguration(config: *mut c_void) -> i32;
    static kCGDisplayShowDuplicateLowResolutionModes: CFTypeRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFArrayGetCount(array: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
    fn CFRelease(object: *mut c_void);
    fn CFDictionaryCreate(
        allocator: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    static kCFBooleanTrue: CFTypeRef;
}

/// The mode marked as the panel's own
const NATIVE_FLAG: u32 = 0x0200_0000;
/// A configuration for this process only: macOS undoes it when the process ends
const CONFIGURE_FOR_APP_ONLY: u32 = 0;

/// A display mode: points, pixels, refresh rate, whether it is the panel's own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mode {
    pub points: (usize, usize),
    pub pixels: (usize, usize),
    pub hz: f64,
    pub native: bool,
}

/// The mode to use instead of `current`, or none when `current` already has the panel's own
/// pixels. The new mode has the panel's pixels, at least the current points, so no window has to
/// move, and the refresh rate nearest the current one.
pub fn unscaled(current: Mode, modes: &[Mode]) -> Option<Mode> {
    // The panel's pixels: the mode marked as the panel's own, else the largest mode at 1x. A
    // scaled mode can have more pixels than the panel; a mode at 1x cannot.
    let panel = modes
        .iter()
        .find(|m| m.native)
        .or_else(|| modes.iter().filter(|m| m.points == m.pixels).max_by_key(|m| m.pixels.0 * m.pixels.1))?
        .pixels;
    if current.pixels == panel {
        return None;
    }
    modes
        .iter()
        .filter(|m| m.pixels == panel && m.points.0 >= current.points.0 && m.points.1 >= current.points.1)
        .min_by(|a, b| (a.hz - current.hz).abs().total_cmp(&(b.hz - current.hz).abs()))
        .copied()
}

unsafe fn mode_of(mode: CFTypeRef) -> Mode {
    Mode {
        points: (CGDisplayModeGetWidth(mode), CGDisplayModeGetHeight(mode)),
        pixels: (CGDisplayModeGetPixelWidth(mode), CGDisplayModeGetPixelHeight(mode)),
        hz: CGDisplayModeGetRefreshRate(mode),
        native: CGDisplayModeGetIOFlags(mode) & NATIVE_FLAG != 0,
    }
}

fn describe(m: &Mode) -> String {
    format!("{}x{} points ({}x{} pixels, {:.0} Hz)", m.points.0, m.points.1, m.pixels.0, m.pixels.1, m.hz)
}

/// Sets every scaled display to its panel's pixels for this process. Returns a line per change.
fn unscale_all() -> Vec<String> {
    let mut lines = Vec::new();
    unsafe {
        let mut ids = [0u32; 16];
        let mut count = 0u32;
        if CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut count) != 0 {
            return vec!["error the displays are not readable".into()];
        }
        let keys = [kCGDisplayShowDuplicateLowResolutionModes];
        let values = [kCFBooleanTrue];
        let options = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            std::ptr::addr_of!(kCFTypeDictionaryKeyCallBacks).cast(),
            std::ptr::addr_of!(kCFTypeDictionaryValueCallBacks).cast(),
        );
        for &display in &ids[..count as usize] {
            let current_ref = CGDisplayCopyDisplayMode(display);
            let all = CGDisplayCopyAllDisplayModes(display, options);
            if current_ref.is_null() || all.is_null() {
                for object in [current_ref, all] {
                    if !object.is_null() {
                        CFRelease(object.cast_mut());
                    }
                }
                continue;
            }
            let refs: Vec<CFTypeRef> = (0..CFArrayGetCount(all))
                .map(|i| CFArrayGetValueAtIndex(all, i))
                .filter(|&m| CGDisplayModeIsUsableForDesktopGUI(m))
                .collect();
            let modes: Vec<Mode> = refs.iter().map(|&m| mode_of(m)).collect();
            let current = mode_of(current_ref);
            if let Some(target) = unscaled(current, &modes) {
                let target_ref = refs[modes.iter().position(|m| *m == target).unwrap()];
                let mut config = std::ptr::null_mut();
                let result = if CGBeginDisplayConfiguration(&mut config) != 0 {
                    -1
                } else if CGConfigureDisplayWithDisplayMode(config, display, target_ref, std::ptr::null()) != 0 {
                    CGCancelDisplayConfiguration(config);
                    -2
                } else {
                    CGCompleteDisplayConfiguration(config, CONFIGURE_FOR_APP_ONLY)
                };
                lines.push(match result {
                    0 => format!("display {display}: {} instead of {}", describe(&target), describe(&current)),
                    e => format!("error display {display}: macOS refused {} ({e})", describe(&target)),
                });
            }
            CFRelease(all.cast_mut());
            CFRelease(current_ref.cast_mut());
        }
        CFRelease(options.cast_mut());
    }
    lines
}

/// The helper's side: sets the modes, reports them on stdout, then keeps them until stdin closes.
pub fn helper_main() -> ! {
    use std::io::{Read, Write};
    let lines = unscale_all();
    for line in &lines {
        println!("{line}");
    }
    println!("done");
    std::io::stdout().flush().ok();
    let mut rest = Vec::new();
    std::io::stdin().read_to_end(&mut rest).ok();
    std::process::exit(0)
}

/// The helper while a game runs. Dropping it closes the helper's input: the helper ends, and
/// macOS gives each display its mode back.
pub struct Unscaled {
    child: std::process::Child,
}

impl Unscaled {
    /// Starts the helper and logs what it changed.
    pub fn start() -> Result<Self, String> {
        use std::{
            io::{BufRead, BufReader},
            process::{Command, Stdio},
        };
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut child = Command::new(exe)
            .arg("--unscale-displays")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("no display mode helper: {e}"))?;
        let stdout = child.stdout.take().ok_or("the display mode helper has no output")?;
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                match line.as_str() {
                    "done" => break,
                    l if l.starts_with("error ") => alvr_common::warn!("Display modes: {}", &l[6..]),
                    l => alvr_common::info!("Display modes while the game runs: {l}"),
                }
            }
        });
        Ok(Self { child })
    }
}

impl Drop for Unscaled {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        for _ in 0..40 {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(points: (usize, usize), pixels: (usize, usize), hz: f64) -> Mode {
        Mode { points, pixels, hz, native: false }
    }

    #[test]
    fn a_scaled_tv_gets_its_panel_pixels_without_shrinking_the_desktop() {
        // The Hisense: 3008 x 1692 points drawn at 6016 x 3384 for a 3840 x 2160 panel
        let current = mode((3008, 1692), (6016, 3384), 60.0);
        let mut native = mode((3840, 2160), (3840, 2160), 60.0);
        native.native = true;
        let modes = [
            current,
            mode((1920, 1080), (3840, 2160), 60.0),
            mode((3840, 2160), (3840, 2160), 30.0),
            native,
            mode((1920, 1080), (1920, 1080), 60.0),
        ];
        // 1920 x 1080 HiDPI has the panel's pixels too, but fewer points: windows would move
        assert_eq!(unscaled(current, &modes), Some(native));
    }

    #[test]
    fn a_display_at_its_panel_pixels_stays() {
        let current = mode((1920, 1080), (3840, 2160), 60.0);
        let modes = [current, mode((3840, 2160), (3840, 2160), 60.0), mode((3008, 1692), (6016, 3384), 60.0)];
        assert_eq!(unscaled(current, &modes), None);
    }

    #[test]
    fn no_mode_with_room_for_the_windows_means_no_change() {
        let current = mode((2560, 1440), (5120, 2880), 60.0);
        let mut native = mode((1920, 1080), (3840, 2160), 60.0);
        native.native = true;
        assert_eq!(unscaled(current, &[current, native]), None);
    }

    #[test]
    fn without_a_native_mark_the_largest_mode_at_1x_names_the_panel() {
        let current = mode((3008, 1692), (6016, 3384), 60.0);
        let modes = [current, mode((1920, 1080), (3840, 2160), 60.0), mode((3840, 2160), (3840, 2160), 60.0), mode((1920, 1080), (1920, 1080), 60.0)];
        assert_eq!(unscaled(current, &modes), Some(mode((3840, 2160), (3840, 2160), 60.0)));
    }
}
