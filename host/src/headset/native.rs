//! cranvr's own protocol behind the headset interface (see cranvr-proto). The host listens on a
//! TCP port. A cranvr client dials it. The two sides agree on a stream. After that this file:
//! - keeps the last tracking samples, so the frame loop can ask for the pose of a sample;
//! - estimates the offset between the headset's clock and the host's;
//! - turns the headset's frame timing reports into a display clock;
//! - sends each encoded slice with the render poses and the display time the host aims for. The
//!   display lock (`display_lock.rs`) plans each frame of the host's own scene for a point of the
//!   headset's display grid;
//! - sets the parity share of the media from the client's loss reports.
//!
//! One client connects at a time. A second one gets the reason `Busy`.
//!
//! A client that the host does not know waits for the user (see `pairing.rs`): the host holds its
//! connection, lists it in the status line and starts the session when the user allows it.
//! The host also answers the headsets' discovery queries (`cranvr_proto::discovery`).
//!
//! The desktop panel and the game sound are in `native/desktop.rs` and `native/audio.rs`.

pub mod audio;
pub mod desktop;
mod displays;
mod microphone;

use super::{display_lock::{DisplayLock, Report}, Button, Device, Event, EventSource, FramePlan, Headset, LinkWindow, OutFrame};
use crate::{
    pairing::{display_name, Pairings},
    runtime_link::Haptic,
    session::Negotiated,
    status,
};
use crate::pose::{DeviceMotion, Fov as ViewFov, Pose as RoomPose, Quat, Vec3, ViewsConfig};
use cranvr_common::inputs::*;
use log::{debug, error, info, warn};
use cranvr_common::foveation::FoveationConfig;
use cranvr_proto::{
    clock::{now_ns, ClockSync},
    discovery::{self, clean_name},
    fec::ParityController,
    identity::{fingerprint, format_code, from_hex, to_hex, Identity, KEY_LEN},
    link::{HostLink, HostListener, HostSide, Incoming, MediaMode, Pending},
    media::{FrameMeta, DEFAULT_SHARD_LEN},
    message::{
        buttons, display_index, negotiate, ClientDatagram, ClientMsg, FrameTiming, Focus, HostDatagram, HostMsg, HostWishes, Input,
        InputHold, LossReport, Ping, Pong, RejectReason, Tracking, Views as ProtoViews, UPDATE_HEADSET, UPDATE_MAC,
    },
    motion::extrapolate,
    tracking::{Features, Tracked},
    wire::{Codec, Error as ProtoError, Motion, Nanos, Pose as ProtoPose, Rect},
    PROTOCOL_VERSION,
};
use crate::hand_mask::HandMask;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, RecvTimeoutError},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

/// What the host wants from native mode.
pub struct Config {
    pub port: u16,
    /// The host's own key. The host signs each handshake with it. Clients check it against the key
    /// that they paired with.
    pub identity: Identity,
    /// The headsets that the user paired. A client on this machine needs no pairing.
    pub pairings: Arc<Pairings>,
    /// The name of this Mac in the discovery answer and on the headset's panel
    pub name: String,
    /// The UDP port that answers discovery queries. `None` turns discovery off.
    pub discovery_port: Option<u16>,
    /// How long the host holds a client that it does not know for the user's answer
    pub approval_wait: Duration,
    /// Ask the user about a client on this machine as well. The USB link and a flat window of the
    /// client are on this machine and need no approval. Tests turn this on to reach the approval.
    pub approve_loopback: bool,
    /// Accept every client without a pairing (for tests)
    pub trust_all: bool,
    /// Whether the host takes a link over a USB cable: the headset's USB network or `adb reverse`.
    /// The user turns it off with the Connection choice Wi-Fi. The host's preamble tells each client,
    /// and the host refuses a client that comes over the cable while it is off.
    pub cable: Arc<AtomicBool>,
    /// Send the media over TCP, as a USB link needs it
    pub tcp_media: bool,
    pub wishes: HostWishes,
    /// Plan the frames of the host's own scene for the headset's display grid
    pub display_lock: bool,
    /// The Mac's desktop as a panel in the client (see `desktop.rs`)
    pub desktop: desktop::Options,
    /// Send the Mac's sound (see `audio.rs`)
    pub audio: bool,
    /// The ring of the headset's microphone for games (see `microphone.rs`), or `None` for no
    /// microphone
    pub microphone: Option<std::path::PathBuf>,
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
    /// Held while a session starts, so two clients that were allowed together cannot both get one
    starting: Mutex<()>,
    /// The threads that hold a client for the user's answer
    asking: Asking,
    /// The tracking features that a game uses now. A new session gets them at once.
    wish: Mutex<Features>,
}

/// The most threads that hold a client for the user's answer at once. A device that opens many
/// connections and finishes each handshake cannot make the host start threads without end.
const MAX_ASKING: usize = 4;

/// Counts the threads in [`ask_user`].
#[derive(Default)]
struct Asking(Arc<AtomicUsize>);

/// A place among the threads in [`ask_user`]. It frees the place when it goes.
struct AskingPlace(Arc<AtomicUsize>);

