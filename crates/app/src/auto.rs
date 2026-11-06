//! Automatic adjustments: tone from the histogram ("Auto" in the Basic
//! panel) and white balance from a picked point (the eyedropper).

use raw::color::{self, TempTint, Xy};
use xmp::Basic;

/// Tone settings from the lightness of a render made with the tone
/// sliders at zero. `luma` holds display values (0..1, sRGB encoded).
///
/// A plain heuristic, not Adobe's:
/// - **Exposure** brings the median to 0.45 (a little above mid grey),
///   within ±3 EV;
/// - **Highlights** pull down when more than 3% of the picture is above
///   0.9 after the exposure, **Shadows** lift when more than 15% is below
///   0.15;
/// - **Whites** / **Blacks** stretch the ends to 0.97 / 0.03 when the
///   picture does not reach them (and Whites pull back clipping);
/// - **Contrast** aims at a 10–90% spread of 0.55.
pub fn auto_tone(luma: &[f32]) -> Basic {
    if luma.is_empty() {
        return Basic::default();
    }
    let mut v = luma.to_vec();
    v.sort_by(f32::total_cmp);
    let median = pct(&v, 0.5).max(1e-4);

    let exposure = (color::srgb_eotf(0.45) / color::srgb_eotf(median)).log2().clamp(-3.0, 3.0);
    // The picture after the exposure change (linear gain, display values).
    let gain = 2f32.powf(exposure);
    let shifted: Vec<f32> = v.iter().map(|&x| color::srgb_oetf((color::srgb_eotf(x) * gain).min(1.0))).collect();
    let frac = |f: &dyn Fn(f32) -> bool| shifted.iter().filter(|&&x| f(x)).count() as f32 / shifted.len() as f32;

    let bright = frac(&|x| x > 0.9);
    let dark = frac(&|x| x < 0.15);
    let clipped = frac(&|x| x >= 0.995);
    let highlights = if bright > 0.03 { -(bright * 300.0).min(70.0) } else { 0.0 };
    let shadows = if dark > 0.15 { (dark * 150.0).min(60.0) } else { 0.0 };
    let top = pct(&shifted, 0.995);
    let bottom = pct(&shifted, 0.005);
    let whites = if clipped > 0.01 {
        -(clipped * 1000.0).min(50.0)
    } else if top < 0.97 {
        ((0.97 - top) * 250.0).min(60.0)
    } else {
        0.0
    };
    let blacks = if bottom > 0.03 { -((bottom - 0.03) * 300.0).min(60.0) } else { 0.0 };
    let spread = pct(&shifted, 0.9) - pct(&shifted, 0.1);
    let contrast = ((0.55 - spread) * 120.0).clamp(-20.0, 30.0);

    Basic {
        exposure: (exposure * 100.0).round() / 100.0,
        contrast: contrast.round(),
        highlights: highlights.round(),
        shadows: shadows.round(),
        whites: whites.round(),
        blacks: blacks.round(),
    }
}

/// Value at fraction `p` of sorted values.
fn pct(sorted: &[f32], p: f32) -> f32 {
    sorted[((sorted.len() - 1) as f32 * p).round() as usize]
}

/// Display lightness (Rec.709 weights on the sRGB values) of an 8-bit RGB
/// buffer.
pub fn luma8(rgb: &[u8]) -> Vec<f32> {
    rgb.as_chunks::<3>().0.iter().map(|p| (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0).collect()
}

/// Incremental Temperature/Tint (rendered files) that make a pixel of
/// linear ProPhoto colour `p` neutral: the light is taken to have the
/// pixel's chromaticity, and Camera Raw's relative scale is ±100 mired
/// from D50.
pub fn incremental_white(p: [f32; 3]) -> Option<(f32, f32)> {
    if p.iter().any(|v| *v <= 1e-5) {
        return None;
    }
    let xyz = color::mul_vec(&color::prophoto_to_xyz(), p.map(f64::from));
    let light = TempTint::from_xy(Xy::from_xyz(xyz));
    let d50 = TempTint::from_xy(color::D50);
    let temperature = (1e6 / d50.temperature - 1e6 / light.temperature).clamp(-100.0, 100.0);
    let tint = (light.tint - d50.tint).clamp(-100.0, 100.0);
    Some((temperature.round() as f32, tint.round() as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_pictures_get_brighter() {
        let dark: Vec<f32> = (0..1000).map(|i| 0.05 + 0.25 * i as f32 / 1000.0).collect();
        let b = auto_tone(&dark);
        assert!(b.exposure > 0.5, "{b:?}");
        // The top does not reach white: whites go up.
        assert!(b.whites >= 0.0);
    }

    #[test]
    fn bright_pictures_get_darker_and_keep_highlights() {
        let bright: Vec<f32> = (0..1000).map(|i| 0.5 + 0.5 * i as f32 / 1000.0).collect();
        let b = auto_tone(&bright);
        assert!(b.exposure < 0.0, "{b:?}");
        assert!(b.highlights <= 0.0);
    }

    #[test]
    fn a_balanced_picture_changes_little() {
        // Median at the target, ends just inside black and white.
        let mid: Vec<f32> = (0..1000).map(|i| i as f32 / 999.0).map(|t| if t < 0.5 { 0.03 + 0.84 * t } else { 0.45 + 1.04 * (t - 0.5) }).collect();
        let b = auto_tone(&mid);
        assert!(b.exposure.abs() < 0.3, "{b:?}");
        assert!(b.whites.abs() <= 5.0 && b.blacks.abs() <= 5.0, "{b:?}");
    }

    #[test]
    fn neutral_points_need_no_correction() {
        let (t, n) = incremental_white([0.4, 0.4, 0.4]).unwrap();
        assert!(t.abs() <= 1.0 && n.abs() <= 1.0, "{t} {n}");
        // A warm (orange) point asks for a cooler balance.
        let (t, _) = incremental_white([0.5, 0.4, 0.25]).unwrap();
        assert!(t < -5.0, "{t}");
        assert!(incremental_white([0.0, 0.3, 0.3]).is_none());
    }
}
