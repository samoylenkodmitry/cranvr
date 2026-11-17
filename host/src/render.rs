//! Metal compute shaders that write straight into the encoder's NV12 buffer, so a frame never
//! leaves the GPU before encoding:
//! - `pattern` draws the head-tracked test scene;
//! - `compose` copies the eyes of a game frame (IOSurfaces shared by the runtime under Wine).

use cranvr_common::foveation::Foveation;
use cranvr_vt::{ffi::*, PixelBuffer};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::NSString;
use objc2_metal::{
    MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLComputeCommandEncoder, MTLComputePipelineState,
    MTLBlitCommandEncoder, MTLCommandBufferStatus, MTLCreateSystemDefaultDevice, MTLDevice, MTLLibrary, MTLOrigin,
    MTLPixelFormat, MTLSize, MTLStorageMode, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
};
use std::{ffi::c_void, ptr, ptr::NonNull};

/// Must match `Params` in pattern.metal. Every field is 16 bytes or a group of four u32.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Params {
    pub fov: [[f32; 4]; 2],
    pub head_q: [f32; 4],
    pub head_p: [f32; 4],
    pub eye_q: [[f32; 4]; 2],
    pub eye_p: [[f32; 4]; 2],
    pub ctl: [[f32; 4]; 2],
    /// Desktop screen: center (w = 1 when shown), right and up half extents
    pub screen: [[f32; 4]; 3],
    /// Foveation: center size (xy) and center shift (zw)
    pub fov_center: [f32; 4],
    /// Foveation: edge ratio (xy) and eye size ratio (zw)
    pub fov_edge: [f32; 4],
    /// Encoded eye size
    pub eye_w: u32,
    pub eye_h: u32,
    pub foveated: u32,
    pub full_range: u32,
    /// 0: test scene with eye badges; 1: dim room for the desktop
    pub scene: u32,
    pub pad: [u32; 3],
}

/// Must match `ComposeParams` in pattern.metal.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct ComposeParams {
    /// Tangents of the stream's views: left, right, up, down
    pub fov: [[f32; 4]; 2],
    /// Tangents the game rendered with
    pub src_fov: [[f32; 4]; 2],
    /// The game's image rect in normalized texture coordinates: x, y, width, height
    pub src_rect: [[f32; 4]; 2],
    pub fov_center: [f32; 4],
    pub fov_edge: [f32; 4],
    pub eye_w: u32,
    pub eye_h: u32,
    pub foveated: u32,
    pub full_range: u32,
}

fn foveation_fields(f: Option<&Foveation>) -> (u32, [f32; 4], [f32; 4]) {
    match f {
        Some(f) => (
            1,
            [f.center_size[0], f.center_size[1], f.center_shift[0], f.center_shift[1]],
            [f.edge_ratio[0], f.edge_ratio[1], f.eye_size_ratio[0], f.eye_size_ratio[1]],
        ),
        None => (0, [0.0; 4], [0.0; 4]),
    }
}

impl Params {
    pub fn set_foveation(&mut self, f: Option<&Foveation>) {
        (self.foveated, self.fov_center, self.fov_edge) = foveation_fields(f);
    }
}

impl ComposeParams {
    pub fn set_foveation(&mut self, f: Option<&Foveation>) {
        (self.foveated, self.fov_center, self.fov_edge) = foveation_fields(f);
    }
}

pub struct Renderer {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    /// Desktop copies go on their own queue: a copy may wait for the window server, and the
    /// frames must not wait with it.
    upload_queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pattern: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    compose: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    texture_cache: CVMetalTextureCacheRef,
    /// The newest desktop capture, copied into GPU memory with mipmaps. Sampling a private copy
    /// keeps each frame from waiting on the window server, and the mipmaps keep small text from
    /// shimmering.
    desktop: Retained<ProtocolObject<dyn MTLTexture>>,
    /// The texture shown before `desktop`, reused for the next copy
    spare: Option<Retained<ProtocolObject<dyn MTLTexture>>>,
    /// The copy in flight and its target; the target replaces `desktop` once the copy is done
    upload: Option<(Upload, Retained<ProtocolObject<dyn MTLTexture>>)>,
    /// GPU execution time of the last dispatch, in seconds
    pub last_gpu_s: f64,
    /// GPU time of desktop copies since the caller last reset it, in seconds, and their count
    pub upload_gpu_s: f64,
    pub uploads: u32,
}

