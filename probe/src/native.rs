//! The probe as a client of cranvr's own protocol (`--protocol native`). It behaves like a
//! headset app:
//! - connects to the host, agrees on a stream and sends synthetic tracking once per refresh;
//! - receives the media, restores lost packets with the parity and decodes with VideoToolbox;
//! - reports to the host what a real headset reports: the decode end and the display time of each
//!   frame. It also reports the loss that it saw;
//! - checks the eye badges that the host draws, as the ALVR probe does.
//!
//! The display is simulated. It refreshes on a fixed grid of the stream's rate. A decoded frame
//! gets the first refresh after its decode. The display times in the report lie on that grid. The
//! host's display clock follows it as it would follow a real compositor's.
//!
//! `--drop-percent` drops that share of the media packets before the receiver sees them, so a
//! run on loopback can show the parity at work.

use crate::badge_points;
use cranvr_common::foveation::{Foveation, FoveationConfig};
use cranvr_proto::{
    clock::{now_ns as process_now_ns, ClockSync},
    link::{ClientLink, ClientSide, Incoming},
    media::{FrameReceiver, Received},
    message::{
        buttons, ClientCaps, ClientDatagram, ClientMsg, FrameTiming, HandInput, HostDatagram, HostMsg, Input, LossReport, Ping, Pong,
        Tracking, Views,
    },
    wire::{Codec, Fov, Motion, Pose},
    Error,
};
use cranvr_vt::{DecodedFrame, Decoder};
use std::{
    collections::VecDeque,
    f32::consts::FRAC_PI_4,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

pub struct Options {
    pub seconds: f32,
    /// Expect the fake game's badges (left blue, right red)
    pub game: bool,
    /// Count every frame of the right size
    pub any_picture: bool,
    pub connect: SocketAddr,
    /// Ask for the media over TCP, as a USB link needs it
    pub tcp: bool,
    pub drop_percent: f32,
}

/// The probe's clock runs this far ahead of the process clock, as a headset's clock has its own
/// zero. The host and the probe are both fresh processes on one Mac. Without the skew their clocks
/// would start within a millisecond of each other. The clock sync would have nothing to find.
const CLOCK_SKEW_NS: u64 = 123_456_789_012;

/// The probe's monotonic clock in nanoseconds.
fn now_ns() -> u64 {
    process_now_ns() + CLOCK_SKEW_NS
}

/// Where the probe's eyes sit to the left and to the right of its head, in meters.
const EYE_X: f32 = 0.032;

/// How often the client reports its loss. A unit may wait this long for its packets.
const REPORT_EVERY: Duration = Duration::from_millis(100);
const UNIT_TIMEOUT: Duration = Duration::from_millis(100);
const PING_EVERY: Duration = Duration::from_millis(200);
/// The least time between two key frame requests
const KEY_REQUEST_GAP: Duration = Duration::from_millis(250);

/// xorshift64*, for the packet drops: the same drops on every run.
struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        ((self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32) / (1u64 << 24) as f32
    }
}

#[derive(Default)]
struct Counters {
    packets: AtomicU64,
    dropped: AtomicU64,
    malformed: AtomicU64,
    key_requests: AtomicU64,
    haptics: AtomicU64,
}

/// What the decode thread measured, in ms per frame.
#[derive(Default)]
struct Latencies {
    /// Tracking sample to display
    pose_to_display: Vec<f64>,
    /// Tracking sample to the end of the decode
    pose_to_decode: Vec<f64>,
    /// Tracking sample to the start of the host's send
    pose_to_send: Vec<f64>,
    /// The host's send to the last packet's arrival
    network: Vec<f64>,
    /// The last packet's arrival to the end of the decode
    decode: Vec<f64>,
    /// The display time that the probe's display gave the frame, minus the host's target
    target_gap: Vec<f64>,
}

