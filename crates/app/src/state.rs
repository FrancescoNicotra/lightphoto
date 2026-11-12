//! The edit of one photo (develop settings, LUT, opaque XMP parts), its
//! undo history and the `.xmp` sidecar it is saved to.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lut::CubeFile;
use pipeline::SourceKind;
use xmp::{DevelopSettings, Preset, Props, Value, XmpPacket, NS_CRS};

/// A .cube / .3dl LUT applied after the develop settings.
#[derive(Clone, Debug)]
pub struct LutRef {
    pub path: PathBuf,
    pub name: String,
    pub cube: Arc<CubeFile>,
}

impl PartialEq for LutRef {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && Arc::ptr_eq(&self.cube, &other.cube)
    }
}

/// Everything the user can change on a photo. Cheap enough to clone for
/// each undo step.
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    pub settings: DevelopSettings,
    /// crs properties the typed settings do not write back (the profile
    /// `Look`, `RGBTable` and its `Table_*` data), kept verbatim for the
    /// sidecar.
    pub opaque: Props,
    pub lut: Option<LutRef>,
    /// LUT strength, 0..100.
    pub lut_amount: f32,
    /// Name of the last preset applied, for `{preset}` in export names.
    pub preset_name: Option<String>,
    /// The preset just applied, while its strength can still be changed
    /// (until another setting is edited).
    pub preset: Option<Box<AppliedPreset>>,
}

/// A preset applied at an adjustable strength (Lightroom's preset Amount).
#[derive(Clone, Debug, PartialEq)]
pub struct AppliedPreset {
    pub name: String,
    /// Settings before the preset and with it at 100%.
    pub before: DevelopSettings,
    pub after: DevelopSettings,
    /// 0..200 (%).
    pub amount: f32,
    /// The settings the amount produced; any other change ends the preset.
    pub result: DevelopSettings,
}

/// Namespace of the app's own sidecar properties (the LUT).
pub const NS_LUMEN: &str = "https://github.com/FrancescoNicotra/lightphoto/ns/1.0/";

impl Edit {
    /// Camera Raw's defaults for the kind of source: raw files get the
    /// default sharpening and colour noise reduction, rendered ones none.
    pub fn defaults(kind: SourceKind) -> Edit {
        let mut settings = DevelopSettings::default();
        if kind == SourceKind::Rendered {
            settings.detail.sharpness = 0.0;
            settings.detail.color_nr = 0.0;
        }
        Edit { settings, opaque: Props::default(), lut: None, lut_amount: 100.0, preset_name: None, preset: None }
    }

    /// Applies a preset on top of the current settings, as Lightroom does:
    /// only the keys the preset contains change.
    pub fn apply_preset(&mut self, preset: &Preset, name: &str) -> Result<(), xmp::Error> {
        let mut settings = self.settings.clone();
        settings.try_apply(&preset.packet.props)?;
        self.preset = Some(Box::new(AppliedPreset {
            name: name.to_string(),
            before: self.settings.clone(),
            after: settings.clone(),
            amount: 100.0,
            result: settings.clone(),
        }));
        self.settings = settings;
        for p in preset.packet.props.iter().filter(|p| p.ns == NS_CRS && is_opaque(&p.name)) {
            self.opaque.set(NS_CRS, &p.name, p.value.clone());
        }
        self.preset_name = Some(name.to_string());
        Ok(())
    }

    /// Changes the strength of the preset just applied. `as_shot` is the
    /// camera white balance, for presets that set an absolute one.
    pub fn set_preset_amount(&mut self, amount: f32, as_shot: Option<(f32, f32)>) {
        let Some(p) = &mut self.preset else { return };
        p.amount = amount.clamp(0.0, 200.0);
        p.result = crate::blend::blend(&p.before, &p.after, p.amount / 100.0, as_shot);
        self.settings = p.result.clone();
    }

    /// Forgets the preset strength once the settings were changed in
    /// another way: the amount would no longer mean anything.
    pub fn drop_stale_preset(&mut self) {
        if self.preset.as_ref().is_some_and(|p| p.result != self.settings) {
            self.preset = None;
        }
    }

