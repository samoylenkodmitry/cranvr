//! The Mac desktop in VR. ScreenCaptureKit captures the main display, the renderer shows the
//! newest capture on a flat screen in front of the user, and a controller works as a mouse: point
//! to move the cursor, press a button to click with the left, right or middle mouse button, lean the
//! thumbstick to scroll up, down and to the sides.
//!
//! macOS asks for two permissions on first use: Screen Recording (for the capture) and
//! Accessibility (for the mouse events). Without Accessibility the screen still shows.

use crate::pose::{Pose, Quat, Vec3};
use cranvr_common::controls::mouse::{LEFT, MIDDLE, RIGHT};
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
#[derive(Clone, Copy, Debug, Default, PartialEq)]
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
    fn CGEventSetIntegerValueField(event: *mut c_void, field: u32, value: i64);
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

/// Turns pointing, mouse buttons and thumbstick lean into mouse events on one display.
///
/// The Mac has one mouse. The client names the controller that owns it, so this type sees one
/// point, one set of buttons and one lean at a time.
pub struct Pointer {
    display_id: u32,
    /// The buttons that are down, as the bits of `cranvr_common::controls::mouse`
    held: u8,
    last: Option<CGPoint>,
    /// The scroll that is owed to the Mac, in pixels, across and forward
    scroll: [f32; 2],
    /// The last press, for the click count of a double click
    run: Option<ClickRun>,
    trusted: bool,
    /// When the Accessibility permission was last checked: the user may allow it while the
    /// host runs
    checked: std::time::Instant,
}

/// The presses of one button that follow each other closely, which the Mac counts as a double or a
/// triple click.
#[derive(Clone, Copy, Debug)]
struct ClickRun {
    bit: u8,
    at: std::time::Instant,
    point: CGPoint,
    count: i64,
}

/// A second press of the same button within this time and within `CLICK_REACH` points of the first
/// counts as the next click of a run. The Mac's own double click time is 0.5 s by default.
const CLICK_RUN: std::time::Duration = std::time::Duration::from_millis(500);
const CLICK_REACH: f64 = 4.0;

// CGEventType and CGMouseButton values
const LEFT_MOUSE_DOWN: u32 = 1;
const LEFT_MOUSE_UP: u32 = 2;
const RIGHT_MOUSE_DOWN: u32 = 3;
const RIGHT_MOUSE_UP: u32 = 4;
const MOUSE_MOVED: u32 = 5;
const LEFT_MOUSE_DRAGGED: u32 = 6;
const RIGHT_MOUSE_DRAGGED: u32 = 7;
const OTHER_MOUSE_DOWN: u32 = 25;
const OTHER_MOUSE_UP: u32 = 26;
const OTHER_MOUSE_DRAGGED: u32 = 27;
const BUTTON_LEFT: u32 = 0;
const BUTTON_RIGHT: u32 = 1;
const BUTTON_CENTER: u32 = 2;
const HID_EVENT_TAP: u32 = 0;
const SCROLL_UNITS_PIXEL: u32 = 0;
/// kCGMouseEventClickState: how many clicks of a run this press is
const MOUSE_EVENT_CLICK_STATE: u32 = 1;
/// A lean inside this share of the stick's range scrolls nothing
const SCROLL_DEAD_ZONE: f32 = 0.15;

impl Pointer {
    pub fn new(display_id: u32) -> Self {
        let trusted = unsafe { AXIsProcessTrusted() };
        if !trusted {
            warn!("Desktop: no Accessibility permission, so the controller cannot move the mouse. Allow cranvr-host (or the terminal running it) under Privacy & Security > Accessibility.");
        }
        Self { display_id, held: 0, last: None, scroll: [0.0; 2], run: None, trusted, checked: std::time::Instant::now() }
    }

    /// Points on another display from now on.
    pub fn set_display(&mut self, display_id: u32) {
        if display_id != self.display_id {
            (self.display_id, self.last) = (display_id, None);
        }
    }

    /// Call once per frame of the host's own scene, which has one controller with a trigger and a
    /// thumbstick. `hit` is where the controller points on the screen, if anywhere. The trigger is
    /// the left button, with a hysteresis so a half pulled trigger does not click many times, and
    /// the thumbstick scrolls up and down.
    pub fn update(&mut self, hit: Option<[f32; 2]>, trigger: f32, stick_y: f32) {
        self.update_mouse(hit, trigger_buttons(self.held, trigger), [0.0, stick_y], 1.0);
    }

