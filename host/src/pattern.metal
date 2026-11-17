// World-locked test scene, written straight into the encoder's NV12 buffer.
// One thread fills a 2x2 block: four luma samples and one chroma sample.
#include <metal_stdlib>
using namespace metal;

struct Params {
    float4 fov[2];      // tan of the left, right, up and down angles per eye
    float4 head_q;      // head orientation (x, y, z, w)
    float4 head_p;      // head position in meters
    float4 eye_q[2];    // eye orientation relative to the head
    float4 eye_p[2];    // eye position relative to the head
    float4 ctl[2];      // controller positions; w = 1 when tracked
    float4 screen[3];   // desktop screen: center (w = 1 when shown), right and up half extents
    float4 fov_center;  // foveation: center size (xy) and center shift (zw)
    float4 fov_edge;    // foveation: edge ratio (xy) and eye size ratio (zw)
    uint eye_w;         // encoded eye size
    uint eye_h;
    uint foveated;
    uint full_range;
    uint scene;         // 0: test scene with eye badges; 1: dim room for the desktop
    uint pad[3];
};

// ALVR's CompressAxisAlignedPixelShader: a point of the encoded eye (mirrored for the right
// eye, divided by the eye size ratio) to the point of the full view it shows.
static float2 foveated_to_view(float2 e, float4 fov_center, float4 fov_edge) {
    float2 cs = fov_center.xy, sh = fov_center.zw, er = fov_edge.xy;
    float2 c0 = (1.0 - cs) / 2.0;
    float2 c1 = (er - 1.0) * c0 * (sh + 1.0) / er;
    float2 c2 = (er - 1.0) * cs + 1.0;
    float2 lo = c0 * (sh + 1.0) / c2;
    float2 hi = c0 * (sh - 1.0) / c2 + 1.0;
    float2 center = e * c2 / er + c1;
    float2 g1 = e / lo;
    float2 g2 = (1.0 - e) / (1.0 - hi);
    float2 left_edge = g1 * center + (1.0 - g1) * (e * c2);
    float2 right_edge = g2 * center + (1.0 - g2) * ((e - 1.0) * c2 + 1.0);
    return select(select(center, right_edge, e > hi), left_edge, e < lo);
}

// How many full-view pixels one encoded pixel spans on each axis at `e` (a point of the encoded
// eye as `foveated_to_view` takes it): 1 in the center, the edge ratio at the edges. An encoded
// pixel stands for the average of that many view pixels, so a feature thinner than that must not
// be sampled at one point: it would show in some pixels and miss others. The decoded and unwarped
// picture then shows a thin line as a row of dots.
static float2 compression(float2 e, float4 fov_center, float4 fov_edge) {
    const float h = 1.0 / 128.0;
    float2 lo = max(e - h, 0.0);
    float2 hi = min(e + h, 1.0);
    float2 slope = (foveated_to_view(hi, fov_center, fov_edge) - foveated_to_view(lo, fov_center, fov_edge)) / (hi - lo);
    // The slope in the center, where one encoded pixel is one view pixel
    float2 center = ((fov_edge.xy - 1.0) * fov_center.xy + 1.0) / fov_edge.xy;
    return max(slope / center, 1.0);
}

// The most taps along one axis of an encoded pixel that `compose` takes. A bilinear tap averages
// two texels, so three taps cover the edge ratio of 5 that ALVR's defaults give the vertical axis.
constant uint MAX_TAPS = 3;

static float3 qrot(float4 q, float3 v) {
    float3 t = 2.0 * cross(q.xyz, v);
    return v + q.w * t + cross(q.xyz, t);
}

static float4 qmul(float4 a, float4 b) {
    return float4(a.w * b.xyz + b.w * a.xyz + cross(a.xyz, b.xyz), a.w * b.w - dot(a.xyz, b.xyz));
}

// 1 on a grid line, 0 between lines. `footprint` is the size of one output pixel in the unit of
// `v`. A line that is thinner than the pixel spreads over the pixel and gets as much dimmer as it
// gets wider, so it keeps its weight and shows as a continuous line.
static float grid(float v, float step, float width, float footprint) {
    float d = abs(fract(v / step + 0.5) - 0.5) * step;
    float half_width = max(width, 0.5 * footprint);
    return width / half_width * (1.0 - smoothstep(0.0, half_width, d));
}

