//! The link to the host: connect, receive, decode and report.
//!
//! One thread connects. After the handshake it runs the session with these threads:
//! - the TCP read loop (the thread that connected) and, with UDP media, a UDP read loop. Both
//!   reassemble media packets, restore lost packets with the parity and hand whole frames on;
//! - the decoder thread, which feeds Cranpose's `VideoDecoder` and sends the frame timing. The
//!   decoder hands its pictures to the headset's streamed scene (see `ui.rs`);
//! - the tracking sender (see `tracking.rs`);
//! - the upkeep thread, which pings the host and reports the loss;
//! - the stats thread, which publishes numbers to the hub for the UI.
//!
//! No thread touches Cranpose state. The threads write into the hub, and the UI reads from it.

use crate::{
    hub::{
        streamed_views, DecoderView, Device, HeadsetInfo, Hub, LinkState, MediaChoice, PhoneInfo, Sent, Snapshot, Stats, StreamInfo,
        StreamLayout,
    },
    phone, sizing, tracking,
    usb::{self, Aim, Path},
};
use cranpose::{
    AccessUnit, DecodedFrames, DecoderCapabilities, DecoderConfig, DecoderError, DecoderOutput, VideoCodec, VideoDecoder,
};
use cranpose_xr::{Hand, StreamCounts, Vibration};
use cranvr_proto::{
    clock::{now_ns, ClockSync},
    identity::format_code,
    link::{ClientLink, ClientSide, ConnectObserver, Incoming},
    media::{FrameReceiver, Received},
    message::{
        ClientCaps, ClientDatagram, ClientMsg, FrameTiming, HostDatagram, HostMsg, LossReport, Ping, Pong, RejectReason,
        StreamParams, Views, DESKTOP_STREAM, UPDATE_HEADSET, UPDATE_MAC,
    },
    media::LostUnit,
    wire::{Codec, Error, Nanos},
};
use std::{
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

/// The time that each step of the handshake may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the client reports its loss. A unit may wait this long for its packets.
const REPORT_EVERY: Duration = Duration::from_millis(100);
const UNIT_TIMEOUT: Duration = Duration::from_millis(100);
/// Pings at the start of a session, until the clock estimate has a few exchanges, and after that.
/// The host ends a session after five seconds without a packet.
const FAST_PING: Duration = Duration::from_millis(50);
const SLOW_PING: Duration = Duration::from_millis(200);
const FAST_PINGS: u32 = 20;
/// The least time between two key frame requests
const KEY_REQUEST_GAP: Duration = Duration::from_millis(250);
/// How often the stats thread publishes, and how often it writes a line to the log
const STATS_EVERY: Duration = Duration::from_millis(500);
const LOG_EVERY: Duration = Duration::from_secs(2);
/// The hand and the vibration that a pulse of the host asks for
fn pulse_vibration(pulse: &cranvr_proto::message::Haptics) -> (Hand, Vibration) {
    let hand = if pulse.hand == 0 { Hand::Left } else { Hand::Right };
    let duration = Duration::from_nanos(pulse.duration_ns).max(MIN_PULSE);
    (hand, Vibration::new(pulse.amplitude, duration).with_frequency(pulse.frequency))
}

/// The shortest vibration that the client plays. A host that asks for none means a click.
const MIN_PULSE: Duration = Duration::from_millis(10);
/// The pictures that the decoder keeps for the headset. The streamed scene queues three decoded
/// frames. The frame shown last holds an image too. The decoder needs one to write. The
/// scene's doc says at least 5. Cranpose's demo uses 6.
const DECODER_DEPTH: usize = 6;
/// The highest refresh rate that the client sizes the eyes for. The host's default wish is 120 Hz.
const SIZING_RATE: f32 = 120.0;

/// One eye's image size that a flat window asks for, in pixels. It is the Quest 3's recommended
/// size. A headset reports its own.
const EYE_SIZE: [u32; 2] = [1680, 1760];
/// The refresh rates of a Quest 3, for a flat window. A headset reports its own. The host picks
/// the highest rate in this list that does not exceed its own wish.
const REFRESH_RATES: [f32; 4] = [72.0, 80.0, 90.0, 120.0];
/// The name that the host sees when the runtime gives none. The host trusts clients by this name.
const DEFAULT_NAME: &str = "cranvr-client";
/// The name of a phone or a tablet in the host's log
const PHONE_NAME: &str = "cranvr phone";
/// How long the Hello waits for the headset's first views. The headset reports them a moment
/// after the app starts.
const VIEWS_WAIT: Duration = Duration::from_secs(5);

/// The limits of the hardware decoder for HEVC, asked once. `None` where the platform gives none.
fn decoder_limits() -> Option<&'static DecoderCapabilities> {
    static LIMITS: OnceLock<Option<DecoderCapabilities>> = OnceLock::new();
    LIMITS
        .get_or_init(|| match VideoDecoder::capabilities(VideoCodec::Hevc) {
            Ok(limits) => {
                log::info!(
                    "decoder {}: hardware {:?}, at most {:?} by {:?} pixels, {:?} bit/s",
                    limits.name,
                    limits.hardware_accelerated,
                    limits.max_width,
                    limits.max_height,
                    limits.max_bitrate
                );
                Some(limits)
            }
            Err(e) => {
                log::info!("no decoder limits: {e}");
                None
            }
        })
        .as_ref()
}

/// The size of one eye's picture to ask the host for. It is the recommended size, or a smaller one
/// that the decoder takes at the refresh rate of the stream. With `foveation` the decoder's frame
/// has the layout that the host's foveation gives. Without it the frame holds both eyes whole.
fn eye_for(recommended: [u32; 2], refresh_rates: &[f32], foveation: bool) -> [u32; 2] {
    let rate = refresh_rates.iter().copied().filter(|rate| *rate <= SIZING_RATE).fold(0.0, f32::max).max(1.0);
    let eye = match decoder_limits() {
        Some(limits) => sizing::choose_eye(recommended, foveation, |frame| sizing::decoder_takes(limits, frame, rate)),
        None => recommended,
    };
    if eye != recommended {
        log::info!("the decoder does not take the recommended eye of {recommended:?} at {rate} Hz: asking for {eye:?}");
    }
    eye
}

/// What the client tells the host about itself. A headset reports its name, the size of the
/// pictures that its displays take and its refresh rates. A phone reports its screen. The picture
/// has the screen's shape. The rate is the screen's. The phone takes no foveation, because the flat
/// picture cannot unwarp it.
fn device_caps(tcp_only: bool, headset: Option<&HeadsetInfo>, phone: Option<PhoneInfo>, views: Views) -> ClientCaps {
    if let Some(phone) = phone {
        let refresh_rates = vec![phone.refresh_hz];
        return ClientCaps {
            name: PHONE_NAME.into(),
            eye_size: eye_for(phone::eye_size(phone.screen), &refresh_rates, false),
            views,
            refresh_rates,
            codecs: vec![Codec::Hevc],
            decoders: 1,
            foveation: false,
            tcp_only,
        };
    }
    let (name, recommended, refresh_rates) = match headset {
        Some(HeadsetInfo { properties, .. }) => {
            let size = properties.eye_image_sizes[0].recommended;
            (
                Some(properties.system_name.trim().to_owned()).filter(|name| !name.is_empty()),
                if size[0] > 0 && size[1] > 0 { size } else { EYE_SIZE },
                Some(properties.refresh_rates.clone()).filter(|rates| !rates.is_empty()),
            )
        }
        None => (None, EYE_SIZE, None),
    };
    let refresh_rates = refresh_rates.unwrap_or_else(|| REFRESH_RATES.to_vec());
    ClientCaps {
        name: name.unwrap_or_else(|| DEFAULT_NAME.into()),
        eye_size: eye_for(recommended, &refresh_rates, true),
        views,
        refresh_rates,
        codecs: vec![Codec::Hevc],
        decoders: 1,
        // The headset's streamed scene unwarps the foveated encoding in its shader
        foveation: true,
        tcp_only,
    }
}

