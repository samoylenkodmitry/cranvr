// One clock for every timestamp of the window display: CLOCK_MONOTONIC. EGL reports display
// times on it. Sensor events use CLOCK_BOOTTIME, which also counts the time asleep, so their
// times move by the gap between the two clocks.

use std::time::Duration;

fn read(clock: libc::clockid_t) -> i64 {
    let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(clock, &mut time) };
    time.tv_sec * 1_000_000_000 + time.tv_nsec
}

pub fn now() -> Duration {
    Duration::from_nanos(read(libc::CLOCK_MONOTONIC) as u64)
}

/// A CLOCK_BOOTTIME time in nanoseconds on CLOCK_MONOTONIC. The gap changes only while the
/// device sleeps, and no stream runs then.
pub fn boottime_to_monotonic(boottime_ns: i64) -> Duration {
    let gap = read(libc::CLOCK_BOOTTIME) - read(libc::CLOCK_MONOTONIC);
    Duration::from_nanos((boottime_ns - gap).max(0) as u64)
}
