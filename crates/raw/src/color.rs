//! Colour science shared by the decoder and the pipeline: 3×3 matrices,
//! chromaticities, the working space (linear ProPhoto / ROMM RGB, D50, like
//! Camera Raw), sRGB output and Adobe's temperature/tint scale.

/// Row-major 3×3 matrix. Profile maths runs in f64; pixels use [`Mat3::to_f32`].
pub type Mat3 = [[f64; 3]; 3];
pub type Vec3 = [f64; 3];

pub const IDENTITY: Mat3 = [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]];

pub fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

pub fn mul_vec(m: &Mat3, v: Vec3) -> Vec3 {
    std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

pub fn diag(v: Vec3) -> Mat3 {
    [[v[0], 0., 0.], [0., v[1], 0.], [0., 0., v[2]]]
}

pub fn scale(m: &Mat3, s: f64) -> Mat3 {
    m.map(|row| row.map(|x| x * s))
}

pub fn lerp(a: &Mat3, b: &Mat3, t: f64) -> Mat3 {
    std::array::from_fn(|i| std::array::from_fn(|j| a[i][j] * t + b[i][j] * (1.0 - t)))
}

pub fn invert(m: &Mat3) -> Option<Mat3> {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let co = [e * i - f * h, f * g - d * i, d * h - e * g];
    let det = a * co[0] + b * co[1] + c * co[2];
    if det.abs() < 1e-12 {
        return None;
    }
    let r = 1.0 / det;
    Some([
        [co[0] * r, (c * h - b * i) * r, (b * f - c * e) * r],
        [co[1] * r, (a * i - c * g) * r, (c * d - a * f) * r],
        [co[2] * r, (b * g - a * h) * r, (a * e - b * d) * r],
    ])
}

pub fn to_f32(m: &Mat3) -> [[f32; 3]; 3] {
    m.map(|row| row.map(|x| x as f32))
}

#[inline]
pub fn apply(m: &[[f32; 3]; 3], p: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2])
}

// ---------------------------------------------------------------------------
// Chromaticities and colour spaces

/// CIE 1931 chromaticity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Xy {
    pub x: f64,
    pub y: f64,
}

impl Xy {
    pub const fn new(x: f64, y: f64) -> Self {
        Xy { x, y }
    }

    /// XYZ with Y = 1.
    pub fn to_xyz(self) -> Vec3 {
        [self.x / self.y, 1.0, (1.0 - self.x - self.y) / self.y]
    }

    pub fn from_xyz(v: Vec3) -> Self {
        let sum = v[0] + v[1] + v[2];
        if sum <= 0.0 {
            return D50;
        }
        Xy::new(v[0] / sum, v[1] / sum)
    }
}

/// The DNG profile connection space white (XYZ 0.9642, 1, 0.8249).
pub const D50: Xy = Xy::new(0.3457, 0.3585);
pub const D65: Xy = Xy::new(0.3127, 0.3290);

pub const PROPHOTO_PRIMARIES: [Xy; 3] = [Xy::new(0.7347, 0.2653), Xy::new(0.1596, 0.8404), Xy::new(0.0366, 0.0001)];
pub const SRGB_PRIMARIES: [Xy; 3] = [Xy::new(0.64, 0.33), Xy::new(0.30, 0.60), Xy::new(0.15, 0.06)];
pub const ADOBE_RGB_PRIMARIES: [Xy; 3] = [Xy::new(0.64, 0.33), Xy::new(0.21, 0.71), Xy::new(0.15, 0.06)];
pub const P3_PRIMARIES: [Xy; 3] = [Xy::new(0.680, 0.320), Xy::new(0.265, 0.690), Xy::new(0.150, 0.060)];
pub const REC2020_PRIMARIES: [Xy; 3] = [Xy::new(0.708, 0.292), Xy::new(0.170, 0.797), Xy::new(0.131, 0.046)];

/// RGB → XYZ for the given primaries, scaled so RGB (1,1,1) is `white` with Y = 1.
pub fn rgb_to_xyz(primaries: [Xy; 3], white: Xy) -> Mat3 {
    let p = primaries.map(Xy::to_xyz);
    let m: Mat3 = std::array::from_fn(|i| [p[0][i], p[1][i], p[2][i]]);
    let s = mul_vec(&invert(&m).expect("degenerate primaries"), white.to_xyz());
    mul(&m, &diag(s))
}

const BRADFORD: Mat3 = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];

