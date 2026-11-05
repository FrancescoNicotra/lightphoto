//! The "Dreamy days in spring" preset on the GX80 sample (skipped when the
//! files in samples/pixls are missing; see crates/raw/tests/samples.rs).
//!
//! - P1010041.RW2: the RAW, from raw.pixls.us (CC0);
//! - P1010041-camera.jpg: its embedded camera JPEG (`lumen-cli preview`);
//! - P1010041-camera-lightroom-dreamy.jpg: that JPEG with the preset applied
//!   in Lightroom 12 for iOS, exported as sRGB JPEG at full size.

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
fn dreamy_preset_is_brighter_and_warmer() {
    let Some(path) = sample("pixls/P1010041.RW2") else { return };
    let preset = Preset::load(sample("Dreamy days in spring.xmp").unwrap()).unwrap();
    let photo = Photo::load(&path, &DecodeOptions { half_size: true }).unwrap();
    let opts = RenderOptions { max_size: Some(800), ..Default::default() };

    let plain = Pipeline::new(&DevelopSettings::default(), SourceKind::Raw).render(&photo, &opts);
    let dreamy_pipeline = Pipeline::new(&preset.settings, SourceKind::Raw);
    let dreamy = dreamy_pipeline.render(&photo, &opts);
    assert_eq!((dreamy.width, dreamy.height), (800, 601));
    assert!(dreamy.pixels.iter().flatten().all(|v| (0.0..=1.0).contains(v)));

    let (l0, sd0, cool0) = stats(&plain);
    let (l1, sd1, cool1) = stats(&dreamy);
    eprintln!("plain L {l0:.3} sd {sd0:.3} b-r {cool0:+.3}; dreamy L {l1:.3} sd {sd1:.3} b-r {cool1:+.3}");
    // Shadows +50 and the master curve lift the midtones.
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
    let small = p.render(&photo, &RenderOptions { max_size: Some(400), ..Default::default() });
    let full = p.render(&photo, &RenderOptions::default()).downscale(400);
    let (a, b) = (stats(&small), stats(&full));
    assert!((a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01 && (a.2 - b.2).abs() < 0.01, "{a:?} vs {b:?}");
}

/// Mean ΔE2000 between two linear ProPhoto images of the same size.
fn mean_delta_e(a: &Image, b: &Image) -> f64 {
    use raw::color;
    let to_xyz = color::to_f32(&color::mul(&color::rgb_to_xyz(color::SRGB_PRIMARIES, color::D65), &color::prophoto_to_srgb()));
    let lab = |p: [f32; 3]| color::to_lab(color::apply(&to_xyz, p).map(|v| v.max(0.0)));
    let sum: f64 = a.pixels.iter().zip(&b.pixels).map(|(p, q)| color::delta_e2000(lab(*p), lab(*q)) as f64).sum();
    sum / a.pixels.len() as f64
}

/// The whole preset against Lightroom itself. Grain is random in both, so
/// the images are compared at 480 px, where it averages out.
#[test]
fn matches_lightroom_on_the_camera_jpeg() {
    let (Some(src), Some(reference)) = (sample("pixls/P1010041-camera.jpg"), sample("pixls/P1010041-camera-lightroom-dreamy.jpg")) else {
        return;
    };
    let preset = Preset::load(sample("Dreamy days in spring.xmp").unwrap()).unwrap();
    let photo = Photo::load(&src, &DecodeOptions::default()).unwrap();
    let ours = Pipeline::new(&preset.settings, SourceKind::of(&photo)).render(&photo, &RenderOptions::default()).downscale(480);
    let lightroom = Photo::load(&reference, &DecodeOptions::default()).unwrap().to_working(None).downscale(480);
    assert_eq!((ours.width, ours.height), (lightroom.width, lightroom.height));
    let de = mean_delta_e(&ours, &lightroom);
    eprintln!("mean ΔE2000 vs Lightroom at 480 px: {de:.2}");
    assert!(de < 3.0, "{de}");
}
