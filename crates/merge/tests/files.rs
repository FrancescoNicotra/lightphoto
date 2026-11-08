//! The whole chain on files: Bayer DNGs with EXIF → merge → floating point
//! DNG → read back through rawler. Plus the error cases of plan §7.4.

mod common;

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use align::Transform;
use common::scene::{Scene, ShotSpec, NEUTRAL};
use common::{block_log_error, memory_sources, truth, write_dng, COLOR_MATRIX};
use merge::{merge_hdr, AlignStatus, Control, EvSource, MergeError, MergeOptions, Source};
use raw::{DecodeOptions, Orientation, Photo};

const W: usize = 800;
const H: usize = 540;

fn dir(name: &str) -> PathBuf {
    let d = common::tmp(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn bracket_files(dir: &std::path::Path, scene: &Scene) -> (Vec<Source>, Vec<ShotSpec>) {
    let (cx, cy) = (W as f64 / 2.0, H as f64 / 2.0);
    let specs = vec![
        ShotSpec { exif_time: Some(1.0 / 400.0), transform: Transform::translation(-2.7, 4.6), ..ShotSpec::new(0.25, 1) },
        ShotSpec { exif_time: Some(1.0 / 100.0), ..ShotSpec::new(1.0, 2) },
        ShotSpec {
            exif_time: Some(1.0 / 25.0),
            transform: Transform::similarity(1.0, -0.3f64.to_radians(), cx, cy, 3.1, 1.2),
            ..ShotSpec::new(4.0, 3)
        },
    ];
    let sources = specs
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let p = dir.join(format!("IMG_{}.dng", 100 + i));
            write_dng(&p, scene, s, W, H);
            Source::File(p)
        })
        .collect();
    (sources, specs)
}

#[test]
fn dng_bracket_end_to_end() {
    let d = dir("e2e");
    let scene = Scene::new(21, W, H);
    let (sources, specs) = bracket_files(&d, &scene);
    let opts = MergeOptions { auto_crop: true, ..MergeOptions::default() };
    let (merged, written) = merge_hdr(sources.clone(), None, &opts, &Control::default()).unwrap();
    assert_eq!(written.path, d.join("IMG_100-HDR.dng"));
    assert!(merged.warnings.is_empty(), "{:?}", merged.warnings);
    for (f, s) in merged.frames.iter().zip(&specs) {
        let err = f.transform.corner_rms(&s.transform, W, H);
        eprintln!("{}: {:+.4} EV (EXIF {:?}, {:?}), {:?}, corner RMS {err:.4} px", f.name, f.ev, f.exif_ev, f.ev_source, f.alignment);
        assert!((f.ev - s.exposure.log2()).abs() < 0.03, "{}", f.name);
        assert!(f.exif_ev.is_some_and(|e| (e - s.exposure.log2()).abs() < 1e-6), "EXIF not read: {:?}", f.exif_ev);
        assert!(err < 0.25, "{}: corner RMS {err}", f.name);
    }
    assert_eq!(merged.frames[1].ev_source, EvSource::Reference);
    assert!(matches!(merged.frames[2].alignment, AlignStatus::Aligned { .. }));
    eprintln!(
        "wrote {} ({} × {}, {:.2} MB), scale {:.4}, BaselineExposure {:+.3}, noise {:.3}",
        written.path.display(),
        written.width,
        written.height,
        written.bytes as f64 / 1e6,
        written.scale,
        written.baseline_exposure,
        merged.baseline_noise
    );

    // Radiance through the Bayer mosaic and the bilinear demosaic.
    let t = truth(&scene, W, H, None);
    let m = 16;
    let (err, n) = block_log_error(&merged.image, &t, 8, |x, y| {
        let r = t.get(x, y);
        x >= m && y >= m && x < W - m && y < H - m && (r[0].max(r[1]).max(r[2]) as f64) * 0.25 < 0.85
    });
    eprintln!("radiance error {err:.4} EV over {n} blocks");
    assert!(err < 0.02, "{err}");

    // BaselineExposure: log₂(1/s) above the reference's 0.25.
    assert!((written.baseline_exposure - (0.25 + (1.0 / written.scale as f64).log2())).abs() < 1e-9);

    // Round trip through rawler.
    let photo = Photo::load(&written.path, &DecodeOptions::default()).unwrap();
    let r = photo.info.raw.as_ref().unwrap();
    assert!(r.float);
    assert_eq!((photo.data.width, photo.data.height), (W, H));
    assert_eq!(photo.info.orientation, Orientation::Normal);
    assert!((photo.info.baseline_exposure - written.baseline_exposure).abs() < 1e-5);
    assert!(r.as_shot_neutral.iter().zip(NEUTRAL).all(|(a, b)| (a - b).abs() < 1e-5), "{:?}", r.as_shot_neutral);
    let cm = &photo.camera_profile().unwrap().calibrations[0].color_matrix;
    assert!(cm.iter().flatten().zip(COLOR_MATRIX.iter().flatten()).all(|(a, b)| (a - b).abs() < 1e-5));
    let s = written.scale;
    let mut worst = 0.0f32;
    for (a, b) in photo.data.pixels.iter().zip(&merged.image.pixels) {
        for c in 0..3 {
            let expected = b[c] * s;
            if expected > 1e-3 {
                worst = worst.max((a[c] - expected).abs() / expected);
            }
        }
    }
    eprintln!("round trip: worst relative error {worst:.2e}");
    assert!(worst <= 1e-3, "{worst}");

    // Initial settings: CreatorTool and the crop.
    let bytes = std::fs::read(&written.path).unwrap();
    let packet = xmp::XmpPacket::parse(&xmp::extract_xmp(&bytes).expect("embedded XMP")).unwrap();
    assert!(packet.props.get("http://ns.adobe.com/xap/1.0/", "CreatorTool").and_then(|v| v.as_text()).is_some_and(|t| t.starts_with("Lumen")));
    let crop = merged.crop.expect("crop");
    assert_eq!(packet.props.crs("CropTop").and_then(|v| v.as_f32()), Some((crop.top * 1e6).round() / 1e6));
    assert!(raw::embedded_preview(&written.path).unwrap().is_some_and(|p| p.width() <= 1024 && p.width() > 500));

    // A second merge does not overwrite the first.
    let (_, again) = merge_hdr(sources, None, &MergeOptions { align: false, ..MergeOptions::default() }, &Control::default()).unwrap();
    assert_eq!(again.path, d.join("IMG_100-HDR-2.dng"));

    // A merged DNG is not a valid input.
    let float_input = vec![Source::File(written.path.clone()), Source::File(d.join("IMG_101.dng"))];
    assert_eq!(merge_hdr(float_input, None, &opts, &Control::default()).err(), Some(MergeError::NotRaw(vec!["IMG_100-HDR.dng".into()])));
}

