//! The Mac desktop in VR. ScreenCaptureKit captures the main display, the renderer shows the
//! newest capture on a flat screen in front of the user, and the right controller works as a
//! mouse: point to move the cursor, pull the trigger to click, push the thumbstick to scroll.
//!
//! macOS asks for two permissions on first use: Screen Recording (for the capture) and
//! Accessibility (for the mouse events). Without Accessibility the screen still shows.

use crate::pose::{Pose, Quat, Vec3};
use log::warn;
use cranvr_vt::PixelBuffer;
use crate::screen_capture::{self, SendBox};
use dispatch2::DispatchQueue;
use objc2::{define_class, msg_send, rc::Retained, runtime::ProtocolObject, AllocAnyThread, DefinedClass};
use objc2_core_media::{CMSampleBuffer, CMTime, CMTimeFlags};
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{SCContentFilter, SCStream, SCStreamConfiguration, SCStreamOutput, SCStreamOutputType};
use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::SyncSender,
        Arc, Mutex, OnceLock,
    },
};

/// The newest captured frame, numbered from 1.
type Latest = Arc<Mutex<Option<(u64, PixelBuffer)>>>;

/// What the capture output keeps: the newest frame, and a channel that hears of each new one.
pub struct OutputState {
    latest: Latest,
    notify: Option<SyncSender<()>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "CranvrDesktopOutput"]
    #[ivars = OutputState]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            // Frames without changes carry no image
            let image = unsafe { CMSampleBufferGetImageBuffer(sample as *const CMSampleBuffer as *const c_void) };
            if !image.is_null() {
                let state = self.ivars();
                let mut latest = state.latest.lock().unwrap();
                let seq = latest.as_ref().map_or(1, |(n, _)| n + 1);
                *latest = Some((seq, unsafe { PixelBuffer::retain_raw(image) }));
                drop(latest);
                if let Some(notify) = &state.notify {
                    notify.try_send(()).ok();
                }
            }
        }
    }
);

impl Output {
    fn new(state: OutputState) -> Retained<Self> {
        let this = Self::alloc().set_ivars(state);
        unsafe { msg_send![super(this), init] }
    }
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetImageBuffer(sample: *const c_void) -> *mut c_void;
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGRect {
    origin: CGPoint,
    size: CGPoint,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGEventCreateMouseEvent(source: *const c_void, kind: u32, point: CGPoint, button: u32) -> *mut c_void;
    fn CGEventCreateScrollWheelEvent2(source: *const c_void, units: u32, count: u32, w1: i32, w2: i32, w3: i32) -> *mut c_void;
    fn CGEventPost(tap: u32, event: *mut c_void);
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *mut c_void);
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

/// The wait after a failed start or stop of a capture before the next try
const RETRY_AFTER: std::time::Duration = std::time::Duration::from_millis(200);
const K_CV_PIXEL_FORMAT_32BGRA: u32 = 0x4247_5241;
/// kCVPixelFormatType_420YpCbCr8BiPlanarFullRange: what the video encoder takes without a conversion
const K_CV_PIXEL_FORMAT_420_FULL: u32 = 0x3432_3066;

/// How a capture runs.
pub struct CaptureSpec {
    /// The widest capture in pixels; the display's own pixel width caps it
    pub max_width: usize,
    /// Captures per second at most; ScreenCaptureKit sends one only when the display changes
    pub fps: i32,
    /// Draw the mouse cursor into the capture
    pub cursor: bool,
    /// YUV 4:2:0 for the video encoder instead of BGRA for the renderer
    pub yuv: bool,
    /// Hears of each new frame
    pub notify: Option<SyncSender<()>>,
}

/// A running capture of the main display.
pub struct Capture {
    latest: Latest,
    _stream: SendBox<Retained<SCStream>>,
    _output: SendBox<Retained<Output>>,
    _queue: SendBox<dispatch2::DispatchRetained<DispatchQueue>>,
    pub display_id: u32,
    /// Width over height of the display
    pub aspect: f32,
    /// The capture's size in pixels
    pub size: (u32, u32),
    /// The state asked for last. A failed stop or start turns it back, so the next call retries.
    running: Arc<std::sync::atomic::AtomicBool>,
    /// When a start or stop failed last. The next try waits RETRY_AFTER: ScreenCaptureKit kept
    /// pipes open for each failed start, and a display that left made the host try every 10 ms.
    failed_at: Arc<Mutex<Option<std::time::Instant>>>,
    /// The display's size in points
    pub points: (f64, f64),
}

fn configuration((width, height): (usize, usize), yuv: bool, cursor: bool, fps: i32) -> Retained<SCStreamConfiguration> {
    let config = unsafe { SCStreamConfiguration::new() };
    unsafe {
        config.setWidth(width);
        config.setHeight(height);
        config.setPixelFormat(if yuv { K_CV_PIXEL_FORMAT_420_FULL } else { K_CV_PIXEL_FORMAT_32BGRA });
        config.setMinimumFrameInterval(CMTime { value: 1, timescale: fps.max(1), flags: CMTimeFlags::Valid, epoch: 0 });
        config.setQueueDepth(4);
        config.setShowsCursor(cursor);
    }
    config
}

impl Capture {
    /// Captures a display (the main one without `display_id`) at up to `max_width` pixels wide,
    /// up to `fps` times per second (only when something on the display changes).
    pub fn start(display_id: Option<u32>, max_width: usize, fps: i32) -> Result<Self, String> {
        Self::start_with(display_id, CaptureSpec { max_width, fps, cursor: true, yuv: false, notify: None })
    }

