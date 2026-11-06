//! Enhanced Correlation Coefficient maximisation (Evangelidis & Psarakis,
//! 2008), inverse compositional form, with per-pixel weights.
//!
//! The reference is linearised under a small warp `ΔW(x; Δp)`, so its
//! gradients and the warp Jacobian are fixed for a level; each iteration only
//! samples the moving image at `T(x)`. The closed-form ECC step makes the
//! result invariant to gain and offset between the two images:
//!
//! ```text
//! ē, ī   zero-mean (weighted) reference and warped moving image
//! G      zero-mean Jacobian of the reference w.r.t. Δp,  H = GᵀG
//! λ      = (ēᵀē − ēᵀG H⁻¹ Gᵀē) / (īᵀē − īᵀG H⁻¹ Gᵀē)
//! Δp     = H⁻¹ Gᵀ(λ ī − ē)
//! T      ← T ∘ ΔW(Δp)⁻¹
//! ```
//!
//! Parameters live in normalised coordinates (centre of the level at 0, half
//! the long side = 1) so the homography terms are well conditioned.

use rayon::prelude::*;

use crate::warp::{bilinear, cubic};
use crate::{Model, Plane, Transform};

/// A row span `[x0, x1)` of row `y` that takes part in the estimate.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Span {
    pub y: usize,
    pub x0: usize,
    pub x1: usize,
}

/// One pyramid level of the reference, with what ECC precomputes on it.
pub(crate) struct RefLevel<'a> {
    pub plane: &'a Plane,
    pub gx: &'a [f32],
    pub gy: &'a [f32],
    pub spans: &'a [Span],
}

pub(crate) struct LevelResult {
    pub transform: Transform,
    pub correlation: f64,
    pub converged: bool,
}

/// Runs ECC on one level, starting from `t` (reference → moving, in this
/// level's pixel coordinates).
pub(crate) fn refine(
    r: &RefLevel,
    m: &Plane,
    t: Transform,
    model: Model,
    max_iterations: u32,
    tolerance: f64,
    use_cubic: bool,
) -> LevelResult {
    let ctx = Ctx::new(r, m, use_cubic);
    match model {
        Model::Translation => ctx.run(
            t,
            max_iterations,
            tolerance,
            |gu, gv, _, _| [gu, gv],
            |p: &[f64; 2]| [[1.0, 0.0, p[0]], [0.0, 1.0, p[1]], [0.0, 0.0, 1.0]],
        ),
        Model::Euclidean => ctx.run(
            t,
            max_iterations,
            tolerance,
            |gu, gv, u, v| [gu, gv, u * gv - v * gu],
            |p: &[f64; 3]| {
                let (s, c) = p[2].sin_cos();
                [[c, -s, p[0]], [s, c, p[1]], [0.0, 0.0, 1.0]]
            },
        ),
        Model::Similarity => ctx.run(
            t,
            max_iterations,
            tolerance,
            |gu, gv, u, v| [gu, gv, u * gu + v * gv, u * gv - v * gu],
            |p: &[f64; 4]| {
                [
                    [1.0 + p[2], -p[3], p[0]],
                    [p[3], 1.0 + p[2], p[1]],
                    [0.0, 0.0, 1.0],
                ]
            },
        ),
        Model::Homography => ctx.run(
            t,
            max_iterations,
            tolerance,
            |gu, gv, u, v| {
                let r = u * gu + v * gv;
                [u * gu, v * gu, gu, u * gv, v * gv, gv, -u * r, -v * r]
            },
            |p: &[f64; 8]| {
                [
                    [1.0 + p[0], p[1], p[2]],
                    [p[3], 1.0 + p[4], p[5]],
                    [p[6], p[7], 1.0],
                ]
            },
        ),
    }
}

struct Ctx<'a> {
    r: &'a RefLevel<'a>,
    m: &'a Plane,
    use_cubic: bool,
    /// Pixel → normalised coordinates, and back.
    norm: Transform,
    denorm: Transform,
    scale: f32,
}

impl<'a> Ctx<'a> {
    fn new(r: &'a RefLevel<'a>, m: &'a Plane, use_cubic: bool) -> Self {
        let (w, h) = (r.plane.width as f64, r.plane.height as f64);
        let s = 0.5 * w.max(h);
        let (cx, cy) = (0.5 * w, 0.5 * h);
        Ctx {
            r,
            m,
            use_cubic,
            norm: Transform([
                [1.0 / s, 0.0, -cx / s],
                [0.0, 1.0 / s, -cy / s],
                [0.0, 0.0, 1.0],
            ]),
            denorm: Transform([[s, 0.0, cx], [0.0, s, cy], [0.0, 0.0, 1.0]]),
            scale: s as f32,
        }
    }

