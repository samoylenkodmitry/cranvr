// cranvr: the Mac desktop as an OpenXR compositor layer (see host/src/desktop_layer.rs for the
// protocol). The headset's compositor samples the desktop once, at the display's resolution and
// with the newest head pose. In the eye stream the same picture is sampled twice: the host draws
// it into the eye view, and the compositor warps that view through the lens. A test with a fixed
// picture made 10-point text readable that the eye stream blurred.
//
// The app listens on TCP port PORT; the host connects when ALVR connects. The video goes from
// the network straight into a hardware decoder, which writes into an Android surface that the
// compositor shows as a quad layer (XR_KHR_android_surface_swapchain): no copy on the GPU. The
// cursor comes over UDP and is drawn as a second, small quad at the newest position every
// refresh, so it does not wait for a video frame.
//
// The app shows one panel per Mac display that the host streams: panel 0 for the display the
// host captures, and more after the "Add display" button. Each panel has its own decoder and its
// own surface. A new stream for a panel, as after a quality change, takes over only once it has a
// picture, so the panel never shows black meanwhile.
//
// The app places the panels itself, in the headset's stage space (see panel.rs). It draws the
// controllers, their rays and the panels' handles, buttons and corners in an overlay layer. Every
// refresh it sends the host over UDP where the active controller points, and how the head sees
// each panel. The buttons' requests go to the host over the TCP link.
//
// debug.cranvr.panel_test runs requests once, after the first panel shows, for tests without
// hands: a list such as "add,quality:0:1,pause:1:1,close:1" (quality and pause take the panel
// and the value).
//
// debug.cranvr.console_test sends one console request, a JSON object such as
// {"action":"set","msaa":0}, once the first menu arrives, for tests without hands. "front"
// instead puts the console straight ahead of the head. "exit" presses the console's Exit button.
//
// debug.cranvr.layer_flags picks the compositor's filters for the desktop layer: "ss" for
// supersampling, "sharp" for sharpening, "ss,sharp" (the default) for both, "none" for neither.

