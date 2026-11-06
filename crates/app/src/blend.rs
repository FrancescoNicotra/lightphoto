//! Preset strength (the Amount slider, 0–200%): every numeric setting moves
//! between the settings before the preset and the settings with it, as in
//! Lightroom's preset Amount.
//!
//! The settings go through their XMP form (`DevelopSettings::write_into`),
//! so every scalar the typed settings know is covered without listing the
//! fields twice. Point curves are blended as functions; colour grading hues
//! along the shorter arc; the profile (Look) through its own amount; what
//! cannot be scaled (RGB tables) comes with the preset as soon as the amount
//! is above zero.

use pipeline::tone;
use xmp::{Curve, DevelopSettings, Props, Value, WhiteBalance, NS_CRS};

/// `before` and `after` blended at `t` (0 = before, 1 = after, up to 2).
/// `as_shot` is the camera white balance (temperature, tint), used when one
/// side leaves the white balance "As Shot".
pub fn blend(before: &DevelopSettings, after: &DevelopSettings, t: f32, as_shot: Option<(f32, f32)>) -> DevelopSettings {
    let t = t.clamp(0.0, 2.0);
    if t == 1.0 {
        return after.clone();
    }
    if t == 0.0 {
        return before.clone();
    }
    let (mut pb, mut pa) = (Props::default(), Props::default());
    before.write_into(&mut pb);
    after.write_into(&mut pa);

    let mut blended = Props::default();
    for p in pa.iter().filter(|p| p.ns == NS_CRS) {
        let key = p.name.as_str();
        if key == "ProcessVersion" || key == "Temperature" || key == "Tint" {
            continue;
        }
        let Some(b) = pb.crs(key) else { continue };
        let value = if key.starts_with("ToneCurvePV2012") {
            let (Some(cb), Some(ca)) = (curve(b), curve(&p.value)) else { continue };
            curve_value(&blend_curve(&cb, &ca, t))
        } else {
            let (Some(vb), Some(va)) = (b.as_f32(), p.value.as_f32()) else { continue };
            let v = match hue_partner(key) {
                Some(sat_key) => blend_hue(vb, va, t, pb.crs(sat_key).and_then(Value::as_f32).unwrap_or(0.0)),
                None => {
                    let (lo, hi) = range(key);
                    (vb + (va - vb) * t).clamp(lo, hi)
                }
            };
            Value::Text(xmp::fmt_num(v))
        };
        blended.set_crs(key, value);
    }

    let mut out = after.clone();
    out.apply(&blended);
    // The curves keep the preset's name ("Custom"…) unless both are linear.
    if [&out.curves.master, &out.curves.red, &out.curves.green, &out.curves.blue].iter().all(|c| c.is_identity()) {
        out.curves.name = "Linear".into();
    }

    // White balance: absolute values for raw files, blended when either
    // side sets one.
    if before.white_balance != after.white_balance || before.temperature != after.temperature || before.tint != after.tint {
        let side = |s: &DevelopSettings| match (&s.white_balance, s.temperature, s.tint) {
            (WhiteBalance::Custom, Some(t), tint) => Some((t, tint.unwrap_or(0.0))),
            (WhiteBalance::AsShot, ..) => as_shot,
            _ => None,
        };
        match (side(before), side(after)) {
            (Some((tb, nb)), Some((ta, na))) => {
                // Even steps in mired, as the temperature slider.
                let mired = 1e6 / tb + (1e6 / ta - 1e6 / tb) * t;
                out.white_balance = WhiteBalance::Custom;
                out.temperature = Some((1e6 / mired.max(20.0)).clamp(2000.0, 50000.0).round());
                out.tint = Some((nb + (na - nb) * t).clamp(-150.0, 150.0).round());
            }
            _ if t < 0.5 => {
                out.white_balance = before.white_balance.clone();
                out.temperature = before.temperature;
                out.tint = before.tint;
            }
            _ => {}
        }
    }

    // The profile scales with its own amount when the preset brings it.
    if after.look != before.look {
        if let Some(look) = &mut out.look {
            look.amount = (look.amount * t).clamp(0.0, 2.0);
        }
    }
    out
}

/// Slider limits, so 200% does not push a value off its scale.
fn range(key: &str) -> (f32, f32) {
    const POSITIVE: &[&str] = &[
        "Sharpness", "SharpenDetail", "SharpenEdgeMasking", "LuminanceSmoothing", "ColorNoiseReduction",
        "ColorNoiseReductionDetail", "ColorNoiseReductionSmoothness", "GrainAmount", "GrainSize", "GrainFrequency",
        "PostCropVignetteMidpoint", "PostCropVignetteFeather", "SplitToningShadowSaturation", "SplitToningHighlightSaturation",
        "ColorGradeMidtoneSat", "ColorGradeGlobalSat", "ColorGradeBlending",
    ];
    match key {
        "Exposure2012" => (-5.0, 5.0),
        "Sharpness" => (0.0, 150.0),
        "SharpenRadius" => (0.5, 3.0),
        "ParametricShadowSplit" | "ParametricMidtoneSplit" | "ParametricHighlightSplit" => (5.0, 95.0),
        k if POSITIVE.contains(&k) => (0.0, 100.0),
        _ => (-100.0, 100.0),
    }
}

