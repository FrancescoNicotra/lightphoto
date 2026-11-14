//! Demosaic quality on a real photo: the camera's own JPEG, linearised, is
//! taken as a full-colour ground truth, turned into a Bayer mosaic, rebuilt
//! by each method and compared with the original (the usual Kodak-style
//! evaluation). PSNR is computed on square-root values, roughly how the
//! eye weighs errors, away from the borders.

use raw::demosaic::{self, Cfa};
use raw::{rcd, DecodeOptions, Image, Photo};

fn ground_truth() -> Image {
    let path = test_samples::gx80().camera_jpeg;
    // Photo::load gives linear ProPhoto; back to linear sRGB-primaries is not
    // needed: any linear RGB works as ground truth.
    Photo::load(&path, &DecodeOptions::default()).unwrap().to_working(None)
}

fn psnr(a: &Image, b: &Image, border: usize) -> f64 {
    let (w, h) = (a.width, a.height);
    let mut sum = 0.0f64;
    let mut n = 0usize;
    for y in border..h - border {
        for x in border..w - border {
            let (p, q) = (a.get(x, y), b.get(x, y));
            for c in 0..3 {
                sum += (p[c].max(0.0).sqrt() - q[c].max(0.0).sqrt()).powi(2) as f64;
            }
            n += 3;
        }
    }
    -10.0 * (sum / n as f64).log10()
}

#[test]
fn rcd_beats_bilinear_on_a_real_photo() {
    let truth = ground_truth();
    let (w, h) = (truth.width, truth.height);
    for name in ["RGGB", "BGGR"] {
        let cfa = Cfa::bayer(name).unwrap();
        let mosaic: Vec<f32> = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| truth.get(x, y)[cfa.color_at(x, y)]).collect();
        let bilinear = demosaic::bilinear(&mosaic, w, h, &cfa);
        let rcd = rcd::rcd_region(&mosaic, w, h, &cfa, [0, 0, w, h], [1.0; 3]).unwrap();
        let (pb, pr) = (psnr(&bilinear, &truth, 8), psnr(&rcd, &truth, 8));
        eprintln!("{name}: bilinear {pb:.2} dB, RCD {pr:.2} dB (+{:.2})", pr - pb);
        assert!(pr > pb + 3.0, "{name}: bilinear {pb} RCD {pr}");
    }
}

/// Malvar–He–Cutler (2004) gradient-corrected linear interpolation, as a
/// published reference point: its 5×5 kernels are exact, so a correct RCD
/// should at least match it.
fn mhc(m: &[f32], w: usize, h: usize, cfa: &Cfa) -> Image {
    let at = |x: isize, y: isize| m[(y.clamp(0, h as isize - 1) as usize) * w + x.clamp(0, w as isize - 1) as usize];
    let k = |x: usize, y: usize, taps: &[(isize, isize, f32)]| taps.iter().map(|&(dx, dy, c)| c * at(x as isize + dx, y as isize + dy)).sum::<f32>() / 8.0;
    let g_at_rb = [(0, 0, 4.0), (0, -1, 2.0), (0, 1, 2.0), (-1, 0, 2.0), (1, 0, 2.0), (0, -2, -1.0), (0, 2, -1.0), (-2, 0, -1.0), (2, 0, -1.0)];
    let row_neighbours = [(0, 0, 5.0), (-1, 0, 4.0), (1, 0, 4.0), (-2, 0, -1.0), (2, 0, -1.0), (-1, -1, -1.0), (1, -1, -1.0), (-1, 1, -1.0), (1, 1, -1.0), (0, -2, 0.5), (0, 2, 0.5)];
    let col_neighbours: Vec<(isize, isize, f32)> = row_neighbours.iter().map(|&(dx, dy, c)| (dy, dx, c)).collect();
    let opposite = [(0, 0, 6.0), (-1, -1, 2.0), (1, -1, 2.0), (-1, 1, 2.0), (1, 1, 2.0), (0, -2, -1.5), (0, 2, -1.5), (-2, 0, -1.5), (2, 0, -1.5)];
    Image::from_fn(w, h, |x, y| {
        let own = cfa.color_at(x, y);
        let v = m[y * w + x];
        let mut p = [0.0f32; 3];
        p[own] = v;
        if own == 1 {
            // Which colour sits left/right of this green?
            let row_colour = cfa.color_at(x + 1, y);
            p[row_colour] = k(x, y, &row_neighbours);
            p[2 - row_colour] = k(x, y, &col_neighbours);
        } else {
            p[1] = k(x, y, &g_at_rb);
            p[2 - own] = k(x, y, &opposite);
        }
        p.map(|c| c.max(0.0))
    })
}

#[test]
fn rcd_matches_or_beats_the_published_reference() {
    let full = ground_truth();
    // Halving averages away the camera's own demosaic and JPEG artefacts:
    // closer to the clean images such methods are usually measured on.
    for (label, truth) in [("full", full.clone()), ("half", full.downscale(full.width.max(full.height) / 2))] {
        let (w, h) = (truth.width, truth.height);
        let cfa = Cfa::bayer("RGGB").unwrap();
        let mosaic: Vec<f32> = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| truth.get(x, y)[cfa.color_at(x, y)]).collect();
        let pb = psnr(&demosaic::bilinear(&mosaic, w, h, &cfa), &truth, 8);
        let pm = psnr(&mhc(&mosaic, w, h, &cfa), &truth, 8);
        let pr = psnr(&rcd::rcd_region(&mosaic, w, h, &cfa, [0, 0, w, h], [1.0; 3]).unwrap(), &truth, 8);
        eprintln!("{label}: bilinear {pb:.2}, Malvar–He–Cutler {pm:.2}, RCD {pr:.2} dB");
        assert!(pr > pm, "{label}: MHC {pm} RCD {pr}");
    }
}

/// The channel means stay those of the truth: RCD alone would shift them by
/// about 1% in this foliage (a faint cast); bilinear is exact by design.
#[test]
fn rcd_keeps_the_mean_colour() {
    let truth = ground_truth();
    let (w, h) = (truth.width, truth.height);
    let cfa = Cfa::bayer("BGGR").unwrap();
    let mosaic: Vec<f32> = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| truth.get(x, y)[cfa.color_at(x, y)]).collect();
    let rcd = rcd::rcd_region(&mosaic, w, h, &cfa, [0, 0, w, h], [1.0; 3]).unwrap();
    let mean = |img: &Image| -> [f64; 3] { std::array::from_fn(|c| img.pixels.iter().map(|p| p[c] as f64).sum::<f64>() / img.pixels.len() as f64) };
    let (t, r) = (mean(&truth), mean(&rcd));
    eprintln!("means: truth {t:.5?}, RCD {r:.5?}");
    assert!(t.iter().zip(r).all(|(a, b)| (a - b).abs() / a < 0.003), "{t:?} vs {r:?}");
}
