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
    hub::{streamed_views, DecoderView, HeadsetInfo, Hub, LinkState, MediaChoice, Sent, Snapshot, Stats, StreamInfo, StreamLayout},
    sizing, tracking,
};
use cranpose::{
    AccessUnit, DecodedFrames, DecoderCapabilities, DecoderConfig, DecoderError, DecoderOutput, VideoCodec, VideoDecoder,
};
use cranpose_xr::{Hand, StreamCounts, Vibration};
use cranvr_proto::{
    clock::{now_ns, ClockSync},
    identity::{format_code, Identity},
    link::{ClientLink, ClientSide, Incoming},
    media::{FrameReceiver, Received},
    message::{
        ClientCaps, ClientDatagram, ClientMsg, FrameTiming, HostDatagram, HostMsg, LossReport, Ping, Pong, RejectReason,
        StreamParams, Views,
    },
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

/// The size of one eye's picture to ask the host for: the headset's recommended size, or a smaller
/// one that the decoder takes at the refresh rate of the stream. The decoder's frame is the one
/// that the host's foveation gives.
fn eye_for(recommended: [u32; 2], refresh_rates: &[f32]) -> [u32; 2] {
    let rate = refresh_rates.iter().copied().filter(|rate| *rate <= SIZING_RATE).fold(0.0, f32::max).max(1.0);
    let eye = match decoder_limits() {
        Some(limits) => sizing::choose_eye(recommended, true, |frame| sizing::decoder_takes(limits, frame, rate)),
        None => recommended,
    };
    if eye != recommended {
        log::info!("the decoder does not take the recommended eye of {recommended:?} at {rate} Hz: asking for {eye:?}");
    }
    eye
}

/// What the client tells the host about itself. A headset reports its name, the size of the
/// pictures that its displays take and its refresh rates.
fn device_caps(tcp_only: bool, headset: Option<&HeadsetInfo>, views: Views, identity: &Identity) -> ClientCaps {
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
        eye_size: eye_for(recommended, &refresh_rates),
        views,
        refresh_rates,
        codecs: vec![Codec::Hevc],
        decoders: 1,
        // The headset's streamed scene unwarps ALVR's foveated encoding in its shader
        foveation: true,
        tcp_only,
        public_key: identity.public_key(),
    }
}

/// A running connection. `stop` ends it.
pub struct Connection {
    stop: Arc<AtomicBool>,
    link: Arc<Mutex<Option<Arc<ClientLink>>>>,
}

impl Connection {
    /// Connects in the background. The hub gets the progress under `id`.
    pub fn start(hub: &'static Hub, id: u64, host: String, media: MediaChoice) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let link = Arc::new(Mutex::new(None));
        let (thread_stop, thread_link) = (Arc::clone(&stop), Arc::clone(&link));
        let spawned = thread::Builder::new()
            .name("cranvr-link".into())
            .spawn(move || run(hub, id, &host, media, &thread_stop, &thread_link));
        if let Err(e) = spawned {
            hub.report(id, |snapshot| snapshot.state = LinkState::Failed(format!("no thread for the link: {e}")));
        }
        Self { stop, link }
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

/// Says in words why a connection ended.
fn describe(error: &Error, host: SocketAddr) -> String {
    match error {
        Error::Timeout => format!("{host} did not answer. Does cranvr-host run with --protocol native?"),
        Error::Closed => format!("{host} closed the connection"),
        Error::Io(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            format!("{host} refused the connection. Does cranvr-host run with --protocol native?")
        }
        other => other.to_string(),
    }
}

/// The port that cranvr-host listens on for the native protocol unless told otherwise.
const DEFAULT_PORT: u16 = 9950;

/// Turns what the user typed into an address. The port is optional. A name resolves through the
/// system, which may take a moment.
fn resolve(text: &str) -> Result<SocketAddr, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Enter the address of the Mac that runs cranvr-host".into());
    }
    let with_port = if text.parse::<SocketAddr>().is_ok() {
        text.to_string()
    } else if let Ok(ip) = text.parse::<IpAddr>() {
        SocketAddr::new(ip, DEFAULT_PORT).to_string()
    } else if text.contains(':') {
        // A name with a port
        text.to_string()
    } else {
        format!("{text}:{DEFAULT_PORT}")
    };
    with_port
        .to_socket_addrs()
        .map_err(|e| format!("Cannot resolve {text}: {e}"))?
        .next()
        .ok_or_else(|| format!("{text} has no address"))
}

