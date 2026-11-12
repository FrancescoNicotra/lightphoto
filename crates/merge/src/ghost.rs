//! Ghost removal: where something moved between the shots, each moving
//! region takes a single source shot.
//!
//! ```text
//! half-size shots (aligned) ─ noise model σ²(v) = a·v + b, fitted on the shots
//!   ─ z = |Xᵢ − X_ref| / √(σᵢ² + σ_ref²)  (or vs the valid shot nearest the
//!       reference in exposure where the reference is clipped or dark)
//!   ─ z > τ ─ opening, closing, dilation ─ drop small regions ─ components
//!   ─ one source per region (the reference if mostly valid there, else the
//!       best exposed valid shot) ─ feathered masks mᵢ
//! full resolution: out = X · (1 − Σ mᵢ) + Σ mᵢ · vᵢ/eᵢ
//! ```

use align::Transform;
use rayon::prelude::*;

use crate::hdr::Shot;
use crate::weights::{highlight_weight, max3, FrameWeight};

/// Ghost removal strength.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Deghost {
    #[default]
    None,
    Low,
    Medium,
    High,
}

impl std::str::FromStr for Deghost {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "none" | "off" => Deghost::None,
            "low" => Deghost::Low,
            "medium" => Deghost::Medium,
            "high" => Deghost::High,
            _ => return Err(format!("unknown ghost removal level {s:?} (none, low, medium, high)")),
        })
    }
}

/// Parameters of a level. Sizes are fractions of the long side (areas: of
/// the image area), so they mean the same at any resolution.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Level {
    /// z-score above which a pixel is inconsistent.
    pub tau: f32,
    /// Radius of the opening and closing.
    pub open: f32,
    pub dilate: f32,
    /// Regions smaller than this are dropped.
    pub min_area: f32,
    /// Width of the transition outside a region.
    pub feather: f32,
}

impl Deghost {
    pub fn level(self) -> Option<Level> {
        let l = |tau, open, dilate, min_area, feather| Level { tau, open, dilate, min_area, feather };
        match self {
            Deghost::None => None,
            Deghost::Low => Some(l(6.0, 0.002, 0.003, 0.0005, 0.005)),
            Deghost::Medium => Some(l(4.0, 0.002, 0.006, 0.0002, 0.010)),
            Deghost::High => Some(l(2.5, 0.001, 0.012, 0.0001, 0.015)),
        }
    }
}

/// Below this value (luminance, 0..1) a shot is "dark": the reference is
/// replaced by another comparison shot there.
const DARK: f32 = 0.002;

/// Noise of a half-size shot's luminance (mean of the channels) at raw
/// level `v`: `σ² = a·v + b` (photon + read noise).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoiseModel {
    pub a: f64,
    pub b: f64,
}

impl NoiseModel {
    /// Variance of `Y/e` for a shot of exposure `e` at radiance `x`.
    #[inline]
    fn variance(&self, x: f32, e: f32) -> f32 {
        ((self.a * (x * e).max(0.0) as f64 + self.b) / (e as f64 * e as f64)) as f32
    }
}

#[inline]
fn luma(p: [f32; 3]) -> f32 {
    (p[0] + p[1] + p[2]) / 3.0
}

#[inline]
fn valid(s: &Shot, i: usize) -> bool {
    s.covers(i) && highlight_weight(max3(s.data.pixels[i]), s.clipped[i] != 0) >= 0.5
}