impl Asking {
    fn enter(&self) -> Option<AskingPlace> {
        let before = self.0.fetch_add(1, Ordering::AcqRel);
        if before >= MAX_ASKING {
            self.0.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(AskingPlace(Arc::clone(&self.0)))
    }
}

impl Drop for AskingPlace {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
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

/// The `native` status line (see status.rs): where the host listens, the paired headsets and the
/// headsets that wait for the user. A waiting headset has its key (the answer names it), its name,
/// its address and the code that both screens show.
fn status_line(port: u16, pairings: &Pairings) -> Value {
    let paired = pairings.summary();
    let pending: Vec<Value> = pairings
        .waiting()
        .iter()
        .map(|request| json!({"key": to_hex(&request.key), "name": request.name, "ip": request.ip.to_string(), "code": format_code(request.code)}))
        .collect();
    json!({"type": "native", "port": port, "paired": paired, "pending": pending})
}

fn announce(config: &Config) {
    status::emit(status_line(config.port, &config.pairings));
}

/// Starts listening. A bind error comes back to the caller.
pub fn start(config: Config) -> std::io::Result<(Arc<dyn Headset>, Box<dyn EventSource>)> {
    // Any machine on the network may connect. A client that the host does not know waits for the user.
    let address: SocketAddr = ([0, 0, 0, 0], config.port).into();
    let listener = HostListener::bind(address, config.identity.clone())
        .map_err(|e| std::io::Error::other(e.to_string()))?
        .share_cable_switch(Arc::clone(&config.cable));
    info!("Native protocol: this host's key starts with {}", to_hex(&config.identity.public_key()[..8]));
    if let Some(path) = &config.microphone {
        microphone::start(path);
    }
    info!("Native protocol: listening on {address}");
    if !config.cable.load(Ordering::Relaxed) {
        info!("Native protocol: the user chose Wi-Fi, so a headset on the USB cable connects over Wi-Fi");
    }
    // A headset's USB network to this Mac has IPv6 link-local addresses only. Without this
    // listener the headset still reaches the host over Wi-Fi and `adb reverse`.
    let cable_listener = match HostListener::bind_ipv6(config.port, config.identity.clone()) {
        Ok(listener) => {
            let listener = listener.share_cable_switch(Arc::clone(&config.cable));
            info!("Native protocol: listening on [::]:{} for a headset's USB network", config.port);
            Some(listener)
        }
        Err(e) => {
            warn!("Native protocol: no IPv6 listener on port {}: {e}. A headset's USB network cannot reach this host.", config.port);
            None
        }
    };
    if let Some(port) = config.discovery_port {
        start_discovery(port, &config);
    }
    announce(&config);
    let (events, receiver) = mpsc::channel();
    let shared = Arc::new(Shared { events, session: Mutex::new(None), decoder_config: Mutex::new(None), starting: Mutex::new(()), asking: Asking::default(), wish: Mutex::new(Features::NONE) });
    {
        let shared = Arc::clone(&shared);
        let config = Arc::new(config);
        if let Some(listener) = cable_listener {
            let (config, shared) = (Arc::clone(&config), Arc::clone(&shared));
            thread::Builder::new().name("native-accept-ipv6".into()).spawn(move || accept_loop(listener, config, shared))?;
        }
        thread::Builder::new().name("native-accept".into()).spawn(move || accept_loop(listener, config, shared))?;
    }
    Ok((Arc::new(Native { shared }), Box::new(Events(receiver))))
}

/// Answers the headsets' queries on the local network: the Mac's name, the port and the start of
/// the key's fingerprint. A bind error only costs the discovery: a headset can still connect to a
/// typed address.
fn start_discovery(port: u16, config: &Config) {
    static NEVER: AtomicBool = AtomicBool::new(false);
    let responder = match discovery::Responder::bind(port) {
        Ok(responder) => responder,
        Err(e) => {
            warn!("Native protocol: no discovery on UDP port {port}: {e}. A headset needs the address of this Mac typed in.");
            return;
        }
    };
    // The headset asks for the Mac on its USB network by IPv6 multicast to every node on the cable
    let cable_responder = discovery::Responder::bind_ipv6(port)
        .inspect_err(|e| warn!("Native protocol: no IPv6 discovery on UDP port {port}: {e}. A headset's USB network does not find this Mac."))
        .ok();
    let answer = discovery::Answer {
        version: PROTOCOL_VERSION,
        port: config.port,
        fingerprint: fingerprint(&config.identity.public_key()),
        nonce: 0,
        name: config.name.clone(),
    };
    info!("Native protocol: answering discovery queries on UDP port {port} as \"{}\"", clean_name(&config.name));
    if let Some(cable_responder) = cable_responder {
        let answer = answer.clone();
        thread::Builder::new().name("native-discovery-ipv6".into()).spawn(move || cable_responder.run(&NEVER, &answer)).ok();
    }
    thread::Builder::new().name("native-discovery".into()).spawn(move || responder.run(&NEVER, &answer)).ok();
}

/// Reads the cranvr app's lines from the host's input. `allow KEY` or `deny KEY` is the user's answer
/// to a headset's request, with the key that the status line shows. `cable on` or `cable off` is the
/// user's Connection choice: off for Wi-Fi. `menu open` opens the hand menu of the connected headset.
/// `headset JSON` is a press of the app on the headset's own page (`HostMsg::HeadsetRequest`). A
/// person who runs the host by hand may type them.
pub fn read_input(pairings: Arc<Pairings>, cable: Arc<AtomicBool>, headset: Arc<dyn Headset>) {
    thread::Builder::new()
        .name("native-input".into())
        .spawn(move || {
            for line in std::io::stdin().lines().map_while(Result::ok) {
                match parse_command(&line) {
                    Some(Command::Answer { allow, key }) => {
                        if !pairings.answer(&key, allow) {
                            info!("Native protocol: no headset with the key {} waits for an answer", to_hex(&key[..4]));
                        }
                    }
                    Some(Command::Cable(on)) if cable.swap(on, Ordering::Relaxed) != on => info!(
                        "Native protocol: {}. A running link keeps its way.",
                        if on { "the user allowed the USB cable" } else { "the user chose Wi-Fi, so the next headset on the USB cable connects over Wi-Fi" }
                    ),
                    Some(Command::Cable(_)) | None => {}
                    Some(Command::OpenMenu) => {
                        if headset.open_menu() {
                            info!("Native protocol: the user asked the headset to open its hand menu");
                        } else {
                            info!("Native protocol: no headset is connected to open its hand menu");
                        }
                    }
                    Some(Command::Headset(request)) => {
                        if headset.send_headset_request(&request) {
                            info!("Native protocol: the app asks the headset for {request}");
                        } else {
                            info!("Native protocol: no headset is connected for {request}");
                        }
                    }
                }
            }
        })
        .ok();
}

/// A line of the host's input.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    /// The user's answer to the headset with this key
    Answer { allow: bool, key: [u8; KEY_LEN] },
    /// Whether the host takes a link over a USB cable
    Cable(bool),
    /// The user asks the headset to open its hand menu
    OpenMenu,
    /// A press of the app on the headset's own page, as a JSON object
    Headset(String),
}

/// `allow KEY`, `deny KEY` (the key is 64 hex digits), `cable on`, `cable off`, `menu open` or
/// `headset JSON`.
fn parse_command(line: &str) -> Option<Command> {
    if let Some(request) = line.strip_prefix("headset ") {
        let request = request.trim();
        return matches!(serde_json::from_str::<serde_json::Value>(request), Ok(serde_json::Value::Object(_))).then(|| Command::Headset(request.to_string()));
    }
    let mut words = line.split_whitespace();
    let command = match words.next()? {
        word @ ("allow" | "deny") => Command::Answer { allow: word == "allow", key: from_hex(words.next()?)? },
        "cable" => Command::Cable(match words.next()? {
            "on" => true,
            "off" => false,
            _ => return None,
        }),
        "menu" => match words.next()? {
            "open" => Command::OpenMenu,
            _ => return None,
        },
        _ => return None,
    };
    words.next().is_none().then_some(command)
}

/// What a version mismatch says to the host's user. The client that could not talk is gone, so the
/// message names no address.
fn version_notice(error: &ProtoError) -> Option<String> {
    let headset_is_older = error.peer_must_update()?;
    Some(format!(
        "A headset tried to connect with a cranvr app of {} version. {}",
        if headset_is_older { "an older" } else { "a newer" },
        if headset_is_older { UPDATE_HEADSET } else { UPDATE_MAC }
    ))
}

fn accept_loop(listener: HostListener, config: Arc<Config>, shared: Arc<Shared>) {
    loop {
        let pending = match listener.accept() {
            Ok(pending) => pending,
            // A connection that ends before the client's key is a probe: the headset app asks a
            // cabled Mac every second whether it answers (see `cranvr_proto::link::probe_host`).
            Err(ProtoError::Closed) => {
                debug!("Native protocol: a connection ended before its hello");
                continue;
            }
            Err(e) => {
                warn!("Native protocol: a connection failed before its hello: {e}");
                if let Some(message) = version_notice(&e) {
                    status::emit(json!({"type": "network", "problem": "incompatible_client", "message": message}));
                }
                continue;
            }
        };
        let name = display_name(&pending.caps.name);
        if pending.caps.cable && !config.cable.load(Ordering::Relaxed) {
            // The headset probed the cable a moment before the user chose Wi-Fi. It connects over
            // Wi-Fi next, because the preamble of its next probe says that the cable is off.
            info!("Native protocol: {name} at {} came over the USB cable, and the user chose Wi-Fi", pending.peer);
            pending.reject(RejectReason::WiFiOnly);
            continue;
        }
        if shared.session().is_some() {
            warn!("Native protocol: {name} at {} was refused: another headset is connected", pending.peer);
            pending.reject(RejectReason::Busy);
            continue;
        }
        // A client on this Mac needs no pairing. For another one the host looks up the key.
        // The handshake proved that the client holds it, so a client cannot claim the key of
        // another client.
        let known = config.trust_all
            || (pending.peer.ip().is_loopback() && !config.approve_loopback)
            || config.pairings.contains(&pending.client_key);
        if known {
            start_session(&config, &shared, pending, &name);
        } else {
            let (config, shared) = (Arc::clone(&config), Arc::clone(&shared));
            let Some(place) = shared.asking.enter() else {
                warn!("Native protocol: {name} at {} was refused: too many headsets wait for the user", pending.peer);
                pending.reject(RejectReason::Busy);
                continue;
            };
            let spawned = thread::Builder::new().name("native-approval".into()).spawn(move || {
                let _place = place;
                ask_user(&config, &shared, pending, &name)
            });
            if let Err(e) = spawned {
                warn!("Native protocol: no thread for an approval: {e}");
            }
        }
    }
}

/// How an approval ended.
enum Outcome {
    Allowed,
    Denied,
    /// The client closed the connection
    Left,
    NoAnswer,
}

/// Ends the request of a waiting headset on every path out of [`ask_user`], and tells the app.
struct Held<'a> {
    config: &'a Config,
    key: [u8; KEY_LEN],
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.config.pairings.withdraw(&self.key);
        announce(self.config);
    }
}

