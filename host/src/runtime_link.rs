//! The host side of BasaltVR's runtime control protocol (version 9, MIT). The BasaltVR OpenXR
//! runtime runs inside Wine as the game's OpenXR runtime. It connects here over loopback TCP,
//! shares its swapchain images as IOSurface IDs and reports each finished frame. The host sends
//! back the head and controller poses from ALVR. Wire format: `runtime/pe/basalt_control_protocol.h`
//! in BasaltVR.
//!
//! The runtime finds the host through a control file: "<port> <token> <eye width> <eye height>".
//! The launcher passes the file's Windows path to the game in the `BASALT_CONTROL` variable.

use alvr_common::{info, warn, DeviceMotion};
use cranvr_vt::PixelBuffer;
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    thread,
    time::Duration,
};

pub const PROTOCOL_VERSION: u16 = 9;
const TOKEN_LENGTH: usize = 32;
const MAX_MESSAGE: u32 = 4096;
/// The runtime accepts per-eye sizes up to this (BASALT_CONTROL_MAX_VIEW_*; cranvr's runtime
/// patch raises them to the Quest 3 panel size).
pub const MAX_VIEW: (u32, u32) = (2064, 2208);
/// ALVR's ID for "/interaction_profiles/oculus/touch_controller".
const QUEST_TOUCH_PROFILE_ID: u64 = 0x0812_fd85_c843_84a6;

mod msg {
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
    pub const VIEW_CONFIG: u16 = 15;
    pub const CONTROLLER_MASK: u16 = 16;
    pub const FRAME_CONSUMED: u16 = 17;
}

/// One eye of a finished game frame.
#[derive(Clone, Copy, Debug)]
pub struct GameView {
    pub swapchain: u64,
    pub image: u32,
    /// x, y, width, height in pixels of the swapchain image
    pub rect: [i32; 4],
    /// tangents of the left, right, up and down angles the game rendered with
    pub fov_tan: [f32; 4],
}

#[derive(Clone, Debug)]
pub struct GameFrame {
    pub number: u64,
    /// The ALVR tracking sample the game rendered for
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

pub struct Haptic {
    pub hand: u32,
    pub duration: Duration,
    pub frequency: f32,
    pub amplitude: f32,
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
    calibration: Mutex<Calibration>,
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
            calibration: Mutex::new(Calibration::default()),
            control_file: control_file.to_owned(),
            port,
            token: random_token()?,
        });
        link.write_control_file(eye_size, fps)?;
        {
            let link = Arc::clone(&link);
            thread::Builder::new().name("runtime-link".into()).spawn(move || {
                for stream in listener.incoming().flatten() {
                    if let Err(e) = link.serve(stream, &events, &on_haptic) {
                        warn!("Game runtime connection: {e}");
                    }
                    *link.writer.lock().unwrap() = None;
                    events.send(GameEvent::Disconnected).ok();
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
        // The file names this game only: the host takes it away, so an older runtime that writes
        // none leaves the game unnamed
        let program_file = self.control_dir().join("game-program.txt");
        let program = fs::read_to_string(&program_file).ok().map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
        fs::remove_file(&program_file).ok();
        info!("Game runtime connected: {renderer}, {}", program.as_deref().unwrap_or("program not named"));

        *self.writer.lock().unwrap() = Some(stream.try_clone().map_err(|e| e.to_string())?);
        self.send(msg::HELLO_ACK, &PROTOCOL_VERSION.to_le_bytes());
        if let Some(view) = self.calibration.lock().unwrap().view_config {
            self.send_view_config(view);
        }
        for hand in 0..2u32 {
            let mut p = Vec::with_capacity(12);
            p.extend_from_slice(&hand.to_le_bytes());
            p.extend_from_slice(&QUEST_TOUCH_PROFILE_ID.to_le_bytes());
            self.send(msg::INTERACTION_PROFILE, &p);
        }
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
                    let mut views = [GameView { swapchain: 0, image: 0, rect: [0; 4], fov_tan: [0.0; 4] }; 2];
                    for v in &mut views {
                        v.swapchain = r.u64()?;
                        v.image = r.u32()?;
                        let _array_index = r.u32()?;
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
                    on_haptic(Haptic { hand, duration, frequency: r.f32()?, amplitude: r.f32()? });
                }
                msg::TEARDOWN => {
                    info!("Game runtime closed the session");
                    return Ok(());
                }
                other => warn!("Game runtime sent unknown message {other}"),
            }
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
    }

    pub fn frame_consumed(&self, number: u64) {
        self.send(msg::FRAME_CONSUMED, &number.to_le_bytes());
    }

    pub fn head_pose(&self, timestamp_ns: u64, motion: &DeviceMotion) {
        let mut p = timestamp_ns.to_le_bytes().to_vec();
        put_pose(&mut p, motion);
        self.send(msg::HEAD_POSE, &p);
    }

    pub fn controller(&self, timestamp_ns: u64, hand: u32, motion: &DeviceMotion) {
        let mut p = timestamp_ns.to_le_bytes().to_vec();
        p.extend_from_slice(&hand.to_le_bytes());
        p.extend_from_slice(&0u32.to_le_bytes());
        put_pose(&mut p, motion);
        for v in [motion.linear_velocity, motion.angular_velocity] {
            for c in v.to_array() {
                p.extend_from_slice(&c.to_le_bytes());
            }
        }
        self.send(msg::CONTROLLER_MOTION, &p);
    }

    /// Which controllers are tracked: bit 0 left, bit 1 right.
    pub fn controller_mask(&self, timestamp_ns: u64, mask: u32) {
        let mut p = timestamp_ns.to_le_bytes().to_vec();
        p.extend_from_slice(&mask.to_le_bytes());
        p.extend_from_slice(&0u32.to_le_bytes());
        self.send(msg::CONTROLLER_MASK, &p);
    }

    /// An ALVR button: `scalar` false sends a binary value (0 or 1).
    pub fn button(&self, path_id: u64, scalar: bool, value: f32) {
        let mut p = path_id.to_le_bytes().to_vec();
        p.extend_from_slice(&(scalar as u32).to_le_bytes());
        p.extend_from_slice(&value.to_le_bytes());
        self.send(msg::BUTTON_STATE, &p);
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

fn read_message(stream: &mut TcpStream) -> Result<(u16, Vec<u8>), String> {
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
        self.0.get(&view.swapchain)?.get(view.image as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alvr_common::{
        glam::{Quat, Vec3},
        Pose,
    };

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
