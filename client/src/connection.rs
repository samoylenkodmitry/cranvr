//! The link to the host: connect, receive, decode and report.
//!
//! One thread connects. After the handshake it runs the session with these threads:
//! - the TCP read loop (the thread that connected) and, with UDP media, a UDP read loop. Both
//!   reassemble media packets, restore lost packets with the parity and hand whole frames on;
//! - the decoder thread, which feeds Cranpose's `VideoDecoder` and sends the frame timing;
//! - the tracking sender (see `tracking.rs`);
//! - the upkeep thread, which pings the host and reports the loss;
//! - the stats thread, which publishes numbers to the hub for the UI.
//!
//! No thread touches Cranpose state. The threads write into the hub, and the UI reads from it.

use crate::{
    hub::{DecoderView, Hub, LinkState, MediaChoice, Snapshot, Stats, StreamInfo},
    tracking,
};
use cranpose::{AccessUnit, DecodedFrames, DecoderConfig, DecoderError, DecoderOutput, VideoCodec, VideoDecoder};
use cranvr_proto::{
    clock::{now_ns, ClockSync},
    link::{ClientLink, ClientSide, Incoming},
    media::{FrameReceiver, Received},
    message::{
        ClientCaps, ClientDatagram, ClientMsg, FrameTiming, HostDatagram, HostMsg, LossReport, Ping, Pong, StreamParams,
    },
    wire::{Codec, Error, Nanos},
};
use std::{
    collections::VecDeque,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError},
        Arc, Mutex,
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
/// Frames that wait for the decoder's report at the same time
const MAX_PENDING: usize = 64;

/// One eye's image size that the client asks for, in pixels. It is the Quest 3's recommended size.
/// The host sends two such images side by side. Cranpose has no API that reports the headset's
/// display size, so this is a placeholder.
const EYE_SIZE: [u32; 2] = [1680, 1760];
/// The refresh rates of a Quest 3. Cranpose's backend picks the highest rate that the runtime
/// offers, and no API tells the app which. The host picks the highest rate in this list that does
/// not exceed its own wish.
const REFRESH_RATES: [f32; 4] = [72.0, 80.0, 90.0, 120.0];

