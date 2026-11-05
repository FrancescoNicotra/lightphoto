//! Folders: the photos of the open folder (with their thumbnails) and the
//! preset library.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use eframe::egui;
use lut::CubeFile;
use pipeline::{Pipeline, RenderOptions, SourceKind};
use raw::{DecodeOptions, Photo};
use xmp::Preset;

/// RAW extensions rawler reads, plus the rendered formats.
const PHOTO_EXTENSIONS: &[&str] = &[
    "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "rw2", "raw", "rwl", "raf", "orf", "pef", "srw", "3fr",
    "fff", "iiq", "mos", "erf", "kdc", "dcr", "mrw", "jpg", "jpeg", "png", "tif", "tiff", "webp",
];

pub const THUMB_SIZE: u32 = 256;

fn extension(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub fn is_photo(p: &Path) -> bool {
    PHOTO_EXTENSIONS.contains(&extension(p).as_str())
}

/// The photos in a folder (not recursive), sorted by name.
pub fn scan_photos(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_file() && is_photo(p)).collect())
        .unwrap_or_default();
    v.sort_by_key(|p| p.file_name().map(|n| n.to_ascii_lowercase()));
    v
}

/// Loads thumbnails for `paths` on a background thread, in order. A newer
/// call (higher `generation`) stops an older one.
pub fn spawn_thumbnails(
    paths: Vec<PathBuf>,
    generation: u64,
    current: Arc<AtomicU64>,
    tx: Sender<(u64, usize, Option<egui::ColorImage>)>,
    ctx: egui::Context,
) {
    std::thread::Builder::new()
        .name("thumbnails".into())
        .spawn(move || {
            for (i, path) in paths.iter().enumerate() {
                if current.load(Ordering::Relaxed) != generation {
                    return;
                }
                if tx.send((generation, i, thumbnail(path))).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        })
        .expect("spawn thumbnail thread");
}

/// A small sRGB image of the photo: the preview embedded in RAW files, the
/// image itself for JPEG/PNG/TIFF, or a quick default develop.
pub fn thumbnail(path: &Path) -> Option<egui::ColorImage> {
    let img = if raw::is_rendered_image(path) {
        use image::ImageDecoder;
        let mut decoder = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_decoder().ok()?;
        let orientation = decoder.orientation().ok()?;
        let mut img = image::DynamicImage::from_decoder(decoder).ok()?;
        img.apply_orientation(orientation);
        img.thumbnail(THUMB_SIZE, THUMB_SIZE)
    } else {
        match raw::embedded_preview(path).ok().flatten() {
            Some(p) => p.thumbnail(THUMB_SIZE, THUMB_SIZE),
            None => return developed_thumbnail(path),
        }
    };
    let rgb = img.to_rgb8();
    Some(egui::ColorImage::from_rgb([rgb.width() as usize, rgb.height() as usize], rgb.as_raw()))
}

fn developed_thumbnail(path: &Path) -> Option<egui::ColorImage> {
    let photo = Photo::load(path, &DecodeOptions { half_size: true }).ok()?;
    let pipeline = Pipeline::new(&xmp::DevelopSettings::default(), SourceKind::of(&photo));
    let img = pipeline.render(&photo, &RenderOptions { max_size: Some(THUMB_SIZE as usize), ..Default::default() });
    Some(crate::worker::to_color_image(&img).0)
}

/// An entry of the preset panel.
#[derive(Clone)]
pub struct PresetEntry {
    pub name: String,
    /// Preset group, or the sub-folder it was found in.
    pub group: String,
    pub path: PathBuf,
    pub kind: PresetKind,
}

#[derive(Clone)]
pub enum PresetKind {
    /// Develop settings (.xmp, or a "mobile" .dng carrying them).
    Develop(Arc<Preset>),
    Lut(Arc<CubeFile>),
    /// The file could not be read.
    Broken(String),
}

/// Finds presets (.xmp, .dng, .cube, .3dl) under `dir`, a few levels deep.
pub fn scan_presets(dir: &Path) -> Vec<PresetEntry> {
    let mut files = Vec::new();
    collect(dir, 0, &mut files);
    let mut out: Vec<PresetEntry> = files
        .into_iter()
        .map(|path| {
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let folder = path
                .parent()
                .filter(|p| *p != dir)
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let (name, group, kind) = match extension(&path).as_str() {
                "cube" | "3dl" => match lut::load(&path) {
                    Ok(c) => (stem, folder, PresetKind::Lut(Arc::new(c))),
                    Err(e) => (stem, folder, PresetKind::Broken(e.to_string())),
                },
                _ => match Preset::load(&path) {
                    Ok(p) => {
                        let name = p.name.clone().unwrap_or(stem);
                        let group = p.group.clone().filter(|g| !g.is_empty() && !g.starts_with('$')).unwrap_or(folder);
                        (name, group, PresetKind::Develop(Arc::new(p)))
                    }
                    Err(e) => (stem, folder, PresetKind::Broken(e.to_string())),
                },
            };
            PresetEntry { name, group, path, kind }
        })
        // Photos that happen to sit in the folder are not presets, nor are
        // their sidecars (unnamed .xmp next to a photo of the same name).
        .filter(|e| !matches!(&e.kind, PresetKind::Broken(err) if err.contains("no XMP")))
        .filter(|e| !matches!(&e.kind, PresetKind::Develop(p) if p.name.is_none() && is_sidecar(&e.path)))
        .collect();
    out.sort_by_key(|e| (e.group.to_lowercase(), e.name.to_lowercase()));
    out
}

fn is_sidecar(xmp: &Path) -> bool {
    extension(xmp) == "xmp"
        && std::fs::read_dir(xmp.parent().unwrap_or(Path::new("."))).is_ok_and(|rd| {
            rd.flatten().map(|e| e.path()).any(|p| p.file_stem() == xmp.file_stem() && p != xmp && is_photo(&p))
        })
}

fn collect(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && depth < 3 {
            collect(&p, depth + 1, out);
        } else if matches!(extension(&p).as_str(), "xmp" | "dng" | "cube" | "3dl") {
            out.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_sample_preset() {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../samples"));
        let presets = scan_presets(dir);
        let dreamy = presets.iter().find(|p| p.path.ends_with("Dreamy days in spring.xmp")).expect("sample preset");
        assert!(matches!(dreamy.kind, PresetKind::Develop(_)));
        assert!(!dreamy.name.is_empty());
    }

    #[test]
    fn photo_extensions() {
        assert!(is_photo(Path::new("a/P1010041.RW2")));
        assert!(is_photo(Path::new("a/b.JPG")));
        assert!(!is_photo(Path::new("a/P1010041.xmp")));
    }
}
