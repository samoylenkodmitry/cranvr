// cranvr: a 3D scene that Cranpose draws for both eyes, in a projection layer of its own.
//
// A thread of its own holds the Cranpose composition and a GL context of its own, in the share
// group of ALVR's context: Cranpose's wgpu never touches ALVR's GL state. The layer's swapchain is
// two eyes wide. Each image lives within one frame, as with ALVR's own overlay: the render thread
// acquires it when the frame's eyes and shapes are known and hands it to the scene's thread. The
// scene's thread draws both eyes and puts a fence after its GL commands. Before the frame ends,
// the render thread takes the image back, has ALVR's context wait for the fence on the GPU and
// releases the image. No thread waits for the GPU. An image that the compositor still reads after more than
// a frame stalls both threads, so no image waits across frames for the scene. Cranpose draws only
// when the scene looks different. An image it did not draw into holds an older picture, so the
// render thread keeps it for the next frame instead of releasing it.
//
// The scene shows the overlay's draws (panel.rs): the controllers, their rays, the panels'
// corners and the console's frame, as meshes lit like the overlay's shader lit them.

use crate::graphics::{ProjectionLayerAlphaConfig, ProjectionLayerBuilder};
use crate::xr_eye::{fitted, scene_direction, scene_transform, tangents, Eye};
use alvr_common::glam::{Mat4 as GMat4, UVec2, Vec3 as GVec3};
use alvr_common::{error, info};
use glow::HasContext;
use alvr_graphics::{GraphicsContext, OverlayDraw, OverlayShape};
use cranpose::prelude::*;
use cranpose::{ExternalFrames, GpuContext, GuestView};
use cranpose::space::{Camera, Light, Mat4, Material, Mesh, MeshSurface, Scene3d, Vec3};
use khronos_egl as egl;
use openxr as xr;
use std::{
    ffi::c_void,
    num::NonZeroU32,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const GL_SRGB8_ALPHA8: u32 = 0x8C43;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// The overlay's meshes, made once: the same meshes are equal.
#[derive(Clone)]
struct Meshes {
    cube: Arc<Mesh>,
    sphere: Arc<Mesh>,
    cylinder: Arc<Mesh>,
    plane: Arc<Mesh>,
}

impl PartialEq for Meshes {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.cube, &other.cube)
    }
}

impl Meshes {
    fn new() -> Self {
        Self {
            cube: Arc::new(Mesh::cuboid()),
            sphere: Arc::new(Mesh::sphere(24, 12)),
            cylinder: Arc::new(cylinder()),
            plane: Arc::new(Mesh::plane()),
        }
    }
}

/// The overlay's cylinder: radius 0.5 about z, from z = -0.5 to 0.5, closed at both ends.
fn cylinder() -> Mesh {
    const SEGMENTS: u32 = 20;
    let (mut positions, mut uvs, mut normals, mut indices) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for i in 0..=SEGMENTS {
        let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let (x, y) = (a.cos() * 0.5, a.sin() * 0.5);
        for z in [-0.5, 0.5] {
            positions.push([x, y, z]);
            uvs.push([i as f32 / SEGMENTS as f32, z + 0.5]);
            normals.push([a.cos(), a.sin(), 0.0]);
        }
    }
    for i in 0..SEGMENTS {
        let (a, b, c, d) = (2 * i, 2 * i + 1, 2 * i + 2, 2 * i + 3);
        indices.extend([a, c, b, b, c, d]);
    }
    for z in [-0.5f32, 0.5] {
        let center = positions.len() as u32;
        positions.push([0.0, 0.0, z]);
        uvs.push([0.5, 0.5]);
        normals.push([0.0, 0.0, z.signum()]);
        for i in 0..=SEGMENTS {
            let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            positions.push([a.cos() * 0.5, a.sin() * 0.5, z]);
            uvs.push([0.5 + a.cos() * 0.5, 0.5 + a.sin() * 0.5]);
            normals.push([0.0, 0.0, z.signum()]);
        }
        for i in 0..SEGMENTS {
            indices.extend([center, center + 1 + i, center + 2 + i]);
        }
    }
    Mesh::new(positions, uvs, Some(normals), indices).unwrap_or_else(|_| Mesh::cuboid())
}