struct Shared {
    link: ClientLink,
    receiver: Mutex<FrameReceiver>,
    clock: Mutex<ClockSync>,
    /// The head poses that the probe sent, by sample time. The decoder checks the eye poses that
    /// come back in the frame metadata against them.
    sent_poses: Mutex<VecDeque<(u64, Pose)>>,
    /// Frames wait for a key frame at the start and after a lost unit
    waiting_key: AtomicBool,
    last_key_request: Mutex<Instant>,
    /// The decoder config and how many times it came
    config: Mutex<(u64, Vec<u8>)>,
    counters: Counters,
    totals: Mutex<LossReport>,
    drop_percent: f32,
    rng: Mutex<Rng>,
    stop: AtomicBool,
}

impl Shared {
    fn request_key_frame(&self, frame: u32) {
        let mut last = self.last_key_request.lock().unwrap();
        if last.elapsed() < KEY_REQUEST_GAP {
            return;
        }
        *last = Instant::now();
        self.counters.key_requests.fetch_add(1, Ordering::Relaxed);
        self.link.duplex.send_control(&ClientMsg::RequestKeyFrame { stream: 0, frame }).ok();
    }
}

fn caps(tcp: bool) -> ClientCaps {
    let fov = Fov { left: -FRAC_PI_4, right: FRAC_PI_4, up: FRAC_PI_4, down: -FRAC_PI_4 };
    let eye = |x: f32| Pose { position: [x, 0.0, 0.0], ..Pose::IDENTITY };
    ClientCaps {
        name: "cranvr-probe".into(),
        eye_size: [2064, 2208], // Quest 3
        views: Views { fov: [fov, fov], eye_pose: [eye(-EYE_X), eye(EYE_X)] },
        // A Quest 3's rates. The higher ones measure how much room the stream leaves
        refresh_rates: vec![72.0, 80.0, 90.0, 120.0, 144.0, 165.0, 180.0, 240.0],
        codecs: vec![Codec::Hevc],
        decoders: 1,
        foveation: true,
        tcp_only: tcp,
    }
}