    /// Call once per update of the client. `hit` is where the controller points on the screen, if
    /// anywhere. `buttons` holds the bits of the buttons that are down. `lean` is the thumbstick's
    /// lean across and forward, each from -1 to 1. The scroll of one call at full lean is 20
    /// pixels, which suits 72 calls a second: a caller that updates at another rate passes
    /// `scroll_scale` as `72 / rate`, or as the time since its last call times 72.
    pub fn update_mouse(&mut self, hit: Option<[f32; 2]>, buttons: u8, lean: [f32; 2], scroll_scale: f32) {
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
        for event in self.plan(bounds, hit, buttons, lean, scroll_scale, std::time::Instant::now()) {
            post(event);
        }
    }

    /// The mouse events that a change of the controller calls for, in the order in which they go
    /// out: the move, then each button that went down or up, then the scroll. The state of the
    /// pointer moves on as if the caller posted them.
    fn plan(&mut self, bounds: CGRect, hit: Option<[f32; 2]>, buttons: u8, lean: [f32; 2], scroll_scale: f32, now: std::time::Instant) -> Vec<Out> {
        let mut out = Vec::new();
        if let Some([u, v]) = hit {
            let point = CGPoint { x: bounds.origin.x + u as f64 * bounds.size.x, y: bounds.origin.y + v as f64 * bounds.size.y };
            // A controller at rest shakes by a point or two: it moves the mouse only when it
            // points 3 points away, so the Mac's own mouse wins over a resting controller. A
            // pressed button follows every point, for drags.
            let step = if self.held != 0 { 1.0 } else { 3.0 };
            let moved = self.last.is_none_or(|l| (l.x - point.x).abs() + (l.y - point.y).abs() >= step);
            if moved {
                // The Mac drags with the button that went down first
                let drag = [LEFT, RIGHT, MIDDLE].into_iter().find(|bit| self.held & bit != 0);
                out.push(Out::Move { drag, point });
                self.last = Some(point);
            }
        }
        for bit in [LEFT, RIGHT, MIDDLE] {
            let (was, is) = (self.held & bit != 0, buttons & bit != 0);
            if was == is {
                continue;
            }
            if let Some(point) = self.last {
                let clicks = if is { self.count_click(bit, point, now) } else { self.run.map_or(1, |run| run.count) };
                out.push(Out::Button { bit, down: is, point, clicks });
            }
        }
        // A button that a press without a point never posted still counts as down: its release
        // posts nothing either, because no point exists to post it at
        self.held = buttons & (LEFT | RIGHT | MIDDLE);
        // Up to about 1500 pixels per second at full lean (at 72 frames per second)
        let mut whole = [0i32; 2];
        for axis in 0..2 {
            if lean[axis].abs() > SCROLL_DEAD_ZONE {
                self.scroll[axis] += lean[axis] * 20.0 * scroll_scale;
                let trunc = self.scroll[axis].trunc();
                self.scroll[axis] -= trunc;
                whole[axis] = trunc as i32;
            }
        }
        if whole != [0, 0] {
            out.push(Out::Scroll { across: whole[0], forward: whole[1] });
        }
        out
    }

    /// The click count of a press of `bit` at `point`: 1 for a first click and one more for each
    /// press of the same button that follows the last within `CLICK_RUN` and `CLICK_REACH` points.
    fn count_click(&mut self, bit: u8, point: CGPoint, now: std::time::Instant) -> i64 {
        let count = match self.run {
            Some(run) if run.bit == bit && now.duration_since(run.at) <= CLICK_RUN && (run.point.x - point.x).abs() + (run.point.y - point.y).abs() <= CLICK_REACH => {
                run.count + 1
            }
            _ => 1,
        };
        self.run = Some(ClickRun { bit, at: now, point, count });
        count
    }
}

/// The buttons that a trigger holds, for the host's own scene: the trigger is the left button. It goes down past 0.6 and up below 0.4, so a trigger that rests near one value does not
/// click many times. `held` is what is down now.
fn trigger_buttons(held: u8, trigger: f32) -> u8 {
    let down = if held & LEFT != 0 { trigger > 0.4 } else { trigger > 0.6 };
    if down {
        LEFT
    } else {
        0
    }
}

