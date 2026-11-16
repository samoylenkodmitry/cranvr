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
    }
}

fn button_color(look: Look) -> [f32; 4] {
    match look {
        Look::Idle => [0.13, 0.14, 0.17, 0.85],
        _ => [0.36, 0.42, 0.55, 0.95],
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
#[cfg(target_os = "android")]
pub struct BarLayer {
    swapchain: xr::Swapchain<xr::OpenGlEs>,
    size: (u32, u32),
    /// What the picture shows, apart from the bar's place
    shown: String,
    pub bar: Bar,
}

#[cfg(target_os = "android")]
impl BarLayer {
    /// Draws the bar, or draws it again when it changed. `old` is the panel's bar layer so far.
    pub fn update(
        old: Option<Self>,
        bar: Bar,
        session: &xr::Session<xr::OpenGlEs>,
        gfx: &GraphicsContext,
    ) -> Result<Self, String> {
        let shown = format!("{:?}", (bar.size, bar.handle, &bar.buttons));
        if let Some(mut layer) = old {
            if layer.shown == shown {
                layer.bar = bar;
                return Ok(layer);
            }
            let (width, height, bytes) = draw(&bar);
            if (width, height) == layer.size {
                upload(&mut layer.swapchain, gfx, (width, height), &bytes)?;
                return Ok(Self { shown, bar, ..layer });
            }
        }
        let (width, height, bytes) = draw(&bar);
        gfx.make_current();
        let mut swapchain = session
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
            .map_err(|e| e.to_string())?;
        upload(&mut swapchain, gfx, (width, height), &bytes)?;
        Ok(Self { swapchain, size: (width, height), shown, bar })
    }

    /// The quad layer, with the compositor's filters in `settings`.
    pub fn layer<'a>(
        &'a self,
        space: &'a xr::Space,
        settings: Option<&'a xr::sys::CompositionLayerSettingsFB>,
    ) -> xr::CompositionLayerQuad<'a, xr::OpenGlEs> {
        let (c, q) = (self.bar.center, self.bar.orientation);
        let quad = xr::CompositionLayerQuad::new()
            .layer_flags(xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA | xr::CompositionLayerFlags::UNPREMULTIPLIED_ALPHA)
            .space(space)
            .eye_visibility(xr::EyeVisibility::BOTH)
            .sub_image(xr::SwapchainSubImage::new().swapchain(&self.swapchain).image_rect(xr::Rect2Di {
                offset: xr::Offset2Di { x: 0, y: 0 },
                extent: xr::Extent2Di { width: self.size.0 as i32, height: self.size.1 as i32 },
            }))
            .pose(xr::Posef {
                orientation: xr::Quaternionf { x: q.x, y: q.y, z: q.z, w: q.w },
                position: xr::Vector3f { x: c.x, y: c.y, z: c.z },
            })
            .size(xr::Extent2Df { width: self.bar.size.0, height: self.bar.size.1 });
        match settings {
            Some(settings) => unsafe {
                xr::CompositionLayerQuad::from_raw(xr::sys::CompositionLayerQuad {
                    next: std::ptr::from_ref(settings).cast(),
                    ..quad.into_raw()
                })
            },
            None => quad,
        }
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
        let gl = &gfx.gl_context;
        let texture = glow::NativeTexture(NonZeroU32::new(images[index]).ok_or("no texture")?);
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
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
    }
    swapchain.release_image().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::BarButton;
    use alvr_common::glam::{Quat, Vec3};

    #[test]
    fn the_bar_picture_has_the_handle_and_the_buttons() {
        let bar = Bar {
            panel: 0,
            center: Vec3::ZERO,
            orientation: Quat::IDENTITY,
            size: (0.5, 0.052),
            handle: ((-0.15, 0.0), (0.2, 0.018), Look::Idle),
            buttons: vec![BarButton { center: (0.15, 0.0), size: (0.13, 0.04), label: "Pause".into(), look: Look::Hover }],
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