/// The mirror's curved surface: its mesh, made for its size in meters, and its content's size in
/// pixels. The same surfaces are equal.
#[derive(Clone)]
struct MirrorSurface {
    mesh: Arc<Mesh>,
    meters: (f32, f32),
    content: (f32, f32),
}

impl PartialEq for MirrorSurface {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.mesh, &other.mesh)
    }
}

/// The widest content of the mirror in pixels. The surface covers about 750 pixels of an eye at
/// a meter and a half, so more pixels would only shimmer.
const MIRROR_CONTENT_WIDTH: f32 = 1920.0;

/// A screen of `width` by `height` meters bent into an S: across it the surface goes one full
/// sine wave forward and back by `depth`. The picture's top left corner is at u, v = 0, 0.
/// OpenXR's axes: x right, y up, z toward the viewer. The surface has two sides: Cranpose draws
/// a triangle only from its front, so each triangle comes again turned the other way. From behind
/// the picture shows reversed, as through glass.
fn s_surface(width: f32, height: f32, depth: f32) -> Result<Mesh, String> {
    const COLUMNS: u32 = 64;
    let (mut positions, mut uvs, mut normals, mut indices) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for i in 0..=COLUMNS {
        let u = i as f32 / COLUMNS as f32;
        let angle = u * std::f32::consts::TAU;
        let (x, z) = ((u - 0.5) * width, depth * angle.sin());
        // The surface's slope across, dz/dx, turns its normal
        let slope = depth * std::f32::consts::TAU / width * angle.cos();
        let length = (1.0 + slope * slope).sqrt();
        for (y, v) in [(height / 2.0, 0.0), (-height / 2.0, 1.0)] {
            positions.push([x, y, z]);
            uvs.push([u, v]);
            normals.push([-slope / length, 0.0, 1.0 / length]);
        }
    }
    for i in 0..COLUMNS {
        let (a, b, c, d) = (2 * i, 2 * i + 1, 2 * i + 2, 2 * i + 3);
        indices.extend([a, b, c, b, d, c]);
    }
    // The back: the same places with the normals turned around, and each triangle wound the
    // other way
    let back = positions.len() as u32;
    positions.extend_from_within(..);
    uvs.extend_from_within(..);
    normals.extend_from_within(..);
    for normal in &mut normals[back as usize..] {
        *normal = normal.map(|c| -c);
    }
    for i in 0..COLUMNS {
        let (a, b, c, d) = (back + 2 * i, back + 2 * i + 1, back + 2 * i + 2, back + 2 * i + 3);
        indices.extend([a, c, b, b, c, d]);
    }
    Mesh::new(positions, uvs, Some(normals), indices).map_err(|e| format!("{e:?}"))
}

#[composable]
fn Mirror(bitmap: ImageBitmap, surface: MirrorSurface, place: MutableState<Option<[[f32; 4]; 4]>>) {
    let mesh = Arc::clone(&surface.mesh);
    let (width, height) = surface.content;
    Box(
        Modifier::empty().required_size(Size { width, height }).mesh_surface(move || MeshSurface {
            mesh: Arc::clone(&mesh),
            transform: Mat4::from_rows(place.with(|p| p.unwrap_or(Mat4::default().rows()))),
            material: Material::Unlit,
            casts_shadow: false,
            solid: false,
        }),
        BoxSpec::default(),
        move || {
            Image(bitmap.clone(), None, Modifier::empty().fill_max_size(), Alignment::CENTER, ContentScale::FillBounds, 1.0, None);
        },
    );
}

/// What a shape of the overlay looks like; its place comes apart, as it changes every frame.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Look {
    Box,
    Sphere,
    Cylinder,
    /// A flat rectangle of this size in content pixels with round corners; its alpha drops by
    /// `fade` from its bottom edge to its top edge
    Flat { width: f32, height: f32, radius: f32, fade: f32 },
}

