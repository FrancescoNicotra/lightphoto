//! Merging RAW exposure brackets into a floating point HDR DNG, in linear
//! camera RGB (after demosaic, before white balance), as Lightroom does.
//!
//! ```text
//! pass 1, half size, all shots in memory:
//!   decode ─ validate ─ EXIF exposures ─ align (to the neighbour in exposure)
//!   ─ warp ─ exposure ratios from the pixels ─ ghost masks ─ auto crop
//! pass 2, full resolution, one shot at a time:
//!   decode ─ warp (bands of rows) ─ Σ w·v/e, Σ w (+ ghost sources)
//! output: scale + BaselineExposure ─ preview ─ XMP ─ write_linear_dng
//! ```
//!
//! See `docs/merge.md`.

mod bracket;
mod crop;
pub mod exposure;
pub mod ghost;
mod hdr;
pub mod output;
pub mod weights;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use align::{AlignParams, Model, Plane, Transform};
use pipeline::adobe::AdobeProfiles;
use raw::{Capture, DngEncoding, Image, LinearDng};
use rayon::prelude::*;

pub use bracket::Source;
pub use exposure::EvSource;
pub use hdr::Shot;
pub use output::{default_output_path, CropRect};

use hdr::FramePlan;
use weights::{FrameWeight, DARKEST_FLOOR, REFERENCE_FLOOR};

/// At most this many shots, as in Lightroom.
pub const MAX_FRAMES: usize = 9;

#[derive(Clone, Debug)]
pub struct MergeOptions {
    /// Align the shots (hand-held). Off for a tripod.
    pub align: bool,
    /// Write a `crs:Crop*` rectangle excluding the borders that not every
    /// aligned shot covers.
    pub auto_crop: bool,
    pub encoding: DngEncoding,
    /// Adobe profiles for the embedded preview (empty = the file's matrices).
    pub adobe: AdobeProfiles,
}

impl Default for MergeOptions {
    fn default() -> Self {
        MergeOptions { align: true, auto_crop: false, encoding: DngEncoding::HalfDeflate, adobe: AdobeProfiles::default() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Decode,
    Align,
    Exposure,
    Ghosts,
    Merge,
    Write,
}

/// Progress reports and cancellation.
#[derive(Clone, Copy, Default)]
pub struct Control<'a> {
    pub progress: Option<&'a (dyn Fn(Stage, f32) + Sync)>,
    pub cancel: Option<&'a AtomicBool>,
}

impl Control<'_> {
    pub fn report(&self, stage: Stage, fraction: f32) {
        if let Some(p) = self.progress {
            p(stage, fraction);
        }
    }

    pub fn check(&self) -> Result<(), MergeError> {
        match self.cancel {
            Some(c) if c.load(Ordering::Relaxed) => Err(MergeError::Cancelled),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MergeError {
    /// Fewer than 2 or more than [`MAX_FRAMES`] shots.
    FrameCount(usize),
    /// JPEG/TIFF files or DNGs that are already merges (floating point).
    NotRaw(Vec<String>),
    Decode { frame: String, message: String },
    /// "name: Make Model" for every shot.
    DifferentCameras(Vec<String>),
    /// Sensor size, crop or CFA differ.
    DifferentSensors(Vec<String>),
    DifferentOrientations(Vec<String>),
    /// Neither EXIF nor the pixels give these shots' exposure.
    NoExposure(Vec<String>),
    Cancelled,
    Write(String),
}

impl std::fmt::Display for MergeError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            MergeError::FrameCount(n) => write!(f, "an HDR merge needs 2 to {MAX_FRAMES} photos, not {n}"),
            MergeError::NotRaw(v) => write!(f, "not RAW files (or already merged): {}", v.join(", ")),
            MergeError::Decode { frame, message } => write!(f, "{frame}: {message}"),
            MergeError::DifferentCameras(v) => write!(f, "photos from different cameras: {}", v.join("; ")),
            MergeError::DifferentSensors(v) => write!(f, "photos with different sensor size, crop or colour filter: {}", v.join("; ")),
            MergeError::DifferentOrientations(v) => write!(f, "photos with different orientations: {}", v.join("; ")),
            MergeError::NoExposure(v) => {
                write!(f, "cannot tell the exposure of {} (no EXIF, too few usable pixels): leave it out of the bracket", v.join(", "))
            }
            MergeError::Cancelled => f.write_str("merge cancelled"),
            MergeError::Write(e) => write!(f, "writing the DNG failed: {e}"),
        }
    }
}

impl std::error::Error for MergeError {}

