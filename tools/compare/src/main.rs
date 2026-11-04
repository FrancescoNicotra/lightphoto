//! Measures how close our render is to a reference image (a Lightroom export
//! or the preview embedded in a DNG): ΔE2000 statistics and a difference map.
//!
//! usage: compare <ours.jpg> <reference.jpg> [--diff diff.png] [--size N]
//!
//! `--size N` compares at N pixels on the long edge (area average), which
//! averages out random grain and sharpening.

use std::process::ExitCode;

use rayon::prelude::*;
use raw::color::{self, delta_e2000, to_lab, D65};
use raw::Image;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let diff = value("--diff");
    let size = value("--size").and_then(|v| v.parse::<usize>().ok());
    let files: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| !a.starts_with("--") && (*i == 0 || !args[i - 1].starts_with("--")))
        .map(|(_, a)| a)
        .collect();
    if files.len() != 2 {
        eprintln!("usage: compare <ours.jpg> <reference.jpg> [--diff diff.png] [--size N]");
        return ExitCode::from(2);
    }
    match run(files[0], files[1], diff.as_deref(), size) {
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

fn run(ours: &str, reference: &str, diff: Option<&str>, size: Option<usize>) -> Result<(), Box<dyn std::error::Error>> {
    let (mut a, mut b) = (load(ours)?, load(reference)?);
    if let Some(n) = size {
        (a, b) = (a.downscale(n), b.downscale(n));
    }
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

    // Where the differences are: by lightness of the reference, and by its
    // hue (for colourful pixels), with mean ours − reference.
    println!("\n{:<18} {:>6} {:>7} {:>7} {:>7} {:>7} {:>7}", "reference", "share", "ΔE", "ΔL", "Δa", "Δb", "ΔC");
    let row = |label: String, sel: &dyn Fn(&[f32; 3]) -> bool| {
        let idx: Vec<usize> = (0..pairs.len()).filter(|&i| sel(&pairs[i].1)).collect();
        if idx.is_empty() {
            return;
        }
        let n = idx.len() as f64;
        let avg = |f: &dyn Fn(usize) -> f64| idx.iter().map(|&i| f(i)).sum::<f64>() / n;
        let chroma = |p: &[f32; 3]| (p[1] as f64).hypot(p[2] as f64);
        println!(
            "{label:<18} {:>5.1}% {:>7.2} {:>+7.1} {:>+7.1} {:>+7.1} {:>+7.1}",
            100.0 * n / pairs.len() as f64,
            avg(&|i| de[i] as f64),
            avg(&|i| (pairs[i].0[0] - pairs[i].1[0]) as f64),
            avg(&|i| (pairs[i].0[1] - pairs[i].1[1]) as f64),
            avg(&|i| (pairs[i].0[2] - pairs[i].1[2]) as f64),
            avg(&|i| chroma(&pairs[i].0) - chroma(&pairs[i].1)),
        );
    };
    for lo in (0..100).step_by(20) {
        row(format!("L* {lo}–{}", lo + 20), &|p: &[f32; 3]| p[0] >= lo as f32 && p[0] < (lo + 20) as f32);
    }
    // Lab hue angles; red straddles 0°.
    let bands: [(&str, f64, f64); 6] = [
        ("red", -30.0, 30.0),
        ("orange/yellow", 30.0, 100.0),
        ("green", 100.0, 160.0),
        ("aqua", 160.0, 220.0),
        ("blue", 220.0, 300.0),
        ("magenta", 300.0, 330.0),
    ];
    for (name, h0, h1) in bands {
        row(format!("hue {name}"), &|p: &[f32; 3]| {
            let h = (p[2] as f64).atan2(p[1] as f64).to_degrees(); // −180..180
            let h = if h0 < 0.0 { h } else { h.rem_euclid(360.0) };
            (p[1] as f64).hypot(p[2] as f64) > 10.0 && h >= h0 && h < h1
        });
    }

    if let Some(path) = diff {
        // 0 → black, ΔE 10 or more → white.
        let buf: Vec<u8> = de.iter_mut().map(|d| (*d / 10.0 * 255.0).clamp(0.0, 255.0) as u8).collect();
        image::GrayImage::from_raw(w as u32, h as u32, buf).ok_or("buffer size")?.save(path)?;
        println!("wrote {path}");
    }
    Ok(())
}
