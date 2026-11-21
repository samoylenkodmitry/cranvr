//! The host side of BasaltVR's runtime control protocol (version 9, MIT). The BasaltVR OpenXR
//! runtime runs inside Wine as the game's OpenXR runtime. It connects here over loopback TCP,
//! shares its swapchain images as IOSurface IDs and reports each finished frame. The host sends
//! back the head pose and each controller's grip pose, and the controller's aim pose when the
//! headset has one separate from the grip pose. Wire format: `runtime/pe/basalt_control_protocol.h`
//! in BasaltVR.
//!
//! The aim pose is cranvr's extension (runtime patch 0004, message `CONTROLLER_MOTION_AIM`). The
//! runtime of the game support that this host belongs to has the patch, and the host sends the
//! message to every runtime. A hand without an aim pose gets the plain grip message, so the game's
//! aim space lies at the grip pose.
//!
//! Tracking of hands, body, face and eyes is cranvr's extension too (runtime patch 0005, messages 19
//! to 23). The runtime tells the host which features its game uses (`TRACKING_REQUEST`), and the host
//! sends their data. The host also writes `tracking-features.txt` next to the control file: the
//! features that the connected headset offers. The runtime reads it before a game creates its OpenXR
//! instance, because the game asks for the instance's extensions first, and lists the extensions of
//! those features only.
//!
//! The runtime finds the host through a control file: "<port> <token> <eye width> <eye height>".
//! The launcher passes the file's Windows path to the game in the `BASALT_CONTROL` variable.

use crate::pose::DeviceMotion;
use cranvr_proto::tracking::{Body, Eyes, Face, Features, Joint, BODY_JOINTS, FACE_CONFIDENCES, FACE_WEIGHTS, FULL_BODY_JOINTS, HAND_JOINTS};
use log::{info, warn};
use cranvr_common::inputs::QUEST_TOUCH_PROFILE_ID;
use cranvr_vt::PixelBuffer;
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

pub const PROTOCOL_VERSION: u16 = 9;
const TOKEN_LENGTH: usize = 32;
const MAX_MESSAGE: u32 = 4096;
/// The longest silence toward the runtime; it gives up after 5 seconds
const KEEPALIVE: Duration = Duration::from_secs(1);
/// The file next to the control file with the features that the headset offers, as words
pub const TRACKING_FILE: &str = "tracking-features.txt";
/// The runtime accepts per-eye sizes up to this (BASALT_CONTROL_MAX_VIEW_*; cranvr's runtime
/// patch raises them to the Quest 3 panel size).
pub const MAX_VIEW: (u32, u32) = (2064, 2208);

pub(crate) mod msg {
    pub const HELLO: u16 = 1;
    pub const HELLO_ACK: u16 = 2;
    pub const CREATE_SWAPCHAIN: u16 = 3;
    pub const SWAPCHAIN_ACK: u16 = 4;
    pub const DESTROY_SWAPCHAIN: u16 = 5;
    pub const DESTROY_SWAPCHAIN_ACK: u16 = 6;
    pub const FRAME_READY: u16 = 7;
    pub const TEARDOWN: u16 = 8;
    pub const HEAD_POSE: u16 = 9;
    pub const CONTROLLER_MOTION: u16 = 10;
    pub const BUTTON_STATE: u16 = 11;
    pub const HAPTIC: u16 = 12;
    pub const INTERACTION_PROFILE: u16 = 13;
    pub const INPUT_RESET: u16 = 14;
    pub const VIEW_CONFIG: u16 = 15;
    pub const CONTROLLER_MASK: u16 = 16;
    pub const FRAME_CONSUMED: u16 = 17;
    /// cranvr's runtime patch 0004: the grip and the aim pose of a controller in one message
    pub const CONTROLLER_MOTION_AIM: u16 = 18;
    /// cranvr's runtime patch 0005: the features that the game uses (runtime to host)
    pub const TRACKING_REQUEST: u16 = 19;
    pub const HAND_TRACKING: u16 = 20;
    pub const BODY_TRACKING: u16 = 21;
    pub const FACE_TRACKING: u16 = 22;
    pub const EYE_GAZE: u16 = 23;
}

/// One eye of a finished game frame.
#[derive(Clone, Copy, Debug)]
pub struct GameView {
    pub swapchain: u64,
    pub image: u32,
    /// The layer of an array swapchain. The runtime copies each layer into a surface of its own
    /// and sends the surface's place in `image` (image × layers + layer, BasaltVR's
    /// prepareSwapchainViewForExport), with the layer 0.
    pub layer: u32,
    /// x, y, width, height in pixels of the swapchain image
    pub rect: [i32; 4],
    /// tangents of the left, right, up and down angles the game rendered with
    pub fov_tan: [f32; 4],
}

#[derive(Clone, Debug)]
pub struct GameFrame {
    pub number: u64,
    /// The tracking sample the game rendered for
    pub tracking_ns: u64,
    pub views: [GameView; 2],
}

pub enum GameEvent {
    /// `program` is the game's program path, which a runtime with cranvr's patch 0002 writes to
    /// game-program.txt before it connects
    Connected { renderer: String, program: Option<String> },
    Swapchain { id: u64, images: Vec<PixelBuffer> },
    SwapchainGone { id: u64 },
    Frame(GameFrame),
    Disconnected,
    /// Not from the game: the headset sent a new pose. The host's own scene draws for it at once.
    HeadPose,
}