/// Runs the probe and returns its exit code: 0 when at least half of the expected frames arrived
/// and decoded with the right badges.
pub fn run(o: Options) -> i32 {
    println!("probe: waiting for the host at {} (run cranvr-host --protocol native)", o.connect);
    let started = Instant::now();
    let link = loop {
        match ClientLink::connect(o.connect, &caps(o.tcp), Duration::from_secs(5)) {
            Ok(link) => break link,
            // The host is not up yet, or not ready
            Err(Error::Io(_) | Error::Timeout | Error::Closed) if started.elapsed() < Duration::from_secs(60) => {
                thread::sleep(Duration::from_millis(200));
            }
            Err(e) => {
                eprintln!("probe: cannot connect: {e}");
                return 1;
            }
        }
    };
    let stream = link.welcome.stream.clone();
    let foveation = stream.foveation.map(|f| Foveation::new(stream.eye_size, &FoveationConfig::from(f)));
    let expected_size = foveation.map_or((stream.eye_size[0] as usize * 2, stream.eye_size[1] as usize), |f| {
        (f.optimized[0] as usize * 2, f.optimized[1] as usize)
    });
    let fps = stream.refresh_hz;
    println!(
        "probe: streaming started: {}x{} per eye at {fps} Hz, foveated: {}, expecting {}x{} frames, {} media, parity packets from the host",
        stream.eye_size[0],
        stream.eye_size[1],
        foveation.is_some(),
        expected_size.0,
        expected_size.1,
        if link.duplex.has_udp() { "UDP" } else { "TCP" },
    );
    let shared = Arc::new(Shared {
        link,
        receiver: Mutex::new(FrameReceiver::new()),
        clock: Mutex::new(ClockSync::new()),
        sent_poses: Mutex::new(VecDeque::new()),
        waiting_key: AtomicBool::new(true),
        last_key_request: Mutex::new(Instant::now() - KEY_REQUEST_GAP),
        config: Mutex::new((0, Vec::new())),
        counters: Counters::default(),
        totals: Mutex::new(LossReport::default()),
        drop_percent: o.drop_percent,
        rng: Mutex::new(Rng(0x9E37_79B9_7F4A_7C15)),
        stop: AtomicBool::new(false),
    });

    let (units, unit_receiver) = mpsc::channel::<Received>();
    // The simulated display's refresh times: this origin plus whole periods
    let vsync_origin = now_ns();
    let period = (1e9 / fps as f64) as u64;

    // The read loops
    if shared.link.duplex.has_udp() {
        let (shared, units) = (Arc::clone(&shared), units.clone());
        thread::Builder::new().name("udp-read".into()).spawn(move || read_loop(&shared, &units, true)).unwrap();
    }
    {
        let (shared, units) = (Arc::clone(&shared), units.clone());
        thread::Builder::new()
            .name("tcp-read".into())
            .spawn(move || {
                read_loop(&shared, &units, false);
                shared.stop.store(true, Ordering::Relaxed);
            })
            .unwrap();
    }
    // The headset's clock: tracking, input, pings and the loss reports
    {
        let shared = Arc::clone(&shared);
        thread::Builder::new().name("tracking".into()).spawn(move || tracking_loop(&shared, period)).unwrap();
    }
    // The decoder
    let results = Arc::new(Mutex::new(DecodeResults::default()));
    {
        let (shared, results) = (Arc::clone(&shared), Arc::clone(&results));
        let points = badge_points(foveation.as_ref());
        let check = BadgeCheck { game: o.game, any_picture: o.any_picture, expected_size };
        thread::Builder::new()
            .name("decode".into())
            .spawn(move || decode_loop(&shared, &unit_receiver, points, check, (vsync_origin, period), &results))
            .unwrap();
    }

    // Progress lines and the end
    let begin = Instant::now();
    let mut last_report = Duration::ZERO;
    while begin.elapsed().as_secs_f32() < o.seconds && !shared.stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(50));
        if begin.elapsed().saturating_sub(last_report) >= Duration::from_secs(2) {
            last_report = begin.elapsed();
            let r = results.lock().unwrap();
            println!(
                "probe: {:.1}s: {} frames received, {} decoded ({} good), {:.1} fps",
                last_report.as_secs_f32(),
                r.received,
                r.decoded,
                r.good,
                r.decoded as f32 / last_report.as_secs_f32()
            );
        }
    }
    shared.stop.store(true, Ordering::Relaxed);
    shared.link.duplex.send_control(&ClientMsg::Bye).ok();
    thread::sleep(Duration::from_millis(100));
    shared.link.duplex.close();

    let r = results.lock().unwrap();
    let expected = (o.seconds * fps) as u64;
    println!(
        "probe: done: {} received, {} decoded ({} good, {} wrong) at {}x{}, expected about {expected}; {} haptic pulses",
        r.received,
        r.decoded,
        r.good,
        r.bad,
        r.size.0,
        r.size.1,
        shared.counters.haptics.load(Ordering::Relaxed)
    );
    print_summary(&shared, &r, o.seconds, expected);
    i32::from(r.good * 2 < expected)
}

#[derive(Default)]
struct DecodeResults {
    /// Frames that reached the decoder
    received: u64,
    decoded: u64,
    good: u64,
    bad: u64,
    size: (usize, usize),
    /// Frames that got their own refresh. Frames that a newer frame replaced within one refresh
    shown: u64,
    replaced: u64,
    /// Frames dropped before the decoder: they were not key frames and no key frame came yet
    skipped: u64,
    /// Frames whose eye poses equal the head pose that the probe sent plus the eye offset, frames
    /// whose poses differ, and frames whose sample the probe no longer knew
    poses_ok: u64,
    poses_bad: u64,
    poses_unknown: u64,
    /// Frames whose slice rectangle covers the whole decoded image
    slices_ok: u64,
    /// Frames that came before the host's clock estimate was ready, so they have no target
    no_target: u64,
    latencies: Latencies,
}

struct BadgeCheck {
    game: bool,
    any_picture: bool,
    expected_size: (usize, usize),
}

