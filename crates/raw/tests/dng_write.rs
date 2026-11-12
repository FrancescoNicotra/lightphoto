//! `write_linear_dng` against rawler's decoder: the half float encoder and
//! the tags must survive a round trip, and a "merge" of a single exposure
//! must develop exactly like the RAW it came from.

mod common;

use std::path::PathBuf;

use common::{tiff, write_dng, Val, CROP, SIZE};
use raw::color::{self, Mat3};
use raw::profile::{Calibration, ColorProfile};
use raw::{load_capture, write_linear_dng, ColorSource, DecodeOptions, DngEncoding, Image, LinearDng, Orientation, Photo};
use rawler::decoders::RawMetadata;
use rawler::exif::Exif;
use rawler::formats::tiff::Rational;

const CM_A: Mat3 = [[0.8550, -0.3708, 0.0129], [-0.4014, 1.1559, 0.2805], [-0.0442, 0.1159, 0.6436]];
const CM_D65: Mat3 = [[0.7771, -0.3020, -0.0629], [-0.4029, 1.1950, 0.2345], [-0.0821, 0.1977, 0.6119]];
const FM_A: Mat3 = [[0.5925, 0.2563, 0.1155], [0.2193, 0.7787, 0.0020], [0.0331, 0.0007, 0.7913]];
const FM_D65: Mat3 = [[0.4836, 0.3825, 0.0982], [0.1773, 0.8530, -0.0303], [0.0213, -0.1306, 0.9343]];
const CC: Mat3 = [[1.02, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.97]];
const XMP: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="+0.50"/></rdf:RDF></x:xmpmeta>"#;

