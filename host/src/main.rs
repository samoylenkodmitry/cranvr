//! cranvr-host: streams to a Quest, through the stock ALVR client (v20.14.1) or through cranvr's
//! own protocol (see headset.rs). It shows a game when the BasaltVR OpenXR runtime inside Wine
//! connects (see runtime_link.rs). Without a game it shows a head-tracked test scene.

mod compat;
mod desktop;
mod desktop_layer;
mod display_modes;
mod game_audio;
mod game_sizes;
mod headset;
mod latency;
mod network;
mod pacer;
mod pairing;
mod pose_gaps;
mod render;
mod runtime_link;
mod screen_capture;
mod session;
mod status;
mod virtual_display;

use alvr_common::{error, info, warn, RIGHT_THUMBSTICK_CLICK_ID, RIGHT_THUMBSTICK_Y_ID, RIGHT_TRIGGER_VALUE_ID};
use alvr_filesystem::Layout;
use cranvr_common::foveation::{Foveation, FoveationConfig};
use cranvr_proto::{message::HostWishes, wire::Codec};
use cranvr_vt::{EncodedFrame, Encoder, EncoderConfig, PixelBuffer};
use headset::{Device, Event, FrameSlice, Headset, OutFrame, Protocol};
use render::{ComposeParams, Params, Renderer};
use runtime_link::{GameEvent, GameFrame, RuntimeLink, Swapchains};
use serde_json::json;
use session::{Negotiated, Options};
use std::{
    collections::VecDeque,
    env,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

/// What the event loop learns and the frame loop uses.
struct Shared {
    stream: Mutex<Option<Negotiated>>,
    /// Newest tracking sample time from the headset, in ns; 0 = none yet
    latest_sample_ns: AtomicU64,
    views: Mutex<Views>,
    force_idr: AtomicBool,
    inputs: Mutex<Inputs>,
    /// The desktop as a layer in cranvr's Quest app (desktop_layer.rs)
    layer: Arc<desktop_layer::LayerState>,
    /// The display the desktop view captures; 0 before the capture starts
    desktop_display: std::sync::atomic::AtomicU32,
}

/// Controller inputs the desktop uses.
#[derive(Clone, Copy, Default)]
struct Inputs {
    right_trigger: f32,
    right_stick_y: f32,
    /// Set by a click of the right thumbstick: put the screen in front of the user again
    recenter: bool,
}

/// The Mac desktop in VR, shown when no game runs.
#[derive(Clone, Copy)]
struct DesktopOptions {
    enabled: bool,
    /// How much lower than the controller's handle the pointing ray goes, in degrees
    pointer_pitch: f32,
    /// Capture width in pixels
    max_width: usize,
    /// Captures per second. Each capture costs GPU time in the window server (about 3 ms on a 6K
    /// display), so 30 leaves more room for the stream than 60.
    fps: i32,
    /// A virtual display of this size in points, for the headset alone (see virtual_display.rs)
    virtual_size: Option<(u32, u32)>,
    /// The screen goes where the headset looks, tilt included: for tests with a headset that
    /// lies still
    screen_in_view: bool,
    /// cranvr's Quest app may show the desktop as a compositor layer (desktop_layer.rs)
    layer: bool,
    /// The virtual display becomes the main display while a headset is connected
    vr_main: bool,
    /// The display to capture; None for the main display
    display_id: Option<u32>,
}

#[derive(Clone, Copy)]
struct Views {
    fov: [[f32; 4]; 2],
    eye_q: [[f32; 4]; 2],
    eye_p: [[f32; 4]; 2],
}

impl Default for Views {
    fn default() -> Self {
        Self {
            fov: [[-1.0, 1.0, 1.0, -1.0]; 2],
            eye_q: [[0.0, 0.0, 0.0, 1.0]; 2],
            eye_p: [[-0.032, 0.0, 0.0, 0.0], [0.032, 0.0, 0.0, 0.0]],
        }
    }
}

fn usage() -> ! {
    eprintln!(
        "usage: cranvr-host [--protocol native|alvr, default alvr] [--native-port PORT] [--fps 72|80|90|120, default 120] [--eye-width PIXELS] [--mbps N] [--trust HOSTNAME]... [--trust-all] [--pair CODE] [--no-display-lock] [--client-ip IP]... [--tcp] [--no-foveation] [--buffering-frames N] [--game-scale F] [--decoder-low-latency] [--strict-rate-control] [--no-game-audio] [--desktop] [--desktop-fps N] [--virtual-display WxH] [--vr-main-display] [--pointer-pitch DEG] [--screen-in-view] [--no-desktop-layer] [--config-dir DIR] [--status]\n\
         \n  --protocol P      how the headset's app talks to the host: alvr (the stock ALVR client,\
         \n                    the default) or native (cranvr's own protocol and client). The\
         \n                    native mode sends the Mac's sound as PCM and, with --desktop, the\
         \n                    desktop as a second video stream for the client's desktop panel.\
         \n  --native-port N   with --protocol native: the TCP port to listen on (default 9950)\
         \n  --trust HOSTNAME  accept this ALVR client (the hostname shows in the client's lobby)\
         \n  --trust-all       accept any client on the network, ALVR or native (for tests)\
         \n  --pair CODE       with --protocol native: pair the headset that shows this code. The\
         \n                    cranvr client shows a code of six digits when the host does not know it.\
         \n                    The host remembers the headset in paired-headsets.txt.\
         \n  --no-display-lock with --protocol native: render at the arrival of each pose, not at a\
         \n                    time that follows the headset's display (for comparison)\
         \n  --client-ip IP    connect to a client at this address; 127.0.0.1 with `adb forward` of\
         \n                    TCP 9943 and 9944 reaches a client on a phone (needs --tcp)\
         \n  --tcp             stream over TCP instead of UDP (needed when the client runs on this Mac)\
         \n  --no-foveation    encode every pixel at full density (needs a smaller eye size)\
         \n  --buffering-frames N  frames the headset may hold before it decodes (default 2; less\
         \n                    is less latency and more stutter on a busy network)\
         \n  --game-scale F    a game renders each eye at this share of the stream's eye size\
         \n                    (0.3 to 1, default 0.7; foveated encoding sends about a third of\
         \n                    the pixels anyway)\
         \n  --decoder-low-latency  ask the headset's decoder for its low-latency mode (Quest 3)\
         \n  --strict-rate-control  VideoToolbox's low-latency rate control: even frame sizes, one\
         \n                    frame at a time; slower per frame on an M5 (6.1 ms against 3.6 to 5.0)\
         \n  --no-game-audio   do not send this Mac's sound to the headset (it needs Screen Recording)\
         \n  --keep-display-modes  keep the Mac's displays in their modes while a game runs (by\
         \n                    default a scaled display gets its panel's own pixels: less GPU time)\
         \n  --desktop         show the Mac's main display on a screen in VR while no game runs; the\
         \n                    right controller moves the mouse, the trigger clicks, the thumbstick\
         \n                    scrolls, a thumbstick click puts the screen in front of you again\
         \n  --desktop-fps N   desktop captures per second (default 30; 60 costs more GPU time)\
         \n  --virtual-display WxH  with --desktop: a display of W x H points (HiDPI) for the headset\
         \n                    alone, such as 1920x1080; the desktop view shows it\
         \n  --vr-main-display with --virtual-display: it is the main display while a headset is\
         \n                    connected, so the menu bar, the Dock and new windows open in VR\
         \n  --pointer-pitch DEG  tilt of the pointing ray below the controller handle (default -40)\
         \n  --screen-in-view  put the screen where the headset looks, tilt included: for tests with a\
         \n                    headset that lies on a desk\
         \n  --no-desktop-layer keep the desktop in the eye stream, also for cranvr's Quest app, which\
         \n                    otherwise shows it as a sharper compositor layer. With --protocol native\
         \n                    it stops the desktop panel.\
         \n\nEnvironment: CRANVR_POINTER_DRY_RUN=1 logs the mouse events that a controller would make\
         \nand posts none (for tests with a controller on a desk).\
         \n  --status          print status lines (@cranvr JSON) on stdout, for the cranvr app\
         \n\nGames: the host writes runtime-control.txt into the config dir. Start the game under Wine\
         \nwith BASALT_CONTROL set to that file's Windows path (scripts/run-game.sh does this)."
    );
    std::process::exit(2)
}

fn parse_args() -> (Options, DesktopOptions, PathBuf) {
    let mut options = Options {
        protocol: Protocol::Alvr,
        native_port: 9950,
        fps: 120.0,
        eye_width: 0,
        eye_width_unfoveated: 0,
        eye_width_given: false,
        mbps: 100,
        trust: vec![],
        trust_all: false,
        pair_code: None,
        display_lock: true,
        client_ips: vec![],
        tcp: false,
        foveation: true,
        buffering_frames: 2.0,
        game_scale: 0.7,
        decoder_low_latency: false,
        strict_rate: false,
        game_audio: true,
        unscale_displays: true,
    };
    let mut desktop = DesktopOptions {
        enabled: false,
        pointer_pitch: -40.0,
        screen_in_view: false,
        layer: true,
        max_width: 2560,
        fps: 30,
        virtual_size: None,
        vr_main: false,
        display_id: None,
    };
    let mut config_dir = env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("Library/Application Support/cranvr");
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--protocol" => {
                options.protocol = match value().as_str() {
                    "native" => Protocol::Native,
                    "alvr" => Protocol::Alvr,
                    _ => usage(),
                }
            }
            "--native-port" => options.native_port = value().parse().unwrap_or_else(|_| usage()),
            "--fps" => options.fps = value().parse().unwrap_or_else(|_| usage()),
            "--eye-width" => options.eye_width = value().parse().unwrap_or_else(|_| usage()),
            "--mbps" => options.mbps = value().parse().unwrap_or_else(|_| usage()),
            "--trust" => options.trust.push(value()),
            "--trust-all" => options.trust_all = true,
            "--pair" => {
                options.pair_code = Some(cranvr_proto::identity::parse_code(&value()).unwrap_or_else(|| {
                    eprintln!("--pair takes the six digits that the headset shows");
                    usage()
                }))
            }
            "--no-display-lock" => options.display_lock = false,
            "--client-ip" => options.client_ips.push(value().parse().unwrap_or_else(|_| usage())),
            "--tcp" => options.tcp = true,
            "--no-foveation" => options.foveation = false,
            "--buffering-frames" => options.buffering_frames = value().parse().unwrap_or_else(|_| usage()),
            "--game-scale" => {
                options.game_scale = value().parse::<f32>().ok().filter(|s| (0.3..=1.0).contains(s)).unwrap_or_else(|| usage())
            }
            "--decoder-low-latency" => options.decoder_low_latency = true,
            "--strict-rate-control" => options.strict_rate = true,
            "--no-game-audio" => options.game_audio = false,
            "--keep-display-modes" => options.unscale_displays = false,
            "--desktop" => desktop.enabled = true,
            "--pointer-pitch" => desktop.pointer_pitch = value().parse().unwrap_or_else(|_| usage()),
            "--screen-in-view" => desktop.screen_in_view = true,
            "--no-desktop-layer" => desktop.layer = false,
            "--desktop-fps" => desktop.fps = value().parse().unwrap_or_else(|_| usage()),
            "--virtual-display" => desktop.virtual_size = Some(parse_size(&value()).unwrap_or_else(|| usage())),
            "--vr-main-display" => desktop.vr_main = true,
            "--config-dir" => config_dir = PathBuf::from(value()),
            "--status" => status::enable(),
            _ => usage(),
        }
    }
    // A width that the user gave counts for every client. Otherwise a client that takes no
    // foveation gets the size of the unfoveated table below.
    options.eye_width_given = options.eye_width != 0;
    options.eye_width_unfoveated = if options.eye_width == 0 { default_eye_width(false, options.fps as u32) } else { 0 };
    if options.eye_width == 0 {
        options.eye_width = default_eye_width(options.foveation, options.fps as u32);
    }
    (options, desktop, config_dir)
}