/// Holds a client that the host does not know until the user answers. The client and the host
/// compute the same code. The app shows it to the user with Allow and Deny. The user compares it with
/// the code in the headset. The thread ends with the answer, a timeout or the client's departure.
fn ask_user(config: &Config, shared: &Arc<Shared>, mut pending: Pending, name: &str) {
    let (key, peer) = (pending.client_key, pending.peer);
    let code = match pending.hold_for_approval(&config.name) {
        Ok(code) => code,
        Err(e) => {
            warn!("Native protocol: {name} at {peer} did not take part in the approval: {e}");
            return;
        }
    };
    if !config.pairings.ask(key, name, peer.ip(), code) {
        warn!("Native protocol: {name} at {peer} was refused: another headset waits for the user");
        pending.reject(RejectReason::Busy);
        return;
    }
    let held = Held { config, key };
    announce(config);
    info!(
        "Native protocol: {name} at {peer} wants to connect and shows the code {}. Allow it in the cranvr app, or type `allow {key}` or `deny {key}` into this host's input.",
        format_code(code),
        key = to_hex(&key)
    );
    let deadline = Instant::now() + config.approval_wait;
    let outcome = loop {
        match config.pairings.answered(&key) {
            Some(true) => break Outcome::Allowed,
            Some(false) => break Outcome::Denied,
            None => {}
        }
        if pending.client_left() {
            break Outcome::Left;
        }
        if Instant::now() >= deadline {
            break Outcome::NoAnswer;
        }
        thread::sleep(APPROVAL_POLL);
    };
    match outcome {
        Outcome::Allowed => {
            config.pairings.add(&key, name);
            drop(held);
            info!("Native protocol: paired {name} at {peer}");
            start_session(config, shared, pending, name);
        }
        Outcome::Denied => {
            info!("Native protocol: the user denied {name} at {peer}");
            pending.reject(RejectReason::Denied);
        }
        Outcome::Left => info!("Native protocol: {name} at {peer} left before the user answered"),
        Outcome::NoAnswer => {
            info!("Native protocol: nobody answered for {name} at {peer} within {} s", config.approval_wait.as_secs());
            pending.reject(RejectReason::NoAnswer);
        }
    }
}

/// How often a waiting approval looks for the user's answer and for the client's departure
const APPROVAL_POLL: Duration = Duration::from_millis(100);