static float sphere(float3 o, float3 d, float3 c, float r) {
    float3 oc = o - c;
    float b = dot(oc, d);
    float h = b * b - (dot(oc, oc) - r * r);
    return h < 0.0 ? -1.0 : -b - sqrt(h);
}

// `px` is the size of one output pixel, as a tangent of the view angle; it picks the mip level.
static float3 shade(float3 o, float3 d, constant Params &p, uint eye, float2 uv, float px, texture2d<float, access::sample> desk) {
    float3 col;
    if (p.scene == 0) {
        // A fixed badge per eye (left red, right blue) that the probe checks
        if (uv.x > 0.08 && uv.x < 0.14 && uv.y > 0.46 && uv.y < 0.54) {
            return eye == 0 ? float3(1.0, 0.1, 0.1) : float3(0.1, 0.2, 1.0);
        }
        // Sky: latitude and longitude lines every 15 degrees, brighter every 45
        float lon = atan2(d.x, -d.z) * 57.29578;
        float lat = asin(clamp(d.y, -1.0, 1.0)) * 57.29578;
        col = mix(float3(0.05, 0.07, 0.12), float3(0.25, 0.45, 0.8), saturate(d.y * 0.5 + 0.5));
        // The angle of one pixel in degrees. Longitude lines cover more degrees of longitude the
        // nearer they are to the poles: the pixel's size is that of its latitude.
        float pixel = px * 57.29578;
        float lon_pixel = pixel / max(sqrt(1.0 - d.y * d.y), 0.05);
        col = mix(col, float3(0.8), 0.5 * max(grid(lon, 15.0, 0.25, lon_pixel), grid(lat, 15.0, 0.25, pixel)));
        col = mix(col, float3(1.0, 0.85, 0.4), 0.8 * max(grid(lon, 45.0, 0.5, lon_pixel), grid(lat, 45.0, 0.5, pixel)));
    } else {
        // A dim room, so the screen stands out
        col = mix(float3(0.02, 0.02, 0.03), float3(0.07, 0.08, 0.1), saturate(d.y * 0.5 + 0.5));
    }

    float nearest = 1e9;
    // Floor at y = 0: 50 cm checkers with green lines every meter
    if (d.y < -1e-4) {
        float t = -o.y / d.y;
        if (t > 0.0) {
            nearest = t;
            float3 h = o + t * d;
            float2 cell = floor(h.xz / 0.5);
            float3 floor_col = mix(float3(0.25), float3(0.6), fmod(abs(cell.x + cell.y), 2.0));
            // One pixel covers this much of the floor, along the ray's long axis
            float spot = min(t * px / max(abs(d.y), 0.05), 1.0);
            floor_col = mix(floor_col, float3(0.1, 0.8, 0.3), max(grid(h.x, 1.0, 0.01, spot), grid(h.z, 1.0, 0.01, spot)));
            floor_col *= p.scene == 0 ? 1.0 : 0.3;
            col = mix(floor_col, col, saturate(t / 40.0));
        }
    }

    // The desktop screen: a flat rectangle that shows the captured display
    if (p.screen[0].w > 0.5) {
        float3 c = p.screen[0].xyz, r = p.screen[1].xyz, u = p.screen[2].xyz;
        float3 n = cross(r, u);
        float facing = dot(d, n);
        if (abs(facing) > 1e-6) {
            float t = dot(c - o, n) / facing;
            if (t > 0.0 && t < nearest) {
                float3 h = o + t * d - c;
                float x = dot(h, r) / dot(r, r);
                float y = dot(h, u) / dot(u, u);
                if (abs(x) <= 1.0 && abs(y) <= 1.0) {
                    constexpr sampler s(filter::linear, mip_filter::linear, address::clamp_to_edge, coord::normalized);
                    nearest = t;
                    // texels under one output pixel, for the mip level
                    float footprint = t * px / max(abs(facing) / length(n), 0.25);
                    float texels = footprint * float(desk.get_width()) / (2.0 * length(r));
                    col = desk.sample(s, float2((x + 1.0) * 0.5, (1.0 - y) * 0.5), level(max(log2(texels), 0.0))).rgb;
                }
            }
        }
    }

    // Controllers: 4 cm balls, yellow left and cyan right
    for (uint i = 0; i < 2; i++) {
        if (p.ctl[i].w > 0.5) {
            float t = sphere(o, d, p.ctl[i].xyz, 0.04);
            if (t > 0.0 && t < nearest) {
                nearest = t;
                col = i == 0 ? float3(1.0, 0.8, 0.1) : float3(0.1, 0.9, 1.0);
            }
        }
    }
    return col;
}

