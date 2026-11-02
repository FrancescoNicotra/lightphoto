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
