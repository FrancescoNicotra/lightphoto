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
}

impl Edit {
    /// Camera Raw's defaults for the kind of source: raw files get the
    /// default sharpening and colour noise reduction, rendered ones none.
    pub fn defaults(kind: SourceKind) -> Edit {
        let mut settings = DevelopSettings::default();
        if kind == SourceKind::Rendered {
            settings.detail.sharpness = 0.0;
            settings.detail.color_nr = 0.0;
        }
        Edit { settings, opaque: Props::default(), lut: None, lut_amount: 100.0, preset_name: None }
    }

    /// Applies a preset on top of the current settings, as Lightroom does:
    /// only the keys the preset contains change.
    pub fn apply_preset(&mut self, preset: &Preset, name: &str) -> Result<(), xmp::Error> {
        let mut settings = self.settings.clone();
        settings.try_apply(&preset.packet.props)?;
        self.settings = settings;
        for p in preset.packet.props.iter().filter(|p| p.ns == NS_CRS && is_opaque(&p.name)) {
            self.opaque.set(NS_CRS, &p.name, p.value.clone());
        }
        self.preset_name = Some(name.to_string());
        Ok(())
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
        Ok(edit)
    }

    /// Writes the edit into an XMP packet (a sidecar's, or a new one).
    pub fn write_into(&self, packet: &mut XmpPacket) {
        let props = &mut packet.props;
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

/// Where Lightroom keeps the settings of a RAW file: `name.xmp` next to it.
/// Rendered files (JPEG, TIFF…) would carry them inside, which we do not
/// write, so they have no sidecar.
pub fn sidecar_path(photo: &Path) -> Option<PathBuf> {
    (!raw::is_rendered_image(photo)).then(|| photo.with_extension("xmp"))
}

/// The edit saved next to `photo`, if any.
pub fn load_sidecar(photo: &Path, kind: SourceKind) -> Option<Edit> {
    let path = sidecar_path(photo).filter(|p| p.is_file())?;
    let preset = Preset::load(&path).ok()?;
    Edit::from_props(&preset.packet.props, kind).ok()
}

/// Saves the edit to the photo's sidecar, keeping whatever else the file
/// already contains (other namespaces, unknown crs keys).
pub fn save_sidecar(photo: &Path, edit: &Edit) -> Result<PathBuf, String> {
    let path = sidecar_path(photo).ok_or("i file JPEG/TIFF/PNG non hanno un sidecar .xmp")?;
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
    fn sidecars_only_for_raw_files() {
        assert_eq!(sidecar_path(Path::new("/a/P1.RW2")), Some(PathBuf::from("/a/P1.xmp")));
        assert_eq!(sidecar_path(Path::new("/a/P1.jpg")), None);
    }
}
