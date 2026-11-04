//! Global tone: the Basic sliders, the parametric curve and the point curves,
//! baked into 1D lookup tables over "Melissa" values (ProPhoto primaries with
//! the sRGB transfer curve, the space Lightroom's curves and readouts use).
//!
//! These are approximations of Adobe's PV2012 controls: same direction and a
//! similar strength, not the same maths (which is not public).

use xmp::{Basic, Curve, ParametricCurve};

/// A 1D table over [0, 1] with linear interpolation; inputs are clamped.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut1 {
    table: Vec<f32>,
}

const LUT_SIZE: usize = 4096;

impl Lut1 {
    pub fn from_fn(f: impl Fn(f32) -> f32) -> Self {
        let last = (LUT_SIZE - 1) as f32;
        Lut1 { table: (0..LUT_SIZE).map(|i| f(i as f32 / last)).collect() }
    }

    pub fn identity() -> Self {
        Self::from_fn(|v| v)
    }

    pub fn is_identity(&self) -> bool {
        let last = (LUT_SIZE - 1) as f32;
        self.table.iter().enumerate().all(|(i, v)| (v - i as f32 / last).abs() < 1e-6)
    }

    #[inline]
    pub fn get(&self, v: f32) -> f32 {
        let t = v.clamp(0.0, 1.0) * (LUT_SIZE - 1) as f32;
        let i = (t as usize).min(LUT_SIZE - 2);
        let f = t - i as f32;
        self.table[i] + (self.table[i + 1] - self.table[i]) * f
    }

    /// `self` followed by `next`.
    pub fn then(&self, next: &Lut1) -> Lut1 {
        Lut1 { table: self.table.iter().map(|&v| next.get(v)).collect() }
    }

    /// Makes the table non-decreasing and keeps it inside [0, 1].
    fn monotone(mut self) -> Self {
        let mut max = 0.0f32;
        for v in &mut self.table {
            max = max.max(v.clamp(0.0, 1.0));
            *v = max;
        }
        self
    }
}

/// Monotone cubic (Fritsch–Carlson) interpolation through curve points given
/// in 0..255. Inputs outside the first/last point take the end values, as in
/// Lightroom (a curve starting at (5, 0) maps everything below 5 to black).
pub fn spline(curve: &Curve) -> Lut1 {
    let mut pts: Vec<(f32, f32)> = curve.0.iter().map(|&(x, y)| (x / 255.0, y / 255.0)).collect();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-6);
    if pts.len() < 2 {
        return Lut1::identity();
    }
    let n = pts.len();
    let h: Vec<f32> = (0..n - 1).map(|i| pts[i + 1].0 - pts[i].0).collect();
    let delta: Vec<f32> = (0..n - 1).map(|i| (pts[i + 1].1 - pts[i].1) / h[i]).collect();
    let mut m = vec![0.0f32; n];
    m[0] = delta[0];
    m[n - 1] = delta[n - 2];
    for i in 1..n - 1 {
        m[i] = if delta[i - 1] * delta[i] <= 0.0 { 0.0 } else { (delta[i - 1] + delta[i]) / 2.0 };
    }
    for i in 0..n - 1 {
        if delta[i] == 0.0 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
            continue;
        }
        let (a, b) = (m[i] / delta[i], m[i + 1] / delta[i]);
        let s = a * a + b * b;
        if s > 9.0 {
            let t = 3.0 / s.sqrt();
            m[i] = t * a * delta[i];
            m[i + 1] = t * b * delta[i];
        }
    }
    Lut1::from_fn(|x| {
        if x <= pts[0].0 {
            return pts[0].1;
        }
        if x >= pts[n - 1].0 {
            return pts[n - 1].1;
        }
        let i = (0..n - 1).find(|&i| x <= pts[i + 1].0).unwrap();
        let t = (x - pts[i].0) / h[i];
        let (t2, t3) = (t * t, t * t * t);
        let y = (2.0 * t3 - 3.0 * t2 + 1.0) * pts[i].1
            + (t3 - 2.0 * t2 + t) * h[i] * m[i]
            + (-2.0 * t3 + 3.0 * t2) * pts[i + 1].1
            + (t3 - t2) * h[i] * m[i + 1];
        y.clamp(0.0, 1.0)
    })
}