/// Negotiates a stream with a client that may connect, sends the welcome and starts the session.
fn start_session(config: &Config, shared: &Arc<Shared>, pending: Pending, name: &str) {
    let _starting = shared.starting.lock().unwrap();
    // Another headset may have connected while this one waited for the user
    if shared.session().is_some() {
        warn!("Native protocol: {name} at {} was refused: another headset is connected", pending.peer);
        pending.reject(RejectReason::Busy);
        return;
    }
    let mut stream = match negotiate(&config.wishes, &pending.caps) {
        Ok(stream) => stream,
        Err(reason) => {
            warn!("Native protocol: no stream for {name}: {reason:?}");
            pending.reject(reason);
            return;
        }
    };
    stream.audio = config.audio.then_some(cranvr_proto::message::AudioFormat::HOST_CAPTURE);
    let (mode, shard_len) = if config.tcp_media || pending.caps.tcp_only {
        (MediaMode::Tcp, 60_000)
    } else {
        (MediaMode::Udp, DEFAULT_SHARD_LEN)
    };
    let (peer, caps_views, cable, can_record) = (pending.peer, pending.caps.views, pending.caps.cable, pending.caps.microphone);
    let link = match pending.accept(stream.clone(), mode, shard_len) {
        Ok(link) => link,
        Err(e) => {
            warn!("Native protocol: the handshake with {name} failed: {e}");
            return;
        }
    };
    info!("Native protocol: {name} at {peer} connected, {} media, sealed", if link.duplex.has_udp() { "UDP" } else { "TCP" });
    let mut session = Session::new(link, &stream, config.display_lock, cable);
    session.microphone = can_record;
    let session = Arc::new(session);
    session.displays.set(displays::Displays::start(config.desktop.clone(), Arc::clone(&session))).ok();
    if config.audio {
        audio::attach(&session);
    }
    microphone::attach(&session);
    *shared.session.lock().unwrap() = Some(Arc::clone(&session));
    let shared = Arc::clone(shared);
    thread::Builder::new()
        .name("native-session".into())
        .spawn(move || run_session(&shared, &session, stream, caps_views, peer))
        .expect("session thread");
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
    shared.emit(Event::Connected { stream: negotiated, ip: peer.ip(), cable: session.cable });
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
    if let Some(displays) = session.displays.get() {
        displays.stop();
    }
    audio::detach(session);
    microphone::detach(session);
    *shared.session.lock().unwrap() = None;
    // No panel of a gone headset holds a hand back
    shared.emit(Event::Held([false; 2]));
    shared.emit(Event::TrackingOffer(Features::NONE));
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
        // Tracking and input go over UDP and the focus over TCP, so a datagram that the client sent
        // before it lost the focus may arrive after the message. It would press a button that the host
        // let go of. An app without the focus has none to send: the host drops what comes.
        Incoming::Datagram(
            ClientDatagram::Tracking(_)
            | ClientDatagram::InputHold(_)
            | ClientDatagram::Hands(_)
            | ClientDatagram::Body(_)
            | ClientDatagram::Face(_)
            | ClientDatagram::Eyes(_),
        ) if session.paused() => {}
        Incoming::Datagram(ClientDatagram::Tracking(mut t)) => {
            crate::test_input::tracking(&mut t);
            session.history.lock().unwrap().push(t);
            session.lock.lock().unwrap().on_tracking(t.display_time);
            shared.emit(Event::Tracking { sample_ns: t.sample_time });
            // A test hand that the headset tracks comes with each sample, as a headset's hands do
            if let Some(hands) = crate::test_input::hands(&t) {
                if let Some(mut tracked) = sanitized(Tracked::Hands(hands)) {
                    raise_to_host_room(&mut tracked, session.floor.lock().unwrap().unwrap_or(0.0));
                    shared.emit(Event::Tracked(tracked));
                }
            }
        }
        Incoming::Datagram(ClientDatagram::InputHold(InputHold { mut input, held })) => {
            crate::test_input::input(&mut input);
            session.on_input(shared, &input, held)
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
        Incoming::Datagram(
            data @ (ClientDatagram::Hands(_) | ClientDatagram::Body(_) | ClientDatagram::Face(_) | ClientDatagram::Eyes(_)),
        ) => {
            let tracked = match data {
                ClientDatagram::Hands(h) => Tracked::Hands(h),
                ClientDatagram::Body(b) => Tracked::Body(b),
                ClientDatagram::Face(f) => Tracked::Face(f),
                ClientDatagram::Eyes(e) => Tracked::Eyes(e),
                _ => return,
            };
            // A tracked test hand stands in for the client's hands
            let replaced = matches!(tracked, Tracked::Hands(_)) && crate::test_input::replaces_hands();
            if session.wished(&tracked) && !replaced {
                if let Some(mut tracked) = sanitized(tracked) {
                    raise_to_host_room(&mut tracked, session.floor.lock().unwrap().unwrap_or(0.0));
                    shared.emit(Event::Tracked(tracked));
                }
            }
        }
        Incoming::Control(ClientMsg::Focus(focus)) => session.on_focus(shared, focus),
        Incoming::Control(ClientMsg::TrackingOffer(offer)) => {
            // The offer keeps the face's sources, which the game's runtime reports to the game
            let offer = offer.offered();
            *session.offered.lock().unwrap() = offer;
            info!("Native protocol: the headset offers {}", describe(offer));
            shared.emit(Event::TrackingOffer(offer));
            // A game may have asked before the offer came
            session.apply_wish(*shared.wish.lock().unwrap());
        }
        Incoming::Datagram(ClientDatagram::Hello { .. }) | Incoming::Media(_) => {}
        Incoming::Control(ClientMsg::RequestKeyFrame { stream, .. }) if display_index(stream).is_some() => {
            if let Some(displays) = session.displays.get() {
                displays.request_key_frame(stream);
            }
        }
        Incoming::Control(ClientMsg::RequestKeyFrame { .. }) => shared.emit(Event::RequestIdr),
        Incoming::Control(ClientMsg::DisplayWish { stream, wish }) => {
            if let Some(displays) = session.displays.get() {
                displays.on_wish(stream, wish, session);
            }
        }
        Incoming::Control(ClientMsg::Display(request)) => {
            if let Some(displays) = session.displays.get() {
                displays.on_request(request);
            }
        }
        Incoming::Datagram(ClientDatagram::Microphone(sound)) => microphone::on_datagram(session, sound),
        Incoming::Datagram(ClientDatagram::DisplayMouse { stream, mouse }) => {
            if let Some(displays) = session.displays.get() {
                displays.on_mouse(stream, mouse);
            }
        }
        Incoming::Control(ClientMsg::Views(v)) => shared.emit(Event::Views(views_config(&v))),
        Incoming::Control(ClientMsg::Bye) => session.stop(),
        Incoming::Control(ClientMsg::Floor(height)) if height.is_finite() => {
            let mut floor = session.floor.lock().unwrap();
            if floor.is_none_or(|known| (known - height).abs() > 0.005) {
                info!("Native protocol: the headset's floor lies {:.2} m {} its origin", height.abs(), if height < 0.0 { "below" } else { "above" });
            }
            *floor = Some(height);
        }
        Incoming::Control(ClientMsg::Floor(_)) => {}
        Incoming::Control(ClientMsg::Console(request)) => shared.emit(Event::Console(request)),
        Incoming::Control(ClientMsg::HeadsetPage(page)) => {
            if !crate::status::headset_page(&page) {
                warn!("Native protocol: the headset sent a page that is no JSON object");
            }
        }
        Incoming::Control(ClientMsg::Recenter(recenter)) => {
            let ahead_ms = session.clock.lock().unwrap().to_local(recenter.change_time).map(|at| (at as i64 - now as i64) as f64 / 1e6);
            info!(
                "Native protocol: the headset recentered, the new origin takes effect {} from now",
                ahead_ms.map_or("at an unknown time".to_string(), |ms| format!("{ms:.1} ms"))
            );
            shared.emit(Event::Recenter);
        }
        // The proof and the approval belong to the handshake. A later one means nothing.
        Incoming::Control(ClientMsg::Hello(_) | ClientMsg::PairNonce(_)) => {}
    }
}

fn views_config(views: &ProtoViews) -> ViewsConfig {
    let pose = |p: &ProtoPose| RoomPose { orientation: Quat::from_array(p.orientation), position: Vec3::from_array(p.position) };
    let fov = |f: &cranvr_proto::wire::Fov| ViewFov { left: f.left, right: f.right, up: f.up, down: f.down };
    ViewsConfig {
        local_view_transforms: [pose(&views.eye_pose[0]), pose(&views.eye_pose[1])],
        fov: [fov(&views.fov[0]), fov(&views.fov[1])],
    }
}

/// The pose in the headset's room: the host's scene put the room up by `floor` (see `device_motion`),
/// so the pose goes down again.
fn proto_pose(p: &RoomPose, floor: f32) -> ProtoPose {
    let [x, y, z] = p.position.to_array();
    ProtoPose { orientation: p.orientation.to_array(), position: [x, y + floor, z] }
}

/// A pose is not moved further than this. A wrong clock estimate or a pause of the client would
/// otherwise throw the head far away.
const MAX_HORIZON: f32 = 0.08;

/// The time from `valid_at`, when a sample's poses are valid, to `target`, in seconds. `None` for
/// a sample without a display time, or for a target in the past.
fn horizon_seconds(valid_at: Nanos, target: Nanos) -> Option<f32> {
    if valid_at == 0 || target == 0 {
        return None;
    }
    let seconds = (target as i128 - valid_at as i128) as f64 / 1e9;
    Some((seconds as f32).clamp(0.0, MAX_HORIZON))
}

/// Which of a hand's two poses: the grip pose is where the hand holds the controller, the aim pose is
/// where the controller points. The headset reports both for a tracked controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandPose {
    Grip,
    Aim,
}

fn device_of(t: &Tracking, device: Device, pose: HandPose) -> Option<Motion> {
    match (device, pose) {
        (Device::Head, _) => Some(t.head),
        (Device::LeftHand, HandPose::Grip) => t.left_grip,
        (Device::RightHand, HandPose::Grip) => t.right_grip,
        (Device::LeftHand, HandPose::Aim) => t.left_aim,
        (Device::RightHand, HandPose::Aim) => t.right_aim,
    }
}

