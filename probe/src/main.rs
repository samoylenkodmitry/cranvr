//! cranvr-probe: a headless client of cranvr's protocol for tests on this Mac. It connects to the
//! host's TCP port, sends a slowly turning head and synthetic controllers once per refresh,
//! receives the media, restores lost packets with the parity, decodes the HEVC stream with
//! VideoToolbox and checks the red and blue eye badges that the host draws. It reports its frame
//! timing and its loss to the host, as a headset app does (see native.rs).
//!
//! Exit code 0 means at least half the expected frames arrived and decoded with the right badges.

mod compositor;
mod displays;
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
         \n  --displays N       show N displays: the Mac's own and N-1 that the probe asks the host to add (start\
         \n                     the host with --desktop and CRANVR_POINTER_DRY_RUN=1). It removes the last one near the end.\
         \n  --displays-in-game against a game that runs: wish no display for 5 s, show them for 5 s, hide them again\
         \n                     and count the pictures in each part. Implies --desktop's streams, not its wish.\
         \n  --hold-script      the right hand points at a panel in every other window of its script: the input of\
         \n                     those windows must not reach the game (cranvr-fake-game --expect-held)\
         \n  --hold-hands       both hands point at a panel all the time: a game without hand code must get the\
         \n                     controllers that the host makes of the tracked hands with rest values\
         \n                     (cranvr-fake-game --expect-hand-held)\
         \n  --tracking         offer hands, body, face and eyes, and send the script of tracked.rs for\
         \n                     each feature that the host asks for. The head stands still.\
         \n  --still-head       the head stands still (as with --tracking), so a game's room is the\
         \n                     probe's room and a game's controller poses match the script\
         \n  --quest-view       the view of a Quest 3's eyes: 54 degrees out, 40 in, 44 up and 55 down\
         \n                     (default: 45 degrees each way)\
         \n  --hands-lift M     both hands play their script M meters higher, into the view of the eyes (default 0)\
         \n  --no-controllers   with --tracking: no controller is tracked, so a game without hand code\
         \n                     gets controllers made of the hands\
         \n  --face-source S    with --tracking: where the face comes from, visual (cameras), audio\
         \n                     (the microphone, the default) or unknown (the offer names no source)\
         \n  --pause AT:FOR     the app loses the input focus at AT seconds (2 or more) for FOR seconds, as under\
         \n                     the system's menu: the probe tells the host, sends no tracking and no input and\
         \n                     counts the frames that still arrive. The exit code says whether the stream paused\
         \n                     and went on.\
         \n  --left-trigger AT:FOR  the left trigger goes to 1 at AT seconds and stays for FOR seconds, once. The script\
         \n                     of the cycle is off then, so the pulls are the only presses. A game that shows one\
         \n                     pointer at a time, such as a menu, then has its left controller pressed for the\
         \n                     first time at AT.\
         \n  --right-trigger AT:FOR  the same for the right trigger\
         \n  --menu AT          the left controller's menu button goes down at AT seconds for 0.3 s, once\
         \n  --display-fps N    with --desktop: each display's wish asks for at most N pictures a second (1 to 60)\
         \n  --microphone       offer a microphone: while the host asks for it, the probe sends a click every 500 ms\
         \n                     and prints the time of each on mach_continuous_time"
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
        displays: 1,
        displays_in_game: false,
        display_fps: None,
        microphone: false,
        hold_script: false,
        hold_hands: false,
        tracking: false,
        no_controllers: false,
        still_head: false,
        quest_view: false,
        hands_lift: 0.0,
        face_source: Default::default(),
        pause: None,
        left_trigger: None,
        menu: None,
        right_trigger: None,
    };
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--seconds" => options.seconds = value().parse().unwrap_or_else(|_| usage()),
            "--game" => options.game = true,
            "--any-picture" => options.any_picture = true,
            "--connect" => options.connect = value().to_socket_addrs().ok().and_then(|mut a| a.next()).unwrap_or_else(|| usage()),
            "--tcp" => options.tcp = true,
            "--desktop" => options.desktop = true,
            "--displays" => options.displays = value().parse().ok().filter(|n| (1..=cranvr_proto::message::MAX_DISPLAYS).contains(n)).unwrap_or_else(|| usage()),
            "--displays-in-game" => options.displays_in_game = true,
            "--microphone" => options.microphone = true,
            "--display-fps" => options.display_fps = Some(value().parse().ok().filter(|fps| (1..=60).contains(fps)).unwrap_or_else(|| usage())),
            "--hold-script" => options.hold_script = true,
            "--hold-hands" => options.hold_hands = true,
            "--tracking" => options.tracking = true,
            "--no-controllers" => options.no_controllers = true,
            "--still-head" => options.still_head = true,
            "--quest-view" => options.quest_view = true,
            "--hands-lift" => options.hands_lift = value().parse().unwrap_or_else(|_| usage()),
            "--face-source" => options.face_source = tracked::FaceSource::parse(&value()).unwrap_or_else(|| usage()),
            "--left-trigger" => options.left_trigger = Some(parse_pull(&value()).unwrap_or_else(|| usage())),
            "--menu" => options.menu = Some(value().parse().unwrap_or_else(|_| usage())),
            "--right-trigger" => options.right_trigger = Some(parse_pull(&value()).unwrap_or_else(|| usage())),
            "--pause" => options.pause = Some(parse_pause(&value()).unwrap_or_else(|| usage())),
            "--drop-percent" => options.drop_percent = value().parse().unwrap_or_else(|_| usage()),
            _ => usage(),
        }
    }
    std::process::exit(native::run(options));
}
