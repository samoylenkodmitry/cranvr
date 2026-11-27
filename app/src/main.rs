//! cranvr: the window for cranvr-host. It starts and stops the host, pairs headsets, sets the
//! stream quality, shows the Mac desktop in VR, and starts Windows games under Wine.

mod appinfo;
mod backend;
mod black_eye;
mod game_run;
mod launch;
mod microphone;
mod openvr;
mod pages;
mod pending;
mod recorder;
mod run;
mod setup;
mod steam;
mod system;
mod ui;
mod usb;
mod vr_system;
mod vr_window;

use cranpose::AppLauncher;

/// SIGTERM, SIGINT and SIGHUP end the app as a close of its window does: a recording that runs stops
/// first (recorder.rs), then the host. An update of the app, `kill` and a logout send SIGTERM. Without
/// this the app died at once and left the headset's recorder running (2026-11-27).
fn end_on_signals() {
    use std::sync::atomic::{AtomicI32, Ordering};
    // The handler may only write to a pipe: a thread does the work
    static PIPE: AtomicI32 = AtomicI32::new(-1);
    extern "C" fn on_signal(_: libc::c_int) {
        let fd = PIPE.load(Ordering::Relaxed);
        if fd >= 0 {
            // SAFETY: write is safe in a signal handler; one byte wakes the thread
            unsafe { libc::write(fd, [1u8].as_ptr().cast(), 1) };
        }
    }
    let mut fds = [0; 2];
    // SAFETY: pipe fills `fds`; the ends close with the process and stay out of child processes
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return;
    }
    for fd in fds {
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    PIPE.store(fds[1], Ordering::Relaxed);
    let handler = on_signal as extern "C" fn(libc::c_int);
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler is async-signal-safe
        unsafe { libc::signal(signal, handler as libc::sighandler_t) };
    }
    std::thread::Builder::new()
        .name("cranvr-signals".into())
        .spawn(move || {
            let mut byte = 0u8;
            // SAFETY: read fills one byte; an interrupted read tries again
            while unsafe { libc::read(fds[0], (&mut byte as *mut u8).cast(), 1) } != 1 {}
            eprintln!("cranvr: a signal ends the app");
            recorder::stop_at_exit();
            backend::stop_host();
            std::process::exit(0);
        })
        .ok();
}

fn main() {
    end_on_signals();
    // Wine's driver reads the microphone's source from a file: it holds the choice from the start
    let settings = backend::Settings::load();
    if let Err(e) = microphone::write_choice(&backend::config_dir(), &settings.microphone) {
        eprintln!("cranvr: the microphone's choice is not written: {e}");
    }
    // A test beside the user's app runs with no window (see run.rs)
    if std::env::args().any(|arg| arg == "--headless") {
        run::headless();
        recorder::stop_at_exit();
        backend::stop_host();
        return;
    }
    let result = AppLauncher::new().with_title("cranvr").with_size(820, 760).try_run(ui::CranvrApp);
    recorder::stop_at_exit();
    backend::stop_host();
    if let Err(error) = result {
        eprintln!("cranvr: {error}");
        std::process::exit(1);
    }
}
