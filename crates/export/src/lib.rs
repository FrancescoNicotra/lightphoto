//! Export: output size, Lanczos-3 resize in linear light, grain after the
//! resize, sRGB encoding with an embedded ICC profile and essential EXIF.

pub mod meta;
pub mod resize;

use std::io::BufWriter;
use std::path::Path;

use image::ImageEncoder;
use pipeline::Pipeline;
use raw::{Image, PhotoInfo};

/// Output size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Size {
    Original,
    /// Long edge in pixels (2048, 1600, 1080…).
    LongEdge(usize),
    /// Percentage of the original.
    Percent(f32),
    /// Fit inside a box, keeping the aspect ratio.
    Fit(usize, usize),
}

impl std::str::FromStr for Size {
    type Err = String;

    /// "original", "2048", "50%" or "1920x1080".
    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim().to_ascii_lowercase();
        let bad = || format!("invalid size {s:?}: use original, 2048, 50% or 1920x1080");
        if s == "original" || s == "full" {
            Ok(Size::Original)
        } else if let Some(p) = s.strip_suffix('%') {
            p.parse::<f32>().ok().filter(|p| *p > 0.0).map(Size::Percent).ok_or_else(bad)
        } else if let Some((w, h)) = s.split_once('x') {
            Ok(Size::Fit(w.parse().map_err(|_| bad())?, h.parse().map_err(|_| bad())?))
        } else {
            s.parse().ok().filter(|n| *n > 0).map(Size::LongEdge).ok_or_else(bad)
        }
    }
}

impl Size {
    /// Output dimensions for a `w`×`h` image; never upscales.
    pub fn dimensions(self, w: usize, h: usize) -> (usize, usize) {
        let s = match self {
            Size::Original => 1.0,
            Size::LongEdge(n) => n as f64 / w.max(h) as f64,
            Size::Percent(p) => p as f64 / 100.0,
            Size::Fit(bw, bh) => (bw as f64 / w as f64).min(bh as f64 / h as f64),
        }
        .min(1.0);
        (((w as f64 * s).round() as usize).max(1), ((h as f64 * s).round() as usize).max(1))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// 8 bit, sRGB ICC, EXIF.
    Jpeg { quality: u8 },
    /// 16 bit, sRGB ICC, EXIF.
    Png,
    /// 16 bit, sRGB ICC.
    Tiff,
}

impl Format {
    pub fn from_path(path: &Path, quality: u8) -> Option<Format> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => Some(Format::Jpeg { quality }),
            "png" => Some(Format::Png),
            "tif" | "tiff" => Some(Format::Tiff),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExportOptions {
    pub size: Size,
    pub format: Format,
    /// Output sharpening for screen, 0 (off) to 100, applied after resizing.
    pub sharpen: f32,
}

/// Resizes a rendered image (display linear ProPhoto, see
/// [`Pipeline::render`]), adds grain and sharpening, and returns it ready
/// to encode. Takes the image: at the original size it is not copied.
pub fn finish(rendered: Image, pipeline: &Pipeline, opts: &ExportOptions) -> Image {
    let (w, h) = opts.size.dimensions(rendered.width, rendered.height);
    let mut img = resize::resize_owned(rendered, w, h);
    img.map(|p| p.map(|v| v.clamp(0.0, 1.0)));
    if opts.sharpen > 0.0 {
        img.map(|p| p.map(raw::color::srgb_oetf));
        pipeline::local::sharpen(&mut img, opts.sharpen, 0.6, 50.0, 0.0, 1.0);
        img.map(|p| p.map(|v| raw::color::srgb_eotf(v.clamp(0.0, 1.0))));
    }
    pipeline.grain(&mut img);
    img
}

/// Encodes a finished image (display linear ProPhoto) to `path`.
pub fn save(img: &Image, info: &PhotoInfo, format: Format, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = (img.width as u32, img.height as u32);
    let file = BufWriter::new(std::fs::File::create(path)?);
    match format {
        Format::Jpeg { quality } => {
            let buf = raw::encode_srgb8(img, 0.0);
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(file, quality.min(100));
            enc.set_icc_profile(meta::srgb_icc())?;
            enc.set_exif_metadata(meta::exif(info))?;
            enc.write_image(&buf, w, h, image::ExtendedColorType::Rgb8)?;
        }
        Format::Png => {
            let buf = raw::encode_srgb16(img, 0.0);
            let mut enc = image::codecs::png::PngEncoder::new(file);
            enc.set_icc_profile(meta::srgb_icc())?;
            enc.set_exif_metadata(meta::exif(info))?;
            enc.write_image(&ne_bytes(&buf), w, h, image::ExtendedColorType::Rgb16)?;
        }
        Format::Tiff => {
            let buf = raw::encode_srgb16(img, 0.0);
            let mut enc = image::codecs::tiff::TiffEncoder::new(file);
            enc.set_icc_profile(meta::srgb_icc())?;
            enc.write_image(&ne_bytes(&buf), w, h, image::ExtendedColorType::Rgb16)?;
        }
    }
    Ok(())
}

/// `write_image` takes 16-bit samples as native-endian bytes.
fn ne_bytes(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|s| s.to_ne_bytes()).collect()
}

/// Expands a file name template: `{name}` (input file stem), `{size}`
/// (output long edge) and `{preset}`.
pub fn file_name(template: &str, input: &Path, long_edge: usize, preset: Option<&str>) -> String {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("photo");
    template.replace("{name}", stem).replace("{size}", &long_edge.to_string()).replace("{preset}", preset.unwrap_or("none"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!("original".parse(), Ok(Size::Original));
        assert_eq!("2048".parse(), Ok(Size::LongEdge(2048)));
        assert_eq!("50%".parse(), Ok(Size::Percent(50.0)));
        assert_eq!("1920x1080".parse(), Ok(Size::Fit(1920, 1080)));
        assert!("big".parse::<Size>().is_err());
    }

    #[test]
    fn sizes_never_upscale() {
        assert_eq!(Size::LongEdge(1080).dimensions(4592, 3448), (1080, 811));
        assert_eq!(Size::LongEdge(8000).dimensions(4592, 3448), (4592, 3448));
        assert_eq!(Size::Percent(50.0).dimensions(4592, 3448), (2296, 1724));
        assert_eq!(Size::Fit(1000, 1000).dimensions(3448, 4592), (751, 1000));
    }

    #[test]
    fn templates() {
        let n = file_name("{name}-{preset}-{size}", Path::new("/a/P1010041.RW2"), 2048, Some("Dreamy"));
        assert_eq!(n, "P1010041-Dreamy-2048");
    }

    #[test]
    fn writes_all_formats_with_metadata() {
        let dir = std::env::temp_dir().join(format!("lumen-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = Image::from_fn(32, 20, |x, _| [x as f32 / 31.0, 0.3, 0.2]);
        let info = PhotoInfo { make: "Lumen".into(), model: "Test".into(), ..Default::default() };
        for (name, format) in [("a.jpg", Format::Jpeg { quality: 90 }), ("a.png", Format::Png), ("a.tif", Format::Tiff)] {
            let path = dir.join(name);
            save(&img, &info, format, &path).unwrap();
            let back = image::open(&path).unwrap();
            assert_eq!((back.width(), back.height()), (32, 20), "{name}");
        }
        let jpeg = std::fs::read(dir.join("a.jpg")).unwrap();
        assert!(jpeg.windows(12).any(|w| w == b"ICC_PROFILE\0"));
        assert!(jpeg.windows(6).any(|w| w == b"Exif\0\0"));
        std::fs::remove_dir_all(dir).ok();
    }
}
