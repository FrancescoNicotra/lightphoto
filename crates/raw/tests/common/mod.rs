//! Shared by the integration tests: a minimal TIFF writer and a synthetic
//! Bayer DNG, so no real RAW file has to live in the repository.

#![allow(dead_code)]

use std::path::PathBuf;

use raw::color::Mat3;
use raw::profile::ColorProfile;
use raw::TempTint;

pub const BLACK: u32 = 512;
pub const WHITE: u32 = 16383;
pub const SIZE: (usize, usize) = (64, 48);
/// x, y, w, h: the size is the sensor minus 8 pixels.
pub const CROP: [u32; 4] = [2, 2, 56, 40];
pub const COLOR_MATRIX: Mat3 = [[0.7771, -0.3020, -0.0629], [-0.4029, 1.1950, 0.2345], [-0.0821, 0.1977, 0.6119]];
pub const GREY: f64 = 0.18;

pub enum Val {
    Short(Vec<u16>),
    Long(Vec<u32>),
    Byte(Vec<u8>),
    Ascii(&'static str),
    SRational(Vec<f64>),
    Rational(Vec<f64>),
}

/// Minimal little-endian TIFF writer: one IFD, one strip.
pub fn tiff(mut entries: Vec<(u16, Val)>, strip: &[u8]) -> Vec<u8> {
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
pub fn write_dng(name: &str, (w, h): (usize, usize)) -> (PathBuf, [f64; 3]) {
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

