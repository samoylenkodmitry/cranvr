// Pacing to the display. The frame loop waits for the display's vsync, then until a lead time
// before the next one. Only then it takes the newest pose, warps the frame and swaps. The swap
// lands just before SurfaceFlinger takes buffers for that refresh, so no frame waits in the
// window's buffer queue, and the pose is as fresh as the render time allows.
//
// The lead covers the render on the CPU and the GPU. It comes from the property
// debug.cranvr.lead_us (`adb shell setprop debug.cranvr.lead_us 6000`), or a default.

use std::{
    ffi::c_void,
    ptr,
    sync::atomic::{AtomicI64, Ordering},
    time::Duration,
};

const DEFAULT_LEAD: Duration = Duration::from_millis(6);

#[link(name = "android")]
extern "C" {
    fn ALooper_prepare(options: i32) -> *mut c_void;
    fn ALooper_pollOnce(timeout_ms: i32, fd: *mut i32, events: *mut i32, data: *mut *mut c_void) -> i32;
    fn AChoreographer_getInstance() -> *mut c_void;
    fn AChoreographer_postFrameCallback64(
        choreographer: *mut c_void,
        callback: extern "C" fn(i64, *mut c_void),
        data: *mut c_void,
    );
}

extern "C" fn on_frame(frame_time_ns: i64, data: *mut c_void) {
    unsafe { &*(data as *const AtomicI64) }.store(frame_time_ns, Ordering::Release);
}

pub struct Pacing {
    choreographer: *mut c_void,
    vsync: Box<AtomicI64>,
    last_vsync: i64,
    period: Duration,
    pub lead: Duration,
}

impl Pacing {
    /// Call it on the frame loop's thread: the vsync callbacks run on its looper.
    pub fn new() -> Self {
        let choreographer = unsafe {
            ALooper_prepare(1);
            AChoreographer_getInstance()
        };
        Self {
            choreographer,
            vsync: Box::new(AtomicI64::new(0)),
            last_vsync: 0,
            period: Duration::from_nanos(16_666_667),
            lead: lead_from_property().unwrap_or(DEFAULT_LEAD),
        }
    }

    /// The display's refresh rate, measured over the vsyncs so far.
    pub fn refresh_rate(&self) -> f32 {
        1.0 / self.period.as_secs_f32()
    }

    /// Measures the refresh rate over `frames` vsyncs: the median gap between them, so a missed
    /// vsync does not count. `each` runs after every wait. It must show a frame: a compositor
    /// with variable refresh, such as the Pixel's, lowers the rate of a window that shows none.
    /// The wait skips the lead: with a period estimate longer than the true one, the sleep would
    /// pass the next vsync, so every gap would come out as two refreshes.
    pub fn measure(&mut self, frames: usize, mut each: impl FnMut()) -> f32 {
        let mut gaps = Vec::with_capacity(frames);
        for _ in 0..frames {
            let before = self.last_vsync;
            self.last_vsync = self.next_vsync();
            each();
            if before != 0 {
                gaps.push(self.last_vsync - before);
            }
        }
        gaps.sort_unstable();
        if let Some(&median) = gaps.get(gaps.len() / 2) {
            self.period = Duration::from_nanos(median.max(1) as u64);
        }
        self.refresh_rate()
    }

    /// Waits for the next vsync, then until the lead time before the vsync after it. Returns
    /// that vsync's time on CLOCK_MONOTONIC.
    pub fn wait(&mut self) -> Duration {
        let vsync = self.next_vsync();
        if self.last_vsync != 0 {
            let gap = Duration::from_nanos((vsync - self.last_vsync).max(0) as u64);
            // A gap of one refresh updates the period. A missed vsync does not: it would make the
            // period longer, so the next sleeps would miss more vsyncs.
            if gap > self.period * 3 / 4 && gap < self.period * 5 / 4 {
                self.period = (self.period * 7 + gap) / 8;
            }
        }
        self.last_vsync = vsync;
        let next = Duration::from_nanos(vsync as u64) + self.period;
        sleep_until(next.saturating_sub(self.lead));
        next
    }

    /// Waits for the next vsync and returns its time in nanoseconds on CLOCK_MONOTONIC.
    fn next_vsync(&self) -> i64 {
        let data = &*self.vsync as *const AtomicI64 as *mut c_void;
        self.vsync.store(0, Ordering::Relaxed);
        unsafe { AChoreographer_postFrameCallback64(self.choreographer, on_frame, data) };
        while self.vsync.load(Ordering::Acquire) == 0 {
            unsafe { ALooper_pollOnce(100, ptr::null_mut(), ptr::null_mut(), ptr::null_mut()) };
        }
        self.vsync.load(Ordering::Acquire)
    }
}

fn lead_from_property() -> Option<Duration> {
    super::property::get("lead_us")?.parse().ok().map(Duration::from_micros)
}

fn sleep_until(time: Duration) {
    let target = libc::timespec { tv_sec: time.as_secs() as _, tv_nsec: time.subsec_nanos() as _ };
    unsafe { libc::clock_nanosleep(libc::CLOCK_MONOTONIC, libc::TIMER_ABSTIME, &target, ptr::null_mut()) };
}