/// Reads the host's packets until the connection ends. UDP and TCP each have a loop.
fn read_loop(shared: &Arc<Shared>, units: &mpsc::Sender<Received>, udp: bool) {
    let handler = |incoming: Incoming<'_, ClientSide>| match incoming {
        Incoming::Media(packet) => on_media(shared, packet, units),
        Incoming::Datagram(HostDatagram::Ping(p)) => {
            let now = now_ns();
            let pong = Pong { id: p.id, ping_sent: p.sent, received: now, sent: now_ns() };
            shared.link.duplex.send_datagram(&ClientDatagram::Pong(pong)).ok();
        }
        Incoming::Datagram(HostDatagram::Pong(p)) => {
            shared.clock.lock().unwrap().add(p.ping_sent, p.received, p.sent, now_ns());
        }
        Incoming::Datagram(HostDatagram::Haptics(_)) => {
            if shared.counters.haptics.fetch_add(1, Ordering::Relaxed) == 0 {
                println!("probe: first haptic pulse");
            }
        }
        Incoming::Datagram(HostDatagram::Audio(_)) => {}
        Incoming::Control(HostMsg::DecoderConfig { config, .. }) => {
            let mut c = shared.config.lock().unwrap();
            *c = (c.0 + 1, config);
        }
        Incoming::Control(HostMsg::Bye) => shared.stop.store(true, Ordering::Relaxed),
        Incoming::Control(_) => {}
    };
    let reason = if udp { shared.link.duplex.read_udp(handler) } else { shared.link.duplex.read_tcp(handler) };
    if !shared.stop.load(Ordering::Relaxed) {
        println!("probe: the link ended: {reason}");
    }
}

