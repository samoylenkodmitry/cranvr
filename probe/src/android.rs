//! The probe's decoder on Android: ALVR's own MediaCodec path, as the Quest client uses it, and a
//! display that takes one image per refresh the way the Quest's compositor does. The display
//! reports each frame to the client core, which sends the latency of each stage to the host.
//!
//! The decoder hands out images that only the GPU reads, as on the Quest: a Qualcomm decoder
//! writes them in its compressed UBWC format. The display samples the eye badges with OpenGL ES
//! through ALVR's EGL context, as the Quest client samples the whole frame.

use alvr_client_core::{
    video_decoder::{self, VideoDecoderConfig, VideoDecoderSource},
    ClientCoreContext,
};
use alvr_common::RelaxedAtomic;
use alvr_session::{CodecType, VideoConfig};
use alvr_graphics::{GraphicsContext, GL_TEXTURE_EXTERNAL_OES};
use glow::HasContext;
use ndk::hardware_buffer::HardwareBuffer;
use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::{mpsc::Sender, Arc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Starts the binder threads that an app process gets from Android's runtime. MediaCodec hands
/// its output to the ImageReader through binder callbacks: a plain program started by adb has no
/// thread to take them, and the decoder stalls after its first frames. The NDK's link stubs leave
/// these two functions out, so they are looked up in the device's library.
pub fn start_binder_threads() -> Result<(), String> {
    extern "C" {
        fn dlopen(filename: *const std::ffi::c_char, flags: i32) -> *mut std::ffi::c_void;
        fn dlsym(handle: *mut std::ffi::c_void, symbol: *const std::ffi::c_char) -> *mut std::ffi::c_void;
    }
    const RTLD_NOW: i32 = 2;
    unsafe {
        let library = dlopen(c"libbinder_ndk.so".as_ptr(), RTLD_NOW);
        if library.is_null() {
            return Err("libbinder_ndk.so did not load".into());
        }
        let set_count = dlsym(library, c"ABinderProcess_setThreadPoolMaxThreadCount".as_ptr());
        let start = dlsym(library, c"ABinderProcess_startThreadPool".as_ptr());
        if set_count.is_null() || start.is_null() {
            return Err("libbinder_ndk.so has no ABinderProcess functions".into());
        }
        let set_count: extern "C" fn(u32) -> bool = std::mem::transmute(set_count);
        let start: extern "C" fn() = std::mem::transmute(start);
        set_count(4);
        start();
    }
    Ok(())
}

/// Cb and Cr read at one point of a decoded frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    pub cb: u8,
    pub cr: u8,
}

pub struct DecodedFrame {
    pub width: usize,
    pub height: usize,
    pub samples: Vec<Sample>,
}

/// What the stream told the client when it started.
pub struct Stream {
    pub fps: f32,
    pub video: VideoConfig,
}

pub struct Decoder {
    sink: video_decoder::VideoDecoderSink,
    arrivals: Vec<Instant>,
    running: Arc<RelaxedAtomic>,
    display: Option<JoinHandle<()>>,
}

