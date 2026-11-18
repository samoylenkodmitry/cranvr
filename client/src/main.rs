//! The client in a flat window, for development. It takes the launch arguments `--host=ADDRESS`,
//! `--connect` and `--media=udp|tcp`. `RUST_LOG=info` prints the log. `--phone` runs the phone's
//! app in the window: the picture fills the window and a tap opens the panels. The window has no
//! motion sensor, so the head looks straight ahead.
#![allow(clippy::print_stderr, reason = "a command-line tool prints its output")]

fn main() {
    env_logger::init();
    let phone = std::env::args().any(|argument| argument == "--phone");
    let launcher = cranpose::AppLauncher::new().with_title("cranvr client");
    let result = if phone {
        launcher.with_size(1280, 800).try_run(cranvr_client::PhoneApp)
    } else {
        launcher.with_size(2240, 960).try_run(cranvr_client::ClientApp)
    };
    if let Err(error) = result {
        eprintln!("the client failed to start: {error}");
        std::process::exit(1);
    }
}
