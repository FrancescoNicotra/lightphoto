//! Output metadata: a compact sRGB ICC profile and an EXIF block.

use raw::color::{self, D50, D65};
use raw::PhotoInfo;

// ---------------------------------------------------------------------------
// ICC

fn s15f16(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

fn xyz_tag(v: [f64; 3]) -> Vec<u8> {
    [b"XYZ ".as_slice(), &[0; 4], &s15f16(v[0]), &s15f16(v[1]), &s15f16(v[2])].concat()
}

fn pad4(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
}

/// An ICC v2 display profile for sRGB (IEC 61966-2.1): D50-adapted
/// colorants and a 1024-point transfer curve.
pub fn srgb_icc() -> Vec<u8> {
    let desc = {
        let text = b"sRGB IEC61966-2.1 (Lumen)\0";
        let mut d = [b"desc".as_slice(), &[0; 4], &(text.len() as u32).to_be_bytes(), text].concat();
        d.extend([0; 4 + 4 + 2 + 1 + 67]); // empty Unicode and ScriptCode parts
        d
    };
    let cprt = [b"text".as_slice(), &[0; 4], b"No copyright, use freely\0"].concat();
    let to_xyz = color::mul(&color::adaptation(D65, D50), &color::rgb_to_xyz(color::SRGB_PRIMARIES, D65));
    let column = |i: usize| [to_xyz[0][i], to_xyz[1][i], to_xyz[2][i]];
    let trc = {
        let n = 1024u32;
        let mut t = [b"curv".as_slice(), &[0; 4], &n.to_be_bytes()].concat();
        for i in 0..n {
            let v = color::srgb_eotf(i as f32 / (n - 1) as f32);
            t.extend(((v * 65535.0).round() as u16).to_be_bytes());
        }
        t
    };
    let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"desc", desc),
        (b"cprt", cprt),
        (b"wtpt", xyz_tag(D50.to_xyz())),
        (b"rXYZ", xyz_tag(column(0))),
        (b"gXYZ", xyz_tag(column(1))),
        (b"bXYZ", xyz_tag(column(2))),
        (b"rTRC", trc),
    ];
    // gTRC and bTRC share rTRC's data.
    let count = tags.len() + 2;
    let mut data = Vec::new();
    let mut table = Vec::new();
    let data_start = 128 + 4 + count * 12;
    let mut trc_entry = (0u32, 0u32);
    for (sig, body) in &tags {
        let offset = (data_start + data.len()) as u32;
        table.push((**sig, offset, body.len() as u32));
        if *sig == b"rTRC" {
            trc_entry = (offset, body.len() as u32);
        }
        data.extend(body);
        pad4(&mut data);
    }
    table.push((*b"gTRC", trc_entry.0, trc_entry.1));
    table.push((*b"bTRC", trc_entry.0, trc_entry.1));

    let size = (data_start + data.len()) as u32;
    let mut out = Vec::with_capacity(size as usize);
    out.extend(size.to_be_bytes());
    out.extend([0; 4]); // preferred CMM
    out.extend([0x02, 0x10, 0, 0]); // version 2.1
    out.extend(b"mntrRGB XYZ ");
    for v in [2026u16, 1, 1, 0, 0, 0] {
        out.extend(v.to_be_bytes());
    }
    out.extend(b"acsp");
    out.extend([0; 4 * 4 + 8]); // platform, flags, manufacturer, model, attributes
    out.extend(0u32.to_be_bytes()); // perceptual intent
    for v in D50.to_xyz() {
        out.extend(s15f16(v));
    }
    out.extend([0; 4 + 16 + 28]); // creator, profile ID, reserved
    debug_assert_eq!(out.len(), 128);
    out.extend((count as u32).to_be_bytes());
    for (sig, offset, len) in table {
        out.extend(sig);
        out.extend(offset.to_be_bytes());
        out.extend(len.to_be_bytes());
    }
    out.extend(data);
    out
}

// ---------------------------------------------------------------------------
// EXIF

enum Field {
    Ascii(String),
    Short(u16),
    Long(u32),
    Rational(u32, u32),
}

fn rational(v: f64) -> Field {
    // Exposure times like 1/500 s keep a unit numerator.
    if v > 0.0 && v < 1.0 && (1.0 / v - (1.0 / v).round()).abs() < 1e-3 {
        Field::Rational(1, (1.0 / v).round() as u32)
    } else {
        Field::Rational((v * 1000.0).round() as u32, 1000)
    }
}

/// One IFD followed by its out-of-line values, laid out at `start`.
fn ifd(mut entries: Vec<(u16, Field)>, start: usize) -> Vec<u8> {
    entries.sort_by_key(|e| e.0);
    let mut head = (entries.len() as u16).to_le_bytes().to_vec();
    let data_start = start + 2 + entries.len() * 12 + 4;
    let mut data = Vec::new();
    for (tag, field) in &entries {
        let (ty, count, bytes): (u16, u32, Vec<u8>) = match field {
            Field::Ascii(s) => (2, s.len() as u32 + 1, [s.as_bytes(), &[0]].concat()),
            Field::Short(v) => (3, 1, v.to_le_bytes().to_vec()),
            Field::Long(v) => (4, 1, v.to_le_bytes().to_vec()),
            Field::Rational(n, d) => (5, 1, [n.to_le_bytes(), d.to_le_bytes()].concat()),
        };
        head.extend(tag.to_le_bytes());
        head.extend(ty.to_le_bytes());
        head.extend(count.to_le_bytes());
        if bytes.len() <= 4 {
            let mut inline = bytes;
            inline.resize(4, 0);
            head.extend(inline);
        } else {
            head.extend(((data_start + data.len()) as u32).to_le_bytes());
            data.extend(bytes);
            if data.len() % 2 == 1 {
                data.push(0);
            }
        }
    }
    head.extend(0u32.to_le_bytes()); // no next IFD
    head.extend(data);
    head
}

