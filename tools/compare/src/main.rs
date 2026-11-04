//! Measures how close our render is to a reference image (a Lightroom export
//! or the preview embedded in a DNG): ΔE2000 statistics and a difference map.
//!
//! usage: compare <ours.jpg> <reference.jpg> [--diff diff.png]

use std::process::ExitCode;

use rayon::prelude::*;
use raw::color::{self, D65};
use raw::Image;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let diff = args.iter().position(|a| a == "--diff").and_then(|i| args.get(i + 1)).cloned();
    let files: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| !a.starts_with("--") && (*i == 0 || args[i - 1] != "--diff"))
        .map(|(_, a)| a)
        .collect();
    if files.len() != 2 {
        eprintln!("usage: compare <ours.jpg> <reference.jpg> [--diff diff.png]");
        return ExitCode::from(2);
    }
    match run(files[0], files[1], diff.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// An sRGB file as linear sRGB.
fn load(path: &str) -> Result<Image, Box<dyn std::error::Error>> {
    let img = image::open(path)?.into_rgb32f();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let src = img.as_raw();
    Ok(Image::from_fn(w, h, |x, y| {
        let i = (y * w + x) * 3;
        [src[i], src[i + 1], src[i + 2]].map(color::srgb_eotf)
    }))
}

fn run(ours: &str, reference: &str, diff: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let (a, b) = (load(ours)?, load(reference)?);
    let aspect = |i: &Image| i.width as f64 / i.height as f64;
    if (aspect(&a) / aspect(&b) - 1.0).abs() > 0.01 {
        eprintln!("warning: aspect ratios differ ({:.3} vs {:.3}); the images are stretched to match", aspect(&a), aspect(&b));
    }
    // Compare at the smaller of the two sizes.
    let (w, h) = if a.width * a.height <= b.width * b.height { (a.width, a.height) } else { (b.width, b.height) };
    let (a, b) = (export::resize::resize(&a, w, h), export::resize::resize(&b, w, h));

    let to_xyz = color::to_f32(&color::rgb_to_xyz(color::SRGB_PRIMARIES, D65));
    let lab = |p: [f32; 3]| to_lab(color::apply(&to_xyz, p.map(|v| v.clamp(0.0, 1.0))));
    let pairs: Vec<([f32; 3], [f32; 3])> = a.pixels.par_iter().zip(&b.pixels).map(|(p, q)| (lab(*p), lab(*q))).collect();
    let mut de: Vec<f32> = pairs.par_iter().map(|(p, q)| delta_e2000(*p, *q)).collect();

    let mean_lab = |k: usize| -> [f64; 3] {
        let n = pairs.len() as f64;
        std::array::from_fn(|c| pairs.iter().map(|pq| if k == 0 { pq.0[c] } else { pq.1[c] } as f64).sum::<f64>() / n)
    };
    let (la, lb) = (mean_lab(0), mean_lab(1));

    let mut sorted = de.clone();
    sorted.sort_by(f32::total_cmp);
    let pct = |p: f64| sorted[((sorted.len() - 1) as f64 * p) as usize];
    let mean = de.iter().map(|v| *v as f64).sum::<f64>() / de.len() as f64;

    println!("compared at {w} × {h}");
    println!("ΔE2000   mean {mean:.2}   median {:.2}   p95 {:.2}   max {:.2}", pct(0.5), pct(0.95), pct(1.0));
    println!("mean Lab ours      L {:.1}  a {:+.1}  b {:+.1}", la[0], la[1], la[2]);
    println!("mean Lab reference L {:.1}  a {:+.1}  b {:+.1}", lb[0], lb[1], lb[2]);
    println!("global ΔE2000 of the means {:.2}", delta_e2000(la.map(|v| v as f32), lb.map(|v| v as f32)));

    if let Some(path) = diff {
        // 0 → black, ΔE 10 or more → white.
        let buf: Vec<u8> = de.iter_mut().map(|d| (*d / 10.0 * 255.0).clamp(0.0, 255.0) as u8).collect();
        image::GrayImage::from_raw(w as u32, h as u32, buf).ok_or("buffer size")?.save(path)?;
        println!("wrote {path}");
    }
    Ok(())
}

/// CIE XYZ (D65, Y = 1) → CIELAB.
fn to_lab(xyz: [f32; 3]) -> [f32; 3] {
    let w = D65.to_xyz();
    let f = |t: f32| if t > 0.008_856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let [fx, fy, fz] = [f(xyz[0] / w[0] as f32), f(xyz[1]), f(xyz[2] / w[2] as f32)];
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIEDE2000 colour difference (Sharma, Wu, Dalal 2005).
fn delta_e2000(l1: [f32; 3], l2: [f32; 3]) -> f32 {
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
        let to_xyz = color::to_f32(&color::rgb_to_xyz(color::SRGB_PRIMARIES, D65));
        let l = to_lab(color::apply(&to_xyz, [1.0; 3]));
        assert!((l[0] - 100.0).abs() < 1e-3 && l[1].abs() < 1e-2 && l[2].abs() < 1e-2, "{l:?}");
    }
}