    /// Whether the edit differs from the defaults for this kind of file.
    pub fn is_modified(&self, kind: SourceKind) -> bool {
        let d = Edit::defaults(kind);
        self.settings != d.settings || self.lut.is_some() || !self.opaque.is_empty()
    }

    /// Takes the develop settings of `other` (Paste settings).
    pub fn paste(&mut self, other: &Edit) {
        *self = Edit { preset: None, ..other.clone() };
    }

    /// Resets everything except the LUT choice, like Lightroom's "Reset".
    pub fn reset(&mut self, kind: SourceKind) {
        *self = Edit { lut: None, ..Edit::defaults(kind) };
    }

    /// Settings saved in a sidecar (or embedded in a file), over the defaults.
    pub fn from_props(props: &Props, kind: SourceKind) -> Result<Edit, xmp::Error> {
        let mut edit = Edit::defaults(kind);
        edit.settings.try_apply(props)?;
        for p in props.iter().filter(|p| p.ns == NS_CRS && is_opaque(&p.name)) {
            edit.opaque.set(NS_CRS, &p.name, p.value.clone());
        }
        if let Some(path) = props.get(NS_LUMEN, "LUT").and_then(Value::as_text).map(PathBuf::from) {
            // A LUT that has moved is dropped, the rest of the edit stays.
            if let Ok(cube) = lut::load(&path) {
                let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                edit.lut = Some(LutRef { path, name, cube: Arc::new(cube) });
                edit.lut_amount = props.get(NS_LUMEN, "LUTAmount").and_then(Value::as_f32).unwrap_or(100.0);
            }
        }
        Ok(edit)
    }

    /// Writes the edit into an XMP packet (a sidecar's, or a new one).
    pub fn write_into(&self, packet: &mut XmpPacket) {
        if !packet.prefixes.iter().any(|(_, u)| u == NS_LUMEN) {
            packet.prefixes.push(("lumen".into(), NS_LUMEN.into()));
        }
        let props = &mut packet.props;
        match &self.lut {
            Some(l) => {
                props.set(NS_LUMEN, "LUT", Value::Text(l.path.to_string_lossy().into_owned()));
                props.set(NS_LUMEN, "LUTAmount", Value::Text(xmp::fmt_num(self.lut_amount)));
            }
            None => {
                props.remove(NS_LUMEN, "LUT");
                props.remove(NS_LUMEN, "LUTAmount");
            }
        }
        self.settings.write_into(props);
        props.set_crs("HasSettings", Value::Text("True".into()));
        for p in self.opaque.iter() {
            props.set(&p.ns, &p.name, p.value.clone());
        }
        match &self.settings.look {
            None => {
                props.remove(NS_CRS, "Look");
            }
            Some(look) => {
                // The look is written from its source properties; only the
                // amount is edited in the app.
                if let Some(Value::Struct(l)) = props.0.iter_mut().find(|p| p.ns == NS_CRS && p.name == "Look").map(|p| &mut p.value) {
                    l.set_crs("Amount", Value::Text(xmp::fmt_num(look.amount)));
                }
            }
        }
        if self.settings.rgb_table.is_none() {
            props.remove(NS_CRS, "RGBTable");
        }
    }
}

fn is_opaque(key: &str) -> bool {
    key == "Look" || key == "RGBTable" || key.starts_with("Table_")
}

/// Where the settings of a photo are saved. RAW files: `name.xmp`, as in
/// Lightroom. JPEG/PNG/TIFF: Lightroom writes inside the file, which we
/// never touch, so they get `name.jpg.xmp` (the extension kept, so a RAW and
/// a JPEG with the same name do not share one).
pub fn sidecar_path(photo: &Path) -> PathBuf {
    if raw::is_rendered_image(photo) {
        let mut name = photo.file_name().unwrap_or_default().to_os_string();
        name.push(".xmp");
        photo.with_file_name(name)
    } else {
        photo.with_extension("xmp")
    }
}

/// The edit saved next to `photo`, if any.
pub fn load_sidecar(photo: &Path, kind: SourceKind) -> Option<Edit> {
    let path = Some(sidecar_path(photo)).filter(|p| p.is_file())?;
    let preset = Preset::load(&path).ok()?;
    Edit::from_props(&preset.packet.props, kind).ok()
}

