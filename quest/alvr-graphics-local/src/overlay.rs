// cranvr: simple lit shapes in a layer over the others: the controllers, their rays and the
// handle of the desktop panel. The app draws them with the newest poses, so they do not wait for
// the Mac. The shader smooths the edges (see overlay.wgsl): multisampling at the eye's full size
// would cost the Quest about 50 MB of memory per eye and much of its memory bandwidth.

use super::{GraphicsContext, SDR_FORMAT};
use alvr_common::{
    glam::{Mat4, UVec2, Vec3},
    Fov, Pose,
};
use glyph_brush_layout::ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use std::{f32::consts::TAU, ops::Range, rc::Rc};
use wgpu::{
    include_wgsl, util::DeviceExt, BindGroup, BindGroupDescriptor, BindGroupEntry,
    BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindingResource, BindingType, BlendComponent, BlendFactor,
    BlendOperation, BlendState, Buffer, BufferBindingType, BufferUsages, Color, ColorTargetState,
    ColorWrites, CommandEncoderDescriptor, CompareFunction, DepthStencilState, Device, Extent3d,
    FragmentState, LoadOp, Operations, PipelineLayout, PipelineLayoutDescriptor, PrimitiveState,
    PrimitiveTopology, PushConstantRange, RenderPassColorAttachment,
    RenderPassDepthStencilAttachment, RenderPassDescriptor, RenderPipeline,
    RenderPipelineDescriptor, ShaderModule, ShaderStages, StoreOp, TextureDescriptor,
    TextureDimension, TextureFormat, TextureUsages, TextureView, VertexAttribute,
    VertexBufferLayout, VertexFormat, VertexState, VertexStepMode, Texture, TexelCopyTextureInfo,
    TexelCopyBufferLayout, Origin3d, TextureAspect, TextureSampleType, TextureViewDimension,
    SamplerBindingType, SamplerDescriptor, FilterMode,
};

const DEPTH_FORMAT: TextureFormat = TextureFormat::Depth24Plus;
const SEGMENTS: usize = 24;
/// The push constants: the model's columns, the color, the flat shape's parameters and its place
/// in the text atlas
const PUSH_SIZE: u32 = 96;
/// The text atlas: labels rasterized in rows, white with the glyphs' coverage as alpha
const ATLAS: (u32, u32) = (1024, 1024);
const FONT_PX: f32 = 40.0;
const ROW: u32 = 52;
/// A flat shape's quad reaches this far past the shape on each side, so that the soft edge falls
/// inside the quad. At 1 m it is about 5 pixels of a Quest 3.
const FLAT_MARGIN: f32 = 0.004;

/// A solid shape of size 1 around the origin, or a flat one in the XY plane. The cylinder's axis
/// is Z.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OverlayShape {
    Box,
    Cylinder,
    Sphere,
    /// A flat rectangle of this size in meters, with round corners of `radius`. Its alpha drops
    /// by `fade` from the bottom edge to the top edge. The model must not scale it.
    RoundRect { width: f32, height: f32, radius: f32, fade: f32 },
    /// A text from `OverlayRenderer::label`, as large as fits a box of this size in meters,
    /// centered on the model's origin
    Label { id: u16, width: f32, height: f32 },
}

/// One shape: where it goes and its color. Alpha below 1 lets the layers below show through.
pub struct OverlayDraw {
    pub shape: OverlayShape,
    pub model: Mat4,
    pub color: [f32; 4],
}

pub struct OverlayView {
    pub swapchain_index: u32,
    pub pose: Pose,
    pub fov: Fov,
}

pub struct OverlayRenderer {
    context: Rc<GraphicsContext>,
    solid: RenderPipeline,
    flat: RenderPipeline,
    vertices: Buffer,
    /// Box, cylinder, sphere, quad
    ranges: [Range<u32>; 4],
    /// The view projection and the eye's position, one buffer per eye
    eyes: [(Buffer, BindGroup); 2],
    atlas: Texture,
    /// Each label's text, its place in the atlas (u, v, width, height) and its width over height
    labels: Vec<(String, [f32; 4], f32)>,
    /// Where the next label goes in the atlas
    shelf: (u32, u32),
    depth: [TextureView; 2],
    render_targets: [Vec<TextureView>; 2],
}

