// The app's window as an EGL surface, with the display time of each frame.
// EGL_ANDROID_get_frame_timestamps reports when SurfaceFlinger put a frame on the display; the
// latency numbers of the window display come from these times.
//
// In front-buffer mode the app draws into the buffer that the display shows. SurfaceFlinger
// composes that buffer again at every refresh (EGL_ANDROID_front_buffer_auto_refresh), so a
// frame waits in no queue. The mode needs an EGL config with EGL_MUTABLE_RENDER_BUFFER_BIT_KHR,
// which ALVR's context lacks: the surface gets a context of its own that shares textures with
// ALVR's. A frame then has no swap, so EGL reports no display time for it. The display time is
// derived instead: the first composition after the GPU finished, plus the compositor's own
// composition-to-display latency (EGL_ANDROID_get_frame_timestamps' compositor timing).

use alvr_graphics::GraphicsContext;
use khronos_egl as egl;
use std::{ffi::c_void, time::Duration};

const EGL_TIMESTAMPS_ANDROID: i32 = 0x3430;
const EGL_DISPLAY_PRESENT_TIME_ANDROID: i32 = 0x343A;
const EGL_TIMESTAMP_PENDING_ANDROID: i64 = -2;
const EGL_COMPOSITE_DEADLINE_ANDROID: i32 = 0x3431;
const EGL_COMPOSITE_INTERVAL_ANDROID: i32 = 0x3432;
const EGL_COMPOSITE_TO_PRESENT_LATENCY_ANDROID: i32 = 0x3433;
const EGL_MUTABLE_RENDER_BUFFER_BIT_KHR: i32 = 0x1000;
const EGL_FRONT_BUFFER_AUTO_REFRESH_ANDROID: i32 = 0x314C;
const EGL_OPENGL_ES3_BIT: i32 = 0x0040;

type GetNextFrameId = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut u64) -> u32;
type GetFrameTimestamps = unsafe extern "C" fn(*mut c_void, *mut c_void, u64, i32, *const i32, *mut i64) -> u32;
type GetCompositorTiming = unsafe extern "C" fn(*mut c_void, *mut c_void, i32, *const i32, *mut i64) -> u32;

type SetFrameRate = unsafe extern "C" fn(*mut c_void, f32, i8) -> i32;

/// Asks for a refresh rate for the window. The call exists from Android 11 on, so the app looks
/// it up at run time instead of linking to it.
unsafe fn set_frame_rate(window: *mut c_void, rate: f32) {
    for library in [c"libnativewindow.so", c"libandroid.so"] {
        let handle = libc::dlopen(library.as_ptr(), libc::RTLD_NOW);
        if handle.is_null() {
            continue;
        }
        let symbol = libc::dlsym(handle, c"ANativeWindow_setFrameRate".as_ptr());
        if !symbol.is_null() {
            let set = std::mem::transmute::<*mut c_void, SetFrameRate>(symbol);
            let result = set(window, rate, 0);
            super::log::line(&format!("cranvr-flat: asked for {rate} Hz: {result}"));
            return;
        }
    }
    super::log::line("cranvr-flat: this Android has no ANativeWindow_setFrameRate");
}

/// When a frame reached the display.
pub enum Presented {
    At(Duration),
    Pending,
    /// The compositor dropped the frame, or EGL kept no time for it
    Unknown,
}

pub struct WindowSurface {
    egl: egl::DynamicInstance<egl::EGL1_4>,
    display: egl::Display,
    context: egl::Context,
    surface: egl::Surface,
    pub width: i32,
    pub height: i32,
    next_frame_id: Option<GetNextFrameId>,
    frame_timestamps: Option<GetFrameTimestamps>,
    compositor_timing: Option<GetCompositorTiming>,
    /// The surface draws into the displayed buffer, with a context of its own
    pub front_buffer: bool,
}

impl WindowSurface {
    /// # Safety
    /// `window` must be a valid ANativeWindow that outlives the surface.
    pub unsafe fn new(gfx: &GraphicsContext, window: *mut c_void, refresh_rate: f32, requested: bool) -> Self {
        let egl = egl::DynamicInstance::<egl::EGL1_4>::load_required().expect("libEGL");
        // The display asks for this rate; SurfaceFlinger switches the panel to it
        set_frame_rate(window, refresh_rate);
        let display = gfx.egl_display;
        let own = if requested { front_buffer_context(&egl, gfx) } else { None };
        let (config, context) = own.unwrap_or((gfx.egl_config, gfx.egl_context));
        let front_buffer = own.is_some();
        if front_buffer != requested {
            super::log::line("cranvr-flat: front-buffer mode refused: the driver has no EGL config with a mutable render buffer");
        }
        let surface = egl.create_window_surface(display, config, window, None).expect("window surface");
        let timestamps = egl.surface_attrib(display, surface, EGL_TIMESTAMPS_ANDROID, egl::TRUE as i32).is_ok();
        let lookup = |name: &str| egl.get_proc_address(name).map(|f| f as *const c_void);
        let next_frame_id = lookup("eglGetNextFrameIdANDROID").filter(|_| timestamps).map(|f| std::mem::transmute::<_, GetNextFrameId>(f));
        let frame_timestamps =
            lookup("eglGetFrameTimestampsANDROID").filter(|_| timestamps).map(|f| std::mem::transmute::<_, GetFrameTimestamps>(f));
        let compositor_timing =
            lookup("eglGetCompositorTimingANDROID").filter(|_| timestamps).map(|f| std::mem::transmute::<_, GetCompositorTiming>(f));
        let width = egl.query_surface(display, surface, egl::WIDTH).unwrap_or(0);
        let height = egl.query_surface(display, surface, egl::HEIGHT).unwrap_or(0);
        let this = Self { egl, display, context, surface, width, height, next_frame_id, frame_timestamps, compositor_timing, front_buffer };
        if front_buffer {
            this.make_current();
            // The switch to the single buffer happens at the next swap
            this.egl.surface_attrib(display, surface, egl::RENDER_BUFFER, egl::SINGLE_BUFFER).ok();
            this.swap();
            this.egl.surface_attrib(display, surface, EGL_FRONT_BUFFER_AUTO_REFRESH_ANDROID, egl::TRUE as i32).ok();
            let single = this.egl.query_surface(display, surface, egl::RENDER_BUFFER).ok() == Some(egl::SINGLE_BUFFER);
            super::log::line(&format!("cranvr-flat: front-buffer mode {}", if single { "on" } else { "refused by the driver" }));
        }
        this
    }