/// Melissa value of 18% grey: the pivot of the contrast curve.
const MID_GREY: f32 = 0.4613;

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Contrast, Highlights, Shadows, Whites, Blacks (−100..100 each).
pub fn basic(b: &Basic) -> Lut1 {
    let k = 1.0 + b.contrast / 100.0 * 0.6;
    // v·(1−v)³ peaks at 0.25, v³·(1−v) at 0.75; both normalised to peak 1.
    let shadows = |v: f32| v * (1.0 - v).powi(3) / 0.1055;
    let highlights = |v: f32| v.powi(3) * (1.0 - v) / 0.1055;
    Lut1::from_fn(|v| {
        let mut v = if (k - 1.0).abs() < 1e-6 {
            v
        } else if v < MID_GREY {
            MID_GREY * (v / MID_GREY).powf(k)
        } else {
            1.0 - (1.0 - MID_GREY) * ((1.0 - v) / (1.0 - MID_GREY)).powf(k)
        };
        v += b.highlights / 100.0 * 0.15 * highlights(v);
        v += b.shadows / 100.0 * 0.15 * shadows(v);
        v += b.whites / 100.0 * 0.15 * v.powi(4);
        v += b.blacks / 100.0 * 0.06 * (1.0 - v).powi(4);
        v
    })
    .monotone()
}

/// The parametric curve: four tonal regions split at Shadow/Midtone/Highlight
/// Split, each raised or lowered by a smooth bump over its region.
pub fn parametric(p: &ParametricCurve) -> Lut1 {
    let amounts = [p.shadows, p.darks, p.lights, p.highlights];
    if amounts.iter().all(|a| *a == 0.0) {
        return Lut1::identity();
    }
    let s = [p.shadow_split, p.midtone_split, p.highlight_split].map(|v| (v / 100.0).clamp(0.01, 0.99));
    let edges = [0.0, s[0], s[1], s[2], 1.0];
    Lut1::from_fn(|v| {
        let mut out = v;
        for (i, amount) in amounts.iter().enumerate() {
            let (a, b) = (edges[i], edges[i + 1]);
            let half = (b - a) / 2.0;
            // The bump reaches half a region beyond each edge; the ends of
            // the curve stay fixed.
            let (lo, hi) = ((a - half).max(0.0), (b + half).min(1.0));
            if v <= lo || v >= hi {
                continue;
            }
            let t = (v - lo) / (hi - lo);
            out += amount / 100.0 * 0.25 * (std::f32::consts::PI * t).sin().powi(2) * (b - a).sqrt();
        }
        out
    })
    .monotone()
}

/// Base rendering for raw files: identity up to `KNEE`, then a soft shoulder
/// that maps everything above the clip point into the last few percent, so
/// negative Highlights/Exposure can still pull detail back.
pub fn shoulder(x: f32) -> f32 {
    const KNEE: f32 = 0.7;
    if x <= KNEE {
        x.max(0.0)
    } else {
        KNEE + (1.0 - KNEE) * ((x - KNEE) / (1.0 - KNEE)).tanh()
    }
}

/// Applies a curve to RGB the way the DNG SDK applies profile tone curves
/// (`RefBaselineRGBTone`): the largest and smallest channels go through the
/// curve and the middle one keeps its relative position, so hue is kept.
pub fn rgb_tone(p: [f32; 3], curve: &Lut1) -> [f32; 3] {
    let p = p.map(|v| v.clamp(0.0, 1.0));
    let (mut hi, mut lo) = (0, 0);
    for c in 1..3 {
        if p[c] > p[hi] {
            hi = c;
        }
        if p[c] < p[lo] {
            lo = c;
        }
    }
    if hi == lo || p[hi] - p[lo] <= 0.0 {
        return [curve.get(p[0]); 3];
    }
    let mid = 3 - hi - lo;
    let (fh, fl) = (curve.get(p[hi]), curve.get(p[lo]));
    let mut out = [0.0; 3];
    out[hi] = fh;
    out[lo] = fl;
    out[mid] = fl + (fh - fl) * (p[mid] - p[lo]) / (p[hi] - p[lo]);
    out
}