#[test]
fn errors() {
    let d = dir("errors");
    let scene = Scene::new(22, 320, 240);
    let specs = [ShotSpec::new(1.0, 1), ShotSpec::new(4.0, 2)];
    let opts = MergeOptions::default();
    let ctl = Control::default();
    let run = |sources: Vec<Source>| merge_hdr(sources, Some(&d.join("out.dng")), &opts, &ctl).err();

    let one = memory_sources(&scene, &specs[..1], 320, 240);
    assert_eq!(run(one), Some(MergeError::FrameCount(1)));

    let jpeg = d.join("photo.jpg");
    image::RgbImage::new(8, 8).save(&jpeg).unwrap();
    let mut s = memory_sources(&scene, &specs, 320, 240);
    s.push(Source::File(jpeg));
    assert_eq!(run(s), Some(MergeError::NotRaw(vec!["photo.jpg".into()])));

    let other_camera = |f: &dyn Fn(&mut raw::Capture)| {
        let mut s = memory_sources(&scene, &specs, 320, 240);
        if let Source::Memory { capture, .. } = &mut s[1] {
            f(Arc::make_mut(capture));
        }
        run(s)
    };
    assert!(matches!(other_camera(&|c| c.camera.model = "Other".into()), Some(MergeError::DifferentCameras(_))));
    assert!(matches!(other_camera(&|c| c.info.raw.as_mut().unwrap().crop = [0, 0, 300, 200]), Some(MergeError::DifferentSensors(_))));
    assert!(matches!(other_camera(&|c| c.info.orientation = Orientation::Rotate90), Some(MergeError::DifferentOrientations(_))));

    // Cancelled: no file, not even a temporary one.
    let cancel = AtomicBool::new(true);
    let ctl = Control { progress: None, cancel: Some(&cancel) };
    let r = merge_hdr(memory_sources(&scene, &specs, 320, 240), Some(&d.join("cancelled.dng")), &opts, &ctl);
    assert_eq!(r.err(), Some(MergeError::Cancelled));
    let left: Vec<_> = std::fs::read_dir(&d).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
    assert!(left.iter().all(|n| !n.to_string_lossy().contains("cancelled")), "{left:?}");
}

#[test]
fn progress_is_reported() {
    let scene = Scene::new(23, 320, 240);
    let specs = [ShotSpec::new(1.0, 1), ShotSpec::new(4.0, 2)];
    let stages = std::sync::Mutex::new(Vec::new());
    let report = |s: merge::Stage, f: f32| stages.lock().unwrap().push((s, f));
    let ctl = Control { progress: Some(&report), cancel: None };
    let out = dir("progress").join("p.dng");
    merge_hdr(memory_sources(&scene, &specs, 320, 240), Some(&out), &MergeOptions::default(), &ctl).unwrap();
    let stages = stages.into_inner().unwrap();
    for s in [merge::Stage::Decode, merge::Stage::Align, merge::Stage::Exposure, merge::Stage::Merge, merge::Stage::Write] {
        assert!(stages.iter().any(|&(st, f)| st == s && f == 1.0), "{s:?} missing: {stages:?}");
    }
}
