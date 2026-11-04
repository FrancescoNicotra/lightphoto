//! Adobe's own profiles, read from a local Lightroom / Camera Raw install
//! when there is one: camera profiles (.dcp, e.g. "Adobe Standard") and the
//! "Adobe Raw" looks (Adobe Color, Vivid…), whose look tables are not in
//! presets or DNGs. Nothing is bundled: these files are Adobe's.
//!
//! Search roots, in order: the folders in `LUMEN_ADOBE_DIR` (separated like
//! PATH), then the usual install locations. Each root may contain
//! `CameraProfiles/` and `Settings/…/Profiles/`.

use std::path::{Path, PathBuf};

use raw::color::Mat3;
use raw::{ColorSource, Photo};
use xmp::{DevelopSettings, Preset, Value, NS_CRS};

#[derive(Clone, Debug, Default)]
pub struct AdobeProfiles {
    pub roots: Vec<PathBuf>,
}

fn default_roots() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        v.push("/Applications/Adobe Lightroom.app/Contents/Resources".into());
        v.push("/Applications/Adobe Lightroom Classic/Adobe Lightroom Classic.app/Contents/Resources".into());
        v.push("/Library/Application Support/Adobe/CameraRaw".into());
        if let Some(home) = std::env::var_os("HOME") {
            v.push(PathBuf::from(home).join("Library/Application Support/Adobe/CameraRaw"));
        }
    } else if cfg!(windows) {
        v.push(r"C:\ProgramData\Adobe\CameraRaw".into());
        if let Some(appdata) = std::env::var_os("APPDATA") {
            v.push(PathBuf::from(appdata).join(r"Adobe\CameraRaw"));
        }
    }
    v
}

impl AdobeProfiles {
    pub fn discover() -> Self {
        let mut roots: Vec<PathBuf> = std::env::var_os("LUMEN_ADOBE_DIR").map(|v| std::env::split_paths(&v).collect()).unwrap_or_default();
        roots.extend(default_roots());
        roots.retain(|r| r.is_dir());
        AdobeProfiles { roots }
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// The .dcp called `profile_name` (e.g. "Adobe Standard") for a camera.
    /// Tries the file name first; otherwise looks among the same maker's
    /// profiles for one with the same first ColorMatrix, which finds regional
    /// twins such as the GX80, sold as GX85 in the US.
    pub fn camera_profile(&self, make: &str, model: &str, color_matrix: Option<&Mat3>, profile_name: &str) -> Option<PathBuf> {
        let file = format!("{make} {model} {profile_name}.dcp").to_ascii_lowercase();
        let folders: Vec<PathBuf> = self.roots.iter().map(|r| r.join("CameraProfiles").join(profile_name)).filter(|d| d.is_dir()).collect();
        let entries = || folders.iter().filter_map(|d| std::fs::read_dir(d).ok()).flatten().filter_map(|e| e.ok()).map(|e| e.path());
        if let Some(p) = entries().find(|p| file_name(p).to_ascii_lowercase() == file) {
            return Some(p);
        }
        let cm = color_matrix?;
        let prefix = make.to_ascii_lowercase();
        let suffix = format!("{profile_name}.dcp").to_ascii_lowercase();
        entries()
            .filter(|p| {
                let n = file_name(p).to_ascii_lowercase();
                n.starts_with(&prefix) && n.ends_with(&suffix)
            })
            .find(|p| raw::dcp::peek(p).is_some_and(|(_, m)| same_matrix(&m, cm)))
    }

    /// A look profile (.xmp with PresetType "Look") by UUID, or by name.
    pub fn look(&self, name: &str, uuid: Option<&str>) -> Option<PathBuf> {
        let mut found_by_name = None;
        for root in &self.roots {
            let mut files = Vec::new();
            profile_files(&root.join("Settings"), &mut files, false);
            for f in files {
                let Ok(text) = std::fs::read_to_string(&f) else { continue };
                if uuid.is_some_and(|u| text.contains(&format!("crs:UUID=\"{u}\""))) {
                    return Some(f);
                }
                if found_by_name.is_none() && file_name(&f).eq_ignore_ascii_case(&format!("{name}.xmp")) {
                    found_by_name = Some(f);
                }
            }
        }
        found_by_name
    }
}

fn file_name(p: &Path) -> &str {
    p.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

fn same_matrix(a: &Mat3, b: &Mat3) -> bool {
    a.iter().flatten().zip(b.iter().flatten()).all(|(x, y)| (x - y).abs() < 2e-4)
}

/// .xmp files under any folder named "Profiles" below `dir`.
fn profile_files(dir: &Path, out: &mut Vec<PathBuf>, inside: bool) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            profile_files(&p, out, inside || file_name(&p) == "Profiles");
        } else if inside && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("xmp")) {
            out.push(p);
        }
    }
}

/// What [`prepare`] did, for the user.
#[derive(Clone, Debug, Default)]
pub struct Prepared {
    pub camera_profile: Option<PathBuf>,
    pub look: Option<PathBuf>,
    pub notes: Vec<String>,
}

/// Swaps in Adobe's camera profile for a RAW photo (the one the look asks
/// for, Adobe Standard by default) and fills referenced-only looks from the
/// installed profile files.
pub fn prepare(adobe: &AdobeProfiles, photo: &mut Photo, settings: &mut DevelopSettings) -> Prepared {
    let mut out = Prepared::default();
    if adobe.is_empty() {
        return out;
    }

    if let Some(look) = &mut settings.look {
        if look.look_table.is_none() && look.rgb_table.is_none() {
            match adobe.look(&look.name, look.uuid.as_deref()) {
                Some(path) => match Preset::load(&path).and_then(|p| look.resolve_from(&p.packet.props)) {
                    Ok(()) => out.look = Some(path),
                    Err(e) => out.notes.push(format!("could not read {}: {e}", path.display())),
                },
                None => out.notes.push(format!("profile \"{}\" not found in the Adobe install", look.name)),
            }
        }
    }

    if let ColorSource::Camera(cam) = &photo.color {
        let wanted = settings
            .look
            .as_ref()
            .and_then(|l| l.parameters.get(NS_CRS, "CameraProfile").and_then(Value::as_text).map(str::to_string))
            .unwrap_or_else(|| "Adobe Standard".into());
        let cm = cam.profile.calibrations.first().map(|c| c.color_matrix);
        match adobe.camera_profile(&photo.info.make, &photo.info.model, cm.as_ref(), &wanted) {
            Some(path) => match raw::dcp::load(&path) {
                Ok(dcp) => {
                    photo.set_profile(dcp.profile);
                    out.camera_profile = Some(path);
                }
                Err(e) => out.notes.push(format!("could not read {}: {e}", path.display())),
            },
            None => out.notes.push(format!("no \"{wanted}\" profile installed for {} {}", photo.info.make, photo.info.model)),
        }
    }
    out
}
