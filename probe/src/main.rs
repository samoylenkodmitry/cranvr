//! cranvr-probe: a headless client of cranvr's protocol for tests on this Mac. It connects to the
//! host's TCP port, sends a slowly turning head and synthetic controllers once per refresh,
//! receives the media, restores lost packets with the parity, decodes the HEVC stream with
//! VideoToolbox and checks the red and blue eye badges that the host draws. It reports its frame
//! timing and its loss to the host, as a headset app does (see native.rs).
//!
//! Exit code 0 means at least half the expected frames arrived and decoded with the right badges.

mod compositor;
mod hands;
mod native;
mod native_desktop;
mod tracked;

use cranvr_common::foveation::{view_to_frame, Foveation};
use std::{env, net::ToSocketAddrs};

/// The host draws the eye badges at this point of each eye's view.
const BADGE_VIEW: [f32; 2] = [0.11, 0.5];

/// Where the badges sit in the encoded frame. With foveated encoding the foveation's unwarp
/// formula gives the place, so a match proves that the host's warp agrees with the client.
fn badge_points(foveation: Option<&Foveation>) -> Vec<(f32, f32)> {
    (0..2)
        .map(|eye| {
            let p = view_to_frame(foveation, eye, BADGE_VIEW);
            (p[0], p[1])
        })
        .collect()
}

fn usage() -> ! {
    eprintln!(
        "usage: cranvr-probe [--seconds N] [--game | --any-picture] [--connect HOST:PORT] [--tcp] [--drop-percent P] [--desktop]\n\
         \n  --seconds N        how long the stream runs (default 10)\
         \n  --game             expect the fake game's badges (left blue, right red)\
         \n  --any-picture      count every frame of the right size, for a program that draws no badges\
         \n  --connect HOST:PORT  the host's address (default 127.0.0.1:9950)\
         \n  --tcp              ask for the media over TCP, as a USB link needs it\
         \n  --drop-percent P   drop this share of the media packets, to test the parity\
         \n  --desktop          ask for the Mac's desktop as a second stream, play a pointer script\
         \n                     and count the sound (start the host with --desktop and CRANVR_POINTER_DRY_RUN=1)\
         \n  --tracking         offer hands, body, face and eyes, and send the script of tracked.rs for\
         \n                     each feature that the host asks for. The head stands still.\
         \n  --still-head       the head stands still (as with --tracking), so a game's room is the\
         \n                     probe's room and a game's controller poses match the script\
         \n  --hands-lift M     both hands play their script M meters higher, into the view of the eyes (default 0)\
         \n  --no-controllers   with --tracking: no controller is tracked, so a game without hand code\
         \n                     gets controllers made of the hands\
         \n  --face-source S    with --tracking: where the face comes from, visual (cameras), audio\
         \n                     (the microphone, the default) or unknown (the offer names no source)\
         \n  --pause AT:FOR     the app loses the input focus at AT seconds (2 or more) for FOR seconds, as under\
         \n                     the system's menu: the probe tells the host, sends no tracking and no input and\
         \n                     counts the frames that still arrive. The exit code says whether the stream paused\
         \n                     and went on.\
         \n  --left-trigger AT:FOR  the left trigger goes to 1 at AT seconds and stays for FOR seconds, once.\
         \n                     The right trigger pulls to 0.75 for half a second in every 3 seconds. A game that\
         \n                     shows only one pointer at a time, such as a menu, then has its left controller\
         \n                     pressed for the first time at AT.\
         \n  --protocol native  accepted and ignored: cranvr's protocol is the only one"
    );
    std::process::exit(2);
}

/// `AT:FOR` as two numbers of seconds
fn parse_pause(text: &str) -> Option<(f32, f32)> {
    let (at, length) = text.split_once(':')?;
    let (at, length) = (at.parse::<f32>().ok()?, length.parse::<f32>().ok()?);
    (at >= 2.0 && length > 0.0).then_some((at, length))
}

/// `AT:FOR` as two numbers of seconds, the start of a pull and its length
fn parse_pull(text: &str) -> Option<(f32, f32)> {
    let (at, length) = text.split_once(':')?;
    let (at, length) = (at.parse::<f32>().ok()?, length.parse::<f32>().ok()?);
    (at >= 0.0 && length > 0.0).then_some((at, length))
}

fn main() {
    let mut options = native::Options {
        seconds: 10.0,
        game: false,
        any_picture: false,
        connect: "127.0.0.1:9950".parse().unwrap(),
        tcp: false,
        drop_percent: 0.0,
        desktop: false,
        tracking: false,
        no_controllers: false,
        still_head: false,
        hands_lift: 0.0,
        face_source: Default::default(),
        pause: None,
        left_trigger: None,
    };
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--seconds" => options.seconds = value().parse().unwrap_or_else(|_| usage()),
            "--game" => options.game = true,
            "--any-picture" => options.any_picture = true,
            "--protocol" => {
                if value() != "native" {
                    usage()
                }
            }
            "--connect" => options.connect = value().to_socket_addrs().ok().and_then(|mut a| a.next()).unwrap_or_else(|| usage()),
            "--tcp" => options.tcp = true,
            "--desktop" => options.desktop = true,
            "--tracking" => options.tracking = true,
            "--no-controllers" => options.no_controllers = true,
            "--still-head" => options.still_head = true,
            "--hands-lift" => options.hands_lift = value().parse().unwrap_or_else(|_| usage()),
            "--face-source" => options.face_source = tracked::FaceSource::parse(&value()).unwrap_or_else(|| usage()),
            "--left-trigger" => options.left_trigger = Some(parse_pull(&value()).unwrap_or_else(|| usage())),
            "--pause" => options.pause = Some(parse_pause(&value()).unwrap_or_else(|| usage())),
            "--drop-percent" => options.drop_percent = value().parse().unwrap_or_else(|_| usage()),
            _ => usage(),
        }
    }
    std::process::exit(native::run(options));
}