/// Position and normal
type Vertex = [f32; 6];

fn push_triangle(out: &mut Vec<Vertex>, points: [Vec3; 3], normals: [Vec3; 3]) {
    for (p, n) in points.into_iter().zip(normals) {
        out.push([p.x, p.y, p.z, n.x, n.y, n.z]);
    }
}

fn box_vertices() -> Vec<Vertex> {
    let mut out = Vec::new();
    for axis in 0..3 {
        for sign in [-1.0f32, 1.0] {
            let mut n = Vec3::ZERO;
            n[axis] = sign;
            let u = [Vec3::Y, Vec3::Z, Vec3::X][axis];
            let v = n.cross(u);
            let c = n * 0.5;
            let (a, b, c2, d) = (c - u * 0.5 - v * 0.5, c + u * 0.5 - v * 0.5, c + u * 0.5 + v * 0.5, c - u * 0.5 + v * 0.5);
            push_triangle(&mut out, [a, b, c2], [n; 3]);
            push_triangle(&mut out, [a, c2, d], [n; 3]);
        }
    }
    out
}

fn cylinder_vertices() -> Vec<Vertex> {
    let mut out = Vec::new();
    let at = |i: usize| {
        let a = i as f32 / SEGMENTS as f32 * TAU;
        Vec3::new(a.cos() * 0.5, a.sin() * 0.5, 0.0)
    };
    for i in 0..SEGMENTS {
        let (p, q) = (at(i), at(i + 1));
        let (np, nq) = (p.normalize(), q.normalize());
        let (top, bottom) = (Vec3::Z * 0.5, Vec3::Z * -0.5);
        push_triangle(&mut out, [p + bottom, q + bottom, q + top], [np, nq, nq]);
        push_triangle(&mut out, [p + bottom, q + top, p + top], [np, nq, np]);
        push_triangle(&mut out, [top, p + top, q + top], [Vec3::Z; 3]);
        push_triangle(&mut out, [bottom, q + bottom, p + bottom], [-Vec3::Z; 3]);
    }
    out
}

fn quad_vertices() -> Vec<Vertex> {
    let mut out = Vec::new();
    let (a, b, c, d) = (Vec3::new(-0.5, -0.5, 0.0), Vec3::new(0.5, -0.5, 0.0), Vec3::new(0.5, 0.5, 0.0), Vec3::new(-0.5, 0.5, 0.0));
    push_triangle(&mut out, [a, b, c], [Vec3::Z; 3]);
    push_triangle(&mut out, [a, c, d], [Vec3::Z; 3]);
    out
}

fn pipeline(
    device: &Device,
    layout: &PipelineLayout,
    shader: &ShaderModule,
    (vertex, fragment): (&str, &str),
    depth_write: bool,
) -> RenderPipeline {
    device.create_render_pipeline(&RenderPipelineDescriptor {
        label: Some(vertex),
        layout: Some(layout),
        vertex: VertexState {
            module: shader,
            entry_point: Some(vertex),
            compilation_options: Default::default(),
            buffers: &[VertexBufferLayout {
                array_stride: 24,
                step_mode: VertexStepMode::Vertex,
                attributes: &[
                    VertexAttribute { format: VertexFormat::Float32x3, offset: 0, shader_location: 0 },
                    VertexAttribute { format: VertexFormat::Float32x3, offset: 12, shader_location: 1 },
                ],
            }],
        },
        primitive: PrimitiveState { topology: PrimitiveTopology::TriangleList, ..Default::default() },
        depth_stencil: Some(DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: depth_write,
            depth_compare: CompareFunction::Less,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(FragmentState {
            module: shader,
            entry_point: Some(fragment),
            compilation_options: Default::default(),
            targets: &[Some(ColorTargetState {
                format: SDR_FORMAT,
                blend: Some(BlendState {
                    color: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::OneMinusSrcAlpha,
                        operation: BlendOperation::Add,
                    },
                    alpha: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::OneMinusSrcAlpha,
                        operation: BlendOperation::Add,
                    },
                }),
                write_mask: ColorWrites::ALL,
            })],
        }),
        multiview: None,
        cache: None,
    })
}

