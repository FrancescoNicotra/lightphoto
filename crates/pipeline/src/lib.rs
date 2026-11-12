//! The develop pipeline: applies Camera Raw settings (a preset or a sidecar)
//! to a decoded photo, in Camera Raw's order:
//!
//! ```text
//! linear ProPhoto (white balanced) ─ straighten/crop
//!   ─ calibration ─ exposure ─ shoulder (raw)
//!   → Melissa: profile curve ─ profile RGB table ─ Basic tone ─ parametric
//!     ─ point curves ─ shadow tint                                  (pass A)
//!   → local corrections (masks): point-wise sliders
//!   → colour NR ─ Texture/Clarity ─ Dehaze ─ sharpening
//!     ─ local Texture/Clarity/Dehaze/Sharpness ─ vignette           (spatial)
//!   → HSL ─ vibrance/saturation ─ colour grading ─ .cube LUT        (pass B)
//!   → display-referred linear ProPhoto; grain is added after resizing
//! ```

pub mod acr3;
pub mod adobe;
pub mod colour;
pub mod gamma;
pub mod geometry;
pub mod local;
pub mod masks;
pub mod stages;
pub mod tone;

use rayon::prelude::*;

use lut::{CubeFile, Lut3D};
use raw::color::{self, Mat3, TempTint, Xy};
use raw::hue_sat::HueSatMap;
use raw::{ColorSource, Image, Photo};
use xmp::{Curve, DevelopSettings, WhiteBalance};

use colour::{ColourOps, ColourSpace, TableOp};
pub use stages::{Recomputed, RenderCache};
use stages::{ColourKey, PhotoId, SpatialKey, ToneKey, Toned, Working, WorkingKey};
use tone::Lut1;

/// Tone curve of Adobe's built-in "Adobe Color" profile, as Lightroom spells
/// it out in DNGs (`crs:Look/crs:Parameters`). Its hue/saturation LookTable
/// is Adobe-internal and not reproduced.
const ADOBE_COLOR_CURVE: [(f32, f32); 7] =
    [(0., 0.), (22., 16.), (40., 35.), (127., 127.), (224., 230.), (240., 246.), (255., 255.)];

