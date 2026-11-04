//! Colour adjustments: calibration, HSL, vibrance/saturation, colour grading
//! and profile RGB tables.
//!
//! HSL, vibrance and grading work in an opponent space built on gamma-encoded
//! Rec.709-primary RGB (D50): Y (luma), and a chroma plane (a, b) whose angle
//! is the familiar hue wheel: 0° red, 60° yellow, 120° green, 240° blue.

use lut::Lut3D;
use raw::color::{self, Mat3};
use xmp::rgb_table::Primaries;
use xmp::{Calibration, ColorGrade, Hsl, RgbTable, Wheel};

use crate::tone::midtones;

const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];
const SQRT3_2: f32 = 0.866_025_4;

/// RGB → (Y, a, b).
#[inline]
pub fn to_yab(p: [f32; 3]) -> [f32; 3] {
    [LUMA[0] * p[0] + LUMA[1] * p[1] + LUMA[2] * p[2], p[0] - 0.5 * (p[1] + p[2]), SQRT3_2 * (p[1] - p[2])]
}

/// (Y, a, b) → RGB, the exact inverse of [`to_yab`].
#[inline]
pub fn from_yab([y, a, b]: [f32; 3]) -> [f32; 3] {
    // With g − b = b'/(√3/2) and r = a + (g + b)/2, solve the luma equation.
    let d = b / SQRT3_2; // g − b
    // y = L0·(a + (g+b)/2) + L1·g + L2·bl, with g = bl + d.
    let k = LUMA[0] * 0.5;
    let bl = (y - LUMA[0] * a - (k + LUMA[1]) * d) / (2.0 * k + LUMA[1] + LUMA[2]);
    let g = bl + d;
    [a + 0.5 * (g + bl), g, bl]
}

/// Hue in degrees (0..360) and chroma of an (a, b) pair.
#[inline]
fn polar(a: f32, b: f32) -> (f32, f32) {
    (b.atan2(a).to_degrees().rem_euclid(360.0), a.hypot(b))
}

#[inline]
fn signed_oetf(v: f32) -> f32 {
    v.signum() * color::srgb_oetf(v.abs())
}

