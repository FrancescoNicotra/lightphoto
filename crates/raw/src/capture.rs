//! A RAW file as linear camera RGB, before orientation: what HDR merging
//! (and later focus stacking / panoramas) works on, and what
//! [`Photo::load`](crate::Photo::load) builds on.
//!
//! ```text
//! file ─rawler→ mosaic ─black/white→ 0..1 ─demosaic→ camera RGB ─default crop→ Capture
//!                  └─ photosites ≥ CLIP_MASK in the demosaic support ─→ clipped
//! ```

use std::path::Path;

use rawler::decoders::{RawDecodeParams, RawMetadata};
use rawler::rawimage::RawPhotometricInterpretation;
use rawler::rawsource::RawSource;
use rawler::{RawImage, RawImageData};
use rayon::prelude::*;

use crate::color::Vec3;
use crate::decode::{color_profile, DngTags};
use crate::demosaic::{self, Cfa};
use crate::rcd;
use crate::image::{Image, Orientation};
use crate::profile::ColorProfile;
use crate::{CameraData, ColorSource, DecodeOptions, Error, Photo, PhotoInfo, RawInfo};

/// Normalised photosite value from which a sample counts as clipped in
/// [`Capture::clipped`]. Lower than the 0.995 of [`Photo::to_working`]:
/// merging should stop trusting a frame a little before the hard clip.
pub const CLIP_MASK: f32 = 0.98;

