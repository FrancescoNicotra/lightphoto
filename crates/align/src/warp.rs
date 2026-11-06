//! Inverse-mapping warps and the interpolation kernels shared with ECC.
//!
//! Every output pixel centre `p` is sent through the transform to `T(p)` in
//! the source and sampled there. Sample positions passed to the kernels are
//! in *index* coordinates (`x − 0.5`), so integer positions hit pixel centres.

use rayon::prelude::*;

use crate::Transform;

/// How [`warp_plane`] samples the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interp {
    Bilinear,
    /// Catmull-Rom 4×4, limited to the min/max of the 4 nearest pixels.
    CatmullRom,
    /// Maximum of the 4 nearest pixels: for masks (e.g. clipping), so a
    /// flagged pixel is never interpolated away.
    Max,
}

#[inline]
fn clamp_idx(i: isize, n: usize) -> usize {
    i.clamp(0, n as isize - 1) as usize
}

/// Integer part and fraction of an index coordinate.
#[inline]
fn split(v: f64) -> (isize, f32) {
    let f = v.floor();
    (f as isize, (v - f) as f32)
}

#[inline]
pub(crate) fn bilinear(src: &[f32], w: usize, h: usize, x: f64, y: f64) -> f32 {
    let (ix, tx) = split(x);
    let (iy, ty) = split(y);
    let (x0, x1) = (clamp_idx(ix, w), clamp_idx(ix + 1, w));
    let (y0, y1) = (clamp_idx(iy, h), clamp_idx(iy + 1, h));
    let r0 = src[y0 * w + x0] + tx * (src[y0 * w + x1] - src[y0 * w + x0]);
    let r1 = src[y1 * w + x0] + tx * (src[y1 * w + x1] - src[y1 * w + x0]);
    r0 + ty * (r1 - r0)
}

#[inline]
fn catmull_rom_weights(t: f32) -> [f32; 4] {
    let t2 = t * t;
    let t3 = t2 * t;
    [
        0.5 * (-t + 2.0 * t2 - t3),
        0.5 * (2.0 - 5.0 * t2 + 3.0 * t3),
        0.5 * (t + 4.0 * t2 - 3.0 * t3),
        0.5 * (t3 - t2),
    ]
}

/// The 4×4 footprint of a cubic sample: clamped indices and weights.
struct Footprint {
    xs: [usize; 4],
    ys: [usize; 4],
    wx: [f32; 4],
    wy: [f32; 4],
}

#[inline]
fn footprint(w: usize, h: usize, x: f64, y: f64) -> Footprint {
    let (ix, tx) = split(x);
    let (iy, ty) = split(y);
    Footprint {
        xs: std::array::from_fn(|k| clamp_idx(ix - 1 + k as isize, w)),
        ys: std::array::from_fn(|k| clamp_idx(iy - 1 + k as isize, h)),
        wx: catmull_rom_weights(tx),
        wy: catmull_rom_weights(ty),
    }
}

impl Footprint {
    /// Catmull-Rom value of channel `get`, limited to the 2×2 neighbours.
    #[inline]
    fn eval(&self, get: impl Fn(usize) -> f32, w: usize) -> f32 {
        let mut v = 0.0;
        for (&y, &wy) in self.ys.iter().zip(&self.wy) {
            let row = y * w;
            let mut r = 0.0;
            for (&x, &wx) in self.xs.iter().zip(&self.wx) {
                r += wx * get(row + x);
            }
            v += wy * r;
        }
        let n = [
            get(self.ys[1] * w + self.xs[1]),
            get(self.ys[1] * w + self.xs[2]),
            get(self.ys[2] * w + self.xs[1]),
            get(self.ys[2] * w + self.xs[2]),
        ];
        let lo = n.iter().copied().fold(f32::INFINITY, f32::min);
        let hi = n.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        v.clamp(lo, hi)
    }
}

#[inline]
pub(crate) fn cubic(src: &[f32], w: usize, h: usize, x: f64, y: f64) -> f32 {
    footprint(w, h, x, y).eval(|i| src[i], w)
}

