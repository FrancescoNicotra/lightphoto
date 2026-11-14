//! End-to-end test through rawler with a DNG written on the fly: checks the
//! glue (crop, orientation, black/white levels, DNG colour tags) without
//! needing a real RAW file in the repository.

mod common;

use std::path::PathBuf;

use common::{write_dng, CROP, GREY, SIZE};
use raw::color::D50;
use raw::{ColorSource, DecodeOptions, Photo, TempTint};

fn assert_rgb(p: [f32; 3], expected: f32, eps: f32, what: &str) {
    assert!(p.iter().all(|c| (c - expected).abs() < eps), "{what}: {p:?}, expected {expected}");
}

/// Where every photosite is at the white level, the highlights are
/// reconstructed as neutral at the brightest of them after white balance:
/// the white level times the largest gain (1 / the smallest neutral). At
/// full size RCD's block means (taken across the step from grey to white)
/// move it by a few tenths of a percent.
fn blown(neutral: [f64; 3]) -> f32 {
    (1.0 / neutral.iter().copied().fold(f64::INFINITY, f64::min)) as f32
}

#[test]
fn develops_synthetic_dng() {
    let (path, neutral) = write_dng("synthetic.dng", SIZE);
    let photo = Photo::load(&path, &DecodeOptions::default()).unwrap();

    let raw = photo.info.raw.as_ref().unwrap();
    assert!(raw.is_dng);
    assert_eq!(raw.cfa, "RGGB");
    assert_eq!(raw.crop, CROP.map(|v| v as usize));
    assert!((photo.info.baseline_exposure - 0.35).abs() < 1e-4);
    assert!(raw.as_shot_neutral.iter().zip(neutral).all(|(a, b)| (a - b).abs() < 1e-4));
    let tt = TempTint::from_xy(raw.as_shot_white);
    assert!((tt.temperature - 4500.0).abs() < 15.0 && tt.tint.abs() < 1.0, "{tt:?}");
    assert!(matches!(photo.color, ColorSource::Camera(_)));

    // 56 × 40 crop, rotated to portrait.
    assert_eq!((photo.data.width, photo.data.height), (40, 56));

    // After rotation, output row y comes from sensor column CROP.x + y.
    let img = photo.to_working(None);
    assert_rgb(img.get(20, 5), GREY as f32, 2e-3, "grey card");
    assert_rgb(img.get(20, 50), blown(neutral), 0.01 * blown(neutral), "clipped highlight");

    // A cooler white balance than as-shot makes the grey card warm.
    let warm = photo.to_working(Some(TempTint { temperature: 6500.0, tint: 0.0 }.to_xy())).get(20, 5);
    assert!(warm[0] > warm[2] * 1.1, "{warm:?}");
    // And D50 itself is a valid white.
    let _ = photo.to_working(Some(D50));
}

#[test]
fn half_size_matches_full_size() {
    let (path, neutral) = write_dng("synthetic-half.dng", SIZE);
    let photo = Photo::load(&path, &DecodeOptions { half_size: true, ..Default::default() }).unwrap();
    assert_eq!((photo.data.width, photo.data.height), (20, 28));
    let img = photo.to_working(None);
    assert_rgb(img.get(10, 3), GREY as f32, 2e-3, "grey card");
    assert_rgb(img.get(10, 25), blown(neutral), 2e-3, "clipped highlight");
}

#[test]
fn rendered_images_load_into_working_space() {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("grey.png");
    // sRGB 118 ≈ 18% linear.
    image::RgbImage::from_pixel(8, 4, image::Rgb([118, 118, 118])).save(&path).unwrap();
    let photo = Photo::load(&path, &DecodeOptions::default()).unwrap();
    assert!(matches!(photo.color, ColorSource::Working));
    assert_eq!((photo.data.width, photo.data.height), (8, 4));
    assert_rgb(photo.to_working(None).get(3, 2), 0.181, 2e-3, "sRGB grey");
}

/// `cargo test --release -p raw --test synthetic_dng -- --ignored --nocapture`
#[test]
#[ignore = "timing only"]
fn timing_at_16_megapixels() {
    use std::time::Instant;
    let (path, _) = write_dng("synthetic-16mp.dng", (4592, 3448));
    for half_size in [false, true] {
        let t = Instant::now();
        let photo = Photo::load(&path, &DecodeOptions { half_size, ..Default::default() }).unwrap();
        let decoded = t.elapsed();
        let img = photo.to_working(None);
        let developed = t.elapsed();
        let preview = img.downscale(2048);
        eprintln!(
            "half_size={half_size}: decode {:?}, to_working {:?}, downscale {:?} -> {}x{}",
            decoded,
            developed - decoded,
            t.elapsed() - developed,
            preview.width,
            preview.height
        );
    }
}
