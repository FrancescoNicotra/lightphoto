//! Glue to `rawler` (RAW files) and `image` (JPEG/PNG/TIFF): file → normalised,
//! demosaiced camera RGB plus the colour metadata needed to develop it.

use std::path::Path;

use rawler::decoders::{Decoder, RawDecodeParams, WellKnownIFD};
use rawler::formats::tiff::IFD;
use rawler::rawimage::RawPhotometricInterpretation;
use rawler::rawsource::RawSource;
use rawler::tags::DngTag;
use rawler::RawImage;
use rayon::prelude::*;

use crate::color::{self, Mat3, Vec3, Xy};
use crate::demosaic::{self, Cfa};
use crate::image::{Image, Orientation};
use crate::profile::{illuminant_temperature, normalize_max, Calibration, ColorProfile};
use crate::{CameraData, ColorSource, DecodeOptions, Error, Photo, PhotoInfo, RawInfo};

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
    let src = RawSource::new(path)?;
    let decoder = rawler::get_decoder(&src)?;
    let raw = decoder.raw_image(&src, &RawDecodeParams::default(), false)?;
    let meta = decoder.raw_metadata(&src, &RawDecodeParams::default()).ok();
    let dng = DngTags::read(decoder.as_ref());

    let (profile, neutral) = color_profile(&raw, dng.as_ref())?;
    let (mut image, crop, cfa_name) = sensor_to_camera_rgb(&raw, opts.half_size)?;

    let exif_orientation = meta.as_ref().and_then(|m| m.exif.orientation);
    let orientation = match raw.orientation.to_u16() {
        1 => exif_orientation.map_or(Orientation::Normal, Orientation::from_exif),
        o => Orientation::from_exif(o),
    };
    image = image.oriented(orientation);

    let as_shot_white = profile.neutral_to_xy(neutral);
    let info = PhotoInfo {
        make: raw.clean_make.clone(),
        model: raw.clean_model.clone(),
        lens: meta.as_ref().and_then(|m| m.lens.as_ref()).map(|l| format!("{} {}", l.lens_make, l.lens_model).trim().to_string()),
        iso: meta.as_ref().and_then(|m| m.exif.iso_speed_ratings.map(u32::from).or(m.exif.iso_speed)),
        exposure_time: meta.as_ref().and_then(|m| m.exif.exposure_time).map(|r| r.n as f64 / r.d.max(1) as f64),
        f_number: meta.as_ref().and_then(|m| m.exif.fnumber).map(|r| r.n as f64 / r.d.max(1) as f64),
        focal_length: meta.as_ref().and_then(|m| m.exif.focal_length).map(|r| r.n as f64 / r.d.max(1) as f64),
        date_time: meta.as_ref().and_then(|m| m.exif.date_time_original.clone()),
        orientation,
        baseline_exposure: dng.as_ref().and_then(|d| d.baseline_exposure).unwrap_or(0.0),
        raw: Some(RawInfo {
            is_dng: dng.is_some(),
            sensor_size: (raw.width, raw.height),
            crop,
            cfa: cfa_name,
            black_level: raw.blacklevel.as_vec(),
            white_level: raw.whitelevel.as_vec(),
            as_shot_neutral: neutral,
            as_shot_white,
        }),
    };
    Ok(Photo {
        info,
        data: image,
        color: ColorSource::Camera(Box::new(CameraData { profile, as_shot_neutral: neutral })),
    })
}