/// Maximum over the bilinear footprint: the 4 nearest pixels, or fewer when
/// the position falls exactly on a pixel row or column.
#[inline]
fn max4(src: &[f32], w: usize, h: usize, x: f64, y: f64) -> f32 {
    let (ix, tx) = split(x);
    let (iy, ty) = split(y);
    let (x0, x1) = (clamp_idx(ix, w), clamp_idx(ix + (tx > 0.0) as isize, w));
    let (y0, y1) = (clamp_idx(iy, h), clamp_idx(iy + (ty > 0.0) as isize, h));
    src[y0 * w + x0]
        .max(src[y0 * w + x1])
        .max(src[y1 * w + x0])
        .max(src[y1 * w + x1])
}

/// Source position (index coordinates) of output pixel `(i, j)`, or `None`
/// if it falls outside the `w × h` source rectangle.
#[inline]
fn source_pos(t: &Transform, i: usize, j: usize, w: usize, h: usize) -> Option<(f64, f64)> {
    let (x, y) = t.apply(i as f64 + 0.5, j as f64 + 0.5);
    // Comparisons with NaN are false, so points sent to infinity are rejected.
    if x >= 0.0 && x <= w as f64 && y >= 0.0 && y <= h as f64 {
        Some((x - 0.5, y - 0.5))
    } else {
        None
    }
}

/// Shared driver: fills an `out_w × out_h` image row by row; `sample` gets
/// the source position, or `None` outside the source (coverage = false).
fn warp_rows<T: Send + Default + Copy>(
    w: usize,
    h: usize,
    t: &Transform,
    out_w: usize,
    out_h: usize,
    sample: impl Fn(Option<(f64, f64)>) -> T + Sync,
) -> (Vec<T>, Vec<bool>) {
    let mut out = vec![T::default(); out_w * out_h];
    let mut cov = vec![false; out_w * out_h];
    if out_w == 0 || w == 0 || h == 0 {
        return (out, cov);
    }
    out.par_chunks_mut(out_w)
        .zip(cov.par_chunks_mut(out_w))
        .enumerate()
        .for_each(|(j, (row, crow))| {
            for (i, (v, c)) in row.iter_mut().zip(crow.iter_mut()).enumerate() {
                let p = source_pos(t, i, j, w, h);
                *c = p.is_some();
                *v = sample(p);
            }
        });
    (out, cov)
}

/// Warps a single-channel `w × h` plane into an `out_w × out_h` frame (usually
/// the reference's): output pixel `p` takes the source value at `t(p)`.
/// Returns the values and the coverage (`false` where `t(p)` falls outside the
/// source; those values are 0).
pub fn warp_plane(
    src: &[f32],
    w: usize,
    h: usize,
    t: &Transform,
    out_w: usize,
    out_h: usize,
    interp: Interp,
) -> (Vec<f32>, Vec<bool>) {
    assert_eq!(src.len(), w * h, "plane size mismatch");
    warp_rows(w, h, t, out_w, out_h, |p| match p {
        None => 0.0,
        Some((x, y)) => match interp {
            Interp::Bilinear => bilinear(src, w, h, x, y),
            Interp::CatmullRom => cubic(src, w, h, x, y),
            Interp::Max => max4(src, w, h, x, y),
        },
    })
}

/// Warps an RGB image with Catmull-Rom (limited per channel to the min/max of
/// the 4 nearest pixels, so no ringing near clipping). Same conventions as
/// [`warp_plane`].
pub fn warp_rgb(
    src: &[[f32; 3]],
    w: usize,
    h: usize,
    t: &Transform,
    out_w: usize,
    out_h: usize,
) -> (Vec<[f32; 3]>, Vec<bool>) {
    assert_eq!(src.len(), w * h, "image size mismatch");
    warp_rows(w, h, t, out_w, out_h, |p| match p {
        None => [0.0; 3],
        Some((x, y)) => {
            let f = footprint(w, h, x, y);
            std::array::from_fn(|c| f.eval(|i| src[i][c], w))
        }
    })
}