/// One mouse event that the pointer posts.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Out {
    /// The mouse moves to `point`. `drag` is the button that is down, if one is.
    Move { drag: Option<u8>, point: CGPoint },
    Button { bit: u8, down: bool, point: CGPoint, clicks: i64 },
    /// Pixels to the right and forward (up), as a thumbstick leans. A negative number goes the
    /// other way.
    Scroll { across: i32, forward: i32 },
}

/// What the pointer injected since the host started, or would have injected in a dry run
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InjectedCounts {
    pub moves: u64,
    /// Button presses and releases of every button. The two lists below say how many belong to the
    /// right and the middle button.
    pub downs: u64,
    pub ups: u64,
    pub right_downs: u64,
    pub middle_downs: u64,
    /// Pixels scrolled forward and back, up and down summed by their sizes
    pub scroll_pixels: u64,
    /// Pixels scrolled to the sides
    pub scroll_across_pixels: u64,
}

static MOVES: AtomicU64 = AtomicU64::new(0);
static DOWNS: AtomicU64 = AtomicU64::new(0);
static UPS: AtomicU64 = AtomicU64::new(0);
static RIGHT_DOWNS: AtomicU64 = AtomicU64::new(0);
static MIDDLE_DOWNS: AtomicU64 = AtomicU64::new(0);
static SCROLL_PIXELS: AtomicU64 = AtomicU64::new(0);
static SCROLL_ACROSS_PIXELS: AtomicU64 = AtomicU64::new(0);

