//! End-to-end test through rawler with a DNG written on the fly: checks the
//! glue (crop, orientation, black/white levels, DNG colour tags) without
//! needing a real RAW file in the repository.

use std::path::PathBuf;

use raw::color::{Mat3, D50};
use raw::profile::ColorProfile;
use raw::{ColorSource, DecodeOptions, Photo, TempTint};

const BLACK: u32 = 512;
const WHITE: u32 = 16383;
const SIZE: (usize, usize) = (64, 48);
/// x, y, w, h: the size is the sensor minus 8 pixels.
const CROP: [u32; 4] = [2, 2, 56, 40];
const COLOR_MATRIX: Mat3 = [[0.7771, -0.3020, -0.0629], [-0.4029, 1.1950, 0.2345], [-0.0821, 0.1977, 0.6119]];
const GREY: f64 = 0.18;

enum Val {
    Short(Vec<u16>),
    Long(Vec<u32>),
    Byte(Vec<u8>),
    Ascii(&'static str),
    SRational(Vec<f64>),
    Rational(Vec<f64>),
}

/// Minimal little-endian TIFF writer: one IFD, one strip.
fn tiff(mut entries: Vec<(u16, Val)>, strip: &[u8]) -> Vec<u8> {
    entries.sort_by_key(|e| e.0);
    let ifd_start = 8;
    let ifd_len = 2 + entries.len() * 12 + 4;
    let mut data = Vec::<u8>::new(); // out-of-line values, after the IFD
    let data_start = ifd_start + ifd_len;
    let mut ifd = Vec::new();
    ifd.extend((entries.len() as u16).to_le_bytes());
    for (tag, val) in &entries {
        let (ty, count, bytes): (u16, usize, Vec<u8>) = match val {
            Val::Short(v) => (3, v.len(), v.iter().flat_map(|x| x.to_le_bytes()).collect()),
            Val::Long(v) => (4, v.len(), v.iter().flat_map(|x| x.to_le_bytes()).collect()),
            Val::Byte(v) => (1, v.len(), v.clone()),
            Val::Ascii(s) => (2, s.len() + 1, [s.as_bytes(), &[0]].concat()),
            Val::SRational(v) => (
                10,
                v.len(),
                v.iter().flat_map(|x| [((x * 10000.0).round() as i32).to_le_bytes(), 10000i32.to_le_bytes()].concat()).collect(),
            ),
            Val::Rational(v) => (
                5,
                v.len(),
                v.iter().flat_map(|x| [((x * 1e6).round() as u32).to_le_bytes(), 1_000_000u32.to_le_bytes()].concat()).collect(),
            ),
        };
        ifd.extend(tag.to_le_bytes());
        ifd.extend(ty.to_le_bytes());
        ifd.extend((count as u32).to_le_bytes());
        if bytes.len() <= 4 {
            let mut inline = bytes.clone();
            inline.resize(4, 0);
            ifd.extend(inline);
        } else {
            ifd.extend(((data_start + data.len()) as u32).to_le_bytes());
            data.extend(&bytes);
            if data.len() % 2 == 1 {
                data.push(0);
            }
        }
    }
    ifd.extend(0u32.to_le_bytes());
    assert_eq!(ifd.len(), ifd_len);

    let mut out = b"II*\0".to_vec();
    out.extend((ifd_start as u32).to_le_bytes());
    out.extend(ifd);
    out.extend(data);
    let strip_offset = out.len() as u32;
    out.extend(strip);
    // Patch StripOffsets (tag 273) now that the layout is known.
    let pos = entries.iter().position(|e| e.0 == 273).unwrap();
    let field = ifd_start + 2 + pos * 12 + 8;
    out[field..field + 4].copy_from_slice(&strip_offset.to_le_bytes());
    out
}

/// A Bayer RGGB DNG lit by a 4500 K light: the left half of the sensor sees
/// an 18% grey card, the right half is blown out. Orientation 6 (90° CW).
fn write_dng(name: &str, (w, h): (usize, usize)) -> (PathBuf, [f64; 3]) {
    let profile = ColorProfile::from_color_matrix(COLOR_MATRIX, 6500.0);
    let neutral = profile.xy_to_neutral(TempTint { temperature: 4500.0, tint: 0.0 }.to_xy());

    let rggb = [[0, 1], [1, 2]];
    let mut strip = Vec::with_capacity(w * h * 2);
    for y in 0..h {
        for x in 0..w {
            let v = if x < w / 2 {
                let c = rggb[y % 2][x % 2];
                BLACK as f64 + GREY * neutral[c] * (WHITE - BLACK) as f64
            } else {
                WHITE as f64
            };
            strip.extend((v.round() as u16).to_le_bytes());
        }
    }

    let entries = vec![
        (254, Val::Long(vec![0])),
        (256, Val::Long(vec![w as u32])),
        (257, Val::Long(vec![h as u32])),
        (258, Val::Short(vec![16])),
        (259, Val::Short(vec![1])),
        (262, Val::Short(vec![32803])),
        (271, Val::Ascii("Lumen")),
        (272, Val::Ascii("Synthetic")),
        (273, Val::Long(vec![0])),
        (274, Val::Short(vec![6])),
        (277, Val::Short(vec![1])),
        (278, Val::Long(vec![h as u32])),
        (279, Val::Long(vec![(w * h * 2) as u32])),
        (284, Val::Short(vec![1])),
        (33421, Val::Short(vec![2, 2])),
        (33422, Val::Byte(vec![0, 1, 1, 2])),
        (50706, Val::Byte(vec![1, 4, 0, 0])),
        (50708, Val::Ascii("Lumen Synthetic")),
        (50714, Val::Long(vec![BLACK])),
        (50717, Val::Long(vec![WHITE])),
        (50719, Val::Long(CROP[..2].to_vec())),
        (50720, Val::Long(vec![w as u32 - 8, h as u32 - 8])),
        (50721, Val::SRational(COLOR_MATRIX.iter().flatten().copied().collect())),
        (50728, Val::Rational(neutral.to_vec())),
        (50730, Val::SRational(vec![0.35])),
        (50778, Val::Short(vec![21])),
    ];
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, tiff(entries, &strip)).unwrap();
    (path, neutral)
}

fn assert_rgb(p: [f32; 3], expected: f32, eps: f32, what: &str) {
    assert!(p.iter().all(|c| (c - expected).abs() < eps), "{what}: {p:?}, expected {expected}");
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
    assert_rgb(img.get(20, 50), 1.0, 1e-3, "clipped highlight");

    // A cooler white balance than as-shot makes the grey card warm.
    let warm = photo.to_working(Some(TempTint { temperature: 6500.0, tint: 0.0 }.to_xy())).get(20, 5);
    assert!(warm[0] > warm[2] * 1.1, "{warm:?}");
    // And D50 itself is a valid white.
    let _ = photo.to_working(Some(D50));
}

#[test]
fn half_size_matches_full_size() {
    let (path, _) = write_dng("synthetic-half.dng", SIZE);
    let photo = Photo::load(&path, &DecodeOptions { half_size: true }).unwrap();
    assert_eq!((photo.data.width, photo.data.height), (20, 28));
    let img = photo.to_working(None);
    assert_rgb(img.get(10, 3), GREY as f32, 2e-3, "grey card");
    assert_rgb(img.get(10, 25), 1.0, 1e-3, "clipped highlight");
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
        let photo = Photo::load(&path, &DecodeOptions { half_size }).unwrap();
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
