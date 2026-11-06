//! The develop pipeline: applies Camera Raw settings (a preset or a sidecar)
//! to a decoded photo, in Camera Raw's order:
//!
//! ```text
//! linear ProPhoto (white balanced) ─ straighten/crop
//!   ─ calibration ─ exposure ─ shoulder (raw)
//!   → Melissa: profile curve ─ profile RGB table ─ Basic tone ─ parametric
//!     ─ point curves ─ shadow tint                                  (pass A)
//!   → colour NR ─ Texture/Clarity ─ Dehaze ─ sharpening ─ vignette  (spatial)
//!   → HSL ─ vibrance/saturation ─ colour grading ─ .cube LUT        (pass B)
//!   → display-referred linear ProPhoto; grain is added after resizing
//! ```

pub mod acr3;
pub mod adobe;
pub mod colour;
pub mod geometry;
pub mod local;
pub mod tone;

use rayon::prelude::*;

use lut::{CubeFile, Lut3D};
use raw::color::{self, Mat3, TempTint, Xy};
use raw::hue_sat::HueSatMap;
use raw::{ColorSource, Image, Photo};
use xmp::{Curve, DevelopSettings, WhiteBalance};

use colour::{ColourOps, ColourSpace, TableOp};
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
        let small;
        let src = match opts.max_size {
            Some(n) if photo.data.width.max(photo.data.height) > n => {
                small = Photo { data: photo.data.downscale(n), info: photo.info.clone(), color: photo.color.clone() };
                &small
            }
            _ => photo,
        };
        let white = self.white_point(photo);
        let img = src.to_working(white);
        // Pixel radii are given for the full-size image.
        let full = opts.full_size.unwrap_or_else(|| {
            photo.info.raw.as_ref().map_or(photo.data.width.max(photo.data.height), |r| r.crop[2].max(r.crop[3]))
        });
        let px = img.width.max(img.height) as f32 / full as f32;
        // Cropping first: the post-crop vignette and the other spatial
        // operations then see the final frame.
        let img = geometry::apply(img, &self.settings.crop, photo.info.orientation);
        let stage = self.profile_stage(photo, white);
        self.process_with(img, self.exposure(photo) + stage.exposure_offset, px, &stage)
    }

    /// Runs the pipeline on white-balanced linear ProPhoto. `px` is this
    /// image's size relative to the full-size photo (for pixel radii).
    pub fn process(&self, img: Image, exposure: f32, px: f32) -> Image {
        self.process_with(img, exposure, px, &ProfileStage::default())
    }

    pub fn process_with(&self, mut img: Image, exposure: f32, px: f32, stage: &ProfileStage) -> Image {
        let gain = 2f32.powf(exposure);
        img.pixels.par_iter_mut().for_each(|p| *p = self.pass_a(*p, gain, stage));

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
        let e = &s.effects;
        local::vignette(&mut img, e.vignette_amount, e.vignette_midpoint, e.vignette_roundness, e.vignette_feather);

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
            SourceKind::Raw => tone::rgb_tone(p, stage.tone_curve.as_ref().unwrap_or(&self.acr3)).map(color::srgb_oetf),
            SourceKind::Rendered => p.map(color::srgb_oetf),
        };
        e = e.map(|v| self.look_curve.get(v));
        if let Some(t) = &self.profile_table {
            e = t.apply(e.map(color::srgb_eotf)).map(|v| color::srgb_oetf(v.clamp(0.0, 1.0)));
        }
        if let Some(t) = &self.preset_table {
            e = t.apply(e.map(color::srgb_eotf)).map(|v| color::srgb_oetf(v.clamp(0.0, 1.0)));
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
        let mut lin = e.map(|v| color::srgb_eotf(v.clamp(0.0, 1.0)));
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
        img.map(|p| p.map(color::srgb_oetf));
        local::grain(img, e.grain_amount, e.grain_size, e.grain_frequency, 0);
        img.map(|p| p.map(|v| color::srgb_eotf(v.clamp(0.0, 1.0))));
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
    fn relative_white_balance_warms() {
        let m = color::to_f32(&relative_white_balance(30.0, 0.0));
        let grey = color::apply(&m, [0.5; 3]);
        assert!(grey[0] > grey[2], "{grey:?}");
        let w = color::mul_vec(&relative_white_balance(0.0, 0.0), [1.0; 3]);
        assert!(w.iter().all(|v| (v - 1.0).abs() < 2e-3), "{w:?}");
    }
}