/// The eye width that the host picks when the user gives none.
fn default_eye_width(foveation: bool, fps: u32) -> u32 {
    // With strict rate control an M5 encoded about a megapixel in 2 ms (3200x1696 in 10.5 ms,
    // 2496x1312 in 6.9 ms), and these sizes come from that; the overlapping encoder is faster
    // (2432x1152 in 3.5 ms). With ALVR's default foveation a Quest 3 eye (2064 wide) encodes
    // as about 1216x1152. A client without foveation that got an eye of 2064 wide saw frames of
    // 4128 by 2162 pixels. They took 10.5 ms each to encode. A Quest 3 showed green blocks in
    // part of the picture. Its decoder advertises HEVC level 6.2, so the cause is not known.
    match (foveation, fps) {
        (true, _) => 2064,
        (false, 0..=72) => 1728,
        (false, 73..=90) => 1504,
        (false, _) => 1248,
    }
}

/// Starts the capture of the Mac's sound in the background: ScreenCaptureKit takes up to a few
/// seconds to answer, longer when macOS asks for Screen Recording.
fn start_game_audio() {
    thread::Builder::new()
        .name("game-audio".into())
        .spawn(|| match game_audio::Capture::start() {
            Ok(capture) => {
                info!("Game audio: capturing this Mac's sound for the headset");
                status::emit(json!({"type": "audio", "ok": true}));
                // The capture runs as long as the host
                let _capture = capture;
                loop {
                    thread::park();
                }
            }
            Err(e) => {
                warn!("Game audio: {e}");
                status::emit(json!({"type": "audio", "ok": false, "message": e}));
            }
        })
        .ok();
}