fn sphere_vertices() -> Vec<Vertex> {
    let mut out = Vec::new();
    let rings = SEGMENTS / 2;
    let at = |ring: usize, i: usize| {
        let (t, a) = (ring as f32 / rings as f32 * TAU / 2.0, i as f32 / SEGMENTS as f32 * TAU);
        Vec3::new(t.sin() * a.cos(), t.cos(), t.sin() * a.sin())
    };
    for ring in 0..rings {
        for i in 0..SEGMENTS {
            let (a, b, c, d) = (at(ring, i), at(ring, i + 1), at(ring + 1, i + 1), at(ring + 1, i));
            push_triangle(&mut out, [a * 0.5, b * 0.5, c * 0.5], [a, b, c]);
            push_triangle(&mut out, [a * 0.5, c * 0.5, d * 0.5], [a, c, d]);
        }
    }
    out
}

impl OverlayRenderer {
    pub fn new(context: Rc<GraphicsContext>, resolution: UVec2, swapchain_textures: [Vec<u32>; 2]) -> Self {
        let device = &context.device;
        let shapes = [box_vertices(), cylinder_vertices(), sphere_vertices(), quad_vertices()];
        let mut ranges = [0..0, 0..0, 0..0, 0..0];
        let mut data = Vec::new();
        for (range, shape) in ranges.iter_mut().zip(&shapes) {
            let start = (data.len() / 6) as u32;
            data.extend(shape.iter().flatten().copied());
            *range = start..(data.len() / 6) as u32;
        }
        let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("overlay_vertices"),
            contents: &bytes,
            usage: BufferUsages::VERTEX,
        });

        let eye_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("overlay_eye"),
            entries: &[
                BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::VERTEX_FRAGMENT,
                    ty: BindingType::Buffer { ty: BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 1,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Texture {
                        sample_type: TextureSampleType::Float { filterable: true },
                        view_dimension: TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 2,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Sampler(SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let atlas = super::create_texture(device, UVec2::new(ATLAS.0, ATLAS.1), SDR_FORMAT);
        // A white corner for the shapes without text
        context.queue.write_texture(
            TexelCopyTextureInfo { texture: &atlas, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All },
            &[255u8; 4 * 4 * 4],
            TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(16), rows_per_image: Some(4) },
            Extent3d { width: 4, height: 4, depth_or_array_layers: 1 },
        );
        let atlas_view = atlas.create_view(&Default::default());
        let sampler = device.create_sampler(&SamplerDescriptor {
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            ..Default::default()
        });
        let eye = || {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("overlay_eye"),
                size: 80,
                usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let group = device.create_bind_group(&BindGroupDescriptor {
                label: Some("overlay_eye"),
                layout: &eye_layout,
                entries: &[
                    BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() },
                    BindGroupEntry { binding: 1, resource: BindingResource::TextureView(&atlas_view) },
                    BindGroupEntry { binding: 2, resource: BindingResource::Sampler(&sampler) },
                ],
            });
            (buffer, group)
        };
        let eyes = [eye(), eye()];

        let shader = device.create_shader_module(include_wgsl!("../resources/overlay.wgsl"));
        let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("overlay"),
            bind_group_layouts: &[&eye_layout],
            push_constant_ranges: &[PushConstantRange { stages: ShaderStages::VERTEX_FRAGMENT, range: 0..PUSH_SIZE }],
        });
        let solid = pipeline(device, &layout, &shader, ("solid_vertex", "solid_fragment"), true);
        let flat = pipeline(device, &layout, &shader, ("flat_vertex", "flat_fragment"), false);

        let depth_view = || {
            device
                .create_texture(&TextureDescriptor {
                    label: Some("overlay_depth"),
                    size: Extent3d { width: resolution.x, height: resolution.y, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: TextureDimension::D2,
                    format: DEPTH_FORMAT,
                    usage: TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let depth = [depth_view(), depth_view()];
        let render_targets = [
            super::create_gl_swapchain(device, &swapchain_textures[0], resolution, SDR_FORMAT),
            super::create_gl_swapchain(device, &swapchain_textures[1], resolution, SDR_FORMAT),
        ];
        Self { context, solid, flat, vertices, ranges, eyes, atlas, labels: Vec::new(), shelf: (0, ROW), depth, render_targets }
    }

    /// The number of a text for `OverlayShape::Label`. A new text goes into the atlas once.
    pub fn label(&mut self, text: &str) -> u16 {
        if let Some(i) = self.labels.iter().position(|l| l.0 == text) {
            return i as u16;
        }
        let (width, height, alpha) = rasterize_text(text, FONT_PX);
        let height = height.min(ROW);
        let (mut x, mut y) = self.shelf;
        if x + width > ATLAS.0 {
            (x, y) = (0, y + ROW);
        }
        if y + height > ATLAS.1 || self.labels.len() >= u16::MAX as usize {
            // The atlas is full: the text shows as nothing
            return u16::MAX;
        }
        let rgba: Vec<u8> = alpha.iter().flat_map(|&a| [255, 255, 255, a]).collect();
        self.context.queue.write_texture(
            TexelCopyTextureInfo { texture: &self.atlas, mip_level: 0, origin: Origin3d { x, y, z: 0 }, aspect: TextureAspect::All },
            &rgba,
            TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) },
            Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.shelf = (x + width + 2, y);
        let (aw, ah) = (ATLAS.0 as f32, ATLAS.1 as f32);
        let rect = [x as f32 / aw, y as f32 / ah, width as f32 / aw, height as f32 / ah];
        self.labels.push((text.to_string(), rect, width as f32 / height as f32));
        (self.labels.len() - 1) as u16
    }

    /// Draws the shapes for both eyes: the solid ones first, then the flat ones, which do not
    /// hide what lies behind them.
    pub fn render(&self, views: [OverlayView; 2], draws: &[OverlayDraw]) {
        let queue = &self.context.queue;
        for (view, (buffer, _)) in views.iter().zip(&self.eyes) {
            let view_matrix = Mat4::from_rotation_translation(view.pose.orientation, view.pose.position).inverse();
            let view_proj = super::projection_from_fov(view.fov) * view_matrix;
            let floats: Vec<f32> =
                view_proj.to_cols_array().into_iter().chain(view.pose.position.extend(1.0).to_array()).collect();
            let bytes: Vec<u8> = floats.iter().flat_map(|f| f.to_le_bytes()).collect();
            queue.write_buffer(buffer, 0, &bytes);
        }
        let mut encoder =
            self.context.device.create_command_encoder(&CommandEncoderDescriptor { label: Some("overlay") });
        for (eye, view) in views.iter().enumerate() {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("overlay"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &self.render_targets[eye][view.swapchain_index as usize],
                    resolve_target: None,
                    ops: Operations { load: LoadOp::Clear(Color::TRANSPARENT), store: StoreOp::Store },
                })],
                depth_stencil_attachment: Some(RenderPassDepthStencilAttachment {
                    view: &self.depth[eye],
                    depth_ops: Some(Operations { load: LoadOp::Clear(1.0), store: StoreOp::Discard }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            pass.set_bind_group(0, &self.eyes[eye].1, &[]);
            pass.set_vertex_buffer(0, self.vertices.slice(..));
            for flat in [false, true] {
                pass.set_pipeline(if flat { &self.flat } else { &self.solid });
                let is_flat = |shape: &OverlayShape| matches!(shape, OverlayShape::RoundRect { .. } | OverlayShape::Label { .. });
                for draw in draws.iter().filter(|d| is_flat(&d.shape) == flat) {
                    // The white corner of the atlas, for shapes without text
                    let white = [2.0 / ATLAS.0 as f32, 2.0 / ATLAS.1 as f32, 0.0, 0.0];
                    let (model, params, uv, range) = match draw.shape {
                        OverlayShape::Box => (draw.model, [0.0; 4], white, &self.ranges[0]),
                        OverlayShape::Cylinder => (draw.model, [0.0; 4], white, &self.ranges[1]),
                        OverlayShape::Sphere => (draw.model, [0.0; 4], white, &self.ranges[2]),
                        OverlayShape::RoundRect { width, height, radius, fade } => {
                            let quad = Vec3::new(width + 2.0 * FLAT_MARGIN, height + 2.0 * FLAT_MARGIN, 1.0);
                            (draw.model * Mat4::from_scale(quad), [radius, fade, width, height], white, &self.ranges[3])
                        }
                        OverlayShape::Label { id, width, height } => {
                            let Some((_, rect, aspect)) = self.labels.get(id as usize) else { continue };
                            let (mut w, mut h) = (height * aspect, height);
                            if w > width {
                                (w, h) = (width, width / aspect);
                            }
                            // The rectangle test passes everywhere: the text's alpha shapes it
                            (draw.model * Mat4::from_scale(Vec3::new(w, h, 1.0)), [0.0, 0.0, w + 1.0, h + 1.0], *rect, &self.ranges[3])
                        }
                    };
                    let (x, y, z, t) = (model.x_axis, model.y_axis, model.z_axis, model.w_axis);
                    let floats = [
                        x.x, x.y, x.z, t.x, y.x, y.y, y.z, t.y, z.x, z.y, z.z, t.z,
                    ]
                    .into_iter()
                    .chain(draw.color)
                    .chain(params)
                    .chain(uv);
                    let constants: Vec<u8> = floats.flat_map(|f| f.to_le_bytes()).collect();
                    pass.set_push_constants(ShaderStages::VERTEX_FRAGMENT, 0, &constants);
                    pass.draw(range.clone(), 0..1);
                }
            }
        }
        queue.submit(Some(encoder.finish()));
    }
}

/// A text at `px` pixels per em: its width and height in pixels and its coverage, row by row
/// from the top.
pub fn rasterize_text(text: &str, px: f32) -> (u32, u32, Vec<u8>) {
    let font = FontRef::try_from_slice(include_bytes!("../resources/Ubuntu-Medium.ttf")).unwrap();
    let scale = PxScale::from(px);
    let scaled = font.as_scaled(scale);
    let mut glyphs = Vec::new();
    let mut x = 1.0;
    let mut previous = None;
    for c in text.chars() {
        let id = font.glyph_id(c);
        if let Some(previous) = previous {
            x += scaled.kern(previous, id);
        }
        glyphs.push(id.with_scale_and_position(scale, point(x, 1.0 + scaled.ascent())));
        x += scaled.h_advance(id);
        previous = Some(id);
    }
    let width = (x.ceil() as u32 + 2).min(ATLAS.0);
    let height = (scaled.ascent() - scaled.descent()).ceil() as u32 + 2;
    let mut alpha = vec![0u8; (width * height) as usize];
    for glyph in glyphs {
        let Some(outline) = font.outline_glyph(glyph) else { continue };
        let bounds = outline.px_bounds();
        outline.draw(|gx, gy, coverage| {
            let (px, py) = (bounds.min.x as i32 + gx as i32, bounds.min.y as i32 + gy as i32);
            if (0..width as i32).contains(&px) && (0..height as i32).contains(&py) {
                let i = (py as u32 * width + px as u32) as usize;
                alpha[i] = alpha[i].max((coverage * 255.0) as u8);
            }
        });
    }
    (width, height, alpha)
}