/// A running connection. `stop` ends it.
pub struct Connection {
    stop: Arc<AtomicBool>,
    link: Arc<Mutex<Option<Arc<ClientLink>>>>,
}

impl Connection {
    /// Connects in the background. The hub gets the progress under `id`. `fingerprint` is the start
    /// of the host's key fingerprint when the host came from the search.
    pub fn start(hub: &'static Hub, id: u64, host: String, fingerprint: Option<[u8; 8]>, media: MediaChoice) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let link = Arc::new(Mutex::new(None));
        let (thread_stop, thread_link) = (Arc::clone(&stop), Arc::clone(&link));
        let spawned = thread::Builder::new()
            .name("cranvr-link".into())
            .spawn(move || run(hub, id, &host, fingerprint, media, &thread_stop, &thread_link));
        if let Err(e) = spawned {
            hub.report(id, |snapshot| snapshot.state = LinkState::Failed(format!("no thread for the link: {e}")));
        }
        Self { stop, link }
    }

    /// Sends a console request to the host. It goes nowhere while the handshake runs.
    pub fn send_console(&self, request: String) {
        if let Some(link) = self.link.lock().unwrap().as_ref() {
            link.duplex.send_control(&ClientMsg::Console(request)).ok();
        }
    }

    /// Tells the host goodbye and ends the threads. A handshake that still runs ends within its
    /// timeout.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(link) = self.link.lock().unwrap().as_ref() {
            link.duplex.send_control(&ClientMsg::Bye).ok();
            link.duplex.close();
        }
    }
}

/// Says in words why a connection ended, and what the user does next.
fn failure_text(error: &Error, host: &str) -> String {
    match error {
        Error::Version { .. } => {
            if error.peer_must_update() == Some(true) {
                format!("The cranvr on {host} is older than this app. {UPDATE_MAC}")
            } else {
                format!("The cranvr on {host} is newer than this app. {UPDATE_HEADSET}")
            }
        }
        Error::Rejected(RejectReason::Denied) => "The Mac denied this headset. Press the Mac to ask again.".into(),
        Error::Rejected(RejectReason::NoAnswer) => "Nobody answered on the Mac. Press the Mac to ask again.".into(),
        Error::Rejected(RejectReason::Busy) => "The Mac is busy with another headset.".into(),
        Error::HostKeyChanged => format!(
            "{host} does not have the key that this headset paired with. Someone may pose as the Mac. If you reinstalled cranvr on the Mac, press Forget host key and connect again."
        ),
        Error::Timeout => format!("{host} did not answer. Is cranvr open on the Mac?"),
        Error::Closed => format!("{host} closed the connection"),
        Error::Io(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            format!("{host} refused the connection. Is cranvr open on the Mac?")
        }
        other => other.to_string(),
    }
}

/// Shows the approval code in the panel while the Mac's user decides.
struct Waiting<'a> {
    hub: &'static Hub,
    id: u64,
    stop: &'a AtomicBool,
}

impl ConnectObserver for Waiting<'_> {
    fn waiting(&mut self, code: u32, host_name: &str) {
        let (code, host) = (format_code(code), host_name.to_owned());
        log::info!("the Mac {host:?} asks its user to allow this headset: the code is {code}");
        self.hub.report(self.id, |snapshot| snapshot.state = LinkState::Waiting { code, host });
    }

    fn cancelled(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

/// The port that cranvr-host listens on for the native protocol unless told otherwise.
const DEFAULT_PORT: u16 = 9950;

/// What the user typed with a port: the default port when there is none.
fn with_port(text: &str) -> String {
    let text = text.trim();
    if text.parse::<SocketAddr>().is_ok() {
        text.to_string()
    } else if let Ok(ip) = text.parse::<IpAddr>() {
        SocketAddr::new(ip, DEFAULT_PORT).to_string()
    } else if text.contains(':') {
        // A name with a port
        text.to_string()
    } else {
        format!("{text}:{DEFAULT_PORT}")
    }
}

/// The name under which the app remembers the key of the host that the user typed. Two ways to
/// write one address give one name.
pub fn host_name(text: &str) -> String {
    with_port(text).to_lowercase()
}

/// Turns what the user typed into an address. The port is optional. A name resolves through the
/// system, which may take a moment.
fn resolve(text: &str) -> Result<SocketAddr, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Enter the address of the Mac".into());
    }
    with_port(text)
        .to_socket_addrs()
        .map_err(|e| format!("Cannot resolve {text}: {e}"))?
        .next()
        .ok_or_else(|| format!("{text} has no address"))
}

/// Where a connection to `text` goes and which path that is. A phone or a headset asks the cable
/// now: the panel's list may be old, and a link that ended connects again through here. A flat
/// window goes where the text says, because a host beside it is no cable.
fn route(hub: &'static Hub, text: &str, fingerprint: Option<[u8; 8]>) -> (String, Option<Path>) {
    if !hub.device().is_live() {
        return (text.to_string(), None);
    }
    let loopback = usb::is_loopback(text);
    let cable = usb::probe();
    // A Mac that this headset paired with is known by its address. Another Mac behind a typed
    // address shows itself to a question, which is asked only when a cable could take its place.
    let mut mac = fingerprint.or_else(|| {
        let key = crate::settings::host_key(&host_name(text)).filter(|_| !loopback)?;
        Some(cranvr_proto::identity::fingerprint(&key))
    });
    if mac.is_none() && !loopback && cable.is_some() {
        mac = resolve(text).ok().and_then(usb::identify);
    }
    let mut found = hub.found();
    if cable.is_none() && loopback && mac.is_some_and(|mac| !found.iter().any(|host| host.fingerprint == mac)) {
        // The cable is out and the search has not listed the Mac yet: ask the network now
        found = crate::discovery::search_now(Duration::from_millis(1500));
    }
    let target = usb::target(&Aim { address: text.to_string(), fingerprint: mac }, cable.as_ref(), &found);
    if target.address != text.trim() {
        log::info!("{text} connects as {} over {:?}", target.address, target.path);
    }
    (target.address, Some(target.path))
}

