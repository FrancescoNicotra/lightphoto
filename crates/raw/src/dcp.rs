//! DNG Camera Profiles (.dcp): the TIFF-like files Adobe ships for each
//! camera ("Adobe Standard", "Camera Neutral"…). Same tags as the profile
//! part of a DNG, in IFD0 of a file whose magic number is "RC" instead of 42.

use std::path::Path;

use crate::color::{self, Mat3};
use crate::hue_sat::HueSatMap;
use crate::profile::{illuminant_temperature, Calibration, ColorProfile};
use crate::Error;

const UNIQUE_CAMERA_MODEL: u16 = 50708;
const COLOR_MATRIX: [u16; 2] = [50721, 50722];
const CALIBRATION_ILLUMINANT: [u16; 2] = [50778, 50779];
const PROFILE_NAME: u16 = 50936;
const HUE_SAT_MAP_DIMS: u16 = 50937;
const HUE_SAT_MAP_DATA: [u16; 2] = [50938, 50939];
const TONE_CURVE: u16 = 50940;
const FORWARD_MATRIX: [u16; 2] = [50964, 50965];
const LOOK_TABLE_DIMS: u16 = 50981;
const LOOK_TABLE_DATA: u16 = 50982;
const HUE_SAT_MAP_ENCODING: u16 = 51107;
const LOOK_TABLE_ENCODING: u16 = 51108;
const BASELINE_EXPOSURE_OFFSET: u16 = 51109;

/// One IFD entry, values converted to f64 (or kept as text).
struct Entry {
    tag: u16,
    numbers: Vec<f64>,
    text: Option<String>,
}

fn read_ifd0(b: &[u8]) -> Result<Vec<Entry>, Error> {
    let bad = |what: &str| Error::Decode(format!("DCP: {what}"));
    let le = match b.get(..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return Err(bad("not a TIFF/DCP file")),
    };
    let u16_at = |o: usize| -> Option<u16> {
        let s: [u8; 2] = b.get(o..o + 2)?.try_into().ok()?;
        Some(if le { u16::from_le_bytes(s) } else { u16::from_be_bytes(s) })
    };
    let u32_at = |o: usize| -> Option<u32> {
        let s: [u8; 4] = b.get(o..o + 4)?.try_into().ok()?;
        Some(if le { u32::from_le_bytes(s) } else { u32::from_be_bytes(s) })
    };
    let magic = u16_at(2).ok_or_else(|| bad("truncated"))?;
    if magic != 0x4352 && magic != 42 {
        return Err(bad("unknown magic number"));
    }
    let ifd = u32_at(4).ok_or_else(|| bad("truncated"))? as usize;
    let count = u16_at(ifd).ok_or_else(|| bad("truncated IFD"))? as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let e = ifd + 2 + i * 12;
        let (tag, ty, n) = (u16_at(e), u16_at(e + 2), u32_at(e + 4));
        let (Some(tag), Some(ty), Some(n)) = (tag, ty, n) else { return Err(bad("truncated entry")) };
        let n = n as usize;
        let size = match ty {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 => 4,
            5 | 10 | 12 => 8,
            _ => continue,
        };
        let len = size * n;
        let start = if len <= 4 { e + 8 } else { u32_at(e + 8).ok_or_else(|| bad("truncated entry"))? as usize };
        let data = b.get(start..start + len).ok_or_else(|| bad("value out of range"))?;
        let word = |k: usize| u32_at(start + k * 4).unwrap_or(0);
        let numbers: Vec<f64> = match ty {
            1 | 7 => data.iter().map(|&v| v as f64).collect(),
            6 => data.iter().map(|&v| v as i8 as f64).collect(),
            3 => (0..n).map(|k| u16_at(start + k * 2).unwrap_or(0) as f64).collect(),
            8 => (0..n).map(|k| u16_at(start + k * 2).unwrap_or(0) as i16 as f64).collect(),
            4 => (0..n).map(|k| word(k) as f64).collect(),
            9 => (0..n).map(|k| word(k) as i32 as f64).collect(),
            11 => (0..n).map(|k| f32::from_bits(word(k)) as f64).collect(),
            5 => (0..n).map(|k| word(2 * k) as f64 / (word(2 * k + 1).max(1)) as f64).collect(),
            10 => (0..n).map(|k| word(2 * k) as i32 as f64 / (word(2 * k + 1) as i32).max(1) as f64).collect(),
            12 => (0..n)
                .map(|k| {
                    let (a, b2) = (word(2 * k) as u64, word(2 * k + 1) as u64);
                    f64::from_bits(if le { a | b2 << 32 } else { a << 32 | b2 })
                })
                .collect(),
            _ => Vec::new(),
        };
        let text = (ty == 2).then(|| String::from_utf8_lossy(data).trim_end_matches('\0').to_string());
        out.push(Entry { tag, numbers, text });
    }
    Ok(out)
}

