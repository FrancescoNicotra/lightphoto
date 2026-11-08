//! Accumulating the radiance estimate, at half size (analysis, previews) and
//! at full resolution, one shot at a time.
//!
//! Full resolution keeps only the accumulators, `Σ w·X` (3 × f32) and `Σ w`
//! (f32), plus the shot being added: it is decoded, warped into the
//! reference frame in bands of rows (so the warped copy never exists whole)
//! and dropped.

use std::time::Instant;

use align::{warp_plane, warp_rgb, Interp, Transform};
use raw::Image;
use rayon::prelude::*;

use crate::bracket::Source;
use crate::weights::FrameWeight;
use crate::{Control, MergeError, Stage, Timings};

/// Rows warped at a time in the full-resolution pass.
const BAND: usize = 256;

/// A shot at half size, warped into the reference frame.
#[derive(Clone, Debug)]
pub struct Shot {
    pub data: Image,
    /// 1 where clipped (the warped mask: maximum of the 4 nearest pixels).
    pub clipped: Vec<u8>,
    /// `None` = covers the whole frame (reference, or not aligned).
    pub coverage: Option<Vec<bool>>,
}

impl Shot {
    #[inline]
    pub fn covers(&self, i: usize) -> bool {
        self.coverage.as_ref().is_none_or(|c| c[i])
    }

    /// The shot resampled through `t` (reference → shot coordinates).
    pub fn warped(&self, t: &Transform) -> Shot {
        let (w, h) = (self.data.width, self.data.height);
        let (rgb, coverage) = warp_rgb(&self.data.pixels, w, h, t, w, h);
        let mask: Vec<f32> = self.clipped.iter().map(|&c| f32::from(c)).collect();
        let (m, _) = warp_plane(&mask, w, h, t, w, h, Interp::Max);
        Shot {
            data: Image { width: w, height: h, pixels: rgb },
            clipped: m.iter().map(|&v| u8::from(v > 0.0)).collect(),
            coverage: Some(coverage),
        }
    }
}

/// How one shot enters the full-resolution merge.
#[derive(Clone, Debug)]
pub struct FramePlan {
    pub source: Source,
    pub weight: FrameWeight,
    /// Reference → shot at full resolution; `None` = identity.
    pub transform: Option<Transform>,
    /// Ghost removal: where this shot is the only source (F4).
    pub ghost: Option<crate::ghost::SourceMask>,
}

/// Running sums for `X = Σ w·v/e / Σ w`, blended with the ghost sources:
/// `out = X · (1 − M) + G`, `G = Σ mᵢ · vᵢ/eᵢ`, `M = Σ mᵢ`.
pub struct Accumulator {
    pub width: usize,
    pub height: usize,
    num: Vec<[f32; 3]>,
    den: Vec<f32>,
    ghost: Option<(Vec<[f32; 3]>, Vec<f32>)>,
}

impl Accumulator {
    pub fn new(width: usize, height: usize, ghosts: bool) -> Self {
        let n = width * height;
        Accumulator {
            width,
            height,
            num: vec![[0.0; 3]; n],
            den: vec![0.0; n],
            ghost: ghosts.then(|| (vec![[0.0; 3]; n], vec![0.0; n])),
        }
    }

    /// Adds rows `y0..y0 + rows` of a shot. `sample(i)` gives the value and
    /// clipping flag of pixel `i` of the band, `None` where the shot does not
    /// cover it; `mask(x, y)` its ghost-source weight at full coordinates.
    pub fn add_band(
        &mut self,
        y0: usize,
        rows: usize,
        fw: &FrameWeight,
        sample: impl Fn(usize) -> Option<([f32; 3], bool)> + Sync,
        mask: Option<&(dyn Fn(usize, usize) -> f32 + Sync)>,
    ) {
        let w = self.width;
        let range = y0 * w..(y0 + rows) * w;
        let num = &mut self.num[range.clone()];
        let den = &mut self.den[range.clone()];
        let ghost = self.ghost.as_mut().map(|(g, m)| (&mut g[range.clone()], &mut m[range.clone()]));
        let add_row = |y: usize, num: &mut [[f32; 3]], den: &mut [f32], mut ghost: Option<(&mut [[f32; 3]], &mut [f32])>| {
            for x in 0..w {
                let Some((v, clipped)) = sample(y * w + x) else { continue };
                let (wt, est) = fw.sample(v, clipped);
                if wt > 0.0 {
                    for c in 0..3 {
                        num[x][c] += wt * est[c];
                    }
                    den[x] += wt;
                }
                if let (Some(f), Some((g, m))) = (mask, ghost.as_mut()) {
                    let k = f(x, y0 + y);
                    if k > 0.0 {
                        let val = fw.value(v, clipped);
                        for c in 0..3 {
                            g[x][c] += k * val[c];
                        }
                        m[x] += k;
                    }
                }
            }
        };
        match ghost {
            None => num.par_chunks_mut(w).zip(den.par_chunks_mut(w)).enumerate().for_each(|(y, (n, d))| add_row(y, n, d, None)),
            Some((g, m)) => num
                .par_chunks_mut(w)
                .zip(den.par_chunks_mut(w))
                .zip(g.par_chunks_mut(w).zip(m.par_chunks_mut(w)))
                .enumerate()
                .for_each(|(y, ((n, d), (g, m)))| add_row(y, n, d, Some((g, m)))),
        }
    }