/// Colour grading hues and the saturation that gives them meaning.
fn hue_partner(key: &str) -> Option<&'static str> {
    Some(match key {
        "SplitToningShadowHue" => "SplitToningShadowSaturation",
        "SplitToningHighlightHue" => "SplitToningHighlightSaturation",
        "ColorGradeMidtoneHue" => "ColorGradeMidtoneSat",
        "ColorGradeGlobalHue" => "ColorGradeGlobalSat",
        _ => return None,
    })
}

/// Hues turn along the shorter arc; a hue whose saturation was zero
/// before had no colour, so the preset's hue is taken as is.
fn blend_hue(before: f32, after: f32, t: f32, before_sat: f32) -> f32 {
    if before_sat == 0.0 {
        return after;
    }
    let d = (after - before + 540.0).rem_euclid(360.0) - 180.0;
    (before + d * t.min(1.0)).rem_euclid(360.0).round()
}

fn curve(v: &Value) -> Option<Curve> {
    let pts: Vec<(f32, f32)> = v
        .items()
        .iter()
        .filter_map(|i| {
            let (x, y) = i.as_text()?.split_once(',')?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        })
        .collect();
    (pts.len() >= 2).then_some(Curve(pts))
}

fn curve_value(c: &Curve) -> Value {
    Value::Seq(c.0.iter().map(|(x, y)| Value::Text(format!("{}, {}", xmp::fmt_num(*x), xmp::fmt_num(*y)))).collect())
}

/// Blends two point curves as functions: at every point of either curve
/// the output moves from one curve's value to the other's.
pub fn blend_curve(before: &Curve, after: &Curve, t: f32) -> Curve {
    if before == after {
        return after.clone();
    }
    let (fb, fa) = (tone::spline(before), tone::spline(after));
    let mut xs: Vec<f32> = before.0.iter().chain(&after.0).map(|p| p.0.round()).collect();
    xs.sort_by(f32::total_cmp);
    xs.dedup();
    Curve(
        xs.into_iter()
            .map(|x| {
                let (yb, ya) = (fb.get(x / 255.0) * 255.0, fa.get(x / 255.0) * 255.0);
                (x, (yb + (ya - yb) * t).clamp(0.0, 255.0).round())
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dreamy() -> (DevelopSettings, DevelopSettings) {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../samples/Dreamy days in spring.xmp");
        let preset = xmp::Preset::load(path).unwrap();
        let before = DevelopSettings::default();
        let mut after = before.clone();
        after.try_apply(&preset.packet.props).unwrap();
        (before, after)
    }

    #[test]
    fn ends_are_exact() {
        let (b, a) = dreamy();
        assert_eq!(blend(&b, &a, 0.0, None), b);
        assert_eq!(blend(&b, &a, 1.0, None), a);
    }

    #[test]
    fn half_way_and_beyond() {
        let (b, a) = dreamy();
        let half = blend(&b, &a, 0.5, Some((4877.0, 13.0)));
        assert_eq!(half.basic.contrast, a.basic.contrast / 2.0);
        assert_eq!(half.basic.shadows, a.basic.shadows / 2.0);
        assert_eq!(half.effects.grain_amount, (a.effects.grain_amount / 2.0).round());
        // Between the camera's white balance and the preset's.
        let t = half.temperature.unwrap();
        assert!(t > 4877.0 && t < a.temperature.unwrap(), "{t}");
        // The profile is applied at half strength.
        assert_eq!(half.look.as_ref().unwrap().amount, 0.5);

        let double = blend(&b, &a, 2.0, Some((4877.0, 13.0)));
        assert_eq!(double.basic.contrast, (a.basic.contrast * 2.0).clamp(-100.0, 100.0));
        assert!(double.basic.exposure.abs() <= 5.0);
        // Re-reading the written settings gives the same values.
        let mut props = Props::default();
        double.write_into(&mut props);
        let mut back = double.clone();
        back.apply(&props);
        assert_eq!(back, double);
    }

    #[test]
    fn curves_blend_as_functions() {
        let linear = Curve::default();
        let s = Curve(vec![(0.0, 0.0), (64.0, 40.0), (192.0, 215.0), (255.0, 255.0)]);
        let half = blend_curve(&linear, &s, 0.5);
        let at = |c: &Curve, x: f32| tone::spline(c).get(x / 255.0) * 255.0;
        assert!((at(&half, 64.0) - 52.0).abs() < 1.5, "{:?}", half.0);
        assert!((at(&half, 192.0) - 203.5).abs() < 1.5, "{:?}", half.0);
        assert_eq!(blend_curve(&linear, &s, 1.0).0.len(), 4);
    }

    #[test]
    fn hues_take_the_short_way() {
        assert_eq!(blend_hue(350.0, 10.0, 0.5, 20.0), 0.0);
        assert_eq!(blend_hue(0.0, 200.0, 0.5, 0.0), 200.0);
    }
}
