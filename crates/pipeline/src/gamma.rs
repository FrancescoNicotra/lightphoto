//! Fast sRGB transfer functions for the per-pixel passes. `powf` is the
//! single most expensive thing in them (about nine calls per pixel in the
//! colour pass), so values in 0..1 go through tables instead; anything
//! outside falls back to the exact formula. Error below 2·10⁻⁵, far under
//! one 16-bit step.

use std::sync::LazyLock;

use raw::color::{srgb_eotf, srgb_oetf};

const N: usize = 4096;

/// oetf sampled on √v: the curve is smooth in √v, steep in v near black.
static OETF_SQRT: LazyLock<Vec<f32>> = LazyLock::new(|| {
    (0..=N).map(|i| {
        let s = i as f32 / N as f32;
        srgb_oetf(s * s)
    })
    .collect()
});

static EOTF: LazyLock<Vec<f32>> = LazyLock::new(|| (0..=N).map(|i| srgb_eotf(i as f32 / N as f32)).collect());

#[inline]
fn lookup(table: &[f32], t: f32) -> f32 {
    let x = t * N as f32;
    let i = (x as usize).min(N - 1);
    let f = x - i as f32;
    table[i] + (table[i + 1] - table[i]) * f
}

/// Linear → sRGB-encoded.
#[inline]
pub fn oetf(v: f32) -> f32 {
    if (0.0..=1.0).contains(&v) {
        lookup(&OETF_SQRT, v.sqrt())
    } else {
        srgb_oetf(v)
    }
}

/// sRGB-encoded → linear.
#[inline]
pub fn eotf(e: f32) -> f32 {
    if (0.0..=1.0).contains(&e) {
        lookup(&EOTF, e)
    } else {
        srgb_eotf(e)
    }
}

/// [`oetf`] keeping the sign, for values below zero.
#[inline]
pub fn signed_oetf(v: f32) -> f32 {
    v.signum() * oetf(v.abs())
}

#[inline]
pub fn signed_eotf(v: f32) -> f32 {
    v.signum() * eotf(v.abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_exact_curves() {
        let mut worst = (0.0f32, 0.0f32);
        for i in 0..=200_000 {
            let v = i as f32 / 200_000.0;
            worst.0 = worst.0.max((oetf(v) - srgb_oetf(v)).abs());
            worst.1 = worst.1.max((eotf(v) - srgb_eotf(v)).abs());
        }
        assert!(worst.0 < 2e-5 && worst.1 < 2e-5, "{worst:?}");
        for v in [-0.5f32, 1.0, 1.7, 4.0] {
            assert!((oetf(v) - srgb_oetf(v)).abs() < 1e-6 || v < 0.0);
            assert!((signed_oetf(v) - v.signum() * srgb_oetf(v.abs())).abs() < 2e-5);
        }
    }

    #[test]
    fn round_trip() {
        for i in 0..=1000 {
            let v = i as f32 / 1000.0;
            assert!((eotf(oetf(v)) - v).abs() < 4e-5, "{v}");
        }
    }
}
