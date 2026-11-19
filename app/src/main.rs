//! cranvr: the window for cranvr-host. It starts and stops the host, pairs headsets, sets the
//! stream quality, shows the Mac desktop in VR, and starts Windows games under Wine.

mod backend;
mod launch;
mod setup;
mod steam;
mod system;
mod ui;
mod vr_window;

use cranpose::AppLauncher;

fn main() {
    let result = AppLauncher::new().with_title("cranvr").with_size(820, 760).try_run(ui::CranvrApp);
    backend::stop_host();
    if let Err(error) = result {
        eprintln!("cranvr: {error}");
        std::process::exit(1);
    }
}
