//! Camera colour: the DNG "camera to XYZ" model (DNG spec, chapter 6), the
//! same maths the DNG SDK and Camera Raw use for the matrix part of a profile.
//!
//! Camera RGB → XYZ D50 depends on the white balance: the matrices are
//! interpolated between the two calibration illuminants by temperature, and
//! the white point is adapted to D50 (or handled by the ForwardMatrix).

use crate::color::{self, Mat3, TempTint, Vec3, Xy};

/// One calibration: a ColorMatrix (XYZ → camera) and, if the profile has one,
/// a ForwardMatrix (white-balanced camera → XYZ D50), measured under a light
/// of the given temperature.
#[derive(Clone, Debug, PartialEq)]
pub struct Calibration {
    /// Correlated colour temperature of the illuminant in kelvin; 0 if unknown.
    pub temperature: f64,
    pub color_matrix: Mat3,
    pub forward_matrix: Option<Mat3>,
    /// Per-camera-unit correction (DNG CameraCalibration), identity if absent.
    pub camera_calibration: Mat3,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColorProfile {
    /// One or two calibrations; with two, the first has the lower temperature.
    pub calibrations: Vec<Calibration>,
    /// DNG AnalogBalance, identity if absent.
    pub analog_balance: Mat3,
}

/// Temperature of the EXIF LightSource codes used by CalibrationIlluminant,
/// with the values of the DNG SDK (`dng_camera_profile::IlluminantToTemperature`).
pub fn illuminant_temperature(code: u16) -> f64 {
    match code {
        3 | 17 => 2850.0,                   // tungsten, standard light A
        24 => 3200.0,                       // ISO studio tungsten
        23 | 13 => 5000.0,                  // D50, day white fluorescent
        1 | 4 | 9 | 18 | 20 => 5500.0,      // daylight, flash, fine weather, B, D55
        10 | 19 | 21 => 6500.0,             // cloudy, C, D65
        11 | 22 => 7500.0,                  // shade, D75
        12 => 6400.0,                       // daylight fluorescent
        2 | 14 => 4200.0,                   // (cool white) fluorescent
        15 => 3450.0,                       // white fluorescent
        16 => 2925.0,                       // warm white fluorescent
        _ => 0.0,
    }
}

impl ColorProfile {
    pub fn new(mut calibrations: Vec<Calibration>, analog_balance: Option<Mat3>) -> Self {
        calibrations.sort_by(|a, b| a.temperature.total_cmp(&b.temperature));
        ColorProfile { calibrations, analog_balance: analog_balance.unwrap_or(color::IDENTITY) }
    }

    /// A profile with a single matrix, as the non-DNG decoders provide.
    pub fn from_color_matrix(color_matrix: Mat3, temperature: f64) -> Self {
        Self::new(
            vec![Calibration { temperature, color_matrix, forward_matrix: None, camera_calibration: color::IDENTITY }],
            None,
        )
    }

    pub fn has_forward_matrix(&self) -> bool {
        self.calibrations.iter().all(|c| c.forward_matrix.is_some())
    }

    /// The calibration for a white point: linear in inverse temperature between
    /// the two illuminants, clamped at the ends.
    fn interpolated(&self, white: Xy) -> Calibration {
        let first = &self.calibrations[0];
        let [lo, hi] = match self.calibrations.as_slice() {
            [a, b] if a.temperature > 0.0 && b.temperature > a.temperature => [a, b],
            _ => return first.clone(),
        };
        let t = TempTint::from_xy(white).temperature;
        let g = if t <= lo.temperature {
            1.0
        } else if t >= hi.temperature {
            0.0
        } else {
            (1.0 / t - 1.0 / hi.temperature) / (1.0 / lo.temperature - 1.0 / hi.temperature)
        };
        Calibration {
            temperature: t,
            color_matrix: color::lerp(&lo.color_matrix, &hi.color_matrix, g),
            forward_matrix: lo.forward_matrix.zip(hi.forward_matrix).map(|(a, b)| color::lerp(&a, &b, g)),
            camera_calibration: color::lerp(&lo.camera_calibration, &hi.camera_calibration, g),
        }
    }

    /// AnalogBalance · CameraCalibration · ColorMatrix: XYZ → camera for `white`.
    fn xyz_to_camera(&self, cal: &Calibration) -> Mat3 {
        color::mul(&self.analog_balance, &color::mul(&cal.camera_calibration, &cal.color_matrix))
    }

    /// The white point whose camera response is `neutral` (DNG AsShotNeutral).
    /// Iterative, because the matrix depends on the white it is solving for.
    pub fn neutral_to_xy(&self, neutral: Vec3) -> Xy {
        let mut xy = color::D50;
        for _ in 0..30 {
            let Some(cam_to_xyz) = color::invert(&self.xyz_to_camera(&self.interpolated(xy))) else {
                break;
            };
            let next = Xy::from_xyz(color::mul_vec(&cam_to_xyz, neutral));
            let done = (next.x - xy.x).abs() + (next.y - xy.y).abs() < 1e-7;
            xy = next;
            if done {
                break;
            }
        }
        xy
    }

