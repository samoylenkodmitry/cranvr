// cranvr: a panel's bar (its handle and buttons) as a quad layer.
//
// The overlay layer drew the bar's text in the eye views, where it shimmered and showed stairs:
// the text was drawn small, from a larger picture, without the compositor's filters. The bar is
// now a picture of its own at 1600 pixels per meter, which the compositor samples like the panel,
// with its supersampling and sharpening. The app draws the picture again only when the bar
// changes, as when a ray moves onto a button.

use crate::panel::{Bar, Look};
use alvr_graphics::rasterize_text;
#[cfg(target_os = "android")]
use {alvr_graphics::GraphicsContext, glow::HasContext, openxr as xr, std::num::NonZeroU32};

/// The bar picture's pixels per meter: a 4 cm button is 64 pixels tall
const PIXELS_PER_METER: f32 = 1600.0;
#[cfg(target_os = "android")]
const GL_SRGB8_ALPHA8: u32 = 0x8C43;
/// The text's height as a share of a button's height
const TEXT_SHARE: f32 = 0.5;

fn handle_color(look: Look) -> [f32; 4] {
    match look {
        Look::Idle => [0.75, 0.77, 0.8, 0.6],
        Look::Hover => [1.0, 1.0, 1.0, 0.95],
        Look::Active => [0.45, 0.7, 1.0, 1.0],
        Look::Label => [0.0; 4],
    }
}

fn button_color(look: Look) -> [f32; 4] {
    match look {
        Look::Idle => [0.13, 0.14, 0.17, 0.85],
        Look::Label => [0.0; 4],
        Look::Active => [0.2, 0.42, 0.8, 0.95],
        Look::Hover => [0.36, 0.42, 0.55, 0.95],
    }
}

/// An RGBA picture, rows from the top, alpha not premultiplied.
struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<f32>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Self {
        Self { width, height, pixels: vec![0.0; (width * height * 4) as usize] }
    }

    /// Lays `color` with `coverage` over the pixel.
    fn blend(&mut self, x: u32, y: u32, color: [f32; 4], coverage: f32) {
        let i = ((y * self.width + x) * 4) as usize;
        let sa = color[3] * coverage;
        if sa <= 0.0 {
            return;
        }
        let da = self.pixels[i + 3];
        let oa = sa + da * (1.0 - sa);
        for c in 0..3 {
            self.pixels[i + c] = (color[c] * sa + self.pixels[i + c] * da * (1.0 - sa)) / oa;
        }
        self.pixels[i + 3] = oa;
    }

    /// A rectangle with round corners, centered at (cx, cy) pixels, with smooth edges.
    fn round_rect(&mut self, (cx, cy): (f32, f32), (w, h): (f32, f32), radius: f32, color: [f32; 4]) {
        let (hw, hh) = (w / 2.0, h / 2.0);
        let r = radius.min(hw).min(hh);
        let (x0, x1) = ((cx - hw - 1.0).max(0.0) as u32, ((cx + hw + 1.0) as u32).min(self.width));
        let (y0, y1) = ((cy - hh - 1.0).max(0.0) as u32, ((cy + hh + 1.0) as u32).min(self.height));
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = ((x as f32 + 0.5 - cx).abs(), (y as f32 + 0.5 - cy).abs());
                let (qx, qy) = (px - (hw - r), py - (hh - r));
                let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
                let distance = outside + qx.max(qy).min(0.0) - r;
                self.blend(x, y, color, (0.5 - distance).clamp(0.0, 1.0));
            }
        }
    }

    /// A white text, centered at (cx, cy) pixels, no wider than `max_width`.
    fn text(&mut self, text: &str, (cx, cy): (f32, f32), height: f32, max_width: f32) {
        let mut px = height / 0.75;
        let (mut w, mut h, mut alpha) = rasterize_text(text, px);
        if w as f32 > max_width {
            px *= max_width / w as f32;
            (w, h, alpha) = rasterize_text(text, px);
        }
        let (x0, y0) = ((cx - w as f32 / 2.0).round() as i32, (cy - h as f32 / 2.0).round() as i32);
        for y in 0..h {
            for x in 0..w {
                let (tx, ty) = (x0 + x as i32, y0 + y as i32);
                if (0..self.width as i32).contains(&tx) && (0..self.height as i32).contains(&ty) {
                    let coverage = alpha[(y * w + x) as usize] as f32 / 255.0;
                    self.blend(tx as u32, ty as u32, [1.0, 1.0, 1.0, 1.0], coverage);
                }
            }
        }
    }

    /// The picture as bytes, rows from the bottom as OpenGL reads them.
    fn bytes_bottom_up(&self) -> Vec<u8> {
        let row = (self.width * 4) as usize;
        let mut out = Vec::with_capacity(self.pixels.len());
        for y in (0..self.height as usize).rev() {
            out.extend(self.pixels[y * row..(y + 1) * row].iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8));
        }
        out
    }
}

