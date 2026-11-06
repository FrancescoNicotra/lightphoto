//! Linear camera RGB → floating point `LinearRaw` DNG, the format Lightroom
//! uses for its HDR merges.
//!
//! rawler's [`DngWriter`] lays out the file (IFD0 with the thumbnail, EXIF and
//! XMP, SubIFDs) but only writes float data as uncompressed 32 bit strips, so
//! the raw tiles are encoded here: 16 bit half floats, 256×256 tiles, the DNG
//! floating point predictor (34894) and Deflate.
//!
//! ```text
//! IFD0     thumbnail (RGB, ≤ 240×120), colour tags, Orientation, XMP, EXIF
//! ├ SubIFD 0  LinearRaw, 3 × half float, tiled, Deflate + predictor 34894
//! └ SubIFD 1  JPEG preview, ≤ 1024 px
//! ```
//!
//! The DNG convention is that every image in the file, previews included, is
//! stored in sensor orientation; `Orientation` applies to all of them.

use std::fs::File;
use std::io::{self, BufWriter, Seek, Write};
use std::path::{Path, PathBuf};

use flate2::write::ZlibEncoder;
use flate2::Compression;
use image::imageops::FilterType;
use image::{DynamicImage, RgbImage};
use rawler::decoders::RawMetadata;
use rawler::dng::writer::DngWriter;
use rawler::dng::DNG_VERSION_V1_4;
use rawler::formats::tiff::{CompressionMethod, PhotometricInterpretation, Rational, SRational, TiffError, TiffWriter, Value};
use rawler::tags::{DngTag, TiffCommonTag};
use rayon::prelude::*;

use crate::capture::Capture;
use crate::color::{self, Mat3, Vec3};
use crate::image::{Image, Orientation};
use crate::profile::{illuminant_code, ColorProfile};
use crate::{CameraData, ColorSource, Error, Photo, PhotoInfo};

/// Side of the square raw tiles.
pub const TILE: usize = 256;
/// Long edge of the JPEG preview.
pub const PREVIEW_SIZE: u32 = 1024;
const THUMBNAIL_SIZE: (u32, u32) = (240, 120);
const PREVIEW_QUALITY: f32 = 0.9;
/// TIFF predictor "floating point, 2× byte stride" (DNG 1.5).
const PREDICTOR_FLOAT_X2: u16 = 34894;
const SAMPLES: usize = 3;

/// What goes into the DNG. Everything is in sensor orientation.
#[derive(Clone, Copy, Debug)]
pub struct LinearDng<'a> {
    /// Camera RGB, 1.0 = white (the clip point of a single exposure). Values
    /// above 1 are kept; negative ones and NaN are written as 0.
    pub image: &'a Image,
    pub orientation: Orientation,
    /// EXIF Make/Model as the camera writes them.
    pub make: &'a str,
    pub model: &'a str,
    /// DNG UniqueCameraModel: Lightroom and `pipeline::adobe` find the
    /// camera's Adobe profiles with it.
    pub unique_camera_model: &'a str,
    /// ColorMatrix, ForwardMatrix, CameraCalibration (1 and 2), illuminants
    /// and AnalogBalance. Only the matrices: HueSatMaps, LookTables and
    /// curves come from the installed Adobe profiles, as for the source RAW.
    pub profile: &'a ColorProfile,
    pub as_shot_neutral: Vec3,
    /// In EV, as the DNG tag.
    pub baseline_exposure: f64,
    /// DNG BaselineNoise, 1 for a single exposure; lower for a merge.
    pub baseline_noise: f64,
    /// EXIF of the reference exposure.
    pub metadata: Option<&'a RawMetadata>,
    /// A complete XMP packet for tag 700 (initial `crs:` settings, CreatorTool).
    pub xmp: Option<&'a [u8]>,
    /// sRGB rendering for the embedded preview, in sensor orientation; resized
    /// to [`PREVIEW_SIZE`] (Lanczos) if larger. Without one, a plain render
    /// (white balance, matrix, BaselineExposure, no tone curve) is used.
    pub preview: Option<&'a RgbImage>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DngEncoding {
    /// 16 bit half floats in Deflate-compressed tiles with predictor 34894.
    #[default]
    HalfDeflate,
    /// 32 bit floats in uncompressed strips: three times larger, for checking
    /// the encoder.
    F32Uncompressed,
}