    /// A capture as `spec` describes it.
    pub fn start_with(display_id: Option<u32>, spec: CaptureSpec) -> Result<Self, String> {
        let CaptureSpec { max_width, fps, cursor, yuv, notify } = spec;
        let display = screen_capture::display(display_id)?;
        let main_id = unsafe { display.displayID() };
        let (points_w, points_h) = unsafe { (display.width() as usize, display.height() as usize) };
        let width = (points_w * 2).min(max_width) & !1;
        let height = (width * points_h / points_w.max(1)) & !1;

        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &NSArray::new())
        };
        let config = configuration((width, height), yuv, cursor, fps);
        let stream = unsafe { SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, None) };
        let latest: Latest = Arc::default();
        let output = Output::new(OutputState { latest: Arc::clone(&latest), notify });
        let queue = DispatchQueue::new("cranvr.desktop", None);
        unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*output),
                SCStreamOutputType::Screen,
                Some(&queue),
            )
        }
        .map_err(|e| format!("capture output: {}", e.localizedDescription()))?;

        screen_capture::start(&stream)?;

        Ok(Self {
            latest,
            _stream: SendBox(stream),
            _output: SendBox(output),
            _queue: SendBox(queue),
            display_id: main_id,
            aspect: width as f32 / height.max(1) as f32,
            size: (width as u32, height as u32),
            running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            failed_at: Arc::new(Mutex::new(None)),
            points: (points_w as f64, points_h as f64),
        })
    }

    /// Stops or restarts the capture. A stopped capture costs the Mac nothing, and `latest` keeps
    /// the last frame. The call does not wait for ScreenCaptureKit.
    pub fn set_running(&self, run: bool) {
        if self.running.load(std::sync::atomic::Ordering::Relaxed) == run {
            return;
        }
        if self.failed_at.lock().unwrap().is_some_and(|at| at.elapsed() < RETRY_AFTER) {
            return;
        }
        self.running.store(run, std::sync::atomic::Ordering::Relaxed);
        // A start right after a stop may fail while the stop still runs
        let running = Arc::clone(&self.running);
        let failed_at = Arc::clone(&self.failed_at);
        let done = block2::RcBlock::new(move |error: *mut objc2_foundation::NSError| {
            // The first few failures go to the log: the retries come every few milliseconds
            static LOGGED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            *failed_at.lock().unwrap() = (!error.is_null()).then(std::time::Instant::now);
            if !error.is_null() {
                running.store(!run, std::sync::atomic::Ordering::Relaxed);
                if LOGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 5 {
                    warn!("Desktop: the capture did not {}: {}", if run { "start" } else { "stop" }, screen_capture::describe(error));
                }
            }
        });
        unsafe {
            if run {
                self._stream.0.startCaptureWithCompletionHandler(Some(&done));
            } else {
                self._stream.0.stopCaptureWithCompletionHandler(Some(&done));
            }
        }
    }

    /// The newest frame of the display and its number, once the first one arrived.
    pub fn latest(&self) -> Option<(u64, PixelBuffer)> {
        self.latest.lock().unwrap().clone()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if self.running.load(std::sync::atomic::Ordering::Relaxed) {
            unsafe { self._stream.0.stopCaptureWithCompletionHandler(None) };
        }
    }
}