struct Upload {
    commands: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    _textures: CvTextures,
    _source: PixelBuffer,
}

impl Upload {
    fn done(&self) -> bool {
        matches!(self.commands.status(), MTLCommandBufferStatus::Completed | MTLCommandBufferStatus::Error)
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        if !self.done() {
            self.commands.waitUntilCompleted();
        }
    }
}

/// Releases the CoreVideo textures of one dispatch.
struct CvTextures(Vec<CVMetalTextureRef>);

impl Drop for CvTextures {
    fn drop(&mut self) {
        for t in &self.0 {
            unsafe { CFRelease(*t) };
        }
    }
}

impl Renderer {
    pub fn new() -> Result<Self, String> {
        let device = MTLCreateSystemDefaultDevice().ok_or("no Metal device")?;
        let queue = device.newCommandQueue().ok_or("no command queue")?;
        let upload_queue = device.newCommandQueue().ok_or("no command queue")?;
        let source = NSString::from_str(include_str!("pattern.metal"));
        let library = device
            .newLibraryWithSource_options_error(&source, None)
            .map_err(|e| format!("shader compile failed: {e}"))?;
        let pipeline = |name: &str| {
            let function = library.newFunctionWithName(&NSString::from_str(name)).ok_or(format!("no {name} function"))?;
            device.newComputePipelineStateWithFunction_error(&function).map_err(|e| format!("{name} pipeline failed: {e}"))
        };
        let (pattern, compose) = (pipeline("pattern")?, pipeline("compose")?);
        let mut texture_cache = ptr::null_mut();
        let status = unsafe {
            CVMetalTextureCacheCreate(
                kCFAllocatorDefault,
                ptr::null(),
                Retained::as_ptr(&device) as *mut c_void,
                ptr::null(),
                &mut texture_cache,
            )
        };
        if status != 0 {
            return Err(format!("CVMetalTextureCacheCreate failed: {status}"));
        }
        let desktop = mipmapped_texture(&device, 2, 2)?;
        Ok(Self {
            device,
            queue,
            upload_queue,
            pattern,
            compose,
            texture_cache,
            desktop,
            spare: None,
            upload: None,
            last_gpu_s: 0.0,
            upload_gpu_s: 0.0,
            uploads: 0,
        })
    }