use crate::{
    bar::BarLayer,
    console::{Menu, CONSOLE_ID},
    flat::property,
    graphics::{ProjectionLayerAlphaConfig, ProjectionLayerBuilder},
    interaction::{ButtonAction, InteractionContext},
    panel::{Command, Hand, Inputs, PanelControl},
};
use alvr_common::{
    error,
    glam::{Mat3, Quat, UVec2, Vec3},
    LEFT_THUMBSTICK_Y_ID, LEFT_TRIGGER_VALUE_ID, RIGHT_THUMBSTICK_CLICK_ID, RIGHT_THUMBSTICK_Y_ID,
    RIGHT_TRIGGER_VALUE_ID,
};
use alvr_graphics::{GraphicsContext, OverlayRenderer, OverlayView, SDR_FORMAT_GL};
use glow::HasContext;
use ndk::{
    media::media_codec::{
        DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec, MediaCodecDirection, MediaFormat,
    },
    native_window::NativeWindow,
};
use openxr as xr;
use std::{
    collections::{BTreeMap, HashMap},
    io::{Read, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket},
    num::NonZeroU32,
    path::PathBuf,
    ptr,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

pub const PORT: u16 = 9950;
pub const CURSOR_PORT: u16 = 9951;
/// The host listens here for where the controller points
pub const POINTER_PORT: u16 = 9952;
/// Where the app keeps the panels' places
const PANEL_FILE: &str = "/sdcard/Android/data/dev.cranvr.client/files/panels.txt";
/// The overlay's size as a share of the eye's recommended size. Below 1 the compositor scales
/// the overlay up, and its edges turn to stairs.
const OVERLAY_SCALE: f32 = 1.0;
const VERSION: u32 = 2;

const MSG_HELLO: u8 = 0;
const MSG_CONFIG: u8 = 1;
const MSG_FRAME: u8 = 3;
const MSG_VISIBLE: u8 = 4;
const MSG_STATE: u8 = 5;
const MSG_CLOSED: u8 = 6;
const MSG_PAUSE: u8 = 16;
const MSG_QUALITY: u8 = 17;
const MSG_ADD: u8 = 18;
const MSG_CLOSE: u8 = 19;
const MSG_INSET_CONFIG: u8 = 7;
const MSG_INSET_FRAME: u8 = 8;
const MSG_INSET_CLOSED: u8 = 9;
/// Host to app: the console's menu as JSON (see console.rs)
const MSG_MENU: u8 = 22;
/// App to host: a console button's request as JSON
const MSG_CONSOLE: u8 = 23;

const GL_SRGB8_ALPHA8: u32 = 0x8C43;

// ALVR's logger drops info lines while a stream runs: these go to logcat directly (tag cranvr)
macro_rules! info {
    ($($arg:tt)*) => { crate::flat::log::line(&format!($($arg)*)) };
}
use info as warn;
/// The cursor picture's size in pixels; it stands for 32 points of the Mac's display
const CURSOR_SIZE: u32 = 64;

struct Frame {
    timestamp_ns: u64,
    data: Vec<u8>,
    /// For an inset: the part of the display that the frame shows, as u0, v0, u1, v1
    part: Option<[f32; 4]>,
}

/// The part of the display that the inset's surface shows now
type Part = Arc<Mutex<Option<[f32; 4]>>>;

/// A new stream for a panel: the panel, the picture's size, its frames
type NewStream = (u8, (u32, u32), mpsc::Receiver<Frame>);

/// What the network threads hand to the render thread.
#[derive(Default)]
struct Shared {
    config: Mutex<Vec<NewStream>>,
    /// Panels that the host closed
    closed: Mutex<Vec<u8>>,
    /// A new inset stream: the panel, the size, its frames
    inset_config: Mutex<Option<NewStream>>,
    /// The host closed the inset
    inset_closed: AtomicBool,
    /// The host's reports: panel, paused, quality
    states: Mutex<Vec<(u8, bool, u8)>>,
    /// A new host connected: the old panels go
    reset: AtomicBool,
    /// The host's address, for the pointer
    host: Mutex<Option<IpAddr>>,
    /// The link to the host, for the buttons' requests
    writer: Mutex<Option<TcpStream>>,
    visible: AtomicBool,
    connected: AtomicBool,
    /// The newest cursor position: the panel, u and v on its display, whether the cursor is on
    /// a panel's display
    cursor: Mutex<Option<(u8, f32, f32, bool, Instant)>>,
    /// Frames decoded since the last log line
    decoded: AtomicU32,
    /// The host's newest menu for the console, not taken yet
    menu: Mutex<Option<Vec<u8>>>,
}

/// Sends the codec to the decoder thread, which then uses it alone.
struct SendCodec(MediaCodec);
unsafe impl Send for SendCodec {}

/// One panel's picture: the surface that its decoder writes into.
struct Stream {
    swapchain: xr::Swapchain<xr::OpenGlEs>,
    size: (u32, u32),
    /// The decoder has shown its first picture
    ready: Arc<AtomicBool>,
}

/// The swapchains of the overlay layer and the renderer that draws into them.
struct Overlay {
    swapchains: [xr::Swapchain<xr::OpenGlEs>; 2],
    renderer: OverlayRenderer,
    resolution: UVec2,
}

pub struct DesktopLayer {
    shared: Arc<Shared>,
    gfx: Rc<GraphicsContext>,
    control: PanelControl,
    overlay: Option<Overlay>,
    view_resolution: UVec2,
    /// The eye views of the coming refresh, while the overlay shows
    views: Option<[xr::View; 2]>,
    pointer: Option<UdpSocket>,
    pointer_seq: u32,
    last_frame: Instant,
    /// The panels' pictures on show
    streams: BTreeMap<u8, Stream>,
    /// New pictures that wait for their first frame
    pending: BTreeMap<u8, Stream>,
    /// The host's newest report on each panel: paused, quality
    host_states: HashMap<u8, (bool, u8)>,
    /// Requests from debug.cranvr.panel_test, not sent yet
    test_commands: Vec<Command>,
    /// A request from debug.cranvr.console_test, sent once the console has its first menu
    console_test: Option<String>,
    /// Whether the console's Exit button was pressed
    exit: bool,
    /// Each panel's bar layer
    bars: HashMap<u8, BarLayer>,
    /// The sharp inset over the panel that the head points at: the panel, its picture, the
    /// part of the display that the picture shows
    inset: Option<(u8, Stream, Part)>,
    /// A new inset that waits for its first frame
    inset_pending: Option<(u8, Stream, Part)>,
    cursor_swapchain: Option<xr::Swapchain<xr::OpenGlEs>>,
    settings: Option<xr::sys::CompositionLayerSettingsFB>,
    last_log: Instant,
    /// Frames with the layer go to the compositor with the time of the coming refresh. The
    /// stream frame's own time can lie up to a second back; then the compositor places the
    /// layer by an old head pose, and the screen shakes when the head turns. The property
    /// `debug.cranvr.layer_time` set to `frame` keeps the stream frame's time, for a comparison.
    pub vsync_time: bool,
}

impl DesktopLayer {
    /// Starts to listen for the host. Call it once the session runs.
    pub fn new(
        session: &xr::Session<xr::OpenGlEs>,
        gfx: &Rc<GraphicsContext>,
        layer_settings: bool,
        view_resolution: UVec2,
    ) -> Self {
        // The listeners outlive a session: a new session would find the ports taken
        static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
        let shared = Arc::clone(SHARED.get_or_init(|| {
            let shared = Arc::new(Shared::default());
            let listen_shared = Arc::clone(&shared);
            thread::Builder::new().name("desktop-listen".into()).spawn(move || listen(&listen_shared)).ok();
            let cursor_shared = Arc::clone(&shared);
            thread::Builder::new().name("desktop-cursor".into()).spawn(move || cursor_loop(&cursor_shared)).ok();
            shared
        }));
        // A stream that came before this session needs a new surface
        shared.connected.store(false, Ordering::Relaxed);
        let flags = property::get("layer_flags").unwrap_or_else(|| "ss,sharp".into());
        let mut bits = 0u64;
        if flags.contains("ss") {
            bits |= xr::CompositionLayerSettingsFlagsFB::QUALITY_SUPER_SAMPLING.into_raw();
        }
        if flags.contains("sharp") {
            bits |= xr::CompositionLayerSettingsFlagsFB::QUALITY_SHARPENING.into_raw();
        }
        let settings = (layer_settings && bits != 0).then(|| xr::sys::CompositionLayerSettingsFB {
            ty: xr::StructureType::COMPOSITION_LAYER_SETTINGS_FB,
            next: ptr::null(),
            layer_flags: xr::CompositionLayerSettingsFlagsFB::from_raw(bits),
        });
        let cursor_swapchain = cursor_swapchain(session, gfx)
            .map_err(|e| error!("cranvr desktop layer: cursor: {e}"))
            .ok();
        let vsync_time = property::get("layer_time").as_deref() != Some("frame");
        info!("cranvr desktop layer: listening on port {PORT}, filters {flags:?}, refresh time {vsync_time}");
        Self {
            shared,
            gfx: Rc::clone(gfx),
            control: PanelControl::new(PathBuf::from(PANEL_FILE)),
            overlay: None,
            view_resolution,
            views: None,
            pointer: UdpSocket::bind("0.0.0.0:0").ok(),
            pointer_seq: 0,
            last_frame: Instant::now(),
            streams: BTreeMap::new(),
            pending: BTreeMap::new(),
            host_states: HashMap::new(),
            test_commands: property::get("panel_test").map(|t| test_commands(&t)).unwrap_or_default(),
            console_test: property::get("console_test").filter(|t| !t.is_empty()),
            exit: false,
            bars: HashMap::new(),
            inset: None,
            inset_pending: None,
            cursor_swapchain,
            settings,
            last_log: Instant::now(),
            vsync_time,
        }
    }

    /// Takes new streams from the host: a surface swapchain of each one's size, and a decoder
    /// that writes into it. Call it every frame on the render thread.
    pub fn update(&mut self, session: &xr::Session<xr::OpenGlEs>) {
        if self.shared.reset.swap(false, Ordering::Relaxed) {
            self.streams.clear();
            self.pending.clear();
            self.host_states.clear();
            self.inset = None;
            self.inset_pending = None;
        }
        let configs: Vec<NewStream> = self.shared.config.lock().unwrap().drain(..).collect();
        for (id, size, frames) in configs {
            let ready = Arc::new(AtomicBool::new(false));
            match surface_decoder(session, size, frames, Arc::clone(&self.shared), Arc::clone(&ready), None) {
                Ok(swapchain) => {
                    info!("cranvr desktop layer: panel {id} stream {}x{}", size.0, size.1);
                    let stream = Stream { swapchain, size, ready };
                    // A panel on show keeps its old picture until the new one has a frame
                    if self.streams.contains_key(&id) {
                        self.pending.insert(id, stream);
                    } else {
                        self.streams.insert(id, stream);
                    }
                }
                Err(e) => error!("cranvr desktop layer: {e}"),
            }
        }
        let ready: Vec<u8> = self.pending.iter().filter(|(_, s)| s.ready.load(Ordering::Relaxed)).map(|(id, _)| *id).collect();
        for id in ready {
            if let Some(stream) = self.pending.remove(&id) {
                self.streams.insert(id, stream);
            }
        }
        for id in self.shared.closed.lock().unwrap().drain(..) {
            self.streams.remove(&id);
            self.pending.remove(&id);
        }
        if self.shared.inset_closed.swap(false, Ordering::Relaxed) {
            self.inset = None;
            self.inset_pending = None;
        }
        let inset_config = self.shared.inset_config.lock().unwrap().take();
        if let Some((id, size, frames)) = inset_config {
            let (ready, part) = (Arc::new(AtomicBool::new(false)), Part::default());
            match surface_decoder(session, size, frames, Arc::clone(&self.shared), Arc::clone(&ready), Some(Arc::clone(&part))) {
                Ok(swapchain) => {
                    info!("cranvr desktop layer: inset for panel {id}, {}x{}", size.0, size.1);
                    self.inset_pending = Some((id, Stream { swapchain, size, ready }, part));
                }
                Err(e) => error!("cranvr desktop layer: inset: {e}"),
            }
        }
        if self.inset_pending.as_ref().is_some_and(|p| p.1.ready.load(Ordering::Relaxed)) {
            self.inset = self.inset_pending.take();
        }
        for (id, paused, quality) in self.shared.states.lock().unwrap().drain(..) {
            self.host_states.insert(id, (paused, quality));
        }
        if self.last_log.elapsed() > Duration::from_secs(5) {
            let decoded = self.shared.decoded.swap(0, Ordering::Relaxed);
            if !self.streams.is_empty() {
                info!(
                    "cranvr desktop layer: {} panels, {:.1} frames per second decoded",
                    self.streams.len(),
                    decoded as f32 / self.last_log.elapsed().as_secs_f32()
                );
            }
            self.last_log = Instant::now();
        }
    }

    /// Whether the panels show: the host is connected, sends a stream and runs no game.
    fn shown(&self) -> bool {
        !self.streams.is_empty() && self.shared.connected.load(Ordering::Relaxed) && self.shared.visible.load(Ordering::Relaxed)
    }

    /// Moves the panels by the controllers, tells the host where the active controller points and
    /// how the head sees each panel, sends the buttons' requests, and draws the overlay for the
    /// refresh at `time`. Call it every frame before `layers`.
    pub fn frame(
        &mut self,
        session: &xr::Session<xr::OpenGlEs>,
        interaction: &InteractionContext,
        space: &xr::Space,
        time: xr::Time,
    ) {
        self.views = None;
        let dt = self.last_frame.elapsed().as_secs_f32().min(0.1);
        self.last_frame = Instant::now();
        if !self.shown() {
            return;
        }
        let Ok((flags, views)) = session.locate_views(xr::ViewConfigurationType::PRIMARY_STEREO, time, space) else {
            return;
        };
        if !flags.contains(xr::ViewStateFlags::POSITION_VALID | xr::ViewStateFlags::ORIENTATION_VALID) || views.len() < 2 {
            return;
        }
        let (left, right) = (crate::from_xr_pose(views[0].pose), crate::from_xr_pose(views[1].pose));
        let head = alvr_common::Pose { orientation: left.orientation, position: (left.position + right.position) / 2.0 };

        let locate = |s: &xr::Space| {
            s.locate(space, time)
                .ok()
                .filter(|l| {
                    l.location_flags.contains(xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID)
                })
                .map(|l| crate::from_xr_pose(l.pose))
        };
        let button = |id: u64| match interaction.button_actions.get(&id) {
            Some(ButtonAction::Scalar(a)) => a.state(session, xr::Path::NULL).map_or(0.0, |s| s.current_state),
            Some(ButtonAction::Binary(a)) => a.state(session, xr::Path::NULL).map_or(0.0, |s| s.current_state as u8 as f32),
            None => 0.0,
        };
        let ids = [(*LEFT_TRIGGER_VALUE_ID, *LEFT_THUMBSTICK_Y_ID), (*RIGHT_TRIGGER_VALUE_ID, *RIGHT_THUMBSTICK_Y_ID)];
        let hands = [0, 1].map(|i| {
            let h = &interaction.hands_interaction[i];
            Hand { aim: locate(&h.aim_space), grip: locate(&h.grip_space), trigger: button(ids[i].0), stick_y: button(ids[i].1) }
        });
        let inputs = Inputs { hands, head, stick_click: button(*RIGHT_THUMBSTICK_CLICK_ID) > 0.5, dt };
        let streams: Vec<(u8, f32, u32)> =
            self.streams.iter().map(|(id, s)| (*id, s.size.0 as f32 / s.size.1.max(1) as f32, s.size.0)).collect();
        let menu = self.shared.menu.lock().unwrap().take().and_then(|json| Menu::parse(&json));
        let first_menu = menu.is_some();
        if let Some(menu) = menu {
            self.control.set_menu(menu);
        }
        let mut out = self.control.update(&inputs, &streams);
        if first_menu {
            match self.console_test.take() {
                Some(test) if test == "front" => self.control.console_in_front(),
                Some(test) if test == "exit" => out.exit = true,
                test => out.console.extend(test),
            }
        }
        if out.exit && !self.exit {
            info!("cranvr console: exit");
            self.exit = true;
        }
        // The console's requests go to the host over TCP
        if !out.console.is_empty() {
            if let Some(writer) = self.shared.writer.lock().unwrap().as_mut() {
                for request in out.console.drain(..) {
                    info!("cranvr console: asks the host: {request}");
                    let mut message = vec![MSG_CONSOLE];
                    message.extend((request.len() as u32).to_le_bytes());
                    message.extend(request.as_bytes());
                    writer.write_all(&message).ok();
                }
            }
        }
        out.commands.append(&mut self.test_commands);
        for (id, (paused, quality)) in &self.host_states {
            self.control.set_state(*id, *paused, *quality);
        }
        let mut bars = HashMap::new();
        for bar in self.control.bars() {
            let id = bar.panel;
            match BarLayer::update(self.bars.remove(&id), bar, session, &self.gfx) {
                Ok(layer) => {
                    bars.insert(id, layer);
                }
                Err(e) => error!("cranvr desktop layer: bar: {e}"),
            }
        }
        self.bars = bars;

        // The buttons' requests go to the host over TCP
        if !out.commands.is_empty() {
            if let Some(writer) = self.shared.writer.lock().unwrap().as_mut() {
                for command in &out.commands {
                    let (kind, payload) = match *command {
                        Command::Pause(id, paused) => (MSG_PAUSE, vec![id, paused as u8]),
                        Command::Quality(id, quality) => (MSG_QUALITY, vec![id, quality]),
                        Command::Add => (MSG_ADD, vec![]),
                        Command::Close(id) => (MSG_CLOSE, vec![id]),
                    };
                    info!("cranvr desktop layer: asks the host: {command:?}");
                    let mut message = vec![kind];
                    message.extend((payload.len() as u32).to_le_bytes());
                    message.extend(payload);
                    writer.write_all(&message).ok();
                }
            }
        }

        // Where the active controller points and how the head sees each panel go to the host:
        // sequence number, panel, u, v, flags (1: on a panel), trigger, scroll, then the count
        // and for each panel its number, gaze and wanted width
        if let (Some(socket), Some(host)) = (&self.pointer, *self.shared.host.lock().unwrap()) {
            let (panel, u, v) = out.pointer.unwrap_or((0, 0.0, 0.0));
            let mut packet = Vec::with_capacity(23 + 4 * out.views.len());
            packet.extend(self.pointer_seq.to_le_bytes());
            packet.push(panel);
            packet.extend(u.to_le_bytes());
            packet.extend(v.to_le_bytes());
            packet.push(out.pointer.is_some() as u8);
            packet.extend(out.trigger.to_le_bytes());
            packet.extend(out.scroll.to_le_bytes());
            packet.push(out.views.len() as u8);
            for (id, gaze, wanted) in &out.views {
                packet.extend([*id, *gaze as u8]);
                packet.extend(wanted.to_le_bytes());
            }
            let (look_panel, lu, lv, lwanted) = out.look.unwrap_or((255, 0.0, 0.0, 0));
            packet.push(look_panel);
            packet.extend(lu.to_le_bytes());
            packet.extend(lv.to_le_bytes());
            packet.extend(lwanted.to_le_bytes());
            socket.send_to(&packet, SocketAddr::new(host, POINTER_PORT)).ok();
            self.pointer_seq = self.pointer_seq.wrapping_add(1);
        }

        if self.overlay.is_none() {
            self.overlay = self.create_overlay(session).map_err(|e| error!("cranvr overlay: {e}")).ok();
        }
        let Some(overlay) = &mut self.overlay else { return };
        self.gfx.make_current();
        let draws = self.control.draws();
        let mut indices = [0u32; 2];
        for (index, swapchain) in indices.iter_mut().zip(overlay.swapchains.iter_mut()) {
            let Ok(i) = swapchain.acquire_image() else { return };
            swapchain.wait_image(xr::Duration::INFINITE).ok();
            *index = i;
        }
        let view = |eye: usize| OverlayView {
            swapchain_index: indices[eye],
            pose: crate::from_xr_pose(views[eye].pose),
            fov: crate::from_xr_fov(views[eye].fov),
        };
        overlay.renderer.render([view(0), view(1)], &draws);
        for swapchain in &mut overlay.swapchains {
            swapchain.release_image().ok();
        }
        self.views = Some([views[0], views[1]]);
    }

    fn create_overlay(&self, session: &xr::Session<xr::OpenGlEs>) -> Result<Overlay, String> {
        let resolution = (self.view_resolution.as_vec2() * OVERLAY_SCALE).as_uvec2();
        let swapchain = || crate::graphics::create_swapchain(session, &self.gfx, resolution, SDR_FORMAT_GL, None);
        let swapchains = [swapchain(), swapchain()];
        let images = |s: &xr::Swapchain<xr::OpenGlEs>| s.enumerate_images().map_err(|e| e.to_string());
        let textures = [images(&swapchains[0])?, images(&swapchains[1])?];
        let renderer = OverlayRenderer::new(Rc::clone(&self.gfx), resolution, textures);
        info!("cranvr overlay: {}x{} per eye", resolution.x, resolution.y);
        Ok(Overlay { swapchains, renderer, resolution })
    }

    /// The overlay with the controllers, their rays and the handle, over all other layers.
    pub fn overlay_layer<'a>(&'a self, space: &'a xr::Space) -> Option<ProjectionLayerBuilder<'a>> {
        let (overlay, views) = (self.overlay.as_ref()?, self.views.as_ref()?);
        let rect = xr::Rect2Di {
            offset: xr::Offset2Di { x: 0, y: 0 },
            extent: xr::Extent2Di { width: overlay.resolution.x as i32, height: overlay.resolution.y as i32 },
        };
        let view = |eye: usize| {
            xr::CompositionLayerProjectionView::new().pose(views[eye].pose).fov(views[eye].fov).sub_image(
                xr::SwapchainSubImage::new().swapchain(&overlay.swapchains[eye]).image_array_index(0).image_rect(rect),
            )
        };
        Some(ProjectionLayerBuilder::new(
            space,
            [view(0), view(1)],
            Some(ProjectionLayerAlphaConfig { premultiplied: true }),
            None,
        ))
    }

    /// Whether the console's Exit button asked to close the app.
    pub fn wants_exit(&self) -> bool {
        self.exit
    }

    /// A quad layer for each panel, and one for the cursor on the panel that holds it.
    pub fn layers<'a>(&'a self, space: &'a xr::Space) -> Vec<xr::CompositionLayerQuad<'a, xr::OpenGlEs>> {
        let mut quads = Vec::new();
        if !self.shown() {
            return quads;
        }
        let cursor = *self.shared.cursor.lock().unwrap();
        // The console stands apart from the screens; its frame is in the overlay. The compositor
        // stacks the layers in this order, so a nearer console comes after the screens.
        let console = self.bars.get(&CONSOLE_ID).and_then(|bar| bar.layer(space, self.settings.as_ref()));
        let console_last = self.control.console_nearer();
        let mut console = console.map(|quad| if console_last { Some(quad) } else { quads.push(quad); None }).flatten();
        for (id, panel) in self.control.panels() {
            let Some(Stream { swapchain, size: (width, height), .. }) = self.streams.get(&id) else { continue };
            let (center, right, up) = (panel.center, panel.right() * panel.width / 2.0, panel.up() * panel.height / 2.0);
            let orientation = orientation(right, up);
            let screen = xr::CompositionLayerQuad::new()
                .space(space)
                .eye_visibility(xr::EyeVisibility::BOTH)
                .sub_image(xr::SwapchainSubImage::new().swapchain(swapchain).image_rect(xr::Rect2Di {
                    offset: xr::Offset2Di { x: 0, y: 0 },
                    extent: xr::Extent2Di { width: *width as i32, height: *height as i32 },
                }))
                .pose(xr::Posef { orientation, position: to_xr(center) })
                .size(xr::Extent2Df { width: panel.width, height: panel.height });
            quads.push(match &self.settings {
                Some(settings) => unsafe {
                    xr::CompositionLayerQuad::from_raw(xr::sys::CompositionLayerQuad {
                        next: ptr::from_ref(settings).cast(),
                        ..screen.into_raw()
                    })
                },
                None => screen,
            });
            // The sharp inset over the part of the picture that the head points at
            if let Some((_, Stream { swapchain, size: (iw, ih), .. }, part)) = self.inset.as_ref().filter(|i| i.0 == id) {
                if let Some([u0, v0, u1, v1]) = *part.lock().unwrap() {
                    let middle = panel.at(((u0 + u1) / 2.0 - 0.5) * panel.width, (0.5 - (v0 + v1) / 2.0) * panel.height)
                        + panel.normal() * 0.0005;
                    let inset = xr::CompositionLayerQuad::new()
                        .space(space)
                        .eye_visibility(xr::EyeVisibility::BOTH)
                        .sub_image(xr::SwapchainSubImage::new().swapchain(swapchain).image_rect(xr::Rect2Di {
                            offset: xr::Offset2Di { x: 0, y: 0 },
                            extent: xr::Extent2Di { width: *iw as i32, height: *ih as i32 },
                        }))
                        .pose(xr::Posef { orientation, position: to_xr(middle) })
                        .size(xr::Extent2Df { width: (u1 - u0) * panel.width, height: (v1 - v0) * panel.height });
                    quads.push(match &self.settings {
                        Some(settings) => unsafe {
                            xr::CompositionLayerQuad::from_raw(xr::sys::CompositionLayerQuad {
                                next: ptr::from_ref(settings).cast(),
                                ..inset.into_raw()
                            })
                        },
                        None => inset,
                    });
                }
            }
            if let Some(quad) = self.bars.get(&id).and_then(|bar| bar.layer(space, self.settings.as_ref())) {
                quads.push(quad);
            }

            if let (Some(cursor_swapchain), Some((panel_id, u, v, true, at))) = (&self.cursor_swapchain, cursor) {
                if panel_id != id || at.elapsed() > Duration::from_secs(2) {
                    continue;
                }
                let (r, w) = (panel.right(), panel.up());
                // The tip of the arrow sits at the cursor position; the picture is a square of
                // 32 points of the display
                let size = panel.width * CURSOR_SIZE as f32 / *width as f32;
                let tip = center + right * (2.0 * u - 1.0) + up * (1.0 - 2.0 * v);
                let middle = tip + r * size / 2.0 - w * size / 2.0 + panel.normal() * 0.002;
                quads.push(
                    xr::CompositionLayerQuad::new()
                        .layer_flags(
                            xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA | xr::CompositionLayerFlags::UNPREMULTIPLIED_ALPHA,
                        )
                        .space(space)
                        .eye_visibility(xr::EyeVisibility::BOTH)
                        .sub_image(xr::SwapchainSubImage::new().swapchain(cursor_swapchain).image_rect(xr::Rect2Di {
                            offset: xr::Offset2Di { x: 0, y: 0 },
                            extent: xr::Extent2Di { width: CURSOR_SIZE as i32, height: CURSOR_SIZE as i32 },
                        }))
                        .pose(xr::Posef { orientation, position: to_xr(middle) })
                        .size(xr::Extent2Df { width: size, height: size }),
                );
            }
        }
        quads.extend(console.take());
        quads
    }
}

/// Requests from a list such as "add,quality:0:1,pause:1:1,close:1".
fn test_commands(text: &str) -> Vec<Command> {
    text.split(',')
        .filter_map(|item| {
            let parts: Vec<&str> = item.trim().split(':').collect();
            let number = |i: usize| parts.get(i).and_then(|p| p.parse::<u8>().ok());
            match parts[0] {
                "add" => Some(Command::Add),
                "close" => Some(Command::Close(number(1)?)),
                "quality" => Some(Command::Quality(number(1)?, number(2)?)),
                "pause" => Some(Command::Pause(number(1)?, number(2)? != 0)),
                _ => None,
            }
        })
        .collect()
}

fn to_xr(v: Vec3) -> xr::Vector3f {
    xr::Vector3f { x: v.x, y: v.y, z: v.z }
}

/// The rotation that turns a quad's x axis to `right` and its y axis to `up`.
fn orientation(right: Vec3, up: Vec3) -> xr::Quaternionf {
    let (r, u) = (right.normalize_or_zero(), up.normalize_or_zero());
    let q = Quat::from_mat3(&Mat3::from_cols(r, u, r.cross(u))).normalize();
    xr::Quaternionf { x: q.x, y: q.y, z: q.z, w: q.w }
}

/// Serves one host at a time. A new host replaces the old one: a host that quit may leave its
/// connection open on this side for good, when Wi-Fi loses its close.
fn listen(shared: &Arc<Shared>) {
    let listener = match TcpListener::bind(("0.0.0.0", PORT)) {
        Ok(l) => l,
        Err(e) => {
            error!("cranvr desktop layer: cannot listen on port {PORT}: {e}");
            return;
        }
    };
    let mut current: Option<TcpStream> = None;
    let latest = Arc::new(AtomicU32::new(0));
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Some(old) = current.take() {
                    old.shutdown(Shutdown::Both).ok();
                }
                current = stream.try_clone().ok();
                let number = latest.fetch_add(1, Ordering::Relaxed) + 1;
                let (shared, latest) = (Arc::clone(shared), Arc::clone(&latest));
                thread::Builder::new()
                    .name("desktop-host".into())
                    .spawn(move || {
                        let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                        info!("cranvr desktop layer: host {peer} connected");
                        if let Err(e) = serve(stream, &shared) {
                            info!("cranvr desktop layer: host {peer} left: {e}");
                        }
                        // A newer host may already show its desktop
                        if latest.load(Ordering::Relaxed) == number {
                            shared.connected.store(false, Ordering::Relaxed);
                        }
                    })
                    .ok();
            }
            Err(e) => warn!("cranvr desktop layer: {e}"),
        }
    }
}