/// One shape: its look and its color.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Shape {
    look: Look,
    color: [f32; 4],
}

/// The overlay's draws as shapes and their places in the scene.
fn shapes(draws: &[OverlayDraw]) -> (Vec<Shape>, Vec<[[f32; 4]; 4]>) {
    let mut shapes = Vec::with_capacity(draws.len());
    let mut places = Vec::with_capacity(draws.len());
    for draw in draws {
        let (look, model) = match draw.shape {
            OverlayShape::Box => (Look::Box, draw.model),
            // Cranpose's sphere has a radius of one, the overlay's of a half
            OverlayShape::Sphere => (Look::Sphere, draw.model * GMat4::from_scale(GVec3::splat(0.5))),
            OverlayShape::Cylinder => (Look::Cylinder, draw.model),
            OverlayShape::RoundRect { width, height, radius, fade } => {
                // The plane spans 0 to 1 in x and y; the content's pixels are a share of a meter
                let scale = FLAT_PIXELS / width.max(height);
                let look = Look::Flat {
                    width: (width * scale).max(2.0),
                    height: (height * scale).max(2.0),
                    radius: radius * scale,
                    fade,
                };
                let corner = GMat4::from_translation(GVec3::new(-width / 2.0, -height / 2.0, 0.0));
                (look, draw.model * corner * GMat4::from_scale(GVec3::new(width, height, 1.0)))
            }
            OverlayShape::Label { .. } => continue,
        };
        shapes.push(Shape { look, color: draw.color });
        places.push(scene_transform(&model));
    }
    (shapes, places)
}

/// The larger side of a flat shape's content, in pixels
const FLAT_PIXELS: f32 = 64.0;

#[composable]
fn Solid(index: usize, shape: Shape, places: MutableState<Vec<[[f32; 4]; 4]>>, meshes: Meshes) {
    let [r, g, b, a] = shape.color;
    let color = Color(r, g, b, a);
    let (mesh, material, size) = match shape.look {
        Look::Box => (meshes.cube.clone(), Material::Lit, (8.0, 8.0)),
        Look::Sphere => (meshes.sphere.clone(), Material::Lit, (8.0, 8.0)),
        Look::Cylinder => (meshes.cylinder.clone(), Material::Lit, (8.0, 8.0)),
        Look::Flat { width, height, .. } => (meshes.plane.clone(), Material::Unlit, (width, height)),
    };
    let surface = Modifier::empty()
        .required_size(Size { width: size.0, height: size.1 })
        .mesh_surface(move || MeshSurface {
            mesh: Arc::clone(&mesh),
            transform: Mat4::from_rows(places.with(|p| p.get(index).copied().unwrap_or(Mat4::default().rows()))),
            material: material.clone(),
            casts_shadow: false,
            solid: false,
        });
    let surface = match shape.look {
        // The plane's content runs from the shape's bottom edge up; a ray fades toward its end
        Look::Flat { height, fade, .. } if fade > 0.0 => {
            let brush = Brush::vertical_gradient(vec![color, Color(r, g, b, a * (1.0 - fade))], 0.0, height);
            surface.draw_behind(move |scope| scope.draw_rect(brush.clone()))
        }
        Look::Flat { radius, .. } => surface.rounded_corners(radius).background(color),
        _ => surface.background(color),
    };
    Box(surface, BoxSpec::default(), || {});
}

/// The mirror's states: its newest picture, its surface and where the surface stands.
#[derive(Clone, Copy, PartialEq)]
struct MirrorStates {
    bitmap: MutableState<Option<ImageBitmap>>,
    surface: MutableState<Option<MirrorSurface>>,
    place: MutableState<Option<[[f32; 4]; 4]>>,
}

/// The eyes that the scene shows.
#[derive(Clone, Debug, Default, PartialEq)]
struct Eyes {
    eyes: Option<[Eye; 2]>,
}