/// A flat screen in the world: its center, and vectors from the center to the right edge and to
/// the top edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Screen {
    pub center: Vec3,
    pub right: Vec3,
    pub up: Vec3,
}

/// How far in front of the user a new screen goes, in meters.
const SCREEN_DISTANCE: f32 = 1.4;
/// The width of the screen, in meters. At 1.4 m this covers about 75 degrees.
const SCREEN_WIDTH: f32 = 2.2;

impl Screen {
    /// Upright, facing the user, at eye height in the direction the head looks.
    pub fn in_front_of(head: &Pose, aspect: f32) -> Self {
        let forward = head.orientation * Vec3::NEG_Z;
        let flat = Vec3::new(forward.x, 0.0, forward.z).try_normalize().unwrap_or(Vec3::NEG_Z);
        let right = Vec3::new(-flat.z, 0.0, flat.x);
        Self {
            center: head.position + flat * SCREEN_DISTANCE,
            right: right * (SCREEN_WIDTH / 2.0),
            up: Vec3::Y * (SCREEN_WIDTH / aspect / 2.0),
        }
    }

    /// Square to the direction the head looks, tilt included, at the same distance. Tests use it:
    /// a headset that lies on a desk looks down, so an upright screen would be out of view.
    pub fn in_view_of(head: &Pose, aspect: f32) -> Self {
        Self {
            center: head.position + head.orientation * Vec3::NEG_Z * SCREEN_DISTANCE,
            right: head.orientation * Vec3::X * (SCREEN_WIDTH / 2.0),
            up: head.orientation * Vec3::Y * (SCREEN_WIDTH / aspect / 2.0),
        }
    }

    /// Where a ray hits the screen, as (u, v) from the top left corner (0, 0) to the bottom
    /// right corner (1, 1).
    pub fn hit(&self, origin: Vec3, direction: Vec3) -> Option<[f32; 2]> {
        let normal = self.right.cross(self.up);
        let facing = direction.dot(normal);
        if facing.abs() < 1e-6 {
            return None;
        }
        let t = (self.center - origin).dot(normal) / facing;
        if t <= 0.0 {
            return None;
        }
        let h = origin + direction * t - self.center;
        let x = h.dot(self.right) / self.right.length_squared();
        let y = h.dot(self.up) / self.up.length_squared();
        (x.abs() <= 1.0 && y.abs() <= 1.0).then_some([(x + 1.0) / 2.0, (1.0 - y) / 2.0])
    }

    /// For the shader: center, right and up, each padded to four floats; w = 1 marks a screen.
    pub fn to_params(self) -> [[f32; 4]; 3] {
        let p = |v: Vec3, w| [v.x, v.y, v.z, w];
        [p(self.center, 1.0), p(self.right, 0.0), p(self.up, 0.0)]
    }
}

/// The direction a controller points, from its grip pose. The grip pose's -Z axis runs along the
/// handle; the pointing ray of a Touch controller goes out lower, by `pitch_deg`.
pub fn pointing_ray(grip: &Pose, pitch_deg: f32) -> (Vec3, Vec3) {
    let tilt = Quat::from_rotation_x(pitch_deg.to_radians());
    (grip.position, grip.orientation * tilt * Vec3::NEG_Z)
}

