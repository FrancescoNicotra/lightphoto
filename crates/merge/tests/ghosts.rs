//! Ghost removal on a disk that moves between the shots (plan §8.2): the
//! detected mask matches the disk positions, the output shows a single disk,
//! and a static scene gives (almost) no mask.

mod common;

use align::Transform;
use common::scene::{Mover, Scene, ShotSpec};
use common::{memory_sources, sum, truth, write_dng};
use merge::{prepare, Control, Deghost, MergeOptions, Source};
use raw::Image;

const W: usize = 960;
const H: usize = 640;
const RADIUS: f64 = 56.0;

fn positions() -> [(f64, f64); 3] {
    let y = 0.42 * H as f64;
    [(0.27 * W as f64, y), (0.34 * W as f64, y + 20.0), (0.41 * W as f64, y)]
}

fn moving_scene(seed: u64) -> Scene {
    let mut s = Scene::new(seed, W, H);
    s.mover = Some(Mover { radius: RADIUS, level: 0.01 });
    s
}

fn specs(transforms: bool) -> Vec<ShotSpec> {
    let p = positions();
    let (cx, cy) = (W as f64 / 2.0, H as f64 / 2.0);
    let t = |i: usize| match (transforms, i) {
        (true, 0) => Transform::translation(2.2, -3.6),
        (true, 2) => Transform::similarity(1.0, 0.2f64.to_radians(), cx, cy, -1.4, 2.5),
        _ => Transform::IDENTITY,
    };
    [0.25, 1.0, 4.0]
        .iter()
        .enumerate()
        .map(|(i, &e)| ShotSpec { mover: Some(p[i]), transform: t(i), ..ShotSpec::new(e, 40 + i as u64) })
        .collect()
}

/// Half-size truth: union of the disk positions.
fn true_mask(w: usize, h: usize) -> Vec<bool> {
    (0..w * h)
        .map(|i| {
            let (x, y) = (2.0 * (i % w) as f64 + 1.0, 2.0 * (i / w) as f64 + 1.0);
            positions().iter().any(|&(cx, cy)| (x - cx).hypot(y - cy) < RADIUS)
        })
        .collect()
}

fn iou(a: &[bool], b: &[bool]) -> f64 {
    let inter = a.iter().zip(b).filter(|(x, y)| **x && **y).count();
    let union = a.iter().zip(b).filter(|(x, y)| **x || **y).count();
    inter as f64 / union.max(1) as f64
}

/// Dark blobs (output under 85% of the static scene, so partial ghosts count, where the darkest shot
/// is not clipped) of at least 200 pixels, with their centroids.
fn dark_blobs(out: &Image, background: &Image) -> Vec<(f64, f64)> {
    let mask: Vec<bool> = out
        .pixels
        .iter()
        .zip(&background.pixels)
        .map(|(o, b)| b.iter().all(|&c| c * 0.25 < 0.85) && sum(*o) < 0.85 * sum(*b))
        .collect();
    // Opening: 1-px demosaic artefacts along the colour edges are not blobs.
    let (w, h) = (out.width, out.height);
    let mask = merge::ghost::dilate(&merge::ghost::erode(&mask, w, h, 3), w, h, 3);
    let (labels, areas) = merge::ghost::components(&mask, w, h);
    let mut c = vec![(0.0, 0.0); areas.len()];
    for (i, &l) in labels.iter().enumerate() {
        c[l as usize].0 += (i % out.width) as f64;
        c[l as usize].1 += (i / out.width) as f64;
    }
    (1..areas.len()).filter(|&l| areas[l] >= 200).map(|l| (c[l].0 / areas[l] as f64, c[l].1 / areas[l] as f64)).collect()
}

/// Share of the pixels where only the other shots had the disk (3 px away
/// from the reference's disk) that are darker than 85% of the background.
fn residue(out: &Image, background: &Image) -> f64 {
    let p = positions();
    let (mut n, mut dark) = (0, 0);
    for y in 0..out.height {
        for x in 0..out.width {
            let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
            let d = |i: usize| (fx - p[i].0).hypot(fy - p[i].1);
            if d(1) > RADIUS + 3.0 && (d(0) < RADIUS - 3.0 || d(2) < RADIUS - 3.0) {
                n += 1;
                dark += usize::from(sum(out.get(x, y)) < 0.85 * sum(background.get(x, y)));
            }
        }
    }
    dark as f64 / n as f64
}