impl Decoder {
    pub fn new(
        config_nal: &[u8],
        stream: &Stream,
        context: Arc<ClientCoreContext>,
        points: Vec<(f32, f32)>,
        sender: Sender<DecodedFrame>,
    ) -> Result<Self, String> {
        let config = VideoDecoderConfig {
            codec: CodecType::Hevc,
            force_software_decoder: stream.video.force_software_decoder,
            max_buffering_frames: stream.video.max_buffering_frames,
            buffering_history_weight: stream.video.buffering_history_weight,
            options: stream.video.mediacodec_extra_options.clone(),
            config_buffer: config_nal.to_vec(),
        };
        let (sink, source) = video_decoder::create_decoder(config, {
            let context = Arc::clone(&context);
            move |result| match result {
                Ok(timestamp) => context.report_frame_decoded(timestamp),
                Err(e) => context.report_fatal_decoder_error(&e.to_string()),
            }
        });
        let running = Arc::new(RelaxedAtomic::new(true));
        let interval = Duration::from_secs_f32(1.0 / stream.fps);
        let display = thread::Builder::new()
            .name("display".into())
            .spawn({
                let running = Arc::clone(&running);
                move || display_loop(source, context, interval, points, sender, running)
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { sink, arrivals: Vec::new(), running, display: Some(display) })
    }

    pub fn decode(&mut self, timestamp_ns: u64, nal: &[u8]) -> Result<(), String> {
        self.arrivals.push(Instant::now());
        if self.sink.push_nal(Duration::from_nanos(timestamp_ns), nal) {
            Ok(())
        } else {
            Err("the decoder takes no more input now".into())
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // How evenly the frames came: jitter makes the client's decoder queue hold frames longer
        let mut gaps: Vec<f32> = self.arrivals.windows(2).map(|w| (w[1] - w[0]).as_secs_f32() * 1000.0).collect();
        gaps.sort_by(f32::total_cmp);
        if !gaps.is_empty() {
            let at = |q: f32| gaps[((gaps.len() - 1) as f32 * q) as usize];
            let short = gaps.iter().filter(|&&g| g < 3.0).count();
            println!(
                "probe: frame arrival gaps p10 {:.2} ms, median {:.2} ms, p90 {:.2} ms; {short} of {} under 3 ms",
                at(0.1),
                at(0.5),
                at(0.9),
                gaps.len()
            );
        }
        self.running.set(false);
        if let Some(display) = self.display.take() {
            display.join().ok();
        }
    }
}

/// Takes one image at each refresh, like the Quest's compositor. The image shows at the next
/// refresh, so the time from the submit to the display is one refresh interval.
fn display_loop(
    mut source: VideoDecoderSource,
    context: Arc<ClientCoreContext>,
    interval: Duration,
    points: Vec<(f32, f32)>,
    sender: Sender<DecodedFrame>,
    running: Arc<RelaxedAtomic>,
) {
    let graphics = GraphicsContext::new_gl();
    let reader = match unsafe { BadgeReader::new(&graphics.gl_context, points.len()) } {
        Ok(reader) => reader,
        Err(e) => {
            eprintln!("probe: no GL reader: {e}");
            return;
        }
    };
    let mut next = Instant::now();
    while running.value() {
        next += interval;
        thread::sleep(next.saturating_duration_since(Instant::now()));
        let Some((timestamp, buffer)) = source.get_frame() else { continue };
        context.report_compositor_start(timestamp);
        context.report_submit(timestamp, interval);
        // The read comes after the reports, so it does not count as the client's compose time.
        // The image stays alive until the next get_frame call.
        let Some(frame) = (unsafe { reader.read(&graphics, buffer, &points) }) else { continue };
        if sender.send(frame).is_err() {
            return;
        }
    }
}

/// Samples points of a decoded image on the GPU into a row of pixels: one pixel per point.
struct BadgeReader {
    program: glow::Program,
    uv: glow::UniformLocation,
    texture: glow::Texture,
    framebuffer: glow::Framebuffer,
    vertex_array: glow::VertexArray,
    count: usize,
}

const VERTEX: &str = "#version 300 es
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}";

// The external sampler turns the decoder's YUV into RGB, as the Quest client's shader does
const FRAGMENT: &str = "#version 300 es
#extension GL_OES_EGL_image_external_essl3 : require
precision mediump float;
uniform samplerExternalOES image;
uniform vec2 uv;
out vec4 color;
void main() { color = texture(image, uv); }";

impl BadgeReader {
    unsafe fn new(gl: &glow::Context, count: usize) -> Result<Self, String> {
        let program = gl.create_program()?;
        for (kind, source) in [(glow::VERTEX_SHADER, VERTEX), (glow::FRAGMENT_SHADER, FRAGMENT)] {
            let shader = gl.create_shader(kind)?;
            gl.shader_source(shader, source);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                return Err(gl.get_shader_info_log(shader));
            }
            gl.attach_shader(program, shader);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(gl.get_program_info_log(program));
        }
        let uv = gl.get_uniform_location(program, "uv").ok_or("no uv uniform")?;

        let texture = gl.create_texture()?;
        gl.bind_texture(GL_TEXTURE_EXTERNAL_OES, Some(texture));
        gl.tex_parameter_i32(GL_TEXTURE_EXTERNAL_OES, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
        gl.tex_parameter_i32(GL_TEXTURE_EXTERNAL_OES, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);

        let target = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(target));
        let pixels = glow::PixelUnpackData::Slice(None);
        gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, count as i32, 1, 0, glow::RGBA, glow::UNSIGNED_BYTE, pixels);
        let framebuffer = gl.create_framebuffer()?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
        gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(target), 0);
        let vertex_array = gl.create_vertex_array()?;
        Ok(Self { program, uv, texture, framebuffer, vertex_array, count })
    }

    /// Reads Cb and Cr at `points` (fractions of the width and the height) of the image in
    /// `buffer`, an AHardwareBuffer.
    unsafe fn read(&self, graphics: &GraphicsContext, buffer: *mut c_void, points: &[(f32, f32)]) -> Option<DecodedFrame> {
        let desc = HardwareBuffer::from_ptr(NonNull::new(buffer.cast())?).describe();
        let gl = &graphics.gl_context;
        let mut rgba = vec![0u8; self.count * 4];
        graphics.render_ahardwarebuffer_using_texture(buffer, self.texture, || {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.framebuffer));
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vertex_array));
            for (i, &(x, y)) in points.iter().enumerate() {
                gl.viewport(i as i32, 0, 1, 1);
                gl.uniform_2_f32(Some(&self.uv), x, y);
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
            }
            gl.read_pixels(0, 0, self.count as i32, 1, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut rgba)));
        });
        let (pixels, _) = rgba.as_chunks::<4>();
        let samples = pixels
            .iter()
            .map(|p| {
                let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
                // BT.709, full range
                let cb = 128.0 - 0.1146 * r - 0.3854 * g + 0.5 * b;
                let cr = 128.0 + 0.5 * r - 0.4542 * g - 0.0458 * b;
                Sample { cb: cb.clamp(0.0, 255.0) as u8, cr: cr.clamp(0.0, 255.0) as u8 }
            })
            .collect();
        Some(DecodedFrame { width: desc.width as usize, height: desc.height as usize, samples })
    }
}
