//! Merging synthetic brackets in memory: radiance, dynamic range, noise and
//! exposure estimation (plan/HDR.md §8.2).

mod common;

use common::scene::{Scene, ShotSpec};
use common::{block_log_error, memory_sources, stats, sum, truth};
use merge::{prepare, AlignStatus, Control, EvSource, MergeOptions, Warning};
use raw::Image;

const W: usize = 960;
const H: usize = 640;

fn no_align() -> MergeOptions {
    MergeOptions { align: false, ..MergeOptions::default() }
}

fn merge_memory(scene: &Scene, specs: &[ShotSpec], opts: &MergeOptions) -> (merge::Merged, Image) {
    let sources = memory_sources(scene, specs, W, H);
    let prepared = prepare(sources, opts, &Control::default()).unwrap();
    let merged = prepared.merge(&Control::default()).unwrap();
    let t = truth(scene, W, H, None);
    (merged, t)
}

/// Shots with exposures `evs` (EV), the reference at 0.
fn bracket(evs: &[f64]) -> Vec<ShotSpec> {
    evs.iter().enumerate().map(|(i, &ev)| ShotSpec::new(ev.exp2(), 100 + i as u64)).collect()
}

fn max3(p: [f32; 3]) -> f32 {
    p[0].max(p[1]).max(p[2])
}

/// Checks radiance (block means, where the darkest shot is not clipped),
/// and returns the error.
fn check_radiance(out: &Image, t: &Image, e_min: f64, what: &str) -> f64 {
    let (err, blocks) = block_log_error(out, t, 8, |x, y| (max3(t.get(x, y)) as f64) * e_min < 0.9);
    eprintln!("{what}: radiance error {err:.4} EV over {blocks} blocks");
    assert!(blocks > 1000, "{what}: {blocks} blocks");
    assert!(err < 0.02, "{what}: radiance error {err} EV");
    err
}

#[test]
fn three_shots_radiance_range_and_noise() {
    let scene = Scene::new(1, W, H);
    let specs = bracket(&[-2.0, 0.0, 2.0]);
    let sources = memory_sources(&scene, &specs, W, H);
    let reference_shot = match &sources[1] {
        merge::Source::Memory { capture, .. } => capture.data.clone(),
        _ => unreachable!(),
    };
    let prepared = prepare(sources, &no_align(), &Control::default()).unwrap();
    assert_eq!(prepared.reference, 1);
    assert!(prepared.frames.iter().all(|f| f.alignment == AlignStatus::Disabled || f.alignment == AlignStatus::Reference));
    for (f, ev) in prepared.frames.iter().zip([-2.0, 0.0, 2.0]) {
        eprintln!("{}: {:+.4} EV ({:?})", f.name, f.ev, f.ev_source);
        assert!((f.ev - ev).abs() < 0.03, "{}: {} EV", f.name, f.ev);
    }
    assert!(prepared.baseline_noise < 1.0 && prepared.baseline_noise > 0.2, "{}", prepared.baseline_noise);
    let merged = prepared.merge(&Control::default()).unwrap();
    let out = &merged.image;
    let t = truth(&scene, W, H, None);
    check_radiance(out, &t, 0.25, "3 shots");

    // Dynamic range: pixels clipped in the reference but not in the darkest shot.
    let (mut total, mut ok) = (0usize, 0usize);
    for i in 0..W * H {
        let (x, y) = (i % W, i / W);
        let r = t.get(x, y);
        if max3(reference_shot.pixels[i]) >= 0.98 && (max3(r) as f64) * 0.25 < 0.85 {
            total += 1;
            if (sum(out.pixels[i]) / sum(r)).log2().abs() < 0.05 {
                ok += 1;
            }
        }
    }
    let share = ok as f64 / total as f64;
    eprintln!("recovered {ok} of {total} clipped reference pixels ({:.3}%)", 100.0 * share);
    assert!(total > 10_000 && share > 0.99, "{share}");

    // Noise in the flat shadow patch.
    let (px0, py0, px1, py1, _) = scene.patch;
    let rect = (px0 as usize + 2, py0 as usize + 2, px1 as usize - 2, py1 as usize - 2);
    let (m_out, s_out) = stats(out, rect.0, rect.1, rect.2, rect.3);
    let (m_ref, s_ref) = stats(&reference_shot, rect.0, rect.1, rect.2, rect.3);
    let gain = (m_out / s_out) / (m_ref / s_ref);
    eprintln!("shadow SNR: merge {:.1}, reference {:.1}, gain {gain:.2}", m_out / s_out, m_ref / s_ref);
    assert!(gain >= 0.8 * 4f64.sqrt(), "SNR gain {gain}");
}