/// Bradford chromatic adaptation of XYZ values from white `from` to white `to`.
pub fn adaptation(from: Xy, to: Xy) -> Mat3 {
    let inv = invert(&BRADFORD).unwrap();
    let a = mul_vec(&BRADFORD, from.to_xyz());
    let b = mul_vec(&BRADFORD, to.to_xyz());
    // Keep the gain finite for nonsensical whites (e.g. far outside the locus).
    let gain = std::array::from_fn(|i| (b[i] / a[i]).clamp(0.1, 10.0));
    mul(&inv, &mul(&diag(gain), &BRADFORD))
}

/// Linear ProPhoto RGB (the working space) → XYZ D50.
pub fn prophoto_to_xyz() -> Mat3 {
    rgb_to_xyz(PROPHOTO_PRIMARIES, D50)
}

pub fn xyz_to_prophoto() -> Mat3 {
    invert(&prophoto_to_xyz()).unwrap()
}

/// Linear ProPhoto RGB → linear sRGB (D50 → D65 via Bradford).
pub fn prophoto_to_srgb() -> Mat3 {
    let xyz_to_srgb = invert(&rgb_to_xyz(SRGB_PRIMARIES, D65)).unwrap();
    mul(&xyz_to_srgb, &mul(&adaptation(D50, D65), &prophoto_to_xyz()))
}

pub fn srgb_to_prophoto() -> Mat3 {
    invert(&prophoto_to_srgb()).unwrap()
}

/// Linear ProPhoto → linear RGB with `primaries` and the same D50 white (no
/// adaptation): the "D50-relative" spaces Camera Raw's tables live in.
pub fn prophoto_to_d50_rgb(primaries: [Xy; 3]) -> Mat3 {
    mul(&invert(&rgb_to_xyz(primaries, D50)).unwrap(), &prophoto_to_xyz())
}

/// sRGB transfer function, linear → encoded.
#[inline]
pub fn srgb_oetf(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// sRGB transfer function, encoded → linear.
#[inline]
pub fn srgb_eotf(v: f32) -> f32 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

// ---------------------------------------------------------------------------
// Temperature / tint (the Camera Raw "Temperature" and "Tint" sliders)

/// Robertson's isotemperature lines: (mired, u, v, slope), as in the DNG SDK
/// (`dng_temperature.cpp`). The 325 mired row keeps the SDK's u = 0.24702
/// (Robertson's paper has 0.24792) so the numbers match Lightroom's.
const TEMP_TABLE: [[f64; 4]; 31] = [
    [0., 0.18006, 0.26352, -0.24341],
    [10., 0.18066, 0.26589, -0.25479],
    [20., 0.18133, 0.26846, -0.26876],
    [30., 0.18208, 0.27119, -0.28539],
    [40., 0.18293, 0.27407, -0.30470],
    [50., 0.18388, 0.27709, -0.32675],
    [60., 0.18494, 0.28021, -0.35156],
    [70., 0.18611, 0.28342, -0.37915],
    [80., 0.18740, 0.28668, -0.40955],
    [90., 0.18880, 0.28997, -0.44278],
    [100., 0.19032, 0.29326, -0.47888],
    [125., 0.19462, 0.30141, -0.58204],
    [150., 0.19962, 0.30921, -0.70471],
    [175., 0.20525, 0.31647, -0.84901],
    [200., 0.21142, 0.32312, -1.0182],
    [225., 0.21807, 0.32909, -1.2168],
    [250., 0.22511, 0.33439, -1.4512],
    [275., 0.23247, 0.33904, -1.7298],
    [300., 0.24010, 0.34308, -2.0637],
    [325., 0.24702, 0.34655, -2.4681],
    [350., 0.25591, 0.34951, -2.9641],
    [375., 0.26400, 0.35200, -3.5814],
    [400., 0.27218, 0.35407, -4.3633],
    [425., 0.28039, 0.35577, -5.3762],
    [450., 0.28863, 0.35714, -6.7262],
    [475., 0.29685, 0.35823, -8.5955],
    [500., 0.30505, 0.35907, -11.324],
    [525., 0.31320, 0.35968, -15.628],
    [550., 0.32129, 0.36011, -23.325],
    [575., 0.32931, 0.36038, -40.770],
    [600., 0.33724, 0.36051, -116.45],
];

const TINT_SCALE: f64 = -3000.0;

/// White point as Camera Raw shows it: kelvin and tint (green −, magenta +).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempTint {
    pub temperature: f64,
    pub tint: f64,
}