kernel void pattern(texture2d<float, access::write> luma [[texture(0)]],
                    texture2d<float, access::write> chroma [[texture(1)]],
                    texture2d<float, access::sample> desk [[texture(2)]],
                    constant Params &p [[buffer(0)]],
                    uint2 gid [[thread_position_in_grid]]) {
    uint width = p.eye_w * 2;
    if (gid.x * 2 >= width || gid.y * 2 >= p.eye_h) {
        return;
    }
    float3 sum = float3(0.0);
    for (uint k = 0; k < 4; k++) {
        uint2 px = gid * 2 + uint2(k & 1, k >> 1);
        uint eye = px.x >= p.eye_w ? 1 : 0;
        float2 uv = float2((float(px.x - eye * p.eye_w) + 0.5) / float(p.eye_w), (float(px.y) + 0.5) / float(p.eye_h));
        // The size of this pixel in the full view, as a multiple of the center's pixel
        float squeeze = 1.0;
        if (p.foveated != 0) {
            // the right eye is stored mirrored, so its outer edge is at the frame's right side
            float2 e = float2(eye == 1 ? 1.0 - uv.x : uv.x, uv.y) / p.fov_edge.zw;
            float2 c = compression(e, p.fov_center, p.fov_edge);
            squeeze = max(c.x, c.y);
            float2 v = foveated_to_view(e, p.fov_center, p.fov_edge);
            uv = float2(eye == 1 ? 1.0 - v.x : v.x, v.y);
        }
        float4 f = p.fov[eye];
        float3 ray_eye = normalize(float3(mix(f.x, f.y, uv.x), mix(f.z, f.w, uv.y), -1.0));
        float3 ray = normalize(qrot(qmul(p.head_q, p.eye_q[eye]), ray_eye));
        float3 origin = p.head_p.xyz + qrot(p.head_q, p.eye_p[eye].xyz);
        // The scene has no texels to average. It filters its thin features by the pixel's size.
        float pixel = (f.y - f.x) / float(p.eye_w);
        float3 rgb = saturate(shade(origin, ray, p, eye, uv, pixel * squeeze, desk));
        float y = dot(rgb, float3(0.2126, 0.7152, 0.0722));
        luma.write(float4(p.full_range != 0 ? y : (16.0 + 219.0 * y) / 255.0), px);
        sum += rgb;
    }
    // BT.709 chroma from the average of the four samples
    float3 rgb = sum * 0.25;
    float y = dot(rgb, float3(0.2126, 0.7152, 0.0722));
    float2 c = float2((rgb.b - y) / 1.8556, (rgb.r - y) / 1.5748);
    c = p.full_range != 0 ? c + 0.5 : (c * 224.0 + 128.0) / 255.0;
    chroma.write(float4(c, 0.0, 0.0), gid);
}

// Game frames: copy each eye of the game's swapchain images into the encoder's NV12 buffer,
// with the same foveation, and remap if the game rendered a different field of view.
struct ComposeParams {
    float4 fov[2];      // tangents of the stream's views (left, right, up, down)
    float4 src_fov[2];  // tangents the game rendered with
    float4 src_rect[2]; // the game's image rect, in normalized texture coordinates (x, y, w, h)
    float4 fov_center;
    float4 fov_edge;
    uint eye_w;
    uint eye_h;
    uint foveated;
    uint full_range;
};