fn serve(mut stream: TcpStream, shared: &Shared) -> std::io::Result<()> {
    stream.set_nodelay(true).ok();
    let mut hello = vec![MSG_HELLO];
    hello.extend(4u32.to_le_bytes());
    hello.extend(VERSION.to_le_bytes());
    stream.write_all(&hello)?;
    *shared.host.lock().unwrap() = stream.peer_addr().ok().map(|a| a.ip());
    *shared.writer.lock().unwrap() = stream.try_clone().ok();
    shared.config.lock().unwrap().clear();
    shared.reset.store(true, Ordering::Relaxed);
    shared.connected.store(true, Ordering::Relaxed);
    shared.visible.store(true, Ordering::Relaxed);
    let mut frames: HashMap<u8, mpsc::Sender<Frame>> = HashMap::new();
    let mut inset_frames: Option<mpsc::Sender<Frame>> = None;
    loop {
        let mut header = [0u8; 5];
        stream.read_exact(&mut header)?;
        let len = u32::from_le_bytes(header[1..].try_into().unwrap()) as usize;
        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload)?;
        match header[0] {
            MSG_CONFIG if len >= 10 => {
                let id = payload[0];
                let width = u32::from_le_bytes(payload[2..6].try_into().unwrap());
                let height = u32::from_le_bytes(payload[6..10].try_into().unwrap());
                let (sender, receiver) = mpsc::channel();
                frames.insert(id, sender);
                shared.config.lock().unwrap().push((id, (width, height), receiver));
            }
            MSG_FRAME if len > 10 => {
                let timestamp_ns = u64::from_le_bytes(payload[1..9].try_into().unwrap());
                if let Some(sender) = frames.get(&payload[0]) {
                    sender.send(Frame { timestamp_ns, data: payload[10..].to_vec(), part: None }).ok();
                }
            }
            MSG_VISIBLE if len >= 1 => shared.visible.store(payload[0] != 0, Ordering::Relaxed),
            MSG_STATE if len >= 3 => shared.states.lock().unwrap().push((payload[0], payload[1] != 0, payload[2])),
            MSG_INSET_CONFIG if len >= 9 => {
                let width = u32::from_le_bytes(payload[1..5].try_into().unwrap());
                let height = u32::from_le_bytes(payload[5..9].try_into().unwrap());
                let (sender, receiver) = mpsc::channel();
                inset_frames = Some(sender);
                *shared.inset_config.lock().unwrap() = Some((payload[0], (width, height), receiver));
            }
            MSG_INSET_FRAME if len > 26 => {
                let f = |i: usize| f32::from_le_bytes(payload[i..i + 4].try_into().unwrap());
                let part = [f(1), f(5), f(9), f(13)];
                let timestamp_ns = u64::from_le_bytes(payload[17..25].try_into().unwrap());
                if let Some(sender) = &inset_frames {
                    sender.send(Frame { timestamp_ns, data: payload[26..].to_vec(), part: Some(part) }).ok();
                }
            }
            MSG_INSET_CLOSED => {
                inset_frames = None;
                shared.inset_closed.store(true, Ordering::Relaxed);
            }
            MSG_CLOSED if len >= 1 => {
                frames.remove(&payload[0]);
                shared.closed.lock().unwrap().push(payload[0]);
            }
            MSG_MENU => *shared.menu.lock().unwrap() = Some(payload.to_vec()),
            _ => {}
        }
    }
}

