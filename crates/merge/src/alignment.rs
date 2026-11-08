//! Aligning the shots of a bracket with the [`align`] crate: the planes,
//! model selection, and a robust pass for scenes with moving objects.

use align::{warp_plane, AlignParams, Alignment, Interp, Model, Plane};
use rayon::prelude::*;

use crate::hdr::Shot;
use crate::weights::{highlight_weight, max3};

/// A homography must reduce the residual `1 − ρ` of a similarity by this
/// factor to be used.
const HOMOGRAPHY_GAIN: f32 = 0.8;
/// Below this correlation a result may have been pulled by a moving object:
/// the robust pass is tried too.
const SUSPICIOUS: f32 = 0.98;
/// Residuals (log₂) beyond `max(K · σ, MIN)` are outliers in the robust pass.
const OUTLIER_K: f32 = 5.0;
const OUTLIER_MIN: f32 = 0.3;
/// The robust pass must keep at least this share of the usable pixels.
const MIN_INLIERS: f32 = 0.5;

/// Log luminance for alignment, weighted 0 where clipped (h < 0.5) or near
/// the noise floor.
pub fn align_plane(s: &Shot) -> Plane {
    let (w, h) = (s.data.width, s.data.height);
    let px = &s.data.pixels;
    let data = px.par_iter().map(|p| ((p[0] + p[1] + p[2]) / 3.0).max(1e-4).log2()).collect();
    let weight = px
        .par_iter()
        .zip(&s.clipped)
        .map(|(p, &c)| {
            let y = (p[0] + p[1] + p[2]) / 3.0;
            f32::from(highlight_weight(max3(*p), c != 0) >= 0.5 && y >= 0.01)
        })
        .collect();
    Plane { width: w, height: h, data, weight: Some(weight) }
}

/// Scale (pixels) of the local contrast normalisation.
const NORMALISE_SIGMA: f32 = 12.0;
/// Contrast floor (EV) of the normalisation, so flat noisy areas are not
/// amplified.
const NORMALISE_FLOOR: f32 = 0.05;

/// A plane with its local contrast normalised, `(L − μ) / √(σ² + floor²)`
/// over a Gaussian window (weighted), softly limited to ±3. A large object
/// then counts by its outline rather than its area, so an object moving
/// between the shots cannot pull the alignment onto itself. Less precise
/// than the plain plane (noise is amplified): used only to start the
/// robust pass.
fn normalised(p: &Plane) -> Plane {
    let (w, h) = (p.width, p.height);
    let weight = p.weight.clone().unwrap_or_else(|| vec![1.0; w * h]);
    let lw: Vec<f32> = p.data.iter().zip(&weight).map(|(l, w)| l * w).collect();
    let llw: Vec<f32> = p.data.iter().zip(&weight).map(|(l, w)| l * l * w).collect();
    let blur = |v: &[f32]| crate::ghost::blur(v, w, h, NORMALISE_SIGMA);
    let ((s1, s2), sw) = rayon::join(|| rayon::join(|| blur(&lw), || blur(&llw)), || blur(&weight));
    let data = (0..w * h)
        .into_par_iter()
        .map(|i| {
            if sw[i] < 1e-3 {
                return 0.0;
            }
            let mean = s1[i] / sw[i];
            let var = (s2[i] / sw[i] - mean * mean).max(0.0);
            let z = (p.data[i] - mean) / (var + NORMALISE_FLOOR * NORMALISE_FLOOR).sqrt();
            3.0 * (z / 3.0).tanh()
        })
        .collect();
    Plane { width: w, height: h, data, weight: Some(weight) }
}

fn run(reference: &Plane, moving: &Plane, model: Model, coarse_search: f32) -> Option<Alignment> {
    align::align(reference, moving, &AlignParams { model, coarse_search, ..AlignParams::default() }).ok().filter(|a| a.converged)
}

