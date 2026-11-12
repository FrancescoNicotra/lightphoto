//! RAW decoding: from a camera file to linear ProPhoto RGB (the working space).
//!
//! `rawler` reads the sensor data; this crate normalises it, demosaics it and
//! applies the DNG colour model with a chosen white balance:
//!
//! ```text
//! file ─rawler→ mosaic ─black/white→ 0..1 ─demosaic→ camera RGB ─crop→ Capture ─orient→ Photo
//! Photo::to_working(white) ─WB + clip→ ─camera→XYZ D50 (DNG maths)→ ProPhoto linear
//! ```
//!
//! [`dng_write`] goes the other way: linear camera RGB (an HDR merge) to a
//! floating point LinearRaw DNG.

pub mod capture;
pub mod color;
pub mod dcp;
mod decode;
pub mod demosaic;
pub mod dng_write;
pub mod hue_sat;
pub mod image;
pub mod profile;
pub mod rcd;

use std::path::Path;

pub use crate::image::{Image, Orientation};
pub use capture::{load_capture, Capture};
pub use dng_write::{write_linear_dng, DngEncoding, LinearDng};
pub use color::{TempTint, Xy};
pub use decode::{embedded_preview, is_rendered_image};
pub use profile::ColorProfile;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Decode(String),
    Unsupported(String),
    Encode(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Decode(e) => write!(f, "decoding failed: {e}"),
            Error::Unsupported(e) => write!(f, "unsupported: {e}"),
            Error::Encode(e) => write!(f, "encoding failed: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

#[derive(Clone, Debug, Default)]
pub struct DecodeOptions {
    /// Bin 2×2 Bayer cells instead of demosaicing: half the resolution, much
    /// faster. Meant for previews.
    pub half_size: bool,
}

/// A decoded photo, cropped and oriented for display.
#[derive(Clone, Debug)]
pub struct Photo {
    pub info: PhotoInfo,
    /// Camera RGB for RAW files (0..1, 1 = clip, not white balanced), linear
    /// ProPhoto for rendered images. See [`Photo::to_working`].
    pub data: Image,
    pub color: ColorSource,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ColorSource {
    /// `data` is camera RGB and needs the profile and a white balance.
    Camera(Box<CameraData>),
    /// `data` is already linear ProPhoto (JPEG/PNG/TIFF input).
    Working,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CameraData {
    pub profile: ColorProfile,
    /// Camera response to the as-shot white, max component 1.
    pub as_shot_neutral: color::Vec3,
}

#[derive(Clone, Debug, Default)]
pub struct PhotoInfo {
    pub make: String,
    pub model: String,
    pub lens: Option<String>,
    pub iso: Option<u32>,
    /// Seconds.
    pub exposure_time: Option<f64>,
    pub f_number: Option<f64>,
    /// Millimetres.
    pub focal_length: Option<f64>,
    /// EXIF DateTimeOriginal, "YYYY:MM:DD HH:MM:SS".
    pub date_time: Option<String>,
    /// Already applied to `Photo::data`.
    pub orientation: Orientation,
    /// DNG BaselineExposure in EV: how much the camera underexposes on
    /// purpose. Rendering adds it to the user's exposure, as Camera Raw does.
    pub baseline_exposure: f64,
    pub raw: Option<RawInfo>,
}

#[derive(Clone, Debug)]
pub struct RawInfo {
    pub is_dng: bool,
    /// Floating point samples (an HDR merge): `white_level` is 1.0 and the
    /// data may exceed the range of a single exposure.
    pub float: bool,
    pub sensor_size: (usize, usize),
    /// x, y, width, height of the default crop on the sensor.
    pub crop: [usize; 4],
    pub cfa: String,
    pub black_level: Vec<f32>,
    pub white_level: Vec<f32>,
    pub as_shot_neutral: color::Vec3,
    pub as_shot_white: Xy,
}

/// Raw channel value above which a photosite is treated as clipped.
const CLIP: f32 = 0.995;

impl Photo {
    /// Decodes a RAW file (anything rawler supports, DNG first) or a
    /// JPEG/PNG/TIFF, chosen by extension.
    pub fn load(path: impl AsRef<Path>, opts: &DecodeOptions) -> Result<Photo, Error> {
        decode::load(path.as_ref(), opts)
    }

    /// Replaces the camera profile (e.g. with Adobe Standard from a .dcp),
    /// keeping the as-shot neutral and recomputing the as-shot white point.
    pub fn set_profile(&mut self, profile: ColorProfile) {
        let ColorSource::Camera(cam) = &mut self.color else { return };
        let white = profile.neutral_to_xy(cam.as_shot_neutral);
        cam.profile = profile;
        if let Some(raw) = &mut self.info.raw {
            raw.as_shot_white = white;
        }
    }

    pub fn camera_profile(&self) -> Option<&ColorProfile> {
        match &self.color {
            ColorSource::Camera(cam) => Some(&cam.profile),
            ColorSource::Working => None,
        }
    }

    /// The white point the camera recorded, for RAW files.
    pub fn as_shot_white(&self) -> Option<Xy> {
        self.info.raw.as_ref().map(|r| r.as_shot_white)
    }

    /// Linear ProPhoto RGB with the given white balance (as shot if `None`).
    /// Scene white maps to (1, 1, 1) when it is just at the clip point; pixels
    /// with a clipped channel are clipped to white so highlights stay neutral.
    pub fn to_working(&self, white: Option<Xy>) -> Image {
        let mut out = self.data.clone();
        self.working_in_place(&mut out, white);
        out
    }

    /// As [`Photo::to_working`], converting the photo's own pixels instead of
    /// a copy, for when the photo is not needed afterwards (an export).
    pub fn into_working(mut self, white: Option<Xy>) -> Image {
        let mut data = std::mem::replace(&mut self.data, Image::new(0, 0));
        self.working_in_place(&mut data, white);
        data
    }

    fn working_in_place(&self, out: &mut Image, white: Option<Xy>) {
        let ColorSource::Camera(cam) = &self.color else { return };
        let white = white.or(self.as_shot_white()).unwrap_or(color::D50);
        let neutral = cam.profile.xy_to_neutral(white);
        let gains = neutral.map(|n| (1.0 / n.max(1e-6)) as f32);
        let to_pcs = color::mul(&cam.profile.camera_to_xyz_d50(white), &color::diag(neutral));
        let m = color::to_f32(&color::mul(&color::xyz_to_prophoto(), &to_pcs));

        out.map(|p| {
            let clipped = p.iter().any(|&c| c >= CLIP);
            let balanced: [f32; 3] = std::array::from_fn(|c| {
                let v = p[c] * gains[c];
                if clipped {
                    v.min(1.0)
                } else {
                    v
                }
            });
            color::apply(&m, balanced)
        });
    }
}

/// Linear ProPhoto → 8-bit sRGB (interleaved RGB), with `exposure` in EV.
/// A plain clip-and-encode: tone mapping arrives with the develop pipeline.
pub fn encode_srgb8(img: &Image, exposure: f64) -> Vec<u8> {
    encode_srgb(img, exposure, 255.0)
}

/// As [`encode_srgb8`], 16 bits per channel.
pub fn encode_srgb16(img: &Image, exposure: f64) -> Vec<u16> {
    encode_srgb(img, exposure, 65535.0)
}

fn encode_srgb<T: Copy + Default + Send + FromF32>(img: &Image, exposure: f64, max: f32) -> Vec<T> {
    use rayon::prelude::*;

    let m = color::to_f32(&color::scale(&color::prophoto_to_srgb(), 2f64.powf(exposure)));
    let mut out = vec![T::default(); img.pixels.len() * 3];
    out.par_chunks_mut(3).zip(&img.pixels).enumerate().for_each(|(i, (o, p))| {
        let rgb = color::apply(&m, *p);
        for c in 0..3 {
            // ±0.5 LSB of hash noise hides banding in smooth gradients.
            let dither = ((i * 3 + c).wrapping_mul(2_654_435_761) >> 16 & 0xFF) as f32 / 255.0 - 0.5;
            let v = color::srgb_oetf(rgb[c].clamp(0.0, 1.0)) * max + dither;
            o[c] = T::from_f32(v.round().clamp(0.0, max));
        }
    });
    out
}

trait FromF32 {
    fn from_f32(v: f32) -> Self;
}

impl FromF32 for u8 {
    fn from_f32(v: f32) -> Self {
        v as u8
    }
}

impl FromF32 for u16 {
    fn from_f32(v: f32) -> Self {
        v as u16
    }
}