/// The direction a controller points, from its aim pose: the pose's -Z axis, as OpenXR defines it.
/// A link whose headset reports an aim pose needs no `pitch_deg`.
pub fn aim_ray(aim: &Pose) -> (Vec3, Vec3) {
    (aim.position, aim.orientation * Vec3::NEG_Z)
}

/// Turns pointing, trigger and thumbstick into mouse events on one display.
pub struct Pointer {
    display_id: u32,
    pressed: bool,
    last: Option<CGPoint>,
    scroll: f32,
    trusted: bool,
    /// When the Accessibility permission was last checked: the user may allow it while the
    /// host runs
    checked: std::time::Instant,
}

// CGEventType and CGMouseButton values
const LEFT_MOUSE_DOWN: u32 = 1;
const LEFT_MOUSE_UP: u32 = 2;
const MOUSE_MOVED: u32 = 5;
const LEFT_MOUSE_DRAGGED: u32 = 6;
const HID_EVENT_TAP: u32 = 0;
const SCROLL_UNITS_PIXEL: u32 = 0;

impl Pointer {
    pub fn new(display_id: u32) -> Self {
        let trusted = unsafe { AXIsProcessTrusted() };
        if !trusted {
            warn!("Desktop: no Accessibility permission, so the controller cannot move the mouse. Allow cranvr-host (or the terminal running it) under Privacy & Security > Accessibility.");
        }
        Self { display_id, pressed: false, last: None, scroll: 0.0, trusted, checked: std::time::Instant::now() }
    }

    /// Points on another display from now on.
    pub fn set_display(&mut self, display_id: u32) {
        if display_id != self.display_id {
            (self.display_id, self.last) = (display_id, None);
        }
    }

    /// Call once per frame. `hit` is where the controller points on the screen, if anywhere.
    pub fn update(&mut self, hit: Option<[f32; 2]>, trigger: f32, stick_y: f32) {
        self.update_scaled(hit, trigger, stick_y, 1.0);
    }

    /// Like `update`, with the scroll speed scaled: the scroll of one call at full tilt is 20
    /// pixels, which suits 72 calls a second. A caller that updates at another rate passes
    /// `72 / rate`, or the time since its last call times 72.
    pub fn update_scaled(&mut self, hit: Option<[f32; 2]>, trigger: f32, stick_y: f32, scroll_scale: f32) {
        if !self.trusted && self.checked.elapsed() > std::time::Duration::from_secs(2) {
            self.checked = std::time::Instant::now();
            self.trusted = unsafe { AXIsProcessTrusted() };
            if self.trusted {
                log::info!("Desktop: Accessibility allowed, the controller moves the mouse");
            }
        }
        // A dry run goes through all the logic and posts no event, so a test with a controller on
        // a desk never moves the user's real mouse
        if !self.trusted && !dry_run() {
            return;
        }
        let bounds = unsafe { CGDisplayBounds(self.display_id) };
        if let Some([u, v]) = hit {
            let point = CGPoint {
                x: bounds.origin.x + u as f64 * bounds.size.x,
                y: bounds.origin.y + v as f64 * bounds.size.y,
            };
            // A controller at rest shakes by a point or two: it moves the mouse only when it
            // points 3 points away, so the Mac's own mouse wins over a resting controller. A
            // pressed button follows every point, for drags.
            let step = if self.pressed { 1.0 } else { 3.0 };
            let moved = self.last.is_none_or(|l| (l.x - point.x).abs() + (l.y - point.y).abs() >= step);
            if moved {
                post_mouse(if self.pressed { LEFT_MOUSE_DRAGGED } else { MOUSE_MOVED }, point);
                self.last = Some(point);
            }
        }
        // Hysteresis, so a half pulled trigger does not click many times
        let press = if self.pressed { trigger > 0.4 } else { trigger > 0.6 };
        if press != self.pressed {
            if let Some(point) = self.last {
                post_mouse(if press { LEFT_MOUSE_DOWN } else { LEFT_MOUSE_UP }, point);
            }
            self.pressed = press;
        }
        // Up to about 1500 pixels per second at full tilt (at 72 frames per second)
        if stick_y.abs() > 0.15 {
            self.scroll += stick_y * 20.0 * scroll_scale;
            let whole = self.scroll.trunc();
            if whole != 0.0 {
                self.scroll -= whole;
                post_scroll(whole as i32);
            }
        }
    }
}