fn tmp(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

fn profile() -> ColorProfile {
    let cal = |temperature, color_matrix, fm| Calibration {
        temperature,
        color_matrix,
        forward_matrix: Some(fm),
        camera_calibration: CC,
        hue_sat_map: None,
    };
    ColorProfile::new(vec![cal(2850.0, CM_A, FM_A), cal(6500.0, CM_D65, FM_D65)], Some(color::diag([0.98, 1.0, 1.03])))
}

/// 300×270 (four tiles, three of them padded): smooth ramps from 1e-3 to
/// 1.0 per channel, a clipped block and a few values above 1.
fn test_image() -> Image {
    let (w, h) = (300, 270);
    Image::from_fn(w, h, |x, y| {
        if (40..60).contains(&x) && (30..50).contains(&y) {
            return [1.0, 1.0, 0.99];
        }
        if x == 299 && y < 10 {
            return [1.5, 3.25, 7.0];
        }
        let u = x as f32 / (w - 1) as f32;
        let v = y as f32 / (h - 1) as f32;
        let ramp = |t: f32| 1e-3 * 1000f32.powf(t);
        [ramp(u), ramp(v), ramp(((x * 7 + y * 13) % 101) as f32 / 100.0)]
    })
}

fn metadata() -> RawMetadata {
    let exif = Exif {
        orientation: Some(1), // overridden by LinearDng::orientation
        exposure_time: Some(Rational::new(1, 125)),
        fnumber: Some(Rational::new(56, 10)),
        iso_speed_ratings: Some(200),
        focal_length: Some(Rational::new(35, 1)),
        date_time_original: Some("2026:11:06 12:34:56".into()),
        ..Exif::default()
    };
    RawMetadata { exif, ..RawMetadata::default() }
}

fn assert_mat(a: &Mat3, b: &Mat3, what: &str) {
    let d = a.iter().flatten().zip(b.iter().flatten()).map(|(x, y)| (x - y).abs()).fold(0.0, f64::max);
    assert!(d < 1e-6, "{what}: {a:?} vs {b:?}");
}

fn round_trip(encoding: DngEncoding, name: &str) {
    let image = test_image();
    let profile = profile();
    let meta = metadata();
    let orientation = Orientation::Rotate270;
    let neutral = [0.512345, 1.0, 0.701234];
    let dng = LinearDng {
        image: &image,
        orientation,
        make: "Lumen",
        model: "Test Cam",
        unique_camera_model: "Lumen Test Cam",
        profile: &profile,
        as_shot_neutral: neutral,
        baseline_exposure: 1.25,
        baseline_noise: 0.6,
        metadata: Some(&meta),
        xmp: Some(XMP.as_bytes()),
        preview: None,
    };
    let path = tmp(name);
    write_linear_dng(&path, &dng, encoding).unwrap();
    assert!(!tmp(&format!("{name}.tmp")).exists());

    let photo = Photo::load(&path, &DecodeOptions::default()).unwrap();
    let info = &photo.info;
    assert_eq!(info.orientation, orientation);
    assert_eq!((info.make.as_str(), info.model.as_str()), ("Lumen", "Test Cam"));
    assert!((info.baseline_exposure - 1.25).abs() < 1e-6, "{}", info.baseline_exposure);
    assert_eq!(info.iso, Some(200));
    assert_eq!(info.exposure_time, Some(1.0 / 125.0));
    assert_eq!(info.f_number, Some(5.6));
    assert_eq!(info.focal_length, Some(35.0));
    assert_eq!(info.date_time.as_deref(), Some("2026:11:06 12:34:56"));

    let raw = info.raw.as_ref().unwrap();
    assert!(raw.is_dng && raw.float);
    assert_eq!(raw.white_level, vec![1.0; 3]);
    assert_eq!(raw.crop, [0, 0, 300, 270]);
    assert_eq!(raw.cfa, "linear RGB");
    assert!(raw.as_shot_neutral.iter().zip(neutral).all(|(a, b)| (a - b).abs() < 1e-6), "{:?}", raw.as_shot_neutral);

    let ColorSource::Camera(cam) = &photo.color else { panic!("camera colour expected") };
    let p = &cam.profile;
    assert_eq!(p.calibrations.len(), 2);
    for (got, want) in p.calibrations.iter().zip(&profile.calibrations) {
        assert_eq!(got.temperature, want.temperature);
        assert_mat(&got.color_matrix, &want.color_matrix, "ColorMatrix");
        assert_mat(got.forward_matrix.as_ref().unwrap(), want.forward_matrix.as_ref().unwrap(), "ForwardMatrix");
        assert_mat(&got.camera_calibration, &want.camera_calibration, "CameraCalibration");
    }
    assert_mat(&p.analog_balance, &profile.analog_balance, "AnalogBalance");

    // Pixels, after the same orientation.
    let expected = image.clone().oriented(orientation);
    assert_eq!((photo.data.width, photo.data.height), (expected.width, expected.height));
    let tolerance = match encoding {
        DngEncoding::HalfDeflate => 1e-3,
        DngEncoding::F32Uncompressed => 0.0,
    };
    let mut worst = 0.0f32;
    for (got, want) in photo.data.pixels.iter().zip(&expected.pixels) {
        for c in 0..3 {
            worst = worst.max((got[c] - want[c]).abs() / want[c]);
        }
    }
    assert!(worst <= tolerance, "relative error {worst}");
    if encoding == DngEncoding::HalfDeflate {
        assert!(worst > 0.0, "half floats cannot be exact on these ramps");
    }

    // XMP (tag 700) and the previews.
    let bytes = std::fs::read(&path).unwrap();
    assert!(xmp::extract_xmp(&bytes).unwrap().contains("crs:Exposure2012=\"+0.50\""));
    let preview = raw::embedded_preview(&path).unwrap().unwrap();
    assert_eq!((preview.width(), preview.height()), (300, 270));

    // Clipping mask of a LinearRaw file: per pixel, in sensor orientation.
    let capture = load_capture(&path, &DecodeOptions::default()).unwrap();
    assert_eq!(capture.info.orientation, orientation);
    assert_eq!((capture.data.width, capture.data.height), (300, 270));
    for (i, (&m, p)) in capture.clipped.iter().zip(&image.pixels).enumerate() {
        assert_eq!(m == 1, p.iter().any(|&v| v >= 0.98), "pixel {i}");
    }
}

#[test]
fn half_float_round_trip() {
    round_trip(DngEncoding::HalfDeflate, "round-trip-half.dng");
}

#[test]
fn f32_round_trip() {
    round_trip(DngEncoding::F32Uncompressed, "round-trip-f32.dng");
}

#[test]
fn half_float_is_smaller() {
    let image = test_image();
    let profile = profile();
    let dng = LinearDng {
        image: &image,
        orientation: Orientation::Normal,
        make: "Lumen",
        model: "Test Cam",
        unique_camera_model: "Lumen Test Cam",
        profile: &profile,
        as_shot_neutral: [0.5, 1.0, 0.7],
        baseline_exposure: 0.0,
        baseline_noise: 1.0,
        metadata: None,
        xmp: None,
        preview: None,
    };
    let size = |enc, name| {
        let p = tmp(name);
        write_linear_dng(&p, &dng, enc).unwrap();
        std::fs::metadata(&p).unwrap().len()
    };
    let half = size(DngEncoding::HalfDeflate, "size-half.dng");
    let full = size(DngEncoding::F32Uncompressed, "size-f32.dng");
    // 6 bytes/pixel before Deflate against 12.
    assert!(half * 2 < full, "{half} vs {full}");
}

/// A Bayer DNG read as a capture and written back as a linear DNG, as an HDR
/// merge of one frame would be: it must develop like the original.
#[test]
fn single_frame_merge_develops_like_the_original() {
    let (source, _) = write_dng("single-frame-source.dng", SIZE);
    let capture = load_capture(&source, &DecodeOptions::default()).unwrap();

    // The mask: sensor columns ≥ 32 are blown out; the demosaic's support
    // (raw::rcd::SUPPORT_RADIUS) reaches that many columns further left.
    // Crop x = sensor x − 2.
    let (cw, ch) = (CROP[2] as usize, CROP[3] as usize);
    assert_eq!((capture.data.width, capture.data.height), (cw, ch));
    let first = 30 - raw::rcd::SUPPORT_RADIUS;
    for y in 0..ch {
        for x in 0..cw {
            assert_eq!(capture.clipped[y * cw + x], u8::from(x >= first), "({x}, {y})");
        }
    }
    let half = load_capture(&source, &DecodeOptions { half_size: true }).unwrap();
    assert_eq!((half.data.width, half.data.height), (cw / 2, ch / 2));
    for (i, &m) in half.clipped.iter().enumerate() {
        assert_eq!(m, u8::from(i % (cw / 2) >= 15), "half-size pixel {i}");
    }

    let merged = tmp("single-frame-merged.dng");
    write_linear_dng(&merged, &LinearDng::from_capture(&capture), DngEncoding::HalfDeflate).unwrap();

    let original = Photo::load(&source, &DecodeOptions::default()).unwrap();
    let copy = Photo::load(&merged, &DecodeOptions::default()).unwrap();
    assert_eq!(copy.info.orientation, original.info.orientation);
    assert_eq!((copy.info.make.as_str(), copy.info.model.as_str()), (original.info.make.as_str(), original.info.model.as_str()));
    assert!((copy.info.baseline_exposure - original.info.baseline_exposure).abs() < 1e-6);
    let (a, b) = (original.info.raw.as_ref().unwrap(), copy.info.raw.as_ref().unwrap());
    assert!(!a.float && b.float);
    assert!((a.as_shot_white.x - b.as_shot_white.x).abs() < 1e-6 && (a.as_shot_white.y - b.as_shot_white.y).abs() < 1e-6);

    let (x, y) = (original.to_working(None), copy.to_working(None));
    assert_eq!((x.width, x.height), (y.width, y.height));
    let to_xyz = color::to_f32(&color::prophoto_to_xyz());
    let lab = |p| color::to_lab(color::apply(&to_xyz, p));
    // Identical outside the clipping mask (oriented like the photos). Inside
    // it the original's highlights are reconstructed and a merge's are not:
    // neutral above the clip point in the blown columns, and no colour fringe
    // at their edge, where the red channel, clipping higher after white
    // balance, used to bleed into the grey (it still does in the merge).
    // Within 8 photosites of the sensor's edge RCD falls back to bilinear,
    // which makes its own fringe on a step this sharp: not checked.
    let oriented = |f: &(dyn Fn(usize, usize) -> bool + Sync)| Image::from_fn(cw, ch, |x, y| [f32::from(u8::from(f(x, y))); 3]).oriented(original.info.orientation);
    let mask = oriented(&|x, y| capture.clipped[y * cw + x] == 1);
    let rcd = oriented(&|x, y| {
        let (sx, sy) = (x + CROP[0] as usize, y + CROP[1] as usize);
        sx >= 8 && sy >= 8 && sx + 8 < SIZE.0 && sy + 8 < SIZE.1
    });
    let mut worst = 0.0f32;
    for (i, (&p, &q)) in x.pixels.iter().zip(&y.pixels).enumerate() {
        if mask.pixels[i][0] == 0.0 {
            worst = worst.max(color::delta_e2000(lab(p), lab(q)));
        } else if rcd.pixels[i][0] == 1.0 {
            let (lo, hi) = (p[0].min(p[1]).min(p[2]), p[0].max(p[1]).max(p[2]));
            assert!(hi - lo < 0.02 * hi, "pixel {i}: {p:?}");
        }
    }
    // Not 0.5 any more: RCD's block means include the blown columns, which
    // differ between the two, and shift the grey a little (0.42 measured).
    assert!(worst < 0.6, "max ΔE2000 {worst}");
}

/// Floating point DNGs from other writers may omit WhiteLevel (rawler then
/// assumes 2^32 − 1) or give an integer one: 1.0 is white regardless.
#[test]
fn float_dng_white_is_one() {
    let (w, h) = (4usize, 2usize);
    let values: Vec<f32> = (0..w * h * 3).map(|i| i as f32 / 24.0).collect();
    let strip: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    for white in [None, Some(65535u32)] {
        let mut entries = vec![
            (254, Val::Long(vec![0])),
            (256, Val::Long(vec![w as u32])),
            (257, Val::Long(vec![h as u32])),
            (258, Val::Short(vec![32, 32, 32])),
            (259, Val::Short(vec![1])),
            (262, Val::Short(vec![34892])),
            (271, Val::Ascii("Lumen")),
            (272, Val::Ascii("Float")),
            (273, Val::Long(vec![0])),
            (277, Val::Short(vec![3])),
            (278, Val::Long(vec![h as u32])),
            (279, Val::Long(vec![strip.len() as u32])),
            (284, Val::Short(vec![1])),
            (339, Val::Short(vec![3, 3, 3])),
            (50706, Val::Byte(vec![1, 4, 0, 0])),
            (50721, Val::SRational(CM_D65.iter().flatten().copied().collect())),
            (50728, Val::Rational(vec![0.5, 1.0, 0.7])),
            (50778, Val::Short(vec![21])),
        ];
        if let Some(wl) = white {
            entries.push((50717, Val::Long(vec![wl])));
        }
        let path = tmp(&format!("float-white-{white:?}.dng"));
        std::fs::write(&path, tiff(entries, &strip)).unwrap();
        let photo = Photo::load(&path, &DecodeOptions::default()).unwrap();
        let raw = photo.info.raw.as_ref().unwrap();
        assert!(raw.float);
        assert_eq!(raw.white_level, vec![1.0; 3]);
        let got: Vec<f32> = photo.data.pixels.iter().flatten().copied().collect();
        assert_eq!(got, values, "white {white:?}");
    }
}
