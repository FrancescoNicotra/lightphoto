//! Tests against the Panasonic GX80 samples (see the `test-samples` crate):
//! the RW2 from raw.pixls.us, or its versioned crop; and a DNG from Lightroom
//! mobile that only exists on some machines.

use raw::{DecodeOptions, Photo, TempTint};
use test_samples::{optional, Set};
use xmp::{Preset, Value};

/// Lightroom mobile's DNG of another GX80 photo, with its settings in the
/// XMP. Not versioned (it carries the camera's serial number): skipped when
/// missing.
const DNG: &str = "Dreamy days in spring - mobile version.dng";

#[test]
fn decodes_the_gx80_dng() {
    let Some(path) = optional(DNG) else { return };
    let photo = Photo::load(&path, &DecodeOptions { half_size: true, ..Default::default() }).unwrap();
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
    let Some(path) = optional(DNG) else { return };
    let preset = Preset::load(&path).unwrap();
    let number = |key: &str| -> Option<f64> { preset.packet.props.crs(key).and_then(Value::as_text)?.parse().ok() };
    let (Some(temperature), Some(tint)) = (number("AsShotTemperature"), number("AsShotTint")) else {
        eprintln!("skipping: the DNG has no AsShotTemperature/AsShotTint");
        return;
    };

    let photo = Photo::load(&path, &DecodeOptions { half_size: true, ..Default::default() }).unwrap();
    let ours = TempTint::from_xy(photo.as_shot_white().unwrap());
    assert!(
        (ours.temperature - temperature).abs() < temperature * 0.02 && (ours.tint - tint).abs() < 3.0,
        "ours {ours:?}, Lightroom {temperature} K / {tint:+}"
    );
}

// ---------------------------------------------------------------------------
// Panasonic GX80 RW2 from raw.pixls.us (CC0), downloaded into samples/pixls/:
// https://raw.pixls.us/data/Panasonic/DMC-GX80/P1010041.RW2
// Without it, the 1024 × 768 crop in samples/ci (a Bayer DNG).

/// Mean linear sRGB colour, normalised to G = 1.
fn chroma(pixels: impl Iterator<Item = [f32; 3]>) -> [f64; 3] {
    let mut sum = [0.0f64; 3];
    for p in pixels {
        (0..3).for_each(|c| sum[c] += p[c].clamp(0.0, 1.0) as f64);
    }
    sum.map(|s| s / sum[1])
}

#[test]
fn decodes_the_gx80_raw() {
    let sample = test_samples::gx80();
    let path = sample.raw;
    let photo = Photo::load(&path, &DecodeOptions::default()).unwrap();
    assert_eq!((photo.info.make.as_str(), photo.info.model.as_str()), ("Panasonic", "DMC-GX80"));
    let (size, half_size, is_dng) = match sample.set {
        Set::Full => ((4592, 3448), (2296, 1724), false),
        Set::Ci => ((1024, 768), (512, 384), true),
    };
    assert_eq!((photo.data.width, photo.data.height), size);
    let raw = photo.info.raw.as_ref().unwrap();
    assert_eq!(raw.is_dng, is_dng);
    assert_eq!(raw.cfa, "BGGR");
    let tt = TempTint::from_xy(photo.as_shot_white().unwrap());
    assert!((4000.0..6000.0).contains(&tt.temperature), "{tt:?}"); // sunny day

    // Full and half size agree.
    let half = Photo::load(&path, &DecodeOptions { half_size: true, ..Default::default() }).unwrap();
    assert_eq!((half.data.width, half.data.height), half_size);
    let to_srgb = raw::color::to_f32(&raw::color::prophoto_to_srgb());
    let srgb = |p: &Photo| {
        let img = p.to_working(None).downscale(400);
        chroma(img.pixels.into_iter().map(|px| raw::color::apply(&to_srgb, px)))
    };
    let (full_c, half_c) = (srgb(&photo), srgb(&half));
    assert!(full_c.iter().zip(half_c).all(|(a, b)| (a - b).abs() < 0.01), "{full_c:?} vs {half_c:?}");

    // Same overall colour cast as the camera's own JPEG (for the crop, the
    // same region of it). Only roughly: the camera adds its tone curve and a
    // more saturated look.
    let preview = raw::embedded_preview(&path).unwrap().unwrap().into_rgb32f();
    let camera = chroma(preview.pixels().map(|p| p.0.map(raw::color::srgb_eotf)));
    eprintln!("mean colour, G = 1: ours {full_c:.3?}, camera {camera:.3?}");
    assert!(full_c.iter().zip(camera).all(|(a, b)| (a - b).abs() < 0.15), "{full_c:?} vs camera {camera:?}");
}

/// The versioned crop is the RW2's own data: the same photosites, levels,
/// pattern and colour tags. Needs the full RW2 (skipped without it).
#[test]
fn ci_crop_keeps_the_rw2_photosites() {
    let Some(rw2) = optional("pixls/P1010041.RW2") else { return };
    let crop = test_samples::require("ci/P1010041-crop.dng");
    let decode = |path: &std::path::Path| {
        let src = rawler::rawsource::RawSource::new(path).unwrap();
        let decoder = rawler::get_decoder(&src).unwrap();
        decoder.raw_image(&src, &Default::default(), false).unwrap()
    };
    let (full, cropped) = (decode(&rw2), decode(&crop));
    // Must match RAW_CROP in crates/raw/examples/make_ci_samples.rs.
    let (x0, y0) = (1696, 1328);
    let (w, h) = (cropped.width, cropped.height);
    assert_eq!((w, h), (1024, 768));
    let (rawler::RawImageData::Integer(a), rawler::RawImageData::Integer(b)) = (&full.data, &cropped.data) else {
        panic!("integer data expected")
    };
    for y in 0..h {
        let row = &a[(y0 + y) * full.width + x0..][..w];
        assert_eq!(row, &b[y * w..][..w], "row {y}");
    }
    assert_eq!(cropped.blacklevel.as_vec(), full.blacklevel.as_vec());
    assert_eq!(cropped.whitelevel.0, full.whitelevel.0);
    assert_eq!(cropped.camera.cfa.name, full.camera.cfa.name);
    // White balance through AsShotNeutral (rationals): RGB only, the fourth
    // coefficient is NaN for Bayer data.
    let (p, q) = (cropped.wb_coeffs, full.wb_coeffs);
    assert!((0..3).all(|c| (p[c] / p[1] - q[c] / q[1]).abs() < 1e-3), "{p:?} vs {q:?}");
}