/// Midtone weight used by local adjustments: 1 at mid grey, 0 at the ends.
pub fn midtones(v: f32) -> f32 {
    smoothstep(0.0, 0.35, v) * (1.0 - smoothstep(0.65, 1.0, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve(pts: &[(f32, f32)]) -> Curve {
        Curve(pts.to_vec())
    }

    #[test]
    fn identity_curve_is_identity() {
        assert!(spline(&Curve::default()).is_identity());
        assert!(basic(&Basic::default()).is_identity());
        assert!(parametric(&ParametricCurve::default()).is_identity());
    }

    #[test]
    fn spline_hits_points_and_is_monotone() {
        let c = curve(&[(0., 0.), (39., 28.), (126., 145.), (255., 255.)]);
        let lut = spline(&c);
        for (x, y) in &c.0 {
            assert!((lut.get(x / 255.0) - y / 255.0).abs() < 2e-3, "{x} -> {}", lut.get(x / 255.0) * 255.0);
        }
        let mut last = -1.0;
        for i in 0..=1000 {
            let v = lut.get(i as f32 / 1000.0);
            assert!(v >= last);
            last = v;
        }
    }

    #[test]
    fn spline_clamps_outside_points() {
        let lut = spline(&curve(&[(5., 0.), (126., 126.), (255., 250.)]));
        assert_eq!(lut.get(0.0), 0.0);
        assert_eq!(lut.get(4.0 / 255.0), 0.0);
        assert!((lut.get(1.0) - 250.0 / 255.0).abs() < 1e-5);
    }

    #[test]
    fn sliders_move_the_right_tones() {
        let at = |b: Basic, v: f32| basic(&b).get(v);
        let low_contrast = Basic { contrast: -50.0, ..Default::default() };
        assert!(at(low_contrast.clone(), 0.1) > 0.1 && at(low_contrast.clone(), 0.9) < 0.9);
        assert!((at(low_contrast, MID_GREY) - MID_GREY).abs() < 1e-3);

        let shadows = Basic { shadows: 50.0, ..Default::default() };
        assert!(at(shadows.clone(), 0.25) > 0.27 && (at(shadows, 0.9) - 0.9).abs() < 0.01);
        let highlights = Basic { highlights: -50.0, ..Default::default() };
        assert!(at(highlights.clone(), 0.75) < 0.72 && (at(highlights, 0.1) - 0.1).abs() < 0.01);
        assert!(at(Basic { blacks: 100.0, ..Default::default() }, 0.0) > 0.05);
        assert!(at(Basic { whites: -100.0, ..Default::default() }, 1.0) < 0.9);
    }

    #[test]
    fn parametric_regions() {
        let p = ParametricCurve { darks: 60.0, ..Default::default() };
        let lut = parametric(&p);
        assert!(lut.get(0.375) > 0.42);
        assert!((lut.get(0.9) - 0.9).abs() < 1e-4);
        assert_eq!(lut.get(0.0), 0.0);
    }

    #[test]
    fn rgb_tone_keeps_hue() {
        let curve = spline(&curve(&[(0., 0.), (64., 100.), (255., 255.)]));
        let p = [0.6, 0.3, 0.1];
        let out = rgb_tone(p, &curve);
        let ratio = |q: [f32; 3]| (q[1] - q[2]) / (q[0] - q[2]);
        assert!((ratio(out) - ratio(p)).abs() < 1e-5, "{out:?}");
        assert!(out[0] > p[0] && out[2] > p[2]);
        assert_eq!(rgb_tone([0.4; 3], &curve), [curve.get(0.4); 3]);
    }

    #[test]
    fn shoulder_is_smooth_and_bounded() {
        assert_eq!(shoulder(0.5), 0.5);
        assert!(shoulder(1.0) > 0.9 && shoulder(1.0) < 0.97);
        assert!(shoulder(8.0) <= 1.0);
        assert!(shoulder(0.7001) > shoulder(0.7));
    }
}