fn run(
    hub: &'static Hub,
    id: u64,
    text: &str,
    fingerprint: Option<[u8; 8]>,
    media: MediaChoice,
    stop: &AtomicBool,
    slot: &Mutex<Option<Arc<ClientLink>>>,
) {
    // The cable or Wi-Fi: chosen now, so a link that ended and connects again chooses anew
    let (text, path) = route(hub, text, fingerprint);
    let text = text.as_str();
    hub.report(id, |snapshot| snapshot.path = path);
    let host = match resolve(text) {
        Ok(host) => host,
        Err(message) => {
            hub.report(id, |snapshot| snapshot.state = LinkState::Failed(message));
            return;
        }
    };
    // How the host is called in the panel's words
    let by_cable = path == Some(Path::Usb);
    let shown = if by_cable { "The Mac on the USB cable".to_string() } else { host.to_string() };
    let tcp_media = match media {
        MediaChoice::Auto => host.ip().is_loopback(),
        MediaChoice::Udp => false,
        MediaChoice::Tcp => true,
    };
    let headset = hub.headset();
    let device = hub.device();
    let views = match device {
        // The Hello carries the eye views, which the headset reports once it tracks the head
        Device::Headset => hub.feed.wait_views(VIEWS_WAIT).unwrap_or_else(|| {
            log::warn!("the headset reported no eye views within {VIEWS_WAIT:?}; the Hello carries placeholders");
            tracking::placeholder_views()
        }),
        // A phone's frames give them once the screen's size and rate are known
        Device::Phone => hub.feed.wait_views(VIEWS_WAIT).unwrap_or_else(|| {
            log::warn!("the phone's screen was not measured within {VIEWS_WAIT:?}; the Hello carries placeholders");
            phone::default_views()
        }),
        Device::Window => tracking::placeholder_views(),
    };
    // A phone that did not measure its screen in time still connects, at an ordinary rate
    let phone_info = (device == Device::Phone).then(|| hub.phone().unwrap_or(PhoneInfo { screen: [1920, 1080], refresh_hz: 60.0 }));
    let identity = crate::settings::identity();
    let caps = device_caps(tcp_media, headset.as_ref(), phone_info, views);
    log::info!(
        "Hello: {} with {}x{} pixels an eye, refresh rates {:?}, fov {:?}, eyes at {:?}",
        caps.name,
        caps.eye_size[0],
        caps.eye_size[1],
        caps.refresh_rates,
        caps.views.fov,
        caps.views.eye_pose.map(|pose| pose.position),
    );
    hub.feed.set_host_views(views);
    // A host on this device or this Mac (USB through `adb reverse`, or a flat window beside the
    // host) is reached through no network. Its key is not checked, as the host does not check the
    // pairing of a client on loopback. Any other host must be the one that this headset paired with.
    let name = host_name(text);
    // A Mac from the search may have a new address since the last pairing: the fingerprint finds its key.
    // The fingerprint comes first: an address that moved to another paired Mac must not look like a fake
    let pinned = if host.ip().is_loopback() {
        None
    } else {
        fingerprint.and_then(|fp| crate::settings::host_key_by_fingerprint(&fp)).or_else(|| crate::settings::host_key(&name))
    };
    let link = match ClientLink::connect_with(host, &caps, identity, pinned, CONNECT_TIMEOUT, &mut Waiting { hub, id, stop }) {
        Ok(link) => Arc::new(link),
        Err(e) => {
            let message = failure_text(&e, &shown);
            log::warn!("{message}");
            // These end only when the user acts: the Mac's user answers or the headset forgets the
            // key or an app updates. A new try every few seconds would ask the Mac's user again.
            if matches!(e, Error::Rejected(RejectReason::Denied | RejectReason::NoAnswer) | Error::HostKeyChanged | Error::Version { .. }) {
                hub.give_up(id);
            }
            hub.report(id, |snapshot| snapshot.state = LinkState::Failed(message));
            return;
        }
    };
    if pinned.is_none() && !host.ip().is_loopback() {
        // The host accepted this headset, so this is the host that the user meant to pair with
        log::info!("paired with the host key {} at {host}", cranvr_proto::identity::to_hex(&link.host_key[..8]));
        crate::settings::remember_host_key(&name, &link.host_key);
    }
    hub.learn_mac(id, cranvr_proto::identity::fingerprint(&link.host_key));
    *slot.lock().unwrap() = Some(Arc::clone(&link));
    if stop.load(Ordering::Relaxed) {
        // The user ended the attempt while the handshake ran
        link.duplex.send_control(&ClientMsg::Bye).ok();
        link.duplex.close();
        return;
    }
    let reason = session(hub, id, &link, device, views, stop);
    // The threads of the session stop on this flag
    stop.store(true, Ordering::Relaxed);
    link.duplex.close();
    hub.report(id, |snapshot| {
        if !matches!(snapshot.state, LinkState::Failed(_)) {
            snapshot.state = LinkState::Failed(format!("the link to {} ended: {reason}", if by_cable { "the Mac on the USB cable".to_string() } else { host.to_string() }));
        }
    });
}

#[derive(Default)]
struct Counters {
    packets: AtomicU64,
    malformed: AtomicU64,
    /// Frames that arrived whole
    frames: AtomicU64,
    bytes: AtomicU64,
    /// Frames that count as decoded when no decoder runs
    decoded: AtomicU64,
    /// Frames that the decoder refused
    refused: AtomicU64,
    key_requests: AtomicU64,
    haptics: AtomicU64,
}

#[derive(Default)]
struct Totals {
    packets_expected: u64,
    packets_received: u64,
    units_complete: u64,
    units_recovered: u64,
    units_lost: u64,
    packets_recovered: u64,
}

/// Sums of the latencies since the stats thread last looked.
#[derive(Default)]
struct Averages {
    network_ms: f64,
    network_n: u32,
    pose_to_decode_ms: f64,
    pose_to_decode_n: u32,
    /// The tracking sample to the display time of the headset frame that shows the frame, summed
    /// over the same frames
    pose_to_display_ms: f64,
    /// Of the frames with a target, those that came after the headset frame of their point
    with_target: u32,
    late: u32,
}

struct Shared<'a> {
    hub: &'static Hub,
    id: u64,
    link: &'a ClientLink,
    stop: &'a AtomicBool,
    stream: Mutex<StreamParams>,
    /// The parameter sets of the decoder
    config: Mutex<Vec<u8>>,
    /// Counts each change of the stream or its parameter sets, so the decoder starts again
    epoch: AtomicU64,
    period_ns: AtomicU64,
    receiver: Mutex<FrameReceiver>,
    clock: Mutex<ClockSync>,
    /// Frames wait for a key frame at the start and after a lost frame
    waiting_key: AtomicBool,
    last_key_request: Mutex<Instant>,
    counters: Counters,
    totals: Mutex<Totals>,
    averages: Mutex<Averages>,
    view: Mutex<DecoderView>,
    decoder_frames: Mutex<Option<DecodedFrames>>,
    /// What the app runs on. A headset and a phone decode. A window does not.
    device: Device,
    /// The orientation of the left eye in the newest frame that arrived. It is the pose for which
    /// the host rendered the frame.
    rendered: Mutex<Option<[f32; 4]>>,
}

impl Shared<'_> {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn request_key_frame(&self, frame: u32) {
        let mut last = self.last_key_request.lock().unwrap();
        if last.elapsed() < KEY_REQUEST_GAP {
            return;
        }
        *last = Instant::now();
        self.counters.key_requests.fetch_add(1, Ordering::Relaxed);
        self.link.duplex.send_control(&ClientMsg::RequestKeyFrame { stream: 0, frame }).ok();
    }

    fn set_stream(&self, stream: &StreamParams) {
        *self.stream.lock().unwrap() = stream.clone();
        self.period_ns.store((1e9 / f64::from(stream.refresh_hz.max(1.0))) as u64, Ordering::Relaxed);
    }
}