impl<'a> LinearDng<'a> {
    /// A DNG with the same data and colour as a capture (a "merge" of one
    /// exposure), to be completed with an XMP packet and a preview.
    pub fn from_capture(c: &'a Capture) -> Self {
        LinearDng {
            image: &c.data,
            orientation: c.info.orientation,
            make: &c.camera.make,
            model: &c.camera.model,
            unique_camera_model: &c.camera.unique_model,
            profile: &c.profile,
            as_shot_neutral: c.as_shot_neutral,
            baseline_exposure: c.info.baseline_exposure,
            baseline_noise: 1.0,
            metadata: c.metadata.as_ref(),
            xmp: None,
            preview: None,
        }
    }
}

/// Writes `dng` to `path`, through a temporary file in the same folder so
/// that a failure never leaves a partial DNG behind.
pub fn write_linear_dng(path: &Path, dng: &LinearDng, encoding: DngEncoding) -> Result<(), Error> {
    let img = dng.image;
    if img.width == 0 || img.height == 0 || img.pixels.len() != img.width * img.height {
        return Err(Error::Unsupported(format!("cannot write a {}×{} image", img.width, img.height)));
    }
    let tmp = temp_path(path);
    let result = write_file(&tmp, dng, encoding).and_then(|()| Ok(std::fs::rename(&tmp, path)?));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

fn write_file(path: &Path, dng: &LinearDng, encoding: DngEncoding) -> Result<(), Error> {
    let mut file = BufWriter::new(File::create(path)?);
    let mut w = DngWriter::new(&mut file, DNG_VERSION_V1_4).map_err(tiff_error)?;

    // Data first: the IFDs that point to it are built afterwards.
    let data_tags = match encoding {
        DngEncoding::HalfDeflate => write_half_tiles(&mut w.dng, dng.image)?,
        DngEncoding::F32Uncompressed => write_f32_strips(&mut w.dng, dng.image)?,
    };
    let mut raw = w.subframe(0);
    let ifd = raw.ifd_mut();
    for (tag, value) in data_tags.into_iter().chain(raw_ifd_tags(dng.image)) {
        ifd.add_untyped_tag(tag, value);
    }
    raw.finalize().map_err(tiff_error)?;

    let preview = DynamicImage::ImageRgb8(match dng.preview {
        Some(p) => fit(p, PREVIEW_SIZE, PREVIEW_SIZE),
        None => plain_preview(dng),
    });
    let mut sub = w.subframe(1);
    sub.preview(&preview, PREVIEW_QUALITY).map_err(tiff_error)?;
    sub.finalize().map_err(tiff_error)?;
    // Pre-sized, so rawler's nearest-neighbour resize is a no-op.
    let thumbnail = DynamicImage::ImageRgb8(fit(preview.as_rgb8().expect("RGB8 preview"), THUMBNAIL_SIZE.0, THUMBNAIL_SIZE.1));
    w.thumbnail(&thumbnail).map_err(tiff_error)?;

    if let Some(meta) = dng.metadata {
        w.load_metadata(meta).map_err(tiff_error)?;
    }
    // After the EXIF, which may carry its own Orientation.
    let root = w.root_ifd_mut();
    for (tag, value) in root_ifd_tags(dng) {
        root.add_untyped_tag(tag, value);
    }
    if let Some(xmp) = dng.xmp {
        w.xpacket(xmp).map_err(tiff_error)?;
    }
    w.close().map_err(tiff_error)?;
    file.flush()?;
    Ok(())
}

fn tiff_error(e: TiffError) -> Error {
    Error::Encode(e.to_string())
}

// ---------------------------------------------------------------------------
// Tags

fn tag(t: impl Into<u16>) -> u16 {
    t.into()
}

/// Scaled to 1e-6, well below what the reader's f32 can tell apart.
fn srational(v: f64) -> SRational {
    SRational::new((v * 1e6).round() as i32, 1_000_000)
}

fn rational(v: f64) -> Rational {
    Rational::new((v.max(0.0) * 1e6).round() as u32, 1_000_000)
}

fn matrix(m: &Mat3) -> Value {
    Value::SRational(m.iter().flatten().map(|&v| srational(v)).collect())
}

/// IFD0: camera, colour model, orientation.
fn root_ifd_tags(dng: &LinearDng) -> Vec<(u16, Value)> {
    let p = dng.profile;
    let mut tags = vec![
        (tag(TiffCommonTag::Make), Value::from(dng.make)),
        (tag(TiffCommonTag::Model), Value::from(dng.model)),
        (tag(DngTag::UniqueCameraModel), Value::from(dng.unique_camera_model)),
        (tag(TiffCommonTag::Software), Value::from(concat!("Lumen ", env!("CARGO_PKG_VERSION")))),
        (tag(TiffCommonTag::Orientation), Value::Short(vec![dng.orientation as u16])),
        (tag(DngTag::AsShotNeutral), Value::Rational(dng.as_shot_neutral.iter().map(|&v| rational(v)).collect())),
        (tag(DngTag::BaselineExposure), Value::SRational(vec![srational(dng.baseline_exposure)])),
        (tag(DngTag::BaselineNoise), Value::Rational(vec![rational(dng.baseline_noise)])),
        (tag(DngTag::BaselineSharpness), Value::Rational(vec![Rational::new(1, 1)])),
        (tag(DngTag::LinearResponseLimit), Value::Rational(vec![Rational::new(1, 1)])),
    ];
    // `ColorProfile` keeps its calibrations sorted by temperature, which is
    // also the DNG convention for slots 1 and 2.
    let slots = [
        (DngTag::CalibrationIlluminant1, DngTag::ColorMatrix1, DngTag::ForwardMatrix1, DngTag::CameraCalibration1),
        (DngTag::CalibrationIlluminant2, DngTag::ColorMatrix2, DngTag::ForwardMatrix2, DngTag::CameraCalibration2),
    ];
    let forward = p.has_forward_matrix();
    let calibrated = p.calibrations.iter().any(|c| c.camera_calibration != color::IDENTITY);
    for (cal, (illuminant, cm, fm, cc)) in p.calibrations.iter().zip(slots) {
        tags.push((tag(illuminant), Value::Short(vec![illuminant_code(cal.temperature)])));
        tags.push((tag(cm), matrix(&cal.color_matrix)));
        if let (true, Some(m)) = (forward, &cal.forward_matrix) {
            tags.push((tag(fm), matrix(m)));
        }
        // Copied so the DNG renders as its source did. Without a
        // CameraCalibrationSignature, Adobe applies them only with profiles
        // that have no ProfileCalibrationSignature either (the embedded one).
        if calibrated {
            tags.push((tag(cc), matrix(&cal.camera_calibration)));
        }
    }
    if p.analog_balance != color::IDENTITY {
        let ab = &p.analog_balance;
        tags.push((tag(DngTag::AnalogBalance), Value::Rational((0..3).map(|i| rational(ab[i][i])).collect())));
    }
    tags
}

/// SubIFD 0 apart from the data layout: a LinearRaw image whose white is 1.0,
/// black 0, with the whole image as active area and default crop.
fn raw_ifd_tags(img: &Image) -> Vec<(u16, Value)> {
    let (w, h) = (img.width as u32, img.height as u32);
    vec![
        (tag(TiffCommonTag::ImageWidth), Value::Long(vec![w])),
        (tag(TiffCommonTag::ImageLength), Value::Long(vec![h])),
        (tag(TiffCommonTag::PhotometricInt), Value::from(PhotometricInterpretation::LinearRaw)),
        (tag(TiffCommonTag::SamplesPerPixel), Value::Short(vec![SAMPLES as u16])),
        (tag(TiffCommonTag::SampleFormat), Value::Short(vec![3; SAMPLES])), // IEEE float
        (284, Value::Short(vec![1])), // PlanarConfiguration: chunky
        (tag(DngTag::BlackLevelRepeatDim), Value::Short(vec![1, 1])),
        (tag(DngTag::BlackLevel), Value::Short(vec![0; SAMPLES])),
        (tag(DngTag::WhiteLevel), Value::Short(vec![1; SAMPLES])),
        (tag(DngTag::ActiveArea), Value::Long(vec![0, 0, h, w])),
        (tag(DngTag::DefaultCropOrigin), Value::Long(vec![0, 0])),
        (tag(DngTag::DefaultCropSize), Value::Long(vec![w, h])),
        (tag(DngTag::DefaultScale), Value::Rational(vec![Rational::new(1, 1), Rational::new(1, 1)])),
        (tag(DngTag::BestQualityScale), Value::Rational(vec![Rational::new(1, 1)])),
    ]
}

// ---------------------------------------------------------------------------
// Raw data

/// Non-negative and finite: DNG raw values cannot be negative.
fn sanitize(v: f32) -> f32 {
    if v > 0.0 {
        v.min(f32::MAX)
    } else {
        0.0
    }
}

fn write_half_tiles<W: Write + Seek>(tiff: &mut TiffWriter<W>, img: &Image) -> Result<Vec<(u16, Value)>, Error> {
    let across = img.width.div_ceil(TILE);
    let down = img.height.div_ceil(TILE);
    let tiles: Vec<Vec<u8>> = (0..across * down)
        .into_par_iter()
        .map(|i| encode_tile(img, (i % across) * TILE, (i / across) * TILE))
        .collect::<io::Result<_>>()?;
    let mut offsets = Vec::with_capacity(tiles.len());
    let mut counts = Vec::with_capacity(tiles.len());
    for t in &tiles {
        offsets.push(tiff.write_data(t).map_err(tiff_error)?);
        counts.push(t.len() as u32);
    }
    Ok(vec![
        (tag(TiffCommonTag::Compression), Value::from(CompressionMethod::Deflate)),
        (tag(TiffCommonTag::Predictor), Value::Short(vec![PREDICTOR_FLOAT_X2])),
        (tag(TiffCommonTag::BitsPerSample), Value::Short(vec![16; SAMPLES])),
        (tag(TiffCommonTag::TileWidth), Value::Long(vec![TILE as u32])),
        (tag(TiffCommonTag::TileLength), Value::Long(vec![TILE as u32])),
        (tag(TiffCommonTag::TileOffsets), Value::Long(offsets)),
        (tag(TiffCommonTag::TileByteCounts), Value::Long(counts)),
    ])
}

/// One `TILE`² tile at (`x0`, `y0`), padded by repeating the last row and
/// column. Each row of samples (pixel-interleaved RGB) is converted to half
/// floats, split into a plane of high bytes followed by a plane of low bytes,
/// differenced byte-wise with a stride of 2 × samples per pixel (predictor
/// 34894), and the whole tile is zlib-compressed.
fn encode_tile(img: &Image, x0: usize, y0: usize) -> io::Result<Vec<u8>> {
    const N: usize = TILE * SAMPLES;
    const STRIDE: usize = 2 * SAMPLES;
    // Level 3: on a 16 MP RAW, 2% larger than level 6 and five times faster.
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::new(3));
    let mut row = [0u8; 2 * N];
    for ty in 0..TILE {
        let y = (y0 + ty).min(img.height - 1);
        let line = &img.pixels[y * img.width..(y + 1) * img.width];
        for tx in 0..TILE {
            let p = line[(x0 + tx).min(img.width - 1)];
            for (c, v) in p.into_iter().enumerate() {
                let bits = f32_to_f16(sanitize(v));
                let i = tx * SAMPLES + c;
                row[i] = (bits >> 8) as u8;
                row[N + i] = bits as u8;
            }
        }
        for i in (STRIDE..2 * N).rev() {
            row[i] = row[i].wrapping_sub(row[i - STRIDE]);
        }
        enc.write_all(&row)?;
    }
    enc.finish()
}

