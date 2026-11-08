//! The shots of a bracket: where they come from, loading, and the checks
//! that they can be merged (same camera, sensor, crop, CFA and orientation).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use align::Transform;
use raw::{Capture, DecodeOptions, Image};
use rayon::prelude::*;

use crate::{MergeError, Warning};

/// One shot of the bracket.
#[derive(Clone, Debug)]
pub enum Source {
    /// A RAW file, decoded twice: at half size for the analysis, then at
    /// full size for the merge.
    File(PathBuf),
    /// An already decoded full-size capture (tests, or a caller that keeps
    /// them in memory). The half-size copy is a 2×2 binning of it.
    Memory { name: String, capture: Arc<Capture> },
}

impl Source {
    /// File name (or the given name), for messages.
    pub fn name(&self) -> String {
        match self {
            Source::File(p) => p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned()),
            Source::Memory { name, .. } => name.clone(),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            Source::File(p) => Some(p),
            Source::Memory { .. } => None,
        }
    }

    pub(crate) fn load(&self, half: bool) -> Result<Capture, MergeError> {
        match self {
            Source::File(path) => raw::load_capture(path, &DecodeOptions { half_size: half }).map_err(|e| match e {
                raw::Error::Unsupported(_) if raw::is_rendered_image(path) => MergeError::NotRaw(vec![self.name()]),
                e => MergeError::Decode { frame: self.name(), message: e.to_string() },
            }),
            Source::Memory { capture, .. } if half => Ok(bin2(capture)),
            Source::Memory { capture, .. } => Ok(Capture::clone(capture)),
        }
    }

    /// The map from half-size pixel coordinates to full-size ones (pixel
    /// centres at +0.5): `full = s · half + o`, as a transform.
    pub(crate) fn half_to_full(&self, half: &Capture) -> Transform {
        let Some(r) = &half.info.raw else { return Transform::scaling(2.0, 2.0) };
        if matches!(self, Source::Memory { .. }) {
            return Transform::scaling(2.0, 2.0);
        }
        let [cx, cy, cw, ch] = r.crop;
        if is_mosaic(&r.cfa) {
            // Half-size pixels are whole 2×2 cells starting at the first even
            // photosite of the crop: one photosite late when the crop is odd.
            let ox = (2 * cx.div_ceil(2) - cx) as f64;
            let oy = (2 * cy.div_ceil(2) - cy) as f64;
            let mut t = Transform::scaling(2.0, 2.0);
            t.0[0][2] = ox;
            t.0[1][2] = oy;
            t
        } else {
            // LinearRaw: an area downscale, edges onto edges.
            Transform::scaling(cw as f64 / half.data.width as f64, ch as f64 / half.data.height as f64)
        }
    }
}

fn is_mosaic(cfa: &str) -> bool {
    cfa != "linear RGB" && cfa != "monochrome"
}

/// 2×2 binning of a full-size capture (the mask is the OR of the cell).
fn bin2(c: &Capture) -> Capture {
    let (w, h) = (c.data.width / 2, c.data.height / 2);
    let fw = c.data.width;
    let data = Image::from_fn(w, h, |x, y| {
        let i = 2 * y * fw + 2 * x;
        let p = &c.data.pixels;
        std::array::from_fn(|k| 0.25 * (p[i][k] + p[i + 1][k] + p[i + fw][k] + p[i + fw + 1][k]))
    });
    let clipped = if c.clipped.is_empty() {
        vec![0; w * h]
    } else {
        (0..w * h)
            .into_par_iter()
            .map(|j| {
                let i = 2 * (j / w) * fw + 2 * (j % w);
                c.clipped[i] | c.clipped[i + 1] | c.clipped[i + fw] | c.clipped[i + fw + 1]
            })
            .collect()
    };
    Capture { data, clipped, ..meta(c) }
}

/// A capture without its pixels: the metadata a merged DNG copies.
pub(crate) fn meta(c: &Capture) -> Capture {
    Capture {
        info: c.info.clone(),
        data: Image::new(0, 0),
        clipped: Vec::new(),
        profile: c.profile.clone(),
        as_shot_neutral: c.as_shot_neutral,
        camera: c.camera.clone(),
        metadata: c.metadata.clone(),
    }
}

/// Files that are obviously not RAW, before decoding anything.
pub(crate) fn rendered_files(sources: &[Source]) -> Vec<String> {
    sources.iter().filter(|s| s.path().is_some_and(raw::is_rendered_image)).map(Source::name).collect()
}

/// Checks that the shots can be merged; returns the warnings.
pub(crate) fn validate(names: &[String], shots: &[Capture]) -> Result<Vec<Warning>, MergeError> {
    let floats: Vec<String> = names
        .iter()
        .zip(shots)
        .filter(|(_, c)| c.info.raw.as_ref().is_none_or(|r| r.float))
        .map(|(n, _)| n.clone())
        .collect();
    if !floats.is_empty() {
        return Err(MergeError::NotRaw(floats));
    }
    let first = &shots[0];
    let differ = |f: &dyn Fn(&Capture) -> String| -> Option<Vec<String>> {
        let v0 = f(first);
        shots
            .iter()
            .any(|c| f(c) != v0)
            .then(|| names.iter().zip(shots).map(|(n, c)| format!("{n}: {}", f(c))).collect())
    };
    if let Some(list) = differ(&|c| format!("{} {}", c.camera.make, c.camera.model)) {
        return Err(MergeError::DifferentCameras(list));
    }
    if let Some(list) = differ(&|c| {
        let r = c.info.raw.as_ref().expect("checked");
        format!("sensor {}×{}, crop {:?}, {}", r.sensor_size.0, r.sensor_size.1, r.crop, r.cfa)
    }) {
        return Err(MergeError::DifferentSensors(list));
    }
    if let Some(list) = differ(&|c| format!("{:?}", c.info.orientation)) {
        return Err(MergeError::DifferentOrientations(list));
    }

    let mut warnings = Vec::new();
    let focals: Vec<f64> = shots.iter().filter_map(|c| c.info.focal_length).filter(|f| *f > 0.0).collect();
    if focals.len() == shots.len() {
        let (lo, hi) = min_max(&focals);
        if hi > lo * 1.02 {
            warnings.push(Warning::FocalLength { min: lo, max: hi });
        }
    }
    let isos: Vec<f64> = shots.iter().filter_map(|c| c.info.iso).filter(|i| *i > 0).map(f64::from).collect();
    if isos.len() == shots.len() {
        let (lo, hi) = min_max(&isos);
        if (hi / lo).log2() > 2.0 {
            warnings.push(Warning::IsoSpread { min: lo as u32, max: hi as u32 });
        }
    }
    Ok(warnings)
}

fn min_max(v: &[f64]) -> (f64, f64) {
    v.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &x| (a.min(x), b.max(x)))
}
