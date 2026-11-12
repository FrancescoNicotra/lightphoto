//! Alignment accuracy on synthetic shots with known transforms.

mod common;

use align::{align, AlignError, AlignParams, Model, Plane, Transform};
use common::scene::Scene;

const W: usize = 900;
const H: usize = 600;

/// Reference and moving shot: the moving one has a different gain/offset and
/// its own noise.
fn pair(seed: u64, t: &Transform) -> (Plane, Plane) {
    let scene = Scene::new(seed, W, H);
    let r = scene.render(W, H, &Transform::IDENTITY, 1.0, 0.0, 0.02, seed + 100);
    let m = scene.render(W, H, t, 1.4, 0.15, 0.02, seed + 200);
    (r, m)
}

fn check(name: &str, t: &Transform, model: Model) -> f64 {
    let (r, m) = pair(1, t);
    let p = AlignParams {
        model,
        ..AlignParams::default()
    };
    let a = align(&r, &m, &p).unwrap();
    let err = a.transform.corner_rms(t, W, H);
    eprintln!(
        "{name}: corner RMS {err:.4} px, ρ {:.4}, converged {}, model {:?}",
        a.correlation, a.converged, a.model
    );
    assert!(a.converged, "{name}: not converged");
    assert_eq!(a.model, model, "{name}: fell back");
    assert!(err < 0.1, "{name}: corner RMS {err} px");
    err
}

fn centre() -> (f64, f64) {
    (W as f64 / 2.0, H as f64 / 2.0)
}

#[test]
fn subpixel_translation() {
    let t = Transform::translation(3.3, -7.7);
    check("translation/homography", &t, Model::Homography);
    check("translation/translation", &t, Model::Translation);
}

#[test]
fn rotation() {
    let (cx, cy) = centre();
    let t = Transform::similarity(1.0, 0.4f64.to_radians(), cx, cy, 2.6, -1.3);
    check("rotation/homography", &t, Model::Homography);
    check("rotation/euclidean", &t, Model::Euclidean);
}

#[test]
fn rotation_and_scale() {
    let (cx, cy) = centre();
    let t = Transform::similarity(1.002, -0.4f64.to_radians(), cx, cy, -4.4, 3.1);
    check("similarity/homography", &t, Model::Homography);
    check("similarity/similarity", &t, Model::Similarity);
}

#[test]
fn mild_homography() {
    let (cx, cy) = centre();
    let persp = Transform([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.5e-5, -1e-5, 1.0]]);
    // Perspective about the centre, then a small similarity.
    let about = Transform::translation(cx, cy)
        .compose(&persp)
        .compose(&Transform::translation(-cx, -cy));
    let t = Transform::similarity(1.002, 0.3f64.to_radians(), cx, cy, 5.3, 2.2).compose(&about);
    let shift = t.max_corner_shift(W, H);
    assert!(
        shift > 4.0,
        "the test transform should be non-trivial ({shift})"
    );
    check("homography", &t, Model::Homography);
}

/// A shift near the edge of the coarse search (8% of the long side) plus a
/// rotation: must not be mistaken for a degenerate result.
#[test]
fn large_shift() {
    let (cx, cy) = centre();
    let t = Transform::similarity(1.0, 0.5f64.to_radians(), cx, cy, 72.4, -47.6);
    check("large shift", &t, Model::Homography);
}

/// HDR-like: the brighter moving shot clips; clipped pixels have weight 0 and
/// must not pull the estimate.
#[test]
fn weighted_mask_ignores_clipping() {
    let (cx, cy) = centre();
    let t = Transform::similarity(1.0, 0.25f64.to_radians(), cx, cy, -6.2, 4.7);
    let (r, mut m) = pair(2, &t);
    let clip = {
        let mut v = m.data.clone();
        v.sort_by(f32::total_cmp);
        v[v.len() * 3 / 4]
    };
    let weight = m
        .data
        .iter()
        .map(|&v| if v >= clip { 0.0 } else { 1.0 })
        .collect();
    for v in &mut m.data {
        *v = v.min(clip);
    }
    let m = m.with_weight(weight);
    let a = align(&r, &m, &AlignParams::default()).unwrap();
    let err = a.transform.corner_rms(&t, W, H);
    eprintln!("clipped: corner RMS {err:.4} px, ρ {:.4}", a.correlation);
    assert!(a.converged && err < 0.1, "corner RMS {err} px");
}

/// Two unrelated scenes: the result must be flagged as unreliable.
#[test]
fn unrelated_images_are_rejected() {
    let r = Scene::new(3, W, H).render(W, H, &Transform::IDENTITY, 1.0, 0.0, 0.02, 1);
    let m = Scene::new(4, W, H).render(W, H, &Transform::IDENTITY, 1.0, 0.0, 0.02, 2);
    let a = align(&r, &m, &AlignParams::default()).unwrap();
    eprintln!("unrelated: ρ {:.3}, model {:?}", a.correlation, a.model);
    assert!(!a.converged);
}

#[test]
fn input_errors() {
    let p = AlignParams::default();
    let a = Plane::new(64, 64, vec![0.0; 64 * 64]);
    assert_eq!(
        align(&a, &Plane::new(64, 32, vec![0.0; 64 * 32]), &p).unwrap_err(),
        AlignError::SizeMismatch
    );
    assert_eq!(
        align(&a, &Plane::new(64, 64, vec![0.0; 10]), &p).unwrap_err(),
        AlignError::InvalidPlane
    );
    let tiny = Plane::new(8, 8, vec![0.0; 64]);
    assert_eq!(align(&tiny, &tiny, &p).unwrap_err(), AlignError::TooSmall);
    let flat = Plane::new(64, 64, vec![0.5; 64 * 64]);
    assert_eq!(align(&flat, &flat, &p).unwrap_err(), AlignError::NoOverlap);
}
