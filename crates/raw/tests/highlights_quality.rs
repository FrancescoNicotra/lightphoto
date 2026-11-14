//! Highlight reconstruction on a real photo: the camera's JPEG, linearised,
//! is taken as the scene, overexposed, turned into a Bayer mosaic that clips
//! as a sensor does (with daylight white balance gains), reconstructed and
//! compared with the values the sensor could not record.

use raw::demosaic::Cfa;
use raw::{highlights, DecodeOptions, Image, Photo};

/// White balance multipliers of a daylight shot (R, G, B).
const GAINS: [f32; 3] = [2.0, 1.0, 1.5];

fn scene() -> Image {
    let path = test_samples::gx80().camera_jpeg;
    Photo::load(&path, &DecodeOptions::default()).unwrap().to_working(None)
}

struct Errors {
    /// Mean |log₂(estimate / truth)| over the clipped samples, in EV.
    reconstructed: f64,
    /// The same with the clipped value kept (no reconstruction).
    clipped: f64,
    samples: usize,
}

fn measure(scene: &Image, stops: f32, cfa: &Cfa) -> Errors {
    let (w, h) = (scene.width, scene.height);
    let k = 2f32.powf(stops);
    // Balanced scene value of photosite (x, y), and whether the JPEG itself
    // was clipped there (no truth to compare with).
    let truth = |x: usize, y: usize| scene.get(x, y)[cfa.color_at(x, y)].max(1e-4) * k;
    let valid = |x: usize, y: usize| {
        let p = scene.get(x, y);
        // The pixel must not have every channel clipped once overexposed:
        // that is left to Photo::to_working.
        p.iter().all(|&v| v < 0.95) && (0..3).any(|c| p[c].max(0.0) * k / GAINS[c] < highlights::CLIP)
    };
    let mut mosaic: Vec<f32> = (0..w * h).map(|i| (truth(i % w, i / w) / GAINS[cfa.color_at(i % w, i / w)]).min(1.0)).collect();
    highlights::reconstruct_mosaic(&mut mosaic, w, h, cfa, GAINS);

    let (mut rec, mut clip, mut n) = (0.0f64, 0.0f64, 0usize);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let c = cfa.color_at(x, y);
            let t = truth(x, y);
            if t / GAINS[c] < 1.0 || !valid(x, y) {
                continue;
            }
            rec += ((mosaic[y * w + x] * GAINS[c] / t) as f64).log2().abs();
            clip += ((GAINS[c] / t) as f64).log2().abs();
            n += 1;
        }
    }
    Errors { reconstructed: rec / n as f64, clipped: clip / n as f64, samples: n }
}

/// Measured on the whole photo: 0.24 EV instead of 0.39 at +1 EV, 0.33
/// instead of 0.55 at +2. Much of what is left is foliage texture: where
/// green clipped, the pixels that clipped are those greener than their
/// neighbours, which the colour ratios measured next to them cannot know.
/// The CI crop is foliage only, the hardest case: limit 0.85 there.
#[test]
fn reconstruction_recovers_most_of_the_clipped_light() {
    let scene = scene();
    let limit = match test_samples::gx80().set {
        test_samples::Set::Full => 0.7,
        test_samples::Set::Ci => 0.85,
    };
    for name in ["BGGR", "RGGB"] {
        let cfa = Cfa::bayer(name).unwrap();
        for stops in [1.0, 2.0] {
            let e = measure(&scene, stops, &cfa);
            eprintln!(
                "{name} +{stops} EV: {} clipped samples, error {:.3} EV reconstructed, {:.3} EV left clipped",
                e.samples, e.reconstructed, e.clipped
            );
            assert!(e.samples > 1000, "{}", e.samples);
            assert!(e.reconstructed < e.clipped * limit, "{name} +{stops}: {:.3} vs {:.3}", e.reconstructed, e.clipped);
        }
    }
}

/// The GX80 photo has a blown sky through the leaves, where green and blue
/// clip and red does not. Reconstructed, those pixels go above the clip
/// point and stay bluish white; left as they were (or with darktable's
/// formula, which averages in the clipped blue) green stays the lowest
/// channel and the sky turns magenta as soon as Exposure is lowered.
#[test]
fn a_blown_sky_does_not_turn_magenta() {
    let path = test_samples::gx80().raw;
    for half_size in [false, true] {
        let photo = Photo::load(&path, &DecodeOptions { half_size, ..Default::default() }).unwrap();
        let raw::ColorSource::Camera(cam) = &photo.color else { panic!("raw expected") };
        let gains = cam.as_shot_neutral.map(|n| 1.0 / n as f32);
        let (mut partly, mut magenta, mut above) = (0usize, 0usize, 0usize);
        for p in &photo.data.pixels {
            if p.iter().any(|&v| v >= highlights::CLIP) && !p.iter().all(|&v| v >= highlights::CLIP) {
                partly += 1;
                let b: [f32; 3] = std::array::from_fn(|c| p[c] * gains[c]);
                magenta += usize::from(b[1] < 0.95 * b[0].min(b[2]));
                above += usize::from(b.iter().any(|&v| v > 1.2));
            }
        }
        eprintln!("half size {half_size}: {partly} pixels partly clipped, {magenta} magenta, {above} above 1.2");
        assert!(partly > 100, "{partly}");
        assert!(magenta * 20 < partly, "{magenta} of {partly}");
        assert!(above * 2 > partly, "{above} of {partly}");
    }
}
