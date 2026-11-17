//! The client in a flat window, for development. It takes the launch arguments `--host=ADDRESS`,
//! `--connect` and `--media=udp|tcp`. `RUST_LOG=info` prints the log.
#![allow(clippy::print_stderr, reason = "a command-line tool prints its output")]

fn main() {
    env_logger::init();
    if let Err(error) = cranpose::AppLauncher::new()
        .with_title("cranvr client")
        .with_size(2240, 960)
        .try_run(cranvr_client::ClientApp)
    {
        eprintln!("the client failed to start: {error}");
        std::process::exit(1);
    }
}
