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
mod run;
mod setup;
mod steam;
mod system;
mod ui;
mod usb;
mod vr_system;
mod vr_window;

use cranpose::AppLauncher;

fn main() {
    // Wine's driver reads the microphone's source from a file: it holds the choice from the start
    let settings = backend::Settings::load();
    if let Err(e) = microphone::write_choice(&backend::config_dir(), &settings.microphone) {
        eprintln!("cranvr: the microphone's choice is not written: {e}");
    }
    // A test beside the user's app runs with no window (see run.rs)
    if std::env::args().any(|arg| arg == "--headless") {
        run::headless();
        backend::stop_host();
        return;
    }
    let result = AppLauncher::new().with_title("cranvr").with_size(820, 760).try_run(ui::CranvrApp);
    backend::stop_host();
    if let Err(error) = result {
        eprintln!("cranvr: {error}");
        std::process::exit(1);
    }
}