/// The mouse events injected so far
pub fn injected_counts() -> InjectedCounts {
    InjectedCounts {
        moves: MOVES.load(Ordering::Relaxed),
        downs: DOWNS.load(Ordering::Relaxed),
        ups: UPS.load(Ordering::Relaxed),
        right_downs: RIGHT_DOWNS.load(Ordering::Relaxed),
        middle_downs: MIDDLE_DOWNS.load(Ordering::Relaxed),
        scroll_pixels: SCROLL_PIXELS.load(Ordering::Relaxed),
        scroll_across_pixels: SCROLL_ACROSS_PIXELS.load(Ordering::Relaxed),
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

/// The name of a button for the log
fn button_name(bit: u8) -> &'static str {
    match bit {
        LEFT => "left",
        RIGHT => "right",
        _ => "middle",
    }
}

/// The Core Graphics event for a mouse event, or null when the system gives none. The caller posts
/// it and releases it.
fn build(event: Out) -> *mut c_void {
    unsafe {
        match event {
            Out::Move { drag, point } => {
                let (kind, button) = match drag {
                    Some(LEFT) => (LEFT_MOUSE_DRAGGED, BUTTON_LEFT),
                    Some(RIGHT) => (RIGHT_MOUSE_DRAGGED, BUTTON_RIGHT),
                    Some(_) => (OTHER_MOUSE_DRAGGED, BUTTON_CENTER),
                    None => (MOUSE_MOVED, BUTTON_LEFT),
                };
                CGEventCreateMouseEvent(std::ptr::null(), kind, point, button)
            }
            Out::Button { bit, down, point, clicks } => {
                let (kind, button) = match (bit, down) {
                    (LEFT, true) => (LEFT_MOUSE_DOWN, BUTTON_LEFT),
                    (LEFT, false) => (LEFT_MOUSE_UP, BUTTON_LEFT),
                    (RIGHT, true) => (RIGHT_MOUSE_DOWN, BUTTON_RIGHT),
                    (RIGHT, false) => (RIGHT_MOUSE_UP, BUTTON_RIGHT),
                    (_, true) => (OTHER_MOUSE_DOWN, BUTTON_CENTER),
                    (_, false) => (OTHER_MOUSE_UP, BUTTON_CENTER),
                };
                let event = CGEventCreateMouseEvent(std::ptr::null(), kind, point, button);
                if !event.is_null() {
                    CGEventSetIntegerValueField(event, MOUSE_EVENT_CLICK_STATE, clicks);
                }
                event
            }
            // The first wheel scrolls up for a positive number. The second wheel scrolls to the
            // left for a positive number, so a lean to the right is a negative one.
            Out::Scroll { across, forward } => CGEventCreateScrollWheelEvent2(std::ptr::null(), SCROLL_UNITS_PIXEL, 2, forward, -across, 0),
        }
    }
}

fn post(event: Out) {
    // The counts and the log of a dry run come from the plan, not from the event
    match event {
        Out::Move { drag, point } => {
            let count = MOVES.fetch_add(1, Ordering::Relaxed) + 1;
            if dry_run() {
                // A move comes up to 120 times a second: the first and every hundredth go to the log
                if count == 1 || count.is_multiple_of(100) {
                    let name = drag.map_or("move".to_string(), |bit| format!("{} drag", button_name(bit)));
                    log::info!("Desktop (dry run): mouse {name} at ({:.0}, {:.0}) points, event {count} of its kind", point.x, point.y);
                }
                return;
            }
        }
        Out::Button { bit, down, point, clicks } => {
            (if down { &DOWNS } else { &UPS }).fetch_add(1, Ordering::Relaxed);
            match (bit, down) {
                (RIGHT, true) => drop(RIGHT_DOWNS.fetch_add(1, Ordering::Relaxed)),
                (MIDDLE, true) => drop(MIDDLE_DOWNS.fetch_add(1, Ordering::Relaxed)),
                _ => {}
            }
            if dry_run() {
                log::info!(
                    "Desktop (dry run): {} button {} at ({:.0}, {:.0}) points, click {clicks}",
                    button_name(bit),
                    if down { "down" } else { "up" },
                    point.x,
                    point.y
                );
                return;
            }
        }
        Out::Scroll { across, forward } => {
            SCROLL_PIXELS.fetch_add(u64::from(forward.unsigned_abs()), Ordering::Relaxed);
            SCROLL_ACROSS_PIXELS.fetch_add(u64::from(across.unsigned_abs()), Ordering::Relaxed);
            if dry_run() {
                log::info!("Desktop (dry run): scroll {forward} pixels forward and {across} across");
                return;
            }
        }
    }
    let event = build(event);
    if !event.is_null() {
        unsafe {
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

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventGetIntegerValueField(event: *mut c_void, field: u32) -> i64;
        fn CGEventGetType(event: *mut c_void) -> u32;
        fn CGEventGetLocation(event: *mut c_void) -> CGPoint;
    }

    // CGEventField values
    const BUTTON_NUMBER: u32 = 3;
    const SCROLL_POINT_DELTA_FORWARD: u32 = 96;
    const SCROLL_POINT_DELTA_ACROSS: u32 = 97;

    /// What the system's event holds, which the host would post. Building an event needs no
    /// permission, so the test reads each one back instead of posting it.
    fn read_back(out: Out) -> (u32, i64, i64, CGPoint) {
        let event = build(out);
        assert!(!event.is_null(), "{out:?}");
        let held = unsafe {
            (CGEventGetType(event), CGEventGetIntegerValueField(event, BUTTON_NUMBER), CGEventGetIntegerValueField(event, MOUSE_EVENT_CLICK_STATE), CGEventGetLocation(event))
        };
        unsafe { CFRelease(event) };
        held
    }

    #[test]
    fn each_planned_event_becomes_the_system_event_of_its_kind() {
        let point = CGPoint { x: 640.0, y: 360.0 };
        let button = |bit, down, clicks| read_back(Out::Button { bit, down, point, clicks });
        // The kind, the button number, the click count and the point of each button's press and release
        assert_eq!(button(LEFT, true, 1), (LEFT_MOUSE_DOWN, 0, 1, point));
        assert_eq!(button(LEFT, false, 1), (LEFT_MOUSE_UP, 0, 1, point));
        assert_eq!(button(RIGHT, true, 1), (RIGHT_MOUSE_DOWN, 1, 1, point));
        assert_eq!(button(RIGHT, false, 1), (RIGHT_MOUSE_UP, 1, 1, point));
        assert_eq!(button(MIDDLE, true, 2), (OTHER_MOUSE_DOWN, 2, 2, point), "the middle button is button number 2 of the other kind");
        assert_eq!(button(MIDDLE, false, 2), (OTHER_MOUSE_UP, 2, 2, point));
        // A move is a plain move, and a drag has the kind of the button that is down
        assert_eq!(read_back(Out::Move { drag: None, point }).0, MOUSE_MOVED);
        assert_eq!(read_back(Out::Move { drag: Some(LEFT), point }).0, LEFT_MOUSE_DRAGGED);
        let right_drag = read_back(Out::Move { drag: Some(RIGHT), point });
        assert_eq!((right_drag.0, right_drag.1), (RIGHT_MOUSE_DRAGGED, 1));
        let middle_drag = read_back(Out::Move { drag: Some(MIDDLE), point });
        assert_eq!((middle_drag.0, middle_drag.1), (OTHER_MOUSE_DRAGGED, 2));
        assert_eq!(right_drag.3, point);
    }

    #[test]
    fn a_scroll_event_carries_pixels_in_both_axes() {
        let event = build(Out::Scroll { across: 12, forward: 20 });
        assert!(!event.is_null());
        let (forward, across) = unsafe {
            (CGEventGetIntegerValueField(event, SCROLL_POINT_DELTA_FORWARD), CGEventGetIntegerValueField(event, SCROLL_POINT_DELTA_ACROSS))
        };
        unsafe { CFRelease(event) };
        // Forward is up. The second wheel scrolls to the left for a positive number, so a lean to the
        // right reads back negative.
        assert_eq!((forward, across), (20, -12));
    }

    fn display() -> CGRect {
        CGRect { origin: CGPoint { x: 100.0, y: 50.0 }, size: CGPoint { x: 1000.0, y: 500.0 } }
    }

    /// A pointer with no display behind it, so the tests plan the events and post none.
    fn pointer() -> Pointer {
        Pointer { display_id: 0, held: 0, last: None, scroll: [0.0; 2], run: None, trusted: true, checked: std::time::Instant::now() }
    }

    fn at(u: f32, v: f32) -> CGPoint {
        CGPoint { x: 100.0 + u as f64 * 1000.0, y: 50.0 + v as f64 * 500.0 }
    }

    fn plan(pointer: &mut Pointer, hit: Option<[f32; 2]>, buttons: u8, lean: [f32; 2]) -> Vec<Out> {
        pointer.plan(display(), hit, buttons, lean, 1.0, std::time::Instant::now())
    }

    #[test]
    fn each_button_goes_down_and_up_at_the_point_that_the_controller_points_at() {
        for (bit, name) in [(LEFT, "left"), (RIGHT, "right"), (MIDDLE, "middle")] {
            let mut pointer = pointer();
            let down = plan(&mut pointer, Some([0.5, 0.5]), bit, [0.0; 2]);
            assert_eq!(
                down,
                vec![Out::Move { drag: None, point: at(0.5, 0.5) }, Out::Button { bit, down: true, point: at(0.5, 0.5), clicks: 1 }],
                "{name}"
            );
            // The controller drifts and the button stays down: a drag with that button
            let drag = plan(&mut pointer, Some([0.6, 0.5]), bit, [0.0; 2]);
            assert_eq!(drag, vec![Out::Move { drag: Some(bit), point: at(0.6, 0.5) }], "{name}");
            let up = plan(&mut pointer, Some([0.6, 0.5]), 0, [0.0; 2]);
            assert_eq!(up, vec![Out::Button { bit, down: false, point: at(0.6, 0.5), clicks: 1 }], "{name}");
        }
    }

    #[test]
    fn a_button_that_is_down_goes_up_when_the_controller_stops_pointing_at_the_panel() {
        let mut pointer = pointer();
        plan(&mut pointer, Some([0.2, 0.2]), RIGHT | MIDDLE, [0.0; 2]);
        // The client lets go with no point, or the host finds the client silent
        let end = plan(&mut pointer, None, 0, [0.0; 2]);
        assert_eq!(
            end,
            vec![
                Out::Button { bit: RIGHT, down: false, point: at(0.2, 0.2), clicks: 1 },
                Out::Button { bit: MIDDLE, down: false, point: at(0.2, 0.2), clicks: 1 }
            ]
        );
        assert_eq!(pointer.held, 0);
        assert!(plan(&mut pointer, None, 0, [0.0; 2]).is_empty());
    }

    #[test]
    fn two_buttons_at_once_each_post_their_own_press_and_the_drag_follows_the_first() {
        let mut pointer = pointer();
        plan(&mut pointer, Some([0.5, 0.5]), RIGHT, [0.0; 2]);
        let both = plan(&mut pointer, Some([0.5, 0.5]), RIGHT | LEFT, [0.0; 2]);
        assert_eq!(both, vec![Out::Button { bit: LEFT, down: true, point: at(0.5, 0.5), clicks: 1 }]);
        // The left button is the first in the order of the drags
        let drag = plan(&mut pointer, Some([0.7, 0.5]), RIGHT | LEFT, [0.0; 2]);
        assert_eq!(drag, vec![Out::Move { drag: Some(LEFT), point: at(0.7, 0.5) }]);
    }

    #[test]
    fn a_resting_controller_moves_the_mouse_only_past_three_points() {
        let mut pointer = pointer();
        assert_eq!(plan(&mut pointer, Some([0.5, 0.5]), 0, [0.0; 2]).len(), 1);
        // 2 points away: the Mac's own mouse wins
        assert!(plan(&mut pointer, Some([0.502, 0.5]), 0, [0.0; 2]).is_empty());
        // With a button down every point counts
        plan(&mut pointer, Some([0.502, 0.5]), LEFT, [0.0; 2]);
        assert_eq!(plan(&mut pointer, Some([0.5025, 0.5]), LEFT, [0.0; 2]).len(), 1);
    }

    #[test]
    fn the_thumbstick_scrolls_in_both_axes_and_its_dead_zone_scrolls_nothing() {
        let mut pointer = pointer();
        // Full lean forward: 20 pixels at one call
        assert_eq!(plan(&mut pointer, None, 0, [0.0, 1.0]), vec![Out::Scroll { across: 0, forward: 20 }]);
        assert_eq!(plan(&mut pointer, None, 0, [-1.0, -0.5]), vec![Out::Scroll { across: -20, forward: -10 }]);
        // A lean inside the dead zone is nothing, on each axis by itself
        assert!(plan(&mut pointer, None, 0, [0.1, -0.14]).is_empty());
        assert_eq!(plan(&mut pointer, None, 0, [0.1, 1.0]), vec![Out::Scroll { across: 0, forward: 20 }]);
        // The pixels that a call owes carry over to the next
        let mut pointer = self::pointer();
        let mut total = 0;
        for _ in 0..10 {
            for event in plan(&mut pointer, None, 0, [0.5, 0.0]) {
                if let Out::Scroll { across, .. } = event {
                    total += across;
                }
            }
        }
        assert_eq!(total, 100, "ten calls at half lean owe 100 pixels across");
    }

    #[test]
    fn presses_in_quick_succession_at_one_place_count_as_a_double_click() {
        let mut pointer = pointer();
        let start = std::time::Instant::now();
        let press = |pointer: &mut Pointer, bit: u8, point: [f32; 2], after_ms: u64| {
            let now = start + std::time::Duration::from_millis(after_ms);
            let down = pointer.plan(display(), Some(point), bit, [0.0; 2], 1.0, now);
            let up = pointer.plan(display(), Some(point), 0, [0.0; 2], 1.0, now + std::time::Duration::from_millis(30));
            let clicks = |events: &[Out]| events.iter().find_map(|e| if let Out::Button { clicks, .. } = e { Some(*clicks) } else { None }).unwrap();
            (clicks(&down), clicks(&up))
        };
        assert_eq!(press(&mut pointer, LEFT, [0.5, 0.5], 0), (1, 1));
        assert_eq!(press(&mut pointer, LEFT, [0.5, 0.5], 200), (2, 2));
        assert_eq!(press(&mut pointer, LEFT, [0.5, 0.5], 400), (3, 3));
        // Another button, a far point and a long pause each start a new run
        assert_eq!(press(&mut pointer, RIGHT, [0.5, 0.5], 600).0, 1);
        assert_eq!(press(&mut pointer, RIGHT, [0.9, 0.9], 800).0, 1);
        assert_eq!(press(&mut pointer, RIGHT, [0.9, 0.9], 1500).0, 1);
    }

    #[test]
    fn the_hosts_own_scene_clicks_with_its_trigger() {
        let mut pointer = pointer();
        let now = std::time::Instant::now();
        let step = |pointer: &mut Pointer, trigger: f32| {
            let buttons = trigger_buttons(pointer.held, trigger);
            pointer.plan(display(), Some([0.5, 0.5]), buttons, [0.0, 0.0], 1.0, now)
        };
        assert_eq!(step(&mut pointer, 0.5).len(), 1, "a half pulled trigger only moves the mouse");
        assert!(step(&mut pointer, 0.7).iter().any(|e| matches!(e, Out::Button { bit: LEFT, down: true, .. })));
        assert!(step(&mut pointer, 0.5).is_empty(), "the hysteresis keeps the button down");
        assert!(step(&mut pointer, 0.3).iter().any(|e| matches!(e, Out::Button { bit: LEFT, down: false, .. })));
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