/// "1920x1080" as (1920, 1080).
fn parse_size(text: &str) -> Option<(u32, u32)> {
    let (w, h) = text.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Lets the host keep more files open than the 256 that macOS allows by default. A leak of pipes
/// in ScreenCaptureKit once took all 256, and the desktop layer then sent no frames.
fn raise_file_limit() {
    #[repr(C)]
    struct Limit {
        current: u64,
        max: u64,
    }
    extern "C" {
        fn getrlimit(resource: i32, limit: *mut Limit) -> i32;
        fn setrlimit(resource: i32, limit: *const Limit) -> i32;
    }
    const RLIMIT_NOFILE: i32 = 8;
    const WANTED: u64 = 10240;
    let mut limit = Limit { current: 0, max: 0 };
    if unsafe { getrlimit(RLIMIT_NOFILE, &mut limit) } == 0 && limit.current < WANTED {
        limit.current = WANTED.min(limit.max);
        unsafe { setrlimit(RLIMIT_NOFILE, &limit) };
    }
}

/// What the native protocol takes from the command line.
fn native_config(
    options: &Options,
    config_dir: &std::path::Path,
    desktop: headset::native::desktop::Options,
) -> headset::native::Config {
    if !options.trust.is_empty() {
        warn!("--trust does not count for the native protocol: pair the headset with --pair CODE");
    }
    headset::native::Config {
        port: options.native_port,
        pairings: Arc::new(pairing::Pairings::load(config_dir, options.pair_code)),
        trust_all: options.trust_all,
        display_lock: options.display_lock,
        desktop,
        audio: options.game_audio,
        tcp_media: options.tcp,
        wishes: HostWishes {
            // A foveated eye takes the size that the headset asks for: its recommended size, within
            // its decoder's limits. The ALVR protocol keeps the host's own choice.
            eye_width: if options.eye_width_given || !options.foveation { options.eye_width } else { 0 },
            eye_width_unfoveated: options.eye_width_unfoveated,
            refresh_hz: options.fps,
            bitrate_bps: options.mbps * 1_000_000,
            codecs: vec![Codec::Hevc],
            foveation: options.foveation.then(|| FoveationConfig::default().into()),
            full_range: true,
            slices: 1,
        },
    }
}

fn main() {
    raise_file_limit();
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--display-helper") {
        virtual_display::helper_main(&args[2..]);
    }
    if args.get(1).map(String::as_str) == Some("--unscale-displays") {
        display_modes::helper_main();
    }
    let (options, mut desktop, config_dir) = parse_args();
    std::fs::create_dir_all(&config_dir).expect("config dir");
    status::set_menu_file(config_dir.join("vr-menu.json"));
    let layout = Layout::new(&config_dir);
    let alvr = options.protocol == Protocol::Alvr;
    if alvr {
        session::prepare(&layout, &options);
        alvr_server_core::initialize_environment(layout.clone());
    }
    // The logger comes from ALVR's server core. It opens no socket and reads no session file.
    alvr_server_core::init_logging(None, Some(layout.crash_log()));
    // The native protocol sends the sound itself (headset/native/audio.rs)
    if !alvr {
        game_audio::set_sink(headset::native::audio::send);
    }
    if options.game_audio {
        start_game_audio();
    }
    // The native client shows the desktop as a panel (headset/native/desktop.rs), not as the old
    // Quest app's layer
    let desktop_panel = desktop.enabled && desktop.layer;
    if !alvr {
        desktop.layer = false;
    }
    let layer: Arc<desktop_layer::LayerState> = Arc::default();
    info!(
        "cranvr-host: {} Hz, {} px per eye, {} Mbps, config in {}",
        options.fps,
        options.eye_width,
        options.mbps,
        config_dir.display()
    );
    if options.trust_all {
        warn!("--trust-all: any client on this network can connect");
    }

    // The virtual display lives as long as this variable: to the end of main
    let mut virtual_display = None;
    if let (true, Some((width, height))) = (desktop.enabled, desktop.virtual_size) {
        match virtual_display::VirtualDisplay::create(width, height, 60.0) {
            Ok(display) => {
                info!("Virtual display {}: {}", display.id, display.describe());
                desktop.display_id = Some(display.id);
                virtual_display = Some(display);
            }
            Err(e) => {
                warn!("Virtual display: {e}; the desktop view shows the main display");
                status::emit(json!({"type": "virtual_display", "ok": false, "message": e}));
            }
        }
    }

    let (headset, mut events) = match options.protocol {
        Protocol::Alvr => headset::alvr::start(layout.session()),
        Protocol::Native => {
            let panel = headset::native::desktop::Options {
                enabled: desktop_panel,
                display_id: desktop.display_id,
                layer: Arc::clone(&layer),
                mbps: headset::native::desktop::MBPS,
            };
            headset::native::start(native_config(&options, &config_dir, panel))
        }
        .unwrap_or_else(|e| {
            error!("Native protocol: cannot listen on port {}: {e}", options.native_port);
            std::process::exit(1)
        }),
    };
    let shared = Arc::new(Shared {
        stream: Mutex::new(None),
        latest_sample_ns: AtomicU64::new(0),
        views: Mutex::new(Views::default()),
        force_idr: AtomicBool::new(true),
        inputs: Mutex::new(Inputs::default()),
        layer,
        desktop_display: std::sync::atomic::AtomicU32::new(0),
    });

    let (link, game_events) = {
        let headset = Arc::clone(&headset);
        let on_haptic = move |h: runtime_link::Haptic| headset.send_haptics(h);
        let control_file = config_dir.join("runtime-control.txt");
        RuntimeLink::start(&control_file, runtime_link::MAX_VIEW, options.fps as u32, on_haptic).expect("runtime control socket")
    };
    info!("Game runtime control file: {}", link.control_file().display());

    {
        let headset = Arc::clone(&headset);
        let shared = Arc::clone(&shared);
        let link = Arc::clone(&link);
        thread::Builder::new()
            .name("frames".into())
            .spawn(move || frame_loop(headset, shared, link, game_events, (options.mbps, options.strict_rate, options.unscale_displays), desktop))
            .unwrap();
    }

    let mut watch = network::StreamWatch::default();
    let mut layer_stop = Arc::new(AtomicBool::new(false));
    // The console in the headset gets the menu when it connects and when it changes
    let mut console = ConsoleMenu::default();
    loop {
        console.update(&*headset);
        let event = match events.next(Duration::from_millis(50)) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match event {
            Event::Connected { stream: negotiated, ip: client_ip } => {
                watch.connected();
                console.connected();
                layer_stop.store(true, Ordering::Relaxed);
                layer_stop = Arc::new(AtomicBool::new(false));
                let display = shared.desktop_display.load(Ordering::Relaxed);
                if let (true, Some(ip), true) = (desktop.enabled && desktop.layer, client_ip, display != 0) {
                    desktop_layer::start(ip, display, Arc::clone(&shared.layer), Arc::clone(&layer_stop));
                }
                info!("Headset address: {}", client_ip.map_or("unknown".into(), |ip| ip.to_string()));
                if let Some(problem) = client_ip.and_then(network::check_route) {
                    warn!("{}", problem["message"].as_str().unwrap_or_default());
                    status::emit(problem);
                }
                match &negotiated {
                    Some(n) if n.codec == Codec::Hevc => info!("Headset connected: {n:?}"),
                    Some(n) => error!("Headset connected with codec {:?}, but this host only encodes HEVC", n.codec),
                    None => error!("Headset connected, but session.json has no stream config"),
                }
                if let Some(n) = &negotiated {
                    status::emit(json!({"type": "headset", "connected": true, "eye": [n.eye_width, n.eye_height], "fps": n.fps, "foveated": n.foveated}));
                    // games that start from now on render at the stream's eye size
                    let scaled = |v: u32| (v as f32 * options.game_scale).round() as u32;
                    if let Err(e) = link.write_control_file((scaled(n.eye_width), scaled(n.eye_height)), n.fps.round() as u32) {
                        warn!("Game runtime control file: {e}");
                    }
                }
                *shared.stream.lock().unwrap() = negotiated.filter(|n| n.codec == Codec::Hevc);
                shared.force_idr.store(true, Ordering::Relaxed);
            }
            Event::Disconnected => {
                info!("Headset disconnected");
                console.disconnected();
                layer_stop.store(true, Ordering::Relaxed);
                if let Some(display) = virtual_display.as_mut().filter(|d| d.is_main()) {
                    match display.restore() {
                        Ok(()) => status::emit(json!({"type": "vr_display", "main": false, "display": display.id})),
                        Err(e) => warn!("Virtual display: {e}"),
                    }
                }
                if let Some(problem) = watch.disconnected(options.tcp) {
                    warn!("{}", problem["message"].as_str().unwrap_or_default());
                    status::emit(problem);
                }
                status::emit(json!({"type": "headset", "connected": false}));
                *shared.stream.lock().unwrap() = None;
                shared.latest_sample_ns.store(0, Ordering::Relaxed);
            }
            Event::Tracking { sample_ns: ns } => {
                shared.latest_sample_ns.store(ns, Ordering::Relaxed);
                pose_gaps::record();
                watch.pose();
                // The switch waits for the first pose: a headset that connects and leaves again,
                // as at the first connection or behind a VPN, must not move every window
                if let (true, Some(display)) = (desktop.vr_main, virtual_display.as_mut().filter(|d| !d.is_main())) {
                    match display.make_main() {
                        Ok(()) => {
                            info!("Virtual display {} is the main display while the headset streams", display.id);
                            status::emit(json!({"type": "vr_display", "main": true, "display": display.id}));
                        }
                        Err(e) => {
                            warn!("Virtual display: {e}");
                            // A refusal would repeat at every pose: no more tries in this run
                            desktop.vr_main = false;
                        }
                    }
                }
                link.new_pose();
                if link.is_connected() {
                    if let Some(head) = headset.motion(Device::Head, ns) {
                        link.head_pose(ns, &head);
                    }
                    let mut mask = 0;
                    for (hand, device) in [Device::LeftHand, Device::RightHand].into_iter().enumerate() {
                        if let Some(motion) = headset.motion(device, ns) {
                            link.controller(ns, hand as u32, &motion);
                            mask |= 1 << hand;
                        }
                    }
                    link.controller_mask(ns, mask);
                }
            }
            Event::Views(config) => {
                let mut views = shared.views.lock().unwrap();
                let mut angles = [[0.0; 4]; 2];
                for e in 0..2 {
                    let f = config.fov[e];
                    views.fov[e] = [f.left.tan(), f.right.tan(), f.up.tan(), f.down.tan()];
                    angles[e] = [f.left, f.right, f.up, f.down];
                    let t = config.local_view_transforms[e];
                    views.eye_q[e] = t.orientation.to_array();
                    views.eye_p[e] = [t.position.x, t.position.y, t.position.z, 0.0];
                }
                let ipd = config.local_view_transforms[1].position.x - config.local_view_transforms[0].position.x;
                let degrees = |a: [f32; 4]| a.map(|r| format!("{:.1}", r.to_degrees())).join(", ");
                info!(
                    "Headset views (left, right, up, down in degrees): left eye {}, right eye {}; IPD {:.1} mm",
                    degrees(angles[0]),
                    degrees(angles[1]),
                    ipd * 1000.0
                );
                link.set_view_config(ipd, angles);
            }
            Event::Buttons(buttons) => {
                let mut inputs = shared.inputs.lock().unwrap();
                for button in buttons {
                    let value = button.value;
                    if button.path_id == *RIGHT_TRIGGER_VALUE_ID {
                        inputs.right_trigger = value;
                    } else if button.path_id == *RIGHT_THUMBSTICK_Y_ID {
                        inputs.right_stick_y = value;
                    } else if button.path_id == *RIGHT_THUMBSTICK_CLICK_ID && value > 0.5 {
                        inputs.recenter = true;
                    }
                    link.button(button.path_id, button.scalar, value);
                }
            }
            Event::Console(request) => {
                info!("The console asks for {request}");
                if !status::console_request(&request) {
                    warn!("A console request that is no JSON object");
                }
            }
            Event::RequestIdr => shared.force_idr.store(true, Ordering::Relaxed),
            // The headset's new origin makes the old screen place wrong: put the screen in front
            // of the user again, as a click of the right thumbstick does
            Event::Recenter => shared.inputs.lock().unwrap().recenter = true,
            Event::Stop => {
                info!("The headset link asked the host to stop");
                break;
            }
            Event::Other => {}
        }
    }
}

