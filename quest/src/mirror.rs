// cranvr: a mirror of the first panel, which the overlay's Cranpose scene shows on a curved
// surface beside the panel (debug.cranvr.mirror=1). It shows how a picture drawn in the scene
// compares with the panel that the compositor samples once.
//
// A second decoder writes the panel's frames into an image reader (desktop_layer.rs). Each new
// picture is a hardware buffer in the decoder's own format. On the render thread the app draws
// it into an RGBA texture in ALVR's GL context, as ALVR's staging renderer does with the eye
// stream, and puts a fence after that. The scene's thread waits for the fence on the GPU and
// copies the texture into a frame of Cranpose's.

use alvr_common::error;
use alvr_graphics::{GraphicsContext, GL_TEXTURE_EXTERNAL_OES};
use glow::{self as gl, HasContext};
use ndk::media::image_reader::{AcquireResult, Image, ImageReader};

const VERTEX: &str = r#"#version 300 es
out vec2 uv;
void main() {
    vec2 screen_uv = vec2(gl_VertexID & 1, gl_VertexID >> 1);
    gl_Position = vec4((screen_uv - 0.5) * 2.0, 0.0, 1.0);
    // The buffer's first row is the picture's top; wgpu reads the texture's first row as its top
    uv = vec2(screen_uv.x, 1.0 - screen_uv.y);
}
"#;

const FRAGMENT: &str = r#"#version 300 es
#extension GL_OES_EGL_image_external_essl3 : enable
precision highp float;
uniform samplerExternalOES tex;
in vec2 uv;
out vec4 out_color;
void main() {
    out_color = vec4(texture(tex, uv).rgb, 1.0);
}
"#;

/// A new picture of the mirror for the scene: an RGBA texture of ALVR's GL share group, its size
/// and the fence after the commands that drew it, as a GL sync object's address.
#[derive(Clone, Copy, Debug)]
pub struct MirrorFrame {
    pub texture: u32,
    pub size: (u32, u32),
    pub fence: usize,
}

/// The second decoder's pictures, turned into textures for the scene at half their size: each
/// pixel of the texture is the mean of 2 by 2 pixels of the picture, as one linear sample halfway
/// between them. The surface spans about 75 degrees, so half of a 3840 pixel picture already
/// gives the headset's display about one pixel for each of its own, and the scene samples the
/// texture with no mipmaps.
pub struct Mirror {
    reader: ImageReader,
    size: (u32, u32),
    program: gl::Program,
    external: gl::Texture,
    /// The texture of the newest picture and its framebuffer
    target: Option<(gl::Texture, gl::Framebuffer, (u32, u32))>,
    /// The picture drawn last: its buffer stays until the next one is drawn
    held: Option<Image>,
}

impl Mirror {
    pub fn new(gfx: &GraphicsContext, reader: ImageReader, size: (u32, u32)) -> Result<Self, String> {
        let gl = &gfx.gl_context;
        gfx.make_current();
        unsafe {
            let program = gl.create_program()?;
            for (kind, source) in [(gl::VERTEX_SHADER, VERTEX), (gl::FRAGMENT_SHADER, FRAGMENT)] {
                let shader = gl.create_shader(kind)?;
                gl.shader_source(shader, source);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    return Err(format!("mirror shader: {}", gl.get_shader_info_log(shader)));
                }
                gl.attach_shader(program, shader);
                gl.delete_shader(shader);
            }
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                return Err(format!("mirror program: {}", gl.get_program_info_log(program)));
            }
            let external = gl.create_texture()?;
            Ok(Self { reader, size, program, external, target: None, held: None })
        }
    }

    /// Draws the newest picture into the mirror's texture, or returns `None` when the decoder
    /// has no new picture.
    pub fn next(&mut self, gfx: &GraphicsContext) -> Option<MirrorFrame> {
        let image = match self.reader.acquire_latest_image() {
            Ok(AcquireResult::Image(image)) => image,
            Ok(_) => return None,
            Err(e) => {
                error!("cranvr mirror: {e}");
                return None;
            }
        };
        let buffer = match image.hardware_buffer() {
            Ok(buffer) => buffer,
            Err(e) => {
                error!("cranvr mirror: {e}");
                return None;
            }
        };
        let gl = &gfx.gl_context;
        gfx.make_current();
        let (width, height) = self.frame_size();
        unsafe {
            let (texture, framebuffer) = match self.target {
                Some((texture, framebuffer, size)) if size == (width, height) => (texture, framebuffer),
                old => {
                    if let Some((texture, framebuffer, _)) = old {
                        gl.delete_framebuffer(framebuffer);
                        gl.delete_texture(texture);
                    }
                    let texture = gl.create_texture().ok()?;
                    gl.bind_texture(gl::TEXTURE_2D, Some(texture));
                    gl.tex_storage_2d(gl::TEXTURE_2D, 1, gl::RGBA8, width as i32, height as i32);
                    gl.bind_texture(gl::TEXTURE_2D, None);
                    let framebuffer = gl.create_framebuffer().ok()?;
                    gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, Some(framebuffer));
                    gl.framebuffer_texture_2d(gl::DRAW_FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::TEXTURE_2D, Some(texture), 0);
                    gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, None);
                    self.target = Some((texture, framebuffer, (width, height)));
                    (texture, framebuffer)
                }
            };
            let (program, external) = (self.program, self.external);
            gfx.render_ahardwarebuffer_using_texture(buffer.as_ptr().cast(), external, || {
                gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, Some(framebuffer));
                gl.viewport(0, 0, width as i32, height as i32);
                gl.disable(gl::SCISSOR_TEST);
                gl.disable(gl::STENCIL_TEST);
                gl.disable(gl::DEPTH_TEST);
                gl.disable(gl::BLEND);
                gl.use_program(Some(program));
                gl.active_texture(gl::TEXTURE0);
                gl.bind_texture(GL_TEXTURE_EXTERNAL_OES, Some(external));
                gl.bind_sampler(0, None);
                gl.draw_arrays(gl::TRIANGLE_STRIP, 0, 4);
            });
            gl.bind_framebuffer(gl::DRAW_FRAMEBUFFER, None);
            gl.use_program(None);
            let fence = gl.fence_sync(gl::SYNC_GPU_COMMANDS_COMPLETE, 0).ok()?;
            gl.flush();
            self.held = Some(image);
            Some(MirrorFrame { texture: texture.0.get(), size: (width, height), fence: fence.0 as usize })
        }
    }

    /// The size of the textures for the scene: half the picture's.
    pub fn frame_size(&self) -> (u32, u32) {
        ((self.size.0 / 2).max(1), (self.size.1 / 2).max(1))
    }
}