fn cursor_loop(shared: &Shared) {
    let socket = match UdpSocket::bind(("0.0.0.0", CURSOR_PORT)) {
        Ok(s) => s,
        Err(e) => {
            error!("cranvr desktop layer: cannot listen for the cursor on port {CURSOR_PORT}: {e}");
            return;
        }
    };
    let mut buffer = [0u8; 64];
    while let Ok(len) = socket.recv(&mut buffer) {
        if len >= 14 {
            let u = f32::from_le_bytes(buffer[4..8].try_into().unwrap());
            let v = f32::from_le_bytes(buffer[8..12].try_into().unwrap());
            *shared.cursor.lock().unwrap() = Some((buffer[13], u, v, buffer[12] != 0, Instant::now()));
        }
    }
}

/// A surface swapchain of `size` and a hardware decoder that writes into its surface.
fn surface_decoder(
    session: &xr::Session<xr::OpenGlEs>,
    (width, height): (u32, u32),
    frames: mpsc::Receiver<Frame>,
    shared: Arc<Shared>,
    ready: Arc<AtomicBool>,
    part: Option<Part>,
) -> Result<xr::Swapchain<xr::OpenGlEs>, String> {
    let create = session
        .instance()
        .exts()
        .khr_android_surface_swapchain
        .ok_or("this runtime has no XR_KHR_android_surface_swapchain")?
        .create_swapchain_android_surface;
    let info = xr::sys::SwapchainCreateInfo {
        ty: xr::sys::SwapchainCreateInfo::TYPE,
        next: ptr::null(),
        create_flags: xr::SwapchainCreateFlags::EMPTY,
        usage_flags: xr::SwapchainUsageFlags::EMPTY,
        format: 0,
        sample_count: 0,
        width,
        height,
        face_count: 0,
        array_size: 0,
        mip_count: 0,
    };
    let mut handle = xr::sys::Swapchain::NULL;
    let mut surface: xr::sys::platform::jobject = ptr::null_mut();
    let result = unsafe { create(session.as_raw(), &info, &mut handle, &mut surface) };
    if result != xr::sys::Result::SUCCESS || surface.is_null() {
        return Err(format!("no surface swapchain: {result:?}"));
    }
    let swapchain = unsafe { xr::Swapchain::from_raw(session.clone(), handle) };

    let context = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) }.map_err(|e| e.to_string())?;
    let env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let window = unsafe { NativeWindow::from_surface(env.get_raw().cast(), surface.cast()) }
        .ok_or("the surface has no native window")?;

    let codec = MediaCodec::from_decoder_type("video/hevc").ok_or("no HEVC decoder")?;
    let mut format = MediaFormat::new();
    format.set_str("mime", "video/hevc");
    format.set_i32("width", width as i32);
    format.set_i32("height", height as i32);
    format.set_i32("low-latency", 1);
    format.set_i32("priority", 0);
    codec
        .configure(&format, Some(&window), MediaCodecDirection::Decoder)
        .map_err(|e| format!("decoder configure: {e}"))?;
    codec.start().map_err(|e| format!("decoder start: {e}"))?;
    let codec = SendCodec(codec);
    thread::Builder::new()
        .name("desktop-decode".into())
        .spawn(move || {
            let _window = window;
            decode_loop(codec, frames, &shared, &ready, part)
        })
        .map_err(|e| e.to_string())?;
    Ok(swapchain)
}