/// The light of the overlay's shapes: from above, ahead and right, as the overlay's shader had it
fn light() -> Light {
    let travel = scene_direction([-0.3, -1.0, -0.6]);
    Light { direction: travel, ambient: 0.45, shadows: false, size: 0.0 }
}

#[composable]
fn Scene(
    eyes: MutableState<Eyes>,
    boxes: MutableState<[(f32, f32); 2]>,
    mirror: MirrorStates,
    shapes: MutableState<Vec<Shape>>,
    places: MutableState<Vec<[[f32; 4]; 4]>>,
    cell: (f32, f32),
    flip: bool,
    meshes: Meshes,
) {
    Row(Modifier::empty(), RowSpec::default(), move || {
        for side in 0..2 {
            let meshes = meshes.clone();
            key(side, move || {
                // Each eye's box sits in the top left corner of its cell, where the layer reads it
                Box(Modifier::empty().width(cell.0).height(cell.1), BoxSpec::default(), move || {
                    let (width, height) = boxes.with(|b| b[side]);
                    let meshes = meshes.clone();
                    Box(
                        Modifier::empty().width(width).height(height).scene3d(move || {
                            let eye = eyes.with(|s| s.eyes.map(|e| e[side]));
                            let camera = eye.map_or(
                                Camera::look_at(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, -1.0, 0.0), width),
                                |e| e.camera(width, height, flip),
                            );
                            Scene3d { camera, light: Some(light()), eye: None }
                        }),
                        BoxSpec::default(),
                        move || {
                            for (index, shape) in shapes.get().into_iter().enumerate() {
                                let meshes = meshes.clone();
                                key(index, move || Solid(index, shape, places, meshes.clone()));
                            }
                            if let (Some(bitmap), Some(surface)) = (mirror.bitmap.get(), mirror.surface.get()) {
                                key(usize::MAX, move || Mirror(bitmap.clone(), surface.clone(), mirror.place));
                            }
                        },
                    );
                });
            });
        }
    });
}

/// A frame for the scene's thread: the image to draw into, the eyes and the shapes.
struct Job {
    image: u32,
    views: [xr::View; 2],
    shapes: Vec<Shape>,
    places: Vec<[[f32; 4]; 4]>,
    mirror: Option<MirrorJob>,
}

/// What the scene needs of the first panel's mirror this frame.
pub struct MirrorJob {
    /// A new picture, when the mirror's decoder had one
    pub frame: Option<crate::mirror::MirrorFrame>,
    /// Where the curved surface stands, in the stage's space and in meters, and its width and
    /// height; `None` while the first panel is not on show
    pub surface: Option<(GMat4, (f32, f32))>,
    /// The size of the mirror's pictures, in pixels
    pub size: (u32, u32),
}

/// A finished frame: the fence after Cranpose's GL commands, or `None` when it saw no change and
/// left the image as it was. The fence is a GL sync object's address, for its trip across threads.
struct Done {
    fence: Option<usize>,
}

/// How long the render thread waits for a frame of the scene before it shows the last one again.
/// A frame takes 0.7 to 0.8 ms of waiting, 2.9 ms at most.
const SCENE_DEADLINE: Duration = Duration::from_millis(4);

pub struct XrScene {
    /// Whether the scene's thread still runs
    alive: Arc<AtomicBool>,
    swapchain: xr::Swapchain<xr::OpenGlEs>,
    eye: UVec2,
    jobs: Option<Sender<Job>>,
    done: Receiver<Done>,
    /// The image that the scene's thread draws into, with the views of its frame
    drawing: Option<(u32, [xr::View; 2])>,
    /// An acquired image that holds no new picture, for the next frame
    held: Option<u32>,
    /// The views of the image released last, which the layer shows
    shown: Option<[xr::View; 2]>,
    /// Whether this frame has the overlay: a frame without it shows no layer
    in_view: bool,
    thread: Option<JoinHandle<()>>,
    stats: Stats,
}

