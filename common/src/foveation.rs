//! ALVR's foveated encoding, ported from ALVR v20.14.1 so the stock ALVR client can unwarp what
//! the host encodes:
//! - server side: `FFR.cpp` and `CompressAxisAlignedPixelShader.hlsl`;
//! - client side: `foveated_encoding_shader_constants` and `stream.wgsl`.
//!
//! Each eye keeps full pixel density in a center region. The edges are compressed by
//! `edge_ratio`. The right eye is mirrored, so for both eyes the "left" edge is the outer one.
//! All UVs here are in that mirrored eye space: for the right eye, x runs from the outer edge.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FoveationConfig {
    pub center_size: [f32; 2],
    pub center_shift: [f32; 2],
    pub edge_ratio: [f32; 2],
}

impl Default for FoveationConfig {
    /// ALVR's defaults.
    fn default() -> Self {
        Self { center_size: [0.45, 0.4], center_shift: [0.4, 0.1], edge_ratio: [4.0, 5.0] }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Foveation {
    /// Full eye size, before compression.
    pub target: [u32; 2],
    /// Encoded eye size, a multiple of 32.
    pub optimized: [u32; 2],
    /// Unpadded encoded size divided by `optimized`.
    pub eye_size_ratio: [f32; 2],
    pub center_size: [f32; 2],
    pub center_shift: [f32; 2],
    pub edge_ratio: [f32; 2],
}

impl Foveation {
    pub fn new(target: [u32; 2], config: &FoveationConfig) -> Self {
        let mut f = Self {
            target,
            optimized: [0; 2],
            eye_size_ratio: [0.0; 2],
            center_size: [0.0; 2],
            center_shift: [0.0; 2],
            edge_ratio: config.edge_ratio,
        };
        for i in 0..2 {
            let t = target[i] as f32;
            let er = config.edge_ratio[i];
            let edge_size = t - config.center_size[i] * t;
            let center_aligned = 1.0 - (edge_size / (er * 2.0)).ceil() * (er * 2.0) / t;
            let edge_aligned = t - center_aligned * t;
            let shift_aligned = (config.center_shift[i] * edge_aligned / (er * 2.0)).ceil() * (er * 2.0) / edge_aligned;
            let optimized = (center_aligned + (1.0 - center_aligned) / er) * t;
            let optimized_aligned = (optimized / 32.0).ceil() * 32.0;
            f.center_size[i] = center_aligned;
            f.center_shift[i] = shift_aligned;
            f.optimized[i] = optimized_aligned as u32;
            f.eye_size_ratio[i] = optimized / optimized_aligned;
        }
        f
    }

    fn axis(&self, i: usize) -> Axis {
        let (cs, sh, er) = (self.center_size[i], self.center_shift[i], self.edge_ratio[i]);
        let c0 = (1.0 - cs) * 0.5;
        Axis { c0, c1: (er - 1.0) * c0 * (sh + 1.0) / er, c2: (er - 1.0) * cs + 1.0, sh, er, ratio: self.eye_size_ratio[i] }
    }

    /// Server direction: a point of the encoded eye image (0..1 of the padded eye) to the point
    /// of the full view that it shows.
    pub fn encoded_to_view(&self, encoded: [f32; 2]) -> [f32; 2] {
        [self.axis(0).encoded_to_view(encoded[0]), self.axis(1).encoded_to_view(encoded[1])]
    }

    /// Client direction: a point of the full view to where it sits in the encoded eye image.
    pub fn view_to_encoded(&self, view: [f32; 2]) -> [f32; 2] {
        [self.axis(0).view_to_encoded(view[0]), self.axis(1).view_to_encoded(view[1])]
    }
}

struct Axis {
    c0: f32,
    c1: f32,
    c2: f32,
    sh: f32,
    er: f32,
    ratio: f32,
}

impl Axis {
    fn encoded_to_view(&self, encoded: f32) -> f32 {
        let Axis { c0, c1, c2, sh, er, ratio } = *self;
        let e = encoded / ratio;
        let lo = c0 * (sh + 1.0) / c2;
        let hi = c0 * (sh - 1.0) / c2 + 1.0;
        let center = e * c2 / er + c1;
        if e < lo {
            let g = e / lo;
            g * center + (1.0 - g) * e * c2
        } else if e > hi {
            let g = (1.0 - e) / (1.0 - hi);
            g * center + (1.0 - g) * ((e - 1.0) * c2 + 1.0)
        } else {
            center
        }
    }

    fn view_to_encoded(&self, view: f32) -> f32 {
        let Axis { c0, c1, c2, sh, er, ratio } = *self;
        let lo_bound = c0 * (sh + 1.0);
        let hi_bound = c0 * (sh - 1.0) + 1.0;
        let lo_c = c0 * (sh + 1.0) / c2;
        let hi_c = c0 * (sh - 1.0) / c2 + 1.0;
        let r = if view < lo_bound {
            let a = c2 * (1.0 - er) / (er * lo_c);
            let b = (c1 + c2 * lo_c) / lo_c;
            (-b + (b * b + 4.0 * a * view).sqrt()) / (2.0 * a)
        } else if view > hi_bound {
            let a = c2 * (er - 1.0) / (er * (1.0 - hi_c));
            let b = (c2 - er * c1 - 2.0 * er * c2 + c2 * er * (1.0 - hi_c) + er) / (er * (1.0 - hi_c));
            let c = (c2 * er - c2) * (c1 - hi_c + c2 * hi_c) / (er * (1.0 - hi_c) * (1.0 - hi_c));
            (-b + (b * b - 4.0 * (c - a * view)).sqrt()) / (2.0 * a)
        } else {
            (view - c1) * er / c2
        };
        r * ratio
    }
}

/// Where a point of an eye's full view sits in the side-by-side encoded frame (0..1 of the frame).
/// `view` is in normal (not mirrored) eye space.
pub fn view_to_frame(f: Option<&Foveation>, eye: usize, view: [f32; 2]) -> [f32; 2] {
    let Some(f) = f else {
        return [(eye as f32 + view[0]) * 0.5, view[1]];
    };
    let mirrored = if eye == 1 { [1.0 - view[0], view[1]] } else { view };
    let e = f.view_to_encoded(mirrored);
    if eye == 1 {
        [1.0 - e[0] * 0.5, e[1]]
    } else {
        [e[0] * 0.5, e[1]]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quest3_size_shrinks_to_about_a_third() {
        let f = Foveation::new([2064, 2208], &FoveationConfig::default());
        assert_eq!(f.optimized[0] % 32, 0);
        assert_eq!(f.optimized[1] % 32, 0);
        let fraction = (f.optimized[0] * f.optimized[1]) as f32 / (2064.0 * 2208.0);
        assert!(fraction > 0.28 && fraction < 0.34, "fraction {fraction}");
    }

    /// The server and client formulas must be inverses, or the headset shows a distorted image.
    #[test]
    fn client_unwarp_inverts_server_warp() {
        for target in [[1680u32, 1760], [2064, 2208], [1248, 1312]] {
            let f = Foveation::new(target, &FoveationConfig::default());
            for i in 1..100 {
                for j in [5, 50, 95] {
                    let view = [i as f32 / 100.0, j as f32 / 100.0];
                    let back = f.encoded_to_view(f.view_to_encoded(view));
                    assert!((back[0] - view[0]).abs() < 1e-3 && (back[1] - view[1]).abs() < 1e-3, "{target:?} {view:?} -> {back:?}");
                }
            }
        }
    }

    #[test]
    fn center_keeps_full_density_and_edges_compress() {
        let f = Foveation::new([2064, 2208], &FoveationConfig::default());
        let density = |x: f32| {
            let a = f.view_to_encoded([x, 0.5])[0];
            let b = f.view_to_encoded([x + 0.001, 0.5])[0];
            (b - a) / 0.001 * f.optimized[0] as f32 / f.target[0] as f32
        };
        assert!((density(0.55) - 1.0).abs() < 0.05, "center {}", density(0.55));
        assert!(density(0.02) < 0.4, "outer edge {}", density(0.02));
    }
}
