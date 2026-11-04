//! Hue/saturation look tables embedded in Camera Raw profiles
//! (`crs:LookTable="<md5>"` + `crs:Table_<md5>="<blob>"`), e.g. the one that
//! turns Adobe Standard into "Adobe Color".
//!
//! The blob uses the same base85 + zlib wrapper as RGB tables. The layout
//! was inferred from Adobe Color.xmp (sizes add up exactly):
//! `u32 0, u32 1, u32 hue divisions, u32 saturation divisions, u32 value
//! divisions`, then one `f32` triple (hue shift in degrees, saturation scale,
//! value scale) per entry, value-major then hue then saturation (as DNG
//! ProfileLookTableData), then an optional `u32` value encoding (0 linear,
//! 1 sRGB, as ProfileLookTableEncoding).

use crate::packet::{Props, Value, NS_CRS};
use crate::rgb_table::decode_blob;
use crate::Error;

#[derive(Clone, Debug, PartialEq)]
pub struct LookTable {
    pub hue_divisions: usize,
    pub sat_divisions: usize,
    pub val_divisions: usize,
    /// (hue shift °, saturation scale, value scale) per entry.
    pub data: Vec<[f32; 3]>,
    pub srgb_encoded: bool,
}

impl LookTable {
    pub fn from_blob(text: &str) -> Result<Self, Error> {
        Self::from_bytes(&decode_blob(text)?)
    }

    pub fn from_bytes(b: &[u8]) -> Result<Self, Error> {
        let u32_at = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap()));
        let bad = |m: &str| Error::Invalid(format!("look table: {m}"));
        let header: Vec<u32> = (0..5).map(|i| u32_at(i * 4)).collect::<Option<_>>().ok_or_else(|| bad("truncated"))?;
        if header[1] != 1 {
            return Err(bad(&format!("unknown version {}", header[1])));
        }
        let [h, s, v] = [header[2], header[3], header[4]].map(|d| d as usize);
        let n = h * s * v.max(1);
        let end = 20 + n * 12;
        if h == 0 || s < 2 || b.len() < end || b.len() > end + 8 {
            return Err(bad(&format!("{} bytes for {h}×{s}×{v}", b.len())));
        }
        let f = |o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let data = (0..n).map(|i| [f(20 + i * 12), f(24 + i * 12), f(28 + i * 12)]).collect();
        Ok(LookTable { hue_divisions: h, sat_divisions: s, val_divisions: v.max(1), data, srgb_encoded: u32_at(end) == Some(1) })
    }

    /// The table as flat floats, for `raw::hue_sat::HueSatMap::new`.
    pub fn floats(&self) -> Vec<f32> {
        self.data.iter().flatten().copied().collect()
    }
}

/// `crs:LookTable` names the md5 of the table; the data sits next to it in
/// `crs:Table_<md5>` (absent when the profile is only referenced).
pub fn find_look_table(p: &Props) -> Result<Option<LookTable>, Error> {
    let Some(md5) = p.get(NS_CRS, "LookTable").and_then(Value::as_text) else { return Ok(None) };
    match p.get(NS_CRS, &format!("Table_{md5}")).and_then(Value::as_text) {
        Some(blob) => LookTable::from_blob(blob).map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rgb_table::encode_blob;

    fn bytes(dims: [u32; 3], entry: [f32; 3], encoding: Option<u32>) -> Vec<u8> {
        let n = (dims[0] * dims[1] * dims[2]) as usize;
        let mut b: Vec<u8> = [0u32, 1, dims[0], dims[1], dims[2]].iter().flat_map(|v| v.to_le_bytes()).collect();
        for _ in 0..n {
            b.extend(entry.iter().flat_map(|v| v.to_le_bytes()));
        }
        if let Some(e) = encoding {
            b.extend(e.to_le_bytes());
        }
        b
    }

    #[test]
    fn decodes_blob() {
        let blob = encode_blob(&bytes([36, 8, 16], [3.0, 1.05, 1.0], Some(1)));
        let t = LookTable::from_blob(&blob).unwrap();
        assert_eq!((t.hue_divisions, t.sat_divisions, t.val_divisions), (36, 8, 16));
        assert_eq!(t.data.len(), 36 * 8 * 16);
        assert_eq!(t.data[100], [3.0, 1.05, 1.0]);
        assert!(t.srgb_encoded);
        assert!(!LookTable::from_bytes(&bytes([6, 2, 1], [0.0, 1.0, 1.0], None)).unwrap().srgb_encoded);
    }

    #[test]
    fn rejects_bad_sizes() {
        let mut b = bytes([6, 2, 1], [0.0, 1.0, 1.0], None);
        b.truncate(b.len() - 4);
        assert!(LookTable::from_bytes(&b).is_err());
    }
}