#[derive(Clone, Debug, PartialEq)]
pub enum Warning {
    /// No exposure time in these shots' EXIF: estimated from the pixels.
    MissingExif(Vec<String>),
    /// The measured exposure differs from EXIF by more than 0.3 EV; the
    /// measurement is used.
    ExifMismatch { frame: String, exif_ev: f64, pixel_ev: f64 },
    /// Every shot has the same exposure: the merge is a noise-reducing average.
    SameExposure,
    IsoSpread { min: u32, max: u32 },
    FocalLength { min: f64, max: f64 },
    /// The shot could not be aligned and is merged as shot; ghost removal is
    /// raised to at least Medium.
    AlignmentFailed(String),
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Warning::MissingExif(v) => write!(f, "no exposure data in {}: exposure estimated from the pixels", v.join(", ")),
            Warning::ExifMismatch { frame, exif_ev, pixel_ev } => {
                write!(f, "{frame}: measured exposure {pixel_ev:+.2} EV, EXIF says {exif_ev:+.2} EV; using the measurement")
            }
            Warning::SameExposure => f.write_str("the photos have the same exposure: not a bracket, the merge averages them to reduce noise"),
            Warning::IsoSpread { min, max } => write!(f, "ISO varies from {min} to {max} (auto ISO?)"),
            Warning::FocalLength { min, max } => write!(f, "focal length varies from {min:.1} to {max:.1} mm"),
            Warning::AlignmentFailed(n) => write!(f, "{n}: alignment failed, merged without alignment"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AlignStatus {
    Reference,
    Disabled,
    Aligned { correlation: f32, model: Model },
    Failed,
}

/// What the merge found out about one shot.
#[derive(Clone, Debug)]
pub struct FrameInfo {
    pub name: String,
    /// log₂ of the exposure relative to the reference.
    pub ev: f64,
    /// The same from EXIF, if known.
    pub exif_ev: Option<f64>,
    pub ev_source: EvSource,
    pub alignment: AlignStatus,
    /// Reference → this shot at full resolution (identity when not aligned).
    pub transform: Transform,
}

#[derive(Clone, Debug, Default)]
pub struct Timings {
    pub decode_half: Duration,
    pub align: Duration,
    pub exposure: Duration,
    pub ghosts: Duration,
    pub decode_full: Duration,
    /// Warping and accumulating at full resolution.
    pub accumulate: Duration,
    pub preview: Duration,
    pub write: Duration,
}

impl Timings {
    pub fn total(&self) -> Duration {
        self.decode_half + self.align + self.exposure + self.ghosts + self.decode_full + self.accumulate + self.preview + self.write
    }
}

/// Pass 1 done: everything but the full-resolution merge.
pub struct Prepared {
    pub frames: Vec<FrameInfo>,
    pub reference: usize,
    pub warnings: Vec<Warning>,
    pub crop: Option<CropRect>,
    /// DNG BaselineNoise: median noise relative to the reference alone.
    pub baseline_noise: f64,
    /// The half-size shots, warped into the reference frame.
    pub shots: Vec<Shot>,
    pub timings: Timings,
    plans: Vec<FramePlan>,
    /// Reference metadata (no pixels).
    meta: Capture,
}

/// Decodes the shots at half size and works out exposures, alignment and
/// ghosts.
pub fn prepare(sources: Vec<Source>, opts: &MergeOptions, ctl: &Control) -> Result<Prepared, MergeError> {
    let n = sources.len();
    if !(2..=MAX_FRAMES).contains(&n) {
        return Err(MergeError::FrameCount(n));
    }
    let rendered = bracket::rendered_files(&sources);
    if !rendered.is_empty() {
        return Err(MergeError::NotRaw(rendered));
    }
    let names: Vec<String> = sources.iter().map(Source::name).collect();
    let mut timings = Timings::default();

    // Decode.
    let t = Instant::now();
    let mut halves = Vec::with_capacity(n);
    for (i, s) in sources.iter().enumerate() {
        ctl.check()?;
        halves.push(s.load(true)?);
        ctl.report(Stage::Decode, (i + 1) as f32 / n as f32);
    }
    let mut warnings = bracket::validate(&names, &halves)?;
    timings.decode_half = t.elapsed();

    // Order by exposure: EXIF, or brightness without it.
    let t = Instant::now();
    let infos: Vec<&raw::PhotoInfo> = halves.iter().map(|c| &c.info).collect();
    let exif = exposure::exif_exposures(&infos);
    if exif.is_none() {
        let missing = names.iter().zip(&infos).filter(|(_, i)| i.exposure_time.is_none_or(|t| t <= 0.0)).map(|(n, _)| n.clone()).collect();
        warnings.push(Warning::MissingExif(missing));
    }
    let mut shots: Vec<Shot> = halves
        .iter_mut()
        .map(|c| Shot { data: std::mem::replace(&mut c.data, Image::new(0, 0)), clipped: std::mem::take(&mut c.clipped), coverage: None })
        .collect();
    let key: Vec<f64> = match &exif {
        Some(e) => e.clone(),
        None => shots.par_iter().map(exposure::brightness).collect(),
    };
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| key[a].total_cmp(&key[b]));
    let rpos = (n - 1) / 2;
    let reference = order[rpos];
    timings.exposure = t.elapsed();

    // Align each shot to its neighbour towards the reference, composing.
    let t = Instant::now();
    let mut half_t = vec![Transform::IDENTITY; n];
    let mut status = vec![if opts.align { AlignStatus::Failed } else { AlignStatus::Disabled }; n];
    status[reference] = AlignStatus::Reference;
    if opts.align {
        ctl.check()?;
        let planes: Vec<Plane> = shots.par_iter().map(align_plane).collect();
        // Link k: order[k] relative to its neighbour towards the reference.
        let neighbour = |k: usize| if k > rpos { k - 1 } else { k + 1 };
        let links: Vec<Option<align::Alignment>> = (0..n)
            .into_par_iter()
            .map(|k| (k != rpos).then(|| align_pair(&planes[order[neighbour(k)]], &planes[order[k]])).flatten())
            .collect();
        let mut done = vec![false; n];
        done[rpos] = true;
        let steps: Vec<usize> = (rpos + 1..n).chain((0..rpos).rev()).collect();
        for (s, &k) in steps.iter().enumerate() {
            let i = order[k];
            let prev = neighbour(k);
            let chained = links[k].as_ref().filter(|a| a.converged && done[prev]);
            let result = match chained {
                Some(a) => Some((a.transform.compose(&half_t[order[prev]]), a.correlation, a.model)),
                None => align_pair(&planes[reference], &planes[i]).map(|a| (a.transform, a.correlation, a.model)),
            };
            match result {
                Some((tr, correlation, model)) => {
                    half_t[i] = tr;
                    status[i] = AlignStatus::Aligned { correlation, model };
                    done[k] = true;
                }
                None => warnings.push(Warning::AlignmentFailed(names[i].clone())),
            }
            ctl.report(Stage::Align, (s + 1) as f32 / steps.len() as f32);
        }
        for i in 0..n {
            if matches!(status[i], AlignStatus::Aligned { .. }) {
                ctl.check()?;
                shots[i] = shots[i].warped(&half_t[i]);
            }
        }
    }
    timings.align = t.elapsed();

    // Exposures from the pixels, chained from the reference.
    let t = Instant::now();
    let mut e = vec![1.0f64; n];
    let mut ev_source = vec![EvSource::Reference; n];
    let mut missing = Vec::new();
    for k in (rpos + 1..n).chain((0..rpos).rev()) {
        let (lo, hi) = if k > rpos { (order[k - 1], order[k]) } else { (order[k], order[k + 1]) };
        let (pixel, _) = exposure::pixel_ratio(&shots[lo], &shots[hi]);
        let nominal = exif.as_ref().map(|x| x[hi] / x[lo]);
        let r = match (pixel, nominal) {
            (Some(p), Some(x)) => {
                if (p / x).log2().abs() >= exposure::EXIF_TOLERANCE_EV {
                    let (fi, ex) = if k > rpos { (hi, x) } else { (lo, 1.0 / x) };
                    let base = if k > rpos { e[lo] } else { e[hi] };
                    let measured = if k > rpos { p } else { 1.0 / p };
                    warnings.push(Warning::ExifMismatch {
                        frame: names[fi].clone(),
                        exif_ev: (base * ex).log2(),
                        pixel_ev: (base * measured).log2(),
                    });
                }
                Some((p, EvSource::Pixels))
            }
            (Some(p), None) => Some((p, EvSource::Pixels)),
            (None, Some(x)) => Some((x, EvSource::Exif)),
            (None, None) => None,
        };
        let Some((r, src)) = r else {
            missing.push(names[order[k]].clone());
            continue;
        };
        if k > rpos {
            e[hi] = e[lo] * r;
            ev_source[hi] = src;
        } else {
            e[lo] = e[hi] / r;
            ev_source[lo] = src;
        }
    }
    if !missing.is_empty() {
        return Err(MergeError::NoExposure(missing));
    }
    if order.windows(2).all(|p| (e[p[1]] / e[p[0]]).log2().abs() < exposure::SAME_EXPOSURE_EV) {
        warnings.push(Warning::SameExposure);
    }
    ctl.report(Stage::Exposure, 1.0);
    timings.exposure += t.elapsed();

    // Shot plans for both passes.
    let meta = bracket::meta(&halves[reference]);
    let neutral = meta.as_shot_neutral.map(|v| v as f32);
    let darkest = (0..n).min_by(|&a, &b| e[a].total_cmp(&e[b])).expect("shots");
    let to_full = sources[reference].half_to_full(&halves[reference]);
    let from_full = to_full.inverse().expect("invertible scale");
    let plans: Vec<FramePlan> = (0..n)
        .map(|i| FramePlan {
            source: sources[i].clone(),
            weight: FrameWeight {
                exposure: e[i] as f32,
                floor: if i == darkest {
                    DARKEST_FLOOR
                } else if i == reference {
                    REFERENCE_FLOOR
                } else {
                    0.0
                },
                neutral,
            },
            transform: matches!(status[i], AlignStatus::Aligned { .. }).then(|| to_full.compose(&half_t[i]).compose(&from_full)),
            ghost: None,
        })
        .collect();

    let crop = if opts.auto_crop { auto_crop(&shots) } else { None };
    let baseline_noise = baseline_noise(&shots, &plans);

    let exif_ev = |i: usize| exif.as_ref().map(|x| (x[i] / x[reference]).log2());
    let frames = (0..n)
        .map(|i| FrameInfo {
            name: names[i].clone(),
            ev: e[i].log2(),
            exif_ev: exif_ev(i),
            ev_source: ev_source[i],
            alignment: status[i].clone(),
            transform: plans[i].transform.unwrap_or(Transform::IDENTITY),
        })
        .collect();
    Ok(Prepared { frames, reference, warnings, crop, baseline_noise, shots, timings, plans, meta })
}

/// A homography must reduce the residual `1 − ρ` of a similarity by this
/// factor to be used.
const HOMOGRAPHY_GAIN: f32 = 0.8;

/// Aligns `moving` to `reference` with a similarity, or a homography when it
/// fits clearly better. Shots two or more EV apart share only a band of
/// valid pixels, where the perspective terms of a homography are poorly
/// constrained and extrapolate badly to the corners; a hand-held bracket
/// rarely needs them. `None` when neither converges.
fn align_pair(reference: &Plane, moving: &Plane) -> Option<align::Alignment> {
    let run = |model| align::align(reference, moving, &AlignParams { model, ..AlignParams::default() }).ok().filter(|a| a.converged);
    let (s, h) = rayon::join(|| run(Model::Similarity), || run(Model::Homography));
    match (s, h) {
        (Some(s), Some(h)) if h.model == Model::Homography && (1.0 - h.correlation) < HOMOGRAPHY_GAIN * (1.0 - s.correlation) => Some(h),
        (Some(s), _) => Some(s),
        (None, h) => h,
    }
}

/// Log luminance for alignment, weighted 0 where clipped (h < 0.5) or near
/// the noise floor.
fn align_plane(s: &Shot) -> Plane {
    let (w, h) = (s.data.width, s.data.height);
    let px = &s.data.pixels;
    let data = px.par_iter().map(|p| ((p[0] + p[1] + p[2]) / 3.0).max(1e-4).log2()).collect();
    let weight = px
        .par_iter()
        .zip(&s.clipped)
        .map(|(p, &c)| {
            let y = (p[0] + p[1] + p[2]) / 3.0;
            f32::from(weights::highlight_weight(weights::max3(*p), c != 0) >= 0.5 && y >= 0.01)
        })
        .collect();
    Plane { width: w, height: h, data, weight: Some(weight) }
}

fn auto_crop(shots: &[Shot]) -> Option<CropRect> {
    if shots.iter().all(|s| s.coverage.is_none()) {
        return None;
    }
    let (w, h) = (shots[0].data.width, shots[0].data.height);
    let all: Vec<bool> = (0..w * h).into_par_iter().map(|i| shots.iter().all(|s| s.covers(i))).collect();
    // One half-size pixel of margin: a half-size pixel covers two full ones.
    let covered: Vec<bool> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % w, i / w);
            (y.saturating_sub(1)..(y + 2).min(h)).all(|yy| (x.saturating_sub(1)..(x + 2).min(w)).all(|xx| all[yy * w + xx]))
        })
        .collect();
    crop::auto_crop(&covered, w, h)
}