/// How often the host looks for a new menu for the headset's console
const MENU_EVERY: Duration = Duration::from_secs(1);

/// Sends the console's menu to a connected headset, when it differs from the last one that went out.
#[derive(Default)]
struct ConsoleMenu {
    connected: bool,
    last: Option<String>,
    checked: Option<Instant>,
}

impl ConsoleMenu {
    fn connected(&mut self) {
        self.connected = true;
        self.last = None;
        self.checked = None;
    }

    fn disconnected(&mut self) {
        self.connected = false;
    }

    fn update(&mut self, headset: &dyn Headset) {
        if !self.connected || self.checked.is_some_and(|at| at.elapsed() < MENU_EVERY) {
            return;
        }
        self.checked = Some(Instant::now());
        let Some(menu) = desktop_layer::menu().and_then(|bytes| String::from_utf8(bytes).ok()) else { return };
        if self.last.as_deref() != Some(&menu) {
            headset.send_menu(&menu);
            self.last = Some(menu);
        }
    }
}

/// One rendered frame on its way to the encoder.
struct EncodeJob {
    encoder: Arc<Encoder>,
    buffer: PixelBuffer,
    index: i64,
    sample: u64,
    idr: bool,
    bitrate: Option<u64>,
}

/// Hands frames to the encoder on its own thread. With strict rate control the encode call blocks
/// for the whole encode, and the next frame renders meanwhile. Without it the call returns at once
/// and the encoder overlaps frames; `MAX_IN_FLIGHT` bounds them.
fn encode_loop(jobs: mpsc::Receiver<EncodeJob>) {
    realtime_thread();
    for job in jobs {
        if let Some(b) = job.bitrate {
            job.encoder.set_bitrate(b);
        }
        if let Err(e) = job.encoder.encode(&job.buffer, job.index, job.sample, job.idr) {
            error!("Encode: {e}");
        }
    }
}

