//! Lanczos-3 resampling, separable, on linear values.

use rayon::prelude::*;
use raw::Image;

fn lanczos3(x: f32) -> f32 {
    let x = x.abs();
    if x < 1e-6 {
        1.0
    } else if x >= 3.0 {
        0.0
    } else {
        let px = std::f32::consts::PI * x;
        3.0 * px.sin() * (px / 3.0).sin() / (px * px)
    }
}

/// For each output sample, (first source index, normalised weights).
fn weights(src: usize, dst: usize) -> Vec<(usize, Vec<f32>)> {
    let ratio = src as f32 / dst as f32;
    // When shrinking, stretch the kernel to cover the source footprint.
    let scale = ratio.max(1.0);
    let support = 3.0 * scale;
    (0..dst)
        .map(|o| {
            let centre = (o as f32 + 0.5) * ratio - 0.5;
            let lo = ((centre - support).floor() as isize).max(0) as usize;
            let hi = ((centre + support).ceil() as isize).min(src as isize - 1) as usize;
            let mut w: Vec<f32> = (lo..=hi).map(|i| lanczos3((i as f32 - centre) / scale)).collect();
            let sum: f32 = w.iter().sum();
            w.iter_mut().for_each(|v| *v /= sum);
            (lo, w)
        })
        .collect()
}

/// Resizes to exactly `w`×`h`.
pub fn resize(img: &Image, w: usize, h: usize) -> Image {
    if (w, h) == (img.width, img.height) {
        return img.clone();
    }
    let xs = weights(img.width, w);
    let ys = weights(img.height, h);
    let mut tmp = Image::new(w, img.height);
    tmp.pixels.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let src = &img.pixels[y * img.width..(y + 1) * img.width];
        for (out, (lo, wt)) in row.iter_mut().zip(&xs) {
            *out = accumulate(wt.iter().enumerate().map(|(k, &w)| (src[lo + k], w)));
        }
    });
    let mut out = Image::new(w, h);
    out.pixels.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (lo, wt) = &ys[y];
        for (x, px) in row.iter_mut().enumerate() {
            *px = accumulate(wt.iter().enumerate().map(|(k, &wk)| (tmp.pixels[(lo + k) * w + x], wk)));
        }
    });
    out
}

fn accumulate(taps: impl Iterator<Item = ([f32; 3], f32)>) -> [f32; 3] {
    let mut acc = [0.0f32; 3];
    for (p, w) in taps {
        for c in 0..3 {
            acc[c] += p[c] * w;
        }
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_stays_flat() {
        let img = Image::from_fn(97, 61, |_, _| [0.25, 0.5, 0.75]);
        for (w, h) in [(40, 25), (200, 130), (97, 30)] {
            let out = resize(&img, w, h);
            assert_eq!((out.width, out.height), (w, h));
            assert!(out.pixels.iter().all(|p| (p[0] - 0.25).abs() < 1e-4 && (p[2] - 0.75).abs() < 1e-4));
        }
    }

    #[test]
    fn downscale_keeps_mean() {
        let img = Image::from_fn(300, 200, |x, y| [((x / 7 + y / 5) % 2) as f32, 0.0, 0.0]);
        let out = resize(&img, 120, 80);
        let mean = |i: &Image| i.pixels.iter().map(|p| p[0]).sum::<f32>() / i.pixels.len() as f32;
        assert!((mean(&img) - mean(&out)).abs() < 0.01);
    }
}