/// Median over the pixels of `min(1, 1/√Σ eᵢ·hᵢ)`: with photon noise, the
/// noise of the merge relative to the reference alone.
fn baseline_noise(shots: &[Shot], plans: &[FramePlan]) -> f64 {
    let n = shots[0].data.pixels.len();
    let mut v: Vec<f32> = (0..n)
        .into_par_iter()
        .step_by(4)
        .map(|i| {
            let s: f32 = shots
                .iter()
                .zip(plans)
                .filter(|(s, _)| s.covers(i))
                .map(|(s, p)| p.weight.exposure * weights::highlight_weight(weights::max3(s.data.pixels[i]), s.clipped[i] != 0))
                .sum();
            if s > 0.0 {
                (1.0 / s.sqrt()).min(1.0)
            } else {
                1.0
            }
        })
        .collect();
    let mid = v.len() / 2;
    let (_, m, _) = v.select_nth_unstable_by(mid, f32::total_cmp);
    *m as f64
}

/// A finished merge: the radiance in units of the reference exposure, in
/// sensor orientation.
pub struct Merged {
    pub image: Image,
    pub frames: Vec<FrameInfo>,
    pub warnings: Vec<Warning>,
    pub crop: Option<CropRect>,
    pub baseline_noise: f64,
    pub timings: Timings,
    meta: Capture,
}

