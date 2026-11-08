//! Relative exposures `eᵢ`: nominal from EXIF (`t · ISO / N²`), refined from
//! the pixels of the aligned half-size shots.
//!
//! EXIF values are nominal (1/125 s is really 1/128, f/5.6 is 5.657), so
//! between shots adjacent in exposure the ratio is measured on pixels valid
//! in both, `r = exp(median(ln Sⱼ − ln Sᵢ))` with `S` the sum of the
//! channels, and chained up to the reference.

use rayon::prelude::*;

use crate::hdr::Shot;
use crate::weights::max3;

/// Pixels used for a ratio: maximum channel within this range in both shots
/// (above the noise, below the highlight knee), not clipped, covered.
pub const VALID_RANGE: (f32, f32) = (0.03, 0.85);
/// Minimum share of the pixels that must be usable for a pixel ratio.
pub const MIN_SAMPLES: f64 = 0.005;
/// Pixel ratios further than this from EXIF (in EV) trigger a warning.
pub const EXIF_TOLERANCE_EV: f64 = 0.3;
/// Shots closer than this (EV) count as the same exposure.
pub const SAME_EXPOSURE_EV: f64 = 0.15;

/// Nominal exposure `t · ISO / N²` of each shot, or `None` if a shot has no
/// exposure time. ISO and aperture are left out when some shot lacks them
/// (then they are taken as equal).
pub fn exif_exposures(info: &[&raw::PhotoInfo]) -> Option<Vec<f64>> {
    let all = |f: &dyn Fn(&raw::PhotoInfo) -> Option<f64>| info.iter().all(|i| f(i).is_some_and(|v| v > 0.0));
    if !all(&|i| i.exposure_time) {
        return None;
    }
    let use_iso = all(&|i| i.iso.map(f64::from));
    let use_n = all(&|i| i.f_number);
    Some(
        info.iter()
            .map(|i| {
                let iso = if use_iso { i.iso.unwrap_or(100) as f64 } else { 100.0 };
                let n = if use_n { i.f_number.unwrap_or(1.0) } else { 1.0 };
                i.exposure_time.unwrap_or(0.0) * iso / (n * n)
            })
            .collect(),
    )
}

/// A brightness statistic increasing with the exposure of the same scene
/// (mean of the clipped channel sum), to order shots without EXIF.
pub fn brightness(s: &Shot) -> f64 {
    let sum: f64 = s.data.pixels.par_iter().map(|p| p.iter().map(|&c| c.clamp(0.0, 1.0) as f64).sum::<f64>()).sum();
    sum / s.data.pixels.len().max(1) as f64
}

/// The measured exposure ratio `e_b / e_a`, with the number of pixels used,
/// or `None` when fewer than [`MIN_SAMPLES`] of them are usable.
pub fn pixel_ratio(a: &Shot, b: &Shot) -> (Option<f64>, usize) {
    let n = a.data.pixels.len();
    let usable = |s: &Shot, i: usize| {
        let p = s.data.pixels[i];
        let m = max3(p);
        s.covers(i) && s.clipped[i] == 0 && (VALID_RANGE.0..=VALID_RANGE.1).contains(&m) && p.iter().all(|&c| c > 0.0)
    };
    let mut d: Vec<f32> = (0..n)
        .into_par_iter()
        .filter(|&i| usable(a, i) && usable(b, i))
        .map(|i| {
            let sa: f32 = a.data.pixels[i].iter().sum();
            let sb: f32 = b.data.pixels[i].iter().sum();
            sb.ln() - sa.ln()
        })
        .collect();
    let samples = d.len();
    if (samples as f64) < MIN_SAMPLES * n as f64 || samples == 0 {
        return (None, samples);
    }
    let mid = samples / 2;
    let (_, m, _) = d.select_nth_unstable_by(mid, f32::total_cmp);
    (Some((*m as f64).exp()), samples)
}

/// Where a shot's exposure came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvSource {
    Reference,
    /// Measured on the pixels (the usual case).
    Pixels,
    /// EXIF, because too few pixels were usable.
    Exif,
}
