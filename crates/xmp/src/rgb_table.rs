//! Camera Raw "RGB tables": the 3D LUTs inside creative profiles
//! (`crs:RGBTable="<md5>"` + `crs:Table_<md5>="<blob>"`).
//!
//! Blob encoding (DNG SDK, dng_big_table):
//!   text  = Adobe base85 (own alphabet, 5 chars -> little-endian u32,
//!           first char least significant; a trailing group of n chars
//!           carries n-1 bytes)
//!   bytes = u32 LE uncompressed length + zlib stream
//!   table = u32 type (1 = RGB table), u32 version (1), u32 dimensions
//!           (1 or 3), u32 divisions, samples as u16 RGB deltas from the
//!           identity ramp (3D order: r outer, g, b inner), then u32
//!           primaries, u32 gamma, u32 gamut, f64 min amount, f64 max
//!           amount, optional u32 flags. All little-endian.

use std::io::{Read, Write};

use crate::Error;

const ALPHABET: &[u8; 85] =
    b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ.-:+=^!/*?`'|()[]{}@%$#";

const TABLE_TYPE_RGB: u32 = 1;
const TABLE_VERSION: u32 = 1;

fn digit(c: u8) -> Option<u32> {
    ALPHABET.iter().position(|&a| a == c).map(|p| p as u32)
}

pub fn decode_base85(text: &str) -> Vec<u8> {
    let digits: Vec<u32> = text.bytes().filter_map(digit).collect();
    let mut out = Vec::with_capacity(digits.len() * 4 / 5);
    for group in digits.chunks(5) {
        let mut value: u32 = 0;
        for &d in group.iter().rev() {
            value = value.wrapping_mul(85).wrapping_add(d);
        }
        let bytes = value.to_le_bytes();
        out.extend_from_slice(&bytes[..group.len().saturating_sub(1).min(4)]);
    }
    out
}

pub fn encode_base85(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 5 / 4 + 5);
    for chunk in bytes.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        let mut value = u32::from_le_bytes(word);
        for _ in 0..chunk.len() + 1 {
            out.push(ALPHABET[(value % 85) as usize] as char);
            value /= 85;
        }
    }
    out
}

/// Text blob -> uncompressed table bytes.
pub fn decode_blob(text: &str) -> Result<Vec<u8>, Error> {
    let bytes = decode_base85(text);
    if bytes.len() < 4 {
        return Err(Error::Invalid("table blob too short".into()));
    }
    let expected = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    let mut out = Vec::with_capacity(expected);
    flate2::read::ZlibDecoder::new(&bytes[4..])
        .read_to_end(&mut out)
        .map_err(|e| Error::Invalid(format!("table zlib: {e}")))?;
    if out.len() != expected {
        return Err(Error::Invalid(format!("table size {} != declared {expected}", out.len())));
    }
    Ok(out)
}

pub fn encode_blob(table: &[u8]) -> String {
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    z.write_all(table).expect("in-memory write");
    let mut bytes = (table.len() as u32).to_le_bytes().to_vec();
    bytes.extend(z.finish().expect("in-memory write"));
    encode_base85(&bytes)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Primaries {
    Srgb,
    AdobeRgb,
    ProPhoto,
    DisplayP3,
    Rec2020,
    Other(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gamma {
    Linear,
    Srgb,
    G1_8,
    G2_2,
    Rec2020,
    Other(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gamut {
    Clip,
    Extend,
    Other(u32),
}

// Enum codes as in the DNG SDK headers (dng_rgb_table); still to be
// confirmed against a real profile exported by Camera Raw.
impl Primaries {
    fn from_code(c: u32) -> Self {
        [Self::Srgb, Self::AdobeRgb, Self::ProPhoto, Self::DisplayP3, Self::Rec2020]
            .get(c as usize)
            .copied()
            .unwrap_or(Self::Other(c))
    }
    fn code(self) -> u32 {
        match self {
            Self::Srgb => 0,
            Self::AdobeRgb => 1,
            Self::ProPhoto => 2,
            Self::DisplayP3 => 3,
            Self::Rec2020 => 4,
            Self::Other(c) => c,
        }
    }
}

impl Gamma {
    fn from_code(c: u32) -> Self {
        [Self::Linear, Self::Srgb, Self::G1_8, Self::G2_2, Self::Rec2020]
            .get(c as usize)
            .copied()
            .unwrap_or(Self::Other(c))
    }
    fn code(self) -> u32 {
        match self {
            Self::Linear => 0,
            Self::Srgb => 1,
            Self::G1_8 => 2,
            Self::G2_2 => 3,
            Self::Rec2020 => 4,
            Self::Other(c) => c,
        }
    }
}

impl Gamut {
    fn from_code(c: u32) -> Self {
        match c {
            0 => Self::Clip,
            1 => Self::Extend,
            c => Self::Other(c),
        }
    }
    fn code(self) -> u32 {
        match self {
            Self::Clip => 0,
            Self::Extend => 1,
            Self::Other(c) => c,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RgbTable {
    /// 1 (three 1D curves) or 3 (3D cube).
    pub dimensions: u32,
    pub divisions: u32,
    /// 16-bit samples; 3D index = (r * d + g) * d + b.
    pub samples: Vec<[u16; 3]>,
    pub primaries: Primaries,
    pub gamma: Gamma,
    pub gamut: Gamut,
    pub min_amount: f64,
    pub max_amount: f64,
    pub flags: Option<u32>,
}

fn ramp(divisions: u32) -> Vec<u16> {
    let d = divisions as u64;
    (0..d).map(|i| ((i * 0xFFFF + (d >> 1)) / (d - 1)) as u16).collect()
}

struct Cursor<'a>(&'a [u8]);

impl Cursor<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        if self.0.len() < N {
            return Err(Error::Invalid("rgb table truncated".into()));
        }
        let (head, rest) = self.0.split_at(N);
        self.0 = rest;
        Ok(head.try_into().unwrap())
    }
    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn f64(&mut self) -> Result<f64, Error> {
        Ok(f64::from_le_bytes(self.take()?))
    }
}

impl RgbTable {
    pub fn from_blob(text: &str) -> Result<Self, Error> {
        Self::from_bytes(&decode_blob(text)?)
    }

    pub fn to_blob(&self) -> String {
        encode_blob(&self.to_bytes())
    }

    pub fn identity(divisions: u32) -> Self {
        let r = ramp(divisions);
        let mut samples = Vec::with_capacity(r.len().pow(3));
        for &rr in &r {
            for &gg in &r {
                for &bb in &r {
                    samples.push([rr, gg, bb]);
                }
            }
        }
        RgbTable {
            dimensions: 3,
            divisions,
            samples,
            primaries: Primaries::ProPhoto,
            gamma: Gamma::Srgb,
            gamut: Gamut::Clip,
            min_amount: 0.0,
            max_amount: 2.0,
            flags: None,
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let mut c = Cursor(bytes);
        let kind = c.u32()?;
        let version = c.u32()?;
        if kind != TABLE_TYPE_RGB || version != TABLE_VERSION {
            return Err(Error::Invalid(format!("unsupported table type {kind} version {version}")));
        }
        let dimensions = c.u32()?;
        let divisions = c.u32()?;
        if !(dimensions == 1 || dimensions == 3) || !(2..=256).contains(&divisions) {
            return Err(Error::Invalid(format!("bad table shape {dimensions}D x {divisions}")));
        }
        let nop = ramp(divisions);
        let mut samples = Vec::new();
        if dimensions == 1 {
            for &n in &nop {
                samples.push([c.u16()?.wrapping_add(n), c.u16()?.wrapping_add(n), c.u16()?.wrapping_add(n)]);
            }
        } else {
            samples.reserve(nop.len().pow(3));
            for &rn in &nop {
                for &gn in &nop {
                    for &bn in &nop {
                        samples.push([
                            c.u16()?.wrapping_add(rn),
                            c.u16()?.wrapping_add(gn),
                            c.u16()?.wrapping_add(bn),
                        ]);
                    }
                }
            }
        }
        let primaries = Primaries::from_code(c.u32()?);
        let gamma = Gamma::from_code(c.u32()?);
        let gamut = Gamut::from_code(c.u32()?);
        let min_amount = c.f64()?;
        let max_amount = c.f64()?;
        let flags = c.u32().ok();
        Ok(RgbTable { dimensions, divisions, samples, primaries, gamma, gamut, min_amount, max_amount, flags })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for v in [TABLE_TYPE_RGB, TABLE_VERSION, self.dimensions, self.divisions] {
            out.extend(v.to_le_bytes());
        }
        let nop = ramp(self.divisions);
        let d = nop.len();
        for (i, s) in self.samples.iter().enumerate() {
            let base = if self.dimensions == 1 {
                [nop[i]; 3]
            } else {
                [nop[i / (d * d)], nop[(i / d) % d], nop[i % d]]
            };
            for ch in 0..3 {
                out.extend(s[ch].wrapping_sub(base[ch]).to_le_bytes());
            }
        }
        for v in [self.primaries.code(), self.gamma.code(), self.gamut.code()] {
            out.extend(v.to_le_bytes());
        }
        out.extend(self.min_amount.to_le_bytes());
        out.extend(self.max_amount.to_le_bytes());
        if let Some(f) = self.flags {
            out.extend(f.to_le_bytes());
        }
        out
    }

    /// Sample at grid indices, normalized to 0..1.
    pub fn sample(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        let d = self.divisions as usize;
        let s = self.samples[(r * d + g) * d + b];
        s.map(|v| v as f32 / 65535.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base85_round_trip_all_lengths() {
        for n in 0..12 {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(decode_base85(&encode_base85(&data)), data, "len {n}");
        }
    }

    #[test]
    fn identity_table_stores_zero_deltas_and_round_trips() {
        let t = RgbTable::identity(9);
        let bytes = t.to_bytes();
        // Identity encodes to all-zero deltas after the 16-byte header.
        assert!(bytes[16..16 + 9 * 9 * 9 * 6].iter().all(|&b| b == 0));
        let back = RgbTable::from_blob(&t.to_blob()).unwrap();
        assert_eq!(back, t);
        let s = back.sample(8, 0, 4);
        assert_eq!(&s[..2], &[1.0, 0.0]);
        assert!((s[2] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn rejects_garbage() {
        assert!(RgbTable::from_blob("not a table").is_err());
    }
}
