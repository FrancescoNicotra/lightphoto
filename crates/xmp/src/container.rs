//! Locating the XMP packet inside the files presets ship in: plain .xmp
//! sidecars, DNG/TIFF (tag 700) and JPEG (APP1 segment).

const TIFF_TAG_XMP: u16 = 700;
const JPEG_XMP_HEADER: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

/// Returns the XMP document embedded in (or making up) `bytes`.
pub fn extract_xmp(bytes: &[u8]) -> Option<String> {
    let raw = if is_tiff(bytes) {
        tiff_xmp(bytes)
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        jpeg_xmp(bytes)
    } else {
        None
    };
    let raw = raw.unwrap_or(bytes);
    trim_to_packet(&String::from_utf8_lossy(raw))
}

fn is_tiff(b: &[u8]) -> bool {
    b.starts_with(b"II*\0") || b.starts_with(b"MM\0*")
}

/// Reads tag 700 from IFD0. DNG writers always put XMP there.
fn tiff_xmp(b: &[u8]) -> Option<&[u8]> {
    let le = b[0] == b'I';
    let u16_at = |o: usize| -> Option<u16> {
        let s: [u8; 2] = b.get(o..o + 2)?.try_into().ok()?;
        Some(if le { u16::from_le_bytes(s) } else { u16::from_be_bytes(s) })
    };
    let u32_at = |o: usize| -> Option<u32> {
        let s: [u8; 4] = b.get(o..o + 4)?.try_into().ok()?;
        Some(if le { u32::from_le_bytes(s) } else { u32::from_be_bytes(s) })
    };

    let ifd = u32_at(4)? as usize;
    let count = u16_at(ifd)? as usize;
    for i in 0..count {
        let entry = ifd + 2 + i * 12;
        if u16_at(entry)? != TIFF_TAG_XMP {
            continue;
        }
        let len = u32_at(entry + 4)? as usize;
        // BYTE/UNDEFINED payloads up to 4 bytes live inline in the entry.
        let start = if len <= 4 { entry + 8 } else { u32_at(entry + 8)? as usize };
        return b.get(start..start.checked_add(len)?);
    }
    None
}

fn jpeg_xmp(b: &[u8]) -> Option<&[u8]> {
    let mut pos = 2;
    while pos + 4 <= b.len() && b[pos] == 0xFF {
        let marker = b[pos + 1];
        if marker == 0xDA {
            break; // start of scan: no more metadata segments
        }
        let len = u16::from_be_bytes([b[pos + 2], b[pos + 3]]) as usize;
        let body = b.get(pos + 4..pos + 2 + len)?;
        if marker == 0xE1 && body.starts_with(JPEG_XMP_HEADER) {
            return Some(&body[JPEG_XMP_HEADER.len()..]);
        }
        pos += 2 + len;
    }
    None
}

/// Cuts the text down to the outermost x:xmpmeta element (dropping
/// xpacket wrappers and padding); bare rdf:RDF documents are kept as is.
fn trim_to_packet(text: &str) -> Option<String> {
    for (open, close) in [("<x:xmpmeta", "</x:xmpmeta>"), ("<x:xapmeta", "</x:xapmeta>")] {
        if let Some(start) = text.find(open) {
            let end = text[start..].find(close)? + start + close.len();
            return Some(text[start..end].to_string());
        }
    }
    text.contains("<rdf:RDF").then(|| text.trim().to_string())
}