/// What the headset needs to show a frame, besides the encoder's output.
struct FrameNote {
    sample: u64,
    eye_poses: [alvr_common::Pose; 2],
    /// The encoded frame's size
    rect: [u32; 4],
    /// The plan that the frame followed
    plan: Option<headset::FramePlan>,
}

/// Notes of frames that the encoder has not returned yet. The encoder holds at most
/// `MAX_IN_FLIGHT` frames. A frame that it drops leaves its note behind, so the list has a bound.
const MAX_NOTES: usize = 16;

impl FrameNote {
    fn push(notes: &Mutex<VecDeque<FrameNote>>, note: FrameNote) {
        let mut notes = notes.lock().unwrap();
        if notes.len() >= MAX_NOTES {
            notes.pop_front();
        }
        notes.push_back(note);
    }

    /// Forgets the note of a frame that the encoder refused.
    fn cancel(notes: &Mutex<VecDeque<FrameNote>>, sample: u64) {
        notes.lock().unwrap().retain(|n| n.sample != sample);
    }

    /// The note of the frame with this sample. Older notes belong to frames that never came back.
    fn take(notes: &Mutex<VecDeque<FrameNote>>, sample: u64) -> FrameNote {
        let mut notes = notes.lock().unwrap();
        while let Some(note) = notes.pop_front() {
            if note.sample == sample {
                return note;
            }
        }
        FrameNote { sample, eye_poses: Default::default(), rect: [0; 4], plan: None }
    }
}

/// A planned frame may reuse the sample of the frame before it this many times in a row.
const MAX_REPEATS: u64 = 2;

/// Frames the encoder may hold at once. More would wait inside the encoder and add latency.
const MAX_IN_FLIGHT: usize = 2;

/// Asks macOS to schedule this thread before ordinary work (and to run the GPU work it submits
/// sooner). Without it, a busy Mac delays the frame threads and the stream drops frames.
fn realtime_thread() {
    extern "C" {
        fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
    }
    const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
    unsafe { pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0) };
}

/// A game that sent no frame for this long counts as paused, and the test scene comes back.
const GAME_IDLE: Duration = Duration::from_millis(500);
/// A game that sent frames and then none for this long gives the desktop back, with the console
/// and its Stop button. SUPERHOT VR sent no frame after the headset left for 11 seconds and came
/// back, and the user saw only black.
const GAME_STALL: Duration = Duration::from_secs(3);
/// A game that sent no frame this long after it connected gives the desktop back too.
const GAME_LOAD: Duration = Duration::from_secs(90);

/// What the frame loop knows about the game under Wine.
#[derive(Default)]
struct Game {
    swapchains: Swapchains,
    connected: bool,
    /// When the game connected
    since: Option<Instant>,
    last_frame: Option<Instant>,
    /// The game's program path, from the runtime
    program: Option<String>,
    /// The game's frame rate in each stats window after its loading, until the host decides on
    /// its size for the next start
    rates: Vec<f64>,
    /// Frames the game delivered since the last stats window, encoded or not
    delivered: u32,
    /// Set the displays to their panels' pixels while the game runs
    unscale: bool,
    /// The helper that holds those modes
    unscaled: Option<display_modes::Unscaled>,
}

impl Game {
    /// Whether the game hides the desktop: while it sends frames, and while it loads.
    fn hides_desktop(&self, frame_now: bool) -> bool {
        frame_now
            || self.connected
                && match self.last_frame {
                    Some(at) => at.elapsed() < GAME_STALL,
                    None => self.since.is_some_and(|since| since.elapsed() < GAME_LOAD),
                }
    }

    fn active(&self) -> bool {
        self.connected && self.last_frame.is_some_and(|t| t.elapsed() < GAME_IDLE)
    }

    /// Applies one event. A frame replaces `newest`; the replaced frame goes back to the game.
    fn apply(&mut self, event: GameEvent, newest: &mut Option<GameFrame>, link: &RuntimeLink, dropped: &mut u32) {
        match event {
            GameEvent::Connected { renderer, program } => {
                info!("Game connected ({renderer})");
                self.program = program;
                self.rates.clear();
                status::emit(json!({"type": "game", "connected": true, "name": renderer, "program": self.program}));
                self.swapchains.clear();
                self.connected = true;
                self.since = Some(Instant::now());
                if self.unscale && self.unscaled.is_none() {
                    match display_modes::Unscaled::start() {
                        Ok(helper) => self.unscaled = Some(helper),
                        Err(e) => warn!("Display modes: {e}"),
                    }
                }
            }
            GameEvent::Swapchain { id, images } => self.swapchains.insert(id, images),
            GameEvent::SwapchainGone { id } => self.swapchains.remove(id),
            GameEvent::Frame(frame) => {
                self.delivered += 1;
                if let Some(old) = newest.replace(frame) {
                    link.frame_consumed(old.number);
                    *dropped += 1;
                }
            }
            GameEvent::HeadPose => {}
            GameEvent::Disconnected => {
                if self.connected {
                    info!("Game disconnected");
                    status::emit(json!({"type": "game", "connected": false}));
                }
                *newest = None;
                self.swapchains.clear();
                self.connected = false;
                self.since = None;
                // The displays get their modes back
                self.unscaled = None;
                self.last_frame = None;
            }
        }
    }
}