/// The motion for the host's scene or game. The poses of the headset lie in its room, whose origin
/// is where the head was when the app started. The host's scenes and games want the floor at
/// height 0, so the room moves up by the height of the floor below its origin. `floor` is 0 until
/// the client says where its floor is. The poses of the controllers are the headset's own: the host
/// adds no offset to them.
fn device_motion(motion: Motion, floor: f32) -> DeviceMotion {
    DeviceMotion {
        pose: RoomPose {
            orientation: Quat::from_array(motion.pose.orientation),
            position: Vec3::from_array(motion.pose.position) - Vec3::new(0.0, floor, 0.0),
        },
        linear_velocity: Vec3::from_array(motion.linear_velocity),
        angular_velocity: Vec3::from_array(motion.angular_velocity),
    }
}

/// The wish that goes to a client, or `None` when nothing goes. The wish stays within what the client offers. The host asks again only
/// when the answer changes, and the first ask for nothing is no ask.
fn wish_to_send(wish: Features, offered: Features, asked: Option<Features>) -> Option<Features> {
    // The first 70 joints of the full body are the default set: a headset that offers the full body
    // serves a game that asks for the default one
    let wish = if wish.contains(Features::BODY) && !offered.contains(Features::BODY) && offered.contains(Features::FULL_BODY) {
        wish.union(Features::FULL_BODY)
    } else {
        wish
    };
    let effective = wish.known().intersect(offered);
    (asked != Some(effective) && !(asked.is_none() && effective.is_empty())).then_some(effective)
}

/// The names of a set of tracking features, for the log
fn describe(features: Features) -> String {
    if features.is_empty() {
        "no tracking".to_string()
    } else {
        features.names().join(", ")
    }
}

/// Whether a pose is one that the game's runtime takes: finite numbers and a quaternion that is
/// near a unit one. The runtime closes the link to the host at a value that is not, so a zero
/// quaternion from a tracker that lost a hand would end the game's session.
fn sound_pose(pose: &ProtoPose) -> bool {
    let norm: f32 = pose.orientation.iter().map(|c| c * c).sum::<f32>().sqrt();
    pose.orientation.iter().chain(&pose.position).all(|c| c.is_finite()) && (0.5..=1.5).contains(&norm)
}

fn sound_motion(motion: &Motion) -> bool {
    sound_pose(&motion.pose) && motion.linear_velocity.iter().chain(&motion.angular_velocity).all(|c| c.is_finite())
}

/// The data of a tracking datagram without what the runtime would refuse. A hand with an unsound
/// root counts as untracked, and so does a gaze. A body with one is dropped. `None` drops the
/// datagram.
fn sanitized(tracked: Tracked) -> Option<Tracked> {
    Some(match tracked {
        Tracked::Hands(mut hands) => {
            for hand in [&mut hands.left, &mut hands.right] {
                if hand.as_ref().is_some_and(|hand| !sound_motion(&hand.root)) {
                    *hand = None;
                }
            }
            Tracked::Hands(hands)
        }
        Tracked::Body(mut body) => {
            if !sound_motion(&body.root) {
                return None;
            }
            body.confidence = if body.confidence.is_finite() { body.confidence.clamp(0.0, 1.0) } else { 0.0 };
            Tracked::Body(body)
        }
        Tracked::Eyes(mut eyes) => {
            if eyes.gaze.as_ref().is_some_and(|gaze| !sound_pose(gaze)) {
                eyes.gaze = None;
            }
            Tracked::Eyes(eyes)
        }
        face @ Tracked::Face(_) => face,
    })
}

/// The datagram's data with its poses in the host's room: the floor goes to height 0, as for the head
/// and the controllers (see `device_motion`).
fn raise_to_host_room(tracked: &mut Tracked, floor: f32) {
    let lower = |position: &mut [f32; 3]| position[1] -= floor;
    match tracked {
        Tracked::Hands(hands) => {
            for hand in [&mut hands.left, &mut hands.right].into_iter().flatten() {
                lower(&mut hand.root.pose.position);
            }
        }
        Tracked::Body(body) => lower(&mut body.root.pose.position),
        Tracked::Eyes(eyes) => {
            if let Some(gaze) = &mut eyes.gaze {
                lower(&mut gaze.position);
            }
        }
        Tracked::Face(_) => {}
    }
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
    /// What the game saw last, after the mask
    last: Option<Input>,
    /// The hands that the app holds back from the game (see `hand_mask.rs`)
    mask: HandMask,
    /// The hands that were held at the last input
    held: [bool; 2],
}

/// The path ids of one hand: binary inputs by bit, then the analog inputs.
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
                (PRIMARY, LEFT_X_CLICK_ID),
                (SECONDARY, LEFT_Y_CLICK_ID),
                (MENU, LEFT_MENU_CLICK_ID),
                (STICK_CLICK, LEFT_THUMBSTICK_CLICK_ID),
                (TRIGGER_CLICK, LEFT_TRIGGER_CLICK_ID),
                (GRIP_CLICK, LEFT_SQUEEZE_CLICK_ID),
            ],
            touches: [
                (PRIMARY, LEFT_X_TOUCH_ID),
                (SECONDARY, LEFT_Y_TOUCH_ID),
                (STICK_CLICK, LEFT_THUMBSTICK_TOUCH_ID),
                (TRIGGER_CLICK, LEFT_TRIGGER_TOUCH_ID),
                (THUMB_REST, LEFT_THUMBREST_TOUCH_ID),
            ],
            trigger: LEFT_TRIGGER_VALUE_ID,
            grip: LEFT_SQUEEZE_VALUE_ID,
            stick: [LEFT_THUMBSTICK_X_ID, LEFT_THUMBSTICK_Y_ID],
        }
    } else {
        HandIds {
            clicks: [
                (PRIMARY, RIGHT_A_CLICK_ID),
                (SECONDARY, RIGHT_B_CLICK_ID),
                (MENU, RIGHT_MENU_CLICK_ID),
                (STICK_CLICK, RIGHT_THUMBSTICK_CLICK_ID),
                (TRIGGER_CLICK, RIGHT_TRIGGER_CLICK_ID),
                (GRIP_CLICK, RIGHT_SQUEEZE_CLICK_ID),
            ],
            touches: [
                (PRIMARY, RIGHT_A_TOUCH_ID),
                (SECONDARY, RIGHT_B_TOUCH_ID),
                (STICK_CLICK, RIGHT_THUMBSTICK_TOUCH_ID),
                (TRIGGER_CLICK, RIGHT_TRIGGER_TOUCH_ID),
                (THUMB_REST, RIGHT_THUMBREST_TOUCH_ID),
            ],
            trigger: RIGHT_TRIGGER_VALUE_ID,
            grip: RIGHT_SQUEEZE_VALUE_ID,
            stick: [RIGHT_THUMBSTICK_X_ID, RIGHT_THUMBSTICK_Y_ID],
        }
    }
}

impl InputState {
    /// Lets go of every button that the last input held. Nothing goes out before the first input.
    /// A press that was down stays the user's: the mask forgets it.
    fn release(&mut self) -> Vec<Button> {
        self.mask.reset();
        self.held = [false; 2];
        if self.last.is_none() {
            return Vec::new();
        }
        self.changes(&Input::default())
    }

    /// The buttons that the game sees change when the client sends `input` with the hands that it
    /// holds back.
    fn changes_held(&mut self, input: &Input, held: [bool; 2]) -> Vec<Button> {
        self.held = held;
        let masked = self.mask.apply(input, held);
        self.changes(&masked)
    }

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
    /// The network times of the last frames, from the start of the send to the last packet, in ns
    network: VecDeque<Nanos>,
}