/// A camera profile read from a .dcp file.
#[derive(Clone, Debug)]
pub struct Dcp {
    /// The camera the profile was made for (UniqueCameraModel).
    pub camera: String,
    pub profile: ColorProfile,
}

pub fn load(path: impl AsRef<Path>) -> Result<Dcp, Error> {
    parse(&std::fs::read(path)?)
}

pub fn parse(bytes: &[u8]) -> Result<Dcp, Error> {
    let entries = read_ifd0(bytes)?;
    let get = |tag: u16| entries.iter().find(|e| e.tag == tag);
    let nums = |tag: u16| get(tag).map(|e| e.numbers.as_slice());
    let mat3 = |tag: u16| nums(tag).filter(|v| v.len() == 9).map(|v| -> Mat3 { std::array::from_fn(|i| std::array::from_fn(|j| v[i * 3 + j])) });
    let floats = |tag: u16| nums(tag).map(|v| v.iter().map(|&x| x as f32).collect::<Vec<f32>>());
    let dims = |tag: u16| nums(tag).filter(|v| v.len() == 3).map(|v| [v[0] as usize, v[1] as usize, v[2] as usize]);

    let hsm_srgb = nums(HUE_SAT_MAP_ENCODING).and_then(|v| v.first()).is_some_and(|&e| e == 1.0);
    let hsm_dims = dims(HUE_SAT_MAP_DIMS);
    let mut calibrations = Vec::new();
    for i in 0..2 {
        let Some(color_matrix) = mat3(COLOR_MATRIX[i]) else { continue };
        let illuminant = nums(CALIBRATION_ILLUMINANT[i]).and_then(|v| v.first()).map_or(21, |&v| v as u16);
        calibrations.push(Calibration {
            temperature: illuminant_temperature(illuminant),
            color_matrix,
            forward_matrix: mat3(FORWARD_MATRIX[i]),
            camera_calibration: color::IDENTITY,
            hue_sat_map: hsm_dims.zip(floats(HUE_SAT_MAP_DATA[i])).and_then(|(d, f)| HueSatMap::new(d, &f, hsm_srgb)),
        });
    }
    if calibrations.is_empty() {
        return Err(Error::Decode("DCP: no ColorMatrix".into()));
    }
    if calibrations.iter().any(|c| c.forward_matrix.is_none()) {
        calibrations.iter_mut().for_each(|c| c.forward_matrix = None);
    }

    let mut profile = ColorProfile::new(calibrations, None);
    profile.name = get(PROFILE_NAME).and_then(|e| e.text.clone());
    let look_srgb = nums(LOOK_TABLE_ENCODING).and_then(|v| v.first()).is_some_and(|&e| e == 1.0);
    profile.look_table = dims(LOOK_TABLE_DIMS).zip(floats(LOOK_TABLE_DATA)).and_then(|(d, f)| HueSatMap::new(d, &f, look_srgb));
    profile.tone_curve = nums(TONE_CURVE)
        .filter(|v| v.len() >= 4 && v.len() % 2 == 0)
        .map(|v| v.as_chunks::<2>().0.iter().map(|p| (p[0] as f32, p[1] as f32)).collect());
    profile.baseline_exposure_offset = nums(BASELINE_EXPOSURE_OFFSET).and_then(|v| v.first().copied()).unwrap_or(0.0);
    Ok(Dcp { camera: get(UNIQUE_CAMERA_MODEL).and_then(|e| e.text.clone()).unwrap_or_default(), profile })
}