/// Fits the noise model on pairs of shots adjacent in exposure (`order`
/// sorts the shots by exposure): in bins of radiance, the robust variance
/// (1.4826 · MAD)² of the difference of the normalised luminances, then a
/// least-squares fit of `a`, `b` relative to each bin's variance.
pub fn estimate_noise(shots: &[Shot], exposures: &[f32], order: &[usize]) -> NoiseModel {
    const BINS: usize = 40;
    // log₂ radiance from 2⁻¹⁶ to 2⁸.
    let bin = |x: f32| ((x.max(1e-9).log2() + 16.0) / 24.0 * BINS as f32).floor();
    let mut rows: Vec<(f64, f64, f64)> = Vec::new(); // (u_a, u_b, variance)
    for pair in order.windows(2) {
        let (lo, hi) = (pair[0], pair[1]);
        let (el, eh) = (exposures[lo], exposures[hi]);
        let (sl, sh) = (&shots[lo], &shots[hi]);
        let mut bins: Vec<Vec<f32>> = vec![Vec::new(); BINS];
        for i in (0..sl.data.pixels.len()).step_by(2) {
            if !valid(sl, i) || !valid(sh, i) {
                continue;
            }
            let x = luma(sh.data.pixels[i]) / eh;
            let b = bin(x);
            if (0.0..BINS as f32).contains(&b) {
                bins[b as usize].push(x - luma(sl.data.pixels[i]) / el);
            }
        }
        for (k, mut d) in bins.into_iter().enumerate() {
            if d.len() < 200 {
                continue;
            }
            let med = median(&mut d);
            let mut dev: Vec<f32> = d.iter().map(|v| (v - med).abs()).collect();
            let s = 1.4826 * median(&mut dev) as f64;
            let x = ((k as f64 + 0.5) / BINS as f64 * 24.0 - 16.0).exp2();
            let (el, eh) = (el as f64, eh as f64);
            rows.push((x * (1.0 / el + 1.0 / eh), 1.0 / (el * el) + 1.0 / (eh * eh), s * s));
        }
    }
    fit(&rows)
}

/// Weighted least squares of `var = a·u_a + b·u_b`, each row weighted by
/// 1/var² (relative errors), with `a`, `b` ≥ 0.
fn fit(rows: &[(f64, f64, f64)]) -> NoiseModel {
    let fallback = NoiseModel { a: 1.0 / 30000.0, b: 1e-8 };
    let rows: Vec<_> = rows.iter().filter(|r| r.2 > 0.0).collect();
    if rows.len() < 2 {
        return fallback;
    }
    let (mut saa, mut sab, mut sbb, mut sav, mut sbv) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for &&(ua, ub, v) in &rows {
        let w = 1.0 / (v * v);
        saa += w * ua * ua;
        sab += w * ua * ub;
        sbb += w * ub * ub;
        sav += w * ua * v;
        sbv += w * ub * v;
    }
    let det = saa * sbb - sab * sab;
    let (mut a, mut b) = if det.abs() > 1e-300 { ((sav * sbb - sbv * sab) / det, (saa * sbv - sab * sav) / det) } else { (-1.0, -1.0) };
    // A negative term: refit with that term alone at zero.
    if a <= 0.0 {
        a = 0.0;
        b = sbv / sbb;
    } else if b <= 0.0 {
        b = 0.0;
        a = sav / saa;
    }
    if !(a.is_finite() && b.is_finite()) || (a <= 0.0 && b <= 0.0) {
        return fallback;
    }
    NoiseModel { a: a.max(0.0), b: b.max(1e-12) }
}

fn median(v: &mut [f32]) -> f32 {
    let mid = v.len() / 2;
    let (_, m, _) = v.select_nth_unstable_by(mid, f32::total_cmp);
    *m
}

