//! Glue to `rawler` (RAW files) and `image` (JPEG/PNG/TIFF): file → normalised,
//! demosaiced camera RGB plus the colour metadata needed to develop it.

use std::path::Path;

use rawler::decoders::{Decoder, RawDecodeParams, WellKnownIFD};
use rawler::formats::tiff::IFD;
use rawler::rawsource::RawSource;
use rawler::tags::DngTag;
use rawler::RawImage;

use crate::color::{self, Mat3, Vec3, Xy};
use crate::image::{Image, Orientation};
use crate::profile::{illuminant_temperature, normalize_max, Calibration, ColorProfile};
use crate::{ColorSource, DecodeOptions, Error, Photo, PhotoInfo};

/// Extensions handled by the `image` crate; everything else goes to rawler.
const RENDERED_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff", "webp"];

pub fn is_rendered_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| RENDERED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

pub fn load(path: &Path, opts: &DecodeOptions) -> Result<Photo, Error> {
    if is_rendered_image(path) {
        load_rendered(path)
    } else {
        load_raw(path, opts)
    }
}

// ---------------------------------------------------------------------------
// RAW

fn load_raw(path: &Path, opts: &DecodeOptions) -> Result<Photo, Error> {
    crate::capture::capture(path, opts, false).map(crate::capture::Capture::into_photo)
}

/// DNG colour tags that rawler does not interpret.
pub(crate) struct DngTags {
    illuminants: [Option<u16>; 2],
    color_matrices: [Option<Mat3>; 2],
    forward_matrices: [Option<Mat3>; 2],
    camera_calibrations: [Option<Mat3>; 2],
    analog_balance: Option<Mat3>,
    as_shot_neutral: Option<Vec3>,
    as_shot_white_xy: Option<Xy>,
    pub(crate) baseline_exposure: Option<f64>,
    pub(crate) unique_camera_model: Option<String>,
}

impl DngTags {
    pub(crate) fn read(decoder: &dyn Decoder) -> Option<DngTags> {
        let root = decoder.ifd(WellKnownIFD::Root).ok()??;
        root.get_entry(DngTag::DNGVersion)?;
        let raw = decoder.ifd(WellKnownIFD::Raw).ok().flatten();
        // Colour tags belong in IFD0, but some writers put them in the raw IFD.
        let ifds: Vec<&IFD> = std::iter::once(root.as_ref()).chain(raw.as_deref()).collect();
        let floats = |tag: DngTag| -> Option<Vec<f64>> {
            let e = ifds.iter().find_map(|ifd| ifd.get_entry(tag))?;
            Some((0..e.count() as usize).map(|i| e.force_f32(i) as f64).collect())
        };
        let mat3 = |tag| floats(tag).filter(|v| v.len() == 9).map(|v| to_mat3(&v));
        let vec3 = |tag| floats(tag).filter(|v| v.len() == 3).map(|v| [v[0], v[1], v[2]]);
        let u16_tag = |tag| ifds.iter().find_map(|ifd| ifd.get_entry(tag)).map(|e| e.force_u16(0));

        Some(DngTags {
            illuminants: [u16_tag(DngTag::CalibrationIlluminant1), u16_tag(DngTag::CalibrationIlluminant2)],
            color_matrices: [mat3(DngTag::ColorMatrix1), mat3(DngTag::ColorMatrix2)],
            forward_matrices: [mat3(DngTag::ForwardMatrix1), mat3(DngTag::ForwardMatrix2)],
            camera_calibrations: [mat3(DngTag::CameraCalibration1), mat3(DngTag::CameraCalibration2)],
            analog_balance: vec3(DngTag::AnalogBalance).map(color::diag),
            as_shot_neutral: vec3(DngTag::AsShotNeutral),
            as_shot_white_xy: floats(DngTag::AsShotWhiteXY).filter(|v| v.len() == 2).map(|v| Xy::new(v[0], v[1])),
            baseline_exposure: floats(DngTag::BaselineExposure).and_then(|v| v.first().copied()),
            unique_camera_model: root.get_entry(DngTag::UniqueCameraModel).and_then(|e| e.as_string()).map(|s| s.trim_end_matches('\0').to_string()),
        })
    }
}

fn to_mat3(v: &[f64]) -> Mat3 {
    std::array::from_fn(|i| std::array::from_fn(|j| v[i * 3 + j]))
}

