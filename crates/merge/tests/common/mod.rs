//! Synthetic brackets for the merge tests: in-memory captures, Bayer DNGs
//! with EXIF, and the metrics of `plan/HDR.md` §8.2.

#![allow(dead_code)]

pub mod scene;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use merge::Source;
use raw::capture::CameraNames;
use raw::color::Mat3;
use raw::profile::ColorProfile;
use raw::{Capture, Image, Orientation, PhotoInfo, RawInfo};
use rayon::prelude::*;
use scene::{Scene, ShotSpec, NEUTRAL};

pub const COLOR_MATRIX: Mat3 = [[0.7771, -0.3020, -0.0629], [-0.4029, 1.1950, 0.2345], [-0.0821, 0.1977, 0.6119]];
pub const BLACK: f64 = 512.0;
pub const WHITE: f64 = 16383.0;

pub fn tmp(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

fn info(spec: &ShotSpec, w: usize, h: usize) -> PhotoInfo {
    PhotoInfo {
        make: "Lumen".into(),
        model: "Synthetic".into(),
        iso: Some(100),
        exposure_time: spec.exif_time,
        f_number: Some(8.0),
        focal_length: Some(35.0),
        orientation: Orientation::Normal,
        raw: Some(RawInfo {
            is_dng: true,
            float: false,
            sensor_size: (w, h),
            crop: [0, 0, w, h],
            cfa: "RGGB".into(),
            black_level: vec![0.0],
            white_level: vec![1.0],
            as_shot_neutral: NEUTRAL,
            as_shot_white: ColorProfile::from_color_matrix(COLOR_MATRIX, 6500.0).neutral_to_xy(NEUTRAL),
        }),
        ..PhotoInfo::default()
    }
}

/// A full-size capture sampled directly in RGB (no mosaic).
pub fn capture(scene: &Scene, spec: &ShotSpec, w: usize, h: usize) -> Capture {
    let inv = spec.transform.inverse().unwrap();
    let data = Image::from_fn(w, h, |x, y| {
        std::array::from_fn(|c| spec.sample(scene, &inv, x as f64 + 0.5, y as f64 + 0.5, c, x as i64, y as i64) as f32)
    });
    let clipped = data.pixels.par_iter().map(|p| u8::from(p.iter().any(|&c| c >= raw::capture::CLIP_MASK))).collect();
    Capture {
        info: info(spec, w, h),
        data,
        clipped,
        profile: ColorProfile::from_color_matrix(COLOR_MATRIX, 6500.0),
        as_shot_neutral: NEUTRAL,
        camera: CameraNames { make: "Lumen".into(), model: "Synthetic".into(), unique_model: "Lumen Synthetic".into() },
        metadata: None,
    }
}

pub fn memory_sources(scene: &Scene, specs: &[ShotSpec], w: usize, h: usize) -> Vec<Source> {
    specs
        .iter()
        .enumerate()
        .map(|(i, s)| Source::Memory { name: format!("shot{i}"), capture: Arc::new(capture(scene, s, w, h)) })
        .collect()
}

/// The scene radiance at every pixel centre of the reference frame.
pub fn truth(scene: &Scene, w: usize, h: usize, mover: Option<(f64, f64)>) -> Image {
    Image::from_fn(w, h, |x, y| scene.radiance_with(x as f64 + 0.5, y as f64 + 0.5, mover).map(|v| v as f32))
}

// ---------------------------------------------------------------------------
// DNG files

enum Val {
    Short(Vec<u16>),
    Long(Vec<u32>),
    Byte(Vec<u8>),
    Ascii(String),
    /// (numerator, denominator)
    Rational(Vec<(u32, u32)>),
    SRational(Vec<f64>),
}

/// One IFD at `start` (entries sorted), with its out-of-line data after it.
fn ifd(mut entries: Vec<(u16, Val)>, start: usize) -> Vec<u8> {
    entries.sort_by_key(|e| e.0);
    let len = 2 + entries.len() * 12 + 4;
    let mut data = Vec::new();
    let mut out = (entries.len() as u16).to_le_bytes().to_vec();
    for (tag, val) in &entries {
        let (ty, count, bytes): (u16, usize, Vec<u8>) = match val {
            Val::Short(v) => (3, v.len(), v.iter().flat_map(|x| x.to_le_bytes()).collect()),
            Val::Long(v) => (4, v.len(), v.iter().flat_map(|x| x.to_le_bytes()).collect()),
            Val::Byte(v) => (1, v.len(), v.clone()),
            Val::Ascii(s) => (2, s.len() + 1, [s.as_bytes(), &[0]].concat()),
            Val::Rational(v) => (5, v.len(), v.iter().flat_map(|(n, d)| [n.to_le_bytes(), d.to_le_bytes()].concat()).collect()),
            Val::SRational(v) => (
                10,
                v.len(),
                v.iter().flat_map(|x| [((x * 10000.0).round() as i32).to_le_bytes(), 10000i32.to_le_bytes()].concat()).collect(),
            ),
        };
        out.extend(tag.to_le_bytes());
        out.extend(ty.to_le_bytes());
        out.extend((count as u32).to_le_bytes());
        if bytes.len() <= 4 {
            let mut inline = bytes;
            inline.resize(4, 0);
            out.extend(inline);
        } else {
            out.extend(((start + len + data.len()) as u32).to_le_bytes());
            data.extend(&bytes);
            if data.len() % 2 == 1 {
                data.push(0);
            }
        }
    }
    out.extend(0u32.to_le_bytes());
    out.extend(data);
    out
}

fn rational(v: f64) -> (u32, u32) {
    if v < 1.0 {
        // 1/x exposure times are exact as (1, x).
        let inv = 1.0 / v;
        if (inv - inv.round()).abs() < 1e-9 {
            return (1, inv.round() as u32);
        }
    }
    ((v * 1e6).round() as u32, 1_000_000)
}

/// A Bayer RGGB DNG of `spec`: the reference frame is the default crop, at
/// (1, 1) on a sensor two pixels larger (an odd crop origin, so half-size
/// cells start one photosite late). EXIF: exposure time, f/8, ISO 100.
pub fn write_dng(path: &Path, scene: &Scene, spec: &ShotSpec, w: usize, h: usize) {
    let (sw, sh) = (w + 2, h + 2);
    let inv = spec.transform.inverse().unwrap();
    let rggb = [[0, 1], [1, 2]];
    let rows: Vec<Vec<u8>> = (0..sh)
        .into_par_iter()
        .map(|sy| {
            let mut row = Vec::with_capacity(sw * 2);
            for sx in 0..sw {
                let c = rggb[sy % 2][sx % 2];
                let (x, y) = (sx as f64 - 1.0 + 0.5, sy as f64 - 1.0 + 0.5);
                let v = spec.sample(scene, &inv, x, y, c, sx as i64, sy as i64);
                let dn = (BLACK + v * (WHITE - BLACK)).round().clamp(0.0, 65535.0) as u16;
                row.extend(dn.to_le_bytes());
            }
            row
        })
        .collect();
    let strip: Vec<u8> = rows.concat();

    let mut exif = vec![(34855, Val::Short(vec![100])), (33437, Val::Rational(vec![(8, 1)])), (37386, Val::Rational(vec![(35, 1)]))];
    if let Some(t) = spec.exif_time {
        exif.push((33434, Val::Rational(vec![rational(t)])));
    }
    let entries = |exif_offset: u32, strip_offset: u32| {
        vec![
            (254, Val::Long(vec![0])),
            (256, Val::Long(vec![sw as u32])),
            (257, Val::Long(vec![sh as u32])),
            (258, Val::Short(vec![16])),
            (259, Val::Short(vec![1])),
            (262, Val::Short(vec![32803])),
            (271, Val::Ascii("Lumen".into())),
            (272, Val::Ascii("Synthetic".into())),
            (273, Val::Long(vec![strip_offset])),
            (274, Val::Short(vec![1])),
            (277, Val::Short(vec![1])),
            (278, Val::Long(vec![sh as u32])),
            (279, Val::Long(vec![strip.len() as u32])),
            (284, Val::Short(vec![1])),
            (33421, Val::Short(vec![2, 2])),
            (33422, Val::Byte(vec![0, 1, 1, 2])),
            (34665, Val::Long(vec![exif_offset])),
            (50706, Val::Byte(vec![1, 4, 0, 0])),
            (50708, Val::Ascii("Lumen Synthetic".into())),
            (50714, Val::Long(vec![BLACK as u32])),
            (50717, Val::Long(vec![WHITE as u32])),
            (50719, Val::Long(vec![1, 1])),
            (50720, Val::Long(vec![w as u32, h as u32])),
            (50721, Val::SRational(COLOR_MATRIX.iter().flatten().copied().collect())),
            (50728, Val::Rational(NEUTRAL.map(rational).to_vec())),
            (50730, Val::SRational(vec![0.25])),
            (50778, Val::Short(vec![21])),
        ]
    };
    // Lay out once to learn the sizes, then for real.
    let ifd0_len = ifd(entries(0, 0), 8).len();
    let exif_start = 8 + ifd0_len + ifd0_len % 2;
    let exif_bytes = ifd(exif, exif_start);
    let strip_start = exif_start + exif_bytes.len();
    let mut out = b"II*\0".to_vec();
    out.extend(8u32.to_le_bytes());
    out.extend(ifd(entries(exif_start as u32, strip_start as u32), 8));
    out.resize(exif_start, 0);
    out.extend(exif_bytes);
    assert_eq!(out.len(), strip_start);
    out.extend(strip);
    std::fs::write(path, out).unwrap();
}

// ---------------------------------------------------------------------------
// Metrics

/// Luminance-ish sum of the channels.
pub fn sum(p: [f32; 3]) -> f64 {
    p.iter().map(|&c| c as f64).sum()
}

/// Mean |log₂(out / truth)| over `block`² blocks (of the channel sums) where
/// `keep(x, y)` holds for every pixel, and the number of blocks used.
pub fn block_log_error(out: &Image, truth: &Image, block: usize, keep: impl Fn(usize, usize) -> bool + Sync) -> (f64, usize) {
    let (bw, bh) = (out.width / block, out.height / block);
    let errs: Vec<f64> = (0..bw * bh)
        .into_par_iter()
        .filter_map(|b| {
            let (x0, y0) = ((b % bw) * block, (b / bw) * block);
            let (mut so, mut st) = (0.0, 0.0);
            for y in y0..y0 + block {
                for x in x0..x0 + block {
                    if !keep(x, y) {
                        return None;
                    }
                    so += sum(out.get(x, y));
                    st += sum(truth.get(x, y));
                }
            }
            (st > 0.0 && so > 0.0).then(|| (so / st).log2().abs())
        })
        .collect();
    (errs.iter().sum::<f64>() / errs.len().max(1) as f64, errs.len())
}

/// Mean and standard deviation of the channel sums in a rectangle.
pub fn stats(img: &Image, x0: usize, y0: usize, x1: usize, y1: usize) -> (f64, f64) {
    let v: Vec<f64> = (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| sum(img.get(x, y))).collect();
    let m = v.iter().sum::<f64>() / v.len() as f64;
    let var = v.iter().map(|a| (a - m) * (a - m)).sum::<f64>() / (v.len() - 1) as f64;
    (m, var.sqrt())
}