impl Prepared {
    /// The merge at half size, from the shots already in memory.
    pub fn preview(&self) -> Image {
        hdr::merge_shots(&self.shots, &self.plans, false)
    }

    /// Pass 2: the full-resolution merge. The half-size shots are dropped first.
    pub fn merge(self, ctl: &Control) -> Result<Merged, MergeError> {
        let Prepared { frames, reference, warnings, crop, baseline_noise, shots, mut timings, plans, meta } = self;
        drop(shots);
        let image = hdr::merge_full(&plans, reference, ctl, &mut timings)?;
        Ok(Merged { image, frames, warnings, crop, baseline_noise, timings, meta })
    }
}

/// What [`write_dng`] wrote.
#[derive(Clone, Debug)]
pub struct Written {
    pub path: PathBuf,
    pub width: usize,
    pub height: usize,
    /// The output scale `s` (written data = X · s).
    pub scale: f32,
    pub baseline_exposure: f64,
    pub bytes: u64,
}

impl Merged {
    /// Scales the radiance, renders the preview and writes the DNG (through
    /// a temporary file, renamed when complete).
    pub fn write_dng(&mut self, path: &Path, opts: &MergeOptions, ctl: &Control) -> Result<Written, MergeError> {
        ctl.check()?;
        let t = Instant::now();
        let s = output::output_scale(&self.image);
        let data = output::scaled(std::mem::replace(&mut self.image, Image::new(0, 0)), s);
        let baseline_exposure = self.meta.info.baseline_exposure + (1.0 / s as f64).log2();
        let preview = output::render_preview(&data, &self.meta, baseline_exposure, self.crop, &opts.adobe);
        let xmp = output::xmp_packet(self.crop);
        self.timings.preview = t.elapsed();
        ctl.report(Stage::Write, 0.2);
        ctl.check()?;

        let t = Instant::now();
        let dng = LinearDng {
            image: &data,
            baseline_exposure,
            baseline_noise: self.baseline_noise,
            xmp: Some(xmp.as_bytes()),
            preview: Some(&preview),
            ..LinearDng::from_capture(&self.meta)
        };
        raw::write_linear_dng(path, &dng, opts.encoding).map_err(|e| MergeError::Write(e.to_string()))?;
        self.timings.write = t.elapsed();
        ctl.report(Stage::Write, 1.0);
        let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let written = Written { path: path.to_path_buf(), width: data.width, height: data.height, scale: s, baseline_exposure, bytes };
        // Keep the merged data available, in reference units again.
        self.image = data;
        self.image.map(|p| p.map(|c| c / s));
        Ok(written)
    }
}

/// The whole merge: [`prepare`], [`Prepared::merge`], [`Merged::write_dng`].
/// Without `out`, the DNG goes next to the first shot as
/// `<name>-HDR.dng` (`-HDR-2.dng`… if taken).
pub fn merge_hdr(sources: Vec<Source>, out: Option<&Path>, opts: &MergeOptions, ctl: &Control) -> Result<(Merged, Written), MergeError> {
    let first = sources.first().and_then(|s| s.path().map(Path::to_path_buf));
    let prepared = prepare(sources, opts, ctl)?;
    let mut merged = prepared.merge(ctl)?;
    let path = match (out, first) {
        (Some(p), _) => p.to_path_buf(),
        (None, Some(f)) => default_output_path(&f),
        (None, None) => return Err(MergeError::Write("no output path".into())),
    };
    let written = merged.write_dng(&path, opts, ctl)?;
    Ok((merged, written))
}