/// What the pointer injected since the host started, or would have injected in a dry run
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InjectedCounts {
    pub moves: u64,
    pub downs: u64,
    pub ups: u64,
    /// Pixels scrolled, up and down summed by their sizes
    pub scroll_pixels: u64,
}

static MOVES: AtomicU64 = AtomicU64::new(0);
static DOWNS: AtomicU64 = AtomicU64::new(0);
static UPS: AtomicU64 = AtomicU64::new(0);
static SCROLL_PIXELS: AtomicU64 = AtomicU64::new(0);

/// The mouse events injected so far
pub fn injected_counts() -> InjectedCounts {
    InjectedCounts {
        moves: MOVES.load(Ordering::Relaxed),
        downs: DOWNS.load(Ordering::Relaxed),
        ups: UPS.load(Ordering::Relaxed),
        scroll_pixels: SCROLL_PIXELS.load(Ordering::Relaxed),
    }
}

/// Whether the pointer only logs its events: `CRANVR_POINTER_DRY_RUN=1`. The test host sets it,
/// because a controller on a desk must not click on the user's Mac.
fn dry_run() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var("CRANVR_POINTER_DRY_RUN").is_ok_and(|v| v == "1");
        if on {
            log::info!("Desktop: dry run, the pointer logs its mouse events and posts none");
        }
        on
    })
}

fn post_scroll(pixels: i32) {
    SCROLL_PIXELS.fetch_add(u64::from(pixels.unsigned_abs()), Ordering::Relaxed);
    if dry_run() {
        log::info!("Desktop (dry run): scroll {pixels} pixels");
        return;
    }
    unsafe {
        let event = CGEventCreateScrollWheelEvent2(std::ptr::null(), SCROLL_UNITS_PIXEL, 1, pixels, 0, 0);
        if !event.is_null() {
            CGEventPost(HID_EVENT_TAP, event);
            CFRelease(event);
        }
    }
}

