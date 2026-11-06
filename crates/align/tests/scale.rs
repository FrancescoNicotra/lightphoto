//! Half-size → full resolution with the pixel-centre convention.

mod common;

use align::{align, AlignParams, Transform};
use common::scene::{bin2, Scene};

/// Half-size pixel `(i, j)` is the 2×2 average of full pixels `2i, 2i+1`,
/// whose common corner `(2i + 1, 2j + 1)` is the half pixel's centre. A
/// transform estimated at half size must therefore map that point to where the
/// full-resolution transform does.
#[test]
fn half_to_full_matches_binning_geometry() {
    // Deliberately large, so a half-pixel convention slip would show.
    let full = Transform([[1.15, -0.18, 12.3], [0.17, 1.12, -7.9], [2e-5, -3e-5, 1.0]]);
    let half = full.scaled(0.5);
    assert!(half.half_to_full().corner_rms(&full, 3000, 2000) < 1e-9);
    for (i, j) in [(0usize, 0usize), (10, 3), (750, 500), (1499, 999)] {
        // Half pixel centre in half coordinates, and the same point at full size.
        let (hx, hy) = (i as f64 + 0.5, j as f64 + 0.5);
        let (fx, fy) = ((2 * i + 1) as f64, (2 * j + 1) as f64);
        let (a, b) = half.half_to_full().apply(fx, fy);
        let (c, d) = half.apply(hx, hy);
        assert!((a - 2.0 * c).abs() < 1e-9 && (b - 2.0 * d).abs() < 1e-9);
    }
    // The classic mistake, scaling index coordinates (centres at integers), is
    // visibly off for this transform: x_full = 2·x_half + 0.5 in index terms.
    let idx = Transform::translation(0.5, 0.5); // index → edge coordinates
    let to_idx = Transform::translation(-0.5, -0.5);
    let half_idx = to_idx.compose(&half).compose(&idx);
    let naive = idx.compose(&half_idx.scaled(2.0)).compose(&to_idx);
    assert!(naive.corner_rms(&full, 3000, 2000) > 0.05);
}

/// End to end: full-size shots with a known transform, binned to half size,
/// aligned there and scaled back up.
#[test]
fn half_size_alignment_scales_to_full() {
    let (w, h) = (1200, 800);
    let scene = Scene::new(9, w, h);
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let t_full = Transform::similarity(1.02, 1.5f64.to_radians(), cx, cy, 7.3, -5.9);
    let r = bin2(&scene.render(w, h, &Transform::IDENTITY, 1.0, 0.0, 0.0, 0));
    let m = bin2(&scene.render(w, h, &t_full, 0.8, 0.1, 0.0, 0));
    let a = align(&r, &m, &AlignParams::default()).unwrap();
    assert!(a.converged);
    let err = a.transform.half_to_full().corner_rms(&t_full, w, h);
    eprintln!("half → full: corner RMS {err:.4} px (full resolution)");
    assert!(err < 0.05, "corner RMS {err} px");
}
