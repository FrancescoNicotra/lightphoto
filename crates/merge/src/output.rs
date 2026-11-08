//! From the merged radiance to the DNG: output scale and
//! `BaselineExposure`, the initial XMP, the embedded preview, the file name.

use std::path::{Path, PathBuf};

use image::RgbImage;
use pipeline::adobe::{self, AdobeProfiles};
use pipeline::{Pipeline, RenderOptions, SourceKind};
use raw::{CameraData, Capture, ColorSource, Image, Orientation, Photo, PhotoInfo};
use rayon::prelude::*;
use xmp::{Crop, DevelopSettings};

/// The scale maps this percentile of the maximum channel to 1.0.
pub const WHITE_PERCENTILE: f64 = 0.9999;

/// The crop rectangle, in fractions of the image in sensor orientation
/// (Camera Raw's `crs:Crop*`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropRect {
    pub top: f32,
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
}

impl CropRect {
    pub fn to_crop(self) -> Crop {
        Crop { has_crop: true, top: self.top, left: self.left, bottom: self.bottom, right: self.right, angle: 0.0 }
    }
}

/// `s = 1 / percentile₉₉.₉₉(maxᶜ X)`: the written data `X · s` is ≤ 1 but for
/// the brightest 0.01%. 1 for an empty or black image.
pub fn output_scale(img: &Image) -> f32 {
    // A strided sample of ~2M pixels is plenty for the 99.99th percentile.
    let step = (img.pixels.len() / 2_000_000).max(1);
    let mut v: Vec<f32> = img
        .pixels
        .par_iter()
        .step_by(step)
        .map(|p| p[0].max(p[1]).max(p[2]))
        .filter(|m| m.is_finite() && *m > 0.0)
        .collect();
    if v.is_empty() {
        return 1.0;
    }
    let k = ((v.len() as f64 * WHITE_PERCENTILE) as usize).min(v.len() - 1);
    let (_, p, _) = v.select_nth_unstable_by(k, f32::total_cmp);
    1.0 / *p
}

/// `X · s`, limited to 0..1 (the few pixels above the percentile are
/// specular highlights; DNG readers disagree on values above white).
pub fn scaled(mut img: Image, s: f32) -> Image {
    img.map(|p| p.map(|c| if c > 0.0 { (c * s).min(1.0) } else { 0.0 }));
    img
}

/// The XMP packet embedded in the DNG: CreatorTool and the initial develop
/// settings (for now only the automatic crop).
pub fn xmp_packet(crop: Option<CropRect>) -> String {
    let crs = match crop {
        Some(c) => format!(
            "\n   xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\"\n   crs:HasCrop=\"True\"\n   crs:CropTop=\"{:.6}\"\n   crs:CropLeft=\"{:.6}\"\n   crs:CropBottom=\"{:.6}\"\n   crs:CropRight=\"{:.6}\"\n   crs:CropAngle=\"0\"",
            c.top, c.left, c.bottom, c.right
        ),
        None => String::new(),
    };
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  <rdf:Description rdf:about=\"\"\n   xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"\n   xmp:CreatorTool=\"Lumen {}\"{crs}/>\n </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>",
        env!("CARGO_PKG_VERSION")
    )
}

/// The embedded preview: the written data developed with the default
/// settings (plus the crop), in sensor orientation, ≤ 1024 px.
pub fn render_preview(data: &Image, reference: &Capture, baseline_exposure: f64, crop: Option<CropRect>, adobe_profiles: &AdobeProfiles) -> RgbImage {
    let mut info = PhotoInfo { orientation: Orientation::Normal, baseline_exposure, ..reference.info.clone() };
    if let Some(r) = &mut info.raw {
        r.float = true;
    }
    let mut photo = Photo {
        info,
        data: data.downscale(raw::dng_write::PREVIEW_SIZE as usize),
        color: ColorSource::Camera(Box::new(CameraData { profile: reference.profile.clone(), as_shot_neutral: reference.as_shot_neutral })),
    };
    let mut settings = DevelopSettings::default();
    if let Some(c) = crop {
        settings.crop = c.to_crop();
    }
    adobe::prepare(adobe_profiles, &mut photo, &mut settings);
    let pipeline = Pipeline::new(&settings, SourceKind::of(&photo));
    let full_size = data.width.max(data.height);
    let rendered = pipeline.render(&photo, &RenderOptions { max_size: None, full_size: Some(full_size) });
    RgbImage::from_raw(rendered.width as u32, rendered.height as u32, raw::encode_srgb8(&rendered, 0.0)).expect("preview size")
}

/// `<folder>/<stem>-HDR.dng`, or `-HDR-2.dng`, `-HDR-3.dng`… if taken.
pub fn default_output_path(first: &Path) -> PathBuf {
    let dir = first.parent().unwrap_or(Path::new("."));
    let stem = first.file_stem().map_or_else(|| "merge".into(), |s| s.to_string_lossy().into_owned());
    (1..)
        .map(|n| dir.join(if n == 1 { format!("{stem}-HDR.dng") } else { format!("{stem}-HDR-{n}.dng") }))
        .find(|p| !p.exists())
        .expect("a free name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_maps_the_percentile_to_one() {
        let img = Image::from_fn(100, 100, |x, y| [(y * 100 + x) as f32 / 10.0, 0.0, 0.0]);
        let s = output_scale(&img);
        // 99.99th percentile of 0..999.9 ≈ 999.8.
        assert!((1.0 / s - 999.8).abs() < 0.2, "{}", 1.0 / s);
        let out = scaled(img, s);
        assert!(out.pixels.iter().all(|p| p[0] <= 1.0));
    }

    #[test]
    fn output_names_get_a_suffix() {
        let dir = std::env::temp_dir().join(format!("lumen-merge-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("IMG_1.RW2");
        assert_eq!(default_output_path(&first), dir.join("IMG_1-HDR.dng"));
        std::fs::write(dir.join("IMG_1-HDR.dng"), b"x").unwrap();
        assert_eq!(default_output_path(&first), dir.join("IMG_1-HDR-2.dng"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn xmp_carries_the_crop() {
        let x = xmp_packet(Some(CropRect { top: 0.01, left: 0.02, bottom: 0.98, right: 0.97 }));
        let p = xmp::XmpPacket::parse(&x).unwrap();
        assert_eq!(p.props.crs("CropLeft").and_then(|v| v.as_f32()), Some(0.02));
        assert_eq!(p.props.crs("HasCrop").and_then(|v| v.as_bool()), Some(true));
        assert!(xmp::XmpPacket::parse(&xmp_packet(None)).is_ok());
    }
}