    /// Starts copying a new desktop capture into GPU memory, with mipmaps. Returns false and
    /// does nothing while the previous copy runs; the caller offers the capture again later.
    /// The screen shows the capture from the first render after the copy is done.
    pub fn set_desktop(&mut self, frame: &PixelBuffer) -> Result<bool, String> {
        self.finish_upload();
        if self.upload.is_some() {
            return Ok(false);
        }
        let (w, h) = (frame.width(), frame.height());
        let target = match self.spare.take().filter(|t| t.width() == w && t.height() == h) {
            Some(t) => t,
            None => mipmapped_texture(&self.device, w, h)?,
        };
        let mut keep = CvTextures(Vec::new());
        let source = self.texture(frame, 0, MTLPixelFormat::BGRA8Unorm, w, h, &mut keep)?;
        unsafe {
            let commands = self.upload_queue.commandBuffer().ok_or("no command buffer")?;
            let blit = commands.blitCommandEncoder().ok_or("no blit encoder")?;
            blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                &*source,
                0,
                0,
                MTLOrigin { x: 0, y: 0, z: 0 },
                MTLSize { width: w, height: h, depth: 1 },
                &target,
                0,
                0,
                MTLOrigin { x: 0, y: 0, z: 0 },
            );
            blit.generateMipmapsForTexture(&target);
            blit.endEncoding();
            commands.commit();
            self.upload = Some((Upload { commands, _textures: keep, _source: frame.clone() }, target));
        }
        Ok(true)
    }

    /// Shows the copied capture once its copy is done.
    fn finish_upload(&mut self) {
        if self.upload.as_ref().is_some_and(|(u, _)| u.done()) {
            let (upload, texture) = self.upload.take().unwrap();
            self.upload_gpu_s += upload.commands.GPUEndTime() - upload.commands.GPUStartTime();
            self.uploads += 1;
            self.spare = Some(std::mem::replace(&mut self.desktop, texture));
        }
    }

    pub fn device_name(&self) -> String {
        self.device.name().to_string()
    }

    /// Draws the scene into `out` and waits for the GPU. The screen in `params` shows the last
    /// image given to `set_desktop`.
    pub fn render(&mut self, out: &PixelBuffer, params: &Params) -> Result<(), String> {
        self.finish_upload();
        let (pipeline, desktop) = (self.pattern.clone(), self.desktop.clone());
        self.run(&pipeline, out, &[], &[&desktop], bytes_of(params))
    }

    /// Copies the two eyes of a game frame into `out` and waits for the GPU. After this returns,
    /// the game may reuse its images.
    pub fn compose(&mut self, out: &PixelBuffer, left: &PixelBuffer, right: &PixelBuffer, params: &ComposeParams) -> Result<(), String> {
        let pipeline = self.compose.clone();
        self.run(&pipeline, out, &[left, right], &[], bytes_of(params))
    }

    fn texture(&self, buffer: &PixelBuffer, plane: usize, format: MTLPixelFormat, w: usize, h: usize, keep: &mut CvTextures) -> Result<*const ProtocolObject<dyn MTLTexture>, String> {
        let mut texture = ptr::null_mut();
        let status = unsafe {
            CVMetalTextureCacheCreateTextureFromImage(kCFAllocatorDefault, self.texture_cache, buffer.raw(), ptr::null(), format.0, w, h, plane, &mut texture)
        };
        if status != 0 || texture.is_null() {
            return Err(format!("no Metal texture for plane {plane}: {status}"));
        }
        keep.0.push(texture);
        Ok(unsafe { CVMetalTextureGetTexture(texture) } as *const ProtocolObject<dyn MTLTexture>)
    }

    fn run(
        &mut self,
        pipeline: &ProtocolObject<dyn MTLComputePipelineState>,
        out: &PixelBuffer,
        sources: &[&PixelBuffer],
        textures_after: &[&ProtocolObject<dyn MTLTexture>],
        params: &[u8],
    ) -> Result<(), String> {
        let (w, h) = (out.width(), out.height());
        let mut keep = CvTextures(Vec::new());
        let mut textures = vec![
            self.texture(out, 0, MTLPixelFormat::R8Unorm, w, h, &mut keep)?,
            self.texture(out, 1, MTLPixelFormat::RG8Unorm, w / 2, h / 2, &mut keep)?,
        ];
        for s in sources {
            // BGRA8Unorm, not the sRGB format: the shader wants the stored display values
            textures.push(self.texture(s, 0, MTLPixelFormat::BGRA8Unorm, s.width(), s.height(), &mut keep)?);
        }
        textures.extend(textures_after.iter().map(|t| *t as *const ProtocolObject<dyn MTLTexture>));
        let result = unsafe {
            let commands = self.queue.commandBuffer().ok_or("no command buffer")?;
            let encoder = commands.computeCommandEncoder().ok_or("no compute encoder")?;
            encoder.setComputePipelineState(pipeline);
            for (i, t) in textures.iter().enumerate() {
                encoder.setTexture_atIndex(Some(&**t), i);
            }
            encoder.setBytes_length_atIndex(NonNull::new(params.as_ptr() as *mut c_void).unwrap(), params.len(), 0);
            let grid = MTLSize { width: w.div_ceil(2), height: h.div_ceil(2), depth: 1 };
            encoder.dispatchThreads_threadsPerThreadgroup(grid, MTLSize { width: 16, height: 16, depth: 1 });
            encoder.endEncoding();
            commands.commit();
            commands.waitUntilCompleted();
            self.last_gpu_s = commands.GPUEndTime() - commands.GPUStartTime();
            match commands.error() {
                Some(e) => Err(format!("GPU error: {e}")),
                None => Ok(()),
            }
        };
        drop(keep);
        unsafe { CVMetalTextureCacheFlush(self.texture_cache, 0) };
        result
    }
}

