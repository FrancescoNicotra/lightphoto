//! Hue/saturation/value maps: the ProfileHueSatMap and ProfileLookTable of
//! DNG camera profiles, and the LookTable of Camera Raw looks such as
//! "Adobe Color". Applied as in the DNG SDK (`RefBaselineHueSatMap`): RGB is
//! taken to HSV, the table gives a hue shift in degrees and scale factors
//! for saturation and value, trilinearly interpolated. Checked against the
//! SDK's `dng_reference.cpp`.

/// Entries are (hue shift in degrees, saturation scale, value scale), stored
/// value-major, then hue, then saturation (saturation varies fastest).
#[derive(Clone, Debug, PartialEq)]
pub struct HueSatMap {
    pub hue_divisions: usize,
    pub sat_divisions: usize,
    /// 1 for 2D maps that do not depend on value.
    pub val_divisions: usize,
    pub data: Vec<[f32; 3]>,
    /// The value axis is indexed with sRGB-encoded values instead of linear.
    pub srgb_encoded: bool,
}

impl HueSatMap {
    /// Builds a map from flat floats (h, s, v triples) and checks the size.
    pub fn new(dims: [usize; 3], floats: &[f32], srgb_encoded: bool) -> Option<HueSatMap> {
        let [h, s, v] = dims;
        let v = v.max(1);
        if h == 0 || s < 2 || floats.len() != h * s * v * 3 {
            return None;
        }
        let data = floats.as_chunks::<3>().0.to_vec();
        Some(HueSatMap { hue_divisions: h, sat_divisions: s, val_divisions: v, data, srgb_encoded })
    }

    /// Entry-wise blend: `t` of `self`, `1 − t` of `other` (same dimensions).
    pub fn lerp(&self, other: &HueSatMap, t: f64) -> HueSatMap {
        let t = t as f32;
        let data = self
            .data
            .iter()
            .zip(&other.data)
            .map(|(a, b)| std::array::from_fn(|i| a[i] * t + b[i] * (1.0 - t)))
            .collect();
        HueSatMap { data, ..self.clone() }
    }

    pub fn same_shape(&self, other: &HueSatMap) -> bool {
        (self.hue_divisions, self.sat_divisions, self.val_divisions)
            == (other.hue_divisions, other.sat_divisions, other.val_divisions)
    }

    /// Applies the map to one linear RGB pixel (ProPhoto, as Camera Raw does).
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        // Greys go through too: the saturation-0 column can scale value.
        let (h, s, v) = rgb_to_hsv(rgb);
        let hs = self.hue_divisions as f32 / 6.0;
        let h_scaled = h * hs;
        let s_scaled = s * (self.sat_divisions - 1) as f32;
        let mut h0 = h_scaled as usize;
        let s0 = (s_scaled as usize).min(self.sat_divisions - 2);
        let mut h1 = h0 + 1;
        if h0 >= self.hue_divisions - 1 {
            h0 = self.hue_divisions - 1;
            h1 = 0;
        }
        let hf = h_scaled - h0 as f32;
        let sf = s_scaled - s0 as f32;

        let encoded = self.srgb_encoded && self.val_divisions >= 2;
        let ve = if encoded { crate::color::srgb_oetf(v.clamp(0.0, 1.0)) } else { v };
        let (v0, vf) = if self.val_divisions < 2 {
            (0, 0.0)
        } else {
            let vs = ve.clamp(0.0, 1.0) * (self.val_divisions - 1) as f32;
            let v0 = (vs as usize).min(self.val_divisions - 2);
            (v0, vs - v0 as f32)
        };