fn run(
    hub: &'static Hub,
    id: u64,
    text: &str,
    media: MediaChoice,
    stop: &AtomicBool,
    slot: &Mutex<Option<Arc<ClientLink>>>,
) {
    let host = match resolve(text) {
        Ok(host) => host,
        Err(message) => {
            hub.report(id, |snapshot| snapshot.state = LinkState::Failed(message));
            return;
        }
    };
    let tcp_media = match media {
        MediaChoice::Auto => host.ip().is_loopback(),
        MediaChoice::Udp => false,
        MediaChoice::Tcp => true,
    };
    let headset = hub.headset();
    let views = if headset.is_some() {
        // The Hello carries the eye views, which the headset reports once it tracks the head
        hub.feed.wait_views(VIEWS_WAIT).unwrap_or_else(|| {
            log::warn!("the headset reported no eye views within {VIEWS_WAIT:?}; the Hello carries placeholders");
            tracking::placeholder_views()
        })
    } else {
        tracking::placeholder_views()
    };
    let identity = crate::settings::identity();
    let caps = device_caps(tcp_media, headset.as_ref(), views, identity);
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
    let link = match ClientLink::connect(host, &caps, identity, CONNECT_TIMEOUT) {
        Ok(link) => Arc::new(link),
        Err(e) => {
            let message = match &e {
                Error::Rejected(RejectReason::NotPaired) => format!(
                    "{host} does not know this headset. On the Mac, start cranvr-host with --pair {}",
                    format_code(identity.code()).replace(' ', "")
                ),
                other => describe(other, host),
            };
            log::warn!("{message}");
            hub.report(id, |snapshot| snapshot.state = LinkState::Failed(message));
            return;
        }
    };
    *slot.lock().unwrap() = Some(Arc::clone(&link));
    if stop.load(Ordering::Relaxed) {
        // The user ended the attempt while the handshake ran
        link.duplex.send_control(&ClientMsg::Bye).ok();
        link.duplex.close();
        return;
    }
    let reason = session(hub, id, &link, headset.is_some(), views, stop);
    // The threads of the session stop on this flag
    stop.store(true, Ordering::Relaxed);
    link.duplex.close();
    hub.report(id, |snapshot| {
        if !matches!(snapshot.state, LinkState::Failed(_)) {
            snapshot.state = LinkState::Failed(format!("the link to {host} ended: {reason}"));
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
    /// Whether the app runs in a headset. Only a headset decodes.
    in_headset: bool,
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
fn session(hub: &'static Hub, id: u64, link: &ClientLink, in_headset: bool, hello_views: Views, stop: &AtomicBool) -> Error {
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
        in_headset,
    };
    shared.set_stream(&stream);
    let shared = &shared;
    let (units_tx, units_rx) = mpsc::channel::<Received>();
    thread::scope(|scope| {
        let named = |name: &str| thread::Builder::new().name(name.into());
        if link.duplex.has_udp() {
            let tx = units_tx.clone();
            named("cranvr-udp").spawn_scoped(scope, move || drop(read_loop(shared, tx, true))).expect("the UDP thread");
        }
        named("cranvr-upkeep").spawn_scoped(scope, move || upkeep(shared)).expect("the upkeep thread");
        named("cranvr-tracking")
            .spawn_scoped(scope, move || tracking::send_loop(link, &hub.feed, in_headset, refresh_hz, hello_views, stop))
            .expect("the tracking thread");
        named("cranvr-decode").spawn_scoped(scope, move || decode_loop(shared, &units_rx)).expect("the decoder thread");
        named("cranvr-stats").spawn_scoped(scope, move || stats_loop(shared)).expect("the stats thread");
        let reason = read_loop(shared, units_tx, false);
        shared.stop.store(true, Ordering::Relaxed);
        link.duplex.close();
        reason
    })
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
            if let Some(haptics) = s.hub.headset().and_then(|headset| headset.haptics) {
                let (hand, vibration) = pulse_vibration(&pulse);
                haptics.vibrate(hand, vibration);
            }
        }
        // The native host sends no sound yet
        Incoming::Datagram(HostDatagram::Audio(_)) => {}
        Incoming::Control(HostMsg::DecoderConfig { config, .. }) => {
            *s.config.lock().unwrap() = config;
            s.epoch.fetch_add(1, Ordering::Relaxed);
        }
        Incoming::Control(HostMsg::StreamUpdate(stream)) => {
            s.set_stream(&stream);
            s.epoch.fetch_add(1, Ordering::Relaxed);
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
            units.send(unit).ok();
        }
        Ok(None) => {
            let lost = receiver.take_lost();
            drop(receiver);
            if let Some(first) = lost.first() {
                s.waiting_key.store(true, Ordering::Relaxed);
                s.request_key_frame(first.frame);
            }
        }
        Err(_) => {
            s.counters.malformed.fetch_add(1, Ordering::Relaxed);
        }
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
            if let Some(first) = lost.first() {
                s.waiting_key.store(true, Ordering::Relaxed);
                s.request_key_frame(first.frame);
            }
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
    /// headset decodes: its streamed scene draws the decoder's frames.
    fn sync_decoder(&mut self) {
        let s = self.s;
        let wanted = s.epoch.load(Ordering::Relaxed);
        if self.built_for == Some(wanted) || self.retry_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        self.retry_at = None;
        if !s.in_headset {
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
        if self.decoder.is_some() {
            self.submit(unit);
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
            self.send_timing(&sent, unit.completed, unit.completed, display_time, s.period_ns.load(Ordering::Relaxed));
        }
    }

    /// The field of view that the host renders with
    fn host_fov(&self) -> [cranvr_proto::wire::Fov; 2] {
        self.s.hub.feed.host_views().unwrap_or_else(tracking::placeholder_views).fov
    }

    fn submit(&mut self, unit: &Received) {
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
        }
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
                averages.pose_to_decode_n += 1;
                if sent.target_display != 0 {
                    averages.with_target += 1;
                    averages.late += u32::from(shown.late);
                }
            }
            self.send_timing(&sent, decode_end, shown.picked, shown.display_time, shown.period);
        }
    }

    /// Tells the host what the display did with a frame: when the picture came out of the decoder
    /// and when the display shows it.
    fn send_timing(&self, sent: &Sent, decode_end: Nanos, picked: Nanos, display_time: Nanos, period: Nanos) {
        let timing = FrameTiming {
            stream: 0,
            frame: sent.frame,
            tracking_sample: sent.tracking_sample,
            received: sent.received,
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
    let (mut network_ms, mut sample_to_decode_ms) = (None, None);
    // The streamed scene's counts at the last look. A new scene starts from 0.
    let mut scene = StreamCounts::default();
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
        let received_fps = (now_frames - frames) as f32 / seconds;
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
            }
            *averages = Averages::default();
            late_pct
        };
        let ms = |d: Duration| d.as_secs_f32() * 1000.0;
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
            haptic_pulses: c.haptics.load(Ordering::Relaxed),
            shown_fps,
            skipped_fps,
            repeated_fps,
            headset_fps,
            headset_gap_ms: ms(headset_gap),
            head_position: s.hub.feed.head().map(|head| head.pose.position),
            late_pct,
            tracking_live: s.hub.feed.is_live(),
        };
        drop(totals);
        if log_due.elapsed() >= LOG_EVERY {
            log_due = Instant::now();
            log::info!(
                "{:.1} fps received, {:.1} fps decoded, {:.1} fps shown, {:.1} skipped, {:.1} repeated of {:.1} headset frames (gap up to {:.1} ms), {:.0}% late at the take, {} dropped by the decoder, {:.1} Mbps, decode {:.2} ms mean, rtt {} ms, \
                 network {} ms, pose to decode {} ms, tracking {} at {:?}, {} of {} packets lost, {} units recovered, {} units lost, \
                 {} pulses",
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
                if stats.tracking_live { "live" } else { "placeholder" },
                stats.head_position.map(|p| p.map(|v| (v * 1000.0).round() / 1000.0)),
                stats.packets_expected.saturating_sub(stats.packets_received),
                stats.packets_expected,
                stats.units_recovered,
                stats.units_lost,
                stats.haptic_pulses,
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
}