/// The network times that the estimate of a frame's arrival takes
const NETWORK_WINDOW: usize = 64;
/// The share of those frames that came within the estimate
const NETWORK_PERCENTILE: f64 = 0.9;

impl Display {
    fn new(period: Nanos) -> Self {
        Self { next: 0, period, decode: 0.0, network: VecDeque::with_capacity(NETWORK_WINDOW) }
    }

    fn note_network(&mut self, network: Nanos) {
        if self.network.len() == NETWORK_WINDOW {
            self.network.pop_front();
        }
        self.network.push_back(network);
    }

    /// How long a frame takes from the start of its send to its last packet: the 90th percentile of
    /// the last frames. Half the round trip is the estimate before any frame. A frame of the eye
    /// stream moved at about 330 Mbps on a Quest 3's USB network and on a Wi-Fi 6 network (#97), so
    /// a frame of 175 KB took 4.5 ms over the cable and 6.5 ms over Wi-Fi. Half the round trip was
    /// 0.3 and 1.5 ms. A game's frames aimed for points that they could not make: 14 percent of them
    /// came late at 200 Mbps over the cable and 48 percent over Wi-Fi. With this estimate 1.2 percent
    /// came late at 120 Mbps over the cable and none over Wi-Fi.
    fn network_ns(&self, half_round_trip: Nanos) -> Nanos {
        if self.network.is_empty() {
            return half_round_trip;
        }
        let mut sorted: Vec<Nanos> = self.network.iter().copied().collect();
        sorted.sort_unstable();
        sorted[((sorted.len() as f64 * NETWORK_PERCENTILE).ceil() as usize).clamp(1, sorted.len()) - 1]
    }
}

/// The first point of the grid `origin + k * period` at or after `t`, for any whole `k`. The
/// origin is the newest display time that the headset reported. It lies ahead of a frame that is
/// quick, and the display shows frames at the points before it too. Taking the origin for every
/// such frame gave frames that the host sent within one report's interval the same target, a
/// point that the later frames could no longer make.
fn grid_at_or_after(origin: Nanos, period: Nanos, t: Nanos) -> Nanos {
    if period == 0 {
        return origin.max(t);
    }
    let (origin, period, t) = (i128::from(origin), i128::from(period), i128::from(t));
    let steps = (t - origin + period - 1).div_euclid(period);
    (origin + steps * period).max(0) as Nanos
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
    /// From the end of the decode to the take by the headset's frame loop, ms
    decode_queue: f64,
    /// From the take to the display time of the headset frame that shows the frame, ms
    vsync_wait: f64,
    sent_bytes: u64,
    sent_frames: u32,
    peak_frame_bytes: usize,
    data_packets: u64,
    parity_packets: u64,
    loss: LossReport,
    /// How far the host moved the poses of the frames that it sent, in ms. The count follows
    horizon: f64,
    horizon_frames: u32,
}

struct SentFrame {
    frame: u32,
    /// The host's clock when the send began
    sent: Nanos,
}

/// The focus of the headset's app and when it began.
struct FocusState {
    focus: Focus,
    since: Instant,
    /// The session's count of sent frames when the focus began
    frames_at: u64,
}

/// The host's end of a connected client.
struct Session {
    link: HostLink,
    /// The client reaches the host over a USB cable: `adb reverse` or the headset's USB network
    cable: bool,
    clock: Mutex<ClockSync>,
    history: Mutex<History>,
    input: Mutex<InputState>,
    display: Mutex<Display>,
    parity: Mutex<ParityController>,
    stats: Mutex<Stats>,
    /// What the frames did since the last look of the bitrate's control. The display lock keeps the
    /// counts of late and replaced frames.
    window: Mutex<LinkWindow>,
    /// The UDP sends that the system refused, at the last look of the bitrate's control
    refused_seen: AtomicU64,
    sent: Mutex<VecDeque<SentFrame>>,
    /// The display lock's planner. It learns the display grid in every case. The frame loop follows
    /// its plans only when `lock_on` is set.
    lock: Mutex<DisplayLock>,
    lock_on: bool,
    /// The height of the headset's floor in its room, once the client says it
    floor: Mutex<Option<f32>>,
    /// How much of the headset the app has. A client that never says is focused.
    focus: Mutex<FocusState>,
    /// The video frames that the session sent, to tell what a pause cost
    frames_sent: AtomicU64,
    /// The tracking features that the headset offered: none until it says so
    offered: Mutex<Features>,
    /// The tracking features that the host asked the headset for last. `None` before the first ask.
    asked: Mutex<Option<Features>>,
    /// The displays that the client shows as panels: their streams and pointers. They start after
    /// the session exists, because their threads share the session.
    displays: std::sync::OnceLock<Arc<displays::Displays>>,
    /// The client can record its microphone for games
    microphone: bool,
    next_frame: AtomicU32,
    heard: Mutex<Instant>,
    stopped: Mutex<bool>,
    wake: Condvar,
}

impl Session {
    fn new(link: HostLink, stream: &cranvr_proto::message::StreamParams, lock_on: bool, cable: bool) -> Self {
        let period = (1e9 / stream.refresh_hz.max(1.0) as f64) as Nanos;
        let udp = link.duplex.has_udp();
        Self {
            link,
            cable,
            clock: Mutex::new(ClockSync::new()),
            history: Mutex::new(History::default()),
            input: Mutex::new(InputState::default()),
            display: Mutex::new(Display::new(period)),
            parity: Mutex::new(ParityController::new(if udp { PARITY_MIN } else { 0.0 }, PARITY_MAX)),
            stats: Mutex::new(Stats::default()),
            window: Mutex::new(LinkWindow { first: true, ..LinkWindow::default() }),
            refused_seen: AtomicU64::new(0),
            sent: Mutex::new(VecDeque::new()),
            lock: Mutex::new(DisplayLock::new(period)),
            lock_on,
            floor: Mutex::new(None),
            focus: Mutex::new(FocusState { focus: Focus::Focused, since: Instant::now(), frames_at: 0 }),
            frames_sent: AtomicU64::new(0),
            offered: Mutex::new(Features::NONE),
            asked: Mutex::new(None),
            displays: std::sync::OnceLock::new(),
            microphone: false,
            next_frame: AtomicU32::new(0),
            heard: Mutex::new(Instant::now()),
            stopped: Mutex::new(false),
            wake: Condvar::new(),
        }
    }