    pub fn make_current(&self) {
        self.egl
            .make_current(self.display, Some(self.surface), Some(self.surface), Some(self.context))
            .expect("make the window current");
    }

    /// The ID of the frame that the next swap shows, when the driver keeps frame times.
    pub fn next_frame_id(&self) -> Option<u64> {
        let get = self.next_frame_id?;
        let mut id = 0;
        (unsafe { get(self.display.as_ptr(), self.surface.as_ptr(), &mut id) } == egl::TRUE).then_some(id)
    }

    pub fn swap(&self) {
        self.egl.swap_buffers(self.display, self.surface).ok();
    }

    pub fn presented(&self, frame_id: u64) -> Presented {
        let Some(get) = self.frame_timestamps else { return Presented::Unknown };
        let names = [EGL_DISPLAY_PRESENT_TIME_ANDROID];
        let mut values = [0i64];
        let ok = unsafe { get(self.display.as_ptr(), self.surface.as_ptr(), frame_id, 1, names.as_ptr(), values.as_mut_ptr()) };
        match values[0] {
            _ if ok != egl::TRUE => Presented::Unknown,
            EGL_TIMESTAMP_PENDING_ANDROID => Presented::Pending,
            t if t > 0 => Presented::At(Duration::from_nanos(t as u64)),
            _ => Presented::Unknown,
        }
    }
}

impl WindowSurface {
    /// The next composition time, the time between compositions, and the time from a
    /// composition to the display, all on CLOCK_MONOTONIC.
    pub fn compositor_timing(&self) -> Option<(Duration, Duration, Duration)> {
        let get = self.compositor_timing?;
        let names = [EGL_COMPOSITE_DEADLINE_ANDROID, EGL_COMPOSITE_INTERVAL_ANDROID, EGL_COMPOSITE_TO_PRESENT_LATENCY_ANDROID];
        let mut values = [0i64; 3];
        let ok = unsafe { get(self.display.as_ptr(), self.surface.as_ptr(), 3, names.as_ptr(), values.as_mut_ptr()) };
        let [deadline, interval, to_present] = values.map(|v| Duration::from_nanos(v.max(0) as u64));
        (ok == egl::TRUE && values.iter().all(|v| *v > 0)).then_some((deadline, interval, to_present))
    }

    /// The derived display time of what the GPU finished at `done` in front-buffer mode: the
    /// first composition after it, plus the composition-to-display latency.
    pub fn derived_display_time(&self, done: Duration) -> Option<Duration> {
        let (mut deadline, interval, to_present) = self.compositor_timing()?;
        while deadline < done {
            deadline += interval;
        }
        Some(deadline + to_present)
    }
}

/// A config and a context for front-buffer rendering, sharing objects with ALVR's context.
unsafe fn front_buffer_context(egl: &egl::DynamicInstance<egl::EGL1_4>, gfx: &GraphicsContext) -> Option<(egl::Config, egl::Context)> {
    let attributes = [
        egl::RED_SIZE, 8,
        egl::GREEN_SIZE, 8,
        egl::BLUE_SIZE, 8,
        egl::ALPHA_SIZE, 8,
        egl::RENDERABLE_TYPE, EGL_OPENGL_ES3_BIT,
        egl::SURFACE_TYPE, egl::WINDOW_BIT | EGL_MUTABLE_RENDER_BUFFER_BIT_KHR,
        egl::NONE,
    ];
    let config = egl.choose_first_config(gfx.egl_display, &attributes).ok().flatten()?;
    let context_attributes = [egl::CONTEXT_CLIENT_VERSION, 3, egl::NONE];
    let context = egl.create_context(gfx.egl_display, config, Some(gfx.egl_context), &context_attributes).ok()?;
    Some((config, context))
}

impl Drop for WindowSurface {
    fn drop(&mut self) {
        self.egl.make_current(self.display, None, None, None).ok();
        self.egl.destroy_surface(self.display, self.surface).ok();
    }
}