/// The camera profile and the as-shot neutral (camera response to the scene
/// white, max component 1).
pub(crate) fn color_profile(raw: &RawImage, dng: Option<&DngTags>) -> Result<(ColorProfile, Vec3), Error> {
    let profile = match dng {
        Some(d) => {
            let mut cals = Vec::new();
            for i in 0..2 {
                let Some(cm) = d.color_matrices[i] else { continue };
                cals.push(Calibration {
                    temperature: d.illuminants[i].map_or(6500.0, illuminant_temperature),
                    color_matrix: cm,
                    forward_matrix: d.forward_matrices[i],
                    camera_calibration: d.camera_calibrations[i].unwrap_or(color::IDENTITY),
                    hue_sat_map: None,
                });
            }
            // A forward matrix is only usable if every calibration has one.
            if cals.iter().any(|c| c.forward_matrix.is_none()) {
                cals.iter_mut().for_each(|c| c.forward_matrix = None);
            }
            (!cals.is_empty()).then(|| ColorProfile::new(cals, d.analog_balance))
        }
        None => {
            let cals: Vec<_> = raw
                .color_matrix
                .iter()
                .filter(|(_, m)| m.len() >= 9)
                .map(|(illu, m)| Calibration {
                    temperature: illuminant_temperature(*illu as u16),
                    color_matrix: to_mat3(&m[..9].iter().map(|&x| x as f64).collect::<Vec<_>>()),
                    forward_matrix: None,
                    camera_calibration: color::IDENTITY,
                    hue_sat_map: None,
                })
                .collect();
            // Keep at most two, preferring the standard pair StdA + D65.
            let mut cals = cals;
            cals.sort_by_key(|c| (c.temperature as i64 - 2850).abs().min((c.temperature as i64 - 6500).abs()));
            cals.truncate(2);
            (!cals.is_empty()).then(|| ColorProfile::new(cals, None))
        }
    }
    .ok_or_else(|| Error::Unsupported(format!("no colour matrix for {} {}", raw.clean_make, raw.clean_model)))?;

    let wb = raw.wb_coeffs;
    let neutral = if let Some(n) = dng.and_then(|d| d.as_shot_neutral) {
        n
    } else if let Some(xy) = dng.and_then(|d| d.as_shot_white_xy) {
        profile.xy_to_neutral(xy)
    } else if wb[..3].iter().all(|c| c.is_finite() && *c > 0.0) {
        [1.0 / wb[0] as f64, 1.0 / wb[1] as f64, 1.0 / wb[2] as f64]
    } else {
        profile.xy_to_neutral(color::D65)
    };
    Ok((profile, normalize_max(neutral)))
}

/// The JPEG preview Lightroom / the camera stored in the file, largest first.
pub fn embedded_preview(path: &Path) -> Result<Option<image::DynamicImage>, Error> {
    let src = RawSource::new(path)?;
    let decoder = rawler::get_decoder(&src)?;
    let params = RawDecodeParams::default();
    let candidates = [
        decoder.full_image(&src, &params).ok().flatten(),
        decoder.preview_image(&src, &params).ok().flatten(),
        decoder.thumbnail_image(&src, &params).ok().flatten(),
    ];
    Ok(candidates.into_iter().flatten().max_by_key(|i| i.width() as u64 * i.height() as u64))
}

// ---------------------------------------------------------------------------
// JPEG / PNG / TIFF

fn load_rendered(path: &Path) -> Result<Photo, Error> {
    use image::ImageDecoder;

    let mut decoder = image::ImageReader::open(path)?.with_guessed_format()?.into_decoder()?;
    let exif_orientation = decoder.orientation()?;
    let orientation = Orientation::from_exif(exif_orientation.to_exif() as u16);
    let decoded = image::DynamicImage::from_decoder(decoder)?.into_rgb32f();
    let (w, h) = (decoded.width() as usize, decoded.height() as usize);

    // Assumed sRGB (embedded ICC profiles are not read yet).
    let to_working = color::to_f32(&color::srgb_to_prophoto());
    let src = decoded.as_raw();
    let image = Image::from_fn(w, h, |x, y| {
        let i = (y * w + x) * 3;
        let lin = [src[i], src[i + 1], src[i + 2]].map(color::srgb_eotf);
        color::apply(&to_working, lin)
    })
    .oriented(orientation);

    Ok(Photo {
        info: PhotoInfo { orientation, ..PhotoInfo::default() },
        data: image,
        color: ColorSource::Working,
    })
}

impl From<image::ImageError> for Error {
    fn from(e: image::ImageError) -> Self {
        Error::Decode(e.to_string())
    }
}

impl From<rawler::RawlerError> for Error {
    fn from(e: rawler::RawlerError) -> Self {
        Error::Decode(e.to_string())
    }
}