/// Whether a pulse of `amplitude` goes to the headset. A pulse with no amplitude only stops what the hand does, and
/// a game may send one in every frame (a Unity game stops the haptics of its hands about 130 times a second): only
/// the first of such pulses after a pulse with an amplitude goes on, so the hand is stopped once.
fn forwards_haptic(quiet: &mut [bool; 2], hand: u32, amplitude: f32) -> bool {
    let Some(was_quiet) = quiet.get_mut(hand as usize) else {
        return false;
    };
    let silent = amplitude.is_nan() || amplitude <= 0.0;
    let forward = !(silent && *was_quiet);
    *was_quiet = silent;
    forward
}

pub struct Haptic {
    pub hand: u32,
    pub duration: Duration,
    pub frequency: f32,
    pub amplitude: f32,
}

/// What the host handed to the game's runtime and got back from it, since the last `take_counts`.
/// The host's stats line shows it, so a log says whether the controllers reached the game.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputCounts {
    /// Controller poses sent, for the left and the right hand
    pub poses: [u32; 2],
    /// Of those, the poses that carried an aim pose
    pub aim_poses: [u32; 2],
    /// The newest mask of tracked controllers: bit 0 left, bit 1 right
    pub mask: u32,
    /// Button and axis values sent
    pub buttons: u32,
    /// Haptic pulses that the game asked for
    pub haptics: u32,
    /// Tracking messages sent: hands, body, face and eyes
    pub tracking: [u32; 4],
    /// The features that the game asks for now
    pub requested: Features,
    /// Where each hand's grip pose was at its last message, in millimeters, and how far it travelled since the last call.
    /// A controller that lies on a table or reports a pose at the room's origin travels nothing.
    pub at_mm: [[i32; 3]; 2],
    pub moved_mm: [u32; 2],
}

/// The last grip position of a hand and the distance that it travelled since the stats line.
#[derive(Clone, Copy, Default)]
struct HandTravel {
    at: Option<[f32; 3]>,
    moved: f32,
}

#[derive(Default)]
struct InputCounters {
    poses: [AtomicU32; 2],
    aim_poses: [AtomicU32; 2],
    mask: AtomicU32,
    buttons: AtomicU32,
    haptics: AtomicU32,
    tracking: [AtomicU32; 4],
    travel: Mutex<[HandTravel; 2]>,
}

/// What the host knows about the headset, sent to each new runtime connection.
#[derive(Clone, Copy, Default)]
struct Calibration {
    /// ipd, then per eye: left, right, up, down angles in radians
    view_config: Option<[f32; 9]>,
}

pub struct RuntimeLink {
    events: mpsc::Sender<GameEvent>,
    writer: Mutex<Option<TcpStream>>,
    /// When the host last sent the runtime anything
    last_sent: Mutex<Instant>,
    calibration: Mutex<Calibration>,
    counters: InputCounters,
    /// The tracking features that the connected game asks for: the bits of `Features`
    requested: AtomicU8,
    /// The tracking features that the connected headset offers, as the file tells the runtime
    offered: AtomicU8,
    /// The runtime dropped the controllers' interaction profiles at an INPUT_RESET. The next pose
    /// of a controller sends them again.
    profiles_dropped: AtomicBool,
    control_file: PathBuf,
    port: u16,
    token: String,
}

