// cranvr: the stream in a window, for Android devices without an OpenXR runtime, such as a
// Galaxy Tab. The device is the head: its motion sensors give the pose, and its screen shows
// both eyes side by side. At each refresh the newest decoded frame is warped with the newest
// sensor pose, as a headset's compositor does, then swapped to the display. The display times
// come from EGL, so the latency numbers in the log are real ones (see timing.rs).

mod clock;
mod decoder;
mod head;
pub mod log;
mod pacing;
pub mod property;
mod timing;
mod window;

use crate::stream::ParsedStreamConfig;
use alvr_client_core::{video_decoder::VideoDecoderConfig, ClientCapabilities, ClientCoreContext, ClientCoreEvent};
use alvr_common::{
    anyhow::Result,
    glam::{UVec2, Vec3},
    Fov, Pose, ViewParams,
};
use alvr_graphics::{compute_target_view_resolution, GraphicsContext, StreamRenderer, StreamViewParams};
use android_activity::AndroidApp;
use glow::HasContext;
use head::{Head, HEAD_POSITION};
use std::{
    ptr,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    thread,
    time::Duration,
};
use pacing::Pacing;
use timing::{Shown, Timing};
use window::WindowSurface;

/// Set by android_main: the window comes from it.
pub static APP: OnceLock<AndroidApp> = OnceLock::new();
/// Set by android_main when the activity ends.
pub static QUIT: AtomicBool = AtomicBool::new(false);


const REFRESH_RATE: f32 = 120.0;
// The tangent of half the vertical field of view of each eye: about 84 degrees in all
const TAN_HALF_HEIGHT: f32 = 0.9;
const IPD_M: f32 = 0.063;