/// A decoded RAW file in sensor orientation.
#[derive(Clone, Debug)]
pub struct Capture {
    /// `orientation` is the file's, *not* applied to `data`.
    pub info: PhotoInfo,
    /// Camera RGB, 0..1 with 1 = clip point of each channel, cropped to the
    /// default crop, not white balanced, not oriented.
    pub data: Image,
    /// One byte per pixel of `data`: 1 if a photosite at or above
    /// [`CLIP_MASK`] contributes to the pixel through the demosaic (its 3×3
    /// neighbourhood for Bayer and X-Trans, see [`demosaic::support_radius`];
    /// the 2×2 cell at half size), 0 otherwise. For LinearRaw files: any channel ≥ `CLIP_MASK`.
    /// Empty when the capture was decoded only to build a [`Photo`].
    pub clipped: Vec<u8>,
    pub profile: ColorProfile,
    /// Camera response to the as-shot white, max component 1.
    pub as_shot_neutral: Vec3,
    /// Camera names as written in the file (EXIF Make/Model and the DNG
    /// UniqueCameraModel), to copy into a DNG derived from this capture.
    pub camera: CameraNames,
    /// rawler's EXIF and lens data, for the same purpose.
    pub metadata: Option<RawMetadata>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CameraNames {
    pub make: String,
    pub model: String,
    /// DNG UniqueCameraModel; "make model" (rawler's clean names) for
    /// non-DNG files.
    pub unique_model: String,
}

/// Decodes a RAW file into a [`Capture`] with its clipping mask.
pub fn load_capture(path: &Path, opts: &DecodeOptions) -> Result<Capture, Error> {
    capture(path, opts, true)
}

impl Capture {
    /// The orientation applied: the [`Photo`] the app develops.
    pub fn into_photo(self) -> Photo {
        let orientation = self.info.orientation;
        Photo {
            info: self.info,
            data: self.data.oriented(orientation),
            color: ColorSource::Camera(Box::new(CameraData { profile: self.profile, as_shot_neutral: self.as_shot_neutral })),
        }
    }
}

pub(crate) fn capture(path: &Path, opts: &DecodeOptions, with_mask: bool) -> Result<Capture, Error> {
    if crate::decode::is_rendered_image(path) {
        return Err(Error::Unsupported(format!("{} is not a RAW file", path.display())));
    }
    let src = RawSource::new(path)?;
    let decoder = rawler::get_decoder(&src)?;
    let raw = decoder.raw_image(&src, &RawDecodeParams::default(), false)?;
    let meta = decoder.raw_metadata(&src, &RawDecodeParams::default()).ok();
    let dng = DngTags::read(decoder.as_ref());

    let (profile, neutral) = color_profile(&raw, dng.as_ref())?;
    // As-shot white balance gains (max neutral 1 → gains ≥ 1), for RCD.
    let gains = neutral.map(|n| (1.0 / n.max(1e-6)) as f32);
    let sensor = sensor_to_camera_rgb(&raw, opts.half_size, with_mask, gains)?;

    let exif_orientation = meta.as_ref().and_then(|m| m.exif.orientation);
    let orientation = match raw.orientation.to_u16() {
        1 => exif_orientation.map_or(Orientation::Normal, Orientation::from_exif),
        o => Orientation::from_exif(o),
    };

    let as_shot_white = profile.neutral_to_xy(neutral);
    let rational = |r: rawler::formats::tiff::Rational| r.n as f64 / r.d.max(1) as f64;
    let info = PhotoInfo {
        make: raw.clean_make.clone(),
        model: raw.clean_model.clone(),
        lens: meta.as_ref().and_then(|m| m.lens.as_ref()).map(|l| format!("{} {}", l.lens_make, l.lens_model).trim().to_string()),
        iso: meta.as_ref().and_then(|m| m.exif.iso_speed_ratings.map(u32::from).or(m.exif.iso_speed)),
        exposure_time: meta.as_ref().and_then(|m| m.exif.exposure_time).map(rational),
        f_number: meta.as_ref().and_then(|m| m.exif.fnumber).map(rational),
        focal_length: meta.as_ref().and_then(|m| m.exif.focal_length).map(rational),
        date_time: meta.as_ref().and_then(|m| m.exif.date_time_original.clone()),
        orientation,
        baseline_exposure: dng.as_ref().and_then(|d| d.baseline_exposure).unwrap_or(0.0),
        raw: Some(RawInfo {
            is_dng: dng.is_some(),
            float: sensor.float,
            sensor_size: (raw.width, raw.height),
            crop: sensor.crop,
            cfa: sensor.cfa,
            black_level: raw.blacklevel.as_vec(),
            white_level: sensor.white_level,
            as_shot_neutral: neutral,
            as_shot_white,
        }),
    };
    let camera = CameraNames {
        make: raw.make.clone(),
        model: raw.model.clone(),
        unique_model: dng
            .as_ref()
            .and_then(|d| d.unique_camera_model.clone())
            .unwrap_or_else(|| format!("{} {}", raw.clean_make, raw.clean_model)),
    };
    Ok(Capture {
        info,
        data: sensor.image,
        clipped: sensor.clipped,
        profile,
        as_shot_neutral: neutral,
        camera,
        metadata: meta,
    })
}

struct Sensor {
    image: Image,
    clipped: Vec<u8>,
    /// x, y, w, h of the default crop in sensor pixels.
    crop: [usize; 4],
    cfa: String,
    /// The white levels actually used (1.0 for floating point data).
    white_level: Vec<f32>,
    float: bool,
}

/// Black/white normalisation, demosaic and crop, plus the clipping mask if
/// asked for. Returns camera RGB where 1.0 is the clip point of each channel.
fn sensor_to_camera_rgb(raw: &RawImage, half: bool, with_mask: bool, gains: [f32; 3]) -> Result<Sensor, Error> {
    let (w, h) = (raw.width, raw.height);
    let area = raw.crop_area.or(raw.active_area);
    let crop = area.map_or([0, 0, w, h], |r| [r.p.x, r.p.y, r.d.w, r.d.h]);
    // The DNG BlackLevel pattern is anchored at the active area origin.
    let origin = raw.active_area.map_or((0, 0), |r| (r.p.x, r.p.y));

    // Read the decoded samples in place: converting them all to f32 first
    // would cost another copy of the sensor.
    let sample = |i: usize| -> f32 {
        match &raw.data {
            RawImageData::Integer(v) => v[i] as f32,
            RawImageData::Float(v) => v[i],
        }
    };
    let black = raw.blacklevel.as_vec();
    let (bw, bh) = (raw.blacklevel.width.max(1), raw.blacklevel.height.max(1));
    let cpp = raw.cpp;
    // Floating point data (HDR merges) is scaled so that 1.0 is white,
    // whatever WhiteLevel says; without the tag rawler assumes 2^bps − 1.
    let float = matches!(raw.data, RawImageData::Float(_));
    let white = if float { vec![1.0; cpp.max(1)] } else { raw.whitelevel.as_vec() };
    let min_range = if float { f32::EPSILON } else { 1.0 };
    let normalise = |x: usize, y: usize, c: usize| -> f32 {
        let bx = (x + bw - origin.0 % bw) % bw;
        let by = (y + bh - origin.1 % bh) % bh;
        let b = black.get((by * bw + bx) * cpp + c).or(black.first()).copied().unwrap_or(0.0);
        let wl = white.get(c).or(white.first()).copied().unwrap_or(65535.0);
        (sample((y * w + x) * cpp + c) - b) / (wl - b).max(min_range)
    };
    let sensor = |image: Image, clipped: Vec<u8>, cfa: String| Sensor { image, clipped, crop, cfa, white_level: white.clone(), float };

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
            let [cx, cy, cw, ch] = crop;
            if half {
                // Align the crop to the 2×2 cells so they stay whole.
                if let Some(img) = demosaic::half_size(&mosaic, w, h, &cfa) {
                    let img = img.crop(cx.div_ceil(2), cy.div_ceil(2), cw / 2, ch / 2);
                    let clipped = if with_mask {
                        dilated_mask(&mosaic, w, h, Grid { origin: [2 * cx.div_ceil(2), 2 * cy.div_ceil(2)], size: (img.width, img.height), step: 2, before: 0, after: 1 })
                    } else {
                        Vec::new()
                    };
                    return Ok(sensor(img, clipped, cfa.name()));
                }
            }
            // RCD for Bayer sensors, bilinear for anything else (X-Trans).
            let (img, r) = match rcd::rcd_region(&mosaic, w, h, &cfa, [cx, cy, cw, ch], gains) {
                Some(img) => (img, rcd::SUPPORT_RADIUS),
                None => (demosaic::bilinear_region(&mosaic, w, h, &cfa, [cx, cy, cw, ch]), demosaic::support_radius(&cfa)),
            };
            let clipped = if with_mask {
                dilated_mask(&mosaic, w, h, Grid { origin: [cx, cy], size: (img.width, img.height), step: 1, before: r, after: r })
            } else {
                Vec::new()
            };
            Ok(sensor(img, clipped, cfa.name()))
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
            let clipped = if with_mask {
                img.pixels.par_iter().map(|p| u8::from(p.iter().any(|&c| c >= CLIP_MASK))).collect()
            } else {
                Vec::new()
            };
            let name = if cpp >= 3 { "linear RGB" } else { "monochrome" };
            Ok(sensor(img, clipped, name.to_string()))
        }
    }
}