fn on_media(shared: &Shared, packet: &[u8], units: &mpsc::Sender<Received>) {
    shared.counters.packets.fetch_add(1, Ordering::Relaxed);
    if shared.drop_percent > 0.0 && shared.rng.lock().unwrap().unit() * 100.0 < shared.drop_percent {
        shared.counters.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let mut receiver = shared.receiver.lock().unwrap();
    match receiver.push(packet, now_ns()) {
        Ok(Some(unit)) => {
            drop(receiver);
            units.send(unit).ok();
        }
        Ok(None) => {
            let lost = receiver.take_lost();
            drop(receiver);
            if let Some(first) = lost.first() {
                shared.waiting_key.store(true, Ordering::Relaxed);
                shared.request_key_frame(first.frame);
            }
        }
        Err(_) => {
            shared.counters.malformed.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Sends the synthetic tracking at the refresh rate, then the slower reports.
fn tracking_loop(shared: &Shared, period_ns: u64) {
    let origin = Instant::now();
    let period = Duration::from_nanos(period_ns);
    let mut deadline = origin;
    let (mut last_report, mut last_ping, mut ping_id) = (origin, origin - PING_EVERY, 0u32);
    let turn_rate = 20f32.to_radians();
    let mut squeezed = false;
    while !shared.stop.load(Ordering::Relaxed) {
        let t = origin.elapsed().as_secs_f32();
        let (sin, cos) = (turn_rate * t / 2.0).sin_cos();
        // Turns about the vertical axis: the quaternion (0, sin, 0, cos)
        let head = Motion {
            pose: Pose { orientation: [0.0, sin, 0.0, cos], position: [0.0, 1.6, 0.0] },
            linear_velocity: [0.0; 3],
            angular_velocity: [0.0, turn_rate, 0.0],
        };
        let left = Motion { pose: Pose { orientation: [0.0, 0.0, 0.0, 1.0], position: [-0.2, 1.2, -0.4] }, ..Default::default() };
        let sample_time = now_ns();
        {
            let mut sent = shared.sent_poses.lock().unwrap();
            if sent.len() >= 512 {
                sent.pop_front();
            }
            sent.push_back((sample_time, head.pose));
        }
        shared.link.duplex.send_datagram(&ClientDatagram::Tracking(Tracking { sample_time, head, left: Some(left), right: None })).ok();
        // Squeeze the left grip for half a second every 3 seconds, as a game test may ask
        let squeeze = t % 3.0 > 2.5;
        if squeeze != squeezed {
            squeezed = squeeze;
            let mut input = Input { sample_time, ..Default::default() };
            input.hands[0] = HandInput { grip: if squeeze { 1.0 } else { 0.0 }, buttons: buttons::GRIP_CLICK * u32::from(squeeze), ..Default::default() };
            shared.link.duplex.send_datagram(&ClientDatagram::Input(input)).ok();
        }
        if last_ping.elapsed() >= PING_EVERY {
            last_ping = Instant::now();
            shared.link.duplex.send_datagram(&ClientDatagram::Ping(Ping { id: ping_id, sent: now_ns() })).ok();
            ping_id += 1;
        }
        if last_report.elapsed() >= REPORT_EVERY {
            last_report = Instant::now();
            let (report, lost) = {
                let mut receiver = shared.receiver.lock().unwrap();
                receiver.expire(now_ns(), UNIT_TIMEOUT.as_nanos() as u64);
                (receiver.take_report(), receiver.take_lost())
            };
            if report != LossReport::default() {
                shared.link.duplex.send_datagram(&ClientDatagram::Loss(report)).ok();
                let mut totals = shared.totals.lock().unwrap();
                totals.packets_expected += report.packets_expected;
                totals.packets_received += report.packets_received;
                totals.units_complete += report.units_complete;
                totals.units_recovered += report.units_recovered;
                totals.units_lost += report.units_lost;
                totals.packets_recovered += report.packets_recovered;
            }
            if let Some(first) = lost.first() {
                shared.waiting_key.store(true, Ordering::Relaxed);
                shared.request_key_frame(first.frame);
            }
        }
        // Wait for the next refresh. `park_timeout` may wake early, so it checks the clock. A
        // thread that fell far behind starts again from now, not with a burst of samples.
        deadline += period;
        if deadline + period * 4 < Instant::now() {
            deadline = Instant::now();
        }
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            thread::park_timeout(deadline - now);
        }
    }
}

/// The first refresh at or after `t` on the grid `origin + k * period`.
fn next_refresh(origin: u64, period: u64, t: u64) -> u64 {
    if t <= origin {
        origin
    } else {
        origin + (t - origin).div_ceil(period) * period
    }
}

fn decode_loop(
    shared: &Shared,
    units: &mpsc::Receiver<Received>,
    points: Vec<(f32, f32)>,
    check: BadgeCheck,
    (vsync_origin, period): (u64, u64),
    results: &Mutex<DecodeResults>,
) {
    let (decoded_sender, decoded_receiver) = mpsc::channel::<DecodedFrame>();
    let mut decoder: Option<Decoder> = None;
    let mut config_seen = 0;
    let mut last_slot = u64::MAX;
    let mut last_frame: Option<u32> = None;
    let ms = |ns: u64| ns as f64 / 1e6;
    for unit in units {
        let mut r = results.lock().unwrap();
        // A new decoder config, or the first one
        let (generation, config) = shared.config.lock().unwrap().clone();
        if generation != config_seen {
            config_seen = generation;
            decoder = None;
            match Decoder::new(&config, points.clone(), decoded_sender.clone()) {
                Ok(d) => {
                    println!("probe: decoder ready ({} bytes of parameter sets)", config.len());
                    decoder = Some(d);
                    shared.waiting_key.store(true, Ordering::Relaxed);
                }
                Err(e) => eprintln!("probe: decoder: {e}"),
            }
        }
        let Some(d) = decoder.as_mut() else {
            r.skipped += 1;
            continue;
        };
        // The host numbers its frames one by one. A gap means that a unit never came, so the next
        // frame refers to a picture that the decoder does not have.
        let in_order = last_frame.is_none_or(|last| unit.meta.frame == last.wrapping_add(1));
        last_frame = Some(unit.meta.frame);
        if unit.meta.key {
            shared.waiting_key.store(false, Ordering::Relaxed);
        } else if !in_order {
            shared.waiting_key.store(true, Ordering::Relaxed);
        }
        if !unit.meta.key && shared.waiting_key.load(Ordering::Relaxed) {
            // A frame that refers to one the decoder never saw: ask for a key frame
            r.skipped += 1;
            shared.request_key_frame(unit.meta.frame);
            continue;
        }
        if unit.meta.slices.len() != 1 {
            r.skipped += 1;
            continue;
        }
        r.received += 1;
        if let Err(e) = d.decode(unit.meta.tracking_sample, unit.payload()) {
            eprintln!("probe: decode: {e}");
            continue;
        }
        let decode_end = now_ns();
        let Ok(frame) = decoded_receiver.try_recv() else { continue };
        // The simulated display shows the frame at the first refresh after its decode
        let display_time = next_refresh(vsync_origin, period, decode_end);
        let timing = FrameTiming {
            stream: 0,
            frame: unit.meta.frame,
            tracking_sample: unit.meta.tracking_sample,
            received: unit.completed,
            decode_end,
            display_time,
            next_display_time: display_time + period,
        };
        shared.link.duplex.send_datagram(&ClientDatagram::FrameTiming(timing)).ok();
        let slot = (display_time - vsync_origin) / period;
        if slot == last_slot {
            r.replaced += 1;
        } else {
            r.shown += 1;
            last_slot = slot;
        }

        let l = &mut r.latencies;
        l.pose_to_display.push(ms(display_time.saturating_sub(unit.meta.tracking_sample)));
        l.pose_to_decode.push(ms(decode_end.saturating_sub(unit.meta.tracking_sample)));
        l.decode.push(ms(decode_end.saturating_sub(unit.completed)));
        if let Some(sent) = shared.clock.lock().unwrap().to_local(unit.meta.sent) {
            l.pose_to_send.push(ms(sent.saturating_sub(unit.meta.tracking_sample)));
            l.network.push(ms(unit.completed.saturating_sub(sent)));
        }

        // What the host put in the metadata
        let sent = shared.sent_poses.lock().unwrap().iter().rev().find(|(t, _)| *t == unit.meta.tracking_sample).map(|(_, p)| *p);
        match sent {
            Some(head) => {
                let expected = [eye_pose(&head, -EYE_X), eye_pose(&head, EYE_X)];
                if unit.meta.eye_poses.iter().zip(&expected).all(|(a, b)| same_pose(a, b)) {
                    r.poses_ok += 1;
                } else {
                    r.poses_bad += 1;
                    if r.poses_bad <= 3 {
                        eprintln!("probe: eye poses {:?} differ from {:?}", unit.meta.eye_poses, expected);
                    }
                }
            }
            None => r.poses_unknown += 1,
        }
        if unit.meta.slices.len() == 1 && (unit.meta.slices[0].x, unit.meta.slices[0].y) == (0, 0) && (unit.meta.slices[0].w as usize, unit.meta.slices[0].h as usize) == (frame.width, frame.height) {
            r.slices_ok += 1;
        }
        if unit.meta.target_display == 0 {
            r.no_target += 1;
        } else {
            r.latencies.target_gap.push((display_time as i128 - unit.meta.target_display as i128) as f64 / 1e6);
        }

        r.decoded += 1;
        r.size = (frame.width, frame.height);
        let (left, right) = (frame.samples[0], frame.samples[1]);
        let size_ok = r.size == check.expected_size;
        if !size_ok && r.bad == 0 {
            eprintln!("probe: frame is {}x{}, the client expects {}x{}", r.size.0, r.size.1, check.expected_size.0, check.expected_size.1);
        }
        let (red, blue) = if check.game { (right, left) } else { (left, right) };
        let badges_ok = red.cr > 180 && red.cb < 128 && blue.cb > 180 && blue.cr < 128;
        if size_ok && (check.any_picture || badges_ok) {
            r.good += 1;
        } else {
            r.bad += 1;
            if r.bad <= 3 {
                eprintln!("probe: wrong badge colors: left {left:?} right {right:?}");
            }
        }
    }
}

/// The pose of an eye that sits `x` meters to the side of a head, in the head's own frame.
fn eye_pose(head: &Pose, x: f32) -> Pose {
    let [qx, qy, qz, qw] = head.orientation;
    // Rotates the vector (x, 0, 0) by the quaternion: v + 2w(q x v) + 2 q x (q x v)
    let v = [x, 0.0, 0.0];
    let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let q = [qx, qy, qz];
    let t = cross(q, v).map(|c| 2.0 * c);
    let u = cross(q, t);
    let rotated = [v[0] + qw * t[0] + u[0], v[1] + qw * t[1] + u[1], v[2] + qw * t[2] + u[2]];
    Pose { orientation: head.orientation, position: [head.position[0] + rotated[0], head.position[1] + rotated[1], head.position[2] + rotated[2]] }
}

fn same_pose(a: &Pose, b: &Pose) -> bool {
    a.orientation.iter().zip(&b.orientation).all(|(x, y)| (x - y).abs() < 1e-4) && a.position.iter().zip(&b.position).all(|(x, y)| (x - y).abs() < 1e-4)
}

/// The mean, the median, the 95th percentile and the largest value.
fn describe(values: &[f64]) -> String {
    if values.is_empty() {
        return "no data".into();
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
    let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;
    format!("mean {mean:.2}, median {:.2}, p95 {:.2}, max {:.2}", at(0.5), at(0.95), sorted[sorted.len() - 1])
}

fn print_summary(shared: &Shared, r: &DecodeResults, seconds: f32, expected: u64) {
    let c = &shared.counters;
    let totals = *shared.totals.lock().unwrap();
    let lost = totals.packets_expected.saturating_sub(totals.packets_received);
    println!(
        "probe: native: {:.1} frames per second decoded, {:.1} shown on their own refresh, {} replaced by a newer frame, {} skipped before the decoder (of about {expected} expected)",
        r.decoded as f32 / seconds,
        r.shown as f32 / seconds,
        r.replaced,
        r.skipped
    );
    println!(
        "probe: native: packets {} seen, {} dropped by the test, {} malformed; units {} complete, {} lost; {} units needed parity, {} data packets restored; {} lost on the wire of {} expected ({:.2}%); {} key frame requests",
        c.packets.load(Ordering::Relaxed),
        c.dropped.load(Ordering::Relaxed),
        c.malformed.load(Ordering::Relaxed),
        totals.units_complete,
        totals.units_lost,
        totals.units_recovered,
        totals.packets_recovered,
        lost,
        totals.packets_expected,
        100.0 * lost as f64 / totals.packets_expected.max(1) as f64,
        c.key_requests.load(Ordering::Relaxed),
    );
    if let Some(e) = shared.clock.lock().unwrap().estimate() {
        println!("probe: native: clock offset to the host {:.3} ms, round trip {:.3} ms", e.offset_ns as f64 / 1e6, e.rtt_ns as f64 / 1e6);
    }
    let l = &r.latencies;
    println!(
        "probe: native: frame metadata: eye poses right in {} frames, wrong in {}, sample unknown in {}; slice rectangle right in {}; no target display in {}",
        r.poses_ok, r.poses_bad, r.poses_unknown, r.slices_ok, r.no_target
    );
    let near = l.target_gap.iter().filter(|g| g.abs() < 1.0).count();
    println!(
        "probe: native: display time of the probe's display minus the host's target display time (ms): {}; {:.0}% within 1 ms",
        describe(&l.target_gap),
        100.0 * near as f64 / l.target_gap.len().max(1) as f64
    );
    println!("probe: frame arrival: pose to display (ms): {}", describe(&l.pose_to_display));
    println!("probe: frame arrival: pose to decode end (ms): {}", describe(&l.pose_to_decode));
    println!("probe: frame arrival: pose to send start (ms): {}", describe(&l.pose_to_send));
    println!("probe: frame arrival: network, send start to last packet (ms): {}", describe(&l.network));
    println!("probe: frame arrival: decode (ms): {}", describe(&l.decode));
}