fn write_f32_strips<W: Write + Seek>(tiff: &mut TiffWriter<W>, img: &Image) -> Result<Vec<(u16, Value)>, Error> {
    let rows = (1 << 20) / (img.width * SAMPLES * 4) + 1;
    let mut offsets = Vec::new();
    let mut counts = Vec::new();
    for strip in img.pixels.chunks(rows * img.width) {
        // The TIFF header rawler writes is in native byte order.
        let bytes: Vec<u8> = strip.iter().flatten().flat_map(|&v| sanitize(v).to_ne_bytes()).collect();
        offsets.push(tiff.write_data(&bytes).map_err(tiff_error)?);
        counts.push(bytes.len() as u32);
    }
    Ok(vec![
        (tag(TiffCommonTag::Compression), Value::from(CompressionMethod::None)),
        (tag(TiffCommonTag::BitsPerSample), Value::Short(vec![32; SAMPLES])),
        (tag(TiffCommonTag::RowsPerStrip), Value::Long(vec![rows as u32])),
        (tag(TiffCommonTag::StripOffsets), Value::Long(offsets)),
        (tag(TiffCommonTag::StripByteCounts), Value::Long(counts)),
    ])
}

/// IEEE 754 binary32 → binary16, rounding to nearest, ties to even.
/// Overflow gives infinity, NaN stays NaN.
pub fn f32_to_f16(v: f32) -> u16 {
    let x = v.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let exp = ((x >> 23) & 0xff) as i32;
    let mant = x & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if mant != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    let round = |value: u32, shift: u32| -> u32 {
        let kept = value >> shift;
        let rest = value & ((1 << shift) - 1);
        let half = 1 << (shift - 1);
        kept + u32::from(rest > half || (rest == half && kept & 1 == 1))
    };
    if e <= 0 {
        // Subnormal half: m × 2^-24. A carry into the exponent is still the
        // right encoding.
        if e < -10 {
            return sign;
        }
        return sign | round(mant | 0x80_0000, (14 - e) as u32) as u16;
    }
    // A carry may overflow into the exponent, up to infinity: still correct.
    sign | round(((e as u32) << 23) | mant, 13) as u16
}