#[test]
fn five_and_nine_shots() {
    let scene = Scene::new(2, W, H);
    for evs in [vec![-2.0, -1.0, 0.0, 1.0, 2.0], vec![-4.0, -3.0, -2.0, -1.0, 0.0, 1.0, 2.0, 3.0, 4.0]] {
        // Shuffled order: the reference is still the median exposure.
        let mut specs = bracket(&evs);
        specs.reverse();
        specs.swap(0, 2);
        let (merged, t) = merge_memory(&scene, &specs, &no_align());
        let r = merged.frames.iter().position(|f| f.ev_source == EvSource::Reference).unwrap();
        assert!((specs[r].exposure - 1.0).abs() < 1e-9, "reference {r}");
        for (f, s) in merged.frames.iter().zip(&specs) {
            assert!((f.ev - s.exposure.log2()).abs() < 0.03, "{}: {} vs {}", f.name, f.ev, s.exposure.log2());
        }
        check_radiance(&merged.image, &t, evs[0].exp2(), &format!("{} shots", evs.len()));
    }
}

#[test]
fn exposures_without_exif() {
    let scene = Scene::new(3, W, H);
    let mut specs = bracket(&[-2.0, 0.0, 2.0]);
    specs.iter_mut().for_each(|s| s.exif_time = None);
    let prepared = prepare(memory_sources(&scene, &specs, W, H), &no_align(), &Control::default()).unwrap();
    assert!(prepared.warnings.iter().any(|w| matches!(w, Warning::MissingExif(v) if v.len() == 3)));
    for (f, s) in prepared.frames.iter().zip(&specs) {
        let err = (f.ev - s.exposure.log2()).abs();
        eprintln!("{}: {:+.4} EV, error {err:.4}", f.name, f.ev);
        assert!(err < 0.03, "{}: error {err} EV", f.name);
        assert_eq!(f.exif_ev, None);
    }
}

#[test]
fn nominal_exif_is_corrected_from_the_pixels() {
    let scene = Scene::new(4, W, H);
    // True times 1/128, 1/32, 1/8 s; EXIF says 1/125, 1/30, 1/8: the
    // nominal ratios are 0.06 and 0.09 EV off.
    let specs: Vec<ShotSpec> = [(128.0, 125.0), (32.0, 30.0), (8.0, 8.0)]
        .iter()
        .enumerate()
        .map(|(i, &(t, nominal))| ShotSpec { exif_time: Some(1.0 / nominal), ..ShotSpec::new(32.0 / t, 10 + i as u64) })
        .collect();
    let prepared = prepare(memory_sources(&scene, &specs, W, H), &no_align(), &Control::default()).unwrap();
    for (f, s) in prepared.frames.iter().zip(&specs) {
        let err = (f.ev - s.exposure.log2()).abs();
        eprintln!("{}: {:+.4} EV (EXIF {:+.4}), error {err:.4}", f.name, f.ev, f.exif_ev.unwrap());
        assert!(err < 0.02, "{}: error {err} EV", f.name);
    }
    // The nominal values are under 0.3 EV off: no warning for that.
    assert!(!prepared.warnings.iter().any(|w| matches!(w, Warning::ExifMismatch { .. })));
}

#[test]
fn wrong_exif_warns_and_same_exposure_warns() {
    let scene = Scene::new(5, W, H);
    let mut specs = bracket(&[-2.0, 0.0, 2.0]);
    specs[2].exif_time = Some(1.0 / 100.0); // claims 0 EV, is +2
    let prepared = prepare(memory_sources(&scene, &specs, W, H), &no_align(), &Control::default()).unwrap();
    assert!(prepared.warnings.iter().any(|w| matches!(w, Warning::ExifMismatch { frame, .. } if frame == "shot2")), "{:?}", prepared.warnings);

    let specs = bracket(&[0.0, 0.0, 0.0]);
    let (merged, t) = merge_memory(&scene, &specs, &no_align());
    assert!(merged.warnings.contains(&Warning::SameExposure), "{:?}", merged.warnings);
    check_radiance(&merged.image, &t, 1.0, "same exposure");
}

#[test]
fn two_shots_use_the_darker_as_reference() {
    let scene = Scene::new(6, W, H);
    let specs = vec![ShotSpec::new(4.0, 1), ShotSpec::new(1.0, 2)];
    let (merged, t) = merge_memory(&scene, &specs, &no_align());
    assert_eq!(merged.frames[1].ev_source, EvSource::Reference);
    check_radiance(&merged.image, &t, 1.0, "2 shots");
}

#[test]
fn everything_clipped_stays_neutral() {
    let scene = Scene::new(7, W, H);
    let (merged, _) = merge_memory(&scene, &bracket(&[-2.0, 0.0, 2.0]), &no_align());
    let (lx, ly, _) = scene.light;
    let p = merged.image.get(lx as usize, ly as usize);
    let n = common::scene::NEUTRAL;
    let b = [p[0] as f64 / n[0], p[1] as f64 / n[1], p[2] as f64 / n[2]];
    eprintln!("light source: {p:?}, balanced {b:?}");
    assert!(b[0] > 3.0 && (b[0] - b[1]).abs() < 1e-3 * b[0] && (b[2] - b[1]).abs() < 1e-3 * b[0], "{b:?}");
}