/// Runs a session until the link ends and returns the reason.
fn session(hub: &'static Hub, id: u64, link: &ClientLink, device: Device, hello_views: Views, stop: &AtomicBool) -> Error {
    let stream = link.welcome.stream.clone();
    let layout = StreamLayout::new(stream.eye_size, stream.foveation);
    let picture = layout.frame();
    log::info!(
        "streaming {}x{} per eye at {} Hz, {:?}, {} media, {}decoded frames of {}x{}",
        stream.eye_size[0],
        stream.eye_size[1],
        stream.refresh_hz,
        stream.codec,
        if link.duplex.has_udp() { "UDP" } else { "TCP" },
        if stream.foveation.is_some() { "foveated, " } else { "" },
        picture[0],
        picture[1]
    );
    // The decoder's limits were checked for the default foveation before the Hello. The host's
    // own parameters and its wish give the frame now.
    if let Some(limits) = decoder_limits() {
        if !sizing::decoder_takes(limits, picture, stream.refresh_hz) {
            log::warn!(
                "the decoder may not take frames of {}x{} at {} Hz: it reports at most {:?} frames a second",
                picture[0],
                picture[1],
                stream.refresh_hz,
                limits.max_frame_rate(picture[0], picture[1])
            );
        }
    }
    hub.report(id, |snapshot| {
        snapshot.state = LinkState::Streaming;
        snapshot.stream = Some(StreamInfo {
            eye: stream.eye_size,
            picture,
            foveated: stream.foveation.is_some(),
            codec: format!("{:?}", stream.codec),
            refresh_hz: stream.refresh_hz,
            target_mbps: stream.bitrate_bps as f32 / 1e6,
            udp: link.duplex.has_udp(),
        });
    });
    let refresh_hz = stream.refresh_hz;
    // The host asks again for what its games use. A session starts with no wish.
    hub.feed.set_wish(cranvr_proto::tracking::Features::NONE);
    let shared = Shared {
        hub,
        id,
        link,
        stop,
        stream: Mutex::new(stream.clone()),
        config: Mutex::new(Vec::new()),
        epoch: AtomicU64::new(0),
        period_ns: AtomicU64::new(0),
        receiver: Mutex::new(FrameReceiver::new()),
        clock: Mutex::new(ClockSync::new()),
        waiting_key: AtomicBool::new(true),
        last_key_request: Mutex::new(Instant::now() - KEY_REQUEST_GAP),
        counters: Counters::default(),
        totals: Mutex::new(Totals::default()),
        averages: Mutex::new(Averages::default()),
        view: Mutex::new(DecoderView::Idle),
        decoder_frames: Mutex::new(None),
        device,
        rendered: Mutex::new(None),
    };
    shared.set_stream(&stream);
    let shared = &shared;
    let (units_tx, units_rx) = mpsc::channel::<Received>();
    // The desktop panel and the sound have their own state (desktop.rs, audio.rs)
    // Only a headset shows the desktop panel
    let desktop_units = crate::desktop::desktop().session_start(device == Device::Headset);
    // Only a headset or a phone plays the sound. A flat window on the Mac that runs the host would
    // play what the host captures. The host would capture it again: an echo that grows.
    if let (Some(format), true) = (stream.audio, device.is_live()) {
        crate::audio::open(format);
    }
    // A change of the lifecycle wakes the sender, which tells the host about the focus. The sender
    // would notice it on its own turn, at most 20 ms later. A headset that goes to sleep may stop the
    // process right after it, so the host should hear of the change at once.
    let _focus_watch = device.is_live().then(|| cranpose::observe_lifecycle(move |_| hub.feed.wake()));
    let reason = thread::scope(|scope| {
        let named = |name: &str| thread::Builder::new().name(name.into());
        if link.duplex.has_udp() {
            let tx = units_tx.clone();
            named("cranvr-udp").spawn_scoped(scope, move || drop(read_loop(shared, tx, true))).expect("the UDP thread");
        }
        named("cranvr-upkeep").spawn_scoped(scope, move || upkeep(shared)).expect("the upkeep thread");
        named("cranvr-tracking")
            .spawn_scoped(scope, move || tracking::send_loop(link, &hub.feed, device.is_live(), refresh_hz, hello_views, &crate::focus::current, stop))
            .expect("the tracking thread");
        named("cranvr-decode").spawn_scoped(scope, move || decode_loop(shared, &units_rx)).expect("the decoder thread");
        named("cranvr-stats").spawn_scoped(scope, move || stats_loop(shared)).expect("the stats thread");
        named("cranvr-desktop-decode")
            .spawn_scoped(scope, move || crate::desktop::decode_loop(&desktop_units, stop))
            .expect("the desktop decoder thread");
        named("cranvr-desktop-uplink").spawn_scoped(scope, move || crate::desktop::uplink(link, stop)).expect("the desktop uplink thread");
        let reason = read_loop(shared, units_tx, false);
        shared.stop.store(true, Ordering::Relaxed);
        link.duplex.close();
        reason
    });
    crate::desktop::desktop().session_end();
    crate::audio::close();
    hub.feed.set_wish(cranvr_proto::tracking::Features::NONE);
    reason
}

/// Reads the host's packets until the connection ends. UDP and TCP each have a loop.
fn read_loop(s: &Shared<'_>, units: mpsc::Sender<Received>, udp: bool) -> Error {
    let handler = |incoming: Incoming<'_, ClientSide>| match incoming {
        Incoming::Media(packet) => on_media(s, packet, &units),
        Incoming::Datagram(HostDatagram::Ping(p)) => {
            let now = now_ns();
            let pong = Pong { id: p.id, ping_sent: p.sent, received: now, sent: now_ns() };
            s.link.duplex.send_datagram(&ClientDatagram::Pong(pong)).ok();
        }
        Incoming::Datagram(HostDatagram::Pong(p)) => {
            s.clock.lock().unwrap().add(p.ping_sent, p.received, p.sent, now_ns());
        }
        Incoming::Datagram(HostDatagram::Haptics(pulse)) => {
            s.counters.haptics.fetch_add(1, Ordering::Relaxed);
            // An app without the input focus drives no controller (Meta's rule)
            if crate::focus::current().is_focused() {
                if let Some(haptics) = s.hub.headset().and_then(|headset| headset.haptics) {
                    let (hand, vibration) = pulse_vibration(&pulse);
                    haptics.vibrate(hand, vibration);
                }
            }
        }
        Incoming::Datagram(HostDatagram::Audio(audio)) => {
            // How old the sound is on arrival: the host's capture time on this clock, to now
            let age = s.clock.lock().unwrap().to_local(audio.host_time).map(|at| Duration::from_nanos(now_ns().saturating_sub(at)));
            crate::audio::on_datagram(&audio, age);
        }
        Incoming::Datagram(HostDatagram::Cursor(cursor)) => crate::desktop::desktop().on_cursor(cursor),
        Incoming::Control(message @ (HostMsg::Desktop(_) | HostMsg::DecoderConfig { stream: DESKTOP_STREAM, .. })) => {
            crate::desktop::desktop().on_message(&message);
        }
        Incoming::Control(HostMsg::DecoderConfig { config, .. }) => {
            *s.config.lock().unwrap() = config;
            s.epoch.fetch_add(1, Ordering::Relaxed);
        }
        Incoming::Control(HostMsg::StreamUpdate(stream)) => {
            s.set_stream(&stream);
            s.epoch.fetch_add(1, Ordering::Relaxed);
        }
        Incoming::Control(HostMsg::Menu(text)) => s.hub.set_menu(s.id, &text),
        Incoming::Control(HostMsg::TrackingWish(wish)) => {
            log::info!("the host asks for {}", if wish.is_empty() { "no tracking".to_string() } else { wish.names().join(", ") });
            s.hub.feed.set_wish(wish);
        }
        Incoming::Control(HostMsg::Bye) => {
            s.stop.store(true, Ordering::Relaxed);
            s.link.duplex.close();
        }
        Incoming::Control(_) => {}
    };
    if udp {
        s.link.duplex.read_udp(handler)
    } else {
        s.link.duplex.read_tcp(handler)
    }
}

