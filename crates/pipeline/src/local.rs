//! Spatial operations: Texture, Clarity, Dehaze, sharpening, colour noise
//! reduction, vignette and grain. They run on encoded (Melissa) values, with
//! radii given in pixels of the full-size image and scaled to the image
//! actually being processed.

use rayon::prelude::*;
use raw::Image;

use crate::tone::midtones;

/// ProPhoto luminance weights (Y row of ProPhoto → XYZ).
const LUMA: [f32; 3] = [0.288_040, 0.711_874, 0.000_086];

#[inline]
pub fn luma(p: [f32; 3]) -> f32 {
    LUMA[0] * p[0] + LUMA[1] * p[1] + LUMA[2] * p[2]
}

/// Approximate Gaussian blur of one plane: three box blurs per axis.
pub fn blur(plane: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    if sigma < 0.3 || w == 0 || h == 0 {
        return plane.to_vec();
    }
    // Box width giving the requested sigma after three passes.
    let r = (((12.0 * sigma * sigma / 3.0 + 1.0).sqrt() - 1.0) / 2.0).round().max(1.0) as usize;
    let mut a = plane.to_vec();
    let mut tmp = vec![0.0f32; w * h];
    for _ in 0..3 {
        box_rows(&a, &mut tmp, w, r);
        transpose(&tmp, &mut a, w, h);
        box_rows(&a, &mut tmp, h, r);
        transpose(&tmp, &mut a, h, w);
    }
    a
}

/// Running-sum box filter of radius `r` along rows of width `w`; edges clamp.
fn box_rows(src: &[f32], dst: &mut [f32], w: usize, r: usize) {
    let norm = 1.0 / (2 * r + 1) as f32;
    dst.par_chunks_mut(w).zip(src.par_chunks(w)).for_each(|(out, row)| {
        let at = |i: isize| row[i.clamp(0, w as isize - 1) as usize];
        let mut sum: f32 = (-(r as isize)..=r as isize).map(at).sum();
        for (x, o) in out.iter_mut().enumerate() {
            *o = sum * norm;
            let x = x as isize;
            sum += at(x + r as isize + 1) - at(x - r as isize);
        }
    });
}

fn transpose(src: &[f32], dst: &mut [f32], w: usize, h: usize) {
    dst.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
        for (y, v) in col.iter_mut().enumerate() {
            *v = src[y * w + x];
        }
    });
}

fn plane(img: &Image, f: impl Fn([f32; 3]) -> f32 + Sync) -> Vec<f32> {
    img.pixels.par_iter().map(|p| f(*p)).collect()
}

/// Texture (fine detail) and Clarity (larger-scale local contrast), both
/// −100..100: the luma difference from a blurred copy, boosted or reduced,
/// mostly in the midtones. Radii are relative to the image size, so previews
/// and full-size renders match.
pub fn local_contrast(img: &mut Image, texture: f32, clarity: f32) {
    let long = img.width.max(img.height) as f32;
    let y = plane(img, luma);
    let mut delta = vec![0.0f32; y.len()];
    for (amount, sigma) in [(texture, (long * 0.0015).max(1.0)), (clarity, long * 0.012)] {
        if amount == 0.0 {
            continue;
        }
        let base = blur(&y, img.width, img.height, sigma);
        let k = amount / 100.0 * 0.8;
        delta.par_iter_mut().zip(&y).zip(&base).for_each(|((d, &v), &b)| *d += k * (v - b) * midtones(v));
    }
    img.pixels.par_iter_mut().zip(&delta).for_each(|(p, d)| *p = p.map(|c| c + d));
}

/// Dehaze (−100..100) as a global veil model: negative values blend toward a
/// light haze, positive ones subtract it and restore contrast.
pub fn dehaze(img: &mut Image, amount: f32) {
    let d = amount / 100.0 * 0.3;
    let veil = 0.85;
    img.map(|p| p.map(|v| ((v - d * veil) / (1.0 - d)).clamp(0.0, 1.0)));
}

