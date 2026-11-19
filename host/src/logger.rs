//! The host's log. It sets up the `log` crate with one sink: lines of the form
//! `HH:MM:SS.mmm [LEVEL] message`, on stdout. The cranvr app reads that stream. A line of an
//! error also goes to stderr and into the crash log file in the config folder. A panic goes into
//! the same log as an error, with its place, before the thread ends.
//!
//! A line is written with one call, so lines of several threads and the `@cranvr` status lines
//! (see `status.rs`) never mix inside a line.

use ::log::{Level, LevelFilter, Log, Metadata, Record};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    sync::Mutex,
};

struct Logger {
    crash_log: Mutex<Option<File>>,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        // The host's own lines only: a library's debug lines would flood the output
        metadata.level() <= LevelFilter::Info
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let level = match record.level() {
            Level::Error => "ERROR",
            Level::Warn => "WARNING",
            Level::Info => "INFO",
            Level::Debug | Level::Trace => "DEBUG",
        };
        let line = format!("{} [{level}] {}\n", clock(), record.args());
        // Each sink gets the whole line in one write
        std::io::stdout().lock().write_all(line.as_bytes()).ok();
        if record.level() == Level::Error {
            std::io::stderr().lock().write_all(line.as_bytes()).ok();
            if let Some(file) = self.crash_log.lock().unwrap().as_mut() {
                file.write_all(line.as_bytes()).ok();
            }
        }
    }

    fn flush(&self) {}
}

/// The local time of day as `HH:MM:SS.mmm`.
fn clock() -> String {
    let mut now = libc::timeval { tv_sec: 0, tv_usec: 0 };
    // SAFETY: both pointers refer to values that live for the call
    let time = unsafe {
        libc::gettimeofday(&mut now, std::ptr::null_mut());
        let mut time: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now.tv_sec, &mut time);
        time
    };
    format!("{:02}:{:02}:{:02}.{:03}", time.tm_hour, time.tm_min, time.tm_sec, now.tv_usec / 1000)
}

/// Starts the log. `crash_log` is the file for errors and panics; the file keeps the lines of the
/// runs before it.
pub fn init(crash_log: &Path) {
    let file = OpenOptions::new().create(true).append(true).open(crash_log).ok();
    let logger: &'static Logger = Box::leak(Box::new(Logger { crash_log: Mutex::new(file) }));
    if ::log::set_logger(logger).is_ok() {
        ::log::set_max_level(LevelFilter::Info);
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let place = info.location().map_or_else(String::new, |l| format!(" at {}:{}", l.file(), l.line()));
        let message = info.payload().downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| info.payload().downcast_ref::<String>().cloned());
        ::log::error!("Panic{place}: {}", message.unwrap_or_else(|| "no message".into()));
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_clock_has_the_shape_of_a_time_of_day() {
        let text = super::clock();
        let parts: Vec<&str> = text.split([':', '.']).collect();
        assert_eq!(parts.iter().map(|p| p.len()).collect::<Vec<_>>(), [2, 2, 2, 3], "{text}");
        assert!(parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())));
    }
}