impl XrScene {
    /// `eye` is the size of an eye's picture. The cell of each eye in the swapchain keeps its
    /// height and takes the aspect of the widest of `views`.
    pub fn new(session: &xr::Session<xr::OpenGlEs>, gfx: &GraphicsContext, eye: UVec2, views: &[xr::View]) -> Result<Self, String> {
        let aspect = views.iter().map(|v| tangents(v.fov)).map(|[l, r, u, d]| (r - l) / (u - d)).fold(0.0, f32::max);
        let eye = if aspect > 0.0 { UVec2::new((eye.y as f32 * aspect).ceil() as u32, eye.y) } else { eye };
        gfx.make_current();
        let swapchain = session
            .create_swapchain(&xr::SwapchainCreateInfo {
                create_flags: xr::SwapchainCreateFlags::EMPTY,
                usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT | xr::SwapchainUsageFlags::SAMPLED,
                format: GL_SRGB8_ALPHA8,
                sample_count: 1,
                width: eye.x * 2,
                height: eye.y,
                face_count: 1,
                array_size: 1,
                mip_count: 1,
            })
            .map_err(|e| e.to_string())?;
        let images = swapchain.enumerate_images().map_err(|e| e.to_string())?;
        let share = Share {
            display: gfx.egl_display.as_ptr() as usize,
            config: gfx.egl_config.as_ptr() as usize,
            context: gfx.egl_context.as_ptr() as usize,
        };
        // wgpu's GL backend writes rows from the top; OpenXR's GL images run from the bottom
        let flip = !crate::flat::property::get("xr_flip").is_some_and(|v| v == "0");
        let alive = Arc::new(AtomicBool::new(true));
        let running = Arc::clone(&alive);
        let (jobs, job_receiver) = mpsc::channel();
        let (done_sender, done) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("xr-scene".into())
            .spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(share, images, eye, flip, job_receiver, done_sender)));
                running.store(false, Ordering::Relaxed);
                let message = match result {
                    Ok(Ok(())) => return,
                    Ok(Err(e)) => e,
                    Err(panic) => panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_else(|| "a panic".into()),
                };
                error!("cranvr xr scene: {message}");
                crate::flat::log::line(&format!("cranvr xr scene: stopped: {message}"));
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            alive,
            swapchain,
            eye,
            jobs: Some(jobs),
            done,
            drawing: None,
            held: None,
            shown: None,
            in_view: false,
            thread: Some(thread),
            stats: Stats::default(),
        })
    }

    /// Whether the scene's thread still draws.
    pub fn running(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Acquires an image and hands it to the scene's thread with this frame's eyes and the
    /// overlay's draws. A frame whose image the scene has not finished starts no new one.
    pub fn frame(&mut self, views: &[xr::View], draws: &[OverlayDraw], mirror: Option<MirrorJob>) {
        self.in_view = true;
        let (Some(jobs), [left, right, ..]) = (&self.jobs, views) else { return };
        if self.drawing.is_some() {
            return;
        }
        let image = match self.held.take() {
            Some(image) => image,
            None => {
                let start = Instant::now();
                let Ok(image) = self.swapchain.acquire_image() else { return };
                if self.swapchain.wait_image(xr::Duration::INFINITE).is_err() {
                    return;
                }
                self.stats.waited += start.elapsed();
                image
            }
        };
        let (shapes, places) = shapes(draws);
        let views = [*left, *right];
        if jobs.send(Job { image, views, shapes, places, mirror }).is_ok() {
            self.drawing = Some((image, views));
        }
    }

    /// Leaves the layer out of this frame, as nothing in it has the overlay.
    pub fn hide(&mut self) {
        self.in_view = false;
    }

    /// Takes back the image of this frame and releases it. Call it before the frame ends.
    pub fn finish(&mut self, gfx: &GraphicsContext) {
        let Some((image, views)) = self.drawing else { return };
        let start = Instant::now();
        let done = self.done.recv_timeout(SCENE_DEADLINE);
        let took = start.elapsed();
        let stats = &mut self.stats;
        stats.frames += 1;
        stats.spent += took;
        stats.longest = stats.longest.max(took);
        match done {
            Ok(Done { fence: Some(fence) }) => {
                self.drawing = None;
                stats.drawn += 1;
                // The runtime's own fence at the release follows this wait in ALVR's context
                gfx.make_current();
                let gl = &gfx.gl_context;
                let fence = glow::NativeFence(fence as *mut _);
                unsafe {
                    gl.wait_sync(fence, 0, glow::TIMEOUT_IGNORED);
                    gl.delete_sync(fence);
                }
                if self.swapchain.release_image().is_ok() {
                    self.shown = Some(views);
                }
            }
            Ok(Done { fence: None }) => {
                self.drawing = None;
                self.held = Some(image);
            }
            // The scene is late: the layer shows the last image again, and this one stays with
            // the scene's thread until it is done
            Err(_) => stats.late += 1,
        }
        if stats.frames % 300 == 0 {
            let ms = |d: Duration| d.as_secs_f32() * 1000.0;
            crate::flat::log::line(&format!(
                "cranvr xr scene: the render thread waited {:.1} ms a frame for the scene, {:.1} ms at most, {:.1} ms for images; {} of 300 drawn, {} late",
                ms(stats.spent) / 300.0,
                ms(stats.longest),
                ms(stats.waited) / 300.0,
                stats.drawn,
                stats.late
            ));
            *stats = Stats { frames: stats.frames, ..Stats::default() };
        }
    }

    /// The layer with the image released last, at the views it was drawn for.
    pub fn layer<'a>(&'a self, space: &'a xr::Space) -> Option<ProjectionLayerBuilder<'a>> {
        let views = self.shown.as_ref().filter(|_| self.in_view)?;
        let view = |side: usize| {
            let size = fitted(views[side].fov, self.eye);
            let rect = xr::Rect2Di {
                offset: xr::Offset2Di { x: (side as u32 * self.eye.x) as i32, y: 0 },
                extent: xr::Extent2Di { width: size.x as i32, height: size.y as i32 },
            };
            xr::CompositionLayerProjectionView::new()
                .pose(views[side].pose)
                .fov(views[side].fov)
                .sub_image(xr::SwapchainSubImage::new().swapchain(&self.swapchain).image_array_index(0).image_rect(rect))
        };
        Some(ProjectionLayerBuilder::new(space, [view(0), view(1)], Some(ProjectionLayerAlphaConfig { premultiplied: true }), None))
    }
}