/// Saves the edit to the photo's sidecar, keeping whatever else the file
/// already contains (other namespaces, unknown crs keys).
pub fn save_sidecar(photo: &Path, edit: &Edit) -> Result<PathBuf, String> {
    let path = sidecar_path(photo);
    let mut packet = match std::fs::read_to_string(&path) {
        Ok(text) => XmpPacket::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?,
        Err(_) => XmpPacket::default(),
    };
    edit.write_into(&mut packet);
    std::fs::write(&path, packet.to_xml()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Undo/redo by snapshots. Changes are recorded at checkpoints (when a
/// gesture ends), so one slider drag is one undo step.
#[derive(Clone, Debug)]
pub struct History {
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    /// The state at the last checkpoint.
    committed: Edit,
}

const MAX_UNDO: usize = 200;

impl History {
    pub fn new(edit: &Edit) -> History {
        History { undo: Vec::new(), redo: Vec::new(), committed: edit.clone() }
    }

    /// Records `current` as a new step if it differs from the last one.
    pub fn checkpoint(&mut self, current: &Edit) {
        if *current != self.committed {
            self.undo.push(std::mem::replace(&mut self.committed, current.clone()));
            self.redo.clear();
            if self.undo.len() > MAX_UNDO {
                self.undo.remove(0);
            }
        }
    }

    pub fn can_undo(&self, current: &Edit) -> bool {
        !self.undo.is_empty() || *current != self.committed
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self, current: &mut Edit) -> bool {
        self.checkpoint(current);
        let Some(prev) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(current, prev));
        self.committed = current.clone();
        true
    }

    pub fn redo(&mut self, current: &mut Edit) -> bool {
        self.checkpoint(current);
        let Some(next) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(current, next));
        self.committed = current.clone();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_exposure(ev: f32) -> Edit {
        let mut e = Edit::defaults(SourceKind::Raw);
        e.settings.basic.exposure = ev;
        e
    }

    #[test]
    fn undo_redo_by_checkpoints() {
        let mut cur = with_exposure(0.0);
        let mut h = History::new(&cur);
        assert!(!h.can_undo(&cur));

        // A drag through several values is a single step.
        cur.settings.basic.exposure = 0.3;
        cur.settings.basic.exposure = 0.7;
        h.checkpoint(&cur);
        cur.settings.basic.exposure = 1.0;
        h.checkpoint(&cur);
        h.checkpoint(&cur);

        assert!(h.undo(&mut cur));
        assert_eq!(cur, with_exposure(0.7));
        assert!(h.undo(&mut cur));
        assert_eq!(cur, with_exposure(0.0));
        assert!(!h.undo(&mut cur));

        assert!(h.redo(&mut cur));
        assert_eq!(cur, with_exposure(0.7));
        // A new change drops the redo branch.
        cur.settings.basic.contrast = 20.0;
        h.checkpoint(&cur);
        assert!(!h.can_redo());
        assert!(!h.redo(&mut cur));
    }

    #[test]
    fn uncommitted_change_is_undone_too() {
        let mut cur = with_exposure(0.0);
        let mut h = History::new(&cur);
        cur.settings.basic.exposure = 2.0;
        assert!(h.can_undo(&cur));
        assert!(h.undo(&mut cur));
        assert_eq!(cur, with_exposure(0.0));
        assert!(h.redo(&mut cur));
        assert_eq!(cur, with_exposure(2.0));
    }

    #[test]
    fn rendered_sources_start_without_sharpening() {
        assert_eq!(Edit::defaults(SourceKind::Rendered).settings.detail.sharpness, 0.0);
        assert_eq!(Edit::defaults(SourceKind::Raw).settings.detail.sharpness, 40.0);
    }

    #[test]
    fn preset_and_sidecar_round_trip() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../samples/Dreamy days in spring.xmp");
        let preset = Preset::load(path).unwrap();
        let mut edit = Edit::defaults(SourceKind::Raw);
        edit.apply_preset(&preset, "Dreamy").unwrap();
        edit.settings.basic.exposure += 0.5;
        assert_ne!(edit.settings, Edit::defaults(SourceKind::Raw).settings);

        let mut packet = XmpPacket::default();
        edit.write_into(&mut packet);
        let back = XmpPacket::parse(&packet.to_xml()).unwrap();
        let reread = Edit::from_props(&back.props, SourceKind::Raw).unwrap();
        assert_eq!(reread.settings, edit.settings);
        assert_eq!(reread.opaque, edit.opaque);
    }

    #[test]
    fn sidecar_names() {
        assert_eq!(sidecar_path(Path::new("/a/P1.RW2")), PathBuf::from("/a/P1.xmp"));
        assert_eq!(sidecar_path(Path::new("/a/P1.jpg")), PathBuf::from("/a/P1.jpg.xmp"));
    }

    #[test]
    fn preset_amount_and_stale_presets() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../samples/Dreamy days in spring.xmp");
        let preset = Preset::load(path).unwrap();
        let mut edit = Edit::defaults(SourceKind::Raw);
        edit.apply_preset(&preset, "Dreamy").unwrap();
        let full = edit.settings.basic.contrast;
        edit.set_preset_amount(50.0, Some((5000.0, 0.0)));
        assert_eq!(edit.settings.basic.contrast, full / 2.0);
        edit.drop_stale_preset();
        assert!(edit.preset.is_some());
        edit.settings.basic.exposure = 1.0;
        edit.drop_stale_preset();
        assert!(edit.preset.is_none());
        assert!(edit.is_modified(SourceKind::Raw));
        assert!(!Edit::defaults(SourceKind::Raw).is_modified(SourceKind::Raw));
    }

    #[test]
    fn sidecar_with_masks_reads_back_the_same() {
        use xmp::masks::{BlendMode, LocalCorrection, MaskComponent, MaskShape, Stroke};
        let dir = std::env::temp_dir().join(format!("lumen-masks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("P1.RW2");
        let mut edit = Edit::defaults(SourceKind::Raw);
        let mut sky = LocalCorrection::new("Sky", vec![MaskComponent::new(MaskShape::Linear { zero: (0.5, 0.55), full: (0.5, 0.25) })]);
        sky.adjust.exposure = -1.5;
        sky.adjust.temperature = -30.0;
        let mut brush = MaskComponent::new(MaskShape::Brush {
            strokes: vec![Stroke { erase: false, radius: 0.04, flow: 1.0, center_weight: 0.5, dabs: vec![(0.15, 0.85), (0.16, 0.84)], extra: Props::default() }],
        });
        brush.mode = BlendMode::Subtract;
        let mut subject = LocalCorrection::new(
            "Subject",
            vec![MaskComponent::new(MaskShape::Radial { top: 0.35, left: 0.3, bottom: 0.75, right: 0.65, angle: 20.0, feather: 60.0, roundness: 0.0, midpoint: 50.0 }), brush],
        );
        subject.amount = 0.8;
        subject.adjust.shadows = 40.0;
        edit.settings.masks = vec![sky, subject];
        save_sidecar(&photo, &edit).unwrap();
        let back = load_sidecar(&photo, SourceKind::Raw).unwrap();
        assert_eq!(back.settings.masks, edit.settings.masks);
        assert_eq!(back.settings, edit.settings);
        // Saving what was read gives the same file.
        let first = std::fs::read_to_string(dir.join("P1.xmp")).unwrap();
        save_sidecar(&photo, &back).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("P1.xmp")).unwrap(), first);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn sidecar_keeps_the_lut() {
        let dir = std::env::temp_dir().join(format!("lumen-sidecar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cube = dir.join("look.cube");
        lut::Lut3D::identity(2).write_cube(&cube, None).unwrap();
        let photo = dir.join("P1.jpg");
        let mut edit = Edit::defaults(SourceKind::Rendered);
        edit.lut = Some(LutRef { path: cube.clone(), name: "look".into(), cube: Arc::new(lut::load(&cube).unwrap()) });
        edit.lut_amount = 40.0;
        edit.settings.basic.exposure = 0.5;
        assert_eq!(save_sidecar(&photo, &edit).unwrap(), dir.join("P1.jpg.xmp"));
        let back = load_sidecar(&photo, SourceKind::Rendered).unwrap();
        assert_eq!(back.settings, edit.settings);
        assert_eq!(back.lut.as_ref().map(|l| &l.path), Some(&cube));
        assert_eq!(back.lut_amount, 40.0);
        std::fs::remove_dir_all(dir).ok();
    }
}