    fn run<const N: usize>(
        &self,
        mut t: Transform,
        max_iterations: u32,
        tolerance: f64,
        jac: impl Fn(f32, f32, f32, f32) -> [f32; N] + Sync,
        increment: impl Fn(&[f64; N]) -> [[f64; 3]; 3],
    ) -> LevelResult {
        let (w, h) = (self.r.plane.width, self.r.plane.height);
        let mut converged = false;
        let mut iterations = 0;
        loop {
            let acc = self.accumulate(&t, &jac);
            let Some(stats) = acc.centred() else {
                return LevelResult {
                    transform: t,
                    correlation: 0.0,
                    converged: false,
                };
            };
            if converged || iterations >= max_iterations {
                return LevelResult {
                    transform: t,
                    correlation: stats.rho(),
                    converged,
                };
            }
            let Some(dp) = stats.step() else {
                return LevelResult {
                    transform: t,
                    correlation: stats.rho(),
                    converged: false,
                };
            };
            let dw = self
                .denorm
                .compose(&Transform(increment(&dp)))
                .compose(&self.norm);
            let Some(dw_inv) = dw.inverse() else {
                return LevelResult {
                    transform: t,
                    correlation: stats.rho(),
                    converged: false,
                };
            };
            t = t.compose(&dw_inv);
            iterations += 1;
            if dw.max_corner_shift(w, h) < tolerance {
                converged = true;
            }
        }
    }

    fn accumulate<const N: usize>(
        &self,
        t: &Transform,
        jac: &(impl Fn(f32, f32, f32, f32) -> [f32; N] + Sync),
    ) -> Acc<N> {
        let r = self.r.plane;
        let m = self.m;
        let (rw, mw, mh) = (r.width, m.width, m.height);
        let (xmax, ymax) = ((mw - 1) as f64, (mh - 1) as f64);
        let n = &self.norm.0;
        let s = self.scale;
        self.r
            .spans
            .par_iter()
            .fold(Acc::<N>::zero, |mut acc, span| {
                let y = span.y;
                let py = y as f64 + 0.5;
                let v = (n[1][1] * py + n[1][2]) as f32;
                for x in span.x0..span.x1 {
                    let i = y * rw + x;
                    let wr = r.weight.as_ref().map_or(1.0, |wt| wt[i]);
                    if wr <= 0.0 {
                        continue;
                    }
                    let px = x as f64 + 0.5;
                    let (qx, qy) = t.apply(px, py);
                    let (sx, sy) = (qx - 0.5, qy - 0.5);
                    // Comparisons with NaN are false: also skips points at infinity.
                    if !(sx >= 0.0 && sx <= xmax && sy >= 0.0 && sy <= ymax) {
                        continue;
                    }
                    let wm = m.weight.as_ref().map_or(1.0, |wt| min4(wt, mw, sx, sy));
                    let wgt = wr * wm;
                    if wgt <= 0.0 {
                        continue;
                    }
                    let iv = if self.use_cubic {
                        cubic(&m.data, mw, mh, sx, sy)
                    } else {
                        bilinear(&m.data, mw, mh, sx, sy)
                    };
                    let u = (n[0][0] * px + n[0][2]) as f32;
                    let g = jac(self.r.gx[i] * s, self.r.gy[i] * s, u, v);
                    acc.add(wgt as f64, r.data[i] as f64, iv as f64, &g);
                }
                acc
            })
            .reduce(Acc::<N>::zero, |a, b| a.merge(b))
    }
}

/// Minimum of the 4 pixels around an interior index position.
#[inline]
fn min4(src: &[f32], w: usize, x: f64, y: f64) -> f32 {
    let (ix, iy) = (x as usize, y as usize);
    let i = iy * w + ix;
    let (x1, y1) = (
        if (ix as f64) < x { 1 } else { 0 },
        if (iy as f64) < y { w } else { 0 },
    );
    src[i]
        .min(src[i + x1])
        .min(src[i + y1])
        .min(src[i + x1 + y1])
}

/// Weighted raw sums over the valid pixels.
struct Acc<const N: usize> {
    sw: f64,
    se: f64,
    see: f64,
    si: f64,
    sii: f64,
    sei: f64,
    sg: [f64; N],
    sgg: [[f64; N]; N],
    sge: [f64; N],
    sgi: [f64; N],
}

impl<const N: usize> Acc<N> {
    fn zero() -> Self {
        Acc {
            sw: 0.0,
            se: 0.0,
            see: 0.0,
            si: 0.0,
            sii: 0.0,
            sei: 0.0,
            sg: [0.0; N],
            sgg: [[0.0; N]; N],
            sge: [0.0; N],
            sgi: [0.0; N],
        }
    }

