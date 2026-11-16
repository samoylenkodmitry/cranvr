// cranvr: a 3D scene that Cranpose draws for both eyes, in a projection layer of its own.
//
// A thread of its own holds the Cranpose composition and a GL context of its own, in the share
// group of ALVR's context: Cranpose's wgpu never touches ALVR's GL state. The layer's swapchain is
// two eyes wide. The render thread acquires an image and hands it to the scene's thread with the
// eyes' views; the scene's thread draws both eyes and waits for the GPU; the render thread then
// releases the image and shows it with the views it was drawn for. One image is in flight at a
// time, so the render thread never waits for the scene. Cranpose draws only when the scene looks
// different. An image it did not draw into holds an older picture, so the render thread keeps it
// for the next frame instead of showing it.
//
// The scene shows the overlay's draws (panel.rs): the controllers, their rays, the panels'
// corners and the console's frame, as meshes lit like the overlay's shader lit them.

use crate::graphics::{ProjectionLayerAlphaConfig, ProjectionLayerBuilder};
use alvr_common::glam::{Mat4 as GMat4, UVec2, Vec3 as GVec3};
use alvr_common::{error, info};
use alvr_graphics::{GraphicsContext, OverlayDraw, OverlayShape};
use cranpose::prelude::*;
use cranpose::{GpuContext, GuestView};
use cranpose::space::{Camera, Light, Mat4, Material, Mesh, MeshSurface, Scene3d, Vec3};
use khronos_egl as egl;
use openxr as xr;
use std::{
    ffi::c_void,
    num::NonZeroU32,
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    time::Instant,
};

const GL_SRGB8_ALPHA8: u32 = 0x8C43;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Scene units per meter: a Cranpose camera cuts away what lies nearer than a 64th of its focal
/// length in pixels, so the scene is in millimeters
const UNITS: f32 = 1000.0;

/// An eye as OpenXR gives it: where it is, where it looks and the tangents of its view's edges
/// (left, right, up, down).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Eye {
    position: [f32; 3],
    orientation: [f32; 4],
    tangents: [f32; 4],
}

impl Eye {
    fn of(view: &xr::View) -> Self {
        let (p, q, f) = (view.pose.position, view.pose.orientation, view.fov);
        Self {
            position: [p.x, p.y, p.z],
            orientation: [q.x, q.y, q.z, q.w],
            tangents: [f.angle_left.tan(), f.angle_right.tan(), f.angle_up.tan(), f.angle_down.tan()],
        }
    }

    /// The camera that sees the scene as this eye does, in a box of `width` by `height` pixels.
    /// `flip` turns the picture upside down, for a target whose rows go up.
    fn camera(&self, width: f32, height: f32, flip: bool) -> Camera {
        let [l, r, u, d] = self.tangents;
        let focal_length = (width / (r - l)).min(height / (u - d)).max(1.0);
        // How far the view's center lies from the box's middle, in focal lengths
        let (sx, sy) = (-(l + r) * 0.5, (u + d) * 0.5);
        let q = self.orientation;
        let right = scene_direction(rotate(q, [1.0, 0.0, 0.0]));
        let down = scene_direction(rotate(q, [0.0, -1.0, 0.0]));
        let forward = scene_direction(rotate(q, [0.0, 0.0, -1.0]));
        let eye = scene_point(self.position);
        let row = |a: Vec3| [a.x, a.y, a.z, -a.dot(eye)];
        let z = row(forward);
        let sheared = |a: Vec3, s: f32| {
            let r = row(a);
            [r[0] + s * z[0], r[1] + s * z[1], r[2] + s * z[2], r[3] + s * z[3]]
        };
        let (down, sy) = if flip { (down * -1.0, -sy) } else { (down, sy) };
        Camera {
            view: Mat4::from_rows([sheared(right, sx), sheared(down, sy), z, [0.0, 0.0, 0.0, 1.0]]),
            focal_length,
        }
    }
}

/// An OpenXR point in meters as a point of the scene: Cranpose's scene has y down and looks
/// along +z.
fn scene_point([x, y, z]: [f32; 3]) -> Vec3 {
    Vec3::new(x, -y, -z) * UNITS
}

fn scene_direction([x, y, z]: [f32; 3]) -> Vec3 {
    Vec3::new(x, -y, -z)
}

fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let (axis, w) = (Vec3::new(q[0], q[1], q[2]), q[3]);
    let v = Vec3::new(v[0], v[1], v[2]);
    let t = axis.cross(v) * 2.0;
    let r = v + t * w + axis.cross(t);
    [r.x, r.y, r.z]
}