impl Drop for XrScene {
    fn drop(&mut self) {
        // The thread draws into the swapchain's images, which go with `swapchain` after this
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

/// ALVR's EGL display, config and context, as addresses that cross threads.
struct Share {
    display: usize,
    config: usize,
    context: usize,
}

/// What the render thread saw over its last 300 frames with the scene.
#[derive(Default)]
struct Stats {
    frames: u32,
    drawn: u32,
    late: u32,
    spent: Duration,
    longest: Duration,
    waited: Duration,
}

/// A GL texture of the share group as a wgpu texture. wgpu leaves the texture to its owner.
fn wrap_gl_texture(
    device: &wgpu::Device,
    name: u32,
    size: wgpu::Extent3d,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> Result<wgpu::Texture, String> {
    let uses = if usage.contains(wgpu::TextureUsages::RENDER_ATTACHMENT) {
        wgpu::wgt::TextureUses::COLOR_TARGET
    } else {
        wgpu::wgt::TextureUses::COPY_SRC
    };
    let hal_desc = wgpu::hal::TextureDescriptor {
        label: Some("xr scene gl texture"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: uses,
        memory_flags: wgpu::hal::MemoryFlags::empty(),
        view_formats: Vec::new(),
    };
    let desc = wgpu::TextureDescriptor {
        label: Some("xr scene gl texture"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    };
    unsafe {
        let hal_device = device.as_hal::<wgpu::hal::api::Gles>().ok_or("not a GL device")?;
        let raw = hal_device.texture_from_raw(NonZeroU32::new(name).ok_or("no texture")?, &hal_desc, Some(Box::new(|| {})));
        drop(hal_device);
        Ok(device.create_texture_from_hal::<wgpu::hal::api::Gles>(raw, &desc, wgpu::wgt::TextureUses::UNINITIALIZED))
    }
}

fn run(share: Share, images: Vec<u32>, eye: UVec2, flip: bool, jobs: Receiver<Job>, done: Sender<Done>) -> Result<(), String> {
    let start = Instant::now();
    let egl = unsafe { egl::DynamicInstance::<egl::EGL1_4>::load_required() }.map_err(|e| e.to_string())?;
    let (display, config, shared) = unsafe {
        (
            egl::Display::from_ptr(share.display as *mut c_void),
            egl::Config::from_ptr(share.config as *mut c_void),
            egl::Context::from_ptr(share.context as *mut c_void),
        )
    };
    let context = egl
        .create_context(display, config, Some(shared), &[egl::CONTEXT_CLIENT_VERSION, 3, egl::NONE])
        .map_err(|e| format!("context: {e}"))?;
    let surface = egl
        .create_pbuffer_surface(display, config, &[egl::WIDTH, 16, egl::HEIGHT, 16, egl::NONE])
        .map_err(|e| format!("pbuffer: {e}"))?;
    egl.make_current(display, Some(surface), Some(surface), Some(context)).map_err(|e| format!("current: {e}"))?;

    let lookup = |name: &str| egl.get_proc_address(name).map_or(std::ptr::null(), |f| f as *const c_void);
    let hal_adapter = unsafe { wgpu::hal::gles::Adapter::new_external(lookup, wgpu::GlBackendOptions::default()) }
        .ok_or("no GL adapter on the shared context")?;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::GL, ..wgpu::InstanceDescriptor::new_without_display_handle() });
    let adapter = unsafe { instance.create_adapter_from_hal(hal_adapter) };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("cranvr xr scene"),
        required_limits: adapter.limits(),
        ..Default::default()
    }))
    .map_err(|e| format!("device: {e}"))?;
    let (device, queue) = (Arc::new(device), Arc::new(queue));