// ---------------------------------------------------------------------------
// Previews

/// `img` resized (Lanczos) to fit `w`×`h`, or unchanged if it already fits.
fn fit(img: &RgbImage, w: u32, h: u32) -> RgbImage {
    if img.width() <= w && img.height() <= h {
        return img.clone();
    }
    DynamicImage::ImageRgb8(img.clone()).resize(w, h, FilterType::Lanczos3).to_rgb8()
}

/// The image with its white balance, colour matrix and BaselineExposure,
/// clipped and sRGB-encoded: no tone curve, but recognisable.
fn plain_preview(dng: &LinearDng) -> RgbImage {
    let photo = Photo {
        info: PhotoInfo::default(),
        data: dng.image.downscale(PREVIEW_SIZE as usize),
        color: ColorSource::Camera(Box::new(CameraData { profile: dng.profile.clone(), as_shot_neutral: dng.as_shot_neutral })),
    };
    let white = dng.profile.neutral_to_xy(dng.as_shot_neutral);
    let img = photo.to_working(Some(white));
    let srgb = crate::encode_srgb8(&img, dng.baseline_exposure);
    RgbImage::from_raw(img.width as u32, img.height as u32, srgb).expect("preview size")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats() {
        let cases: [(f32, u16); 12] = [
            (0.0, 0x0000),
            (-0.0, 0x8000),
            (1.0, 0x3c00),
            (0.5, 0x3800),
            (-2.0, 0xc000),
            (65504.0, 0x7bff),
            (65520.0, 0x7c00),   // rounds up to infinity
            (6.103_515_6e-5, 0x0400), // smallest normal
            (5.960_464_5e-8, 0x0001), // smallest subnormal
            (2.980_232_2e-8, 0x0000), // half of it: ties to even
            (1.0 + 1.0 / 2048.0, 0x3c00), // tie, rounds to even
            (1.0 + 3.0 / 2048.0, 0x3c02), // tie, rounds to even (up)
        ];
        for (v, bits) in cases {
            assert_eq!(f32_to_f16(v), bits, "{v}");
        }
        assert_eq!(f32_to_f16(f32::INFINITY), 0x7c00);
        assert_eq!(f32_to_f16(f32::NAN) & 0x7e00, 0x7e00);
    }

    #[test]
    fn half_float_relative_error() {
        // Exhaustive over normal halves' range, sampled.
        let mut v = 6.2e-5f32;
        while v < 60000.0 {
            let h = f32_to_f16(v);
            let back = f16_to_f32(h);
            assert!(((back - v) / v).abs() <= 1.0 / 2048.0 + 1e-7, "{v} -> {back}");
            v *= 1.0137;
        }
    }

    fn f16_to_f32(h: u16) -> f32 {
        let exp = (h >> 10) & 0x1f;
        let mant = (h & 0x3ff) as f32;
        let mag = if exp == 0 { mant * 2f32.powi(-24) } else { (1.0 + mant / 1024.0) * 2f32.powi(exp as i32 - 15) };
        if h & 0x8000 != 0 {
            -mag
        } else {
            mag
        }
    }
}