/// What the client tells the host about itself.
fn device_caps(tcp_only: bool) -> ClientCaps {
    ClientCaps {
        name: "cranvr-client".into(),
        eye_size: EYE_SIZE,
        views: tracking::placeholder_views(),
        refresh_rates: REFRESH_RATES.to_vec(),
        codecs: vec![Codec::Hevc],
        decoders: 1,
        // A flat surface cannot unwarp a foveated frame, so the host sends the full picture
        foveation: false,
        tcp_only,
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
    let link = match ClientLink::connect(host, &device_caps(tcp_media), CONNECT_TIMEOUT) {
        Ok(link) => Arc::new(link),
        Err(e) => {
            hub.report(id, |snapshot| snapshot.state = LinkState::Failed(describe(&e, host)));
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
    let reason = session(hub, id, &link, stop);
    // The threads of the session stop on this flag
    stop.store(true, Ordering::Relaxed);
    link.duplex.close();
    hub.report(id, |snapshot| {
        if !matches!(snapshot.state, LinkState::Failed(_)) {
            snapshot.state = LinkState::Failed(format!("the link to {host} ended: {reason}"));
        }
    });
}

/// The frames that the decoder has taken and has not reported yet.
struct Pending {
    metadata: u64,
    frame: u32,
    tracking_sample: Nanos,
    /// The last packet of the frame arrived
    received: Nanos,
    /// The frame went into the decoder
    submitted: Nanos,
}

#[derive(Default)]
struct Counters {
    packets: AtomicU64,
    malformed: AtomicU64,
    /// Frames that arrived whole
    frames: AtomicU64,
    bytes: AtomicU64,
    /// Frames that the decoder output, or that count as decoded when no decoder runs
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
fn session(hub: &'static Hub, id: u64, link: &ClientLink, stop: &AtomicBool) -> Error {
    let stream = link.welcome.stream.clone();
    if stream.foveation.is_some() {
        let message = "the host sent a foveated stream, which this client cannot unwarp".to_string();
        hub.report(id, |snapshot| snapshot.state = LinkState::Failed(message));
        return Error::Closed;
    }
    let picture = [stream.eye_size[0] * 2, stream.eye_size[1]];
    log::info!(
        "streaming {}x{} per eye at {} Hz, {:?}, {} media",
        stream.eye_size[0],
        stream.eye_size[1],
        stream.refresh_hz,
        stream.codec,
        if link.duplex.has_udp() { "UDP" } else { "TCP" }
    );
    hub.report(id, |snapshot| {
        snapshot.state = LinkState::Streaming;
        snapshot.stream = Some(StreamInfo {
            eye: stream.eye_size,
            picture,
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
            .spawn_scoped(scope, move || tracking::send_loop(link, &hub.feed, refresh_hz, stop))
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
        // The headset's haptics need an output API of Cranpose, which is not there yet. The client
        // counts the pulses.
        Incoming::Datagram(HostDatagram::Haptics(_)) => {
            s.counters.haptics.fetch_add(1, Ordering::Relaxed);
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
    /// The surface generation and the epoch that the decoder was made for
    built_for: Option<(u64, u64)>,
    pending: VecDeque<Pending>,
    next_metadata: u64,
    last_pts_us: u64,
    last_frame: Option<u32>,
    /// Holds a key frame's parameter sets and its bytes together
    scratch: Vec<u8>,
}

fn decode_loop(s: &Shared<'_>, units: &mpsc::Receiver<Received>) {
    let mut d = Decode {
        s,
        decoder: None,
        built_for: None,
        pending: VecDeque::new(),
        next_metadata: 0,
        last_pts_us: 0,
        last_frame: None,
        scratch: Vec::new(),
    };
    while !s.stopped() {
        d.sync_decoder();
        match units.recv_timeout(Duration::from_millis(2)) {
            Ok(unit) => d.on_unit(&unit),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        d.drain_reports();
    }
    d.release();
}

impl Decode<'_, '_> {
    /// Lets go of the decoder, so the surface can end.
    fn release(&mut self) {
        let had_decoder = self.decoder.take().is_some();
        *self.s.decoder_frames.lock().unwrap() = None;
        self.pending.clear();
        if had_decoder {
            self.s.hub.set_surface_in_use(false);
        }
    }

    fn set_view(&self, view: DecoderView) {
        *self.s.view.lock().unwrap() = view;
    }

    /// Makes a decoder when the surface and the parameter sets are there, and again when either
    /// changes.
    fn sync_decoder(&mut self) {
        let s = self.s;
        let (surface_generation, surface) = s.hub.surface();
        let wanted = (surface_generation, s.epoch.load(Ordering::Relaxed));
        if self.built_for == Some(wanted) {
            return;
        }
        // The owner of a surface waits for the decoder to let go of it, so this comes first
        self.release();
        let Some(surface) = surface else {
            self.built_for = Some(wanted);
            self.set_view(DecoderView::NoSurface);
            return;
        };
        if s.config.lock().unwrap().is_empty() {
            // The host sends the parameter sets before the first frame
            return;
        }
        self.built_for = Some(wanted);
        let stream = s.stream.lock().unwrap().clone();
        let config = DecoderConfig::new(
            video_codec(stream.codec),
            stream.eye_size[0] * 2,
            stream.eye_size[1],
            DecoderOutput::Surface(surface),
        );
        match VideoDecoder::new(config) {
            Ok(decoder) => {
                log::info!("decoder started for {}x{}", stream.eye_size[0] * 2, stream.eye_size[1]);
                *s.decoder_frames.lock().unwrap() = Some(decoder.frames());
                self.decoder = Some(decoder);
                s.hub.set_surface_in_use(true);
                self.last_pts_us = 0;
                // The new decoder starts at a key frame
                s.waiting_key.store(true, Ordering::Relaxed);
                s.request_key_frame(self.last_frame.unwrap_or(0));
                self.set_view(DecoderView::Running);
            }
            Err(e) => {
                log::warn!("no decoder: {e}");
                self.set_view(DecoderView::Failed(e.to_string()));
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
            let pending = Pending {
                metadata: 0,
                frame: meta.frame,
                tracking_sample: meta.tracking_sample,
                received: unit.completed,
                submitted: unit.completed,
            };
            self.send_timing(&pending, unit.completed);
        }
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
        let metadata = self.next_metadata;
        self.next_metadata += 1;
        let submitted = now_ns();
        let result = match self.decoder.as_mut() {
            Some(decoder) => decoder.submit(AccessUnit { data, timestamp: Duration::from_micros(pts_us), metadata }),
            None => Err(DecoderError::Unavailable),
        };
        self.scratch = scratch;
        match result {
            Ok(()) => {
                if self.pending.len() >= MAX_PENDING {
                    self.pending.pop_front();
                }
                self.pending.push_back(Pending {
                    metadata,
                    frame: meta.frame,
                    tracking_sample: meta.tracking_sample,
                    received: unit.completed,
                    submitted,
                });
            }
            Err(e) => {
                // A frame that the decoder did not take breaks the chain of references
                s.counters.refused.fetch_add(1, Ordering::Relaxed);
                if !matches!(e, DecoderError::Full) {
                    log::warn!("the decoder refused frame {}: {e}", meta.frame);
                }
                s.waiting_key.store(true, Ordering::Relaxed);
                s.request_key_frame(meta.frame);
            }
        }
    }

    /// Takes the reports of the frames that the decoder has output.
    fn drain_reports(&mut self) {
        let Some(frames) = self.s.decoder_frames.lock().unwrap().clone() else { return };
        while let Some(report) = frames.next_report() {
            // The reports come in the order of submission. A frame that the decoder dropped has
            // none, so the older entries go.
            while self.pending.front().is_some_and(|p| p.metadata < report.metadata) {
                self.pending.pop_front();
            }
            let Some(pending) = self.pending.pop_front_if(|p| p.metadata == report.metadata) else { continue };
            let decode_end = pending.submitted + report.decode_time.as_nanos() as u64;
            self.s.counters.decoded.fetch_add(1, Ordering::Relaxed);
            {
                let mut averages = self.s.averages.lock().unwrap();
                averages.pose_to_decode_ms += decode_end.saturating_sub(pending.tracking_sample) as f64 / 1e6;
                averages.pose_to_decode_n += 1;
            }
            self.send_timing(&pending, decode_end);
        }
    }

    /// Tells the host what the display did with a frame. The display time comes from the feed's
    /// clock (see `tracking.rs`).
    fn send_timing(&self, pending: &Pending, decode_end: Nanos) {
        let period = self.s.period_ns.load(Ordering::Relaxed);
        let display_time = self.s.hub.feed.display_time_after(decode_end, period);
        let timing = FrameTiming {
            stream: 0,
            frame: pending.frame,
            tracking_sample: pending.tracking_sample,
            received: pending.received,
            decode_end,
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
    let (mut network_ms, mut sample_to_decode_ms) = (None, None);
    let mut log_due = Instant::now();
    while !s.stopped() {
        thread::sleep(Duration::from_millis(50));
        let elapsed = last.elapsed();
        if elapsed < STATS_EVERY {
            continue;
        }
        last = Instant::now();
        let seconds = elapsed.as_secs_f32();
        let (now_frames, now_decoded, now_bytes) =
            (c.frames.load(Ordering::Relaxed), c.decoded.load(Ordering::Relaxed), c.bytes.load(Ordering::Relaxed));
        let received_fps = (now_frames - frames) as f32 / seconds;
        let decoded_fps = (now_decoded - decoded) as f32 / seconds;
        let received_mbps = (now_bytes - bytes) as f32 * 8.0 / 1e6 / seconds;
        (frames, decoded, bytes) = (now_frames, now_decoded, now_bytes);
        {
            let mut averages = s.averages.lock().unwrap();
            if averages.network_n > 0 {
                network_ms = Some((averages.network_ms / f64::from(averages.network_n)) as f32);
            }
            if averages.pose_to_decode_n > 0 {
                sample_to_decode_ms = Some((averages.pose_to_decode_ms / f64::from(averages.pose_to_decode_n)) as f32);
            }
            *averages = Averages::default();
        }
        let decoder = s.decoder_frames.lock().unwrap().as_ref().map(DecodedFrames::stats).unwrap_or_default();
        let ms = |d: Duration| d.as_secs_f32() * 1000.0;
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
            tracking_live: s.hub.feed.is_live(),
        };
        drop(totals);
        if log_due.elapsed() >= LOG_EVERY {
            log_due = Instant::now();
            log::info!(
                "{:.1} fps received, {:.1} fps decoded, {:.1} Mbps, decode {:.2} ms mean, rtt {} ms, network {} ms, \
                 pose to decode {} ms, {} of {} packets lost, {} units recovered, {} units lost",
                stats.received_fps,
                stats.decoded_fps,
                stats.received_mbps,
                stats.decode_mean_ms,
                stats.rtt_ms.map_or("-".into(), |v| format!("{v:.2}")),
                stats.network_ms.map_or("-".into(), |v| format!("{v:.2}")),
                stats.sample_to_decode_ms.map_or("-".into(), |v| format!("{v:.1}")),
                stats.packets_expected.saturating_sub(stats.packets_received),
                stats.packets_expected,
                stats.units_recovered,
                stats.units_lost,
            );
        }
        s.hub.report(s.id, |snapshot: &mut Snapshot| snapshot.stats = stats);
    }
}