impl RuntimeLink {
    /// Listens on 127.0.0.1 and writes the control file. Game events arrive on the returned
    /// receiver; haptics go to `on_haptic`.
    pub fn start(
        control_file: &Path,
        eye_size: (u32, u32),
        fps: u32,
        on_haptic: impl Fn(Haptic) + Send + 'static,
    ) -> std::io::Result<(Arc<Self>, mpsc::Receiver<GameEvent>)> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let (events, receiver) = mpsc::channel();
        let link = Arc::new(Self {
            events: events.clone(),
            writer: Mutex::new(None),
            last_sent: Mutex::new(Instant::now()),
            calibration: Mutex::new(Calibration::default()),
            counters: InputCounters::default(),
            requested: AtomicU8::new(0),
            offered: AtomicU8::new(0),
            profiles_dropped: AtomicBool::new(false),
            control_file: control_file.to_owned(),
            port,
            token: random_token()?,
        });
        link.write_control_file(eye_size, fps)?;
        // A file from an earlier run names a headset that is gone
        fs::remove_file(link.control_dir().join(TRACKING_FILE)).ok();
        {
            let link = Arc::clone(&link);
            thread::Builder::new().name("runtime-link".into()).spawn(move || {
                for stream in listener.incoming().flatten() {
                    if let Err(e) = link.serve(stream, &events, &on_haptic) {
                        warn!("Game runtime connection: {e}");
                    }
                    *link.writer.lock().unwrap() = None;
                    link.requested.store(0, Ordering::Relaxed);
                    events.send(GameEvent::Disconnected).ok();
                }
            })?;
        }
        {
            // The runtime gives up on the host when it hears nothing for 5 seconds
            // (kSocketTimeoutMilliseconds in BasaltVR's control.cpp). It then stops the game's
            // rendering but keeps the connection open, so the game sent no frames after the
            // headset left for 6 or 11 seconds. While no pose flows, the host sends INPUT_RESET,
            // which the runtime takes as "the headset's controls are gone".
            let link = Arc::clone(&link);
            thread::Builder::new().name("runtime-keepalive".into()).spawn(move || loop {
                thread::sleep(KEEPALIVE / 2);
                if link.last_sent.lock().unwrap().elapsed() >= KEEPALIVE {
                    // The reset also clears the profiles. Without them Unity removes the
                    // controllers, and a game draws no hands when the poses come back.
                    link.send(msg::INPUT_RESET, &[]);
                    link.profiles_dropped.store(true, Ordering::Relaxed);
                }
            })?;
        }
        Ok((link, receiver))
    }

    /// Rewrites the control file with the per-eye size the game should render. Even, and within
    /// what the runtime accepts.
    /// The folder of the control file, where the runtime also reads `game-sizes.txt`
    pub fn control_dir(&self) -> &Path {
        self.control_file.parent().unwrap_or(Path::new("."))
    }

    pub fn write_control_file(&self, eye_size: (u32, u32), fps: u32) -> std::io::Result<()> {
        let w = eye_size.0.min(MAX_VIEW.0) & !1;
        let h = eye_size.1.min(MAX_VIEW.1) & !1;
        // The refresh rate field needs cranvr's runtime patch; the stock runtime rejects it
        fs::write(&self.control_file, format!("{} {} {w} {h} {fps}\n", self.port, self.token))?;
        fs::set_permissions(&self.control_file, fs::Permissions::from_mode(0o600))
    }

    /// Tells the frame loop that the headset sent a new pose.
    pub fn new_pose(&self) {
        self.events.send(GameEvent::HeadPose).ok();
    }

    pub fn control_file(&self) -> &Path {
        &self.control_file
    }

    pub fn is_connected(&self) -> bool {
        self.writer.lock().unwrap().is_some()
    }

    fn serve(&self, mut stream: TcpStream, events: &mpsc::Sender<GameEvent>, on_haptic: &impl Fn(Haptic)) -> Result<(), String> {
        stream.set_nodelay(true).ok();
        // Whether the last pulse that went to the headset left each hand quiet
        let mut quiet = [true; 2];
        // HELLO first, with the token from the control file
        let (kind, payload) = read_message(&mut stream)?;
        let mut r = Reader(&payload);
        if kind != msg::HELLO {
            return Err(format!("expected HELLO, got message {kind}"));
        }
        let version = r.u16()?;
        let token = r.bytes(TOKEN_LENGTH)?;
        if token != self.token.as_bytes() {
            return Err("wrong token".into());
        }
        if version != PROTOCOL_VERSION {
            return Err(format!("runtime speaks protocol {version}, the host speaks {PROTOCOL_VERSION}"));
        }
        let _session = r.u64()?;
        let len = r.u16()? as usize;
        let renderer = String::from_utf8_lossy(r.bytes(len)?).into_owned();
        // The file names this game only: the host takes it away, so a game whose runtime writes
        // none stays unnamed
        let program_file = self.control_dir().join("game-program.txt");
        let program = fs::read_to_string(&program_file).ok().map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
        fs::remove_file(&program_file).ok();
        self.requested.store(0, Ordering::Relaxed);
        info!("Game runtime connected: {renderer}, {}", program.as_deref().unwrap_or("program not named"));

        *self.last_sent.lock().unwrap() = Instant::now();
        *self.writer.lock().unwrap() = Some(stream.try_clone().map_err(|e| e.to_string())?);
        self.send(msg::HELLO_ACK, &PROTOCOL_VERSION.to_le_bytes());
        if let Some(view) = self.calibration.lock().unwrap().view_config {
            self.send_view_config(view);
        }
        self.profiles_dropped.store(false, Ordering::Relaxed);
        self.send_profiles();
        events.send(GameEvent::Connected { renderer, program }).ok();

        let mut generation = 0u32;
        loop {
            let (kind, payload) = read_message(&mut stream)?;
            let mut r = Reader(&payload);
            match kind {
                msg::CREATE_SWAPCHAIN => {
                    let id = r.u64()?;
                    let (width, height, _format, count) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
                    let mut images = Vec::new();
                    for _ in 0..count {
                        let surface_id = r.u32()?;
                        // IOSurface IDs are global and reused: check what the ID points at
                        let image = PixelBuffer::from_iosurface_id(surface_id)
                            .filter(|b| b.width() == width as usize && b.height() == height as usize)
                            .ok_or(format!("IOSurface {surface_id} is missing or has the wrong size"))?;
                        images.push(image);
                    }
                    info!("Game swapchain {id}: {count} images of {width}x{height}");
                    events.send(GameEvent::Swapchain { id, images }).ok();
                    generation += 1;
                    let mut p = id.to_le_bytes().to_vec();
                    p.extend_from_slice(&generation.to_le_bytes());
                    self.send(msg::SWAPCHAIN_ACK, &p);
                }
                msg::DESTROY_SWAPCHAIN => {
                    let id = r.u64()?;
                    events.send(GameEvent::SwapchainGone { id }).ok();
                    self.send(msg::DESTROY_SWAPCHAIN_ACK, &id.to_le_bytes());
                }
                msg::FRAME_READY => {
                    let number = r.u64()?;
                    let _display_time = r.u64()?;
                    let tracking_ns = r.u64()?;
                    let count = r.u32()?;
                    if count != 2 {
                        return Err(format!("frame with {count} views; only stereo is supported"));
                    }
                    let mut views = [GameView { swapchain: 0, image: 0, layer: 0, rect: [0; 4], fov_tan: [0.0; 4] }; 2];
                    for v in &mut views {
                        v.swapchain = r.u64()?;
                        v.image = r.u32()?;
                        v.layer = r.u32()?;
                        v.rect = [r.i32()?, r.i32()?, r.i32()?, r.i32()?];
                        // pose: 4 + 3 floats; the frame shows the pose of tracking_ns already
                        for _ in 0..7 {
                            r.f32()?;
                        }
                        v.fov_tan = [r.f32()?.tan(), r.f32()?.tan(), r.f32()?.tan(), r.f32()?.tan()];
                    }
                    events.send(GameEvent::Frame(GameFrame { number, tracking_ns, views })).ok();
                }
                msg::HAPTIC => {
                    let hand = r.u32()?;
                    let duration = Duration::from_nanos(r.u64()?);
                    let frequency = r.f32()?;
                    let amplitude = r.f32()?;
                    self.counters.haptics.fetch_add(1, Ordering::Relaxed);
                    if forwards_haptic(&mut quiet, hand, amplitude) {
                        on_haptic(Haptic { hand, duration, frequency, amplitude });
                    }
                }
                msg::TRACKING_REQUEST => {
                    let requested = Features(r.u32()? as u8).known();
                    if self.requested.swap(requested.0, Ordering::Relaxed) != requested.0 {
                        info!("Game runtime: the game uses {}", if requested.is_empty() { "no tracking".to_string() } else { requested.names().join(", ") });
                    }
                }
                msg::TEARDOWN => {
                    info!("Game runtime closed the session");
                    return Ok(());
                }
                other => warn!("Game runtime sent unknown message {other}"),
            }
        }
    }

    // The Quest Touch profile for both hands
    fn send_profiles(&self) {
        for hand in 0..2u32 {
            let mut p = Vec::with_capacity(12);
            p.extend_from_slice(&hand.to_le_bytes());
            p.extend_from_slice(&QUEST_TOUCH_PROFILE_ID.to_le_bytes());
            self.send(msg::INTERACTION_PROFILE, &p);
        }
    }

    fn send(&self, kind: u16, payload: &[u8]) {
        let mut writer = self.writer.lock().unwrap();
        let Some(stream) = writer.as_mut() else { return };
        let mut buf = Vec::with_capacity(6 + payload.len());
        buf.extend_from_slice(&(2 + payload.len() as u32).to_le_bytes());
        buf.extend_from_slice(&kind.to_le_bytes());
        buf.extend_from_slice(payload);
        if stream.write_all(&buf).is_err() {
            *writer = None;
        }
        *self.last_sent.lock().unwrap() = Instant::now();
    }

    pub fn frame_consumed(&self, number: u64) {
        self.send(msg::FRAME_CONSUMED, &number.to_le_bytes());
    }

    pub fn head_pose(&self, timestamp_ns: u64, motion: &DeviceMotion) {
        let motion = crate::test_input::game_head(motion);
        let mut p = timestamp_ns.to_le_bytes().to_vec();
        put_pose(&mut p, &motion);
        self.send(msg::HEAD_POSE, &p);
    }

    /// The pose of a controller's grip with its velocities and, when the headset reports one, the
    /// aim pose. A hand without an aim pose sends the grip pose alone, and a game then finds its aim
    /// space at the grip pose.
    pub fn controller(&self, timestamp_ns: u64, hand: u32, grip: &DeviceMotion, aim: Option<&DeviceMotion>) {
        let grip = &crate::test_input::game_controller(grip);
        let aim = aim.map(crate::test_input::game_controller);
        let aim = aim.as_ref();
        if self.profiles_dropped.swap(false, Ordering::Relaxed) {
            self.send_profiles();
        }
        let index = (hand as usize).min(1);
        self.counters.poses[index].fetch_add(1, Ordering::Relaxed);
        {
            let mut travel = self.counters.travel.lock().unwrap();
            let at = grip.pose.position;
            let track = &mut travel[index];
            if let Some(before) = track.at {
                track.moved += ((at.x - before[0]).powi(2) + (at.y - before[1]).powi(2) + (at.z - before[2]).powi(2)).sqrt();
            }
            track.at = Some([at.x, at.y, at.z]);
        }
        let mut p = timestamp_ns.to_le_bytes().to_vec();
        p.extend_from_slice(&hand.to_le_bytes());
        p.extend_from_slice(&0u32.to_le_bytes());
        put_motion(&mut p, grip);
        match aim {
            Some(aim) => {
                self.counters.aim_poses[index].fetch_add(1, Ordering::Relaxed);
                put_motion(&mut p, aim);
                self.send(msg::CONTROLLER_MOTION_AIM, &p);
            }
            None => self.send(msg::CONTROLLER_MOTION, &p),
        }
    }

    /// Which controllers are tracked: bit 0 left, bit 1 right.
    pub fn controller_mask(&self, timestamp_ns: u64, mask: u32) {
        self.counters.mask.store(mask, Ordering::Relaxed);
        let mut p = timestamp_ns.to_le_bytes().to_vec();
        p.extend_from_slice(&mask.to_le_bytes());
        p.extend_from_slice(&0u32.to_le_bytes());
        self.send(msg::CONTROLLER_MASK, &p);
    }

    /// A button or an axis by its path id: `scalar` false sends a binary value (0 or 1).
    pub fn button(&self, path_id: u64, scalar: bool, value: f32) {
        self.counters.buttons.fetch_add(1, Ordering::Relaxed);
        let mut p = path_id.to_le_bytes().to_vec();
        p.extend_from_slice(&(scalar as u32).to_le_bytes());
        p.extend_from_slice(&value.to_le_bytes());
        self.send(msg::BUTTON_STATE, &p);
    }

    /// What went to the runtime and came back from it since the last call.
    pub fn take_counts(&self) -> InputCounts {
        let c = &self.counters;
        let travel = {
            let mut travel = c.travel.lock().unwrap();
            let seen = *travel;
            for track in travel.iter_mut() {
                track.moved = 0.0;
            }
            seen
        };
        InputCounts {
            poses: [c.poses[0].swap(0, Ordering::Relaxed), c.poses[1].swap(0, Ordering::Relaxed)],
            aim_poses: [c.aim_poses[0].swap(0, Ordering::Relaxed), c.aim_poses[1].swap(0, Ordering::Relaxed)],
            mask: c.mask.load(Ordering::Relaxed),
            buttons: c.buttons.swap(0, Ordering::Relaxed),
            haptics: c.haptics.swap(0, Ordering::Relaxed),
            tracking: std::array::from_fn(|i| c.tracking[i].swap(0, Ordering::Relaxed)),
            requested: self.requested_tracking(),
            at_mm: travel.map(|t| t.at.map_or([0; 3], |at| at.map(|v| (v * 1000.0).round() as i32))),
            moved_mm: travel.map(|t| (t.moved * 1000.0).round() as u32),
        }
    }

    /// The tracking features that the connected game uses now. It is empty when no game runs, or when
    /// the runtime lacks cranvr's tracking patch.
    pub fn requested_tracking(&self) -> Features {
        Features(self.requested.load(Ordering::Relaxed))
    }

    /// Tells the runtime which features the headset offers: a file next to the control file, which
    /// the runtime reads when a game lists its OpenXR extensions. An empty set removes the file.
    pub fn set_offered_tracking(&self, offered: Features) {
        let path = self.control_dir().join(TRACKING_FILE);
        self.offered.store(offered.0, Ordering::Relaxed);
        let result = if offered.is_empty() {
            match fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        } else {
            // The runtime may read while the host writes: a rename leaves it the old file or the new
            let partial = path.with_extension("tmp");
            fs::write(&partial, format!("{}\n", offered.names().join(" "))).and_then(|()| fs::rename(&partial, &path))
        };
        if let Err(e) = result {
            warn!("Game runtime: the offered tracking features did not reach {}: {e}", path.display());
        }
    }

    /// One hand of a tracking sample, or `None` while the headset does not track it. `root` is the
    /// pose of the wrist with its velocities. The joints lie in the host's room.
    pub fn hand(&self, timestamp_ns: u64, hand: u32, tracked: Option<(&DeviceMotion, &[Joint; HAND_JOINTS])>) {
        if !self.is_connected() {
            return;
        }
        self.counters.tracking[0].fetch_add(1, Ordering::Relaxed);
        let mut p = timestamp_ns.to_le_bytes().to_vec();
        p.extend_from_slice(&hand.to_le_bytes());
        p.extend_from_slice(&u32::from(tracked.is_some()).to_le_bytes());
        if let Some((root, joints)) = tracked {
            put_motion(&mut p, root);
            put_joints(&mut p, joints);
        }
        self.send(msg::HAND_TRACKING, &p);
    }

    /// The body of a tracking sample. `root` is the pose of the first joint with its velocities.
    pub fn body(&self, timestamp_ns: u64, root: &DeviceMotion, body: &Body, joints: &[Joint]) {
        if !self.is_connected() || ![BODY_JOINTS, FULL_BODY_JOINTS].contains(&joints.len()) {
            return;
        }
        self.counters.tracking[1].fetch_add(1, Ordering::Relaxed);
        let mut p = timestamp_ns.to_le_bytes().to_vec();
        p.extend_from_slice(&(joints.len() as u32).to_le_bytes());
        p.extend_from_slice(&1u32.to_le_bytes());
        p.extend_from_slice(&body.confidence.to_le_bytes());
        put_motion(&mut p, root);
        put_joints(&mut p, joints);
        self.send(msg::BODY_TRACKING, &p);
    }

    /// The face of a tracking sample
    pub fn face(&self, timestamp_ns: u64, face: &Face, weights: &[f32; FACE_WEIGHTS]) {
        if !self.is_connected() {
            return;
        }
        self.counters.tracking[2].fetch_add(1, Ordering::Relaxed);
        let mut p = timestamp_ns.to_le_bytes().to_vec();
        p.extend_from_slice(&u32::from(face.flags).to_le_bytes());
        let confidences: [f32; FACE_CONFIDENCES] = face.confidences();
        for c in confidences.iter().chain(weights) {
            p.extend_from_slice(&c.to_le_bytes());
        }
        self.send(msg::FACE_TRACKING, &p);
    }

    /// The gaze of a tracking sample, or `None` while the headset has none
    pub fn eyes(&self, timestamp_ns: u64, eyes: &Eyes) {
        if !self.is_connected() {
            return;
        }
        self.counters.tracking[3].fetch_add(1, Ordering::Relaxed);
        let mut p = timestamp_ns.to_le_bytes().to_vec();
        p.extend_from_slice(&u32::from(eyes.gaze.is_some()).to_le_bytes());
        let gaze = eyes.gaze.unwrap_or_default();
        for c in gaze.orientation.iter().chain(&gaze.position) {
            p.extend_from_slice(&c.to_le_bytes());
        }
        self.send(msg::EYE_GAZE, &p);
    }

    /// IPD in meters and the field of view of each eye (left, right, up, down; radians).
    pub fn set_view_config(&self, ipd: f32, fov: [[f32; 4]; 2]) {
        let v = [ipd, fov[0][0], fov[0][1], fov[0][2], fov[0][3], fov[1][0], fov[1][1], fov[1][2], fov[1][3]];
        self.calibration.lock().unwrap().view_config = Some(v);
        self.send_view_config(v);
    }

    fn send_view_config(&self, v: [f32; 9]) {
        let p: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
        self.send(msg::VIEW_CONFIG, &p);
    }
}