fn on_media(s: &Shared<'_>, packet: &[u8], units: &mpsc::Sender<Received>) {
    s.counters.packets.fetch_add(1, Ordering::Relaxed);
    let mut receiver = s.receiver.lock().unwrap();
    match receiver.push(packet, now_ns()) {
        Ok(Some(unit)) => {
            drop(receiver);
            if unit.meta.stream == DESKTOP_STREAM {
                crate::desktop::desktop().push_unit(unit);
            } else {
                units.send(unit).ok();
            }
        }
        Ok(None) => {
            let lost = receiver.take_lost();
            drop(receiver);
            on_lost(s, &lost);
        }
        Err(_) => {
            s.counters.malformed.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Asks for a key frame of each stream that lost a unit: the eye stream and the desktop stream
/// each have their own.
fn on_lost(s: &Shared<'_>, lost: &[LostUnit]) {
    if let Some(first) = lost.iter().find(|unit| unit.stream == 0) {
        s.waiting_key.store(true, Ordering::Relaxed);
        s.request_key_frame(first.frame);
    }
    if let Some(first) = lost.iter().find(|unit| unit.stream == DESKTOP_STREAM) {
        crate::desktop::desktop().on_lost(first.frame);
    }
}

/// Pings the host and reports what the receiver lost, until the session ends.
fn upkeep(s: &Shared<'_>) {
    let mut next_ping = Instant::now();
    let mut ping_id = 0u32;
    let mut last_report = Instant::now();
    while !s.stopped() {
        if Instant::now() >= next_ping {
            s.link.duplex.send_datagram(&ClientDatagram::Ping(Ping { id: ping_id, sent: now_ns() })).ok();
            next_ping += if ping_id < FAST_PINGS { FAST_PING } else { SLOW_PING };
            ping_id += 1;
        }
        if last_report.elapsed() >= REPORT_EVERY {
            last_report = Instant::now();
            let (report, lost) = {
                let mut receiver = s.receiver.lock().unwrap();
                receiver.expire(now_ns(), UNIT_TIMEOUT.as_nanos() as u64);
                (receiver.take_report(), receiver.take_lost())
            };
            if report != LossReport::default() {
                s.link.duplex.send_datagram(&ClientDatagram::Loss(report)).ok();
                let mut totals = s.totals.lock().unwrap();
                totals.packets_expected += u64::from(report.packets_expected);
                totals.packets_received += u64::from(report.packets_received);
                totals.units_complete += u64::from(report.units_complete);
                totals.units_recovered += u64::from(report.units_recovered);
                totals.units_lost += u64::from(report.units_lost);
                totals.packets_recovered += u64::from(report.packets_recovered);
            }
            on_lost(s, &lost);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn video_codec(codec: Codec) -> VideoCodec {
    match codec {
        Codec::H264 => VideoCodec::H264,
        Codec::Hevc => VideoCodec::Hevc,
        Codec::Av1 => VideoCodec::Av1,
    }
}

/// A frame that came in, as the timing report names it.
struct Arrival {
    frame: u32,
    tracking_sample: Nanos,
    received: Nanos,
}

impl From<&Sent> for Arrival {
    fn from(sent: &Sent) -> Self {
        Self { frame: sent.frame, tracking_sample: sent.tracking_sample, received: sent.received }
    }
}

/// The decoder thread's state.
struct Decode<'s, 'a> {
    s: &'s Shared<'a>,
    decoder: Option<VideoDecoder>,
    /// The frames of the decoder, as the hub holds them for the UI
    frames: Option<DecodedFrames>,
    /// The epoch that the decoder was made for
    built_for: Option<u64>,
    /// How the pictures lie in the decoder's frames, for the stream that the decoder was made for
    layout: StreamLayout,
    last_pts_us: u64,
    last_frame: Option<u32>,
    /// Holds a key frame's parameter sets and its bytes together
    scratch: Vec<u8>,
    /// A decoder that failed to start is tried again at this time. The decoder of an earlier
    /// session may still hold the codec for a moment.
    retry_at: Option<Instant>,
}

/// How long to wait before a decoder that failed to start is tried again
const RETRY_AFTER: Duration = Duration::from_millis(500);

fn decode_loop(s: &Shared<'_>, units: &mpsc::Receiver<Received>) {
    let layout = {
        let stream = s.stream.lock().unwrap();
        StreamLayout::new(stream.eye_size, stream.foveation)
    };
    let mut d = Decode { s, decoder: None, frames: None, built_for: None, layout, last_pts_us: 0, last_frame: None, scratch: Vec::new(), retry_at: None };
    while !s.stopped() {
        d.sync_decoder();
        match units.recv_timeout(Duration::from_millis(2)) {
            Ok(unit) => d.on_unit(&unit),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        d.drain_shown();
    }
    d.release();
}

impl Decode<'_, '_> {
    /// Lets go of the decoder and takes its frames from the headset's streamed scene.
    fn release(&mut self) {
        self.decoder = None;
        *self.s.decoder_frames.lock().unwrap() = None;
        if let Some(frames) = self.frames.take() {
            self.s.hub.clear_stream(&frames);
        }
    }

    fn set_view(&self, view: DecoderView) {
        *self.s.view.lock().unwrap() = view;
    }

    /// Makes a decoder when the parameter sets are there. Makes a new one when they change. Only a
    /// headset or a phone decodes: its streamed scene draws the decoder's frames.
    fn sync_decoder(&mut self) {
        let s = self.s;
        let wanted = s.epoch.load(Ordering::Relaxed);
        if self.built_for == Some(wanted) || self.retry_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        self.retry_at = None;
        if !s.device.is_live() {
            self.built_for = Some(wanted);
            self.set_view(DecoderView::NoHeadset);
            return;
        }
        if s.config.lock().unwrap().is_empty() {
            // The host sends the parameter sets before the first frame
            return;
        }
        // The first decoder replaces none. The next one replaces the one that ran
        self.release();
        self.built_for = Some(wanted);
        let stream = s.stream.lock().unwrap().clone();
        let layout = StreamLayout::new(stream.eye_size, stream.foveation);
        let [width, height] = layout.frame();
        let config = DecoderConfig::new(video_codec(stream.codec), width, height, DecoderOutput::Frames { depth: DECODER_DEPTH });
        match VideoDecoder::new(config) {
            Ok(decoder) => {
                self.layout = layout;
                log::info!("decoder started for {width}x{height}{}", if stream.foveation.is_some() { " (foveated)" } else { "" });
                let frames = decoder.frames();
                *s.decoder_frames.lock().unwrap() = Some(frames.clone());
                self.decoder = Some(decoder);
                // The decoder starts at a key frame
                s.waiting_key.store(true, Ordering::Relaxed);
                s.request_key_frame(self.last_frame.unwrap_or(0));
                self.last_pts_us = 0;
                self.set_view(DecoderView::Running);
                s.hub.set_stream(frames.clone());
                self.frames = Some(frames);
            }
            Err(e) => {
                log::warn!("no decoder: {e}");
                self.set_view(DecoderView::Failed(e.to_string()));
                self.built_for = None;
                self.retry_at = Some(Instant::now() + RETRY_AFTER);
            }
        }
    }

    fn on_unit(&mut self, unit: &Received) {
        let s = self.s;
        s.counters.frames.fetch_add(1, Ordering::Relaxed);
        s.counters.bytes.fetch_add(unit.payload().len() as u64, Ordering::Relaxed);
        let meta = &unit.meta;
        // The host numbers its frames one by one. A gap means that a frame never came, so the
        // next one refers to a picture that the decoder does not have.
        let in_order = self.last_frame.is_none_or(|last| meta.frame == last.wrapping_add(1));
        self.last_frame = Some(meta.frame);
        if meta.key {
            s.waiting_key.store(false, Ordering::Relaxed);
        } else if !in_order {
            s.waiting_key.store(true, Ordering::Relaxed);
        }
        if !meta.key && s.waiting_key.load(Ordering::Relaxed) {
            s.request_key_frame(meta.frame);
            return;
        }
        if meta.slices.len() != 1 {
            return;
        }
        if let Some(sent) = s.clock.lock().unwrap().to_local(meta.sent) {
            let mut averages = s.averages.lock().unwrap();
            averages.network_ms += unit.completed.saturating_sub(sent) as f64 / 1e6;
            averages.network_n += 1;
        }
        *s.rendered.lock().unwrap() = Some(meta.eye_poses[0].orientation);
        if self.decoder.is_some() {
            if self.submit(unit) && s.device == Device::Phone {
                self.send_phone_timing(unit);
            }
        } else {
            // Without a decoder, as in a flat window, a frame counts as decoded when it arrives.
            // The host still gets its timing and keeps its pace.
            s.counters.decoded.fetch_add(1, Ordering::Relaxed);
            let sent = Sent {
                metadata: 0,
                views: streamed_views(&meta.eye_poses, &self.host_fov(), &self.layout),
                frame: meta.frame,
                tracking_sample: meta.tracking_sample,
                received: unit.completed,
                submitted: unit.completed,
                target_display: meta.target_display,
            };
            let display_time = s.hub.feed.display_time_after(unit.completed, s.period_ns.load(Ordering::Relaxed));
            self.send_timing(&Arrival::from(&sent), unit.completed, unit.completed, display_time, s.period_ns.load(Ordering::Relaxed));
        }
    }

    /// Tells the host when a phone could show the frame. A phone's scene shows the newest frame at
    /// the next screen frame. Nothing tells the connection when the picture came out of the
    /// decoder, so the timing is an estimate. The picture is ready the decoder's mean time after the
    /// arrival. The first screen frame that starts after that takes it. That frame reaches the
    /// display one period after it starts. The display time is therefore the first point of the
    /// screen's grid at or after the ready time plus one period. The take lies one period before it.
    fn send_phone_timing(&self, unit: &Received) {
        let s = self.s;
        let decode = s.decoder_frames.lock().unwrap().as_ref().map_or(Duration::ZERO, |frames| frames.stats().mean_decode);
        let decode_end = unit.completed + decode.as_nanos() as u64;
        let period = s.period_ns.load(Ordering::Relaxed);
        let display_time = s.hub.feed.display_time_after(decode_end + period, period);
        let picked = display_time.saturating_sub(period);
        {
            let mut averages = s.averages.lock().unwrap();
            averages.pose_to_decode_ms += decode_end.saturating_sub(unit.meta.tracking_sample) as f64 / 1e6;
            averages.pose_to_display_ms += display_time.saturating_sub(unit.meta.tracking_sample) as f64 / 1e6;
            averages.pose_to_decode_n += 1;
        }
        let arrival = Arrival { frame: unit.meta.frame, tracking_sample: unit.meta.tracking_sample, received: unit.completed };
        self.send_timing(&arrival, decode_end, picked, display_time, period);
    }

    /// The field of view that the host renders with
    fn host_fov(&self) -> [cranvr_proto::wire::Fov; 2] {
        self.s.hub.feed.host_views().unwrap_or_else(tracking::placeholder_views).fov
    }

    /// Puts the frame into the decoder. Returns whether the decoder took it.
    fn submit(&mut self, unit: &Received) -> bool {
        let s = self.s;
        let meta = &unit.meta;
        // The decoder wants the parameter sets in front of each key frame
        let mut scratch = std::mem::take(&mut self.scratch);
        let data: &[u8] = if meta.key {
            scratch.clear();
            scratch.extend_from_slice(&s.config.lock().unwrap());
            scratch.extend_from_slice(unit.payload());
            &scratch
        } else {
            unit.payload()
        };
        // The timestamp must rise from one frame to the next, so it comes from the arrival time
        // and not from the tracking sample, which may repeat.
        let pts_us = (unit.completed / 1000).max(self.last_pts_us + 1);
        self.last_pts_us = pts_us;
        let metadata = s.hub.new_metadata();
        let submitted = now_ns();
        // The headset asks for the views as soon as the decoder outputs the picture, so the table
        // gets them first
        s.hub.frame_sent(Sent {
            metadata,
            views: streamed_views(&meta.eye_poses, &self.host_fov(), &self.layout),
            frame: meta.frame,
            tracking_sample: meta.tracking_sample,
            received: unit.completed,
            submitted,
            target_display: meta.target_display,
        });
        let result = match self.decoder.as_mut() {
            Some(decoder) => decoder.submit(AccessUnit { data, timestamp: Duration::from_micros(pts_us), metadata }),
            None => Err(DecoderError::Unavailable),
        };
        self.scratch = scratch;
        if let Err(e) = result {
            // A frame that the decoder did not take breaks the chain of references
            s.counters.refused.fetch_add(1, Ordering::Relaxed);
            if !matches!(e, DecoderError::Full) {
                log::warn!("the decoder refused frame {}: {e}", meta.frame);
            }
            s.waiting_key.store(true, Ordering::Relaxed);
            s.request_key_frame(meta.frame);
            return false;
        }
        true
    }

    /// Tells the host about the frames that the headset took for its display since the last call.
    fn drain_shown(&mut self) {
        let s = self.s;
        for shown in s.hub.take_shown() {
            let sent = shown.sent;
            // The decode time is the one of the newest picture, which the headset took
            // The frame was ready before the compositor took it, so the end of the decode lies
            // before the pick
            let decode_end = (sent.submitted + shown.decode_time.as_nanos() as u64).min(shown.picked);
            {
                let mut averages = s.averages.lock().unwrap();
                averages.pose_to_decode_ms += decode_end.saturating_sub(sent.tracking_sample) as f64 / 1e6;
                averages.pose_to_display_ms += shown.display_time.saturating_sub(sent.tracking_sample) as f64 / 1e6;
                averages.pose_to_decode_n += 1;
                if sent.target_display != 0 {
                    averages.with_target += 1;
                    averages.late += u32::from(shown.late);
                }
            }
            self.send_timing(&Arrival::from(&sent), decode_end, shown.picked, shown.display_time, shown.period);
        }
    }

    /// Tells the host what the display did with a frame: when the picture came out of the decoder
    /// and when the display shows it.
    fn send_timing(&self, arrival: &Arrival, decode_end: Nanos, picked: Nanos, display_time: Nanos, period: Nanos) {
        let timing = FrameTiming {
            stream: 0,
            frame: arrival.frame,
            tracking_sample: arrival.tracking_sample,
            received: arrival.received,
            decode_end,
            picked,
            display_time,
            next_display_time: display_time + period,
        };
        self.s.link.duplex.send_datagram(&ClientDatagram::FrameTiming(timing)).ok();
    }
}

/// Publishes the numbers for the UI until the session ends.
fn stats_loop(s: &Shared<'_>) {
    let c = &s.counters;
    let mut last = Instant::now();
    let (mut frames, mut decoded, mut bytes) = (0u64, 0u64, 0u64);
    // The decoder's own count of the pictures that it output. A new decoder starts from 0.
    let (mut decoder_seen, mut decoder_total) = (0u64, 0u64);
    let (mut network_ms, mut sample_to_decode_ms, mut sample_to_display_ms) = (None, None, None);
    // The streamed scene's counts at the last look. A new scene starts from 0.
    let mut scene = StreamCounts::default();
    let mut crypto = s.link.duplex.crypto_stats();
    let mut log_due = Instant::now();
    while !s.stopped() {
        thread::sleep(Duration::from_millis(50));
        let elapsed = last.elapsed();
        if elapsed < STATS_EVERY {
            continue;
        }
        last = Instant::now();
        let seconds = elapsed.as_secs_f32();
        let decoder = s.decoder_frames.lock().unwrap().as_ref().map(DecodedFrames::stats).unwrap_or_default();
        decoder_total += if decoder.decoded >= decoder_seen { decoder.decoded - decoder_seen } else { decoder.decoded };
        decoder_seen = decoder.decoded;
        let (now_frames, now_decoded, now_bytes) =
            (c.frames.load(Ordering::Relaxed), c.decoded.load(Ordering::Relaxed) + decoder_total, c.bytes.load(Ordering::Relaxed));
        let frames_in = now_frames - frames;
        let received_fps = frames_in as f32 / seconds;
        let decoded_fps = (now_decoded - decoded) as f32 / seconds;
        let received_mbps = (now_bytes - bytes) as f32 * 8.0 / 1e6 / seconds;
        (frames, decoded, bytes) = (now_frames, now_decoded, now_bytes);
        let counts = s.hub.stream_stats.counts();
        let since = |now: u64, before: u64| if now >= before { now - before } else { now } as f32 / seconds;
        let (shown_fps, skipped_fps, repeated_fps) =
            (since(counts.shown, scene.shown), since(counts.skipped, scene.skipped), since(counts.repeated, scene.repeated));
        scene = counts;
        let late_pct = {
            let mut averages = s.averages.lock().unwrap();
            let late_pct = 100.0 * averages.late as f32 / averages.with_target.max(1) as f32;
            if averages.network_n > 0 {
                network_ms = Some((averages.network_ms / f64::from(averages.network_n)) as f32);
            }
            if averages.pose_to_decode_n > 0 {
                sample_to_decode_ms = Some((averages.pose_to_decode_ms / f64::from(averages.pose_to_decode_n)) as f32);
                sample_to_display_ms = Some((averages.pose_to_display_ms / f64::from(averages.pose_to_decode_n)) as f32);
            }
            *averages = Averages::default();
            late_pct
        };
        let ms = |d: Duration| d.as_secs_f32() * 1000.0;
        let opened = s.link.duplex.crypto_stats();
        let (open_packets, open_ns) = (opened.opened - crypto.opened, opened.open_ns - crypto.open_ns);
        let open_us_per_frame = if frames_in > 0 { open_ns as f32 / 1e3 / frames_in as f32 } else { 0.0 };
        let open_us_per_packet = if open_packets > 0 { open_ns as f32 / 1e3 / open_packets as f32 } else { 0.0 };
        let open_core_pct = open_ns as f32 / 1e9 / seconds * 100.0;
        crypto = opened;
        let (headset_fps, headset_gap) = s.hub.feed.frame_rate().unwrap_or_default();
        let totals = s.totals.lock().unwrap();
        let stats = Stats {
            received_fps,
            decoded_fps,
            received_mbps,
            decoder: s.view.lock().unwrap().clone(),
            decode_last_ms: ms(decoder.last_decode),
            decode_mean_ms: ms(decoder.mean_decode),
            decode_max_ms: ms(decoder.max_decode),
            decoder_dropped: decoder.dropped + c.refused.load(Ordering::Relaxed),
            packets_expected: totals.packets_expected,
            packets_received: totals.packets_received,
            units_complete: totals.units_complete,
            units_recovered: totals.units_recovered,
            units_lost: totals.units_lost,
            packets_recovered: totals.packets_recovered,
            key_requests: c.key_requests.load(Ordering::Relaxed),
            rtt_ms: s.clock.lock().unwrap().estimate().map(|e| e.rtt_ns as f32 / 1e6),
            network_ms,
            sample_to_decode_ms,
            sample_to_display_ms,
            haptic_pulses: c.haptics.load(Ordering::Relaxed),
            shown_fps,
            skipped_fps,
            repeated_fps,
            headset_fps,
            headset_gap_ms: ms(headset_gap),
            head_position: s.hub.feed.head().map(|head| head.pose.position),
            head_orientation: s.hub.feed.head().map(|head| head.pose.orientation),
            rendered_orientation: *s.rendered.lock().unwrap(),
            late_pct,
            tracking_live: s.hub.feed.is_live(),
            open_us_per_frame,
            open_us_per_packet,
            open_core_pct,
            packets_rejected: opened.rejected,
        };
        drop(totals);
        if log_due.elapsed() >= LOG_EVERY {
            log_due = Instant::now();
            if s.device == Device::Phone {
                // The log line shows the orientation that the phone sent. It shows the orientation
                // for which the host rendered the newest frame too. The two agree when the host
                // takes the tracking. The time between the two moments makes a small difference.
                let looks = |orientation: Option<[f32; 4]>| orientation.map(|q| phone::forward(q).map(|v| (v * 100.0).round() / 100.0));
                log::info!(
                    "phone head: sent {:?} looking {:?}; the newest frame was rendered for {:?} looking {:?}",
                    stats.head_orientation.map(|q| q.map(|v| (v * 1000.0).round() / 1000.0)),
                    looks(stats.head_orientation),
                    stats.rendered_orientation.map(|q| q.map(|v| (v * 1000.0).round() / 1000.0)),
                    looks(stats.rendered_orientation),
                );
            }
            log::info!(
                "{:.1} fps received, {:.1} fps decoded, {:.1} fps shown, {:.1} skipped, {:.1} repeated of {:.1} headset frames (gap up to {:.1} ms), {:.0}% late at the take, {} dropped by the decoder, {:.1} Mbps, decode {:.2} ms mean, rtt {} ms, \
                 network {} ms, pose to decode {} ms, pose to display {} ms, tracking {} at {:?}, {} of {} packets lost, {} units recovered, {} units lost, \
                 {} pulses, opening {:.0} us a frame ({:.2} us a packet, {:.1}% of a core), {} packets refused",
                stats.received_fps,
                stats.decoded_fps,
                stats.shown_fps,
                stats.skipped_fps,
                stats.repeated_fps,
                stats.headset_fps,
                stats.headset_gap_ms,
                stats.late_pct,
                stats.decoder_dropped,
                stats.received_mbps,
                stats.decode_mean_ms,
                stats.rtt_ms.map_or("-".into(), |v| format!("{v:.2}")),
                stats.network_ms.map_or("-".into(), |v| format!("{v:.2}")),
                stats.sample_to_decode_ms.map_or("-".into(), |v| format!("{v:.1}")),
                stats.sample_to_display_ms.map_or("-".into(), |v| format!("{v:.1}")),
                if stats.tracking_live { "live" } else { "placeholder" },
                stats.head_position.map(|p| p.map(|v| (v * 1000.0).round() / 1000.0)),
                stats.packets_expected.saturating_sub(stats.packets_received),
                stats.packets_expected,
                stats.units_recovered,
                stats.units_lost,
                stats.haptic_pulses,
                stats.open_us_per_frame,
                stats.open_us_per_packet,
                stats.open_core_pct,
                stats.packets_rejected,
            );
        }
        s.hub.report(s.id, |snapshot: &mut Snapshot| snapshot.stats = stats);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranvr_proto::message::Haptics as Pulse;

    #[test]
    fn a_pulse_becomes_a_vibration_of_the_same_hand() {
        let (hand, vibration) = pulse_vibration(&Pulse { hand: 1, duration_ns: 40_000_000, frequency: 160.0, amplitude: 0.5 });
        assert_eq!(hand, Hand::Right);
        assert_eq!((vibration.amplitude(), vibration.frequency(), vibration.duration()), (0.5, Some(160.0), Duration::from_millis(40)));
        let (hand, vibration) = pulse_vibration(&Pulse { hand: 0, duration_ns: 0, frequency: 0.0, amplitude: 3.0 });
        // A pulse with no length is a click, a frequency of 0 leaves the choice to the controller,
        // and a strength beyond 1 is the strongest
        assert_eq!((hand, vibration.duration(), vibration.frequency(), vibration.amplitude()), (Hand::Left, MIN_PULSE, None, 1.0));
    }

    #[test]
    fn a_failure_says_which_side_to_update() {
        let host = SocketAddr::from(([192, 168, 1, 20], 9950)).to_string();
        // A Mac with a lower version
        let old_mac = failure_text(&Error::Version { ours: 8, theirs: 7 }, &host);
        assert!(old_mac.ends_with("Update cranvr on the Mac."), "{old_mac}");
        // A newer Mac
        let new_mac = failure_text(&Error::Version { ours: 8, theirs: 9 }, &host);
        assert!(new_mac.ends_with("Update the cranvr app on the headset."), "{new_mac}");
    }

    #[test]
    fn a_refusal_tells_the_user_what_to_press() {
        let host = SocketAddr::from(([192, 168, 1, 20], 9950)).to_string();
        assert_eq!(failure_text(&Error::Rejected(RejectReason::Denied), &host), "The Mac denied this headset. Press the Mac to ask again.");
        assert_eq!(failure_text(&Error::Rejected(RejectReason::NoAnswer), &host), "Nobody answered on the Mac. Press the Mac to ask again.");
        assert!(failure_text(&Error::Timeout, &host).contains("Is cranvr open on the Mac?"));
        assert!(failure_text(&Error::HostKeyChanged, &host).contains("Forget host key"));
    }

    /// The whole way of a new headset to a Mac, against a real host process on this machine: the
    /// search finds the host, a press connects, the headset waits with a code, the host's user
    /// allows it and frames arrive. The window is not a headset, so nothing decodes, but the frames
    /// reach the client whole. It needs a built host (`CRANVR_HOST_BIN`, else `target/debug`) and an
    /// address of this Mac on a network, because the host asks about no client on loopback.
    ///
    /// `cargo test --manifest-path client/Cargo.toml -- --ignored finds_a_host`
    #[test]
    #[ignore = "starts a host process on port 9962"]
    fn a_new_headset_finds_a_host_process_waits_for_the_user_and_gets_frames() {
        use crate::hub::hub;
        use cranvr_proto::discovery::{broadcast_targets, find, DEFAULT_PORT};
        use std::io::{BufRead, BufReader, Write};
        use std::process::{Command, Stdio};

        const PORT: u16 = 9962;
        cranpose::set_platform_preferences(Arc::new(cranpose::MemoryPreferences::default()));
        let host_bin = std::env::var_os("CRANVR_HOST_BIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/debug/cranvr-host"));
        let dir = std::env::temp_dir().join(format!("cranvr-find-mac-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut host = Command::new(&host_bin)
            .args(["--native-port", &PORT.to_string(), "--status", "--fps", "60", "--no-game-audio", "--config-dir"])
            .arg(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("cannot start {}: {e}", host_bin.display()));
        let (lines, status) = mpsc::channel();
        let stdout = host.stdout.take().unwrap();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(json) = line.strip_prefix("@cranvr ").and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok()) {
                    lines.send(json).ok();
                }
            }
        });
        let cleanup_dir = dir.clone();
        let finish = |host: &mut std::process::Child| {
            hub().disconnect();
            host.kill().ok();
            host.wait().ok();
            std::fs::remove_dir_all(&cleanup_dir).ok();
        };

        // The search: the Mac answers on its LAN address. A query to this machine would give loopback.
        let found = (0..5)
            .find_map(|_| {
                find(broadcast_targets(DEFAULT_PORT), Duration::from_secs(1))
                    .unwrap()
                    .into_iter()
                    .find(|host| host.answer.port == PORT && !host.address.is_loopback())
            })
            .expect("a host on port 9962 answers the search from a network address of this Mac");
        assert!(!found.answer.name.is_empty());
        assert_eq!(found.answer.version, cranvr_proto::PROTOCOL_VERSION);

        // The press
        hub().connect(found.socket_addr().to_string(), Some(found.answer.fingerprint), MediaChoice::Auto);
        let wait_for = |what: &str, done: &mut dyn FnMut() -> bool| {
            for _ in 0..400 {
                if done() {
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
            panic!("{what} did not happen: {:?}", hub().snapshot().state);
        };
        let mut shown = String::new();
        wait_for("the wait for the Mac", &mut || match hub().snapshot().state {
            LinkState::Waiting { code, .. } => {
                shown = code;
                true
            }
            _ => false,
        });
        // The host lists the same code with the headset's key
        let request = loop {
            let line = status.recv_timeout(Duration::from_secs(10)).expect("a status line");
            if let Some(request) = line["pending"].as_array().and_then(|list| list.first()).cloned() {
                break request;
            }
        };
        assert_eq!(request["code"].as_str(), Some(shown.as_str()), "the host and the headset show one code");
        assert_eq!(request["key"].as_str().map(str::len), Some(64));

        // The user presses Allow
        let key = request["key"].as_str().unwrap().to_owned();
        writeln!(host.stdin.as_mut().unwrap(), "allow {key}").unwrap();
        wait_for("the stream", &mut || hub().snapshot().state == LinkState::Streaming);
        let mut best = 0.0f32;
        wait_for("frames", &mut || {
            best = best.max(hub().snapshot().stats.received_fps);
            best > 10.0
        });
        println!("frames received: {best:.1} a second");
        finish(&mut host);
    }
}
