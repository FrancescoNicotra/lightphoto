//! Timings at realistic sizes. Ignored by default; run with
//! `cargo test --release -p align --test timing -- --ignored --nocapture`.

mod common;

use std::time::Instant;

use align::{align, warp_rgb, AlignParams, Transform};
use common::scene::Scene;

fn run(w: usize, h: usize) {
    let scene = Scene::new(5, w, h);
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let t = Transform::similarity(1.002, 0.4f64.to_radians(), cx, cy, 13.3, -27.7);
    let r = scene.render(w, h, &Transform::IDENTITY, 1.0, 0.0, 0.02, 1);
    let m = scene.render(w, h, &t, 1.3, 0.1, 0.02, 2);

    let start = Instant::now();
    let a = align(&r, &m, &AlignParams::default()).unwrap();
    let t_align = start.elapsed();
    let err = a.transform.corner_rms(&t, w, h);

    let rgb: Vec<[f32; 3]> = m.data.iter().map(|&v| [v, 0.5 * v, 1.0 - v]).collect();
    let start = Instant::now();
    let (out, _) = warp_rgb(&rgb, w, h, &a.transform, w, h);
    let t_warp = start.elapsed();
    std::hint::black_box(out);

    eprintln!(
        "{w}×{h}: align {:.0} ms (corner RMS {err:.4} px, ρ {:.4}), warp_rgb {:.0} ms",
        t_align.as_secs_f64() * 1e3,
        a.correlation,
        t_warp.as_secs_f64() * 1e3
    );
    assert!(a.converged && err < 0.1);
}

#[test]
#[ignore = "timing; run explicitly"]
fn timings() {
    run(1500, 1000);
    run(3000, 2000);
}