impl TempTint {
    pub fn from_xy(xy: Xy) -> Self {
        let den = 1.5 - xy.x + 6.0 * xy.y;
        let (u, v) = (2.0 * xy.x / den, 3.0 * xy.y / den);
        let (mut last_dt, mut last_du, mut last_dv) = (0.0, 0.0, 0.0);
        for i in 1..TEMP_TABLE.len() {
            let len = (1.0 + TEMP_TABLE[i][3] * TEMP_TABLE[i][3]).sqrt();
            let (mut du, mut dv) = (1.0 / len, TEMP_TABLE[i][3] / len);
            let mut dt = -(u - TEMP_TABLE[i][1]) * dv + (v - TEMP_TABLE[i][2]) * du;
            if dt <= 0.0 || i == TEMP_TABLE.len() - 1 {
                dt = (-dt).max(0.0);
                let f = if i == 1 { 0.0 } else { dt / (last_dt + dt) };
                let (a, b) = (TEMP_TABLE[i - 1], TEMP_TABLE[i]);
                let temperature = 1.0e6 / (a[0] * f + b[0] * (1.0 - f));
                let uu = u - (a[1] * f + b[1] * (1.0 - f));
                let vv = v - (a[2] * f + b[2] * (1.0 - f));
                du = du * (1.0 - f) + last_du * f;
                dv = dv * (1.0 - f) + last_dv * f;
                let len = (du * du + dv * dv).sqrt();
                let tint = (uu * du / len + vv * dv / len) * TINT_SCALE;
                return TempTint { temperature, tint };
            }
            (last_dt, last_du, last_dv) = (dt, du, dv);
        }
        unreachable!()
    }

    pub fn to_xy(self) -> Xy {
        let r = 1.0e6 / self.temperature;
        let offset = self.tint / TINT_SCALE;
        let last = TEMP_TABLE.len() - 2;
        let i = (0..=last).find(|&i| r < TEMP_TABLE[i + 1][0]).unwrap_or(last);
        let (a, b) = (TEMP_TABLE[i], TEMP_TABLE[i + 1]);
        let f = (b[0] - r) / (b[0] - a[0]);
        let mut u = a[1] * f + b[1] * (1.0 - f);
        let mut v = a[2] * f + b[2] * (1.0 - f);
        let (la, lb) = ((1.0 + a[3] * a[3]).sqrt(), (1.0 + b[3] * b[3]).sqrt());
        let du = f / la + (1.0 - f) / lb;
        let dv = a[3] / la * f + b[3] / lb * (1.0 - f);
        let len = (du * du + dv * dv).sqrt();
        u += du / len * offset;
        v += dv / len * offset;
        let den = u - 4.0 * v + 2.0;
        Xy::new(1.5 * u / den, v / den)
    }
}

// ---------------------------------------------------------------------------
// Colour difference