fn mipmapped_texture(device: &ProtocolObject<dyn MTLDevice>, w: usize, h: usize) -> Result<Retained<ProtocolObject<dyn MTLTexture>>, String> {
    unsafe {
        let d = MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(MTLPixelFormat::BGRA8Unorm, w, h, true);
        d.setStorageMode(MTLStorageMode::Private);
        d.setUsage(MTLTextureUsage::ShaderRead);
        device.newTextureWithDescriptor(&d).ok_or_else(|| format!("no {w}x{h} desktop texture"))
    }
}

fn bytes_of<T: Copy>(value: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts(value as *const T as *const u8, std::mem::size_of::<T>()) }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe { CFRelease(self.texture_cache) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranvr_vt::{Encoder, EncoderConfig};
    use std::sync::mpsc;

    fn identity_params(eye_w: u32, eye_h: u32) -> Params {
        Params {
            fov: [[-1.0, 1.0, 1.0, -1.0]; 2],
            head_q: [0.0, 0.0, 0.0, 1.0],
            head_p: [0.0, 1.6, 0.0, 0.0],
            eye_q: [[0.0, 0.0, 0.0, 1.0]; 2],
            eye_p: [[-0.032, 0.0, 0.0, 0.0], [0.032, 0.0, 0.0, 0.0]],
            eye_w,
            eye_h,
            full_range: 1,
            ..Default::default()
        }
    }

    /// With foveation the badges move to where the stock ALVR client expects them.
    #[test]
    fn foveated_badges_land_where_the_client_unwarps_them() {
        use cranvr_common::foveation::{view_to_frame, FoveationConfig};
        let f = Foveation::new([1024, 1088], &FoveationConfig::default());
        let (eye_w, eye_h) = (f.optimized[0], f.optimized[1]);
        let (tx, _rx) = mpsc::channel();
        let config = EncoderConfig { width: eye_w * 2, height: eye_h, fps: 90.0, bitrate_bps: 10_000_000, full_range: true, strict_rate: false };
        let encoder = Encoder::new(&config, tx).expect("encoder");
        let mut buffer = encoder.pixel_buffer().expect("buffer");
        let mut params = identity_params(eye_w, eye_h);
        params.set_foveation(Some(&f));
        Renderer::new().expect("renderer").render(&buffer, &params).expect("render");
        let (w, h) = (buffer.width() as f32, buffer.height() as f32);
        buffer.with_planes_mut(|_, _, uv, uvs| {
            let chroma_at = |p: [f32; 2]| {
                let (x, y) = ((p[0] * w) as usize, (p[1] * h) as usize);
                (uv[(y / 2) * uvs + (x / 2) * 2], uv[(y / 2) * uvs + (x / 2) * 2 + 1])
            };
            let (cb_l, cr_l) = chroma_at(view_to_frame(Some(&f), 0, [0.11, 0.5]));
            let (cb_r, cr_r) = chroma_at(view_to_frame(Some(&f), 1, [0.11, 0.5]));
            assert!(cr_l > 200 && cb_l < 128, "left badge cb {cb_l} cr {cr_l}");
            assert!(cb_r > 200 && cr_r < 128, "right badge cb {cb_r} cr {cr_r}");
        });
    }

    /// The luma of the left eye of an encoded frame, `eye_w` pixels wide, as rows.
    fn left_luma(buffer: &mut PixelBuffer, eye_w: usize, eye_h: usize) -> Vec<f32> {
        buffer.with_planes_mut(|y, ys, _, _| (0..eye_h).flat_map(|row| (0..eye_w).map(move |x| (row, x))).map(|(row, x)| f32::from(y[row * ys + x]) / 255.0).collect())
    }

    /// A game's thin line in the compressed edge of a foveated frame stays continuous. The test
    /// paints a line of one pixel across the top left of the game's eye image, composes the
    /// frame with ALVR's default foveation and follows the line through the encoded picture. A
    /// compose that takes one sample for each encoded pixel misses the line in some columns,
    /// which the headset unwarps into a row of dots.
    #[test]
    fn a_thin_game_line_stays_continuous_in_the_compressed_edge() {
        use cranvr_common::foveation::FoveationConfig;
        let eye = [1680usize, 1760];
        let f = Foveation::new([eye[0] as u32, eye[1] as u32], &FoveationConfig::default());
        let (ow, oh) = (f.optimized[0] as usize, f.optimized[1] as usize);
        // The line y = 60 + 0.37 x, one pixel thick in the game's image
        let line = |x: usize| 60.0 + 0.37 * x as f32;
        let paint = |buffer: &mut PixelBuffer| {
            buffer.with_bgra_mut(|data, stride| {
                for y in 0..eye[1] {
                    for x in 0..eye[0] {
                        let on = (y as f32 + 0.5 - line(x)).abs() < 0.5 * (1.0 + 0.37f32 * 0.37).sqrt();
                        let v = if on { 255 } else { 40 };
                        data[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&[v, v, v, 255]);
                    }
                }
            });
        };
        let (mut left, mut right) = (PixelBuffer::new_bgra_shared(eye[0], eye[1]).unwrap(), PixelBuffer::new_bgra_shared(eye[0], eye[1]).unwrap());
        paint(&mut left);
        paint(&mut right);
        let (tx, _rx) = mpsc::channel();
        let config = EncoderConfig { width: ow as u32 * 2, height: oh as u32, fps: 90.0, bitrate_bps: 10_000_000, full_range: true, strict_rate: false };
        let encoder = Encoder::new(&config, tx).expect("encoder");
        let mut buffer = encoder.pixel_buffer().expect("buffer");
        let mut params = ComposeParams {
            fov: [[-1.0, 1.0, 1.0, -1.0]; 2],
            src_fov: [[-1.0, 1.0, 1.0, -1.0]; 2],
            src_rect: [[0.0, 0.0, 1.0, 1.0]; 2],
            eye_w: ow as u32,
            eye_h: oh as u32,
            full_range: 1,
            ..Default::default()
        };
        params.set_foveation(Some(&f));
        let mut renderer = Renderer::new().expect("renderer");
        let mut gpu = 0.0;
        for round in 0..40 {
            renderer.compose(&buffer, &left, &right, &params).expect("compose");
            if round >= 20 {
                gpu += renderer.last_gpu_s / 20.0;
            }
        }
        let encoded = left_luma(&mut buffer, ow, oh);
        // Follow the line over the view columns 30 to 330, where the vertical compression is the
        // edge ratio of 5: for each encoded column, the brightest pixel near where the line lies
        let mut peaks = Vec::new();
        for view_x in (30..330).step_by(2) {
            let e = f.view_to_encoded([(view_x as f32 + 0.5) / eye[0] as f32, line(view_x) / eye[1] as f32]);
            let (cx, cy) = ((e[0] * ow as f32) as usize, (e[1] * oh as f32) as usize);
            let peak = (cy.saturating_sub(2)..=(cy + 2).min(oh - 1)).map(|y| encoded[y * ow + cx]).fold(0.0f32, f32::max);
            peaks.push(peak);
        }
        let mean = peaks.iter().sum::<f32>() / peaks.len() as f32;
        let least = peaks.iter().copied().fold(f32::MAX, f32::min);
        let background = 40.0 / 255.0;
        println!("thin line in the encoded edge: mean peak {mean:.3}, least {least:.3}, background {background:.3}, compose GPU time {:.2} ms", gpu * 1e3);
        // One sample per pixel leaves columns without the line: its least peak is the background
        assert!(least > background + 0.25 * (mean - background), "the line breaks up: mean {mean}, least {least}");
    }

    /// Renders into a real encoder buffer and checks the eye badges and the floor on the CPU.
    #[test]
    fn renders_badges_and_floor() {
        let (eye_w, eye_h) = (256u32, 256u32);
        let (tx, _rx) = mpsc::channel();
        let config = EncoderConfig { width: eye_w * 2, height: eye_h, fps: 90.0, bitrate_bps: 10_000_000, full_range: true, strict_rate: false };
        let encoder = Encoder::new(&config, tx).expect("encoder");
        let mut buffer = encoder.pixel_buffer().expect("buffer");
        let mut renderer = Renderer::new().expect("renderer");
        renderer.render(&buffer, &identity_params(eye_w, eye_h)).expect("render");
        let w = buffer.width();
        buffer.with_planes_mut(|y, ys, uv, uvs| {
            let chroma_at = |x: usize, row: usize| (uv[(row / 2) * uvs + (x / 2) * 2], uv[(row / 2) * uvs + (x / 2) * 2 + 1]);
            // badge centers: u = 0.11, v = 0.5 in each eye
            let (cb_l, cr_l) = chroma_at((0.11 * eye_w as f32) as usize, eye_h as usize / 2);
            let (cb_r, cr_r) = chroma_at(eye_w as usize + (0.11 * eye_w as f32) as usize, eye_h as usize / 2);
            assert!(cr_l > 200 && cb_l < 128, "left badge cb {cb_l} cr {cr_l}");
            assert!(cb_r > 200 && cr_r < 128, "right badge cb {cb_r} cr {cr_r}");
            // bottom middle of the left eye looks down at the floor: gray, so chroma near 128
            let (cb_f, cr_f) = chroma_at(eye_w as usize / 2, eye_h as usize - 4);
            assert!((cb_f as i32 - 128).abs() < 40 && (cr_f as i32 - 128).abs() < 40, "floor cb {cb_f} cr {cr_f}");
            assert!(y[(eye_h as usize - 4) * ys + w / 4] > 30, "floor is lit");
        });
    }

    /// The desktop scene: a green desktop on a screen straight ahead fills the middle of each
    /// eye, and the eye badges are gone.
    #[test]
    fn renders_desktop_screen() {
        let (eye_w, eye_h) = (256u32, 256u32);
        let (tx, _rx) = mpsc::channel();
        let config = EncoderConfig { width: eye_w * 2, height: eye_h, fps: 90.0, bitrate_bps: 10_000_000, full_range: true, strict_rate: false };
        let encoder = Encoder::new(&config, tx).expect("encoder");
        let mut buffer = encoder.pixel_buffer().expect("buffer");
        let mut desktop = PixelBuffer::new_bgra(64, 40).expect("desktop");
        desktop.with_bgra_mut(|data, stride| {
            for row in 0..40 {
                for x in 0..64 {
                    data[row * stride + x * 4..row * stride + x * 4 + 4].copy_from_slice(&[20, 220, 20, 255]);
                }
            }
        });
        let mut params = identity_params(eye_w, eye_h);
        params.scene = 1;
        params.screen = [[0.0, 1.6, -1.4, 1.0], [1.1, 0.0, 0.0, 0.0], [0.0, 0.69, 0.0, 0.0]];
        let mut renderer = Renderer::new().expect("renderer");
        assert!(renderer.set_desktop(&desktop).expect("desktop upload"));
        // wait for the copy, so the render shows it
        renderer.upload.as_ref().unwrap().0.commands.waitUntilCompleted();
        renderer.render(&buffer, &params).expect("render");
        buffer.with_planes_mut(|y, ys, uv, uvs| {
            let chroma_at = |x: usize, row: usize| (uv[(row / 2) * uvs + (x / 2) * 2], uv[(row / 2) * uvs + (x / 2) * 2 + 1]);
            for eye in 0..2 {
                let x = eye * eye_w as usize + eye_w as usize / 2;
                let (cb, cr) = chroma_at(x, eye_h as usize / 2);
                assert!(cb < 100 && cr < 100 && y[(eye_h as usize / 2) * ys + x] > 120, "eye {eye}: screen middle cb {cb} cr {cr}");
            }
            let (cb, cr) = chroma_at((0.11 * eye_w as f32) as usize, eye_h as usize / 2);
            assert!(cr < 160, "no red badge in the desktop scene: cb {cb} cr {cr}");
        });
    }
}