/// An output grid of `size` pixels laid on the mosaic with `step`
/// photosites per pixel, starting at `origin`: output pixel (x, y) reads
/// photosites `origin + step·(x, y) − before ..= + after` (clamped to the
/// sensor).
struct Grid {
    origin: [usize; 2],
    size: (usize, usize),
    step: usize,
    before: usize,
    after: usize,
}

/// The clipping mask of `grid`: 1 where any photosite it reads is ≥ [`CLIP_MASK`].
fn dilated_mask(mosaic: &[f32], w: usize, h: usize, grid: Grid) -> Vec<u8> {
    let Grid { origin, size: (ow, oh), step, before, after } = grid;
    let span = |o: usize, i: usize, len: usize| (o + step * i).saturating_sub(before)..(o + step * i + after + 1).min(len);
    // Rows of the mosaic that the output needs, reduced horizontally first.
    let rows = span(origin[1], 0, h).start..span(origin[1], oh.saturating_sub(1), h).end;
    let first = rows.start;
    let horizontal: Vec<Vec<bool>> = rows
        .into_par_iter()
        .map(|y| {
            let line = &mosaic[y * w..(y + 1) * w];
            (0..ow).map(|x| line[span(origin[0], x, w)].iter().any(|&v| v >= CLIP_MASK)).collect()
        })
        .collect();
    let mut out = vec![0u8; ow * oh];
    out.par_chunks_mut(ow.max(1)).enumerate().for_each(|(y, row)| {
        let ys = span(origin[1], y, h);
        for (x, o) in row.iter_mut().enumerate() {
            *o = u8::from(ys.clone().any(|sy| horizontal[sy - first][x]));
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_dilates_by_the_support() {
        // 8×6 mosaic with one clipped photosite at (4, 3).
        let (w, h) = (8, 6);
        let mut m = vec![0.5f32; w * h];
        m[3 * w + 4] = 0.99;
        let mask = dilated_mask(&m, w, h, Grid { origin: [1, 1], size: (6, 4), step: 1, before: 1, after: 1 });
        let set: Vec<(usize, usize)> = (0..24).filter(|i| mask[*i] == 1).map(|i| (i % 6, i / 6)).collect();
        // Sensor (3..=5, 2..=4) minus the (1, 1) origin.
        let expected: Vec<(usize, usize)> = (1..=3).flat_map(|y| (2..=4).map(move |x| (x, y))).collect();
        assert_eq!(set, expected);

        // Half size: the 2×2 cell containing (4, 3) is output (1, 0) from origin (2, 2).
        let mask = dilated_mask(&m, w, h, Grid { origin: [2, 2], size: (3, 2), step: 2, before: 0, after: 1 });
        assert_eq!(mask, vec![0, 1, 0, 0, 0, 0]);
    }

    #[test]
    fn mask_threshold_and_borders() {
        let (w, h) = (4, 4);
        let mut m = vec![0.0f32; w * h];
        m[0] = CLIP_MASK; // corner, exactly at the threshold
        m[15] = CLIP_MASK - 1e-4;
        let mask = dilated_mask(&m, w, h, Grid { origin: [0, 0], size: (4, 4), step: 1, before: 2, after: 2 });
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(mask[y * 4 + x], u8::from(x <= 2 && y <= 2), "({x}, {y})");
            }
        }
    }
}