/// Unsharp mask on luma. `amount` is Sharpening 0..150, `radius` 0.5..3 px,
/// `detail` 0..100 (low values limit halos), `masking` 0..100 (high values
/// only sharpen edges).
pub fn sharpen(img: &mut Image, amount: f32, radius: f32, detail: f32, masking: f32, px: f32) {
    let sigma = radius * px;
    if amount <= 0.0 || sigma < 0.3 {
        return;
    }
    let (w, h) = (img.width, img.height);
    let y = plane(img, luma);
    let base = blur(&y, w, h, sigma);
    let k = amount / 100.0 * 1.2;
    let limit = 0.02 + detail / 100.0 * 0.2;
    let edge_threshold = masking / 100.0 * 0.08;
    let deltas: Vec<f32> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let d = (y[i] - base[i]).clamp(-limit, limit);
            let mask = if edge_threshold > 0.0 {
                let (x, yy) = (i % w, i / w);
                let gx = base[yy * w + (x + 1).min(w - 1)] - base[yy * w + x.saturating_sub(1)];
                let gy = base[(yy + 1).min(h - 1) * w + x] - base[yy.saturating_sub(1) * w + x];
                let g = gx.hypot(gy) / sigma.max(1.0);
                ((g - edge_threshold) / 0.02).clamp(0.0, 1.0)
            } else {
                1.0
            };
            k * d * mask
        })
        .collect();
    img.pixels.par_iter_mut().zip(&deltas).for_each(|(p, d)| *p = p.map(|c| c + d));
}

/// Colour noise reduction (0..100): blurs chroma, keeps luma. One channel
/// at a time, so only a few planes are alive at once.
pub fn color_nr(img: &mut Image, amount: f32, px: f32) {
    let sigma = amount / 100.0 * 6.0 * px;
    if sigma < 0.5 {
        return;
    }
    let (w, h) = (img.width, img.height);
    // Luma is taken once, before any channel changes; each channel then
    // only reads itself, so it can be replaced in place.
    let y = plane(img, luma);
    for c in 0..3 {
        let ch: Vec<f32> = img.pixels.par_iter().zip(&y).map(|(p, y)| p[c] - y).collect();
        let blurred = blur(&ch, w, h, sigma);
        drop(ch);
        img.pixels.par_iter_mut().zip(&blurred).zip(&y).for_each(|((p, b), y)| p[c] = y + b);
    }
}

/// Post-crop vignette. `amount` −100..100 (negative darkens), `midpoint`,
/// `roundness` −100..100 and `feather` 0..100 as in Lightroom.
pub fn vignette(img: &mut Image, amount: f32, midpoint: f32, roundness: f32, feather: f32) {
    if amount == 0.0 {
        return;
    }
    let (w, h) = (img.width as f32, img.height as f32);
    // Roundness 0 follows the frame, +100 is a circle, −100 almost a rectangle.
    let p = 2.0 * 2f32.powf(-roundness.min(0.0) / 100.0);
    let circle = roundness.max(0.0) / 100.0;
    let (sx, sy) = if w > h { (1.0, 1.0 - circle * (1.0 - h / w)) } else { (1.0 - circle * (1.0 - w / h), 1.0) };
    let mid = 0.3 + midpoint / 100.0 * 0.7;
    let soft = 0.05 + feather / 100.0 * 0.6;
    let k = amount / 100.0;
    let width = img.width;
    img.pixels.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let ny = ((y as f32 + 0.5) / h * 2.0 - 1.0) / sy;
        for (x, px) in row.iter_mut().enumerate() {
            let nx = ((x as f32 + 0.5) / w * 2.0 - 1.0) / sx;
            let d = (nx.abs().powf(p) + ny.abs().powf(p)).powf(1.0 / p);
            let t = ((d - (mid - soft)) / (2.0 * soft)).clamp(0.0, 1.0);
            let mask = t * t * (3.0 - 2.0 * t);
            let gain = if k < 0.0 { 1.0 + k * mask } else { 1.0 };
            *px = px.map(|v| v * gain + if k > 0.0 { (1.0 - v) * k * mask } else { 0.0 });
        }
    });
}