/// Black/white normalisation, demosaic and crop. Returns camera RGB where 1.0
/// is the clip point of each channel, the crop used (x, y, w, h in sensor
/// pixels) and the CFA pattern name.
fn sensor_to_camera_rgb(raw: &RawImage, half: bool) -> Result<(Image, [usize; 4], String), Error> {
    let (w, h) = (raw.width, raw.height);
    let area = raw.crop_area.or(raw.active_area);
    let crop = area.map_or([0, 0, w, h], |r| [r.p.x, r.p.y, r.d.w, r.d.h]);
    // The DNG BlackLevel pattern is anchored at the active area origin.
    let origin = raw.active_area.map_or((0, 0), |r| (r.p.x, r.p.y));

    let samples = raw.data.as_f32();
    let black = raw.blacklevel.as_vec();
    let (bw, bh) = (raw.blacklevel.width.max(1), raw.blacklevel.height.max(1));
    let white = raw.whitelevel.as_vec();
    let cpp = raw.cpp;
    let normalise = |x: usize, y: usize, c: usize| -> f32 {
        let bx = (x + bw - origin.0 % bw) % bw;
        let by = (y + bh - origin.1 % bh) % bh;
        let b = black.get((by * bw + bx) * cpp + c).or(black.first()).copied().unwrap_or(0.0);
        let wl = white.get(c).or(white.first()).copied().unwrap_or(65535.0);
        (samples[(y * w + x) * cpp + c] - b) / (wl - b).max(1.0)
    };

    match &raw.photometric {
        RawPhotometricInterpretation::Cfa(cfg) => {
            let cfa = Cfa {
                width: cfg.cfa.width,
                height: cfg.cfa.height,
                colors: (0..cfg.cfa.height)
                    .flat_map(|y| (0..cfg.cfa.width).map(move |x| (x, y)))
                    .map(|(x, y)| cfg.cfa.color_at(y, x) as u8)
                    .collect(),
            };
            if cfa.colors.iter().any(|&c| c > 2) || cfa.width == 0 {
                return Err(Error::Unsupported(format!("colour filter pattern {:?}", cfg.cfa.name)));
            }
            let mosaic: Vec<f32> = (0..w * h).into_par_iter().map(|i| normalise(i % w, i / w, 0)).collect();
            if half {
                // Align the crop to the 2×2 cells so they stay whole.
                let [x, y, cw, ch] = crop;
                if let Some(img) = demosaic::half_size(&mosaic, w, h, &cfa) {
                    let img = img.crop(x.div_ceil(2), y.div_ceil(2), cw / 2, ch / 2);
                    return Ok((img, crop, cfa.name()));
                }
            }
            let img = demosaic::bilinear(&mosaic, w, h, &cfa);
            Ok((img.crop(crop[0], crop[1], crop[2], crop[3]), crop, cfa.name()))
        }
        RawPhotometricInterpretation::LinearRaw | RawPhotometricInterpretation::BlackIsZero => {
            let img = Image::from_fn(w, h, |x, y| {
                if cpp >= 3 {
                    std::array::from_fn(|c| normalise(x, y, c))
                } else {
                    [normalise(x, y, 0); 3]
                }
            });
            let img = img.crop(crop[0], crop[1], crop[2], crop[3]);
            let img = if half { img.downscale(img.width.max(img.height) / 2) } else { img };
            let name = if cpp >= 3 { "linear RGB" } else { "monochrome" };
            Ok((img, crop, name.to_string()))
        }
    }
}

/// DNG colour tags that rawler does not interpret.
struct DngTags {
    illuminants: [Option<u16>; 2],
    color_matrices: [Option<Mat3>; 2],
    forward_matrices: [Option<Mat3>; 2],
    camera_calibrations: [Option<Mat3>; 2],
    analog_balance: Option<Mat3>,
    as_shot_neutral: Option<Vec3>,
    as_shot_white_xy: Option<Xy>,
    baseline_exposure: Option<f64>,
}

impl DngTags {
    fn read(decoder: &dyn Decoder) -> Option<DngTags> {
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
        })
    }
}

fn to_mat3(v: &[f64]) -> Mat3 {
    std::array::from_fn(|i| std::array::from_fn(|j| v[i * 3 + j]))
}

/// The camera profile and the as-shot neutral (camera response to the scene
/// white, max component 1).
fn color_profile(raw: &RawImage, dng: Option<&DngTags>) -> Result<(ColorProfile, Vec3), Error> {
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
