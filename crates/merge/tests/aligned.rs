//! Hand-held brackets: known transforms, recovered at full resolution.

mod common;

use align::Transform;
use common::scene::{Scene, ShotSpec};
use common::{block_log_error, memory_sources, truth};
use merge::{prepare, AlignStatus, Control, MergeOptions, Warning};

const W: usize = 960;
const H: usize = 640;

fn hand_held() -> Vec<ShotSpec> {
    let (cx, cy) = (W as f64 / 2.0, H as f64 / 2.0);
    vec![
        ShotSpec { transform: Transform::translation(3.3, -7.7), ..ShotSpec::new(0.25, 1) },
        ShotSpec::new(1.0, 2),
        ShotSpec { transform: Transform::similarity(1.002, 0.4f64.to_radians(), cx, cy, -2.6, 4.1), ..ShotSpec::new(4.0, 3) },
    ]
}

#[test]
fn hand_held_bracket_is_aligned() {
    let scene = Scene::new(11, W, H);
    let specs = hand_held();
    let opts = MergeOptions { auto_crop: true, ..MergeOptions::default() };
    let prepared = prepare(memory_sources(&scene, &specs, W, H), &opts, &Control::default()).unwrap();
    assert!(prepared.warnings.is_empty(), "{:?}", prepared.warnings);
    for (f, s) in prepared.frames.iter().zip(&specs) {
        let err = f.transform.corner_rms(&s.transform, W, H);
        eprintln!("{}: {:?}, corner RMS {err:.4} px, {:+.4} EV\n  {:?}", f.name, f.alignment, f.ev, f.transform);
        assert!(err < 0.25, "{}: corner RMS {err} px", f.name);
        assert!((f.ev - s.exposure.log2()).abs() < 0.03);
    }
    assert!(matches!(prepared.frames[0].alignment, AlignStatus::Aligned { .. }));

    // The crop leaves out the borders the shifted shots do not cover.
    let crop = prepared.crop.expect("auto crop");
    eprintln!("crop {crop:?}");
    assert!(crop.top * H as f32 >= 7.0 && crop.left > 0.0 && crop.right < 1.0 && crop.bottom < 1.0, "{crop:?}");
    let aspect = (crop.right - crop.left) * W as f32 / ((crop.bottom - crop.top) * H as f32);
    assert!((aspect - 1.5).abs() < 0.03, "{aspect}");

    let merged = prepared.merge(&Control::default()).unwrap();
    let t = truth(&scene, W, H, None);
    let margin = 16;
    let (err, n) = block_log_error(&merged.image, &t, 8, |x, y| {
        let r = t.get(x, y);
        x >= margin && y >= margin && x < W - margin && y < H - margin && (r[0].max(r[1]).max(r[2]) as f64) * 0.25 < 0.9
    });
    eprintln!("aligned radiance error {err:.4} EV over {n} blocks");
    assert!(err < 0.02, "{err}");
}

#[test]
fn unrelated_shot_fails_alignment_with_a_warning() {
    let scene = Scene::new(12, W, H);
    let other = Scene::new(99, W, H);
    let mut sources = memory_sources(&scene, &[ShotSpec::new(0.25, 1), ShotSpec::new(1.0, 2)], W, H);
    // The same light, but a different texture: alignment cannot match it.
    let alien = common::capture(&other, &ShotSpec { transform: Transform::translation(40.0, 25.0), ..ShotSpec::new(4.0, 3) }, W, H);
    sources.push(merge::Source::Memory { name: "alien".into(), capture: std::sync::Arc::new(alien) });
    let prepared = prepare(sources, &MergeOptions::default(), &Control::default());
    match prepared {
        Ok(p) => {
            eprintln!("{:?}", p.warnings);
            assert!(p.warnings.contains(&Warning::AlignmentFailed("alien".into())), "{:?}", p.frames[2].alignment);
            assert_eq!(p.frames[2].alignment, AlignStatus::Failed);
            assert_eq!(p.frames[2].transform, Transform::IDENTITY);
            // Ghost removal is forced to Medium even though it was off.
            assert_eq!(p.ghosts.as_ref().map(|g| g.level), Some(merge::Deghost::Medium));
        }
        // Too few pixels agree for an exposure ratio: also acceptable.
        Err(merge::MergeError::NoExposure(v)) => assert_eq!(v, vec!["alien".to_string()]),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn perspective_is_kept_when_it_fits_better() {
    let scene = Scene::new(13, W, H);
    let persp = Transform([[1.001, 0.002, 2.4], [-0.001, 0.999, -1.7], [1.2e-5, -8e-6, 1.0]]);
    let specs = vec![ShotSpec::new(0.5, 1), ShotSpec::new(1.0, 2), ShotSpec { transform: persp, ..ShotSpec::new(2.0, 3) }];
    let opts = MergeOptions::default();
    let prepared = prepare(memory_sources(&scene, &specs, W, H), &opts, &Control::default()).unwrap();
    let f = &prepared.frames[2];
    let err = f.transform.corner_rms(&persp, W, H);
    eprintln!("perspective: {:?}, corner RMS {err:.4} px (corners move {:.1} px)", f.alignment, persp.max_corner_shift(W, H));
    assert!(matches!(f.alignment, AlignStatus::Aligned { model: align::Model::Homography, .. }));
    assert!(err < 0.25, "{err}");
}
