//! Dense image alignment for exposure brackets, focus stacks and panoramas.
//!
//! ```text
//! reference, moving (Plane + weight)
//!   ─Gaussian pyramid ×2 down to ~128 px─→ levels
//!   ─coarsest level: exhaustive NCC translation search (±12%)─→ T₀
//!   ─ECC, inverse compositional, coarse → fine:
//!        translation → Euclidean → model (two finest levels)─→ T
//!   ─quality checks (ρ, convergence, corner deformation), fallback to simpler models
//! ```
//!
//! [`Transform`] maps reference pixel coordinates to moving coordinates with
//! pixel centres at `+0.5`; [`warp_rgb`] / [`warp_plane`] resample the moving
//! image into the reference frame. Depends only on `rayon`, so other features
//! can reuse it without pulling in RAW decoding. See `docs/align.md`.

mod coarse;
mod ecc;
mod plane;
mod transform;
mod warp;

use rayon::prelude::*;

pub use plane::Plane;
pub use transform::Transform;
pub use warp::{warp_plane, warp_rgb, Interp};

/// Motion model, from the most constrained to the most general.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Model {
    /// 2 parameters: shift.
    Translation,
    /// 3 parameters: shift + rotation.
    Euclidean,
    /// 4 parameters: shift + rotation + uniform scale (e.g. focus breathing).
    Similarity,
    /// 8 parameters: full projective transform (hand-held shots).
    Homography,
}

impl Model {
    /// The models tried, in order, when the previous one fails the checks.
    fn fallbacks(self) -> &'static [Model] {
        match self {
            Model::Homography => &[Model::Homography, Model::Similarity, Model::Translation],
            Model::Similarity => &[Model::Similarity, Model::Translation],
            Model::Euclidean => &[Model::Euclidean, Model::Translation],
            Model::Translation => &[Model::Translation],
        }
    }

    /// Model used on pyramid level `level` (0 = finest): translation on the
    /// coarse levels, Euclidean on the third finest, `self` on the two finest.
    fn at_level(self, level: usize) -> Model {
        let schedule = match level {
            0 | 1 => Model::Homography,
            2 => Model::Euclidean,
            _ => Model::Translation,
        };
        self.min(schedule)
    }
}

#[derive(Clone, Debug)]
pub struct AlignParams {
    /// The most general model to estimate.
    pub model: Model,
    /// Coarse translation search range, as a fraction of the long side.
    pub coarse_search: f32,
    /// ECC iterations per pyramid level.
    pub max_iterations: u32,
    /// ECC stops when an update moves the image corners less than this many
    /// pixels (of the current level).
    pub tolerance: f64,
    /// Below this final correlation the result is rejected (next model).
    pub min_correlation: f32,
    /// Results that move a corner, relative to the image centre, by more than
    /// this fraction of the long side are rejected (degenerate rotation, scale
    /// or perspective). The shift itself is bounded by `coarse_search`.
    pub max_corner_shift: f32,
    /// Levels with more pixels than this are estimated on the most textured
    /// 128×128 tiles only, spread over a 4×4 grid, to bound the cost.
    pub max_pixels: usize,
}