fn post_mouse(kind: u32, point: CGPoint) {
    let counter = match kind {
        LEFT_MOUSE_DOWN => &DOWNS,
        LEFT_MOUSE_UP => &UPS,
        _ => &MOVES,
    };
    let count = counter.fetch_add(1, Ordering::Relaxed) + 1;
    if dry_run() {
        // A move comes up to 120 times a second: the first and every hundredth go to the log
        let name = match kind {
            LEFT_MOUSE_DOWN => "button down",
            LEFT_MOUSE_UP => "button up",
            LEFT_MOUSE_DRAGGED => "drag",
            _ => "move",
        };
        if matches!(kind, LEFT_MOUSE_DOWN | LEFT_MOUSE_UP) || count == 1 || count % 100 == 0 {
            log::info!("Desktop (dry run): mouse {name} at ({:.0}, {:.0}) points, event {count} of its kind", point.x, point.y);
        }
        return;
    }
    unsafe {
        let event = CGEventCreateMouseEvent(std::ptr::null(), kind, point, 0);
        if !event.is_null() {
            CGEventPost(HID_EVENT_TAP, event);
            CFRelease(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs Screen Recording for the app that runs the test and a display that changes, such as
    /// a playing video. The layer link stops and starts its capture within 10 ms when the screen
    /// leaves the view and comes back.
    #[test]
    #[ignore]
    fn capture_resumes_after_a_quick_stop_and_start() {
        let wait_past = |capture: &Capture, seq: u64| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while std::time::Instant::now() < deadline {
                if let Some((n, _)) = capture.latest().filter(|(n, _)| *n > seq) {
                    return n;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("no frame after {seq}");
        };
        let spec = CaptureSpec { max_width: 640, fps: 60, cursor: true, yuv: true, notify: None };
        let capture = Capture::start_with(None, spec).unwrap();
        let mut seq = wait_past(&capture, 0);
        for gap_ms in [10, 0, 1, 10] {
            capture.set_running(false);
            std::thread::sleep(std::time::Duration::from_millis(gap_ms));
            capture.set_running(true);
            // a failed start turns the state back; the layer link calls again every 10 ms
            for _ in 0..50 {
                std::thread::sleep(std::time::Duration::from_millis(10));
                capture.set_running(true);
            }
            seq = wait_past(&capture, seq);
        }
    }

    #[test]
    fn screen_faces_the_user_and_maps_corners() {
        let head = Pose { orientation: Quat::IDENTITY, position: Vec3::new(0.0, 1.6, 0.0) };
        let screen = Screen::in_front_of(&head, 16.0 / 10.0);
        assert!((screen.center - Vec3::new(0.0, 1.6, -SCREEN_DISTANCE)).length() < 1e-5);
        // straight ahead hits the middle
        let mid = screen.hit(head.position, Vec3::NEG_Z).unwrap();
        assert!((mid[0] - 0.5).abs() < 1e-5 && (mid[1] - 0.5).abs() < 1e-5);
        // up and to the left hits the top left quarter
        let tl = screen.hit(head.position, Vec3::new(-0.3, 0.2, -1.0).normalize()).unwrap();
        assert!(tl[0] < 0.5 && tl[1] < 0.5);
        // behind and far to the side miss
        assert!(screen.hit(head.position, Vec3::Z).is_none());
        assert!(screen.hit(head.position, Vec3::new(-5.0, 0.0, -1.0).normalize()).is_none());
    }

    #[test]
    fn screen_follows_head_yaw_only() {
        let head = Pose { orientation: Quat::from_rotation_y(90f32.to_radians()) * Quat::from_rotation_x(-0.5), position: Vec3::ZERO };
        let screen = Screen::in_front_of(&head, 2.0);
        // turned left by 90 degrees: the screen is at -X, upright
        assert!((screen.center - Vec3::new(-SCREEN_DISTANCE, 0.0, 0.0)).length() < 1e-4);
        assert!((screen.up.normalize() - Vec3::Y).length() < 1e-5);
    }

    #[test]
    fn screen_in_view_follows_the_tilt() {
        // a headset that lies on a desk and looks 60 degrees down
        let head = Pose { orientation: Quat::from_rotation_x(-60f32.to_radians()), position: Vec3::new(0.0, 0.8, 0.0) };
        let screen = Screen::in_view_of(&head, 16.0 / 9.0);
        let mid = screen.hit(head.position, head.orientation * Vec3::NEG_Z).unwrap();
        assert!((mid[0] - 0.5).abs() < 1e-5 && (mid[1] - 0.5).abs() < 1e-5);
        assert!(screen.center.y < head.position.y);
    }

    #[test]
    fn an_aim_pose_points_along_its_minus_z_axis() {
        let aim = Pose { orientation: Quat::from_rotation_x(-30f32.to_radians()), position: Vec3::new(0.1, 1.2, -0.3) };
        let (origin, direction) = aim_ray(&aim);
        assert_eq!(origin, aim.position);
        assert!((direction - Vec3::new(0.0, -0.5, -0.866_025_4)).length() < 1e-5, "{direction:?}");
    }

    #[test]
    fn pointing_ray_tilts_down() {
        let grip = Pose { orientation: Quat::IDENTITY, position: Vec3::ZERO };
        let (_, d) = pointing_ray(&grip, -40.0);
        assert!(d.y < -0.6 && d.z < -0.7);
    }
}
