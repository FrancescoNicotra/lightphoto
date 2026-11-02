//! Tests against the Panasonic GX80 DNG in /samples (not versioned: they are
//! skipped when the file is missing).

use std::path::PathBuf;

use raw::{DecodeOptions, Photo, TempTint};
use xmp::{Preset, Value};

const DNG: &str = "Dreamy days in spring - mobile version.dng";

fn sample(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples").join(name);
    p.exists().then_some(p).or_else(|| {
        eprintln!("skipping: {name} not found in samples/");
        None
    })
}

#[test]
fn decodes_the_gx80_dng() {
    let Some(path) = sample(DNG) else { return };
    let photo = Photo::load(&path, &DecodeOptions { half_size: true }).unwrap();
    assert_eq!(photo.info.make, "Panasonic");
    assert!(photo.info.model.contains("GX80"), "{}", photo.info.model);
    let raw = photo.info.raw.as_ref().unwrap();
    assert!(raw.is_dng);
    assert_eq!(raw.cfa.len(), 4, "Bayer pattern expected, got {}", raw.cfa);
    assert!(raw::embedded_preview(&path).unwrap().is_some());
}

/// Lightroom writes the as-shot white balance it computed into the XMP; ours
/// comes from AsShotNeutral and the DNG matrices with the same maths.
#[test]
fn as_shot_white_balance_matches_lightroom() {
    let Some(path) = sample(DNG) else { return };
    let preset = Preset::load(&path).unwrap();
    let number = |key: &str| -> Option<f64> { preset.packet.props.crs(key).and_then(Value::as_text)?.parse().ok() };
    let (Some(temperature), Some(tint)) = (number("AsShotTemperature"), number("AsShotTint")) else {
        eprintln!("skipping: the DNG has no AsShotTemperature/AsShotTint");
        return;
    };

    let photo = Photo::load(&path, &DecodeOptions { half_size: true }).unwrap();
    let ours = TempTint::from_xy(photo.as_shot_white().unwrap());
    assert!(
        (ours.temperature - temperature).abs() < temperature * 0.02 && (ours.tint - tint).abs() < 3.0,
        "ours {ours:?}, Lightroom {temperature} K / {tint:+}"
    );
}

// ---------------------------------------------------------------------------
// Panasonic GX80 RW2 from raw.pixls.us (CC0), downloaded into samples/pixls/:
// https://raw.pixls.us/data/Panasonic/DMC-GX80/P1010041.RW2

const RW2: &str = "pixls/P1010041.RW2";

/// Mean linear sRGB colour, normalised to G = 1.
fn chroma(pixels: impl Iterator<Item = [f32; 3]>) -> [f64; 3] {
    let mut sum = [0.0f64; 3];
    for p in pixels {
        (0..3).for_each(|c| sum[c] += p[c].clamp(0.0, 1.0) as f64);
    }
    sum.map(|s| s / sum[1])
}

#[test]
fn decodes_the_gx80_rw2() {
    let Some(path) = sample(RW2) else { return };
    let photo = Photo::load(&path, &DecodeOptions::default()).unwrap();
    assert_eq!((photo.info.make.as_str(), photo.info.model.as_str()), ("Panasonic", "DMC-GX80"));
    assert_eq!((photo.data.width, photo.data.height), (4592, 3448));
    let raw = photo.info.raw.as_ref().unwrap();
    assert!(!raw.is_dng);
    assert_eq!(raw.cfa, "BGGR");
    let tt = TempTint::from_xy(photo.as_shot_white().unwrap());
    assert!((4000.0..6000.0).contains(&tt.temperature), "{tt:?}"); // sunny day

    // Full and half size agree.
    let half = Photo::load(&path, &DecodeOptions { half_size: true }).unwrap();
    assert_eq!((half.data.width, half.data.height), (2296, 1724));
    let to_srgb = raw::color::to_f32(&raw::color::prophoto_to_srgb());
    let srgb = |p: &Photo| {
        let img = p.to_working(None).downscale(400);
        chroma(img.pixels.into_iter().map(|px| raw::color::apply(&to_srgb, px)))
    };
    let (full_c, half_c) = (srgb(&photo), srgb(&half));
    assert!(full_c.iter().zip(half_c).all(|(a, b)| (a - b).abs() < 0.01), "{full_c:?} vs {half_c:?}");

    // Same overall colour cast as the camera's own JPEG. Only roughly: the
    // camera adds its tone curve and a more saturated look.
    let preview = raw::embedded_preview(&path).unwrap().unwrap().into_rgb32f();
    let camera = chroma(preview.pixels().map(|p| p.0.map(raw::color::srgb_eotf)));
    eprintln!("mean colour, G = 1: ours {full_c:.3?}, camera {camera:.3?}");
    assert!(full_c.iter().zip(camera).all(|(a, b)| (a - b).abs() < 0.15), "{full_c:?} vs camera {camera:?}");
}