    let size = wgpu::Extent3d { width: eye.x * 2, height: eye.y, depth_or_array_layers: 1 };
    let textures: Vec<(wgpu::Texture, wgpu::TextureView)> = images
        .iter()
        .map(|&name| {
            let texture = wrap_gl_texture(&device, name, size, FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT)?;
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            Ok::<_, String>((texture, view))
        })
        .collect::<Result<_, _>>()?;

    let gpu = GpuContext { instance, adapter, device: Arc::clone(&device), queue };
    let meshes = Meshes::new();
    let states = std::rc::Rc::new(std::cell::Cell::new(None));
    let held = std::rc::Rc::clone(&states);
    let cell = (eye.x as f32, eye.y as f32);
    let mut guest = GuestView::new(&AppSettings::default(), &gpu, FORMAT, size.width, size.height, 1.0, move || {
        let eyes = remember(|| mutableStateOf(Eyes::default())).with(|state| *state);
        let shapes = remember(|| mutableStateOf(Vec::<Shape>::new())).with(|state| *state);
        let places = remember(|| mutableStateOf(Vec::<[[f32; 4]; 4]>::new())).with(|state| *state);
        let boxes = remember(|| mutableStateOf([cell; 2])).with(|state| *state);
        let mirror = MirrorStates {
            bitmap: remember(|| mutableStateOf(None::<ImageBitmap>)).with(|state| *state),
            surface: remember(|| mutableStateOf(None::<MirrorSurface>)).with(|state| *state),
            place: remember(|| mutableStateOf(None::<[[f32; 4]; 4]>)).with(|state| *state),
        };
        held.set(Some((eyes, boxes, mirror, shapes, places)));
        Scene(eyes, boxes, mirror, shapes, places, cell, flip, meshes.clone());
    })
    .map_err(|e| format!("guest view: {e}"))?;
    guest.shell().renderer().set_transparent_background(true);
    let (eyes, boxes, mirror, shapes, places) = states.get().ok_or("the scene did not compose")?;
    let external = ExternalFrames::new(&gpu, wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureUsages::COPY_DST, true);
    let writer = external.writer();
    // The mirror's texture as wgpu sees it, with its GL name and size
    let mut mirror_source: Option<((u32, (u32, u32)), wgpu::Texture)> = None;
    crate::flat::log::line(&format!("cranvr xr scene: ready in {:?}, {}x{}, flip {flip}", start.elapsed(), size.width, size.height));