impl Default for AlignParams {
    fn default() -> Self {
        AlignParams {
            model: Model::Homography,
            coarse_search: 0.12,
            max_iterations: 30,
            tolerance: 1e-3,
            min_correlation: 0.85,
            max_corner_shift: 0.10,
            max_pixels: 1 << 20,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Alignment {
    /// Reference → moving, in the input planes' pixel coordinates.
    pub transform: Transform,
    /// Final (weighted) correlation coefficient on the finest level, in -1..=1.
    pub correlation: f32,
    /// ECC converged on the finest level *and* the result passed the quality
    /// checks. When `false`, `transform` is the best attempt and should not be
    /// trusted (callers usually fall back to no alignment).
    pub converged: bool,
    /// The model actually used, after any fallback.
    pub model: Model,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AlignError {
    /// `data` or `weight` does not have `width × height` values.
    InvalidPlane,
    /// The two planes have different sizes.
    SizeMismatch,
    /// Too small to align (short side under 16 pixels).
    TooSmall,
    /// No overlap with enough weight and texture to estimate anything.
    NoOverlap,
}

impl std::fmt::Display for AlignError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(match self {
            AlignError::InvalidPlane => "plane data does not match its size",
            AlignError::SizeMismatch => "the images have different sizes",
            AlignError::TooSmall => "the images are too small to align",
            AlignError::NoOverlap => "the images have no usable overlap",
        })
    }
}

impl std::error::Error for AlignError {}

/// Estimates the transform from `reference` to `moving` (same size). Weights,
/// when present, mask out unreliable pixels in either image.
pub fn align(reference: &Plane, moving: &Plane, p: &AlignParams) -> Result<Alignment, AlignError> {
    if !reference.is_valid() || !moving.is_valid() {
        return Err(AlignError::InvalidPlane);
    }
    if (reference.width, reference.height) != (moving.width, moving.height) {
        return Err(AlignError::SizeMismatch);
    }
    if reference.width.min(reference.height) < 16 {
        return Err(AlignError::TooSmall);
    }

    let (rp, mp) = rayon::join(|| plane::pyramid(reference), || plane::pyramid(moving));
    let grads: Vec<(Vec<f32>, Vec<f32>)> = rp.par_iter().map(plane::gradients).collect();
    let spans: Vec<Vec<ecc::Span>> = rp
        .iter()
        .zip(&grads)
        .map(|(l, (gx, gy))| select_spans(l, gx, gy, p.max_pixels))
        .collect();

    let top = rp.len() - 1;
    let radius =
        (p.coarse_search.max(0.0) as f64 * rp[top].width.max(rp[top].height) as f64).ceil();
    let (dx, dy, _) =
        coarse::translation(&rp[top], &mp[top], radius as usize).ok_or(AlignError::NoOverlap)?;
    let init = Transform::translation(dx as f64, dy as f64);

    let (w, h) = (reference.width, reference.height);
    let max_shift = p.max_corner_shift as f64 * w.max(h) as f64;
    let mut best: Option<(bool, Alignment)> = None;
    for &model in p.model.fallbacks() {
        let mut t = init;
        let mut last = None;
        for l in (0..=top).rev() {
            if l != top {
                t = t.scaled(2.0);
            }
            let r = ecc::RefLevel {
                plane: &rp[l],
                gx: &grads[l].0,
                gy: &grads[l].1,
                spans: &spans[l],
            };
            // Bicubic sampling on the finest level: bilinear's varying blur
            // biases the sub-pixel estimate.
            let res = ecc::refine(
                &r,
                &mp[l],
                t,
                model.at_level(l),
                p.max_iterations,
                p.tolerance,
                l == 0,
            );
            t = res.transform;
            last = Some(res);
        }
        let res = last.expect("at least one level");
        let shift_ok = t.max_corner_deformation(w, h) <= max_shift;
        let ok = res.converged && shift_ok && res.correlation >= p.min_correlation as f64;
        let a = Alignment {
            transform: t,
            correlation: res.correlation as f32,
            converged: ok,
            model,
        };
        if ok {
            return Ok(a);
        }
        // Keep the best failed attempt: plausible corners first, then ρ.
        let better = match &best {
            None => true,
            Some((s, b)) => (shift_ok, a.correlation) > (*s, b.correlation),
        };
        if better {
            best = Some((shift_ok, a));
        }
    }
    Ok(best.expect("at least one model").1)
}

/// Pixels that take part in ECC on a level: everything but the 1-px border,
/// or, above `max_pixels`, the most textured tiles of each cell of a 4×4 grid.
fn select_spans(l: &Plane, gx: &[f32], gy: &[f32], max_pixels: usize) -> Vec<ecc::Span> {
    const TILE: usize = 128;
    const GRID: usize = 4;
    let (w, h) = (l.width, l.height);
    if w * h <= max_pixels || w < 2 * TILE || h < 2 * TILE {
        return (1..h - 1)
            .map(|y| ecc::Span {
                y,
                x0: 1,
                x1: w - 1,
            })
            .collect();
    }
    let (ntx, nty) = (w / TILE, h / TILE);
    // Tile (i, j) covers [i·TILE, (i+1)·TILE), the last one stretched to the edge,
    // always inside the 1-px border.
    let bounds = |i: usize, n: usize, size: usize| {
        let a = (i * TILE).max(1);
        let b = if i + 1 == n { size - 1 } else { (i + 1) * TILE };
        (a, b)
    };
    let tiles: Vec<(usize, usize)> = (0..nty)
        .flat_map(|j| (0..ntx).map(move |i| (i, j)))
        .collect();
    let energy: Vec<f64> = tiles
        .par_iter()
        .map(|&(i, j)| {
            let (x0, x1) = bounds(i, ntx, w);
            let (y0, y1) = bounds(j, nty, h);
            let mut e = 0.0f64;
            for y in y0..y1 {
                for x in x0..x1 {
                    let k = y * w + x;
                    let wt = l.weight.as_ref().map_or(1.0, |v| v[k]);
                    e += (wt * (gx[k] * gx[k] + gy[k] * gy[k])) as f64;
                }
            }
            e
        })
        .collect();
    let per_cell = (max_pixels / (TILE * TILE)).div_ceil(GRID * GRID).max(1);
    let mut cells: Vec<Vec<usize>> = vec![Vec::new(); GRID * GRID];
    for (k, &(i, j)) in tiles.iter().enumerate() {
        cells[(j * GRID / nty) * GRID + i * GRID / ntx].push(k);
    }
    let mut spans = Vec::new();
    for mut cell in cells {
        cell.sort_by(|&a, &b| energy[b].total_cmp(&energy[a]));
        for &k in cell.iter().take(per_cell) {
            let (i, j) = tiles[k];
            let (x0, x1) = bounds(i, ntx, w);
            let (y0, y1) = bounds(j, nty, h);
            spans.extend((y0..y1).map(|y| ecc::Span { y, x0, x1 }));
        }
    }
    spans
}