/// Just the camera name and first ColorMatrix, reading as little as possible
/// (for scanning a folder of profiles).
pub fn peek(path: impl AsRef<Path>) -> Option<(String, Mat3)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 8];
    f.read_exact(&mut head).ok()?;
    let le = &head[..2] == b"II";
    let u32_of = |b: [u8; 4]| if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) };
    let ifd = u32_of(head[4..8].try_into().ok()?) as u64;
    // The IFD and the small values it points to usually sit together: read
    // the IFD, then fetch the two values we need.
    f.seek(SeekFrom::Start(ifd)).ok()?;
    let mut n = [0u8; 2];
    f.read_exact(&mut n).ok()?;
    let count = if le { u16::from_le_bytes(n) } else { u16::from_be_bytes(n) } as usize;
    let mut table = vec![0u8; count * 12];
    f.read_exact(&mut table).ok()?;
    let mut fetch = |tag: u16| -> Option<Vec<u8>> {
        let e = table.as_chunks::<12>().0.iter().find(|e| {
            let t = [e[0], e[1]];
            (if le { u16::from_le_bytes(t) } else { u16::from_be_bytes(t) }) == tag
        })?;
        let n = u32_of(e[4..8].try_into().ok()?) as usize;
        let ty = if le { u16::from_le_bytes([e[2], e[3]]) } else { u16::from_be_bytes([e[2], e[3]]) };
        let len = n * if ty == 10 { 8 } else { 1 };
        if len <= 4 {
            return Some(e[8..8 + len].to_vec());
        }
        f.seek(SeekFrom::Start(u32_of(e[8..12].try_into().ok()?) as u64)).ok()?;
        let mut buf = vec![0u8; len];
        f.read_exact(&mut buf).ok()?;
        Some(buf)
    };
    let camera = String::from_utf8_lossy(&fetch(UNIQUE_CAMERA_MODEL)?).trim_end_matches('\0').to_string();
    let cm = fetch(COLOR_MATRIX[0])?;
    if cm.len() != 72 {
        return None;
    }
    let word = |k: usize| u32_of(cm[k * 4..k * 4 + 4].try_into().unwrap()) as i32 as f64;
    let m: Mat3 = std::array::from_fn(|i| std::array::from_fn(|j| word(2 * (i * 3 + j)) / word(2 * (i * 3 + j) + 1).max(1.0)));
    Some((camera, m))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny little-endian DCP with two matrices, a 2D hue/sat map and a curve.
    fn sample() -> Vec<u8> {
        let srational = |v: &[f64]| v.iter().flat_map(|x| [((x * 10000.0).round() as i32).to_le_bytes(), 10000i32.to_le_bytes()].concat()).collect::<Vec<u8>>();
        let floats = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let map: Vec<f32> = (0..6 * 2).flat_map(|_| [10.0f32, 1.0, 1.0]).collect();
        let cm = [0.8, -0.4, 0.0, -0.4, 1.2, 0.3, -0.1, 0.1, 0.7];
        let entries: Vec<(u16, u16, u32, Vec<u8>)> = vec![
            (UNIQUE_CAMERA_MODEL, 2, 8, b"Test Cam".to_vec()),
            (COLOR_MATRIX[0], 10, 9, srational(&cm)),
            (COLOR_MATRIX[1], 10, 9, srational(&cm)),
            (CALIBRATION_ILLUMINANT[0], 3, 1, 17u16.to_le_bytes().to_vec()),
            (CALIBRATION_ILLUMINANT[1], 3, 1, 21u16.to_le_bytes().to_vec()),
            (PROFILE_NAME, 2, 15, b"Adobe Standard\0".to_vec()),
            (HUE_SAT_MAP_DIMS, 4, 3, [6u32, 2, 1].iter().flat_map(|v| v.to_le_bytes()).collect()),
            (HUE_SAT_MAP_DATA[0], 11, 36, floats(&map)),
            (HUE_SAT_MAP_DATA[1], 11, 36, floats(&map)),
            (TONE_CURVE, 11, 4, floats(&[0.0, 0.0, 1.0, 1.0])),
        ];
        let ifd_len = 2 + entries.len() * 12 + 4;
        let mut data = Vec::new();
        let mut ifd = (entries.len() as u16).to_le_bytes().to_vec();
        for (tag, ty, n, bytes) in &entries {
            ifd.extend(tag.to_le_bytes());
            ifd.extend(ty.to_le_bytes());
            ifd.extend(n.to_le_bytes());
            if bytes.len() <= 4 {
                let mut b = bytes.clone();
                b.resize(4, 0);
                ifd.extend(b);
            } else {
                ifd.extend(((8 + ifd_len + data.len()) as u32).to_le_bytes());
                data.extend(bytes);
            }
        }
        ifd.extend(0u32.to_le_bytes());
        [b"IIRC".as_slice(), &8u32.to_le_bytes(), &ifd, &data].concat()
    }

    #[test]
    fn parses_profile_tags() {
        let dcp = parse(&sample()).unwrap();
        assert_eq!(dcp.camera, "Test Cam");
        let p = &dcp.profile;
        assert_eq!(p.name.as_deref(), Some("Adobe Standard"));
        assert_eq!(p.calibrations.len(), 2);
        assert_eq!((p.calibrations[0].temperature, p.calibrations[1].temperature), (2850.0, 6500.0));
        assert!((p.calibrations[0].color_matrix[1][1] - 1.2).abs() < 1e-9);
        let map = p.hue_sat_map(color::D65).unwrap();
        assert_eq!((map.hue_divisions, map.sat_divisions, map.val_divisions), (6, 2, 1));
        assert_eq!(map.data[0], [10.0, 1.0, 1.0]);
        assert_eq!(p.tone_curve.as_deref(), Some(&[(0.0, 0.0), (1.0, 1.0)][..]));
    }

    #[test]
    fn peek_reads_name_and_matrix() {
        let path = std::env::temp_dir().join(format!("lumen-test-{}.dcp", std::process::id()));
        std::fs::write(&path, sample()).unwrap();
        let (camera, m) = peek(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(camera, "Test Cam");
        assert!((m[0][0] - 0.8).abs() < 1e-9 && (m[2][2] - 0.7).abs() < 1e-9);
    }
}