    /// Camera response to a white of chromaticity `white`, max component 1.
    pub fn xy_to_neutral(&self, white: Xy) -> Vec3 {
        let n = color::mul_vec(&self.xyz_to_camera(&self.interpolated(white)), white.to_xyz());
        normalize_max(n)
    }

    /// Camera RGB → XYZ D50 when the scene white is `white`. A camera value
    /// equal to [`Self::xy_to_neutral`] (max channel 1) maps to D50 with Y = 1.
    pub fn camera_to_xyz_d50(&self, white: Xy) -> Mat3 {
        let cal = self.interpolated(white);
        let ab_cc = color::mul(&self.analog_balance, &cal.camera_calibration);
        let ab_cc_inv = color::invert(&ab_cc).unwrap_or(color::IDENTITY);

        if let Some(fm) = cal.forward_matrix {
            let neutral = self.xy_to_neutral(white);
            let reference = color::mul_vec(&ab_cc_inv, neutral);
            let d = color::diag(reference.map(|c| 1.0 / c.max(1e-6)));
            return color::mul(&fm, &color::mul(&d, &ab_cc_inv));
        }

        // XYZ(D50) → camera: adapt D50 to the scene white, then the camera matrix.
        let pcs_to_camera = color::mul(&self.xyz_to_camera(&cal), &color::adaptation(color::D50, white));
        let white_response = color::mul_vec(&pcs_to_camera, color::D50.to_xyz());
        let max = white_response.into_iter().fold(f64::MIN, f64::max);
        color::invert(&color::scale(&pcs_to_camera, 1.0 / max)).unwrap_or(color::IDENTITY)
    }
}

pub fn normalize_max(v: Vec3) -> Vec3 {
    let max = v.into_iter().fold(f64::MIN, f64::max);
    if max > 0.0 {
        v.map(|c| c / max)
    } else {
        [1.0; 3]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{mul_vec, D50, D65};

    // Realistic but made-up matrices for a two-illuminant (StdA, D65) profile.
    // Forward matrix rows sum to the D50 white, as the DNG spec requires.
    const CM_A: Mat3 = [[0.8550, -0.3708, 0.0129], [-0.4014, 1.1559, 0.2805], [-0.0442, 0.1159, 0.6436]];
    const CM_D65: Mat3 = [[0.7771, -0.3020, -0.0629], [-0.4029, 1.1950, 0.2345], [-0.0821, 0.1977, 0.6119]];

    fn two_matrix_profile(forward: bool) -> ColorProfile {
        let fm_a = [[0.5925, 0.2563, 0.1155], [0.2193, 0.7787, 0.0020], [0.0331, 0.0007, 0.7913]];
        let fm_d65 = [[0.4836, 0.3825, 0.0982], [0.1773, 0.8530, -0.0303], [0.0213, -0.1306, 0.9343]];
        let cal = |t, cm, fm| Calibration {
            temperature: t,
            color_matrix: cm,
            forward_matrix: forward.then_some(fm),
            camera_calibration: color::IDENTITY,
        };
        // Deliberately out of order: `new` sorts by temperature.
        ColorProfile::new(vec![cal(6500.0, CM_D65, fm_d65), cal(2850.0, CM_A, fm_a)], None)
    }

    fn assert_white(xyz: Vec3) {
        let d50 = D50.to_xyz();
        assert!(xyz.iter().zip(d50).all(|(a, b)| (a - b).abs() < 2e-3), "{xyz:?} != D50 {d50:?}");
    }

    #[test]
    fn neutral_and_xy_are_inverse() {
        for forward in [false, true] {
            let p = two_matrix_profile(forward);
            for white in [D65, D50, TempTint { temperature: 3200.0, tint: 10.0 }.to_xy()] {
                let back = p.neutral_to_xy(p.xy_to_neutral(white));
                assert!((back.x - white.x).abs() < 1e-5 && (back.y - white.y).abs() < 1e-5, "{white:?} -> {back:?}");
            }
        }
    }

    #[test]
    fn scene_white_maps_to_d50_with_and_without_forward_matrix() {
        for forward in [false, true] {
            let p = two_matrix_profile(forward);
            for white in [D65, TempTint { temperature: 3000.0, tint: 0.0 }.to_xy()] {
                let m = p.camera_to_xyz_d50(white);
                assert_white(mul_vec(&m, p.xy_to_neutral(white)));
            }
        }
    }

    #[test]
    fn single_matrix_uses_it_everywhere() {
        let p = ColorProfile::from_color_matrix(CM_D65, 6500.0);
        let n = p.xy_to_neutral(D65);
        assert_white(mul_vec(&p.camera_to_xyz_d50(D65), n));
    }
}