/// The bar's picture: its size in pixels and its bytes.
fn draw(bar: &Bar) -> (u32, u32, Vec<u8>) {
    let width = (bar.size.0 * PIXELS_PER_METER).round() as u32;
    let height = (bar.size.1 * PIXELS_PER_METER).round() as u32;
    let mut canvas = Canvas::new(width.max(1), height.max(1));
    if let Some(color) = bar.background {
        canvas.round_rect((width as f32 / 2.0, height as f32 / 2.0), (width as f32, height as f32), 0.02 * PIXELS_PER_METER, color);
    }
    let to_px = |(x, y): (f32, f32)| (width as f32 / 2.0 + x * PIXELS_PER_METER, height as f32 / 2.0 - y * PIXELS_PER_METER);
    let size = |(w, h): (f32, f32)| (w * PIXELS_PER_METER, h * PIXELS_PER_METER);
    let (center, handle_size, look) = bar.handle;
    let handle = size(handle_size);
    canvas.round_rect(to_px(center), handle, handle.1 / 2.0, handle_color(look));
    for button in &bar.buttons {
        let (center, (w, h)) = (to_px(button.center), size(button.size));
        canvas.round_rect(center, (w, h), 0.3 * h, button_color(button.look));
        canvas.text(&button.label, center, h * TEXT_SHARE, w - 0.4 * h);
    }
    (canvas.width, canvas.height, canvas.bytes_bottom_up())
}

/// One bar's quad layer: the swapchain and what it shows.
///
/// A picture draws on a thread of its own: the console's took 13 ms on the Mac, which on the
/// Quest would cost the render thread several frames whenever a button's look changes. The old
/// picture stays until the new one is ready.
#[cfg(target_os = "android")]
pub struct BarLayer {
    /// The picture: its swapchain, its size in pixels and its size in meters
    picture: Option<(xr::Swapchain<xr::OpenGlEs>, (u32, u32), (f32, f32))>,
    /// What the picture shows, apart from the bar's place
    shown: String,
    /// A picture being drawn: what it shows, its size in meters and its pixels when done
    drawing: Option<(String, (f32, f32), std::sync::mpsc::Receiver<(u32, u32, Vec<u8>)>)>,
    pub bar: Bar,
}