// One tap of `compose`: the game's color at the point of the full view that the encoded eye
// position `pe` shows (in pixels of the encoded eye, as a fraction of a pixel after its corner).
static float3 compose_tap(float2 pe, uint eye, constant ComposeParams &p, texture2d<float, access::sample> left,
                          texture2d<float, access::sample> right, sampler s) {
    float2 uv = pe / float2(p.eye_w, p.eye_h);
    if (p.foveated != 0) {
        float2 e = float2(eye == 1 ? 1.0 - uv.x : uv.x, uv.y) / p.fov_edge.zw;
        float2 v = foveated_to_view(e, p.fov_center, p.fov_edge);
        uv = float2(eye == 1 ? 1.0 - v.x : v.x, v.y);
    }
    float4 f = p.fov[eye];
    float2 t = float2(mix(f.x, f.y, uv.x), mix(f.z, f.w, uv.y));
    float4 g = p.src_fov[eye];
    float2 src = float2((t.x - g.x) / (g.y - g.x), (g.z - t.y) / (g.z - g.w));
    float4 r = p.src_rect[eye];
    float2 tex = r.xy + src * r.zw;
    // the game's image already holds display values (sRGB swapchains read as plain unorm)
    float3 rgb = eye == 0 ? left.sample(s, tex, level(0)).rgb : right.sample(s, tex, level(0)).rgb;
    return any(src < 0.0) || any(src > 1.0) ? float3(0.0) : rgb;
}

kernel void compose(texture2d<float, access::write> luma [[texture(0)]],
                    texture2d<float, access::write> chroma [[texture(1)]],
                    texture2d<float, access::sample> left [[texture(2)]],
                    texture2d<float, access::sample> right [[texture(3)]],
                    constant ComposeParams &p [[buffer(0)]],
                    uint2 gid [[thread_position_in_grid]]) {
    constexpr sampler s(filter::linear, address::clamp_to_zero, coord::normalized);
    uint width = p.eye_w * 2;
    if (gid.x * 2 >= width || gid.y * 2 >= p.eye_h) {
        return;
    }
    float3 sum = float3(0.0);
    for (uint k = 0; k < 4; k++) {
        uint2 px = gid * 2 + uint2(k & 1, k >> 1);
        uint eye = px.x >= p.eye_w ? 1 : 0;
        float2 pe = float2(float(px.x - eye * p.eye_w), float(px.y));
        // An edge pixel stands for several pixels of the game's image. Averaging taps over the
        // pixel, laid out in the encoded picture, keeps a thin line that the compression squeezes
        // below one pixel. One tap would show the line in some pixels and miss it in others, and
        // the headset would unwarp that into a row of dots.
        uint2 taps = uint2(1, 1);
        if (p.foveated != 0) {
            float2 uv = (pe + 0.5) / float2(p.eye_w, p.eye_h);
            float2 e = float2(eye == 1 ? 1.0 - uv.x : uv.x, uv.y) / p.fov_edge.zw;
            taps = uint2(clamp(ceil(0.5 * compression(e, p.fov_center, p.fov_edge)), 1.0, float(MAX_TAPS)));
        }
        float3 rgb = float3(0.0);
        for (uint ty = 0; ty < taps.y; ty++) {
            for (uint tx = 0; tx < taps.x; tx++) {
                rgb += compose_tap(pe + (float2(tx, ty) + 0.5) / float2(taps), eye, p, left, right, s);
            }
        }
        rgb /= float(taps.x * taps.y);
        float y = dot(rgb, float3(0.2126, 0.7152, 0.0722));
        luma.write(float4(p.full_range != 0 ? y : (16.0 + 219.0 * y) / 255.0), px);
        sum += rgb;
    }
    float3 rgb = sum * 0.25;
    float y = dot(rgb, float3(0.2126, 0.7152, 0.0722));
    float2 c = float2((rgb.b - y) / 1.8556, (rgb.r - y) / 1.5748);
    c = p.full_range != 0 ? c + 0.5 : (c * 224.0 + 128.0) / 255.0;
    chroma.write(float4(c, 0.0, 0.0), gid);
}
