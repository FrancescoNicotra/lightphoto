//! Calibration bench: develops each entry of a manifest of Lightroom
//! references with our pipeline and measures how far it lands from
//! Lightroom's own export (mean ΔE2000 at 480 px, as `compare --size 480`
//! and the `matches_lightroom_on_the_camera_jpeg` test).
//!
//! usage: calibrate [--manifest references.toml] [--samples DIR] [--only TEXT] [--bands]
//!
//! - `--manifest`: defaults to `tools/calibrate/references.toml`.
//! - `--samples`: root the manifest's paths are relative to. Defaults to
//!   `$LUMEN_SAMPLES`, then the nearest `samples/` folder with a `pixls/`
//!   in it, from the workspace upwards (a git worktree has none of its own,
//!   since git ignores `samples/pixls`).
//! - `--only TEXT`: only the entries whose name contains TEXT.
//! - `--bands`: also print, for each entry, the error by lightness band.
//!
//! Entries whose source or reference file is missing are skipped with a
//! warning; the exit code is an error when a measured entry is above its
//! threshold, or when an entry cannot be developed at all.
//!
//! The manifest format and the exports to make are in `docs/calibration.md`.

mod manifest;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use pipeline::{Pipeline, RenderOptions, SourceKind};
use raw::color::{self, delta_e2000, to_lab, D65};
use raw::{DecodeOptions, Image, Photo};
use rayon::prelude::*;

use manifest::{base_settings, Entry, Manifest};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("usage: calibrate [--manifest references.toml] [--samples DIR] [--only TEXT] [--bands]");
        return ExitCode::SUCCESS;
    }
    let manifest_path =
        value("--manifest").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("references.toml"));
    let samples = samples_root(value("--samples").map(PathBuf::from));
    let only = value("--only");
    let bands = args.iter().any(|a| a == "--bands");

    let manifest = match Manifest::load(&manifest_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {}: {e}", manifest_path.display());
            return ExitCode::from(2);
        }
    };
    println!("manifest {}", manifest_path.display());
    println!("samples  {}", samples.display());
    println!("compared at {} px on the long edge\n", manifest.size);

    let entries: Vec<&Entry> =
        manifest.references.iter().filter(|e| only.as_deref().is_none_or(|t| e.name.contains(t))).collect();
    let mut bench = Bench { samples: samples.clone(), size: manifest.size, photos: HashMap::new(), neutral: HashMap::new() };
    let mut outcomes = Vec::new();
    println!(
        "{:<26} {:>6} {:>6} {:>6} {:>7} {:>6} {:>6} {:>6} {:>6}  result",
        "entry", "ΔE", "median", "p95", "neutral", "ΔL", "Δa", "Δb", "max"
    );
    for entry in entries {
        let max = entry.max_delta_e.unwrap_or(manifest.max_delta_e);
        let outcome = match bench.measure(entry) {
            Ok(Some(m)) => {
                let pass = m.mean <= max;
                println!(
                    "{:<26} {:>6.2} {:>6.2} {:>6.2} {:>7.2} {:>+6.1} {:>+6.1} {:>+6.1} {:>6.2}  {}",
                    entry.name,
                    m.mean,
                    m.median,
                    m.p95,
                    m.neutral,
                    m.mean_diff[0],
                    m.mean_diff[1],
                    m.mean_diff[2],
                    max,
                    if pass { "ok" } else { "FAIL" }
                );
                for note in &m.notes {
                    println!("    note: {note}");
                }
                if bands {
                    for (label, share, de, dl) in &m.bands {
                        println!("    {label:<12} {:>5.1}%  ΔE {de:>5.2}  ΔL {dl:>+5.1}", 100.0 * share);
                    }
                }
                if pass {
                    Outcome::Pass(m.mean)
                } else {
                    Outcome::Fail(m.mean)
                }
            }
            Ok(None) => Outcome::Missing,
            Err(e) => {
                println!("{:<26} ERROR: {e}", entry.name);
                Outcome::Error
            }
        };
        outcomes.push((entry, outcome));
    }

    summarise(&outcomes, &samples)
}

enum Outcome {
    Pass(f64),
    Fail(f64),
    Missing,
    Error,
}