#[cfg(target_os = "android")]
impl BarLayer {
    /// Keeps the bar's place, and draws its picture again when it changed. `old` is the panel's
    /// bar layer so far.
    pub fn update(
        old: Option<Self>,
        bar: Bar,
        session: &xr::Session<xr::OpenGlEs>,
        gfx: &GraphicsContext,
    ) -> Result<Self, String> {
        let wanted = format!("{:?}", (bar.size, bar.handle, &bar.buttons, bar.background));
        let mut layer = old.unwrap_or(Self { picture: None, shown: String::new(), drawing: None, bar: bar.clone() });
        layer.bar = bar;
        let done = match layer.drawing.as_ref().map(|(_, _, receiver)| receiver.try_recv()) {
            Some(Ok(picture)) => layer.drawing.take().map(|(shown, meters, _)| (picture, shown, meters)),
            // A drawing that ended without a picture is drawn again
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                layer.drawing = None;
                None
            }
            _ => None,
        };
        if let Some(((width, height, bytes), shown, meters)) = done {
            let mut swapchain = match layer.picture.take() {
                Some((swapchain, size, _)) if size == (width, height) => swapchain,
                _ => {
                    gfx.make_current();
                    session
                        .create_swapchain(&xr::SwapchainCreateInfo {
                            create_flags: xr::SwapchainCreateFlags::EMPTY,
                            usage_flags: xr::SwapchainUsageFlags::SAMPLED | xr::SwapchainUsageFlags::COLOR_ATTACHMENT,
                            format: GL_SRGB8_ALPHA8,
                            sample_count: 1,
                            width,
                            height,
                            face_count: 1,
                            array_size: 1,
                            mip_count: 1,
                        })
                        .map_err(|e| e.to_string())?
                }
            };
            upload(&mut swapchain, gfx, (width, height), &bytes)?;
            layer.picture = Some((swapchain, (width, height), meters));
            layer.shown = shown;
        }
        // One drawing at a time: a change meanwhile draws after it
        if layer.drawing.is_none() && layer.shown != wanted {
            let (sender, receiver) = std::sync::mpsc::channel();
            let bar = layer.bar.clone();
            std::thread::Builder::new()
                .name("bar-picture".into())
                .spawn(move || {
                    sender.send(draw(&bar)).ok();
                })
                .map_err(|e| e.to_string())?;
            layer.drawing = Some((wanted, layer.bar.size, receiver));
        }
        Ok(layer)
    }

    /// The quad layer, with the compositor's filters in `settings`, once a picture is drawn.
    pub fn layer<'a>(
        &'a self,
        space: &'a xr::Space,
        settings: Option<&'a xr::sys::CompositionLayerSettingsFB>,
    ) -> Option<xr::CompositionLayerQuad<'a, xr::OpenGlEs>> {
        let (swapchain, (width, height), (w, h)) = self.picture.as_ref()?;
        let (c, q) = (self.bar.center, self.bar.orientation);
        let quad = xr::CompositionLayerQuad::new()
            .layer_flags(xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA | xr::CompositionLayerFlags::UNPREMULTIPLIED_ALPHA)
            .space(space)
            .eye_visibility(xr::EyeVisibility::BOTH)
            .sub_image(xr::SwapchainSubImage::new().swapchain(swapchain).image_rect(xr::Rect2Di {
                offset: xr::Offset2Di { x: 0, y: 0 },
                extent: xr::Extent2Di { width: *width as i32, height: *height as i32 },
            }))
            .pose(xr::Posef {
                orientation: xr::Quaternionf { x: q.x, y: q.y, z: q.z, w: q.w },
                position: xr::Vector3f { x: c.x, y: c.y, z: c.z },
            })
            .size(xr::Extent2Df { width: *w, height: *h });
        Some(match settings {
            Some(settings) => unsafe {
                xr::CompositionLayerQuad::from_raw(xr::sys::CompositionLayerQuad {
                    next: std::ptr::from_ref(settings).cast(),
                    ..quad.into_raw()
                })
            },
            None => quad,
        })
    }
}

/// Writes a picture into the swapchain's next image.
#[cfg(target_os = "android")]
fn upload(swapchain: &mut xr::Swapchain<xr::OpenGlEs>, gfx: &GraphicsContext, (w, h): (u32, u32), bytes: &[u8]) -> Result<(), String> {
    gfx.make_current();
    let images = swapchain.enumerate_images().map_err(|e| e.to_string())?;
    let index = swapchain.acquire_image().map_err(|e| e.to_string())? as usize;
    swapchain.wait_image(xr::Duration::INFINITE).map_err(|e| e.to_string())?;
    unsafe {
        let texture = glow::NativeTexture(NonZeroU32::new(images[index]).ok_or("no texture")?);
        upload_rgba(&gfx.gl_context, texture, (w, h), bytes);
    }
    swapchain.release_image().map_err(|e| e.to_string())
}

