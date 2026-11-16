// cranvr: the shapes of the overlay layer. The output is premultiplied by alpha.
//
// The edges are smooth without multisampling. A flat shape computes its distance to its own edge
// and covers an edge pixel only partly. A solid shape fades where its surface turns away from the
// eye, which is its outline.

struct Eye {
    view_proj: mat4x4f,
    position: vec4f,
}
@group(0) @binding(0) var<uniform> eye: Eye;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

// The model's columns, with the translation in w; the color; for flat shapes the corner radius,
// the fade along y, and the shape's own width and height; the shape's place in the text atlas
struct PushConstant {
    c0: vec4f,
    c1: vec4f,
    c2: vec4f,
    color: vec4f,
    params: vec4f,
    uv: vec4f,
}
var<push_constant> pc: PushConstant;

fn world_of(p: vec3f) -> vec3f {
    return pc.c0.xyz * p.x + pc.c1.xyz * p.y + pc.c2.xyz * p.z + vec3f(pc.c0.w, pc.c1.w, pc.c2.w);
}

struct SolidOut {
    @builtin(position) position: vec4f,
    @location(0) world: vec3f,
    @location(1) normal: vec3f,
}

@vertex
fn solid_vertex(@location(0) position: vec3f, @location(1) normal: vec3f) -> SolidOut {
    var out: SolidOut;
    let world = world_of(position);
    out.position = eye.view_proj * vec4f(world, 1.0);
    out.world = world;
    // The models turn and scale along their own axes: the inverse transpose divides each axis by
    // its squared scale
    out.normal = pc.c0.xyz * (normal.x / dot(pc.c0.xyz, pc.c0.xyz))
        + pc.c1.xyz * (normal.y / dot(pc.c1.xyz, pc.c1.xyz))
        + pc.c2.xyz * (normal.z / dot(pc.c2.xyz, pc.c2.xyz));
    return out;
}

@fragment
fn solid_fragment(in: SolidOut) -> @location(0) vec4f {
    let n = normalize(in.normal);
    let facing = abs(dot(n, normalize(eye.position.xyz - in.world)));
    let light = normalize(vec3f(0.3, 1.0, 0.6));
    let shade = 0.45 + 0.55 * max(dot(n, light), 0.0) + 0.3 * pow(1.0 - facing, 3.0);
    let a = pc.color.a * smoothstep(0.0, 0.3, facing);
    return vec4f(pc.color.rgb * shade * a, a);
}

struct FlatOut {
    @builtin(position) position: vec4f,
    // Meters from the shape's center
    @location(0) local: vec2f,
    // 0 at the bottom edge of the quad, 1 at the top
    @location(1) along: f32,
    // 0 to 1 across the quad, from its top left corner
    @location(2) uv: vec2f,
}

@vertex
fn flat_vertex(@location(0) position: vec3f) -> FlatOut {
    var out: FlatOut;
    out.position = eye.view_proj * vec4f(world_of(position), 1.0);
    out.local = position.xy * vec2f(length(pc.c0.xyz), length(pc.c1.xyz));
    out.along = position.y + 0.5;
    out.uv = vec2f(position.x + 0.5, 0.5 - position.y);
    return out;
}

@fragment
fn flat_fragment(in: FlatOut) -> @location(0) vec4f {
    // The signed distance to a rectangle with round corners: below 0 inside
    let half = pc.params.zw * 0.5;
    let r = min(pc.params.x, min(half.x, half.y));
    let q = abs(in.local) - (half - vec2f(r));
    let d = length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
    let coverage = clamp(0.5 - d / max(fwidth(d), 1e-6), 0.0, 1.0);
    let text = textureSample(atlas, atlas_sampler, pc.uv.xy + in.uv * pc.uv.zw).a;
    let a = pc.color.a * coverage * text * (1.0 - pc.params.y * in.along);
    return vec4f(pc.color.rgb * a, a);
}