/// Feeds the frames to the decoder and shows every decoded picture at once. Ends when the frame
/// channel closes, at a new stream or when the host leaves.
fn decode_loop(codec: SendCodec, frames: mpsc::Receiver<Frame>, shared: &Shared, ready: &AtomicBool, part: Option<Part>) {
    // The parts of the frames in the decoder, by presentation time: the inset shows the part of
    // the frame that the surface shows
    let mut parts: std::collections::VecDeque<(i64, [f32; 4])> = std::collections::VecDeque::new();
    let codec = codec.0;
    let mut pending: Option<Frame> = None;
    loop {
        if pending.is_none() {
            match frames.recv_timeout(Duration::from_millis(4)) {
                Ok(frame) => pending = Some(frame),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        if let Some(frame) = pending.take() {
            match codec.dequeue_input_buffer(Duration::from_millis(2)) {
                Ok(DequeuedInputBufferResult::Buffer(mut buffer)) => {
                    let room = buffer.buffer_mut().len();
                    let len = frame.data.len().min(room);
                    if len < frame.data.len() {
                        warn!("cranvr desktop layer: a frame of {} bytes did not fit in {room}", frame.data.len());
                    }
                    unsafe { ptr::copy_nonoverlapping(frame.data.as_ptr(), buffer.buffer_mut().as_mut_ptr().cast(), len) };
                    if let Some(p) = frame.part {
                        parts.push_back(((frame.timestamp_ns / 1000) as i64, p));
                        while parts.len() > 16 {
                            parts.pop_front();
                        }
                    }
                    if let Err(e) = codec.queue_input_buffer(buffer, 0, len, frame.timestamp_ns / 1000, 0) {
                        error!("cranvr desktop layer: decoder input: {e}");
                    }
                }
                Ok(DequeuedInputBufferResult::TryAgainLater) => pending = Some(frame),
                Err(e) => {
                    error!("cranvr desktop layer: decoder input: {e}");
                    break;
                }
            }
        }
        loop {
            match codec.dequeue_output_buffer(Duration::ZERO) {
                Ok(DequeuedOutputBufferInfoResult::Buffer(buffer)) => {
                    let pts = buffer.info().presentation_time_us();
                    if let (Some(part), Some(&(_, p))) = (&part, parts.iter().find(|(t, _)| *t == pts)) {
                        *part.lock().unwrap() = Some(p);
                    }
                    if let Err(e) = codec.release_output_buffer(buffer, true) {
                        error!("cranvr desktop layer: decoder output: {e}");
                    }
                    shared.decoded.fetch_add(1, Ordering::Relaxed);
                    ready.store(true, Ordering::Relaxed);
                }
                Ok(DequeuedOutputBufferInfoResult::TryAgainLater) => break,
                Ok(_) => continue,
                Err(e) => {
                    error!("cranvr desktop layer: decoder output: {e}");
                    break;
                }
            }
        }
    }
    codec.stop().ok();
}

/// A fixed picture of an arrow cursor: white with a black edge, the tip at the top left corner.
fn cursor_swapchain(session: &xr::Session<xr::OpenGlEs>, gfx: &GraphicsContext) -> Result<xr::Swapchain<xr::OpenGlEs>, String> {
    let n = CURSOR_SIZE as usize;
    // The classic arrow in a box of 32 points, 2 pixels per point
    let shape: [(f32, f32); 7] = [(2.0, 2.0), (2.0, 46.0), (13.0, 36.0), (21.0, 54.0), (28.0, 51.0), (20.0, 34.0), (35.0, 34.0)];
    let inside = |x: f32, y: f32| {
        let mut odd = false;
        for i in 0..shape.len() {
            let (a, b) = (shape[i], shape[(i + 1) % shape.len()]);
            if (a.1 > y) != (b.1 > y) && x < a.0 + (y - a.1) * (b.0 - a.0) / (b.1 - a.1) {
                odd = !odd;
            }
        }
        odd
    };
    let near = |x: f32, y: f32, d: f32| {
        (0..shape.len()).any(|i| {
            let (a, b) = (shape[i], shape[(i + 1) % shape.len()]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let t = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
            let (px, py) = (a.0 + t * dx - x, a.1 + t * dy - y);
            px * px + py * py < d * d
        })
    };
    let mut pixels = vec![0u8; n * n * 4];
    for row in 0..n {
        for col in 0..n {
            let (x, y) = (col as f32 + 0.5, row as f32 + 0.5);
            let rgba = if near(x, y, 1.6) {
                [0, 0, 0, 255]
            } else if inside(x, y) {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 0]
            };
            // OpenGL counts rows from the bottom: the top row of the picture goes last
            let i = ((n - 1 - row) * n + col) * 4;
            pixels[i..i + 4].copy_from_slice(&rgba);
        }
    }

    gfx.make_current();
    let mut swapchain = session
        .create_swapchain(&xr::SwapchainCreateInfo {
            create_flags: xr::SwapchainCreateFlags::STATIC_IMAGE,
            usage_flags: xr::SwapchainUsageFlags::SAMPLED | xr::SwapchainUsageFlags::COLOR_ATTACHMENT,
            format: GL_SRGB8_ALPHA8,
            sample_count: 1,
            width: CURSOR_SIZE,
            height: CURSOR_SIZE,
            face_count: 1,
            array_size: 1,
            mip_count: 1,
        })
        .map_err(|e| e.to_string())?;
    let images = swapchain.enumerate_images().map_err(|e| e.to_string())?;
    let index = swapchain.acquire_image().map_err(|e| e.to_string())? as usize;
    swapchain.wait_image(xr::Duration::INFINITE).map_err(|e| e.to_string())?;
    unsafe {
        let texture = glow::NativeTexture(NonZeroU32::new(images[index]).ok_or("no texture")?);
        crate::bar::upload_rgba(&gfx.gl_context, texture, (n as u32, n as u32), &pixels);
        gfx.gl_context.finish();
    }
    swapchain.release_image().map_err(|e| e.to_string())?;
    Ok(swapchain)
}