/// A model matrix of the overlay, in meters, as a transform of the scene.
fn scene_transform(model: &GMat4) -> [[f32; 4]; 4] {
    let column = |i: usize| {
        let c = model.col(i);
        [c.x * UNITS, -c.y * UNITS, -c.z * UNITS]
    };
    let [x, y, z, o] = [column(0), column(1), column(2), column(3)];
    let a = |v: [f32; 3]| Vec3::new(v[0], v[1], v[2]);
    // The origin column scales like a point, the axes like directions; both by UNITS
    Mat4::from_axes(a(x), a(y), a(z), a(o)).rows()
}

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

/// The eyes that the scene shows.
#[derive(Clone, Debug, Default, PartialEq)]
struct Shown {
    eyes: Option<[Eye; 2]>,
}

/// The light of the overlay's shapes: from above, ahead and right, as the overlay's shader had it
fn light() -> Light {
    let travel = scene_direction([-0.3, -1.0, -0.6]);
    Light { direction: travel, ambient: 0.45, shadows: false, size: 0.0 }
}

#[composable]
fn Scene(
    eyes: MutableState<Shown>,
    shapes: MutableState<Vec<Shape>>,
    places: MutableState<Vec<[[f32; 4]; 4]>>,
    eye_size: (f32, f32),
    flip: bool,
    meshes: Meshes,
) {
    let (width, height) = eye_size;
    Row(Modifier::empty(), RowSpec::default(), move || {
        for side in 0..2 {
            let meshes = meshes.clone();
            key(side, move || {
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
                    },
                )
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
}

/// A finished frame: the views its image was drawn for, or `None` when Cranpose saw no change
/// and left the image as it was.
struct Done {
    image: u32,
    drawn: Option<[xr::View; 2]>,
}

pub struct XrScene {
    /// Whether the scene's thread still runs
    alive: Arc<std::sync::atomic::AtomicBool>,
    swapchain: xr::Swapchain<xr::OpenGlEs>,
    eye: UVec2,
    jobs: Sender<Job>,
    done: Receiver<Done>,
    in_flight: bool,
    /// An acquired image that holds no new picture, for the next frame
    held: Option<u32>,
    /// The views of the image released last, which the layer shows
    shown: Option<[xr::View; 2]>,
}

impl XrScene {
    pub fn new(session: &xr::Session<xr::OpenGlEs>, gfx: &GraphicsContext, eye: UVec2) -> Result<Self, String> {
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
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let running = Arc::clone(&alive);
        let (jobs, job_receiver) = mpsc::channel();
        let (done_sender, done) = mpsc::channel();
        std::thread::Builder::new()
            .name("xr-scene".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(share, images, eye, flip, job_receiver, done_sender)));
                running.store(false, std::sync::atomic::Ordering::Relaxed);
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
        Ok(Self { alive, swapchain, eye, jobs, done, in_flight: false, held: None, shown: None })
    }

    /// Whether the scene's thread still draws.
    pub fn running(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Releases a finished image and hands the next one to the scene's thread.
    pub fn frame(&mut self, views: &[xr::View], draws: &[OverlayDraw]) {
        while let Ok(done) = self.done.try_recv() {
            self.in_flight = false;
            match done.drawn {
                Some(views) => {
                    if self.swapchain.release_image().is_ok() {
                        self.shown = Some(views);
                    }
                }
                None => self.held = Some(done.image),
            }
        }
        if self.in_flight || views.len() < 2 {
            return;
        }
        let image = match self.held.take() {
            Some(image) => image,
            None => {
                let Ok(image) = self.swapchain.acquire_image() else { return };
                if self.swapchain.wait_image(xr::Duration::INFINITE).is_err() {
                    return;
                }
                image
            }
        };
        let (shapes, places) = shapes(draws);
        if self.jobs.send(Job { image, views: [views[0], views[1]], shapes, places }).is_ok() {
            self.in_flight = true;
        }
    }

    /// The layer with the image released last, at the views it was drawn for.
    pub fn layer<'a>(&'a self, space: &'a xr::Space) -> Option<ProjectionLayerBuilder<'a>> {
        let views = self.shown.as_ref()?;
        let view = |eye: usize| {
            let rect = xr::Rect2Di {
                offset: xr::Offset2Di { x: (eye as u32 * self.eye.x) as i32, y: 0 },
                extent: xr::Extent2Di { width: self.eye.x as i32, height: self.eye.y as i32 },
            };
            xr::CompositionLayerProjectionView::new()
                .pose(views[eye].pose)
                .fov(views[eye].fov)
                .sub_image(xr::SwapchainSubImage::new().swapchain(&self.swapchain).image_array_index(0).image_rect(rect))
        };
        Some(ProjectionLayerBuilder::new(space, [view(0), view(1)], Some(ProjectionLayerAlphaConfig { premultiplied: true }), None))
    }
}

/// ALVR's EGL display, config and context, as addresses that cross threads.
struct Share {
    display: usize,
    config: usize,
    context: usize,
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
            let hal_desc = wgpu::hal::TextureDescriptor {
                label: Some("xr scene image"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::wgt::TextureUses::COLOR_TARGET,
                memory_flags: wgpu::hal::MemoryFlags::empty(),
                view_formats: Vec::new(),
            };
            let desc = wgpu::TextureDescriptor {
                label: Some("xr scene image"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            };
            let texture = unsafe {
                let hal_device = device.as_hal::<wgpu::hal::api::Gles>().ok_or("not a GL device")?;
                let raw = hal_device.texture_from_raw(NonZeroU32::new(name).ok_or("no texture")?, &hal_desc, Some(Box::new(|| {})));
                drop(hal_device);
                device.create_texture_from_hal::<wgpu::hal::api::Gles>(raw, &desc, wgpu::wgt::TextureUses::UNINITIALIZED)
            };
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            Ok::<_, String>((texture, view))
        })
        .collect::<Result<_, _>>()?;

    let gpu = GpuContext { instance, adapter, device: Arc::clone(&device), queue };
    let meshes = Meshes::new();
    let states = std::rc::Rc::new(std::cell::Cell::new(None));
    let held = std::rc::Rc::clone(&states);
    let eye_size = (eye.x as f32, eye.y as f32);
    let mut guest = GuestView::new(&AppSettings::default(), &gpu, FORMAT, size.width, size.height, 1.0, move || {
        let eyes = remember(|| mutableStateOf(Shown::default())).with(|state| *state);
        let shapes = remember(|| mutableStateOf(Vec::<Shape>::new())).with(|state| *state);
        let places = remember(|| mutableStateOf(Vec::<[[f32; 4]; 4]>::new())).with(|state| *state);
        held.set(Some((eyes, shapes, places)));
        Scene(eyes, shapes, places, eye_size, flip, meshes.clone());
    })
    .map_err(|e| format!("guest view: {e}"))?;
    guest.shell().renderer().set_transparent_background(true);
    let (eyes, shapes, places) = states.get().ok_or("the scene did not compose")?;
    crate::flat::log::line(&format!("cranvr xr scene: ready in {:?}, {}x{}, flip {flip}", start.elapsed(), size.width, size.height));

    let (mut frames, mut drawn) = (0u32, 0u32);
    let mut spent = std::time::Duration::ZERO;
    while let Ok(mut job) = jobs.recv() {
        // Only the newest frame counts
        while let Ok(newer) = jobs.try_recv() {
            job = newer;
        }
        let start = Instant::now();
        eyes.set(Shown { eyes: Some([Eye::of(&job.views[0]), Eye::of(&job.views[1])]) });
        shapes.set(std::mem::take(&mut job.shapes));
        places.set(std::mem::take(&mut job.places));
        let (texture, view) = &textures[job.image as usize];
        let drew = guest.draw(texture, view).unwrap_or_else(|e| {
            error!("cranvr xr scene: {e}");
            false
        });
        if drew {
            device.poll(wgpu::PollType::wait_indefinitely()).ok();
        }
        spent += start.elapsed();
        frames += 1;
        drawn += drew as u32;
        if frames <= 3 {
            crate::flat::log::line(&format!("cranvr xr scene: frame {frames} took {:?}", start.elapsed()));
        }
        if frames % 300 == 0 {
            crate::flat::log::line(&format!(
                "cranvr xr scene: {:.1} ms a frame over {frames} frames, {drawn} of the last 300 drawn",
                spent.as_secs_f32() * 1000.0 / 300.0
            ));
            (spent, drawn) = (std::time::Duration::ZERO, 0);
        }
        if done.send(Done { image: job.image, drawn: drew.then_some(job.views) }).is_err() {
            break;
        }
    }
    info!("cranvr xr scene: ends");
    Ok(())
}
