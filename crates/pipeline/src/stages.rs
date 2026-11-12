//! Stage cache: the intermediate images of the last render, kept so that a
//! change to one group of settings only redoes that stage and the ones
//! after it. Moving a colour slider reuses the toned and filtered image;
//! moving a tone slider reuses the decoded, white-balanced and cropped one.
//!
//! ```text
//! working  ─ reduce to the render size, white balance, crop
//! tone     ─ calibration, exposure, profile, Basic, curves, masks (point)
//! spatial  ─ colour NR, Texture/Clarity, Dehaze, sharpening, masks, vignette
//! colour   ─ HSL, vibrance/saturation, colour grading, .cube LUT
//! ```
//!
//! Each stage stores the inputs it was computed from and is reused only if
//! they compare equal, so a stale result cannot leak through. The photo is
//! identified by its pixel buffer, size, a sample of its pixels and its
//! colour data, so swapping a camera profile in place also invalidates.

use raw::color::Xy;
use raw::{ColorSource, Image, Orientation, Photo};
use xmp::{Basic, Calibration, ColorGrade, Crop, Detail, Hsl, Look, ParametricCurve, RgbTable, ToneCurves};

/// What the last render recomputed (for tests and timing).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Recomputed {
    pub working: bool,
    pub tone: bool,
    pub spatial: bool,
    pub colour: bool,
}

/// Intermediate images of the last render of one photo at one size. Keep
/// one per render size (a draft and a preview, say): renders at different
/// sizes would keep evicting each other.
#[derive(Default)]
pub struct RenderCache {
    pub(crate) working: Option<(WorkingKey, Working)>,
    pub(crate) tone: Option<(ToneKey, Toned)>,
    pub(crate) spatial: Option<(SpatialKey, Image)>,
    pub(crate) colour: Option<(ColourKey, Image)>,
    /// What the last render had to recompute.
    pub last: Recomputed,
}

impl RenderCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Bytes held by the cached images.
    pub fn memory(&self) -> usize {
        let size = |img: &Image| img.pixels.len() * std::mem::size_of::<[f32; 3]>();
        self.working.as_ref().map_or(0, |(_, w)| size(&w.img))
            + self.tone.as_ref().map_or(0, |(_, t)| size(&t.img) + t.weights.iter().map(|w| w.len() * 4).sum::<usize>())
            + self.spatial.as_ref().map_or(0, |(_, i)| size(i))
            + self.colour.as_ref().map_or(0, |(_, i)| size(i))
    }
}

/// Cheap identity of a photo's pixels: buffer address, size and a sample.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PhotoId {
    ptr: usize,
    width: usize,
    height: usize,
    sample: Vec<u32>,
    color: ColorSource,
    baseline_exposure: f64,
    orientation: Orientation,
}

impl PhotoId {
    pub(crate) fn of(photo: &Photo) -> Self {
        let px = &photo.data.pixels;
        let step = (px.len() / 61).max(1);
        let sample = px.iter().step_by(step).take(64).flat_map(|p| p.map(f32::to_bits)).collect();
        PhotoId {
            ptr: px.as_ptr() as usize,
            width: photo.data.width,
            height: photo.data.height,
            sample,
            color: photo.color.clone(),
            baseline_exposure: photo.info.baseline_exposure,
            orientation: photo.info.orientation,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WorkingKey {
    pub photo: PhotoId,
    pub max_size: Option<usize>,
    pub full_size: Option<usize>,
    pub white: Option<Xy>,
    pub crop: Crop,
}

/// The white-balanced, cropped image and what later stages need about it.
#[derive(Clone)]
pub(crate) struct Working {
    pub img: Image,
    /// Size before cropping, for placing masks.
    pub canvas: (usize, usize),
    /// Size relative to the full-size photo.
    pub px: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ToneKey {
    pub exposure: f32,
    pub calibration: Calibration,
    pub incremental: (f32, f32),
    pub look: Option<Look>,
    pub rgb_table: Option<RgbTable>,
    pub basic: Basic,
    pub parametric: ParametricCurve,
    pub curves: ToneCurves,
    pub masks: Vec<xmp::masks::LocalCorrection>,
}

pub(crate) struct Toned {
    pub img: Image,
    /// Mask weights, one per local correction that changes something.
    pub weights: Vec<Vec<f32>>,
}

/// Grain is not here: it is added after the render, at the output size.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpatialKey {
    pub detail: Detail,
    /// Texture, Clarity, Dehaze.
    pub local: [f32; 3],
    /// Post-crop vignette: amount, midpoint, roundness, feather.
    pub vignette: [f32; 4],
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ColourKey {
    pub hsl: Hsl,
    pub vibrance: f32,
    pub saturation: f32,
    pub color_grade: ColorGrade,
    pub cube: Option<(lut::CubeFile, f32)>,
}