/// The largest z-score of each pixel over the shots (0 where nothing can be
/// compared).
///
/// A difference is measured against the *range* of the other shot over the
/// pixel and its 4 neighbours, in both directions, so that sub-pixel
/// misregistration on sharp edges (warp interpolation, Bayer sampling) is not
/// taken for motion: `d = min(dist(Xᵢ, range(X_c)), dist(X_c, range(Xᵢ)))`.
pub fn z_scores(shots: &[Shot], exposures: &[f32], reference: usize, noise: &NoiseModel) -> Vec<f32> {
    let n = shots[0].data.pixels.len();
    let (w, h) = (shots[0].data.width, shots[0].data.height);
    // Comparison candidates by distance in exposure to the reference.
    let mut by_distance: Vec<usize> = (0..shots.len()).collect();
    let er = exposures[reference];
    by_distance.sort_by(|&a, &b| (exposures[a] / er).log2().abs().total_cmp(&(exposures[b] / er).log2().abs()));
    // Normalised luminance, NaN where the shot is not valid.
    let x: Vec<Vec<f32>> = shots
        .par_iter()
        .zip(exposures)
        .map(|(s, &e)| (0..n).map(|p| if valid(s, p) { luma(s.data.pixels[p]) / e } else { f32::NAN }).collect())
        .collect();
    let range = |k: usize, p: usize| -> (f32, f32) {
        let (px, py) = (p % w, p / w);
        let mut lo = x[k][p];
        let mut hi = lo;
        let mut take = |q: usize| {
            let v = x[k][q];
            if !v.is_nan() {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        };
        if px > 0 {
            take(p - 1);
        }
        if px + 1 < w {
            take(p + 1);
        }
        if py > 0 {
            take(p - w);
        }
        if py + 1 < h {
            take(p + w);
        }
        (lo, hi)
    };
    let dist = |v: f32, (lo, hi): (f32, f32)| (v - hi).max(lo - v).max(0.0);
    (0..n)
        .into_par_iter()
        .map(|p| {
            let ok = |k: usize| !x[k][p].is_nan() && x[k][p] * exposures[k] >= DARK;
            let mut z = 0.0f32;
            for i in 0..shots.len() {
                if i == reference && ok(reference) || x[i][p].is_nan() {
                    continue;
                }
                let Some(c) = by_distance.iter().copied().find(|&k| k != i && ok(k)) else { continue };
                let (ei, ec) = (exposures[i], exposures[c]);
                let (xi, xc) = (x[i][p], x[c][p]);
                let d = dist(xi, range(c, p)).min(dist(xc, range(i, p)));
                if d == 0.0 {
                    continue;
                }
                // The level from the less noisy (brighter) of the two.
                let level = if ec >= ei { xc } else { xi };
                let var = noise.variance(level, ei) + noise.variance(level, ec);
                z = z.max(d / var.max(1e-20).sqrt());
            }
            z
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Binary morphology (square structuring elements, separable running counts)

fn pass(mask: &[bool], w: usize, h: usize, r: usize, horizontal: bool, all: bool) -> Vec<bool> {
    let (len, lines) = if horizontal { (w, h) } else { (h, w) };
    let idx = move |line: usize, k: usize| if horizontal { line * w + k } else { k * w + line };
    let mut out = vec![false; w * h];
    let cols: Vec<Vec<bool>> = (0..lines)
        .into_par_iter()
        .map(|line| {
            let mut prefix = vec![0u32; len + 1];
            for k in 0..len {
                prefix[k + 1] = prefix[k] + u32::from(mask[idx(line, k)]);
            }
            (0..len)
                .map(|k| {
                    let (a, b) = (k.saturating_sub(r), (k + r + 1).min(len));
                    let count = prefix[b] - prefix[a];
                    // Outside the image counts as neither set nor unset.
                    if all {
                        count as usize == b - a
                    } else {
                        count > 0
                    }
                })
                .collect()
        })
        .collect();
    for (line, v) in cols.into_iter().enumerate() {
        for (k, b) in v.into_iter().enumerate() {
            out[idx(line, k)] = b;
        }
    }
    out
}

pub fn dilate(mask: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    if r == 0 {
        return mask.to_vec();
    }
    pass(&pass(mask, w, h, r, true, false), w, h, r, false, false)
}

pub fn erode(mask: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    if r == 0 {
        return mask.to_vec();
    }
    pass(&pass(mask, w, h, r, true, true), w, h, r, false, true)
}

/// Connected components (8-connectivity): labels (0 = background, 1..) and
/// the area of each label.
pub fn components(mask: &[bool], w: usize, h: usize) -> (Vec<u32>, Vec<usize>) {
    let mut labels = vec![0u32; w * h];
    let mut areas = vec![0usize];
    let mut stack = Vec::new();
    for start in 0..w * h {
        if !mask[start] || labels[start] != 0 {
            continue;
        }
        let label = areas.len() as u32;
        let mut area = 0;
        labels[start] = label;
        stack.push(start);
        while let Some(i) = stack.pop() {
            area += 1;
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if mask[j] && labels[j] == 0 {
                        labels[j] = label;
                        stack.push(j);
                    }
                }
            }
        }
        areas.push(area);
    }
    (labels, areas)
}

/// Approximate Gaussian blur (three box passes per axis) of a plane.
pub(crate) fn blur(v: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    // Box radius giving the variance σ² over three passes: 3·r(r+1)/3 = σ².
    let r = (((4.0 * sigma * sigma + 1.0).sqrt() - 1.0) / 2.0).round() as usize;
    if r == 0 {
        return v.to_vec();
    }
    let box_pass = |src: &[f32], horizontal: bool| -> Vec<f32> {
        let (len, lines) = if horizontal { (w, h) } else { (h, w) };
        let idx = |line: usize, k: usize| if horizontal { line * w + k } else { k * w + line };
        let res: Vec<Vec<f32>> = (0..lines)
            .into_par_iter()
            .map(|line| {
                let mut prefix = vec![0.0f64; len + 1];
                for k in 0..len {
                    prefix[k + 1] = prefix[k] + src[idx(line, k)] as f64;
                }
                (0..len)
                    .map(|k| {
                        let (a, b) = (k.saturating_sub(r), (k + r + 1).min(len));
                        ((prefix[b] - prefix[a]) / (b - a) as f64) as f32
                    })
                    .collect()
            })
            .collect();
        let mut out = vec![0.0; w * h];
        for (line, row) in res.into_iter().enumerate() {
            for (k, x) in row.into_iter().enumerate() {
                out[idx(line, k)] = x;
            }
        }
        out
    };
    let mut out = v.to_vec();
    for _ in 0..3 {
        out = box_pass(&box_pass(&out, true), false);
    }
    out
}

// ---------------------------------------------------------------------------

/// A moving region and the shot it is taken from.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    /// Index of the source shot.
    pub source: usize,
    /// Area in half-size pixels.
    pub area: usize,
}

/// What ghost detection found, at half size.
#[derive(Clone, Debug)]
pub struct Ghosts {
    pub level: Deghost,
    pub noise: NoiseModel,
    pub width: usize,
    pub height: usize,
    /// The motion mask after morphology and the area filter (before
    /// feathering).
    pub motion: Vec<bool>,
    pub regions: Vec<Region>,
    /// Feathered weight of the single sources, `Σ mᵢ` (0..1): the overlay.
    pub overlay: Vec<f32>,
    /// Per shot: where it is the single source.
    pub masks: Vec<Option<SourceMask>>,
}

impl Ghosts {
    /// Share of the image in moving regions.
    pub fn motion_area(&self) -> f64 {
        self.motion.iter().filter(|&&m| m).count() as f64 / self.motion.len().max(1) as f64
    }
}

/// Detects moving regions on the aligned half-size shots and picks their
/// sources. `full_to_half` maps full-resolution pixel coordinates to
/// half-size ones (for [`SourceMask::at_full`]).
pub fn detect(shots: &[Shot], weights: &[FrameWeight], reference: usize, order: &[usize], deghost: Deghost, full_to_half: Transform) -> Option<Ghosts> {
    let level = deghost.level()?;
    let (w, h) = (shots[0].data.width, shots[0].data.height);
    let exposures: Vec<f32> = weights.iter().map(|f| f.exposure).collect();
    let noise = estimate_noise(shots, &exposures, order);
    let z = z_scores(shots, &exposures, reference, &noise);

    let long = w.max(h) as f32;
    let px = |f: f32| (f * long).round().max(1.0) as usize;
    let mut m: Vec<bool> = z.iter().map(|&v| v > level.tau).collect();
    let r = px(level.open);
    m = dilate(&erode(&m, w, h, r), w, h, r); // opening: drops specks
    m = erode(&dilate(&m, w, h, r), w, h, r); // closing: fills holes
    m = dilate(&m, w, h, px(level.dilate));
    let (labels, areas) = components(&m, w, h);
    let min_area = (level.min_area * (w * h) as f32).ceil() as usize;

    // Per region: mean h of every shot, and of e·h.
    let n = shots.len();
    let mut sum_h = vec![vec![0.0f64; n]; areas.len()];
    for (p, &l) in labels.iter().enumerate() {
        if l == 0 || areas[l as usize] < min_area {
            continue;
        }
        for (k, s) in shots.iter().enumerate() {
            if s.covers(p) {
                sum_h[l as usize][k] += highlight_weight(max3(s.data.pixels[p]), s.clipped[p] != 0) as f64;
            }
        }
    }
    let source: Vec<Option<usize>> = (0..areas.len())
        .map(|l| {
            if l == 0 || areas[l] < min_area {
                return None;
            }
            let mean = |k: usize| sum_h[l][k] / areas[l] as f64;
            if mean(reference) >= 0.5 {
                return Some(reference);
            }
            (0..n).max_by(|&a, &b| (exposures[a] as f64 * mean(a)).total_cmp(&(exposures[b] as f64 * mean(b))))
        })
        .collect();
    let motion: Vec<bool> = labels.iter().map(|&l| source[l as usize].is_some()).collect();
    let regions: Vec<Region> = (0..areas.len()).filter_map(|l| source[l].map(|s| Region { source: s, area: areas[l] })).collect();

    // Feathered source masks: grown by half the feather, blurred by a quarter
    // of it, so a region is fully its source and fades out around it.
    let feather = level.feather * long;
    let grow = (feather / 2.0).round() as usize;
    let mut planes: Vec<Option<Vec<f32>>> = (0..n)
        .map(|k| {
            let ind: Vec<bool> = labels.iter().map(|&l| source[l as usize] == Some(k)).collect();
            ind.iter().any(|&b| b).then(|| {
                let grown = dilate(&ind, w, h, grow);
                blur(&grown.iter().map(|&b| f32::from(u8::from(b))).collect::<Vec<_>>(), w, h, feather / 4.0)
            })
        })
        .collect();
    // Overlapping feathers of different sources: normalise to Σ ≤ 1.
    let mut overlay = vec![0.0f32; w * h];
    for p in planes.iter().flatten() {
        overlay.iter_mut().zip(p).for_each(|(o, v)| *o += v);
    }
    for p in planes.iter_mut().flatten() {
        p.iter_mut().zip(&overlay).for_each(|(v, &s)| {
            if s > 1.0 {
                *v /= s;
            }
        });
    }
    overlay.iter_mut().for_each(|o| *o = o.min(1.0));
    let masks = planes.into_iter().map(|p| p.map(|data| SourceMask { width: w, height: h, data, full_to_half })).collect();
    Some(Ghosts { level: deghost, noise, width: w, height: h, motion, regions, overlay, masks })
}

/// Where one shot is the single source, as a feathered weight `0..=1` at
/// half size, sampled bilinearly at full resolution.
#[derive(Clone, Debug)]
pub struct SourceMask {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    /// Full-resolution coordinates → half-size coordinates (pixel centres at +0.5).
    pub full_to_half: Transform,
}

impl SourceMask {
    #[inline]
    pub fn at_half(&self, x: usize, y: usize) -> f32 {
        self.data[y * self.width + x]
    }

    /// `[x0, y0, x1, y1]` of the pixels with a weight, at half size.
    pub fn bbox_half(&self) -> Option<[usize; 4]> {
        let mut b: Option<[usize; 4]> = None;
        for (i, &v) in self.data.iter().enumerate() {
            if v > 0.0 {
                let (x, y) = (i % self.width, i / self.width);
                b = Some(match b {
                    None => [x, y, x + 1, y + 1],
                    Some(r) => [r[0].min(x), r[1].min(y), r[2].max(x + 1), r[3].max(y + 1)],
                });
            }
        }
        b
    }

    /// The same at full size, with a margin for the bilinear sampling
    /// (clamped to the image by the accumulator).
    pub fn bbox_full(&self) -> Option<[usize; 4]> {
        let [x0, y0, x1, y1] = self.bbox_half()?;
        let m = &self.full_to_half.0;
        // Half-size pixel h covers full coordinates (h - o) / s … (h + 1 - o) / s.
        let to_full = |h: f64, s: f64, o: f64| (h - o) / s;
        let fx0 = to_full(x0 as f64 - 1.0, m[0][0], m[0][2]).floor().max(0.0) as usize;
        let fy0 = to_full(y0 as f64 - 1.0, m[1][1], m[1][2]).floor().max(0.0) as usize;
        let fx1 = to_full(x1 as f64 + 1.0, m[0][0], m[0][2]).ceil().max(0.0) as usize;
        let fy1 = to_full(y1 as f64 + 1.0, m[1][1], m[1][2]).ceil().max(0.0) as usize;
        Some([fx0, fy0, fx1, fy1])
    }

    #[inline]
    pub fn at_full(&self, x: usize, y: usize) -> f32 {
        let m = &self.full_to_half.0;
        let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
        // An axis-aligned scale and offset.
        let u = (m[0][0] * fx + m[0][2] - 0.5).clamp(0.0, (self.width - 1) as f64);
        let v = (m[1][1] * fy + m[1][2] - 0.5).clamp(0.0, (self.height - 1) as f64);
        let (x0, y0) = (u.floor() as usize, v.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (tx, ty) = ((u - x0 as f64) as f32, (v - y0 as f64) as f32);
        let d = |x: usize, y: usize| self.data[y * self.width + x];
        let top = d(x0, y0) + tx * (d(x1, y0) - d(x0, y0));
        let bot = d(x0, y1) + tx * (d(x1, y1) - d(x0, y1));
        top + ty * (bot - top)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn morphology() {
        let (w, h) = (9, 7);
        let mut m = vec![false; w * h];
        m[3 * w + 4] = true;
        let d = dilate(&m, w, h, 1);
        assert_eq!(d.iter().filter(|&&b| b).count(), 9);
        assert!(d[2 * w + 3] && d[4 * w + 5] && !d[w + 4]);
        assert_eq!(erode(&d, w, h, 1), m);
        // Opening removes a speck, keeps a 3×3 block.
        let opened = dilate(&erode(&m, w, h, 1), w, h, 1);
        assert!(opened.iter().all(|&b| !b));
        assert_eq!(dilate(&erode(&d, w, h, 1), w, h, 1), d);
    }

    #[test]
    fn components_and_areas() {
        let (w, h) = (6, 4);
        #[rustfmt::skip]
        let m: Vec<bool> = [
            1, 1, 0, 0, 0, 1,
            0, 1, 0, 0, 1, 0,
            0, 0, 0, 0, 0, 0,
            1, 0, 0, 0, 0, 0,
        ].iter().map(|&v| v == 1).collect();
        let (labels, areas) = components(&m, w, h);
        assert_eq!(areas, vec![0, 3, 2, 1]);
        assert_eq!(labels[w + 4], labels[5]); // diagonal neighbours connect
    }

    #[test]
    fn noise_fit_recovers_the_model() {
        let truth = NoiseModel { a: 2e-5, b: 4e-8 };
        let rows: Vec<_> = (0..20)
            .map(|k| {
                let x = 2f64.powf(-10.0 + k as f64 * 0.5);
                let (el, eh) = (0.25, 1.0);
                let ua = x * (1.0 / el + 1.0 / eh);
                let ub = 1.0 / (el * el) + 1.0 / (eh * eh);
                (ua, ub, truth.a * ua + truth.b * ub)
            })
            .collect();
        let f = fit(&rows);
        assert!((f.a / truth.a - 1.0).abs() < 1e-6 && (f.b / truth.b - 1.0).abs() < 1e-6, "{f:?}");
    }

    #[test]
    fn blur_keeps_the_mean() {
        let (w, h) = (64, 48);
        let mut v = vec![0.0f32; w * h];
        v[24 * w + 32] = 1.0;
        let b = blur(&v, w, h, 3.0);
        assert!((b.iter().sum::<f32>() - 1.0).abs() < 1e-3);
        assert!(b[24 * w + 32] < 0.05 && b[24 * w + 32] == b.iter().copied().fold(0.0, f32::max));
    }
}