/// A similarity, or a homography when it fits clearly better. Shots two or
/// more EV apart share only a band of valid pixels, where the perspective
/// terms of a homography are poorly constrained and extrapolate badly to
/// the corners; a hand-held bracket rarely needs them.
fn select(reference: &Plane, moving: &Plane, coarse_search: f32) -> Option<Alignment> {
    let (s, h) = rayon::join(|| run(reference, moving, Model::Similarity, coarse_search), || run(reference, moving, Model::Homography, coarse_search));
    match (s, h) {
        (Some(s), Some(h)) if h.model == Model::Homography && (1.0 - h.correlation) < HOMOGRAPHY_GAIN * (1.0 - s.correlation) => Some(h),
        (Some(s), _) => Some(s),
        (None, h) => h,
    }
}

/// Aligns `moving` to `reference`; `None` when nothing converges.
///
/// A large object moving between the shots lowers the correlation at the
/// right place and can win the coarse search instead (a pure translation
/// lining up the object, accepted on ρ alone). So when the plain result is
/// missing or below [`SUSPICIOUS`], a robust pass runs: a first similarity
/// on planes with normalised local contrast (see [`normalised`]), then the
/// pixels whose residual on the plain planes is an outlier are dropped and
/// the shots are aligned again without them, starting from the identity.
/// The robust result wins if it converges with a higher correlation.
pub fn align_pair(reference: &Plane, moving: &Plane) -> Option<Alignment> {
    let plain = select(reference, moving, AlignParams::default().coarse_search);
    if plain.as_ref().is_some_and(|a| a.correlation >= SUSPICIOUS) {
        return plain;
    }
    let robust = robust_pass(reference, moving);
    match (plain, robust) {
        (Some(p), Some(r)) => Some(if r.correlation > p.correlation { r } else { p }),
        (p, r) => p.or(r),
    }
}

fn robust_pass(reference: &Plane, moving: &Plane) -> Option<Alignment> {
    // A similarity on the normalised planes, from the coarse search.
    let params = AlignParams { model: Model::Similarity, min_correlation: 0.0, ..AlignParams::default() };
    let (nr, nm) = rayon::join(|| normalised(reference), || normalised(moving));
    let first = align::align(&nr, &nm, &params).ok()?;
    let (w, h) = (reference.width, reference.height);
    let (mv, cov) = warp_plane(&moving.data, w, h, &first.transform, w, h, Interp::Bilinear);
    let (mw, _) = match &moving.weight {
        Some(wt) => warp_plane(wt, w, h, &first.transform, w, h, Interp::Bilinear),
        None => (vec![1.0; w * h], cov.clone()),
    };
    let rw = |i: usize| reference.weight.as_ref().map_or(1.0, |v| v[i]);
    let usable: Vec<usize> = (0..w * h).filter(|&i| cov[i] && rw(i) > 0.5 && mw[i] > 0.99).collect();
    if usable.is_empty() {
        return None;
    }
    let mut r: Vec<f32> = usable.iter().map(|&i| mv[i] - reference.data[i]).collect();
    let med = median(&mut r);
    let mut dev: Vec<f32> = r.iter().map(|v| (v - med).abs()).collect();
    let sigma = 1.4826 * median(&mut dev);
    let limit = (OUTLIER_K * sigma).max(OUTLIER_MIN);
    let mut outlier = vec![false; w * h];
    for &i in &usable {
        outlier[i] = (mv[i] - reference.data[i] - med).abs() > limit;
    }
    // Grow the outliers a little: the edges of a moving object are outliers too.
    let outlier = crate::ghost::dilate(&outlier, w, h, 2);
    let kept = usable.iter().filter(|&&i| !outlier[i]).count();
    if (kept as f32) < MIN_INLIERS * usable.len() as f32 {
        return None;
    }
    let weight: Vec<f32> = (0..w * h).map(|i| if outlier[i] { 0.0 } else { rw(i) }).collect();
    let masked = Plane { weight: Some(weight), ..reference.clone() };
    select(&masked, moving, 0.0)
}

fn median(v: &mut [f32]) -> f32 {
    let mid = v.len() / 2;
    let (_, m, _) = v.select_nth_unstable_by(mid, f32::total_cmp);
    *m
}