fn summarise(outcomes: &[(&Entry, Outcome)], samples: &Path) -> ExitCode {
    let measured: Vec<(&str, f64)> = outcomes
        .iter()
        .filter_map(|(e, o)| match o {
            Outcome::Pass(d) | Outcome::Fail(d) => Some((e.name.as_str(), *d)),
            _ => None,
        })
        .collect();
    let count = |f: fn(&Outcome) -> bool| outcomes.iter().filter(|(_, o)| f(o)).count();
    let failed = count(|o| matches!(o, Outcome::Fail(_)));
    let errors = count(|o| matches!(o, Outcome::Error));
    let missing: Vec<&Entry> = outcomes.iter().filter(|(_, o)| matches!(o, Outcome::Missing)).map(|(e, _)| *e).collect();

    println!();
    print!("{} measured, {} passed, {failed} above threshold", measured.len(), measured.len() - failed);
    if errors > 0 {
        print!(", {errors} errors");
    }
    println!(", {} missing", missing.len());
    if !measured.is_empty() {
        let mean = measured.iter().map(|(_, d)| d).sum::<f64>() / measured.len() as f64;
        let (worst, de) = measured.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        println!("mean ΔE2000 over the measured entries {mean:.2}; worst {worst} ({de:.2})");
    }
    if !missing.is_empty() {
        println!("\nwarning: {} entries skipped, files not found under {}:", missing.len(), samples.display());
        for e in &missing {
            for p in e.inputs().filter(|p| !samples.join(p).exists()) {
                println!("  {:<26} {}", e.name, p.display());
            }
        }
        println!("see docs/calibration.md for the Lightroom exports to make");
    }
    if failed > 0 || errors > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Where the manifest's paths start from.
fn samples_root(arg: Option<PathBuf>) -> PathBuf {
    if let Some(p) = arg.or_else(|| std::env::var_os("LUMEN_SAMPLES").map(PathBuf::from)) {
        return p;
    }
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workspace = workspace.canonicalize().unwrap_or(workspace);
    workspace
        .ancestors()
        .map(|a| a.join("samples"))
        .find(|s| s.join("pixls").is_dir())
        .unwrap_or_else(|| workspace.join("samples"))
}

struct Measure {
    mean: f64,
    median: f64,
    p95: f64,
    /// Mean ΔE of the undeveloped source against the reference: how much
    /// Lightroom changed the picture. `mean` well below it means we follow.
    neutral: f64,
    /// Mean of ours − reference in Lab (ΔL, Δa, Δb).
    mean_diff: [f64; 3],
    /// (label, share of pixels, mean ΔE, mean ΔL) by lightness of the reference.
    bands: Vec<(String, f64, f64, f64)>,
    /// What the pipeline says it does not reproduce.
    notes: Vec<String>,
}

struct Bench {
    samples: PathBuf,
    size: usize,
    /// Decoded sources, by path.
    photos: HashMap<PathBuf, Photo>,
    /// Sources developed with the defaults only, at the comparison size.
    neutral: HashMap<PathBuf, Image>,
}

type Error = Box<dyn std::error::Error>;

impl Bench {
    /// `None` when an input file is missing.
    fn measure(&mut self, entry: &Entry) -> Result<Option<Measure>, Error> {
        if entry.inputs().any(|p| !self.samples.join(p).exists()) {
            return Ok(None);
        }
        let source = self.samples.join(&entry.source);
        if !self.photos.contains_key(&source) {
            let photo = Photo::load(&source, &DecodeOptions::default())?;
            self.photos.insert(source.clone(), photo);
        }
        let photo = &self.photos[&source];
        let kind = SourceKind::of(photo);
        if kind != entry.kind.source_kind() {
            return Err(format!("{} is {kind:?}, the manifest says {:?}", entry.source.display(), entry.kind).into());
        }

        let preset = entry.preset.as_ref().map(|p| xmp::Preset::load(self.samples.join(p))).transpose()?;
        let settings = entry.settings(kind, preset.as_ref())?;
        let pipeline = Pipeline::new(&settings, kind);
        let ours = pipeline.render(photo, &RenderOptions::default()).downscale(self.size);
        if !self.neutral.contains_key(&source) {
            let plain = Pipeline::new(&base_settings(kind), kind).render(photo, &RenderOptions::default()).downscale(self.size);
            self.neutral.insert(source.clone(), plain);
        }
        let neutral = &self.neutral[&source];
        let reference = Photo::load(self.samples.join(&entry.reference), &DecodeOptions::default())?.to_working(None).downscale(self.size);

        let (ours_lab, ref_lab) = lab_pair(&ours, &reference, &entry.name);
        let (neutral_lab, _) = lab_pair(neutral, &reference, &entry.name);
        Ok(Some(Measure { notes: pipeline.warnings.clone(), ..measure(&ours_lab, &ref_lab, &neutral_lab) }))
    }
}

/// Two linear ProPhoto images as Lab, at the smaller of their two sizes.
/// Colours are clipped to sRGB, as an exported sRGB file would be.
fn lab_pair(a: &Image, b: &Image, name: &str) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
    let (a, b) = if (a.width, a.height) == (b.width, b.height) {
        (a.clone(), b.clone())
    } else {
        let aspect = |i: &Image| i.width as f64 / i.height as f64;
        if (aspect(a) / aspect(b) - 1.0).abs() > 0.01 {
            eprintln!("warning: {name}: aspect ratios differ ({:.3} vs {:.3}); stretched to match", aspect(a), aspect(b));
        }
        let (w, h) = if a.width * a.height <= b.width * b.height { (a.width, a.height) } else { (b.width, b.height) };
        (export::resize::resize(a, w, h), export::resize::resize(b, w, h))
    };
    let to_srgb = color::to_f32(&color::prophoto_to_srgb());
    let to_xyz = color::to_f32(&color::rgb_to_xyz(color::SRGB_PRIMARIES, D65));
    let lab = |p: &[f32; 3]| to_lab(color::apply(&to_xyz, color::apply(&to_srgb, *p).map(|v| v.clamp(0.0, 1.0))));
    (a.pixels.par_iter().map(lab).collect(), b.pixels.par_iter().map(lab).collect())
}