#[inline]
fn signed_eotf(v: f32) -> f32 {
    v.signum() * color::srgb_eotf(v.abs())
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Converts between linear ProPhoto and the encoded Rec.709 space the colour
/// controls run in.
#[derive(Clone, Debug)]
pub struct ColourSpace {
    to_709: [[f32; 3]; 3],
    from_709: [[f32; 3]; 3],
}

impl ColourSpace {
    pub fn new() -> Self {
        let m = color::prophoto_to_d50_rgb(color::SRGB_PRIMARIES);
        ColourSpace { to_709: color::to_f32(&m), from_709: color::to_f32(&color::invert(&m).unwrap()) }
    }

    #[inline]
    pub fn encode(&self, linear_prophoto: [f32; 3]) -> [f32; 3] {
        color::apply(&self.to_709, linear_prophoto).map(signed_oetf)
    }

    #[inline]
    pub fn decode(&self, encoded: [f32; 3]) -> [f32; 3] {
        color::apply(&self.from_709, encoded.map(signed_eotf))
    }
}

impl Default for ColourSpace {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Calibration

/// The Calibration panel as a 3×3 matrix on linear ProPhoto: each Rec.709
/// primary is rotated in hue (±100 ≈ ±25°) and scaled in chroma, then the
/// matrix is rescaled so white stays white.
pub fn calibration_matrix(c: &Calibration) -> Option<Mat3> {
    let adjust = [(c.red_hue, c.red_sat), (c.green_hue, c.green_sat), (c.blue_hue, c.blue_sat)];
    if adjust.iter().all(|&(h, s)| h == 0.0 && s == 0.0) {
        return None;
    }
    let mut m: Mat3 = [[0.0; 3]; 3];
    for (i, &(hue, sat)) in adjust.iter().enumerate() {
        let mut e = [0.0f32; 3];
        e[i] = 1.0;
        let [y, a, b] = to_yab(e);
        let (h, ch) = polar(a, b);
        let h = (h + hue / 100.0 * 25.0).to_radians();
        let ch = ch * (1.0 + sat / 100.0 * 0.5).max(0.0);
        let col = from_yab([y, ch * h.cos(), ch * h.sin()]);
        for r in 0..3 {
            m[r][i] = col[r] as f64;
        }
    }
    // Columns scaled so R + G + B = white.
    let s = color::mul_vec(&color::invert(&m)?, [1.0; 3]);
    let m = color::mul(&m, &color::diag(s));
    let to = color::prophoto_to_d50_rgb(color::SRGB_PRIMARIES);
    Some(color::mul(&color::invert(&to)?, &color::mul(&m, &to)))
}

// ---------------------------------------------------------------------------
// HSL / Color mixer

/// Band centres on the hue wheel: red, orange, yellow, green, aqua, blue,
/// purple, magenta.
const BAND_HUES: [f32; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 270.0, 300.0];

/// Weights of the eight bands at `hue`: neighbouring bands cross-fade
/// smoothly and the weights always sum to 1.
fn band_weights(hue: f32) -> [f32; 8] {
    let mut w = [0.0f32; 8];
    for i in 0..8 {
        let (a, b) = (BAND_HUES[i], if i == 7 { 360.0 } else { BAND_HUES[i + 1] });
        if hue >= a && hue < b {
            let t = smoothstep(0.0, 1.0, (hue - a) / (b - a));
            w[i] = 1.0 - t;
            w[(i + 1) % 8] = t;
        }
    }
    w
}

#[derive(Clone, Debug)]
pub struct ColourOps {
    hsl: Option<Hsl>,
    vibrance: f32,
    saturation: f32,
    grade: Option<ColorGrade>,
}

impl ColourOps {
    pub fn new(hsl: &Hsl, vibrance: f32, saturation: f32, grade: &ColorGrade) -> Self {
        let active = |w: &Wheel| w.sat != 0.0 || w.lum != 0.0;
        let hsl_active = hsl.hue.iter().chain(&hsl.saturation).chain(&hsl.luminance).any(|v| *v != 0.0);
        let grade_active = [&grade.shadow, &grade.midtone, &grade.highlight, &grade.global].into_iter().any(active);
        ColourOps {
            hsl: hsl_active.then(|| hsl.clone()),
            vibrance,
            saturation,
            grade: grade_active.then(|| grade.clone()),
        }
    }

    pub fn is_identity(&self) -> bool {
        self.hsl.is_none() && self.vibrance == 0.0 && self.saturation == 0.0 && self.grade.is_none()
    }

    /// Applies the controls to an encoded Rec.709 pixel.
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let [mut y, a, b] = to_yab(rgb);
        let (mut hue, mut chroma) = polar(a, b);
        // How colourful the pixel is: greys are left alone by HSL luminance.
        let colourful = smoothstep(0.0, 0.25, chroma);

        if let Some(hsl) = &self.hsl {
            let w = band_weights(hue);
            let dot = |v: &[f32; 8]| w.iter().zip(v).map(|(w, v)| w * v / 100.0).sum::<f32>();
            hue += dot(&hsl.hue) * 30.0;
            chroma *= (1.0 + dot(&hsl.saturation)).max(0.0);
            y *= 1.0 + dot(&hsl.luminance) * 0.4 * colourful;
        }
        if self.vibrance != 0.0 {
            // Strongest on muted colours, gentler on skin tones (reds–yellows).
            let muted = (1.0 - (chroma / 0.5).min(1.0)).powi(2);
            let skin = if (0.0..=70.0).contains(&hue) { 0.5 } else { 1.0 };
            chroma *= (1.0 + self.vibrance / 100.0 * muted * skin).max(0.0);
        }
        if self.saturation != 0.0 {
            chroma *= (1.0 + self.saturation / 100.0).max(0.0);
        }

        let h = hue.to_radians();
        let (mut a, mut b) = (chroma * h.cos(), chroma * h.sin());
        if let Some(g) = &self.grade {
            for (wheel, weight) in grade_weights(g, y) {
                if wheel.sat != 0.0 {
                    let th = wheel.hue.to_radians();
                    let k = weight * wheel.sat / 100.0 * 0.12;
                    a += k * th.cos();
                    b += k * th.sin();
                }
                y += weight * wheel.lum / 100.0 * 0.15 * midtones(y).max(0.3);
            }
        }
        from_yab([y, a, b])
    }
}

/// The wheels with their weight at luma `y`: shadows, midtones and highlights
/// are bell curves centred at 0, the balance point and 1, normalised to sum
/// to 1; Blending widens them. The global wheel always has weight 1.
fn grade_weights(g: &ColorGrade, y: f32) -> [(&Wheel, f32); 4] {
    let centre = (0.5 - g.balance / 100.0 * 0.25).clamp(0.2, 0.8);
    let sigma = 0.18 + g.blending / 100.0 * 0.3;
    let bell = |c: f32| (-((y - c) / sigma).powi(2)).exp();
    let (s, m, h) = (bell(0.0), bell(centre), bell(1.0));
    let sum = s + m + h;
    [(&g.shadow, s / sum), (&g.midtone, m / sum), (&g.highlight, h / sum), (&g.global, 1.0)]
}

// ---------------------------------------------------------------------------
// Profile RGB tables

/// A Camera Raw RGB table, ready to apply to linear ProPhoto.
#[derive(Clone, Debug)]
pub struct TableOp {
    lut: Lut3D,
    to_space: [[f32; 3]; 3],
    from_space: [[f32; 3]; 3],
    gamma: xmp::rgb_table::Gamma,
    amount: f32,
}

impl TableOp {
    pub fn new(t: &RgbTable, amount: f32) -> Self {
        let primaries = match t.primaries {
            Primaries::Srgb => color::SRGB_PRIMARIES,
            Primaries::AdobeRgb => color::ADOBE_RGB_PRIMARIES,
            Primaries::DisplayP3 => color::P3_PRIMARIES,
            Primaries::Rec2020 => color::REC2020_PRIMARIES,
            Primaries::ProPhoto | Primaries::Other(_) => color::PROPHOTO_PRIMARIES,
        };
        let m = color::prophoto_to_d50_rgb(primaries);
        TableOp {
            lut: table_to_lut(t),
            to_space: color::to_f32(&m),
            from_space: color::to_f32(&color::invert(&m).unwrap()),
            gamma: t.gamma,
            amount: amount.clamp(t.min_amount as f32, t.max_amount.max(t.min_amount) as f32),
        }
    }

    pub fn apply(&self, linear_prophoto: [f32; 3]) -> [f32; 3] {
        use xmp::rgb_table::Gamma;
        let encode = |v: f32| match self.gamma {
            Gamma::Linear => v,
            Gamma::Srgb | Gamma::Other(_) => color::srgb_oetf(v),
            Gamma::G1_8 => v.powf(1.0 / 1.8),
            Gamma::G2_2 => v.powf(1.0 / 2.2),
            Gamma::Rec2020 => rec2020_oetf(v),
        };
        let decode = |v: f32| match self.gamma {
            Gamma::Linear => v,
            Gamma::Srgb | Gamma::Other(_) => color::srgb_eotf(v),
            Gamma::G1_8 => v.powf(1.8),
            Gamma::G2_2 => v.powf(2.2),
            Gamma::Rec2020 => rec2020_eotf(v),
        };
        let p = color::apply(&self.to_space, linear_prophoto).map(|v| encode(v.clamp(0.0, 1.0)));
        let out = self.lut.apply_amount(p, self.amount).map(|v| decode(v.clamp(0.0, 1.0)));
        color::apply(&self.from_space, out)
    }
}

fn rec2020_oetf(v: f32) -> f32 {
    if v < 0.018 {
        4.5 * v
    } else {
        1.099 * v.powf(0.45) - 0.099
    }
}

fn rec2020_eotf(v: f32) -> f32 {
    if v < 0.081 {
        v / 4.5
    } else {
        ((v + 0.099) / 1.099).powf(1.0 / 0.45)
    }
}

/// An RGB table as a 3D LUT (1D tables become a 3D LUT acting per channel).
pub fn table_to_lut(t: &RgbTable) -> Lut3D {
    let n = t.divisions as usize;
    if t.dimensions == 1 {
        return Lut3D::from_fn(n, |rgb| {
            std::array::from_fn(|c| {
                let i = (rgb[c] * (n - 1) as f32).round() as usize;
                t.samples[i][c] as f32 / 65535.0
            })
        });
    }
    let mut lut = Lut3D::identity(n);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                lut.data[r + n * (g + n * b)] = t.sample(r, g, b);
            }
        }
    }
    lut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3], eps: f32) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < eps)
    }

    #[test]
    fn yab_round_trips_and_hues() {
        for p in [[0.2, 0.5, 0.9], [1.0, 0.0, 0.0], [0.3, 0.3, 0.3]] {
            assert!(close(from_yab(to_yab(p)), p, 1e-5), "{p:?}");
        }
        let hue = |p| {
            let [_, a, b] = to_yab(p);
            polar(a, b).0
        };
        assert!(hue([1.0, 0.0, 0.0]).min(360.0 - hue([1.0, 0.0, 0.0])) < 1e-3);
        assert!((hue([1.0, 1.0, 0.0]) - 60.0).abs() < 1e-3);
        assert!((hue([0.0, 1.0, 0.0]) - 120.0).abs() < 1e-3);
        assert!((hue([0.0, 0.0, 1.0]) - 240.0).abs() < 1e-3);
    }

    #[test]
    fn band_weights_partition_unity() {
        for h in (0..360).step_by(7) {
            let w = band_weights(h as f32);
            assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-5, "{h}");
        }
        assert_eq!(band_weights(60.0)[2], 1.0);
    }

    #[test]
    fn neutral_settings_change_nothing() {
        let ops = ColourOps::new(&Hsl::default(), 0.0, 0.0, &ColorGrade::default());
        assert!(ops.is_identity());
        assert!(calibration_matrix(&Calibration::default()).is_none());
    }

    #[test]
    fn hsl_targets_its_band_only() {
        let mut hsl = Hsl::default();
        hsl.saturation[3] = -100.0; // greens
        let ops = ColourOps::new(&hsl, 0.0, 0.0, &ColorGrade::default());
        let green = ops.apply([0.2, 0.7, 0.2]); // hue 120°
        let [_, a, b] = to_yab(green);
        assert!(a.hypot(b) < 0.02, "{green:?}");
        let blue = [0.2, 0.3, 0.8];
        assert!(close(ops.apply(blue), blue, 1e-4));
        let grey = [0.5; 3];
        assert!(close(ops.apply(grey), grey, 1e-5));
    }

    #[test]
    fn grading_tints_shadows_and_highlights() {
        let g = ColorGrade {
            shadow: Wheel { hue: 240.0, sat: 100.0, lum: 0.0 },
            highlight: Wheel { hue: 60.0, sat: 100.0, lum: 0.0 },
            ..Default::default()
        };
        let ops = ColourOps::new(&Hsl::default(), 0.0, 0.0, &g);
        let dark = ops.apply([0.1; 3]);
        let bright = ops.apply([0.9; 3]);
        assert!(dark[2] > dark[0], "{dark:?}");
        assert!(bright[0] > bright[2] && bright[1] > bright[2], "{bright:?}");
    }

    #[test]
    fn calibration_keeps_white() {
        let c = Calibration { red_hue: 20.0, green_hue: 20.0, blue_hue: -20.0, blue_sat: 30.0, ..Default::default() };
        let m = calibration_matrix(&c).unwrap();
        let w = color::mul_vec(&m, [1.0; 3]);
        assert!(w.iter().all(|v| (v - 1.0).abs() < 1e-9), "{w:?}");
    }

    #[test]
    fn identity_table_is_transparent() {
        let op = TableOp::new(&RgbTable::identity(17), 1.0);
        let p = [0.2, 0.4, 0.1];
        assert!(close(op.apply(p), p, 2e-3), "{:?}", op.apply(p));
    }
}