    let gl = unsafe { glow::Context::from_loader_function(|name| lookup(name)) };
    while let Ok(mut job) = jobs.recv() {
        eyes.set(Eyes { eyes: Some([Eye::of(&job.views[0]), Eye::of(&job.views[1])]) });
        // The same boxes as the layer's, from the same views
        boxes.set(job.views.map(|view| fitted(view.fov, eye).as_vec2().into()));
        if let Some(job_mirror) = job.mirror.take() {
            if let Some(frame) = job_mirror.frame {
                unsafe {
                    let fence = glow::NativeFence(frame.fence as *mut _);
                    gl.wait_sync(fence, 0, glow::TIMEOUT_IGNORED);
                    gl.delete_sync(fence);
                }
                let key = (frame.texture, frame.size);
                let source = match &mirror_source {
                    Some((known, texture)) if *known == key => Some(texture.clone()),
                    _ => {
                        let extent = wgpu::Extent3d { width: frame.size.0, height: frame.size.1, depth_or_array_layers: 1 };
                        match wrap_gl_texture(&device, frame.texture, extent, wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureUsages::COPY_SRC) {
                            Ok(texture) => {
                                mirror_source = Some((key, texture.clone()));
                                Some(texture)
                            }
                            Err(e) => {
                                error!("cranvr mirror: {e}");
                                None
                            }
                        }
                    }
                };
                external.set_size(frame.size.0, frame.size.1);
                if let (Some(source), Some(target)) = (source, writer.begin()) {
                    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("mirror copy") });
                    let extent = wgpu::Extent3d { width: frame.size.0, height: frame.size.1, depth_or_array_layers: 1 };
                    encoder.copy_texture_to_texture(source.as_image_copy(), target.texture().as_image_copy(), extent);
                    gpu.queue.submit([encoder.finish()]);
                    writer.publish(target.number());
                }
                if let Some(bitmap) = external.next_frame() {
                    mirror.bitmap.set(Some(bitmap));
                }
            }
            match job_mirror.surface {
                Some((model, meters)) => {
                    mirror.place.set(Some(scene_transform(&model)));
                    let (width, height) = job_mirror.size;
                    let content = (MIRROR_CONTENT_WIDTH, MIRROR_CONTENT_WIDTH * height as f32 / width.max(1) as f32);
                    if mirror.surface.with(|s| s.as_ref().map(|s| (s.meters, s.content))) != Some((meters, content)) {
                        match s_surface(meters.0, meters.1, meters.0 * 0.08) {
                            Ok(mesh) => mirror.surface.set(Some(MirrorSurface { mesh: Arc::new(mesh), meters, content })),
                            Err(e) => error!("cranvr mirror: the surface's mesh: {e}"),
                        }
                    }
                }
                None => mirror.surface.set(None),
            }
        }
        shapes.set(std::mem::take(&mut job.shapes));
        places.set(std::mem::take(&mut job.places));
        let (texture, view) = &textures[job.image as usize];
        let drawn = guest.draw(texture, view).unwrap_or_else(|e| {
            error!("cranvr xr scene: {e}");
            false
        });
        // wgpu learns of finished work here without waiting for it
        device.poll(wgpu::PollType::Poll).ok();
        let fence = drawn
            .then(|| unsafe {
                let fence = gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0).ok()?;
                gl.flush();
                Some(fence.0 as usize)
            })
            .flatten();
        if done.send(Done { fence }).is_err() {
            break;
        }
    }
    info!("cranvr xr scene: ends");
    Ok(())
}