/// CIE XYZ (D65, Y = 1) → CIELAB, D65 white.
pub fn to_lab(xyz: [f32; 3]) -> [f32; 3] {
    let w = D65.to_xyz();
    let f = |t: f32| if t > 0.008_856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let [fx, fy, fz] = [f(xyz[0] / w[0] as f32), f(xyz[1]), f(xyz[2] / w[2] as f32)];
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIEDE2000 colour difference (Sharma, Wu, Dalal 2005).
pub fn delta_e2000(l1: [f32; 3], l2: [f32; 3]) -> f32 {
    let [l1, a1, b1] = l1.map(|v| v as f64);
    let [l2, a2, b2] = l2.map(|v| v as f64);
    let c_bar = (a1.hypot(b1) + a2.hypot(b2)) / 2.0;
    let g = 0.5 * (1.0 - (c_bar.powi(7) / (c_bar.powi(7) + 25f64.powi(7))).sqrt());
    let (a1p, a2p) = (a1 * (1.0 + g), a2 * (1.0 + g));
    let (c1p, c2p) = (a1p.hypot(b1), a2p.hypot(b2));
    let hue = |b: f64, a: f64| if a == 0.0 && b == 0.0 { 0.0 } else { b.atan2(a).to_degrees().rem_euclid(360.0) };
    let (h1p, h2p) = (hue(b1, a1p), hue(b2, a2p));

    let dl = l2 - l1;
    let dc = c2p - c1p;
    let dh = if c1p * c2p == 0.0 {
        0.0
    } else if (h2p - h1p).abs() <= 180.0 {
        h2p - h1p
    } else if h2p - h1p > 180.0 {
        h2p - h1p - 360.0
    } else {
        h2p - h1p + 360.0
    };
    let dh_big = 2.0 * (c1p * c2p).sqrt() * (dh / 2.0).to_radians().sin();

    let l_bar = (l1 + l2) / 2.0;
    let c_bar_p = (c1p + c2p) / 2.0;
    let h_bar = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= 180.0 {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 360.0 {
        (h1p + h2p + 360.0) / 2.0
    } else {
        (h1p + h2p - 360.0) / 2.0
    };
    let t = 1.0 - 0.17 * (h_bar - 30.0).to_radians().cos()
        + 0.24 * (2.0 * h_bar).to_radians().cos()
        + 0.32 * (3.0 * h_bar + 6.0).to_radians().cos()
        - 0.20 * (4.0 * h_bar - 63.0).to_radians().cos();
    let d_theta = 30.0 * (-((h_bar - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (c_bar_p.powi(7) / (c_bar_p.powi(7) + 25f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (l_bar - 50.0).powi(2) / (20.0 + (l_bar - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * c_bar_p;
    let sh = 1.0 + 0.015 * c_bar_p * t;
    let rt = -(2.0 * d_theta).to_radians().sin() * rc;
    let (x, y, z) = (dl / sl, dc / sc, dh_big / sh);
    (x * x + y * y + z * z + rt * y * z).sqrt() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &Mat3, b: &Mat3, eps: f64) -> bool {
        a.iter().flatten().zip(b.iter().flatten()).all(|(x, y)| (x - y).abs() < eps)
    }

    #[test]
    fn invert_round_trips() {
        let m = [[0.7, 0.2, 0.1], [-0.3, 1.2, 0.05], [0.02, -0.1, 0.9]];
        assert!(close(&mul(&m, &invert(&m).unwrap()), &IDENTITY, 1e-12));
    }

    #[test]
    fn prophoto_matches_published_matrix() {
        // ROMM RGB → XYZ (D50), e.g. from Bruce Lindbloom.
        let reference = [[0.7976749, 0.1351917, 0.0313534], [0.2880402, 0.7118741, 0.0000857], [0.0, 0.0, 0.8252100]];
        assert!(close(&prophoto_to_xyz(), &reference, 2e-4));
    }

    #[test]
    fn white_stays_white_between_spaces() {
        let w = mul_vec(&prophoto_to_srgb(), [1.0, 1.0, 1.0]);
        assert!(w.iter().all(|c| (c - 1.0).abs() < 1e-3), "{w:?}");
    }

    #[test]
    fn srgb_transfer_round_trips() {
        for v in [0.0, 0.002, 0.18, 0.5, 1.0] {
            assert!((srgb_eotf(srgb_oetf(v)) - v).abs() < 1e-6);
        }
    }

    #[test]
    fn temperature_matches_camera_raw_scale() {
        let d50 = TempTint::from_xy(D50);
        assert!((d50.temperature - 5003.0).abs() < 20.0, "{d50:?}");
        let d65 = TempTint::from_xy(D65);
        assert!((d65.temperature - 6504.0).abs() < 30.0, "{d65:?}");
        // Points on the Planckian locus have (almost) zero tint.
        let a = TempTint::from_xy(Xy::new(0.44757, 0.40745));
        assert!((a.temperature - 2856.0).abs() < 20.0 && a.tint.abs() < 2.0, "{a:?}");
    }

    #[test]
    fn temperature_tint_round_trips() {
        for (t, tint) in [(2500.0, 0.0), (5600.0, 0.0), (5600.0, 25.0), (7500.0, -40.0), (12000.0, 10.0)] {
            let back = TempTint::from_xy(TempTint { temperature: t, tint }.to_xy());
            assert!((back.temperature - t).abs() < t * 1e-3 && (back.tint - tint).abs() < 0.1, "{t} {tint} -> {back:?}");
        }
    }

    #[test]
    fn matches_sharma_test_data() {
        let cases = [
            ([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485], 2.0425),
            ([50.0, 3.1571, -77.2803], [50.0, 0.0, -82.7485], 2.8615),
            ([50.0, 2.8361, -74.0200], [50.0, 0.0, -82.7485], 3.4412),
            ([50.0, -1.3802, -84.2814], [50.0, 0.0, -82.7485], 1.0000),
            ([50.0, 2.5, 0.0], [73.0, 25.0, -18.0], 27.1492),
            ([2.0776, 0.0795, -1.1350], [0.9033, -0.0636, -0.5514], 0.9082),
        ];
        for (a, b, expected) in cases {
            let d = delta_e2000(a, b);
            assert!((d - expected).abs() < 1e-3, "{a:?} {b:?}: {d} != {expected}");
        }
    }

    #[test]
    fn white_is_lab_100() {
        let to_xyz = to_f32(&rgb_to_xyz(SRGB_PRIMARIES, D65));
        let l = to_lab(apply(&to_xyz, [1.0; 3]));
        assert!((l[0] - 100.0).abs() < 1e-3 && l[1].abs() < 1e-2 && l[2].abs() < 1e-2, "{l:?}");
    }
}