/// Writes tightly packed RGBA rows into `texture`, bottom row first.
///
/// wgpu's GLES backend shares this context. It sets `UNPACK_ROW_LENGTH` and `UNPACK_IMAGE_HEIGHT`
/// for its own uploads and leaves them set, and its barriers can leave a pixel buffer bound. The
/// bar picture then came out skewed and torn on the headset: OpenGL read its rows with the row
/// length of some other texture, or read from that buffer. So every unpack setting goes back to
/// its default here first.
#[cfg(target_os = "android")]
pub(crate) unsafe fn upload_rgba(gl: &glow::Context, texture: glow::NativeTexture, (w, h): (u32, u32), bytes: &[u8]) {
    debug_assert_eq!(bytes.len(), (w * h * 4) as usize);
    gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
    gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
    gl.pixel_store_i32(glow::UNPACK_IMAGE_HEIGHT, 0);
    gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, 0);
    gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, 0);
    gl.pixel_store_i32(glow::UNPACK_SKIP_IMAGES, 0);
    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
    gl.bind_texture(glow::TEXTURE_2D, Some(texture));
    gl.tex_sub_image_2d(
        glow::TEXTURE_2D,
        0,
        0,
        0,
        w as i32,
        h as i32,
        glow::RGBA,
        glow::UNSIGNED_BYTE,
        glow::PixelUnpackData::Slice(Some(bytes)),
    );
    gl.bind_texture(glow::TEXTURE_2D, None);
    // wgpu expects OpenGL's default alignment
    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::BarButton;
    use alvr_common::glam::{Quat, Vec3};

    /// The console's picture is the largest one. It is opaque over the whole face, so the
    /// screens behind the console do not show through it.
    #[test]
    fn the_console_picture_is_an_opaque_face() {
        use crate::console::{layout, Menu};
        let games = ["Beat Saber", "Google Earth VR"].map(|n| crate::console::Game { name: n.into(), program: format!("{n}.exe") });
        let menu = Menu { games: games.to_vec(), fps: 120, game_scale: 0.6, msaa: 2, ..Default::default() };
        let (height, items) = layout(&menu);
        let bar = Bar {
            panel: 250,
            center: Vec3::ZERO,
            orientation: Quat::IDENTITY,
            size: (crate::console::WIDTH, height),
            handle: (crate::console::handle_center(height), (0.2, 0.014), Look::Idle),
            buttons: items.into_iter().map(|i| BarButton { center: i.center, size: i.size, label: i.label, look: Look::Idle }).collect(),
            background: Some([0.09, 0.1, 0.12, 1.0]),
        };
        let start = std::time::Instant::now();
        let (w, h, bytes) = draw(&bar);
        println!("console picture {w}x{h} in {:?}", start.elapsed());
        assert_eq!((w, h), (960, (height * PIXELS_PER_METER).round() as u32));
        let alpha = |x: u32, y: u32| bytes[((y * w + x) * 4 + 3) as usize];
        // the middle, the margins, and the corner outside the round face
        assert_eq!([alpha(w / 2, h / 2), alpha(w / 2, 2), alpha(2, h / 2)], [255; 3]);
        assert_eq!(alpha(0, 0), 0);
    }

    #[test]
    fn the_bar_picture_has_the_handle_and_the_buttons() {
        let bar = Bar {
            panel: 0,
            center: Vec3::ZERO,
            orientation: Quat::IDENTITY,
            size: (0.5, 0.052),
            handle: ((-0.15, 0.0), (0.2, 0.018), Look::Idle),
            buttons: vec![BarButton { center: (0.15, 0.0), size: (0.13, 0.04), label: "Pause".into(), look: Look::Hover }],
            background: None,
        };
        let (w, h, bytes) = draw(&bar);
        assert_eq!((w, h), (800, 83));
        let alpha = |x: u32, y: u32| bytes[(((h - 1 - y) * w + x) * 4 + 3) as usize];
        // the handle's middle, a button's corner and the empty gap between them
        assert!(alpha(160, 41) > 120);
        assert!(alpha(w / 2 + 5, 41) == 0);
        // the button's text is white on the button
        let row: Vec<u8> = (560..720).map(|x| bytes[(((h - 1 - 41) * w + x) * 4) as usize]).collect();
        assert!(row.iter().any(|&r| r > 200));
    }
}