    #[inline]
    fn add(&mut self, w: f64, e: f64, i: f64, g: &[f32; N]) {
        let (we, wi) = (w * e, w * i);
        self.sw += w;
        self.se += we;
        self.see += we * e;
        self.si += wi;
        self.sii += wi * i;
        self.sei += we * i;
        for k in 0..N {
            let gk = g[k] as f64;
            let wg = w * gk;
            self.sg[k] += wg;
            self.sge[k] += wg * e;
            self.sgi[k] += wg * i;
            for (s, &gl) in self.sgg[k][k..].iter_mut().zip(&g[k..]) {
                *s += wg * gl as f64;
            }
        }
    }

    fn merge(mut self, o: Self) -> Self {
        self.sw += o.sw;
        self.se += o.se;
        self.see += o.see;
        self.si += o.si;
        self.sii += o.sii;
        self.sei += o.sei;
        for k in 0..N {
            self.sg[k] += o.sg[k];
            self.sge[k] += o.sge[k];
            self.sgi[k] += o.sgi[k];
            for l in k..N {
                self.sgg[k][l] += o.sgg[k][l];
            }
        }
        self
    }

    /// Centred (zero-mean) second moments, or `None` with too little data.
    fn centred(&self) -> Option<Stats<N>> {
        // Need comfortably more (weighted) pixels than parameters.
        if self.sw < (20 * N).max(64) as f64 {
            return None;
        }
        let n = self.sw;
        // Only the upper triangle of `sgg` is accumulated.
        let h = std::array::from_fn(|k| {
            std::array::from_fn(|l| self.sgg[k.min(l)][k.max(l)] - self.sg[k] * self.sg[l] / n)
        });
        Some(Stats {
            ee: self.see - self.se * self.se / n,
            ii: self.sii - self.si * self.si / n,
            ei: self.sei - self.se * self.si / n,
            h,
            ge: std::array::from_fn(|k| self.sge[k] - self.sg[k] * self.se / n),
            gi: std::array::from_fn(|k| self.sgi[k] - self.sg[k] * self.si / n),
        })
    }
}

struct Stats<const N: usize> {
    ee: f64,
    ii: f64,
    ei: f64,
    h: [[f64; N]; N],
    ge: [f64; N],
    gi: [f64; N],
}

impl<const N: usize> Stats<N> {
    fn rho(&self) -> f64 {
        let d = (self.ee * self.ii).sqrt();
        if d > 0.0 {
            self.ei / d
        } else {
            0.0
        }
    }

    /// The ECC parameter update, or `None` if the system is degenerate or the
    /// images are not positively correlated.
    fn step(&self) -> Option<[f64; N]> {
        let he = solve(self.h, self.ge)?;
        let hi = solve(self.h, self.gi)?;
        let dot = |a: &[f64; N], b: &[f64; N]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
        let num = self.ee - dot(&self.ge, &he);
        let den = self.ei - dot(&self.gi, &he);
        if den <= 0.0 || num <= 0.0 {
            return None;
        }
        let lambda = num / den;
        Some(std::array::from_fn(|k| lambda * hi[k] - he[k]))
    }
}

/// Solves `a · x = b` by Gaussian elimination with partial pivoting.
fn solve<const N: usize>(mut a: [[f64; N]; N], mut b: [f64; N]) -> Option<[f64; N]> {
    let scale = (0..N).map(|k| a[k][k].abs()).fold(0.0, f64::max);
    if scale <= 0.0 || !scale.is_finite() {
        return None;
    }
    for c in 0..N {
        let p = (c..N).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))?;
        if a[p][c].abs() <= 1e-12 * scale {
            return None;
        }
        a.swap(c, p);
        b.swap(c, p);
        for r in c + 1..N {
            let f = a[r][c] / a[c][c];
            let (top, rest) = a.split_at_mut(r);
            for (x, &y) in rest[0][c..].iter_mut().zip(&top[c][c..]) {
                *x -= f * y;
            }
            b[r] -= f * b[c];
        }
    }
    let mut x = [0.0; N];
    for c in (0..N).rev() {
        let s: f64 = (c + 1..N).map(|k| a[c][k] * x[k]).sum();
        x[c] = (b[c] - s) / a[c][c];
    }
    Some(x)
}

#[cfg(test)]
mod tests {
    use super::solve;

    #[test]
    fn solves_small_system() {
        let a = [[4.0, 1.0, 0.0], [1.0, 3.0, 1.0], [0.0, 1.0, 2.0]];
        let x = solve(a, [1.0, 2.0, 3.0]).unwrap();
        for r in 0..3 {
            let s: f64 = (0..3).map(|k| a[r][k] * x[k]).sum();
            assert!((s - [1.0, 2.0, 3.0][r]).abs() < 1e-12);
        }
        assert!(solve([[1.0, 2.0], [2.0, 4.0]], [1.0, 1.0]).is_none());
    }
}