/// Encodes one frame per game frame while a game runs, and one test scene frame per headset
/// frame otherwise.
fn frame_loop(
    headset: Arc<dyn Headset>,
    shared: Arc<Shared>,
    link: Arc<RuntimeLink>,
    game_events: mpsc::Receiver<GameEvent>,
    (mbps, strict_rate, unscale_displays): (u64, bool, bool),
    desktop_options: DesktopOptions,
) {
    realtime_thread();
    let mut renderer = match Renderer::new() {
        Ok(r) => r,
        Err(e) => {
            error!("Metal: {e}");
            return;
        }
    };
    info!("Rendering on {}", renderer.device_name());

    let capture = desktop_options
        .enabled
        .then(|| desktop::Capture::start(desktop_options.display_id, desktop_options.max_width, desktop_options.fps))
        .and_then(|c| c.map_err(|e| error!("Desktop: {e}")).ok());
    match &capture {
        Some(c) => {
            shared.desktop_display.store(c.display_id, Ordering::Relaxed);
            let is_virtual = desktop_options.display_id == Some(c.display_id);
            info!("Desktop: capturing display {} (aspect {:.2}, virtual: {is_virtual})", c.display_id, c.aspect);
            let message = if is_virtual { "the virtual display".to_string() } else { format!("display {}", c.display_id) };
            status::emit(json!({"type": "desktop", "ok": true, "message": message, "display": c.display_id, "virtual": is_virtual}));
        }
        None if desktop_options.enabled => {
            status::emit(json!({"type": "desktop", "ok": false, "message": "no screen capture; see the log"}));
        }
        None => {}
    }
    let mut pointer = capture.as_ref().map(|c| desktop::Pointer::new(c.display_id));
    let mut screen: Option<desktop::Screen> = None;
    let mut desktop_seq = 0u64;

    let (sender, receiver) = mpsc::channel::<EncodedFrame>();
    // Encoder time of the frames encoded since the last stats line, and their count
    let encoder_time_us = Arc::new(AtomicU64::new(0));
    let encoded_frames = Arc::new(AtomicU64::new(0));
    // What the video-send thread needs to know about each frame besides the encoder's output. The
    // frame loop notes it when it hands the frame to the encoder. The thread takes it by the
    // frame's tracking sample.
    let notes = Arc::new(Mutex::new(VecDeque::<FrameNote>::new()));
    {
        let headset = Arc::clone(&headset);
        let notes = Arc::clone(&notes);
        let (encoder_time_us, encoded_frames) = (Arc::clone(&encoder_time_us), Arc::clone(&encoded_frames));
        thread::Builder::new()
            .name("video-send".into())
            .spawn(move || {
                realtime_thread();
                let mut last_config: Option<Vec<u8>> = None;
                for frame in receiver {
                    encoder_time_us.fetch_add(frame.encode_time.as_micros() as u64, Ordering::Relaxed);
                    encoded_frames.fetch_add(1, Ordering::Relaxed);
                    if let Some(config) = frame.config_nals {
                        if last_config.as_ref() != Some(&config) {
                            headset.set_decoder_config(config.clone());
                            last_config = Some(config);
                        }
                    }
                    let note = FrameNote::take(&notes, frame.timestamp_ns);
                    headset.send_frame(OutFrame {
                        plan: note.plan,
                        sample_ns: frame.timestamp_ns,
                        idr: frame.is_idr,
                        eye_poses: note.eye_poses,
                        slices: vec![FrameSlice { rect: note.rect, data: frame.data }],
                    });
                }
            })
            .unwrap();
    }

    // One frame may wait while another encodes
    let (jobs, job_receiver) = mpsc::sync_channel::<EncodeJob>(1);
    thread::Builder::new().name("encode".into()).spawn(move || encode_loop(job_receiver)).unwrap();

    let mut encoder: Option<(Arc<Encoder>, Negotiated, Option<Foveation>)> = None;
    let mut game = Game { unscale: unscale_displays, ..Game::default() };
    let mut last_sample = 0;
    // How many frames in a row rendered again for `last_sample`
    let mut repeats = 0u64;
    // The stream's rate replaces this when the encoder starts
    let mut pacer = pacer::Pacer::new(120.0).with_ready(headset.pacer_ready());
    let mut frame_index = 0i64;
    // The display lock's plan that the loop waits for, with the instant at which its start is due
    let mut pending_plan: Option<(headset::FramePlan, Instant)> = None;
    let mut bitrate = mbps * 1_000_000;
    let mut new_bitrate = None;
    let (mut count, mut game_count, mut gpu_ms, mut gpu_exec_ms, mut since) = (0u32, 0u32, 0.0f64, 0.0f64, Instant::now());
    // Stats windows in a row in which a running game made fewer frames than the headset shows
    let mut slow_windows = 0u32;
    let sizes = game_sizes::Sizes::new(link.control_dir());
    // frames not encoded: no new tracking sample, no free buffer, encoder still busy, game frames
    // replaced by a newer one, game frames with an unknown swapchain image
    let (mut no_sample, mut no_buffer, mut busy, mut dropped, mut missing) = (0u32, 0u32, 0u32, 0u32, 0u32);

    loop {
        // While a game runs its frames set the pace. Otherwise the display lock plans the frame,
        // when the link has one: the frame starts at a time that makes it reach the headset's
        // display on its grid. Without a plan the headset's vsync sets the pace.
        // A plan holds while its start lies ahead: a pose that arrives meanwhile wakes the loop,
        // and the loop goes on waiting for the same start. A plan whose start passed without a
        // frame is made again.
        let planned = if game.active() {
            pending_plan = None;
            None
        } else {
            match pending_plan {
                Some((plan, start_at)) if Instant::now() < start_at => {
                    Some(headset::FramePlan { start_in: start_at - Instant::now(), ..plan })
                }
                _ => {
                    let plan = headset.frame_plan();
                    pending_plan = plan.map(|plan| (plan, Instant::now() + plan.start_in));
                    plan
                }
            }
        };
        let planned_at = Instant::now();
        let wait = if game.active() {
            GAME_IDLE
        } else if let Some(plan) = &planned {
            plan.start_in
        } else {
            headset.until_next_vsync().unwrap_or(Duration::from_millis(5))
        };
        let mut game_frame = None;
        match game_events.recv_timeout(wait) {
            Ok(event) => {
                let mut pose = false;
                let mut next = Some(event);
                while let Some(event) = next {
                    match event {
                        GameEvent::HeadPose => pose = true,
                        event => game.apply(event, &mut game_frame, &link, &mut dropped),
                    }
                    next = game_events.try_recv().ok();
                }
                // Without a game the host's own scene draws as soon as a pose arrives: waiting
                // for the next vsync would add up to a frame between the pose and the picture.
                // The pacer keeps it to the headset's refresh rate.
                // A planned frame starts at its planned time, not at the arrival of a pose.
                if planned.is_some() && game_frame.is_none() {
                    continue;
                }
                if game_frame.is_none() && (game.active() || !pose || !pacer.ready()) {
                    continue;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                // The plan holds the frame to the display's grid, so the pacer's budget does not
                // refuse it.
                if game.active() || (planned.is_none() && !pacer.ready()) {
                    continue;
                }
            }
            Err(RecvTimeoutError::Disconnected) => thread::sleep(wait),
        }
        let give_back = |frame: &Option<GameFrame>| {
            if let Some(f) = frame {
                link.frame_consumed(f.number);
            }
        };

        let Some(stream) = shared.stream.lock().unwrap().clone() else {
            give_back(&game_frame);
            encoder = None;
            continue;
        };
        if encoder.as_ref().is_none_or(|(_, n, _)| *n != stream) {
            let foveation = stream
                .foveated
                .then(|| Foveation::new([stream.eye_width, stream.eye_height], &stream.foveation));
            let (eye_w, eye_h) = foveation.map_or((stream.eye_width, stream.eye_height), |f| (f.optimized[0], f.optimized[1]));
            let config = EncoderConfig {
                width: eye_w * 2,
                height: eye_h,
                fps: stream.fps,
                bitrate_bps: bitrate,
                full_range: stream.full_range,
                strict_rate,
            };
            encoder = match Encoder::new(&config, sender.clone()) {
                Ok(e) => {
                    info!(
                        "Encoder: HEVC {}x{} at {} Hz{}",
                        config.width,
                        config.height,
                        config.fps,
                        if foveation.is_some() { " (foveated)" } else { "" }
                    );
                    pacer.set_fps(stream.fps);
                    Some((Arc::new(e), stream.clone(), foveation))
                }
                Err(e) => {
                    error!("Encoder: {e}");
                    thread::sleep(Duration::from_secs(1));
                    None
                }
            };
            shared.force_idr.store(true, Ordering::Relaxed);
        }
        let Some((enc, _, foveation)) = &encoder else {
            give_back(&game_frame);
            continue;
        };
        let (eye_w, eye_h) = foveation.map_or((stream.eye_width, stream.eye_height), |f| (f.optimized[0], f.optimized[1]));
        let views = *shared.views.lock().unwrap();

        let sample = match &game_frame {
            Some(f) => {
                game.last_frame = Some(Instant::now());
                f.tracking_ns
            }
            None => shared.latest_sample_ns.load(Ordering::Relaxed),
        };
        // A planned frame that cannot go out waits a moment, so the loop does not spin
        let retry_later = || {
            if planned.is_some() {
                thread::sleep(Duration::from_micros(500));
            }
        };
        // The client shows each frame for its own tracking sample, so a sample is sent once. Two
        // frames are the exception. A planned frame's start does not wait for a pose: when no
        // newer pose came, it renders again for the same sample, with the pose moved on to the
        // new target. A game's frame comes when the game finishes it. A game that runs near the
        // rate of the poses finishes two frames for one pose now and then, and the second has new
        // content. Dropping it wasted a frame (a fifth of the game's frames in a test with Beat
        // Saber on a loaded Mac) that the headset would show one point later.
        // The frame needs a number of its own, because the notes, the encoder and the headset's
        // reports find a frame by it. The number is the sample's time plus the count of repeats.
        // The pose lookup takes the sample for the nearest one.
        let repeat = (planned.is_some() || (game_frame.is_some() && headset.repeats_samples())) && sample != 0 && sample == last_sample && repeats < MAX_REPEATS;
        if sample == 0 || (sample == last_sample && !repeat) {
            give_back(&game_frame);
            no_sample += 1;
            retry_later();
            continue;
        }
        let sample_id = if repeat { sample + repeats + 1 } else { sample };
        if enc.in_flight() >= MAX_IN_FLIGHT {
            give_back(&game_frame);
            busy += 1;
            retry_later();
            continue;
        }
        let Some(buffer) = enc.pixel_buffer() else {
            give_back(&game_frame);
            no_buffer += 1;
            retry_later();
            continue;
        };
        if let Some(plan) = &planned {
            // The time that the wait took beyond the plan's start
            headset.frame_started(plan.target, planned_at.elapsed().saturating_sub(plan.start_in));
        }
        // The pose of a device for this frame: at the plan's target when the frame follows a plan
        let motion_of = |device: Device| match &planned {
            Some(plan) => headset.motion_at(device, sample, plan.target),
            None => headset.motion(device, sample),
        };
        let gpu_start = Instant::now();

        // A connected game hides the desktop, also while it loads and sends no frames
        shared.layer.game.store(game.hides_desktop(game_frame.is_some()), Ordering::Relaxed);
        // The eye stream needs the desktop capture only without a game and without the layer
        if let Some(c) = &capture {
            c.set_running(!game.connected && game_frame.is_none() && !shared.layer.active.load(Ordering::Relaxed));
        }
        let drawn = match &game_frame {
            Some(frame) => {
                let images = (game.swapchains.image(&frame.views[0]), game.swapchains.image(&frame.views[1]));
                let result = match images {
                    (Some(left), Some(right)) => {
                        let mut params = ComposeParams {
                            fov: views.fov,
                            eye_w,
                            eye_h,
                            full_range: stream.full_range as u32,
                            ..Default::default()
                        };
                        for (e, (view, image)) in frame.views.iter().zip([left, right]).enumerate() {
                            let (w, h) = (image.width() as f32, image.height() as f32);
                            let r = view.rect;
                            params.src_fov[e] = view.fov_tan;
                            params.src_rect[e] = [r[0] as f32 / w, r[1] as f32 / h, r[2] as f32 / w, r[3] as f32 / h];
                        }
                        params.set_foveation(foveation.as_ref());
                        renderer.compose(&buffer, left, right, &params).map(|()| true)
                    }
                    _ => {
                        missing += 1;
                        Ok(false)
                    }
                };
                // compose() waited for the GPU, so the game may draw into these images again
                link.frame_consumed(frame.number);
                result
            }
            None => {
                let mut params = Params {
                    fov: views.fov,
                    head_q: [0.0, 0.0, 0.0, 1.0],
                    head_p: [0.0, 1.6, 0.0, 0.0],
                    eye_q: views.eye_q,
                    eye_p: views.eye_p,
                    eye_w,
                    eye_h,
                    full_range: stream.full_range as u32,
                    ..Default::default()
                };
                params.set_foveation(foveation.as_ref());
                let head = motion_of(Device::Head);
                if let Some(head) = &head {
                    params.head_q = head.pose.orientation.to_array();
                    let p = head.pose.position;
                    params.head_p = [p.x, p.y, p.z, 0.0];
                }
                let mut right_hand = None;
                // cranvr's Quest app draws the controllers itself while it shows the layer
                let layer = shared.layer.active.load(Ordering::Relaxed);
                for (i, device) in [Device::LeftHand, Device::RightHand].into_iter().enumerate() {
                    if let Some(hand) = motion_of(device) {
                        let p = hand.pose.position;
                        if !layer {
                            params.ctl[i] = [p.x, p.y, p.z, 1.0];
                        }
                        if i == 1 {
                            right_hand = Some(hand.pose);
                        }
                    }
                }
                if let (Some(capture), Some(pointer), false) = (&capture, &mut pointer, game.connected) {
                    params.scene = 1;
                    let recenter = std::mem::replace(&mut shared.inputs.lock().unwrap().recenter, false);
                    let input = *shared.inputs.lock().unwrap();
                    if let Some(head) = &head {
                        if screen.is_none() || recenter {
                            screen = Some(if desktop_options.screen_in_view {
                                desktop::Screen::in_view_of(&head.pose, capture.aspect)
                            } else {
                                desktop::Screen::in_front_of(&head.pose, capture.aspect)
                            });
                        }
                    }
                    // cranvr's Quest app places the panels itself, finds where the controller
                    // points and tells the layer link: the eye stream leaves the screen out
                    if let (Some(s), false) = (screen, layer) {
                        params.screen = s.to_params();
                        let hit = right_hand.and_then(|grip| {
                            let (origin, direction) = desktop::pointing_ray(&grip, desktop_options.pointer_pitch);
                            s.hit(origin, direction)
                        });
                        pointer.update(hit, input.right_trigger, input.right_stick_y);
                    }
                    if let Some((seq, image)) = capture.latest() {
                        if seq != desktop_seq {
                            match renderer.set_desktop(&image) {
                                Ok(true) => desktop_seq = seq,
                                Ok(false) => {}
                                Err(e) => error!("Desktop: {e}"),
                            }
                        }
                    }
                }
                renderer.render(&buffer, &params).map(|()| true)
            }
        };
        match drawn {
            Ok(true) => {}
            Ok(false) => continue,
            Err(e) => {
                error!("Render: {e}");
                continue;
            }
        }
        gpu_ms += gpu_start.elapsed().as_secs_f64() * 1000.0;
        gpu_exec_ms += renderer.last_gpu_s * 1000.0;
        repeats = if repeat { repeats + 1 } else { 0 };
        last_sample = sample;
        if game_frame.is_none() {
            pacer.spend();
        }

        if let Some(b) = headset.bitrate_bps() {
            if b > 0 && b != bitrate {
                bitrate = b;
                new_bitrate = Some(b);
            }
        }
        let job = EncodeJob {
            encoder: Arc::clone(enc),
            buffer,
            index: frame_index,
            sample: sample_id,
            idr: shared.force_idr.swap(false, Ordering::Relaxed),
            bitrate: new_bitrate.take(),
        };
        let eye_poses = if headset.wants_eye_poses() {
            let head = motion_of(Device::Head).map(|m| m.pose).unwrap_or_default();
            headset::eye_poses(&head, views.eye_q, views.eye_p)
        } else {
            Default::default()
        };
        FrameNote::push(&notes, FrameNote { sample: sample_id, eye_poses, rect: [0, 0, eye_w * 2, eye_h], plan: planned });
        match jobs.try_send(job) {
            Ok(()) => {
                headset.frame_submitted(sample_id);
                frame_index += 1;
                count += 1;
                game_count += game_frame.is_some() as u32;
            }
            Err(mpsc::TrySendError::Full(job)) => {
                FrameNote::cancel(&notes, sample_id);
                busy += 1;
                if job.idr {
                    shared.force_idr.store(true, Ordering::Relaxed);
                }
                new_bitrate = job.bitrate.or(new_bitrate);
            }
            Err(mpsc::TrySendError::Disconnected(_)) => return,
        }

        if since.elapsed() >= Duration::from_secs(5) {
            if capture.is_some() {
                info!(
                    "Desktop: {} captures shown, copy GPU {:.2} ms each",
                    renderer.uploads,
                    renderer.upload_gpu_s * 1000.0 / renderer.uploads.max(1) as f64
                );
                (renderer.uploads, renderer.upload_gpu_s) = (0, 0.0);
            }
            let secs = since.elapsed().as_secs_f64();
            let n = count.max(1) as f64;
            let encoded = encoded_frames.swap(0, Ordering::Relaxed).max(1) as f64;
            let encode_ms = encoder_time_us.swap(0, Ordering::Relaxed) as f64 / 1000.0 / encoded;
            let latency = headset.take_stats();
            if let Some(l) = &latency {
                info!("Headset latency (ms, average of {} reports): {}", l["frames"], l);
            }
            let (audio_s, unreadable) = game_audio::take_counts();
            if unreadable > 0 {
                warn!("Game audio: {unreadable} buffers in a format the host cannot read");
            }
            let poses = pose_gaps::take(secs);
            let input = link.take_counts();
            if game.connected || input.buttons > 0 || input.haptics > 0 {
                info!(
                    "Game input: {} left and {} right controller poses (tracked mask {:#b}), {} button and axis values, {} haptic pulses from the game",
                    input.poses[0], input.poses[1], input.mask, input.buttons, input.haptics
                );
            }
            let layer_fps = shared.layer.frames.swap(0, Ordering::Relaxed) as f64 / secs;
            status::emit(json!({
                "type": "stats",
                "fps": count as f64 / secs,
                "pose_gap_ms": poses,
                "layer_fps": layer_fps,
                "game_frames": game_count,
                "render_ms": gpu_ms / n,
                "encode_ms": encode_ms,
                "mbps": bitrate / 1_000_000,
                "audio_s": audio_s,
                "latency": latency,
            }));
            info!(
                "{:.1} fps ({game_count} game frames), render {:.2} ms (GPU {:.2} ms), encode {:.2} ms, {} Mbps target, skipped: {no_sample} no new pose, {no_buffer} no buffer, {busy} encoder busy, {dropped} game frames replaced, {missing} unknown images; desktop layer {layer_fps:.1} fps; poses {poses}",
                count as f64 / secs,
                gpu_ms / n,
                gpu_exec_ms / n,
                encode_ms,
                bitrate / 1_000_000
            );
            // A game below the headset's rate cost 10 to 18 ms in tests: the headset waits for frames and
            // its own loop slows down. The count starts 20 s after the game connects, past its
            // loading, and the warning needs two slow windows in a row.
            // The frames that the game delivered: a frame that comes without a new pose is not
            // encoded, and a smaller size would not change that
            let game_fps = std::mem::take(&mut game.delivered) as f64 / secs;
            let rate = stream.fps as f64;
            let loaded = game.since.is_some_and(|t| t.elapsed() > Duration::from_secs(20));
            slow_windows = if loaded && game.active() && game_fps < 0.95 * rate { slow_windows + 1 } else { 0 };
            if slow_windows % 12 == 2 {
                warn!(
                    "Game: {game_fps:.0} fps, below the headset's {rate:.0} Hz, which adds latency. Lower the game's anti-aliasing, or the game render size in cranvr's settings (--game-scale)."
                );
            }
            // After a minute of play the host decides once whether the game gets a smaller size
            // at its next start
            if loaded && game.active() && game.rates.len() < game_sizes::WINDOWS {
                game.rates.push(game_fps);
                if let (game_sizes::WINDOWS, Some(program)) = (game.rates.len(), &game.program) {
                    if let Some((old, new)) = sizes.learn(program, &game.rates, rate) {
                        let name = program.rsplit('\\').next().unwrap_or(program);
                        warn!(
                            "Game: {name} stayed below {rate:.0} Hz. Its next start renders at {:.0}% of the size set in cranvr, after {:.0}%.",
                            new * 100.0,
                            old * 100.0
                        );
                    }
                }
            }
            (count, game_count, gpu_ms, gpu_exec_ms, since) = (0, 0, 0.0, 0.0, Instant::now());
            (no_sample, no_buffer, busy, dropped, missing) = (0, 0, 0, 0, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stalled_game_gives_the_desktop_back() {
        let ago = |s: u64| Instant::now() - Duration::from_secs(s);
        let game = |since: u64, last: Option<u64>| Game { connected: true, since: Some(ago(since)), last_frame: last.map(ago), ..Default::default() };
        // loading, running, a frame now
        assert!(game(10, None).hides_desktop(false));
        assert!(game(60, Some(1)).hides_desktop(false));
        assert!(game(60, Some(30)).hides_desktop(true));
        // stalled after frames, or no frame long after it connected
        assert!(!game(60, Some(5)).hides_desktop(false));
        assert!(!game(120, None).hides_desktop(false));
        assert!(!Game::default().hides_desktop(false));
    }
}
