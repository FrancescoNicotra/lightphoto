//! The "Dreamy days in spring" preset on the GX80 RW2 sample (skipped when
//! samples/pixls/P1010041.RW2 is missing; see crates/raw/tests/samples.rs).

use std::path::PathBuf;

use pipeline::{Pipeline, RenderOptions, SourceKind};
use raw::{DecodeOptions, Image, Photo};
use xmp::{DevelopSettings, Preset};

fn sample(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples").join(name);
    p.exists().then_some(p).or_else(|| {
        eprintln!("skipping: {name} not found in samples/");
        None
    })
}

/// Mean and standard deviation of luma, and mean blue − red, on sRGB values.
fn stats(img: &Image) -> (f64, f64, f64) {
    let m = raw::color::to_f32(&raw::color::prophoto_to_srgb());
    let px: Vec<[f32; 3]> = img.pixels.iter().map(|p| raw::color::apply(&m, *p).map(|v| raw::color::srgb_oetf(v.clamp(0.0, 1.0)))).collect();
    let n = px.len() as f64;
    let luma: Vec<f64> = px.iter().map(|p| 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64).collect();
    let mean = luma.iter().sum::<f64>() / n;
    let sd = (luma.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / n).sqrt();
    let cool = px.iter().map(|p| (p[2] - p[0]) as f64).sum::<f64>() / n;
    (mean, sd, cool)
}

#[test]
fn dreamy_preset_is_softer_and_warmer() {
    let Some(path) = sample("pixls/P1010041.RW2") else { return };
    let preset = Preset::load(sample("Dreamy days in spring.xmp").unwrap()).unwrap();
    let photo = Photo::load(&path, &DecodeOptions { half_size: true }).unwrap();
    let opts = RenderOptions { max_size: Some(800) };

    let plain = Pipeline::new(&DevelopSettings::default(), SourceKind::Raw).render(&photo, &opts);
    let dreamy_pipeline = Pipeline::new(&preset.settings, SourceKind::Raw);
    let dreamy = dreamy_pipeline.render(&photo, &opts);
    assert_eq!((dreamy.width, dreamy.height), (800, 601));
    assert!(dreamy.pixels.iter().flatten().all(|v| (0.0..=1.0).contains(v)));

    let (l0, sd0, cool0) = stats(&plain);
    let (l1, sd1, cool1) = stats(&dreamy);
    eprintln!("plain L {l0:.3} sd {sd0:.3} b-r {cool0:+.3}; dreamy L {l1:.3} sd {sd1:.3} b-r {cool1:+.3}");
    // Contrast −50 and Shadows +50: flatter and brighter.
    assert!(sd1 < sd0 * 0.9, "contrast {sd0} -> {sd1}");
    assert!(l1 > l0, "brightness {l0} -> {l1}");
    // 5600 K custom white balance vs ~4900 K as shot, plus a yellow midtone grade.
    assert!(cool1 < cool0 - 0.02, "warmth {cool0} -> {cool1}");
    assert!(!dreamy_pipeline.warnings.is_empty());
}

#[test]
fn preview_and_full_size_agree() {
    let Some(path) = sample("pixls/P1010041.RW2") else { return };
    let preset = Preset::load(sample("Dreamy days in spring.xmp").unwrap()).unwrap();
    let photo = Photo::load(&path, &DecodeOptions { half_size: true }).unwrap();
    let p = Pipeline::new(&preset.settings, SourceKind::Raw);
    let small = p.render(&photo, &RenderOptions { max_size: Some(400) });
    let full = p.render(&photo, &RenderOptions::default()).downscale(400);
    let (a, b) = (stats(&small), stats(&full));
    assert!((a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01 && (a.2 - b.2).abs() < 0.01, "{a:?} vs {b:?}");
}