/// Camera Raw's named white balance presets (temperature, tint).
fn named_white_balance(name: &str) -> Option<(f64, f64)> {
    Some(match name {
        "Daylight" => (5500.0, 10.0),
        "Cloudy" => (6500.0, 10.0),
        "Shade" => (7500.0, 10.0),
        "Tungsten" => (2850.0, 0.0),
        "Fluorescent" => (3800.0, 21.0),
        "Flash" => (5500.0, 0.0),
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// Scene-referred camera data: absolute white balance, base rendering.
    Raw,
    /// An already rendered JPEG/TIFF: relative white balance, no base curve.
    Rendered,
}

impl SourceKind {
    pub fn of(photo: &Photo) -> Self {
        match photo.color {
            ColorSource::Camera(_) => SourceKind::Raw,
            ColorSource::Working => SourceKind::Rendered,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RenderOptions {
    /// Downscale so the long edge is at most this, before processing.
    pub max_size: Option<usize>,
    /// Long edge of the full-size photo, when `photo` is already a reduced
    /// copy (an app's preview). Pixel radii (sharpening, noise reduction)
    /// are scaled from it. RAW files know it from their crop; defaults to
    /// the photo's own size otherwise.
    pub full_size: Option<usize>,
}

/// The parts of a camera profile (.dcp) that act on pixels, for one photo
/// and white balance. Empty for the file's own matrices and for JPEGs.
#[derive(Clone, Debug, Default)]
pub struct ProfileStage {
    /// ProfileHueSatMap, before exposure.
    pub hue_sat: Option<HueSatMap>,
    /// ProfileLookTable, after exposure (a look's own table replaces it).
    pub look_table: Option<HueSatMap>,
    /// ProfileToneCurve on linear values; without it Adobe's default ACR3
    /// curve runs, as in the DNG SDK.
    pub tone_curve: Option<Lut1>,
    /// BaselineExposureOffset in EV.
    pub exposure_offset: f32,
}

/// Settings prepared for fast per-pixel use.
#[derive(Clone, Debug)]
pub struct Pipeline {
    pub settings: DevelopSettings,
    pub kind: SourceKind,
    /// Things in the settings that are not reproduced.
    pub warnings: Vec<String>,
    /// Calibration and (for rendered sources) relative white balance.
    matrix: Option<[[f32; 3]; 3]>,
    /// Adobe's default raw tone curve (linear → linear).
    acr3: Lut1,
    /// The look's own curve (e.g. Adobe Color's gentle S), on Melissa.
    look_curve: Lut1,
    /// The look's hue/saturation table (Adobe Color's LookTable).
    look_table: Option<HueSatMap>,
    profile_table: Option<TableOp>,
    preset_table: Option<TableOp>,
    tone: Lut1,
    channels: [Lut1; 3],
    colour: ColourOps,
    space: ColourSpace,
    cube: Option<(CubeFile, f32)>,
    /// Local corrections that change something.
    locals: Vec<masks::LocalOp>,
}

impl Pipeline {
    pub fn new(settings: &DevelopSettings, kind: SourceKind) -> Self {
        let s = settings;
        let mut warnings = Vec::new();

        let mut matrix = colour::calibration_matrix(&s.calibration);
        if kind == SourceKind::Rendered && (s.incremental_temperature != 0.0 || s.incremental_tint != 0.0) {
            let wb = relative_white_balance(s.incremental_temperature as f64, s.incremental_tint as f64);
            matrix = Some(matrix.map_or(wb, |m| color::mul(&m, &wb)));
        }

        // Profile: default rendering plus the look's own curve and tables.
        let mut look_curve = Lut1::identity();
        let mut look_table = None;
        let mut profile_table = None;
        if let Some(look) = &s.look {
            let amount = look.amount.clamp(0.0, 2.0);
            let own = look.parameter_settings().curves.master;
            let curve = if !own.is_identity() {
                Some(own)
            } else if look.name == "Adobe Color" {
                Some(Curve(ADOBE_COLOR_CURVE.to_vec()))
            } else {
                None
            };
            if let (Some(curve), SourceKind::Raw) = (curve, kind) {
                let lut = tone::spline(&curve);
                look_curve = Lut1::from_fn(|v| v + (lut.get(v) - v) * amount);
            }
            if kind == SourceKind::Raw {
                look_table = look.look_table.as_ref().and_then(|t| {
                    let map = HueSatMap::new([t.hue_divisions, t.sat_divisions, t.val_divisions], &t.floats(), t.srgb_encoded)?;
                    Some(if amount == 1.0 { map } else { scale_map(&map, amount) })
                });
            }
            profile_table = look.rgb_table.as_ref().map(|t| TableOp::new(t, look.amount));
            let unresolved = look.look_table.is_none() && look.rgb_table.is_none();
            if unresolved && (look.look_table_ref().is_some() || look.stubbed) {
                warnings.push(format!(
                    "profile \"{}\": its hue/saturation table is not available (no Adobe install found), only its tone curve is applied",
                    look.name
                ));
            } else if unresolved && look.parameter_settings().curves.master.is_identity() {
                warnings.push(format!("profile \"{}\" is built into Camera Raw and not reproduced", look.name));
            }
        }
        let preset_table = s.rgb_table.as_ref().map(|t| TableOp::new(t, 1.0));

        let tone = tone::basic(&s.basic).then(&tone::parametric(&s.parametric)).then(&tone::spline(&s.curves.master));
        let channels = [&s.curves.red, &s.curves.green, &s.curves.blue].map(tone::spline);

        if s.presence.texture != 0.0 || s.presence.clarity != 0.0 || s.presence.dehaze != 0.0 {
            warnings.push("Texture, Clarity and Dehaze are approximations of Adobe's local filters".into());
        }
        if s.detail.luminance_nr > 0.0 {
            warnings.push("luminance noise reduction is not implemented yet".into());
        }
        let locals: Vec<masks::LocalOp> = s.masks.iter().enumerate().filter_map(|(i, c)| masks::LocalOp::new(i, c)).collect();
        let unsupported = s.masks.iter().filter(|c| !c.is_supported()).count();
        if unsupported > 0 {
            warnings.push(format!("{unsupported} mask(s) of a kind Lumen cannot render (AI or range masks) are kept but not applied"));
        }

        Pipeline {
            settings: s.clone(),
            kind,
            warnings,
            matrix: matrix.map(|m| color::to_f32(&m)),
            acr3: acr3::curve(),
            look_curve,
            look_table,
            profile_table,
            preset_table,
            tone,
            channels,
            colour: ColourOps::new(&s.hsl, s.presence.vibrance, s.presence.saturation, &s.color_grade),
            space: ColourSpace::new(),
            cube: None,
            locals,
        }
    }

    /// Adds a .cube LUT applied last, on sRGB-encoded values, at `amount` (0..1+).
    pub fn with_cube(mut self, cube: CubeFile, amount: f32) -> Self {
        self.cube = Some((cube, amount));
        self
    }

    /// The white point the settings ask for; `None` means as shot.
    pub fn white_point(&self, photo: &Photo) -> Option<Xy> {
        let s = &self.settings;
        match &s.white_balance {
            WhiteBalance::AsShot => None,
            WhiteBalance::Custom => {
                s.temperature.map(|t| TempTint { temperature: t as f64, tint: s.tint.unwrap_or(0.0) as f64 }.to_xy())
            }
            WhiteBalance::Preset(name) => {
                named_white_balance(name).map(|(temperature, tint)| TempTint { temperature, tint }.to_xy())
            }
            WhiteBalance::Auto => auto_white(photo),
        }
    }

    fn exposure(&self, photo: &Photo) -> f32 {
        let baseline = if self.kind == SourceKind::Raw { photo.info.baseline_exposure as f32 } else { 0.0 };
        self.settings.basic.exposure + baseline
    }

    /// The camera profile's pixel operations for `photo` at white `white`.
    pub fn profile_stage(&self, photo: &Photo, white: Option<Xy>) -> ProfileStage {
        let (Some(profile), SourceKind::Raw) = (photo.camera_profile(), self.kind) else {
            return ProfileStage::default();
        };
        let white = white.or(photo.as_shot_white()).unwrap_or(color::D50);
        ProfileStage {
            hue_sat: profile.hue_sat_map(white),
            look_table: profile.look_table.clone(),
            tone_curve: profile
                .tone_curve
                .as_ref()
                .map(|pts| tone::spline(&Curve(pts.iter().map(|&(x, y)| (x * 255.0, y * 255.0)).collect()))),
            exposure_offset: profile.baseline_exposure_offset as f32,
        }
    }

    /// Develops `photo`. The result is display-referred linear ProPhoto in
    /// 0..1, without grain (see [`Pipeline::grain`]).
    pub fn render(&self, photo: &Photo, opts: &RenderOptions) -> Image {
        // Without a cache each stage hands its image to the next: one copy
        // alive at a time, which matters at full size.
        let white = self.white_point(photo);
        let Working { img, canvas, px } = self.working(photo, opts, white);
        let stage = self.profile_stage(photo, white);
        let weights = self.mask_weights(canvas.0, canvas.1, photo.info.orientation, img.width, img.height);
        self.process_inner(img, self.exposure(photo) + stage.exposure_offset, px, &stage, &weights)
    }

    /// As [`Pipeline::render`] for a photo that is not needed afterwards: its
    /// pixels become the working image, saving a full-size copy. Not for
    /// reduced renders (`max_size`), which make their own copy anyway.
    pub fn render_owned(&self, photo: Photo, opts: &RenderOptions) -> Image {
        if opts.max_size.is_some_and(|n| photo.data.width.max(photo.data.height) > n) {
            return self.render(&photo, opts);
        }
        let white = self.white_point(&photo);
        let stage = self.profile_stage(&photo, white);
        let exposure = self.exposure(&photo) + stage.exposure_offset;
        let full = opts.full_size.unwrap_or_else(|| {
            photo.info.raw.as_ref().map_or(photo.data.width.max(photo.data.height), |r| r.crop[2].max(r.crop[3]))
        });
        let orientation = photo.info.orientation;
        let img = photo.into_working(white);
        let px = img.width.max(img.height) as f32 / full as f32;
        let canvas = (img.width, img.height);
        let img = geometry::apply(img, &self.settings.crop, orientation);
        let weights = self.mask_weights(canvas.0, canvas.1, orientation, img.width, img.height);
        self.process_inner(img, exposure, px, &stage, &weights)
    }

    /// As [`Pipeline::render`], reusing the stages of the previous render
    /// kept in `cache` whose inputs have not changed.
    pub fn render_cached(&self, photo: &Photo, opts: &RenderOptions, cache: &mut RenderCache) -> Image {
        let s = &self.settings;
        let white = self.white_point(photo);
        let mut last = Recomputed::default();

        let key = WorkingKey { photo: PhotoId::of(photo), max_size: opts.max_size, full_size: opts.full_size, white, crop: s.crop.clone() };
        if cache.working.as_ref().is_none_or(|(k, _)| *k != key) {
            let working = self.working(photo, opts, white);
            cache.working = Some((key, working));
            (cache.tone, cache.spatial, cache.colour) = (None, None, None);
            last.working = true;
        }
        let working = &cache.working.as_ref().expect("just filled").1;

        let stage = self.profile_stage(photo, white);
        let key = ToneKey {
            exposure: self.exposure(photo) + stage.exposure_offset,
            calibration: s.calibration.clone(),
            incremental: (s.incremental_temperature, s.incremental_tint),
            look: s.look.clone(),
            rgb_table: s.rgb_table.clone(),
            basic: s.basic.clone(),
            parametric: s.parametric.clone(),
            curves: s.curves.clone(),
            masks: s.masks.clone(),
        };
        if cache.tone.as_ref().is_none_or(|(k, _)| *k != key) {
            let (w, h) = working.canvas;
            let weights = self.mask_weights(w, h, photo.info.orientation, working.img.width, working.img.height);
            let img = self.tone_stage(working.img.clone(), key.exposure, &stage, &weights);
            cache.tone = Some((key, Toned { img, weights }));
            (cache.spatial, cache.colour) = (None, None);
            last.tone = true;
        }
        let toned = &cache.tone.as_ref().expect("just filled").1;

        let (p, e) = (&s.presence, &s.effects);
        let key = SpatialKey {
            detail: s.detail.clone(),
            local: [p.texture, p.clarity, p.dehaze],
            vignette: [e.vignette_amount, e.vignette_midpoint, e.vignette_roundness, e.vignette_feather],
        };
        if cache.spatial.as_ref().is_none_or(|(k, _)| *k != key) {
            let img = self.spatial_stage(toned.img.clone(), working.px, &toned.weights);
            cache.spatial = Some((key, img));
            cache.colour = None;
            last.spatial = true;
        }
        let spatial = &cache.spatial.as_ref().expect("just filled").1;

        let key = ColourKey {
            hsl: s.hsl.clone(),
            vibrance: s.presence.vibrance,
            saturation: s.presence.saturation,
            color_grade: s.color_grade.clone(),
            cube: self.cube.clone(),
        };
        if cache.colour.as_ref().is_none_or(|(k, _)| *k != key) {
            let img = self.colour_stage(spatial.clone());
            cache.colour = Some((key, img));
            last.colour = true;
        }
        cache.last = last;
        cache.colour.as_ref().expect("just filled").1.clone()
    }

    /// Reduced to the render size, white balanced, cropped.
    fn working(&self, photo: &Photo, opts: &RenderOptions, white: Option<Xy>) -> Working {
        let small;
        let src = match opts.max_size {
            Some(n) if photo.data.width.max(photo.data.height) > n => {
                small = Photo { data: photo.data.downscale(n), info: photo.info.clone(), color: photo.color.clone() };
                &small
            }
            _ => photo,
        };
        let img = src.to_working(white);
        // Pixel radii are given for the full-size image.
        let full = opts.full_size.unwrap_or_else(|| {
            photo.info.raw.as_ref().map_or(photo.data.width.max(photo.data.height), |r| r.crop[2].max(r.crop[3]))
        });
        let px = img.width.max(img.height) as f32 / full as f32;
        // Cropping first: the post-crop vignette and the other spatial
        // operations then see the final frame.
        let canvas = (img.width, img.height);
        let img = geometry::apply(img, &self.settings.crop, photo.info.orientation);
        Working { img, canvas, px }
    }

    /// Weight maps of the local corrections that change something, for a
    /// developed image of `out_w`×`out_h` from an oriented
    /// `canvas_w`×`canvas_h` photo.
    fn mask_weights(&self, canvas_w: usize, canvas_h: usize, o: raw::Orientation, out_w: usize, out_h: usize) -> Vec<Vec<f32>> {
        let frame = masks::Frame::new(canvas_w, canvas_h, &self.settings.crop, o, out_w, out_h);
        self.locals
            .iter()
            .map(|op| masks::correction_weights(&self.settings.masks[op.index], &frame).unwrap_or_else(|| vec![0.0; out_w * out_h]))
            .collect()
    }

    /// Runs the pipeline on white-balanced linear ProPhoto. `px` is this
    /// image's size relative to the full-size photo (for pixel radii).
    pub fn process(&self, img: Image, exposure: f32, px: f32) -> Image {
        self.process_with(img, exposure, px, &ProfileStage::default())
    }

    pub fn process_with(&self, img: Image, exposure: f32, px: f32, stage: &ProfileStage) -> Image {
        // Without a frame the masks cannot be placed: they are left out.
        self.process_inner(img, exposure, px, stage, &[])
    }

    /// The shape of mask `index` (ignoring its Amount and on/off switch)
    /// on a developed `out_w`×`out_h` image of `photo`, for an overlay.
    pub fn mask_overlay(&self, photo: &Photo, index: usize, out_w: usize, out_h: usize) -> Option<Vec<f32>> {
        let c = self.settings.masks.get(index)?;
        let shown = xmp::masks::LocalCorrection { amount: 1.0, active: true, ..c.clone() };
        let frame = masks::Frame::new(photo.data.width, photo.data.height, &self.settings.crop, photo.info.orientation, out_w, out_h);
        masks::correction_weights(&shown, &frame)
    }

    /// `weights` holds one map per entry of `self.locals` (or none at all).
    fn process_inner(&self, img: Image, exposure: f32, px: f32, stage: &ProfileStage, weights: &[Vec<f32>]) -> Image {
        let img = self.tone_stage(img, exposure, stage, weights);
        let img = self.spatial_stage(img, px, weights);
        self.colour_stage(img)
    }

    /// Linear → Melissa: profile, tone and the point-wise part of the masks.
    fn tone_stage(&self, mut img: Image, exposure: f32, stage: &ProfileStage, weights: &[Vec<f32>]) -> Image {
        let gain = 2f32.powf(exposure);
        img.pixels.par_iter_mut().for_each(|p| *p = self.pass_a(*p, gain, stage));
        let locals: Vec<(&masks::LocalOp, &Vec<f32>)> = self.locals.iter().zip(weights).collect();
        if !locals.is_empty() {
            img.pixels.par_iter_mut().enumerate().for_each(|(i, p)| {
                for (op, w) in &locals {
                    if w[i] > 0.0 {
                        *p = op.point(*p, w[i]);
                    }
                }
            });
        }
        img
    }

    /// Filters that look at neighbouring pixels, on Melissa values.
    fn spatial_stage(&self, mut img: Image, px: f32, weights: &[Vec<f32>]) -> Image {
        let s = &self.settings;
        local::color_nr(&mut img, s.detail.color_nr, px);
        if s.presence.texture != 0.0 || s.presence.clarity != 0.0 {
            local::local_contrast(&mut img, s.presence.texture, s.presence.clarity);
        }
        if s.presence.dehaze != 0.0 {
            local::dehaze(&mut img, s.presence.dehaze);
        }
        let d = &s.detail;
        local::sharpen(&mut img, d.sharpness, d.sharpen_radius, d.sharpen_detail, d.sharpen_edge_masking, px);
        for (op, w) in self.locals.iter().zip(weights) {
            op.spatial(&mut img, w, px);
        }
        let e = &s.effects;
        local::vignette(&mut img, e.vignette_amount, e.vignette_midpoint, e.vignette_roundness, e.vignette_feather);
        img
    }

    /// Melissa → colour controls → display linear ProPhoto.
    fn colour_stage(&self, mut img: Image) -> Image {
        img.pixels.par_iter_mut().for_each(|p| *p = self.pass_b(*p));
        img
    }

    /// The point-wise part of the pipeline for one pixel (no spatial
    /// operations): white-balanced linear ProPhoto → display linear ProPhoto.
    pub fn point(&self, p: [f32; 3], exposure: f32) -> [f32; 3] {
        self.pass_b(self.pass_a(p, 2f32.powf(exposure), &ProfileStage::default()))
    }

    /// Linear → Melissa, with everything up to the curves. The profile part
    /// follows the DNG SDK: HueSatMap, exposure, LookTable, tone curve.
    fn pass_a(&self, p: [f32; 3], gain: f32, stage: &ProfileStage) -> [f32; 3] {
        let mut p = match &self.matrix {
            Some(m) => color::apply(m, p),
            None => p,
        };
        if let Some(map) = &stage.hue_sat {
            p = map.apply(p.map(|v| v.max(0.0)));
        }
        p = p.map(|v| v * gain);
        p = match self.kind {
            SourceKind::Raw => p.map(tone::shoulder),
            SourceKind::Rendered => p.map(|v| v.clamp(0.0, 1.0)),
        };
        if let Some(map) = self.look_table.as_ref().or(stage.look_table.as_ref()) {
            p = map.apply(p);
        }
        let mut e = match self.kind {
            SourceKind::Raw => tone::rgb_tone(p, stage.tone_curve.as_ref().unwrap_or(&self.acr3)).map(gamma::oetf),
            SourceKind::Rendered => p.map(gamma::oetf),
        };
        e = e.map(|v| self.look_curve.get(v));
        if let Some(t) = &self.profile_table {
            e = t.apply(e.map(gamma::eotf)).map(|v| gamma::oetf(v.clamp(0.0, 1.0)));
        }
        if let Some(t) = &self.preset_table {
            e = t.apply(e.map(gamma::eotf)).map(|v| gamma::oetf(v.clamp(0.0, 1.0)));
        }
        e = std::array::from_fn(|c| self.channels[c].get(self.tone.get(e[c])));
        let tint = self.settings.calibration.shadow_tint;
        if tint != 0.0 {
            // Positive is magenta (less green), in the shadows only.
            let shadow = 1.0 - (local::luma(e) / 0.4).clamp(0.0, 1.0);
            e[1] -= tint / 100.0 * 0.05 * shadow * shadow;
        }
        e
    }

    /// Melissa → colour controls → display linear ProPhoto.
    fn pass_b(&self, e: [f32; 3]) -> [f32; 3] {
        let mut lin = e.map(|v| gamma::eotf(v.clamp(0.0, 1.0)));
        if !self.colour.is_identity() {
            lin = self.space.decode(self.colour.apply(self.space.encode(lin)));
        }
        if let Some((cube, amount)) = &self.cube {
            let to_srgb = color::to_f32(&color::prophoto_to_srgb());
            let from_srgb = color::to_f32(&color::srgb_to_prophoto());
            let s = color::apply(&to_srgb, lin).map(|v| color::srgb_oetf(v.clamp(0.0, 1.0)));
            let out = cube.apply(s);
            let mixed: [f32; 3] = std::array::from_fn(|c| s[c] + (out[c] - s[c]) * amount);
            lin = color::apply(&from_srgb, mixed.map(color::srgb_eotf));
        }
        lin.map(|v| v.clamp(0.0, 1.0))
    }

    /// Adds the preset's grain; run it after resizing to the output size.
    pub fn grain(&self, img: &mut Image) {
        let e = &self.settings.effects;
        if e.grain_amount <= 0.0 {
            return;
        }
        img.map(|p| p.map(gamma::oetf));
        local::grain(img, e.grain_amount, e.grain_size, e.grain_frequency, 0);
        img.map(|p| p.map(|v| gamma::eotf(v.clamp(0.0, 1.0))));
    }

    /// The point-wise part of the settings as a 3D LUT on sRGB-encoded
    /// values, for applying the look to already rendered images. White
    /// balance, Texture/Clarity/Dehaze, sharpening, NR, vignette and grain
    /// cannot be expressed in a LUT and are left out.
    pub fn to_cube(settings: &DevelopSettings, size: usize) -> Lut3D {
        let p = Pipeline::new(settings, SourceKind::Rendered);
        let to_pp = color::to_f32(&color::srgb_to_prophoto());
        let to_srgb = color::to_f32(&color::prophoto_to_srgb());
        let exposure = settings.basic.exposure;
        Lut3D::from_fn(size, |rgb| {
            let lin = color::apply(&to_pp, rgb.map(color::srgb_eotf));
            let out = p.point(lin, exposure);
            color::apply(&to_srgb, out).map(|v| color::srgb_oetf(v.clamp(0.0, 1.0)))
        })
    }
}

/// Relative white balance for rendered images: Temperature/Tint −100..100
/// move the assumed light by up to ±100 mired from D50 (5003 K).
fn relative_white_balance(temperature: f64, tint: f64) -> Mat3 {
    let d50 = TempTint::from_xy(color::D50);
    let mired = 1.0e6 / d50.temperature - temperature;
    let light = TempTint { temperature: 1.0e6 / mired.max(20.0), tint: d50.tint + tint }.to_xy();
    let p = color::prophoto_to_xyz();
    color::mul(&color::invert(&p).unwrap(), &color::mul(&color::adaptation(light, color::D50), &p))
}

/// A look table at `amount` (0..2): shifts and scales move toward identity.
fn scale_map(map: &HueSatMap, amount: f32) -> HueSatMap {
    let data = map.data.iter().map(|d| [d[0] * amount, 1.0 + (d[1] - 1.0) * amount, 1.0 + (d[2] - 1.0) * amount]).collect();
    HueSatMap { data, ..map.clone() }
}

/// Grey-world estimate on camera data, ignoring clipped and very dark pixels.
fn auto_white(photo: &Photo) -> Option<Xy> {
    let ColorSource::Camera(cam) = &photo.color else { return None };
    let small = photo.data.downscale(512);
    let mut sum = [0.0f64; 3];
    for p in &small.pixels {
        let max = p[0].max(p[1]).max(p[2]);
        if max < 0.95 && max > 0.02 {
            (0..3).for_each(|c| sum[c] += p[c] as f64);
        }
    }
    (sum[1] > 0.0).then(|| cam.profile.neutral_to_xy(raw::profile::normalize_max(sum)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_settings_on_rendered_images_are_identity() {
        let mut s = DevelopSettings::default();
        s.detail.sharpness = 0.0;
        s.detail.color_nr = 0.0;
        let p = Pipeline::new(&s, SourceKind::Rendered);
        for v in [[0.0, 0.0, 0.0], [0.18, 0.18, 0.18], [0.6, 0.3, 0.1], [1.0, 1.0, 1.0]] {
            let out = p.point(v, 0.0);
            assert!(out.iter().zip(v).all(|(a, b)| (a - b).abs() < 2e-3), "{v:?} -> {out:?}");
        }
    }

    #[test]
    fn neutral_cube_is_identity() {
        let lut = Pipeline::to_cube(&DevelopSettings::default(), 9);
        let identity = Lut3D::identity(9);
        for (a, b) in lut.data.iter().zip(&identity.data) {
            assert!(a.iter().zip(b).all(|(x, y)| (x - y).abs() < 3e-3), "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn exposure_and_raw_base_rendering() {
        let s = DevelopSettings::default();
        let p = Pipeline::new(&s, SourceKind::Raw);
        let grey = p.point([0.18; 3], 0.0)[1];
        let brighter = p.point([0.18; 3], 1.0)[1];
        assert!(brighter > grey * 1.6, "{grey} {brighter}");
        // Clipped highlights land near white, not beyond.
        let white = p.point([4.0; 3], 0.0);
        assert!(white.iter().all(|v| *v > 0.97 && *v <= 1.0), "{white:?}");
    }

    #[test]
    fn crop_comes_before_the_vignette() {
        let photo = Photo { info: raw::PhotoInfo::default(), data: Image::from_fn(200, 100, |_, _| [0.5; 3]), color: ColorSource::Working };
        let mut s = DevelopSettings::default();
        s.detail.sharpness = 0.0;
        s.detail.color_nr = 0.0;
        s.effects.vignette_amount = -100.0;
        s.crop = xmp::Crop { has_crop: true, top: 0.0, left: 0.5, bottom: 1.0, right: 1.0, angle: 0.0 };
        let out = Pipeline::new(&s, SourceKind::Rendered).render(&photo, &RenderOptions::default());
        assert_eq!((out.width, out.height), (100, 100));
        // Darkened at the corners of the cropped frame, not of the original.
        let (corner, centre) = (out.get(1, 1)[1], out.get(50, 50)[1]);
        assert!(corner < centre * 0.6, "{corner} {centre}");
        let (left_edge, right_edge) = (out.get(1, 50)[1], out.get(98, 50)[1]);
        assert!((left_edge - right_edge).abs() < 0.02, "{left_edge} {right_edge}");
    }

    #[test]
    fn masks_act_where_they_are() {
        use xmp::masks::{LocalCorrection, MaskComponent, MaskShape};
        let photo = Photo { info: raw::PhotoInfo::default(), data: Image::from_fn(100, 100, |_, _| [0.18; 3]), color: ColorSource::Working };
        let mut s = DevelopSettings::default();
        s.detail.sharpness = 0.0;
        s.detail.color_nr = 0.0;
        // Top half brighter by a gradient that is full above y = 0.4.
        let mut c = LocalCorrection::new("sky", vec![MaskComponent::new(MaskShape::Linear { zero: (0.5, 0.6), full: (0.5, 0.4) })]);
        c.adjust.exposure = 1.0;
        s.masks.push(c);
        let p = Pipeline::new(&s, SourceKind::Rendered);
        let out = p.render(&photo, &RenderOptions::default());
        let (top, bottom) = (out.get(50, 10)[1], out.get(50, 90)[1]);
        assert!(top > bottom * 1.6, "{top} {bottom}");
        assert!((bottom - 0.18).abs() < 0.01, "{bottom}");
        let overlay = p.mask_overlay(&photo, 0, 100, 100).unwrap();
        assert!(overlay[10 * 100] > 0.99 && overlay[90 * 100] < 0.01);
    }

    #[test]
    fn relative_white_balance_warms() {
        let m = color::to_f32(&relative_white_balance(30.0, 0.0));
        let grey = color::apply(&m, [0.5; 3]);
        assert!(grey[0] > grey[2], "{grey:?}");
        let w = color::mul_vec(&relative_white_balance(0.0, 0.0), [1.0; 3]);
        assert!(w.iter().all(|v| (v - 1.0).abs() < 2e-3), "{w:?}");
    }
}