    /// The controllers' input came, with the hands that the app holds back from the game. The
    /// buttons that the game sees change go to the host's loop. A change of the held hands goes
    /// there too, for the hand that a pinch makes a controller of.
    fn on_input(&self, shared: &Shared, input: &Input, held: [bool; 2]) {
        let (buttons, held_before) = {
            let mut state = self.input.lock().unwrap();
            let before = state.held;
            (state.changes_held(input, held), before)
        };
        if held != held_before {
            info!(
                "Native protocol: the app holds back {} from the game",
                match held {
                    [false, false] => "no hand",
                    [true, false] => "the left hand",
                    [false, true] => "the right hand",
                    [true, true] => "both hands",
                }
            );
            shared.emit(Event::Held(held));
        }
        if !buttons.is_empty() {
            shared.emit(Event::Buttons(buttons));
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

    /// Whether the app on the headset has no input focus: the host sends no pictures and no sound.
    fn paused(&self) -> bool {
        !self.focus.lock().unwrap().focus.is_focused()
    }

    /// The app on the headset says how much of the headset it has. The client stops its tracking
    /// and input while it is not focused, so a button that the user held would stay down in the
    /// game: the host lets go of every button first. It then tells the frame loop, which stops the
    /// stream. The client keeps its connection and its pings, so the session lives on.
    fn on_focus(&self, shared: &Shared, focus: Focus) {
        let (before, lasted, frames) = {
            let mut state = self.focus.lock().unwrap();
            if state.focus == focus {
                return;
            }
            let sent = self.frames_sent.load(Ordering::Relaxed);
            let before = (state.focus, state.since.elapsed(), sent - state.frames_at);
            *state = FocusState { focus, since: Instant::now(), frames_at: sent };
            before
        };
        info!(
            "Native protocol: the headset's app {}. It was {} for {:.1} s, and the host sent {frames} video frames in that time",
            match focus {
                Focus::Focused => "has the input focus again: the stream goes on",
                Focus::Visible => "is visible without the input focus: the stream pauses",
                Focus::Hidden => "is hidden by the headset: the stream pauses",
            },
            match before {
                Focus::Focused => "focused",
                Focus::Visible => "visible without the input focus",
                Focus::Hidden => "hidden",
            },
            lasted.as_secs_f64(),
        );
        if !focus.is_focused() {
            let released = self.input.lock().unwrap().release();
            if !released.is_empty() {
                shared.emit(Event::Buttons(released));
            }
            // An app without the focus points at no panel
            shared.emit(Event::Held([false; 2]));
        }
        // The headset's decoders may have dropped what came while the app was away, so the next
        // pictures start from key frames
        if focus.is_focused() {
            if let Some(displays) = self.displays.get() {
                displays.request_key_frames();
            }
        }
        shared.emit(Event::Focus(focus));
    }

    /// Asks the headset for the tracking features that a game uses, within what it offers. The host
    /// asks again when the set changes, and sends nothing when the answer stays the same.
    fn apply_wish(&self, wish: Features) {
        let offered = *self.offered.lock().unwrap();
        let mut asked = self.asked.lock().unwrap();
        let Some(effective) = wish_to_send(wish, offered, *asked) else { return };
        *asked = Some(effective);
        match self.link.duplex.send_control(&HostMsg::TrackingWish(effective)) {
            Ok(()) => info!("Native protocol: the headset is asked for {}", describe(effective)),
            Err(e) => warn!("Native protocol: the tracking wish did not go out: {e}"),
        }
    }

    /// Whether the host asked the headset for the feature that `tracked` belongs to. A datagram of a
    /// feature that was not asked for is dropped.
    fn wished(&self, tracked: &Tracked) -> bool {
        let asked = self.asked.lock().unwrap().unwrap_or(Features::NONE);
        match tracked {
            Tracked::Body(_) => !asked.intersect(Features::BODY.union(Features::FULL_BODY)).is_empty(),
            other => asked.contains(other.feature()),
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

    /// The display time that a frame sent now can make, on the headset's clock: the first point of
    /// the display's grid after the frame has crossed the network (as the frames before it did),
    /// decoded and waited for the headset's take, which comes one lead before the point. A target
    /// that leaves the lead out lies before the display time of the headset frame that takes the
    /// frame, so the headset's queue counts the frame as late and skips frames. The planner gives successive frames
    /// successive points (see `DisplayLock::point_for`). 0 while the clocks are not in sync.
    fn target_display(&self, now: Nanos) -> Nanos {
        let (Some(client_now), Some(estimate)) = (self.client_time(now), self.clock.lock().unwrap().estimate()) else { return 0 };
        let ready = {
            let display = self.display.lock().unwrap();
            if display.next == 0 {
                return 0;
            }
            client_now + display.network_ns(estimate.rtt_ns / 2) + display.decode as Nanos + DISPLAY_MARGIN.as_nanos() as Nanos
        };
        self.lock.lock().unwrap().point_for(ready).unwrap_or(0)
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
            if let Some(sent) = sent_on_client {
                display.note_network(t.received.saturating_sub(sent));
            }
        }
        let ms = |ns: u64| ns as f64 / 1e6;
        let mut stats = self.stats.lock().unwrap();
        stats.started.get_or_insert_with(Instant::now);
        stats.frames += 1;
        stats.total += ms(t.display_time.saturating_sub(t.tracking_sample));
        stats.decode += ms(t.decode_end.saturating_sub(t.received));
        stats.decode_queue += ms(t.picked.saturating_sub(t.decode_end));
        stats.vsync_wait += ms(t.display_time.saturating_sub(t.picked));
        if let Some(sent) = sent_on_client {
            let network = ms(t.received.saturating_sub(sent));
            stats.pose_to_send += ms(sent.saturating_sub(t.tracking_sample));
            stats.network += network;
            stats.network_frames += 1;
            let mut window = self.window.lock().unwrap();
            window.network_frames += 1;
            window.network_ms += network;
            window.network_max_ms = window.network_max_ms.max(network);
        }
        drop(stats);
        self.lock.lock().unwrap().on_report(&Report {
            sample: t.tracking_sample,
            decode_end: t.decode_end,
            picked: t.picked,
            display_time: t.display_time,
        });
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

impl Native {
    /// The pose of a device at a tracking sample. With a `target` display time the pose moves by its
    /// velocities from the sample's display time to the target, as a frame shows it.
    fn pose(&self, device: Device, kind: HandPose, sample_ns: u64, target: Option<u64>) -> Option<DeviceMotion> {
        let session = self.shared.session()?;
        let history = session.history.lock().unwrap();
        let t = history.find(sample_ns)?;
        let mut motion = device_of(t, device, kind)?;
        // The pose is valid at the sample's display time. The frame shows at the target.
        if let Some(seconds) = target.and_then(|target| horizon_seconds(t.display_time, target)) {
            motion.pose = extrapolate(&motion, seconds);
        }
        let floor = session.floor.lock().unwrap().unwrap_or(0.0);
        Some(device_motion(motion, floor))
    }
}

impl Headset for Native {
    fn until_next_vsync(&self) -> Option<Duration> {
        self.shared.session()?.until_next_vsync()
    }

    fn motion(&self, device: Device, sample_ns: u64) -> Option<DeviceMotion> {
        self.pose(device, HandPose::Grip, sample_ns, None)
    }

    fn motion_at(&self, device: Device, sample_ns: u64, target: u64) -> Option<DeviceMotion> {
        self.pose(device, HandPose::Grip, sample_ns, Some(target))
    }

    fn aim_motion(&self, device: Device, sample_ns: u64) -> Option<DeviceMotion> {
        self.pose(device, HandPose::Aim, sample_ns, None)
    }

    fn aim_motion_at(&self, device: Device, sample_ns: u64, target: u64) -> Option<DeviceMotion> {
        self.pose(device, HandPose::Aim, sample_ns, Some(target))
    }

    fn frame_plan(&self) -> Option<FramePlan> {
        let session = self.shared.session()?;
        if !session.lock_on {
            return None;
        }
        let now = session.client_time(now_ns())?;
        let plan = session.lock.lock().unwrap().plan(now)?;
        Some(FramePlan { start_in: Duration::from_nanos(plan.start.saturating_sub(now)), target: plan.target, start: plan.start })
    }

    fn start_early(&self, plan: &FramePlan) -> bool {
        let Some(session) = self.shared.session() else { return false };
        let Some(now) = session.client_time(now_ns()) else { return false };
        let early = session.lock.lock().unwrap().starts_early(now, plan.start);
        early
    }

    fn frame_started(&self, target: u64, late: Duration) {
        if let Some(session) = self.shared.session() {
            session.lock.lock().unwrap().commit(target, late.as_nanos() as u64);
        }
    }

    /// The native client sends one pose per refresh. Its poses jitter by a few milliseconds, and
    /// a pose that comes early would lose its frame with the default share.
    fn pacer_ready(&self) -> f32 {
        0.5
    }

    fn wants_eye_poses(&self) -> bool {
        true
    }

    fn repeats_samples(&self) -> bool {
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
        // A frame that followed a plan has its target. Another frame aims for the first display
        // time that it can make.
        let target_display = frame.plan.map_or_else(|| session.target_display(now), |plan| plan.target);
        let parity = session.parity.lock().unwrap().clone();
        let floor = session.floor.lock().unwrap().unwrap_or(0.0);
        let rects: Vec<Rect> = frame.slices.iter().map(|s| Rect { x: s.rect[0], y: s.rect[1], w: s.rect[2], h: s.rect[3] }).collect();
        // A test of the bitrate's control drops the frames that a slower link would drop. Such a frame
        // counts as sent and never reaches the headset, as a lost one.
        let lost = !crate::test_bitrate::passes_cap(frame.slices.iter().map(|s| s.data.len()).sum());
        let send_started = Instant::now();
        let mut sent_bytes = 0;
        let (mut data_packets, mut parity_packets) = (0u64, 0u64);
        for (i, slice) in frame.slices.iter().enumerate().filter(|_| !lost) {
            let meta = FrameMeta {
                stream: 0,
                frame: index,
                slice: i as u8,
                key: frame.idr,
                tracking_sample: frame.sample_ns,
                target_display,
                sent: now,
                eye_poses: [proto_pose(&frame.eye_poses[0], floor), proto_pose(&frame.eye_poses[1], floor)],
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
        // The planner counts the frame. A frame without a plan teaches it the lead only: the
        // frame's measured need would start at its send and read shorter than a planned frame's.
        if target_display != 0 {
            match frame.plan {
                Some(plan) => session.lock.lock().unwrap().sent(frame.sample_ns, target_display, plan.start),
                None => session.lock.lock().unwrap().sent_unplanned(frame.sample_ns, target_display),
            }
            if let Some(t) = session.history.lock().unwrap().find(frame.sample_ns) {
                if let Some(seconds) = horizon_seconds(t.display_time, target_display) {
                    let mut stats = session.stats.lock().unwrap();
                    stats.horizon += f64::from(seconds) * 1e3;
                    stats.horizon_frames += 1;
                }
            }
        }
        session.frames_sent.fetch_add(1, Ordering::Relaxed);
        {
            let send_ms = send_started.elapsed().as_secs_f64() * 1e3;
            let mut window = session.window.lock().unwrap();
            window.sent_frames += 1;
            window.sent_bytes += sent_bytes as u64;
            window.send_ms += send_ms;
            window.send_max_ms = window.send_max_ms.max(send_ms);
        }
        let mut stats = session.stats.lock().unwrap();
        stats.sent_frames += 1;
        stats.sent_bytes += sent_bytes as u64;
        stats.peak_frame_bytes = stats.peak_frame_bytes.max(sent_bytes);
        stats.data_packets += data_packets;
        stats.parity_packets += parity_packets;
    }

    fn frame_submitted(&self, _sample_ns: u64) {}

    fn take_link_window(&self) -> Option<LinkWindow> {
        let session = self.shared.session()?;
        let mut window = std::mem::take(&mut *session.window.lock().unwrap());
        let counts = session.lock.lock().unwrap().take_counts();
        (window.frames, window.late, window.replaced) = (counts.frames, counts.late, counts.replaced);
        let refused = session.link.duplex.dropped_sends();
        window.refused_sends = refused.saturating_sub(session.refused_seen.swap(refused, Ordering::Relaxed));
        window.cable = session.cable;
        Some(window)
    }

    fn link_is_cable(&self) -> Option<bool> {
        Some(self.shared.session()?.cable)
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

    fn set_tracking_wish(&self, wish: Features) {
        *self.shared.wish.lock().unwrap() = wish;
        if let Some(session) = self.shared.session() {
            session.apply_wish(wish);
        }
    }

    fn send_menu(&self, menu: &str) {
        if let Some(session) = self.shared.session() {
            if let Err(e) = session.link.duplex.send_control(&HostMsg::Menu(menu.to_owned())) {
                warn!("Native protocol: the console's menu did not go out: {e}");
            }
        }
    }

    fn send_menu_status(&self, status: &str) {
        if let Some(session) = self.shared.session() {
            if let Err(e) = session.link.duplex.send_control(&HostMsg::MenuStatus(status.to_owned())) {
                warn!("Native protocol: the console's state did not go out: {e}");
            }
        }
    }

    fn send_headset_request(&self, request: &str) -> bool {
        let Some(session) = self.shared.session() else { return false };
        match session.link.duplex.send_control(&HostMsg::HeadsetRequest(request.to_owned())) {
            Ok(()) => true,
            Err(e) => {
                warn!("Native protocol: the app's request to the headset did not go out: {e}");
                false
            }
        }
    }

    fn open_menu(&self) -> bool {
        let Some(session) = self.shared.session() else { return false };
        match session.link.duplex.send_control(&HostMsg::OpenMenu) {
            Ok(()) => true,
            Err(e) => {
                warn!("Native protocol: the request to open the hand menu did not go out: {e}");
                false
            }
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
        let slots = session.lock.lock().unwrap().take_stats();
        let share = |n: u32| 100.0 * f64::from(n) / f64::from(slots.frames().max(1));
        Some(json!({
            "lock": session.lock_on,
            "slot_on_target_pct": share(slots.on_time),
            "slot_late_pct": share(slots.late),
            "slot_early_pct": share(slots.early),
            "slot_replaced_pct": share(slots.replaced),
            "lock_skipped_points": slots.skipped_points,
            "lock_late_start_ms": slots.late_start_ms,
            "lock_late_start_max_ms": slots.late_start_max_ms,
            "lock_need_ms": slots.need_ms,
            "lock_need_p10_ms": slots.need_p10_ms,
            "lock_need_p50_ms": slots.need_p50_ms,
            "lock_lead_ms": slots.lead_ms,
            "lock_margin_ms": slots.margin_ms,
            "horizon_ms": stats.horizon / f64::from(stats.horizon_frames.max(1)),
            "frames": stats.frames,
            "client_fps": fps,
            "video_mbps": stats.sent_bytes as f64 * 8.0 / seconds / 1e6,
            "peak_frame_mbps": stats.peak_frame_bytes as f64 * 8.0 * fps / 1e6,
            "total_ms": stats.total / n,
            "pose_to_send_ms": stats.pose_to_send / stats.network_frames.max(1) as f64,
            "network_ms": stats.network / stats.network_frames.max(1) as f64,
            "decode_ms": stats.decode / n,
            "decode_queue_ms": stats.decode_queue / n,
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