fn check_moving_disk(sources: Vec<Source>, align: bool, scene: &Scene, what: &str) {
    let opts = MergeOptions { align, deghost: Deghost::Medium, ..MergeOptions::default() };
    let prepared = prepare(sources.clone(), &opts, &Control::default()).unwrap();
    for (f, s) in prepared.frames.iter().zip(specs(align)) {
        let err = f.transform.corner_rms(&s.transform, W, H);
        eprintln!("{what}: {} {:?}, corner RMS {err:.3} px", f.name, f.alignment);
        assert!(err < 0.25, "{what}: {} corner RMS {err}", f.name);
    }
    let g = prepared.ghosts.as_ref().expect("ghosts");
    let truth_mask = true_mask(g.width, g.height);
    let score = iou(&g.motion, &truth_mask);
    eprintln!("{what}: noise {:?}, {} regions {:?}, IoU {score:.3}", g.noise, g.regions.len(), g.regions);
    assert!(score > 0.8, "{what}: IoU {score}");
    assert!(g.regions.iter().all(|r| r.source == prepared.reference), "{:?}", g.regions);

    let merged = prepared.merge(&Control::default()).unwrap();
    let background = truth(scene, W, H, None);
    let blobs = dark_blobs(&merged.image, &background);
    eprintln!("{what}: dark blobs {blobs:?}");
    assert_eq!(blobs.len(), 1, "{what}: {blobs:?}");
    let (rx, ry) = positions()[1];
    assert!((blobs[0].0 - rx).hypot(blobs[0].1 - ry) < 3.0, "{what}: disk at {:?}", blobs[0]);

    // Where the disk was in the other shots only: background with ghost
    // removal, darkened without.
    let plain = prepare(sources, &MergeOptions { deghost: Deghost::None, ..opts }, &Control::default()).unwrap().merge(&Control::default()).unwrap();
    let (with, without) = (residue(&merged.image, &background), residue(&plain.image, &background));
    eprintln!("{what}: ghost residue {:.2}% with ghost removal, {:.1}% without", 100.0 * with, 100.0 * without);
    assert!(with < 0.01 && without > 0.5, "{with} / {without}");
}

#[test]
fn moving_disk_in_memory() {
    let scene = moving_scene(51);
    check_moving_disk(memory_sources(&scene, &specs(false), W, H), false, &scene, "memory, tripod");
    check_moving_disk(memory_sources(&scene, &specs(true), W, H), true, &scene, "memory, hand-held");
}

#[test]
fn moving_disk_in_dng_files() {
    let scene = moving_scene(52);
    let dir = common::tmp("ghost-dngs");
    std::fs::create_dir_all(&dir).unwrap();
    let sources = specs(true)
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let p = dir.join(format!("G_{i}.dng"));
            write_dng(&p, &scene, s, W, H);
            Source::File(p)
        })
        .collect();
    check_moving_disk(sources, true, &scene, "DNG, hand-held");
}

#[test]
fn static_scene_has_no_ghosts() {
    for (seed, transforms) in [(61, false), (62, true)] {
        let scene = Scene::new(seed, W, H);
        let specs: Vec<ShotSpec> = specs(transforms).into_iter().map(|s| ShotSpec { mover: None, ..s }).collect();
        let opts = MergeOptions { align: transforms, deghost: Deghost::High, ..MergeOptions::default() };
        let prepared = prepare(memory_sources(&scene, &specs, W, H), &opts, &Control::default()).unwrap();
        let g = prepared.ghosts.as_ref().unwrap();
        eprintln!("static (aligned: {transforms}): mask area {:.4}%, noise {:?}, regions {:?}", 100.0 * g.motion_area(), g.noise, g.regions);
        assert!(g.motion_area() < 0.005, "{}", g.motion_area());
    }
}

#[test]
fn levels_are_ordered() {
    // On the moving disk, a stronger level never finds less motion.
    let scene = moving_scene(53);
    let sources = memory_sources(&scene, &specs(false), W, H);
    let mut last = 0.0;
    for level in [Deghost::Low, Deghost::Medium, Deghost::High] {
        let opts = MergeOptions { align: false, deghost: level, ..MergeOptions::default() };
        let p = prepare(sources.clone(), &opts, &Control::default()).unwrap();
        let area = p.ghosts.as_ref().unwrap().motion_area();
        eprintln!("{level:?}: {:.3}%", 100.0 * area);
        assert!(area > 0.0 && area >= last, "{level:?}: {area} < {last}");
        last = area;
    }
    let none = prepare(sources, &MergeOptions { align: false, ..MergeOptions::default() }, &Control::default()).unwrap();
    assert!(none.ghosts.is_none());
}