fn mean_delta_e(a: &[[f32; 3]], b: &[[f32; 3]]) -> f64 {
    a.par_iter().zip(b).map(|(p, q)| delta_e2000(*p, *q) as f64).sum::<f64>() / a.len() as f64
}

fn measure(ours: &[[f32; 3]], reference: &[[f32; 3]], neutral: &[[f32; 3]]) -> Measure {
    let de: Vec<f32> = ours.par_iter().zip(reference).map(|(p, q)| delta_e2000(*p, *q)).collect();
    let mut sorted = de.clone();
    sorted.sort_by(f32::total_cmp);
    let pct = |p: f64| sorted[((sorted.len() - 1) as f64 * p) as usize] as f64;
    let n = de.len() as f64;
    let mean_diff = std::array::from_fn(|c| ours.iter().zip(reference).map(|(p, q)| (p[c] - q[c]) as f64).sum::<f64>() / n);

    let bands = (0..100)
        .step_by(20)
        .filter_map(|lo| {
            let idx: Vec<usize> = (0..reference.len()).filter(|&i| reference[i][0] >= lo as f32 && reference[i][0] < (lo + 20) as f32).collect();
            let k = idx.len() as f64;
            (k > 0.0).then(|| {
                (
                    format!("L* {lo}–{}", lo + 20),
                    k / n,
                    idx.iter().map(|&i| de[i] as f64).sum::<f64>() / k,
                    idx.iter().map(|&i| (ours[i][0] - reference[i][0]) as f64).sum::<f64>() / k,
                )
            })
        })
        .collect();

    Measure {
        mean: de.iter().map(|v| *v as f64).sum::<f64>() / n,
        median: pct(0.5),
        p95: pct(0.95),
        neutral: mean_delta_e(neutral, reference),
        mean_diff,
        bands,
        notes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_images_measure_zero() {
        let img = Image::from_fn(40, 30, |x, y| [x as f32 / 40.0, y as f32 / 30.0, 0.3]);
        let (a, b) = lab_pair(&img, &img, "same");
        let m = measure(&a, &b, &b);
        assert!(m.mean < 1e-6 && m.p95 < 1e-6 && m.neutral < 1e-6, "{}", m.mean);
        assert!(m.mean_diff.iter().all(|d| d.abs() < 1e-6));
        assert!((m.bands.iter().map(|b| b.1).sum::<f64>() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_brighter_render_shows_as_positive_lightness() {
        let reference = Image::from_fn(20, 20, |_, _| [0.18, 0.18, 0.18]);
        let ours = Image::from_fn(20, 20, |_, _| [0.25, 0.25, 0.25]);
        let (a, b) = lab_pair(&ours, &reference, "brighter");
        let m = measure(&a, &b, &b);
        assert!(m.mean > 3.0 && m.mean_diff[0] > 3.0 && m.mean_diff[1].abs() < 0.1, "{} {:?}", m.mean, m.mean_diff);
        assert!(m.neutral < 1e-6);
    }

    #[test]
    fn different_sizes_are_compared_at_the_smaller() {
        let big = Image::from_fn(48, 36, |_, _| [0.5, 0.4, 0.3]);
        let small = Image::from_fn(24, 18, |_, _| [0.5, 0.4, 0.3]);
        let (a, b) = lab_pair(&big, &small, "sizes");
        assert_eq!((a.len(), b.len()), (24 * 18, 24 * 18));
        assert!(mean_delta_e(&a, &b) < 1e-3);
    }
}