/// Film grain: smooth value noise in luma, two octaves mixed by `roughness`
/// (Lightroom's Frequency), strongest in the midtones. Deterministic for a
/// given seed. `size` is in pixels of this image.
pub fn grain(img: &mut Image, amount: f32, size: f32, roughness: f32, seed: u32) {
    if amount <= 0.0 {
        return;
    }
    let cell = 0.6 + size / 100.0 * 3.0;
    let fine = roughness / 100.0;
    let strength = amount / 100.0 * 0.06;
    let width = img.width;
    img.pixels.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        for (x, p) in row.iter_mut().enumerate() {
            let (fx, fy) = (x as f32 / cell, y as f32 / cell);
            let coarse = value_noise(fx, fy, seed);
            let detail = value_noise(fx * 2.3, fy * 2.3, seed ^ 0x9E37_79B9);
            let n = (coarse * (1.0 - fine * 0.5) + detail * fine) * strength;
            let w = 0.25 + midtones(luma(*p));
            *p = p.map(|v| v + n * w);
        }
    });
}

fn hash(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8DA6_B343) ^ (y as u32).wrapping_mul(0xD816_3841) ^ seed.wrapping_mul(0xCB1A_B31F);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0xFFFF) as f32 / 32767.5 - 1.0
}

/// Smoothly interpolated lattice noise in −1..1.
fn value_noise(x: f32, y: f32, seed: u32) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (tx, ty) = (x - x0, y - y0);
    let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
    let (ix, iy) = (x0 as i32, y0 as i32);
    let top = hash(ix, iy, seed) * (1.0 - sx) + hash(ix + 1, iy, seed) * sx;
    let bottom = hash(ix, iy + 1, seed) * (1.0 - sx) + hash(ix + 1, iy + 1, seed) * sx;
    top * (1.0 - sy) + bottom * sy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_preserves_flat_and_mean() {
        let flat = vec![0.4f32; 40 * 30];
        assert!(blur(&flat, 40, 30, 3.0).iter().all(|v| (v - 0.4).abs() < 1e-5));
        let ramp: Vec<f32> = (0..40 * 30).map(|i| (i % 40) as f32 / 40.0).collect();
        let b = blur(&ramp, 40, 30, 2.0);
        // Interior of a linear ramp is unchanged by a symmetric blur.
        assert!((b[15 * 40 + 20] - ramp[15 * 40 + 20]).abs() < 1e-4);
    }

    #[test]
    fn flat_image_is_unchanged_by_local_ops() {
        let mut img = Image::from_fn(50, 40, |_, _| [0.5, 0.4, 0.3]);
        let orig = img.clone();
        local_contrast(&mut img, 50.0, 50.0);
        sharpen(&mut img, 80.0, 1.0, 50.0, 0.0, 1.0);
        color_nr(&mut img, 50.0, 1.0);
        for (a, b) in img.pixels.iter().zip(&orig.pixels) {
            assert!(a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4), "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn grain_is_deterministic_and_zero_mean() {
        let mut a = Image::from_fn(200, 200, |_, _| [0.5; 3]);
        let mut b = a.clone();
        grain(&mut a, 50.0, 25.0, 50.0, 7);
        grain(&mut b, 50.0, 25.0, 50.0, 7);
        assert_eq!(a, b);
        let mean = a.pixels.iter().map(|p| p[0]).sum::<f32>() / a.pixels.len() as f32;
        assert!((mean - 0.5).abs() < 0.01, "{mean}");
        assert!(a.pixels.iter().any(|p| (p[0] - 0.5).abs() > 0.01));
    }

    #[test]
    fn vignette_darkens_corners_only() {
        let mut img = Image::from_fn(60, 40, |_, _| [0.5; 3]);
        vignette(&mut img, -60.0, 50.0, 0.0, 50.0);
        assert!(img.get(0, 0)[0] < 0.35);
        assert!((img.get(30, 20)[0] - 0.5).abs() < 1e-3);
    }
}