    /// The merged image, in units of the reference exposure.
    pub fn finish(self) -> Image {
        let Accumulator { width, height, mut num, den, ghost } = self;
        let blend = ghost.as_ref();
        num.par_iter_mut().zip(den.par_iter()).enumerate().for_each(|(i, (n, &d))| {
            let x = if d > 0.0 { n.map(|v| v / d) } else { [0.0; 3] };
            *n = match blend {
                None => x,
                Some((g, m)) => {
                    let k = m[i].min(1.0);
                    std::array::from_fn(|c| x[c] * (1.0 - k) + g[i][c])
                }
            };
        });
        Image { width, height, pixels: num }
    }
}

/// Merges the half-size shots (already in the reference frame).
pub fn merge_shots(shots: &[Shot], plans: &[FramePlan], ghosts: bool) -> Image {
    let (w, h) = (shots[0].data.width, shots[0].data.height);
    let mut acc = Accumulator::new(w, h, ghosts);
    for (s, p) in shots.iter().zip(plans) {
        let mask = p.ghost.as_ref().filter(|_| ghosts).map(|g| move |x: usize, y: usize| g.at_half(x, y));
        acc.add_band(
            0,
            h,
            &p.weight,
            |i| s.covers(i).then(|| (s.data.pixels[i], s.clipped[i] != 0)),
            mask.as_ref().map(|f| f as &(dyn Fn(usize, usize) -> f32 + Sync)),
        );
    }
    acc.finish()
}

/// The full-resolution merge: decodes, warps and accumulates one shot at a
/// time, the reference (`plans[reference]`) first.
pub fn merge_full(plans: &[FramePlan], reference: usize, ctl: &Control, timings: &mut Timings) -> Result<Image, MergeError> {
    let ghosts = plans.iter().any(|p| p.ghost.is_some());
    let order: Vec<usize> = std::iter::once(reference).chain((0..plans.len()).filter(|&i| i != reference)).collect();
    let mut acc: Option<Accumulator> = None;
    for (k, &i) in order.iter().enumerate() {
        ctl.check()?;
        let plan = &plans[i];
        let t = Instant::now();
        let cap = plan.source.load(false)?;
        timings.decode_full += t.elapsed();
        let t = Instant::now();
        let (w, h) = (cap.data.width, cap.data.height);
        let acc = acc.get_or_insert_with(|| Accumulator::new(w, h, ghosts));
        if (acc.width, acc.height) != (w, h) {
            return Err(MergeError::DifferentSensors(vec![format!(
                "{}: {w}×{h} instead of {}×{}",
                plan.source.name(),
                acc.width,
                acc.height
            )]));
        }
        let clipped = |i: usize| cap.clipped.get(i).is_some_and(|&c| c != 0);
        let ghost_fn = plan.ghost.as_ref().map(|g| move |x: usize, y: usize| g.at_full(x, y));
        let mask = ghost_fn.as_ref().map(|f| f as &(dyn Fn(usize, usize) -> f32 + Sync));
        match &plan.transform {
            None => acc.add_band(0, h, &plan.weight, |i| Some((cap.data.pixels[i], clipped(i))), mask),
            Some(tr) => {
                let mask_src: Vec<f32> = if cap.clipped.is_empty() {
                    vec![0.0; w * h]
                } else {
                    cap.clipped.par_iter().map(|&c| f32::from(c)).collect()
                };
                for y0 in (0..h).step_by(BAND) {
                    ctl.check()?;
                    let rows = BAND.min(h - y0);
                    let tb = tr.compose(&Transform::translation(0.0, y0 as f64));
                    let (rgb, cov) = warp_rgb(&cap.data.pixels, w, h, &tb, w, rows);
                    let (m, _) = warp_plane(&mask_src, w, h, &tb, w, rows, Interp::Max);
                    acc.add_band(y0, rows, &plan.weight, |i| cov[i].then(|| (rgb[i], m[i] > 0.0)), mask);
                }
            }
        }
        timings.accumulate += t.elapsed();
        ctl.report(Stage::Merge, (k + 1) as f32 / order.len() as f32);
    }
    Ok(acc.expect("at least two shots").finish())
}