#[cfg(test)]
mod compose_tests {
    use super::*;
    use cranvr_common::foveation::{view_to_frame, FoveationConfig};
    use cranvr_vt::{Encoder, EncoderConfig};
    use std::sync::mpsc;

    fn solid(w: usize, h: usize, bgra: [u8; 4]) -> PixelBuffer {
        let mut b = PixelBuffer::new_bgra(w, h).expect("bgra buffer");
        b.with_bgra_mut(|data, stride| {
            for row in 0..h {
                for x in 0..w {
                    data[row * stride + x * 4..row * stride + x * 4 + 4].copy_from_slice(&bgra);
                }
            }
        });
        b
    }

    /// Two game eyes (red left, blue right) composed into a foveated frame. The game rendered a
    /// wider field of view than the stream, so the frame must show only the matching middle.
    #[test]
    fn composes_game_eyes_with_foveation_and_fov_remap() {
        let f = Foveation::new([1024, 1088], &FoveationConfig::default());
        let (tx, _rx) = mpsc::channel();
        let config = EncoderConfig { width: f.optimized[0] * 2, height: f.optimized[1], fps: 90.0, bitrate_bps: 10_000_000, full_range: true, strict_rate: false };
        let encoder = Encoder::new(&config, tx).expect("encoder");
        let mut out = encoder.pixel_buffer().expect("out");
        // left eye: red with a green right half, so the test can see where the remap samples
        let mut left = solid(512, 512, [0, 0, 255, 255]);
        left.with_bgra_mut(|data, stride| {
            for row in 0..512 {
                for x in 256..512 {
                    data[row * stride + x * 4..row * stride + x * 4 + 4].copy_from_slice(&[0, 255, 0, 255]);
                }
            }
        });
        let right = solid(512, 512, [255, 0, 0, 255]);
        let mut p = ComposeParams {
            fov: [[-1.0, 1.0, 1.0, -1.0]; 2],
            // the game rendered twice as wide: tan 2 on each side
            src_fov: [[-2.0, 2.0, 2.0, -2.0]; 2],
            src_rect: [[0.0, 0.0, 1.0, 1.0]; 2],
            eye_w: f.optimized[0],
            eye_h: f.optimized[1],
            full_range: 1,
            ..Default::default()
        };
        p.set_foveation(Some(&f));
        Renderer::new().expect("renderer").compose(&out, &left, &right, &p).expect("compose");
        let (w, h) = (out.width() as f32, out.height() as f32);
        out.with_planes_mut(|_, _, uv, uvs| {
            let at = |eye, view: [f32; 2]| {
                let q = view_to_frame(Some(&f), eye, view);
                let (x, y) = ((q[0] * w) as usize, (q[1] * h) as usize);
                (uv[(y / 2) * uvs + (x / 2) * 2], uv[(y / 2) * uvs + (x / 2) * 2 + 1])
            };
            // stream view x 0.3 is tan -0.4, which is source x 0.4: still in the red half
            let (cb, cr) = at(0, [0.3, 0.5]);
            assert!(cr > 200 && cb < 128, "left eye, left side: cb {cb} cr {cr}");
            // stream view x 0.7 is tan 0.4, source x 0.6: the green half (both chroma low)
            let (cb, cr) = at(0, [0.7, 0.5]);
            assert!(cr < 100 && cb < 100, "left eye, right side: cb {cb} cr {cr}");
            let (cb, cr) = at(1, [0.5, 0.5]);
            assert!(cb > 200 && cr < 128, "right eye: cb {cb} cr {cr}");
        });
    }
}