        let at = |vi: usize, hi: usize, si: usize| self.data[(vi * self.hue_divisions + hi) * self.sat_divisions + si];
        let mut e = [0.0f32; 3];
        let vis: &[(usize, f32)] = if self.val_divisions < 2 { &[(0, 1.0)] } else { &[(v0, 1.0 - vf), (v0 + 1, vf)] };
        for &(vi, vw) in vis {
            for (hi, hw) in [(h0, 1.0 - hf), (h1, hf)] {
                for (si, sw) in [(s0, 1.0 - sf), (s0 + 1, sf)] {
                    let w = vw * hw * sw;
                    if w == 0.0 {
                        continue;
                    }
                    let d = at(vi, hi, si);
                    for k in 0..3 {
                        e[k] += d[k] * w;
                    }
                }
            }
        }
        let h = h + e[0] * (6.0 / 360.0);
        let s = (s * e[1]).min(1.0);
        // As the SDK, the scale acts on the encoded value, pinned to 0..1.
        // Values above 1 (which the SDK never sees here) keep their level.
        let ve = (ve * e[2]).clamp(0.0, ve.max(1.0));
        let v = if encoded { crate::color::srgb_eotf(ve) } else { ve };
        hsv_to_rgb(h, s, v)
    }
}

/// DNG SDK HSV: hue in 0..6 (hexcone sextants), saturation and value.
pub fn rgb_to_hsv([r, g, b]: [f32; 3]) -> (f32, f32, f32) {
    let v = r.max(g).max(b);
    let gap = v - r.min(g).min(b);
    if gap <= 0.0 || v <= 0.0 {
        return (0.0, 0.0, v);
    }
    let mut h = if r == v {
        (g - b) / gap
    } else if g == v {
        2.0 + (b - r) / gap
    } else {
        4.0 + (r - g) / gap
    };
    if h < 0.0 {
        h += 6.0;
    }
    (h, gap / v, v)
}

pub fn hsv_to_rgb(mut h: f32, s: f32, v: f32) -> [f32; 3] {
    if s <= 0.0 {
        return [v; 3];
    }
    h = h.rem_euclid(6.0);
    let i = (h as usize).min(5);
    let f = h - i as f32;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(dims: [usize; 3]) -> HueSatMap {
        let n = dims[0] * dims[1] * dims[2].max(1);
        let floats: Vec<f32> = (0..n).flat_map(|_| [0.0, 1.0, 1.0]).collect();
        HueSatMap::new(dims, &floats, false).unwrap()
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn hsv_round_trips() {
        for p in [[0.8, 0.2, 0.1], [0.1, 0.5, 0.9], [0.3, 0.3, 0.3], [0.2, 0.9, 0.4], [0.9, 0.1, 0.7]] {
            let (h, s, v) = rgb_to_hsv(p);
            assert!(close(hsv_to_rgb(h, s, v), p), "{p:?}");
        }
    }

    #[test]
    fn identity_maps_are_transparent() {
        for dims in [[90, 30, 1], [36, 8, 16]] {
            let m = identity(dims);
            for p in [[0.8, 0.2, 0.1], [0.1, 0.5, 0.9], [0.05, 0.04, 0.02], [0.99, 0.6, 0.0]] {
                assert!(close(m.apply(p), p), "{dims:?} {p:?} -> {:?}", m.apply(p));
            }
        }
    }

    #[test]
    fn hue_shift_and_saturation_scale() {
        // Every entry: +60° (one sextant) and half saturation.
        let dims = [6, 4, 1];
        let floats: Vec<f32> = (0..24).flat_map(|_| [60.0, 0.5, 1.0]).collect();
        let m = HueSatMap::new(dims, &floats, false).unwrap();
        let out = m.apply([1.0, 0.0, 0.0]); // pure red, s = 1
        let (h, s, v) = rgb_to_hsv(out);
        assert!((h - 1.0).abs() < 1e-4 && (s - 0.5).abs() < 1e-4 && (v - 1.0).abs() < 1e-5, "{out:?}");
    }

    #[test]
    fn wrong_sizes_are_rejected() {
        assert!(HueSatMap::new([36, 8, 16], &[0.0; 10], false).is_none());
    }
}
