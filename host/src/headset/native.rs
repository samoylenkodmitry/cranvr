//! cranvr's own protocol behind the headset interface (see cranvr-proto). The host listens on a
//! TCP port. A cranvr client dials it. The two sides agree on a stream. After that this file:
//! - keeps the last tracking samples, so the frame loop can ask for the pose of a sample;
//! - estimates the offset between the headset's clock and the host's;
//! - turns the headset's frame timing reports into a display clock;
//! - sends each encoded slice with the render poses and the display time the host aims for;
//! - sets the parity share of the media from the client's loss reports.
//!
//! One client connects at a time. A second one gets the reason `Busy`.
//!
//! What is still ALVR only: game audio and the desktop layer.

use super::{Button, Device, Event, EventSource, Headset, OutFrame};
use crate::{runtime_link::Haptic, session::Negotiated};
use alvr_common::{
    error,
    glam::{Quat, Vec3},
    info, warn, DeviceMotion, Fov as AlvrFov, Pose as AlvrPose, LEFT_MENU_CLICK_ID, LEFT_SQUEEZE_CLICK_ID, LEFT_SQUEEZE_VALUE_ID,
    LEFT_THUMBREST_TOUCH_ID, LEFT_THUMBSTICK_CLICK_ID, LEFT_THUMBSTICK_TOUCH_ID, LEFT_THUMBSTICK_X_ID, LEFT_THUMBSTICK_Y_ID,
    LEFT_TRIGGER_CLICK_ID, LEFT_TRIGGER_TOUCH_ID, LEFT_TRIGGER_VALUE_ID, LEFT_X_CLICK_ID, LEFT_X_TOUCH_ID, LEFT_Y_CLICK_ID,
    LEFT_Y_TOUCH_ID, RIGHT_A_CLICK_ID, RIGHT_A_TOUCH_ID, RIGHT_B_CLICK_ID, RIGHT_B_TOUCH_ID, RIGHT_MENU_CLICK_ID,
    RIGHT_SQUEEZE_CLICK_ID, RIGHT_SQUEEZE_VALUE_ID, RIGHT_THUMBREST_TOUCH_ID, RIGHT_THUMBSTICK_CLICK_ID,
    RIGHT_THUMBSTICK_TOUCH_ID, RIGHT_THUMBSTICK_X_ID, RIGHT_THUMBSTICK_Y_ID, RIGHT_TRIGGER_CLICK_ID, RIGHT_TRIGGER_TOUCH_ID,
    RIGHT_TRIGGER_VALUE_ID,
};
use alvr_server_core::ViewsConfig;
use cranvr_common::foveation::FoveationConfig;
use cranvr_proto::{
    clock::{now_ns, ClockSync},
    fec::ParityController,
    link::{HostLink, HostListener, HostSide, Incoming, MediaMode},
    media::{FrameMeta, DEFAULT_SHARD_LEN},
    message::{
        buttons, negotiate, ClientDatagram, ClientMsg, FrameTiming, HostDatagram, HostMsg, HostWishes, Input,
        LossReport, Ping, Pong, RejectReason, Tracking, Views as ProtoViews,
    },
    wire::{Codec, Nanos, Pose as ProtoPose, Rect},
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc::{self, RecvTimeoutError},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

/// What the host wants from native mode.
pub struct Config {
    pub port: u16,
    /// Names of the clients that may connect from another machine. A client on this machine may
    /// always connect.
    pub trust: Vec<String>,
    pub trust_all: bool,
    /// Send the media over TCP, as a USB link needs it
    pub tcp_media: bool,
    pub wishes: HostWishes,
}

/// The parity share with no loss. Five percent costs five packets on a frame of a hundred.
const PARITY_MIN: f32 = 0.05;
/// The highest parity share.
const PARITY_MAX: f32 = 0.5;
/// Tracking samples kept for `motion`. At 120 samples per second this is a second.
const HISTORY: usize = 128;
/// Without any packet from the client for this long the session ends.
const SILENCE: Duration = Duration::from_secs(5);
/// Pings per second at the start of a session, until the clock estimate has a few exchanges, and
/// after that.
const FAST_PING: Duration = Duration::from_millis(50);
const SLOW_PING: Duration = Duration::from_millis(200);
const FAST_PINGS: u32 = 20;
/// The display time that the host aims for lies this far after the frame should have arrived and
/// decoded.
const DISPLAY_MARGIN: Duration = Duration::from_millis(1);

struct Shared {
    events: mpsc::Sender<Event>,
    session: Mutex<Option<Arc<Session>>>,
    /// The last decoder config. A new session gets it at once, because the encoder sends it only
    /// when it changes.
    decoder_config: Mutex<Option<Vec<u8>>>,
}

impl Shared {
    fn emit(&self, event: Event) {
        self.events.send(event).ok();
    }

    fn session(&self) -> Option<Arc<Session>> {
        self.session.lock().unwrap().clone()
    }
}

struct Native {
    shared: Arc<Shared>,
}

struct Events(mpsc::Receiver<Event>);

impl EventSource for Events {
    fn next(&mut self, timeout: Duration) -> Result<Event, RecvTimeoutError> {
        self.0.recv_timeout(timeout)
    }
}

/// Starts listening. A bind error comes back to the caller.
pub fn start(config: Config) -> std::io::Result<(Arc<dyn Headset>, Box<dyn EventSource>)> {
    let local_only = !config.trust_all && config.trust.is_empty();
    let address: SocketAddr = if local_only { ([127, 0, 0, 1], config.port).into() } else { ([0, 0, 0, 0], config.port).into() };
    let listener = HostListener::bind(address).map_err(|e| std::io::Error::other(e.to_string()))?;
    info!(
        "Native protocol: listening on {address}{}",
        if local_only { " (this Mac only: add --trust-all or --trust NAME for a headset)" } else { "" }
    );
    let (events, receiver) = mpsc::channel();
    let shared = Arc::new(Shared { events, session: Mutex::new(None), decoder_config: Mutex::new(None) });
    {
        let shared = Arc::clone(&shared);
        thread::Builder::new().name("native-accept".into()).spawn(move || accept_loop(listener, config, shared))?;
    }
    Ok((Arc::new(Native { shared }), Box::new(Events(receiver))))
}

fn accept_loop(listener: HostListener, config: Config, shared: Arc<Shared>) {
    loop {
        let pending = match listener.accept() {
            Ok(pending) => pending,
            Err(e) => {
                warn!("Native protocol: a connection failed before its hello: {e}");
                continue;
            }
        };
        let name = pending.caps.name.clone();
        if shared.session().is_some() {
            warn!("Native protocol: {name} at {} was refused: another headset is connected", pending.peer);
            pending.reject(RejectReason::Busy);
            continue;
        }
        if !(config.trust_all || pending.peer.ip().is_loopback() || config.trust.contains(&name)) {
            warn!("Native protocol: {name} at {} is not trusted (start the host with --trust \"{name}\")", pending.peer);
            pending.reject(RejectReason::NotTrusted);
            continue;
        }
        let stream = match negotiate(&config.wishes, &pending.caps) {
            Ok(stream) => stream,
            Err(reason) => {
                warn!("Native protocol: no stream for {name}: {reason:?}");
                pending.reject(reason);
                continue;
            }
        };
        let (mode, shard_len) = if config.tcp_media || pending.caps.tcp_only {
            (MediaMode::Tcp, 60_000)
        } else {
            (MediaMode::Udp, DEFAULT_SHARD_LEN)
        };
        let (peer, caps_views) = (pending.peer, pending.caps.views);
        let link = match pending.accept(stream.clone(), mode, shard_len) {
            Ok(link) => link,
            Err(e) => {
                warn!("Native protocol: the handshake with {name} failed: {e}");
                continue;
            }
        };
        info!("Native protocol: {name} at {peer} connected, {} media", if link.duplex.has_udp() { "UDP" } else { "TCP" });
        let session = Arc::new(Session::new(link, &stream));
        *shared.session.lock().unwrap() = Some(Arc::clone(&session));
        let shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("native-session".into())
            .spawn(move || run_session(&shared, &session, stream, caps_views, peer))
            .expect("session thread");
    }
}

fn run_session(shared: &Shared, session: &Arc<Session>, stream: cranvr_proto::message::StreamParams, views: ProtoViews, peer: SocketAddr) {
    let negotiated = Negotiated {
        eye_width: stream.eye_size[0],
        eye_height: stream.eye_size[1],
        fps: stream.refresh_hz,
        full_range: stream.full_range,
        codec: stream.codec,
        foveated: stream.foveation.is_some(),
        foveation: stream.foveation.map(FoveationConfig::from).unwrap_or_default(),
    };
    shared.emit(Event::Connected { stream: Some(negotiated), ip: Some(peer.ip()) });
    shared.emit(Event::Views(views_config(&views)));
    if let Some(config) = shared.decoder_config.lock().unwrap().clone() {
        session.send_decoder_config(config);
    }
    thread::scope(|scope| {
        scope.spawn(|| ping_loop(session));
        if session.link.duplex.has_udp() {
            scope.spawn(|| {
                session.link.duplex.read_udp(|incoming| handle(shared, session, incoming));
            });
        }
        let reason = session.link.duplex.read_tcp(|incoming| handle(shared, session, incoming));
        info!("Native protocol: the headset link ended: {reason}");
        session.stop();
    });
    *shared.session.lock().unwrap() = None;
    shared.emit(Event::Disconnected);
}

/// Sends clock pings and ends a session whose client is silent.
fn ping_loop(session: &Session) {
    let mut id = 0u32;
    loop {
        if *session.stopped.lock().unwrap() {
            return;
        }
        if session.heard.lock().unwrap().elapsed() > SILENCE {
            warn!("Native protocol: no packet from the headset for {} s, closing the link", SILENCE.as_secs());
            session.stop();
            return;
        }
        let ping = Ping { id, sent: now_ns() };
        session.link.duplex.send_datagram(&HostDatagram::Ping(ping)).ok();
        let interval = if id < FAST_PINGS { FAST_PING } else { SLOW_PING };
        id += 1;
        // `stop` wakes this wait. A stop that comes just before it leaves one interval to run out.
        let stopped = session.stopped.lock().unwrap();
        if *stopped {
            return;
        }
        drop(session.wake.wait_timeout(stopped, interval).unwrap());
    }
}

fn handle(shared: &Shared, session: &Session, incoming: Incoming<'_, HostSide>) {
    let now = now_ns();
    *session.heard.lock().unwrap() = Instant::now();
    match incoming {
        Incoming::Datagram(ClientDatagram::Tracking(t)) => {
            session.history.lock().unwrap().push(t);
            shared.emit(Event::Tracking { sample_ns: t.sample_time });
        }
        Incoming::Datagram(ClientDatagram::Input(input)) => {
            let buttons = session.input.lock().unwrap().changes(&input);
            if !buttons.is_empty() {
                shared.emit(Event::Buttons(buttons));
            }
        }
        Incoming::Datagram(ClientDatagram::FrameTiming(t)) => session.on_timing(&t),
        Incoming::Datagram(ClientDatagram::Loss(report)) => session.on_loss(&report),
        Incoming::Datagram(ClientDatagram::Ping(p)) => {
            let pong = Pong { id: p.id, ping_sent: p.sent, received: now, sent: now_ns() };
            session.link.duplex.send_datagram(&HostDatagram::Pong(pong)).ok();
        }
        Incoming::Datagram(ClientDatagram::Pong(p)) => {
            session.clock.lock().unwrap().add(p.ping_sent, p.received, p.sent, now);
        }
        Incoming::Datagram(ClientDatagram::Hello { .. }) | Incoming::Media(_) => {}
        Incoming::Control(ClientMsg::RequestKeyFrame { .. }) => shared.emit(Event::RequestIdr),
        Incoming::Control(ClientMsg::Views(v)) => shared.emit(Event::Views(views_config(&v))),
        Incoming::Control(ClientMsg::Bye) => session.stop(),
        Incoming::Control(ClientMsg::Recenter(recenter)) => {
            let ahead_ms = session.clock.lock().unwrap().to_local(recenter.change_time).map(|at| (at as i64 - now as i64) as f64 / 1e6);
            info!(
                "Native protocol: the headset recentered, the new origin takes effect {} from now",
                ahead_ms.map_or("at an unknown time".to_string(), |ms| format!("{ms:.1} ms"))
            );
            shared.emit(Event::Recenter);
        }
        Incoming::Control(ClientMsg::Hello(_)) => {}
    }
}

fn views_config(views: &ProtoViews) -> ViewsConfig {
    let pose = |p: &ProtoPose| AlvrPose { orientation: Quat::from_array(p.orientation), position: Vec3::from_array(p.position) };
    let fov = |f: &cranvr_proto::wire::Fov| AlvrFov { left: f.left, right: f.right, up: f.up, down: f.down };
    ViewsConfig {
        local_view_transforms: [pose(&views.eye_pose[0]), pose(&views.eye_pose[1])],
        fov: [fov(&views.fov[0]), fov(&views.fov[1])],
    }
}

fn proto_pose(p: &AlvrPose) -> ProtoPose {
    ProtoPose { orientation: p.orientation.to_array(), position: p.position.to_array() }
}

/// The newest tracking samples of a session.
#[derive(Default)]
struct History(VecDeque<Tracking>);

impl History {
    fn push(&mut self, t: Tracking) {
        if self.0.len() >= HISTORY {
            self.0.pop_front();
        }
        self.0.push_back(t);
    }

    /// The sample with this time, or the nearest one when the exact one is gone.
    fn find(&self, sample: Nanos) -> Option<&Tracking> {
        self.0.iter().rev().find(|t| t.sample_time == sample).or_else(|| self.0.iter().min_by_key(|t| t.sample_time.abs_diff(sample)))
    }
}

/// The inputs the host last saw. It turns a new input message into button changes.
#[derive(Default)]
struct InputState {
    last: Option<Input>,
}

/// The ALVR path ids of one hand: binary inputs by bit, then the analog inputs.
struct HandIds {
    /// For each click: its bit in `HandInput::buttons` and its id
    clicks: [(u32, u64); 6],
    /// For each touch: its bit in `HandInput::touches` and its id
    touches: [(u32, u64); 5],
    trigger: u64,
    grip: u64,
    stick: [u64; 2],
}

fn hand_ids(hand: usize) -> HandIds {
    use buttons::*;
    if hand == 0 {
        HandIds {
            clicks: [
                (PRIMARY, *LEFT_X_CLICK_ID),
                (SECONDARY, *LEFT_Y_CLICK_ID),
                (MENU, *LEFT_MENU_CLICK_ID),
                (STICK_CLICK, *LEFT_THUMBSTICK_CLICK_ID),
                (TRIGGER_CLICK, *LEFT_TRIGGER_CLICK_ID),
                (GRIP_CLICK, *LEFT_SQUEEZE_CLICK_ID),
            ],
            touches: [
                (PRIMARY, *LEFT_X_TOUCH_ID),
                (SECONDARY, *LEFT_Y_TOUCH_ID),
                (STICK_CLICK, *LEFT_THUMBSTICK_TOUCH_ID),
                (TRIGGER_CLICK, *LEFT_TRIGGER_TOUCH_ID),
                (THUMB_REST, *LEFT_THUMBREST_TOUCH_ID),
            ],
            trigger: *LEFT_TRIGGER_VALUE_ID,
            grip: *LEFT_SQUEEZE_VALUE_ID,
            stick: [*LEFT_THUMBSTICK_X_ID, *LEFT_THUMBSTICK_Y_ID],
        }
    } else {
        HandIds {
            clicks: [
                (PRIMARY, *RIGHT_A_CLICK_ID),
                (SECONDARY, *RIGHT_B_CLICK_ID),
                (MENU, *RIGHT_MENU_CLICK_ID),
                (STICK_CLICK, *RIGHT_THUMBSTICK_CLICK_ID),
                (TRIGGER_CLICK, *RIGHT_TRIGGER_CLICK_ID),
                (GRIP_CLICK, *RIGHT_SQUEEZE_CLICK_ID),
            ],
            touches: [
                (PRIMARY, *RIGHT_A_TOUCH_ID),
                (SECONDARY, *RIGHT_B_TOUCH_ID),
                (STICK_CLICK, *RIGHT_THUMBSTICK_TOUCH_ID),
                (TRIGGER_CLICK, *RIGHT_TRIGGER_TOUCH_ID),
                (THUMB_REST, *RIGHT_THUMBREST_TOUCH_ID),
            ],
            trigger: *RIGHT_TRIGGER_VALUE_ID,
            grip: *RIGHT_SQUEEZE_VALUE_ID,
            stick: [*RIGHT_THUMBSTICK_X_ID, *RIGHT_THUMBSTICK_Y_ID],
        }
    }
}

impl InputState {
    /// The buttons whose value differs from the last input. The first input reports all of them.
    fn changes(&mut self, now: &Input) -> Vec<Button> {
        let before = self.last.unwrap_or_default();
        let first = self.last.is_none();
        let mut out = Vec::new();
        for (hand, (old, new)) in before.hands.iter().zip(&now.hands).enumerate() {
            let ids = hand_ids(hand);
            let mut binary = |id: u64, was: bool, is: bool| {
                if first || was != is {
                    out.push(Button { path_id: id, scalar: false, value: is as u32 as f32 });
                }
            };
            for (bit, id) in ids.clicks {
                binary(id, old.buttons & bit != 0, new.buttons & bit != 0);
            }
            for (bit, id) in ids.touches {
                binary(id, old.touches & bit != 0, new.touches & bit != 0);
            }
            let mut analog = |id: u64, was: f32, is: f32| {
                if first || was != is {
                    out.push(Button { path_id: id, scalar: true, value: is });
                }
            };
            analog(ids.trigger, old.trigger, new.trigger);
            analog(ids.grip, old.grip, new.grip);
            analog(ids.stick[0], old.stick[0], new.stick[0]);
            analog(ids.stick[1], old.stick[1], new.stick[1]);
        }
        self.last = Some(*now);
        out
    }
}

/// What the display clock knows, from the client's frame timing reports. Times are on the
/// client's clock.
struct Display {
    /// A display time that the client predicted, or 0 before the first report
    next: Nanos,
    /// The time between two display times
    period: Nanos,
    /// The average time from the last packet of a frame to the end of its decode, in ns
    decode: f64,
}

/// The first point of the grid `start + k * period` that is at or after `t`.
fn grid_at_or_after(start: Nanos, period: Nanos, t: Nanos) -> Nanos {
    if t <= start || period == 0 {
        start.max(t)
    } else {
        start + (t - start).div_ceil(period) * period
    }
}

/// Sums of what the client reported, since the last stats line.
#[derive(Default)]
struct Stats {
    started: Option<Instant>,
    frames: u32,
    /// From the tracking sample to the display, ms
    total: f64,
    /// From the tracking sample to the start of the send, ms
    pose_to_send: f64,
    network: f64,
    network_frames: u32,
    decode: f64,
    vsync_wait: f64,
    sent_bytes: u64,
    sent_frames: u32,
    peak_frame_bytes: usize,
    data_packets: u64,
    parity_packets: u64,
    loss: LossReport,
}

struct SentFrame {
    frame: u32,
    /// The host's clock when the send began
    sent: Nanos,
}

/// The host's end of a connected client.
struct Session {
    link: HostLink,
    clock: Mutex<ClockSync>,
    history: Mutex<History>,
    input: Mutex<InputState>,
    display: Mutex<Display>,
    parity: Mutex<ParityController>,
    stats: Mutex<Stats>,
    sent: Mutex<VecDeque<SentFrame>>,
    next_frame: AtomicU32,
    heard: Mutex<Instant>,
    stopped: Mutex<bool>,
    wake: Condvar,
}

impl Session {
    fn new(link: HostLink, stream: &cranvr_proto::message::StreamParams) -> Self {
        let period = (1e9 / stream.refresh_hz.max(1.0) as f64) as Nanos;
        let udp = link.duplex.has_udp();
        Self {
            link,
            clock: Mutex::new(ClockSync::new()),
            history: Mutex::new(History::default()),
            input: Mutex::new(InputState::default()),
            display: Mutex::new(Display { next: 0, period, decode: 0.0 }),
            parity: Mutex::new(ParityController::new(if udp { PARITY_MIN } else { 0.0 }, PARITY_MAX)),
            stats: Mutex::new(Stats::default()),
            sent: Mutex::new(VecDeque::new()),
            next_frame: AtomicU32::new(0),
            heard: Mutex::new(Instant::now()),
            stopped: Mutex::new(false),
            wake: Condvar::new(),
        }
    }

    /// Ends the session: the read loops return and the ping loop wakes.
    fn stop(&self) {
        *self.stopped.lock().unwrap() = true;
        self.wake.notify_all();
        self.link.duplex.close();
    }

    fn send_decoder_config(&self, config: Vec<u8>) {
        let message = HostMsg::DecoderConfig { stream: 0, codec: Codec::Hevc, config };
        if let Err(e) = self.link.duplex.send_control(&message) {
            warn!("Native protocol: the decoder config did not go out: {e}");
        }
    }

    /// The time on the headset's clock that is `now` on the host's.
    fn client_time(&self, now: Nanos) -> Option<Nanos> {
        self.clock.lock().unwrap().to_peer(now)
    }

    /// The time until the display's next refresh.
    fn until_next_vsync(&self) -> Option<Duration> {
        let now = self.client_time(now_ns())?;
        let display = self.display.lock().unwrap();
        (display.next != 0).then(|| Duration::from_nanos(grid_at_or_after(display.next, display.period, now) - now))
    }

    /// The display time that a frame sent now can make, on the headset's clock: the first refresh
    /// after the frame has crossed the network and decoded. 0 while the clocks are not in sync.
    fn target_display(&self, now: Nanos) -> Nanos {
        let (Some(client_now), Some(estimate)) = (self.client_time(now), self.clock.lock().unwrap().estimate()) else { return 0 };
        let display = self.display.lock().unwrap();
        if display.next == 0 {
            return 0;
        }
        let ready = client_now + estimate.rtt_ns / 2 + display.decode as Nanos + DISPLAY_MARGIN.as_nanos() as Nanos;
        grid_at_or_after(display.next, display.period, ready)
    }

    fn on_timing(&self, t: &FrameTiming) {
        let sent = {
            let sent = self.sent.lock().unwrap();
            sent.iter().rev().find(|s| s.frame == t.frame).map(|s| s.sent)
        };
        let sent_on_client = sent.and_then(|s| self.client_time(s));
        let nominal;
        {
            let mut display = self.display.lock().unwrap();
            nominal = display.period;
            display.next = t.next_display_time;
            // The client's own gap between display times replaces the nominal one when it is close
            let gap = t.next_display_time.saturating_sub(t.display_time);
            if gap > nominal * 4 / 5 && gap < nominal * 6 / 5 {
                display.period = gap;
            }
            let decode = t.decode_end.saturating_sub(t.received) as f64;
            display.decode = if display.decode == 0.0 { decode } else { display.decode * 0.9 + decode * 0.1 };
        }
        let ms = |ns: u64| ns as f64 / 1e6;
        let mut stats = self.stats.lock().unwrap();
        stats.started.get_or_insert_with(Instant::now);
        stats.frames += 1;
        stats.total += ms(t.display_time.saturating_sub(t.tracking_sample));
        stats.decode += ms(t.decode_end.saturating_sub(t.received));
        stats.vsync_wait += ms(t.display_time.saturating_sub(t.decode_end));
        if let Some(sent) = sent_on_client {
            stats.pose_to_send += ms(sent.saturating_sub(t.tracking_sample));
            stats.network += ms(t.received.saturating_sub(sent));
            stats.network_frames += 1;
        }
    }

    fn on_loss(&self, report: &LossReport) {
        self.parity.lock().unwrap().on_report(report);
        let mut stats = self.stats.lock().unwrap();
        stats.loss.packets_expected += report.packets_expected;
        stats.loss.packets_received += report.packets_received;
        stats.loss.units_complete += report.units_complete;
        stats.loss.units_recovered += report.units_recovered;
        stats.loss.units_lost += report.units_lost;
        stats.loss.packets_recovered += report.packets_recovered;
    }
}

impl Headset for Native {
    fn until_next_vsync(&self) -> Option<Duration> {
        self.shared.session()?.until_next_vsync()
    }

    fn motion(&self, device: Device, sample_ns: u64) -> Option<DeviceMotion> {
        let session = self.shared.session()?;
        let history = session.history.lock().unwrap();
        let t = history.find(sample_ns)?;
        let motion = match device {
            Device::Head => t.head,
            Device::LeftHand => t.left?,
            Device::RightHand => t.right?,
        };
        Some(DeviceMotion {
            pose: AlvrPose { orientation: Quat::from_array(motion.pose.orientation), position: Vec3::from_array(motion.pose.position) },
            linear_velocity: Vec3::from_array(motion.linear_velocity),
            angular_velocity: Vec3::from_array(motion.angular_velocity),
        })
    }

    /// The native client sends one pose per refresh. Its poses jitter by a few milliseconds, and
    /// a pose that comes early would lose its frame with the default share.
    fn pacer_ready(&self) -> f32 {
        0.5
    }

    fn wants_eye_poses(&self) -> bool {
        true
    }

    fn set_decoder_config(&self, config: Vec<u8>) {
        *self.shared.decoder_config.lock().unwrap() = Some(config.clone());
        if let Some(session) = self.shared.session() {
            session.send_decoder_config(config);
        }
    }

    fn send_frame(&self, frame: OutFrame) {
        let Some(session) = self.shared.session() else { return };
        let index = session.next_frame.fetch_add(1, Ordering::Relaxed);
        let now = now_ns();
        let target_display = session.target_display(now);
        let parity = session.parity.lock().unwrap().clone();
        let rects: Vec<Rect> = frame.slices.iter().map(|s| Rect { x: s.rect[0], y: s.rect[1], w: s.rect[2], h: s.rect[3] }).collect();
        let mut sent_bytes = 0;
        let (mut data_packets, mut parity_packets) = (0u64, 0u64);
        for (i, slice) in frame.slices.iter().enumerate() {
            let meta = FrameMeta {
                stream: 0,
                frame: index,
                slice: i as u8,
                key: frame.idr,
                tracking_sample: frame.sample_ns,
                target_display,
                sent: now,
                eye_poses: [proto_pose(&frame.eye_poses[0]), proto_pose(&frame.eye_poses[1])],
                slices: rects.clone(),
            };
            match session.link.send_unit(&meta, &slice.data, &parity) {
                Ok(stats) => {
                    sent_bytes += stats.bytes;
                    data_packets += stats.data_packets as u64;
                    parity_packets += stats.parity_packets as u64;
                }
                Err(e) => {
                    error!("Native protocol: a frame did not go out: {e}");
                    session.stop();
                    return;
                }
            }
        }
        {
            let mut sent = session.sent.lock().unwrap();
            if sent.len() >= 64 {
                sent.pop_front();
            }
            sent.push_back(SentFrame { frame: index, sent: now });
        }
        let mut stats = session.stats.lock().unwrap();
        stats.sent_frames += 1;
        stats.sent_bytes += sent_bytes as u64;
        stats.peak_frame_bytes = stats.peak_frame_bytes.max(sent_bytes);
        stats.data_packets += data_packets;
        stats.parity_packets += parity_packets;
    }

    fn frame_submitted(&self, _sample_ns: u64) {}

    fn bitrate_bps(&self) -> Option<u64> {
        None
    }

    fn send_haptics(&self, haptic: Haptic) {
        if let Some(session) = self.shared.session() {
            let message = cranvr_proto::message::Haptics {
                hand: haptic.hand as u8,
                duration_ns: haptic.duration.as_nanos() as u64,
                frequency: haptic.frequency,
                amplitude: haptic.amplitude,
            };
            session.link.duplex.send_datagram(&HostDatagram::Haptics(message)).ok();
        }
    }

    fn take_stats(&self) -> Option<Value> {
        let session = self.shared.session()?;
        let stats = std::mem::take(&mut *session.stats.lock().unwrap());
        let seconds = stats.started?.elapsed().as_secs_f64();
        if stats.frames == 0 {
            return None;
        }
        let n = stats.frames as f64;
        let fps = stats.frames as f64 / seconds;
        let rtt_ms = session.clock.lock().unwrap().estimate().map_or(0.0, |e| e.rtt_ns as f64 / 1e6);
        let lost = stats.loss.packets_expected.saturating_sub(stats.loss.packets_received);
        let loss_pct = if stats.loss.packets_expected > 0 { 100.0 * lost as f64 / stats.loss.packets_expected as f64 } else { 0.0 };
        let sent_frames = stats.sent_frames.max(1) as f64;
        Some(json!({
            "frames": stats.frames,
            "client_fps": fps,
            "video_mbps": stats.sent_bytes as f64 * 8.0 / seconds / 1e6,
            "peak_frame_mbps": stats.peak_frame_bytes as f64 * 8.0 * fps / 1e6,
            "total_ms": stats.total / n,
            "pose_to_send_ms": stats.pose_to_send / stats.network_frames.max(1) as f64,
            "network_ms": stats.network / stats.network_frames.max(1) as f64,
            "decode_ms": stats.decode / n,
            "vsync_wait_ms": stats.vsync_wait / n,
            "rtt_ms": rtt_ms,
            "loss_pct": loss_pct,
            "units_lost": stats.loss.units_lost,
            "units_recovered": stats.loss.units_recovered,
            "packets_recovered": stats.loss.packets_recovered,
            "parity_pct": 100.0 * stats.parity_packets as f64 / stats.data_packets.max(1) as f64,
            "frame_kb": stats.sent_bytes as f64 / sent_frames / 1000.0,
        }))
    }
}

#[cfg(test)]
#[path = "../tests/native.rs"]
mod tests;