pub fn run() {
    let app = APP.get().expect("android_main stores the app").clone();
    let window = loop {
        if let Some(window) = app.native_window() {
            break window;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let gfx = Rc::new(GraphicsContext::new_gl());
    let front_buffer = property::get("front_buffer").as_deref() == Some("1");
    let surface = unsafe { WindowSurface::new(&gfx, window.ptr().as_ptr().cast(), REFRESH_RATE, front_buffer) };
    surface.make_current();
    let eye = UVec2::new(surface.width as u32 / 2, surface.height as u32);
    log::line(&format!("cranvr-flat: window {}x{}, {}x{} per eye", surface.width, surface.height, eye.x, eye.y));

    // The host streams at the display's own rate. The compositor needs a moment to switch to the
    // rate that the window asked for, so the app shows blank frames for a while before it
    // measures the rate.
    let mut pacing = Pacing::new();
    pacing.measure(60, || blank(&gfx, &surface));
    let refresh_rate = pacing.measure(60, || blank(&gfx, &surface)).round();
    log::line(&format!("cranvr-flat: display at {refresh_rate} Hz, render lead {} us", pacing.lead.as_micros()));

    let core = Arc::new(ClientCoreContext::new(capabilities(eye, refresh_rate)));
    core.resume();
    let head = Head::start(Arc::clone(&core));
    let views = view_params(eye);
    // A test of the warp: the display turns by this yaw while the host draws for the true pose.
    // The picture must move by the matching angle (scripts/e2e-flat.sh --warp-check).
    let warp_yaw = property::get("warp_yaw_deg").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0).to_radians();
    let mut stream: Option<Stream> = None;
    if let Some((deadline, interval, to_present)) = surface.compositor_timing() {
        log::line(&format!(
            "cranvr-flat: compositor: next composition in {:?}, every {:?}, {:?} to the display",
            deadline.saturating_sub(clock::now()),
            interval,
            to_present
        ));
    }
    let mut timing = Timing::default();
    timing.display_times = if surface.front_buffer { "derived" } else { "egl" };

    while !QUIT.load(Ordering::Relaxed) {
        pacing.wait();
        let loop_start = clock::now();
        while let Some(event) = core.poll_event() {
            match event {
                ClientCoreEvent::StreamingStarted(config) => {
                    let config = ParsedStreamConfig::new(&config);
                    log::line(&format!("cranvr-flat: stream {}x{} per eye at {} Hz", config.view_resolution.x, config.view_resolution.y, config.refresh_rate_hint));
                    core.send_view_params(views);
                    surface.make_current();
                    stream = Some(Stream::new(&gfx, config, views, warp_yaw));
                }
                ClientCoreEvent::StreamingStopped => stream = None,
                ClientCoreEvent::DecoderConfig { codec, config_nal } => {
                    if let Some(stream) = &mut stream {
                        stream.start_decoder(&core, codec, config_nal);
                    }
                }
                _ => (),
            }
        }
        match &mut stream {
            Some(stream) => stream.show(&gfx, &core, &surface, &head, &mut timing),
            None => blank(&gfx, &surface),
        }
        timing.rendered(clock::now().saturating_sub(loop_start));
        timing.collect(&surface);
        if let Some(line) = timing.report(clock::now()) {
            log::line(&format!("cranvr-flat: {line}"));
        }
    }
    drop(stream);
    drop(surface);
    drop(window);
}

/// Shows a dark frame while no stream runs.
fn blank(gfx: &GraphicsContext, surface: &WindowSurface) {
    surface.make_current();
    unsafe {
        gfx.gl_context.clear_color(0.04, 0.04, 0.06, 1.0);
        gfx.gl_context.clear(glow::COLOR_BUFFER_BIT);
    }
    surface.swap();
}

fn capabilities(eye: UVec2, refresh_rate: f32) -> ClientCapabilities {
    ClientCapabilities {
        default_view_resolution: eye,
        refresh_rates: vec![refresh_rate],
        foveated_encoding: true,
        encoder_high_profile: true,
        encoder_10_bits: false,
        encoder_av1: false,
        prefer_10bit: false,
        prefer_full_range: true,
        preferred_encoding_gamma: 1.0,
        prefer_hdr: false,
    }
}

/// The two eyes relative to the head: each one shows in half the screen, so its field of view
/// has the shape of that half.
fn view_params(eye: UVec2) -> [ViewParams; 2] {
    let tan_half_width = TAN_HALF_HEIGHT * eye.x as f32 / eye.y as f32;
    let fov = Fov {
        left: -tan_half_width.atan(),
        right: tan_half_width.atan(),
        up: TAN_HALF_HEIGHT.atan(),
        down: -TAN_HALF_HEIGHT.atan(),
    };
    [-0.5, 0.5].map(|side| ViewParams { pose: Pose { position: Vec3::new(side * IPD_M, 0.0, 0.0), ..Default::default() }, fov })
}

struct Stream {
    config: ParsedStreamConfig,
    renderer: StreamRenderer,
    eye_textures: [glow::Texture; 2],
    read_framebuffer: glow::Framebuffer,
    target: UVec2,
    decoder: Option<(VideoDecoderConfig, decoder::VideoDecoderSource)>,
    /// The views that the host drew the shown frame for
    frame_views: [ViewParams; 2],
    /// The eyes relative to the head
    views: [ViewParams; 2],
    /// The test turn of the warp, see `warp_yaw` in `run`
    warp_yaw: f32,
}

impl Stream {
    fn new(gfx: &Rc<GraphicsContext>, config: ParsedStreamConfig, views: [ViewParams; 2], warp_yaw: f32) -> Self {
        let target = compute_target_view_resolution(config.view_resolution, &config.upscaling);
        let gl = &gfx.gl_context;
        let eye_textures = [(); 2].map(|_| unsafe {
            let texture = gl.create_texture().expect("eye texture");
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_storage_2d(glow::TEXTURE_2D, 1, glow::RGBA8, target.x as i32, target.y as i32);
            gl.bind_texture(glow::TEXTURE_2D, None);
            texture
        });
        let read_framebuffer = unsafe { gl.create_framebuffer().expect("framebuffer") };
        // The eye textures hold the encoded values: the window shows them as they are
        let renderer = StreamRenderer::new(
            Rc::clone(gfx),
            config.view_resolution,
            target,
            eye_textures.map(|t| vec![t.0.get()]),
            glow::RGBA8,
            config.foveated_encoding_config.clone(),
            false,
            config.use_full_range && !config.enable_hdr,
            config.encoding_gamma,
            config.upscaling.clone(),
        );
        Self {
            config,
            renderer,
            eye_textures,
            read_framebuffer,
            target,
            decoder: None,
            frame_views: [ViewParams::default(); 2],
            views,
            warp_yaw,
        }
    }

    fn start_decoder(&mut self, core: &Arc<ClientCoreContext>, codec: alvr_session::CodecType, config_nal: Vec<u8>) {
        let config = VideoDecoderConfig {
            codec,
            force_software_decoder: self.config.force_software_decoder,
            max_buffering_frames: self.config.max_buffering_frames,
            buffering_history_weight: self.config.buffering_history_weight,
            options: self.config.decoder_options.clone(),
            config_buffer: config_nal,
        };
        if self.decoder.as_ref().is_some_and(|(current, _)| *current == config) {
            return;
        }
        let split = decoder::video_decoder_split(config.clone(), config.config_buffer.clone(), {
            let core = Arc::clone(core);
            move |result: Result<Duration>| match result {
                Ok(timestamp) => core.report_frame_decoded(timestamp),
                Err(e) => core.report_fatal_decoder_error(&e.to_string()),
            }
        });
        let (mut sink, source) = match split {
            Ok(split) => split,
            Err(e) => {
                core.report_fatal_decoder_error(&e.to_string());
                return;
            }
        };
        self.decoder = Some((config, source));
        core.set_decoder_input_callback(Box::new(move |timestamp, buffer| {
            alvr_common::show_err(sink.push_frame_nal(timestamp, buffer)).unwrap_or(false)
        }));
    }

    fn show(
        &mut self,
        gfx: &GraphicsContext,
        core: &ClientCoreContext,
        surface: &WindowSurface,
        head: &Head,
        timing: &mut Timing,
    ) {
        let frame = self.decoder.as_mut().and_then(|(_, source)| source.newest_frame());
        let (frame_pose_time, buffer) = match frame {
            Some((timestamp, buffer, true)) => {
                self.frame_views = core.report_compositor_start(timestamp);
                (Some(timestamp), buffer)
            }
            // The previous frame again, warped with the newer pose
            _ => (None, ptr::null_mut()),
        };
        let Some(sample) = head.latest() else {
            surface.swap();
            return;
        };
        timing.pose_age(clock::now().saturating_sub(sample.sample_time));
        let turn = alvr_common::glam::Quat::from_rotation_y(self.warp_yaw);
        let head_pose = Pose { orientation: sample.orientation * turn, position: HEAD_POSITION };
        let warp_views = self.views.map(|view| ViewParams { pose: head_pose * view.pose, fov: view.fov });
        unsafe {
            self.renderer.render(
                buffer,
                [0, 1].map(|eye| StreamViewParams {
                    swapchain_index: 0,
                    input_view_params: self.frame_views[eye],
                    output_view_params: warp_views[eye],
                }),
                None,
            );
        }

        if surface.front_buffer {
            // The eye textures come from ALVR's context: its GPU work ends before this context
            // reads them
            gfx.make_current();
            unsafe { gfx.gl_context.finish() };
        }
        surface.make_current();
        let gl = &gfx.gl_context;
        let half = surface.width / 2;
        unsafe {
            // ALVR's renderer leaves the scissor test on at the eye size; a blit obeys it
            gl.disable(glow::SCISSOR_TEST);
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.read_framebuffer));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
            for (eye, texture) in self.eye_textures.iter().enumerate() {
                gl.framebuffer_texture_2d(glow::READ_FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(*texture), 0);
                let x = eye as i32 * half;
                // ALVR draws the eyes upright for GL, as OpenXR swapchains need: a plain copy
                gl.blit_framebuffer(
                    0,
                    0,
                    self.target.x as i32,
                    self.target.y as i32,
                    x,
                    0,
                    x + half,
                    surface.height,
                    glow::COLOR_BUFFER_BIT,
                    glow::LINEAR,
                );
            }
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
        }
        if surface.front_buffer {
            // The pixels are on the displayed buffer once the GPU finishes
            unsafe { gl.finish() };
            let done = clock::now();
            if let Some(timestamp) = frame_pose_time {
                core.report_submit(timestamp, timing.display_lag);
            }
            if let Some(display) = surface.derived_display_time(done) {
                let shown = Shown { frame_id: 0, frame_pose_time, warp_pose_time: sample.sample_time, swapped: done };
                timing.shown_at(shown, display);
            }
            return;
        }
        let frame_id = surface.next_frame_id();
        surface.swap();
        let swapped = clock::now();
        if let Some(timestamp) = frame_pose_time {
            core.report_submit(timestamp, timing.display_lag);
        }
        if let Some(frame_id) = frame_id {
            timing.shown(Shown { frame_id, frame_pose_time, warp_pose_time: sample.sample_time, swapped });
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // The decoder goes first: it holds images that the renderer may still use
        self.decoder = None;
    }
}
