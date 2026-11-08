//! Per-pixel weights of the radiance estimate
//! `X = Σ wᵢ · vᵢ/eᵢ / Σ wᵢ` (see `docs/merge.md`).
//!
//! `wᵢ = eᵢ · h(mᵢ)`: with photon noise the variance of `vᵢ/eᵢ` is about
//! `X/eᵢ`, so the inverse-variance weight is proportional to `eᵢ` and the
//! brightest unclipped shot dominates, with a continuous hand-over to darker
//! shots as it approaches clipping. `h` is per pixel, not per channel, so a
//! single clipped channel cannot shift the hue.

/// `h` starts falling at this maximum channel value…
pub const KNEE_LOW: f32 = 0.80;
/// …and reaches 0 here.
pub const KNEE_HIGH: f32 = 0.95;

/// Floor weight (relative to `h`) of the darkest shot: where every shot is
/// clipped, the darkest one, brought onto the camera neutral, still gives
/// the pixel a value.
pub const DARKEST_FLOOR: f32 = 1e-4;
/// Floor of the reference, far below the darkest's: only used where the
/// darkest shot does not cover the pixel (alignment borders).
pub const REFERENCE_FLOOR: f32 = 1e-8;

#[inline]
pub fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
pub fn max3(v: [f32; 3]) -> f32 {
    v[0].max(v[1]).max(v[2])
}

/// `h(m)`: 1 well below clipping, 0 from [`KNEE_HIGH`] up and wherever the
/// clipping mask is set.
#[inline]
pub fn highlight_weight(max_channel: f32, clipped: bool) -> f32 {
    if clipped {
        0.0
    } else {
        1.0 - smoothstep(KNEE_LOW, KNEE_HIGH, max_channel)
    }
}

/// A clipped pixel moved onto the camera neutral, `n · maxᶜ(vᶜ/nᶜ)`, so that
/// blown highlights stay white after white balance instead of turning
/// magenta or green.
#[inline]
pub fn neutral_fill(v: [f32; 3], neutral: [f32; 3]) -> [f32; 3] {
    let k = (0..3).map(|c| v[c] / neutral[c].max(1e-6)).fold(0.0f32, f32::max);
    neutral.map(|n| n * k)
}

/// How one shot enters the merge.
#[derive(Clone, Copy, Debug)]
pub struct FrameWeight {
    /// `eᵢ`, relative to the reference (1).
    pub exposure: f32,
    /// Minimum `h` (see [`DARKEST_FLOOR`]); 0 for most shots.
    pub floor: f32,
    /// As-shot neutral of the reference, max component 1.
    pub neutral: [f32; 3],
}

impl FrameWeight {
    /// Weight and radiance estimate (`v/e`, in reference units) of a sample.
    #[inline]
    pub fn sample(&self, v: [f32; 3], clipped: bool) -> (f32, [f32; 3]) {
        let h = highlight_weight(max3(v), clipped);
        let inv = 1.0 / self.exposure;
        if h > 0.0 {
            (self.exposure * h.max(self.floor), v.map(|c| c * inv))
        } else if self.floor > 0.0 {
            (self.exposure * self.floor, neutral_fill(v, self.neutral).map(|c| c * inv))
        } else {
            (0.0, [0.0; 3])
        }
    }

    /// The sample's own estimate, used where it is the single source of a
    /// ghost region: `v/e`, neutral-filled when clipped.
    #[inline]
    pub fn value(&self, v: [f32; 3], clipped: bool) -> [f32; 3] {
        let inv = 1.0 / self.exposure;
        if highlight_weight(max3(v), clipped) > 0.0 {
            v.map(|c| c * inv)
        } else {
            neutral_fill(v, self.neutral).map(|c| c * inv)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_weight_is_continuous_and_zero_when_clipped() {
        assert_eq!(highlight_weight(0.5, false), 1.0);
        assert_eq!(highlight_weight(KNEE_LOW, false), 1.0);
        assert!((highlight_weight(0.875, false) - 0.5).abs() < 1e-6);
        assert_eq!(highlight_weight(KNEE_HIGH, false), 0.0);
        assert_eq!(highlight_weight(0.1, true), 0.0);
        let mut prev = 1.0;
        for i in 0..=100 {
            let h = highlight_weight(0.79 + 0.0017 * i as f32, false);
            assert!(h <= prev && prev - h < 0.03);
            prev = h;
        }
    }

    #[test]
    fn neutral_fill_is_neutral_and_not_darker() {
        let n = [0.5, 1.0, 0.7];
        let v = neutral_fill([1.0, 1.0, 0.6], n);
        // Balanced: equal channels, at least as bright as the brightest one.
        let b = [v[0] / n[0], v[1] / n[1], v[2] / n[2]];
        assert!((b[0] - b[1]).abs() < 1e-6 && (b[1] - b[2]).abs() < 1e-6);
        assert!((b[0] - 2.0).abs() < 1e-6);
    }

    #[test]
    fn darkest_floor_only_matters_when_clipped() {
        let f = FrameWeight { exposure: 0.25, floor: DARKEST_FLOOR, neutral: [1.0; 3] };
        let (w, x) = f.sample([0.2, 0.2, 0.2], false);
        assert_eq!(w, 0.25);
        assert!((x[0] - 0.8).abs() < 1e-6);
        let (w, x) = f.sample([1.0, 0.99, 1.0], true);
        assert!((w - 0.25 * DARKEST_FLOOR).abs() < 1e-9);
        assert!((x[1] - 4.0).abs() < 1e-5);
        let plain = FrameWeight { floor: 0.0, ..f };
        assert_eq!(plain.sample([1.0; 3], true).0, 0.0);
    }
}