fn put_pose(p: &mut Vec<u8>, motion: &DeviceMotion) {
    for c in motion.pose.orientation.to_array() {
        p.extend_from_slice(&c.to_le_bytes());
    }
    for c in motion.pose.position.to_array() {
        p.extend_from_slice(&c.to_le_bytes());
    }
}

/// A pose and then its linear and angular velocity: 13 floats
fn put_motion(p: &mut Vec<u8>, motion: &DeviceMotion) {
    put_pose(p, motion);
    for v in [motion.linear_velocity, motion.angular_velocity] {
        for c in v.to_array() {
            p.extend_from_slice(&c.to_le_bytes());
        }
    }
}

/// The poses of joints, then a flags byte for each: 7 floats and a byte per joint
fn put_joints(p: &mut Vec<u8>, joints: &[Joint]) {
    for joint in joints {
        for c in joint.pose.orientation.iter().chain(&joint.pose.position) {
            p.extend_from_slice(&c.to_le_bytes());
        }
    }
    p.extend(joints.iter().map(|joint| joint.flags));
}

pub(crate) fn read_message(stream: &mut TcpStream) -> Result<(u16, Vec<u8>), String> {
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).map_err(|e| e.to_string())?;
    let length = u32::from_le_bytes(head);
    if !(2..=MAX_MESSAGE).contains(&length) {
        return Err(format!("bad message length {length}"));
    }
    let mut body = vec![0u8; length as usize];
    stream.read_exact(&mut body).map_err(|e| e.to_string())?;
    Ok((u16::from_le_bytes([body[0], body[1]]), body[2..].to_vec()))
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.0.len() < n {
            return Err("message too short".into());
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.bytes(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
}

fn random_token() -> std::io::Result<String> {
    let mut bytes = [0u8; TOKEN_LENGTH / 2];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Keeps the swapchain images the frame loop needs, by swapchain ID.
#[derive(Default)]
pub struct Swapchains(HashMap<u64, Vec<PixelBuffer>>);

impl Swapchains {
    pub fn insert(&mut self, id: u64, images: Vec<PixelBuffer>) {
        self.0.insert(id, images);
    }
    pub fn remove(&mut self, id: u64) {
        self.0.remove(&id);
    }
    pub fn clear(&mut self) {
        self.0.clear();
    }
    pub fn image(&self, view: &GameView) -> Option<&PixelBuffer> {
        // `image` already counts the layers: the runtime flattens it
        self.0.get(&view.swapchain)?.get(view.image as usize)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_hand_that_is_quiet_hears_no_more_stops() {
        let mut quiet = [true; 2];
        assert!(!super::forwards_haptic(&mut quiet, 0, 0.0), "a quiet hand needs no stop");
        assert!(super::forwards_haptic(&mut quiet, 0, 0.4), "a pulse goes on");
        assert!(super::forwards_haptic(&mut quiet, 0, 0.4), "so does the next pulse");
        assert!(super::forwards_haptic(&mut quiet, 0, 0.0), "the first stop after a pulse goes on");
        assert!(!super::forwards_haptic(&mut quiet, 0, 0.0), "the stops after it do not");
        assert!(super::forwards_haptic(&mut quiet, 1, 0.1), "the other hand has its own state");
        assert!(!super::forwards_haptic(&mut quiet, 0, 0.0));
        assert!(!super::forwards_haptic(&mut quiet, 2, 0.5), "a hand that does not exist gets nothing");
    }

    use super::*;

    use crate::pose::{Pose, Quat, Vec3};

    fn message(kind: u16, payload: &[u8]) -> Vec<u8> {
        let mut m = (2 + payload.len() as u32).to_le_bytes().to_vec();
        m.extend_from_slice(&kind.to_le_bytes());
        m.extend_from_slice(payload);
        m
    }

    /// A fake runtime: connects with the token from the control file, expects the hello reply
    /// and the controller profiles, sends one frame and receives the head pose.
    #[test]
    fn handshake_frame_and_pose() {
        let dir = std::env::temp_dir().join(format!("cranvr-link-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let control = dir.join("control.txt");
        let (link, events) = RuntimeLink::start(&control, (1600, 1700), 120, |_| ()).unwrap();
        let text = fs::read_to_string(&control).unwrap();
        let fields: Vec<&str> = text.split_whitespace().collect();
        assert_eq!(fields.len(), 5);
        assert_eq!((fields[2], fields[3], fields[4]), ("1600", "1700", "120"));
        assert_eq!(fs::metadata(&control).unwrap().permissions().mode() & 0o777, 0o600);

        // The runtime names the game in a file before its HELLO
        fs::write(dir.join("game-program.txt"), "C:\\Games\\Game.exe").unwrap();
        let mut runtime = TcpStream::connect(("127.0.0.1", fields[0].parse::<u16>().unwrap())).unwrap();
        let mut hello = PROTOCOL_VERSION.to_le_bytes().to_vec();
        hello.extend_from_slice(fields[1].as_bytes());
        hello.extend_from_slice(&7u64.to_le_bytes());
        hello.extend_from_slice(&4u16.to_le_bytes());
        hello.extend_from_slice(b"test");
        runtime.write_all(&message(msg::HELLO, &hello)).unwrap();
        assert_eq!(read_message(&mut runtime).unwrap(), (msg::HELLO_ACK, PROTOCOL_VERSION.to_le_bytes().to_vec()));
        for hand in 0..2u32 {
            let (kind, payload) = read_message(&mut runtime).unwrap();
            assert_eq!(kind, msg::INTERACTION_PROFILE);
            assert_eq!(&payload[..4], &hand.to_le_bytes());
        }
        match events.recv_timeout(Duration::from_secs(2)).unwrap() {
            GameEvent::Connected { program, .. } => assert_eq!(program.as_deref(), Some("C:\\Games\\Game.exe")),
            _ => panic!("expected Connected"),
        }
        assert!(!dir.join("game-program.txt").exists(), "the host takes the name away");

        let mut frame = 42u64.to_le_bytes().to_vec();
        frame.extend_from_slice(&0u64.to_le_bytes());
        frame.extend_from_slice(&123_456u64.to_le_bytes());
        frame.extend_from_slice(&2u32.to_le_bytes());
        for eye in 0..2u64 {
            frame.extend_from_slice(&eye.to_le_bytes());
            frame.extend_from_slice(&1u32.to_le_bytes());
            frame.extend_from_slice(&0u32.to_le_bytes());
            for v in [0i32, 0, 1600, 1700] {
                frame.extend_from_slice(&v.to_le_bytes());
            }
            for v in [0.0f32, 0.0, 0.0, 1.0, 0.0, 1.6, 0.0, -0.8, 0.8, 0.8, -0.8] {
                frame.extend_from_slice(&v.to_le_bytes());
            }
        }
        runtime.write_all(&message(msg::FRAME_READY, &frame)).unwrap();
        match events.recv_timeout(Duration::from_secs(2)).unwrap() {
            GameEvent::Frame(f) => {
                assert_eq!((f.number, f.tracking_ns), (42, 123_456));
                assert_eq!(f.views[1].swapchain, 1);
                assert!((f.views[0].fov_tan[0] - (-0.8f32).tan()).abs() < 1e-6);
            }
            _ => panic!("expected a frame"),
        }

        let motion = DeviceMotion {
            pose: Pose { orientation: Quat::IDENTITY, position: Vec3::new(0.0, 1.6, 0.0) },
            linear_velocity: Vec3::ZERO,
            angular_velocity: Vec3::ZERO,
        };
        link.head_pose(123_456, &motion);
        let (kind, payload) = read_message(&mut runtime).unwrap();
        assert_eq!(kind, msg::HEAD_POSE);
        assert_eq!(payload.len(), 8 + 7 * 4);
        link.frame_consumed(42);
        assert_eq!(read_message(&mut runtime).unwrap(), (msg::FRAME_CONSUMED, 42u64.to_le_bytes().to_vec()));
        fs::remove_dir_all(&dir).ok();
    }

    /// A fake runtime that connected to `link`'s control file and got the hello reply and the two
    /// controller profiles.
    fn connected_runtime(link: &RuntimeLink) -> TcpStream {
        let text = fs::read_to_string(link.control_file()).unwrap();
        let fields: Vec<&str> = text.split_whitespace().collect();
        let mut runtime = TcpStream::connect(("127.0.0.1", fields[0].parse::<u16>().unwrap())).unwrap();
        let mut hello = PROTOCOL_VERSION.to_le_bytes().to_vec();
        hello.extend_from_slice(fields[1].as_bytes());
        hello.extend_from_slice(&7u64.to_le_bytes());
        hello.extend_from_slice(&0u16.to_le_bytes());
        runtime.write_all(&message(msg::HELLO, &hello)).unwrap();
        assert_eq!(read_message(&mut runtime).unwrap().0, msg::HELLO_ACK);
        for _ in 0..2 {
            assert_eq!(read_message(&mut runtime).unwrap().0, msg::INTERACTION_PROFILE);
        }
        runtime
    }

    fn floats(payload: &[u8]) -> Vec<f32> {
        payload.chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect()
    }

    #[test]
    fn a_runtime_gets_both_poses_of_a_hand_in_one_message() {
        let dir = std::env::temp_dir().join(format!("cranvr-link-aim-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (link, events) = RuntimeLink::start(&dir.join("control.txt"), (1600, 1700), 120, |_| ()).unwrap();
        let mut runtime = connected_runtime(&link);
        assert!(matches!(events.recv_timeout(Duration::from_secs(2)).unwrap(), GameEvent::Connected { .. }));

        let motion = |x: f32, angle: f32, v: f32| DeviceMotion {
            pose: Pose { orientation: Quat::from_rotation_x(angle), position: Vec3::new(x, 1.2, -0.4) },
            linear_velocity: Vec3::new(v, 0.0, 0.0),
            angular_velocity: Vec3::new(0.0, v, 0.0),
        };
        let (grip, aim) = (motion(0.2, 0.0, 1.0), motion(0.25, -0.5, 2.0));
        link.controller(99, 1, &grip, Some(&aim));
        let (kind, payload) = read_message(&mut runtime).unwrap();
        assert_eq!(kind, msg::CONTROLLER_MOTION_AIM);
        assert_eq!(payload.len(), 8 + 4 + 4 + 26 * 4);
        assert_eq!((&payload[..8], &payload[8..12], &payload[12..16]), (&99u64.to_le_bytes()[..], &1u32.to_le_bytes()[..], &[0u8; 4][..]));
        let values = floats(&payload[16..]);
        let expected = |m: &DeviceMotion| {
            let (q, p) = (m.pose.orientation.to_array(), m.pose.position.to_array());
            [&q[..], &p[..], &m.linear_velocity.to_array()[..], &m.angular_velocity.to_array()[..]].concat()
        };
        // The grip pose first, then the aim pose, each with its velocities
        assert_eq!(values[..13], expected(&grip)[..]);
        assert_eq!(values[13..], expected(&aim)[..]);
        // A hand without an aim pose sends the message of the grip pose alone
        link.controller(100, 0, &grip, None);
        let (kind, payload) = read_message(&mut runtime).unwrap();
        assert_eq!((kind, payload.len()), (msg::CONTROLLER_MOTION, 8 + 4 + 4 + 13 * 4));
        assert_eq!(link.take_counts().aim_poses, [0, 1]);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn controllers_that_come_back_after_a_reset_get_their_profiles_again() {
        let dir = std::env::temp_dir().join(format!("cranvr-link-reset-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (link, _events) = RuntimeLink::start(&dir.join("control.txt"), (1600, 1700), 120, |_| ()).unwrap();
        let mut runtime = connected_runtime(&link);
        let motion = DeviceMotion { pose: Pose { orientation: Quat::IDENTITY, position: Vec3::new(0.2, 1.2, -0.4) }, linear_velocity: Vec3::ZERO, angular_velocity: Vec3::ZERO };
        // No pose flows, as while the user sets up the room: the keepalive resets the input
        assert_eq!(read_message(&mut runtime).unwrap().0, msg::INPUT_RESET);
        link.controller(7, 1, &motion, Some(&motion));
        let kinds: Vec<u16> = (0..3).map(|_| read_message(&mut runtime).unwrap().0).collect();
        assert_eq!(kinds, [msg::INTERACTION_PROFILE, msg::INTERACTION_PROFILE, msg::CONTROLLER_MOTION_AIM]);
        // The profiles go once per reset
        link.controller(8, 1, &motion, Some(&motion));
        assert_eq!(read_message(&mut runtime).unwrap().0, msg::CONTROLLER_MOTION_AIM);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wrong_token_is_refused() {
        let dir = std::env::temp_dir().join(format!("cranvr-link-token-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let control = dir.join("control.txt");
        let (link, _events) = RuntimeLink::start(&control, (1600, 1700), 72, |_| ()).unwrap();
        let port: u16 = fs::read_to_string(&control).unwrap().split_whitespace().next().unwrap().parse().unwrap();
        let mut runtime = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut hello = PROTOCOL_VERSION.to_le_bytes().to_vec();
        hello.extend_from_slice(&[b'0'; TOKEN_LENGTH]);
        hello.extend_from_slice(&0u64.to_le_bytes());
        hello.extend_from_slice(&0u16.to_le_bytes());
        runtime.write_all(&message(msg::HELLO, &hello)).unwrap();
        let mut buf = [0u8; 1];
        runtime.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        assert_eq!(runtime.read(&mut buf).unwrap_or(0), 0, "the host must close the connection");
        assert!(!link.is_connected());
        fs::remove_dir_all(&dir).ok();
    }
}