/// A little-endian TIFF block with the essential EXIF fields (without the
/// "Exif\0\0" prefix). Orientation is 1: the pixels are already upright.
pub fn exif(info: &PhotoInfo) -> Vec<u8> {
    let ascii = |s: &str| (!s.trim().is_empty()).then(|| Field::Ascii(s.trim().to_string()));
    let ifd0 = |exif_at: u32| {
        let mut e = vec![(274, Field::Short(1)), (305, Field::Ascii("Lumen".into())), (34665, Field::Long(exif_at))];
        e.extend(ascii(&info.make).map(|f| (271, f)));
        e.extend(ascii(&info.model).map(|f| (272, f)));
        e
    };
    let mut sub: Vec<(u16, Field)> = vec![(40961, Field::Short(1))]; // ColorSpace = sRGB
    sub.extend(info.exposure_time.map(|v| (33434, rational(v))));
    sub.extend(info.f_number.map(|v| (33437, rational(v))));
    sub.extend(info.iso.map(|v| (34855, Field::Short(v.min(65535) as u16))));
    sub.extend(info.date_time.as_deref().and_then(ascii).map(|f| (36867, f)));
    sub.extend(info.focal_length.map(|v| (37386, rational(v))));
    sub.extend(info.lens.as_deref().and_then(ascii).map(|f| (42036, f)));

    // IFD0's size does not depend on the pointer value, so lay it out once
    // to find where the Exif IFD starts.
    let exif_at = 8 + ifd(ifd0(0), 8).len();
    let mut out = b"II*\0".to_vec();
    out.extend(8u32.to_le_bytes());
    out.extend(ifd(ifd0(exif_at as u32), 8));
    out.extend(ifd(sub, exif_at));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be32(b: &[u8], o: usize) -> u32 {
        u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
    }

    #[test]
    fn icc_profile_is_well_formed() {
        let p = srgb_icc();
        assert_eq!(be32(&p, 0) as usize, p.len());
        assert_eq!(&p[36..40], b"acsp");
        let count = be32(&p, 128) as usize;
        assert_eq!(count, 9);
        for i in 0..count {
            let e = 132 + i * 12;
            let (off, len) = (be32(&p, e + 4) as usize, be32(&p, e + 8) as usize);
            assert_eq!(off % 4, 0);
            assert!(off + len <= p.len(), "tag {i}");
        }
        // Colorants sum to the D50 white.
        let tag = |sig: &[u8; 4]| {
            let e = (0..count).map(|i| 132 + i * 12).find(|&e| &p[e..e + 4] == sig).unwrap();
            let off = be32(&p, e + 4) as usize;
            (0..3).map(|k| i32::from_be_bytes(p[off + 8 + 4 * k..off + 12 + 4 * k].try_into().unwrap()) as f64 / 65536.0).collect::<Vec<_>>()
        };
        let (r, g, b) = (tag(b"rXYZ"), tag(b"gXYZ"), tag(b"bXYZ"));
        let sum: Vec<f64> = (0..3).map(|k| r[k] + g[k] + b[k]).collect();
        let d50 = D50.to_xyz();
        assert!(sum.iter().zip(d50).all(|(a, b)| (a - b).abs() < 2e-3), "{sum:?}");
    }

    #[test]
    fn exif_points_to_sub_ifd() {
        let info = PhotoInfo {
            make: "Panasonic".into(),
            model: "DMC-GX80".into(),
            exposure_time: Some(1.0 / 500.0),
            f_number: Some(3.5),
            iso: Some(200),
            ..Default::default()
        };
        let e = exif(&info);
        assert_eq!(&e[..4], b"II*\0");
        let le16 = |o: usize| u16::from_le_bytes([e[o], e[o + 1]]);
        let le32 = |o: usize| u32::from_le_bytes(e[o..o + 4].try_into().unwrap());
        let n = le16(8) as usize;
        let ptr = (0..n).map(|i| 10 + i * 12).find(|&o| le16(o) == 34665).map(|o| le32(o + 8)).unwrap() as usize;
        let sub_n = le16(ptr) as usize;
        let tags: Vec<u16> = (0..sub_n).map(|i| le16(ptr + 2 + i * 12)).collect();
        assert_eq!(tags, vec![33434, 33437, 34855, 40961]);
        let exposure = (0..sub_n).map(|i| ptr + 2 + i * 12).find(|&o| le16(o) == 33434).unwrap();
        let at = le32(exposure + 8) as usize;
        assert_eq!((le32(at), le32(at + 4)), (1, 500));
    }
}
