//! Warp exactness and coverage.

use align::{warp_plane, warp_rgb, Interp, Transform};

fn image(w: usize, h: usize) -> Vec<[f32; 3]> {
    (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            [
                (x * 0.37).sin() + y * 0.01,
                (x * y * 0.003).cos(),
                ((x + 2.0 * y) * 0.21).sin().abs(),
            ]
        })
        .collect()
}

#[test]
fn identity_is_exact() {
    let (w, h) = (37, 23);
    let src = image(w, h);
    let (out, cov) = warp_rgb(&src, w, h, &Transform::IDENTITY, w, h);
    assert_eq!(out, src);
    assert!(cov.iter().all(|&c| c));

    let plane: Vec<f32> = src.iter().map(|p| p[0]).collect();
    for interp in [Interp::Bilinear, Interp::CatmullRom, Interp::Max] {
        let (out, cov) = warp_plane(&plane, w, h, &Transform::IDENTITY, w, h, interp);
        assert_eq!(out, plane, "{interp:?}");
        assert!(cov.iter().all(|&c| c));
    }
}

#[test]
fn integer_translation_and_coverage() {
    let (w, h) = (40, 30);
    let (dx, dy) = (7usize, 4usize);
    let src = image(w, h);
    // Reference point p shows the source at p + (dx, dy).
    let t = Transform::translation(dx as f64, dy as f64);
    let (out, cov) = warp_rgb(&src, w, h, &t, w, h);
    for y in 0..h {
        for x in 0..w {
            let inside = x + dx < w && y + dy < h;
            assert_eq!(cov[y * w + x], inside, "coverage at ({x}, {y})");
            if inside {
                assert_eq!(out[y * w + x], src[(y + dy) * w + x + dx]);
            }
        }
    }
    assert_eq!(cov.iter().filter(|&&c| c).count(), (w - dx) * (h - dy));

    // Negative shift: the uncovered band is on the other side.
    let t = Transform::translation(-3.0, 0.0);
    let (out, cov) = warp_rgb(&src, w, h, &t, w, h);
    for y in 0..h {
        for x in 0..w {
            assert_eq!(cov[y * w + x], x >= 3);
            if x >= 3 {
                assert_eq!(out[y * w + x], src[y * w + x - 3]);
            }
        }
    }
}

#[test]
fn fractional_coverage() {
    let (w, h) = (50, 20);
    let plane = vec![1.0f32; w * h];
    // Centre of output column x goes to x + 0.5 + 10.5: inside while ≤ w.
    let (_, cov) = warp_plane(
        &plane,
        w,
        h,
        &Transform::translation(10.5, 0.0),
        w,
        h,
        Interp::CatmullRom,
    );
    assert_eq!(cov.iter().filter(|&&c| c).count(), (w - 10) * h);
    // A larger output frame: everything beyond the source is uncovered.
    let (out, cov) = warp_plane(
        &plane,
        w,
        h,
        &Transform::IDENTITY,
        w + 5,
        h + 2,
        Interp::Bilinear,
    );
    assert_eq!(cov.iter().filter(|&&c| c).count(), w * h);
    assert!(out
        .iter()
        .zip(&cov)
        .all(|(&v, &c)| if c { v == 1.0 } else { v == 0.0 }));
}

#[test]
fn catmull_rom_does_not_ring() {
    // A hard step (like clipping): no overshoot after a fractional shift.
    let (w, h) = (32, 8);
    let plane: Vec<f32> = (0..w * h)
        .map(|i| if i % w < 16 { 0.1 } else { 1.0 })
        .collect();
    let t = Transform::similarity(1.0, 0.05, 16.0, 4.0, 0.37, 0.21);
    let (out, cov) = warp_plane(&plane, w, h, &t, w, h, Interp::CatmullRom);
    for (&v, &c) in out.iter().zip(&cov) {
        if c {
            assert!((0.1..=1.0).contains(&v), "overshoot {v}");
        }
    }
    // ...while still being sharper than bilinear on smooth data.
    let ramp: Vec<f32> = (0..w * h).map(|i| ((i % w) as f32 * 0.3).sin()).collect();
    let t = Transform::translation(0.5, 0.0);
    let (cubic, _) = warp_plane(&ramp, w, h, &t, w, h, Interp::CatmullRom);
    let (lin, _) = warp_plane(&ramp, w, h, &t, w, h, Interp::Bilinear);
    let truth = |x: usize| ((x as f32 + 0.5) * 0.3).sin();
    let err = |v: &[f32]| {
        (2..w - 3)
            .map(|x| (v[x] - truth(x)).abs())
            .fold(0.0, f32::max)
    };
    assert!(
        err(&cubic) < 0.5 * err(&lin),
        "cubic {} vs bilinear {}",
        err(&cubic),
        err(&lin)
    );
}

#[test]
fn max_keeps_mask_pixels() {
    let (w, h) = (16, 16);
    let mut mask = vec![0.0f32; w * h];
    mask[8 * w + 8] = 1.0;
    let (out, _) = warp_plane(
        &mask,
        w,
        h,
        &Transform::translation(0.5, 0.5),
        w,
        h,
        Interp::Max,
    );
    // The flagged pixel survives at full strength in the four outputs around it.
    for (x, y) in [(7, 7), (8, 7), (7, 8), (8, 8)] {
        assert_eq!(out[y * w + x], 1.0, "({x}, {y})");
    }
    assert_eq!(out.iter().filter(|&&v| v > 0.0).count(), 4);
}
