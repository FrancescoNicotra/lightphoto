//! The manifest of Lightroom references (`references.toml`).
//!
//! ```toml
//! size = 480           # long edge of the comparison, in pixels
//! max_delta_e = 3.0    # default threshold on the mean ΔE2000
//!
//! [[reference]]
//! name = "contrast-p50"
//! source = "pixls/P1010041-camera.jpg"     # relative to the samples root
//! kind = "jpeg"                            # "jpeg" (any rendered file) or "raw"
//! settings = { Contrast2012 = 50 }         # crs: keys, on top of the preset if any
//! reference = "pixls/lightroom/P1010041-contrast-p50.jpg"
//! max_delta_e = 2.0                        # optional, overrides the default
//! ```
//!
//! An entry may also name a `preset` (.xmp, .dng…); its settings are
//! applied first, then `settings`. Unknown keys are refused, so a typo
//! fails at once instead of measuring an unedited photo.

use std::path::{Path, PathBuf};

use pipeline::SourceKind;
use serde::Deserialize;
use xmp::{DevelopSettings, Preset, Props, Value};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default = "default_size")]
    pub size: usize,
    #[serde(default = "default_max")]
    pub max_delta_e: f64,
    #[serde(default, rename = "reference")]
    pub references: Vec<Entry>,
}

fn default_size() -> usize {
    480
}

fn default_max() -> f64 {
    3.0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// An already rendered file: JPEG, PNG or TIFF.
    Jpeg,
    Raw,
}

impl Kind {
    pub fn source_kind(self) -> SourceKind {
        match self {
            Kind::Jpeg => SourceKind::Rendered,
            Kind::Raw => SourceKind::Raw,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    pub source: PathBuf,
    pub kind: Kind,
    #[serde(default)]
    pub preset: Option<PathBuf>,
    /// Camera Raw keys without the `crs:` prefix, e.g. `Contrast2012 = -50`.
    #[serde(default)]
    pub settings: toml::Table,
    /// Lightroom's export.
    pub reference: PathBuf,
    #[serde(default)]
    pub max_delta_e: Option<f64>,
    /// Free text: how the reference was made.
    #[serde(default)]
    #[allow(dead_code)]
    pub note: Option<String>,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Manifest, Box<dyn std::error::Error>> {
        Manifest::parse(&std::fs::read_to_string(path)?)
    }

    pub fn parse(text: &str) -> Result<Manifest, Box<dyn std::error::Error>> {
        let m: Manifest = toml::from_str(text)?;
        if m.size == 0 {
            return Err("size must be positive".into());
        }
        for (i, e) in m.references.iter().enumerate() {
            if m.references[..i].iter().any(|o| o.name == e.name) {
                return Err(format!("duplicate entry name {:?}", e.name).into());
            }
            e.props().map_err(|err| format!("{}: {err}", e.name))?;
        }
        Ok(m)
    }
}

impl Entry {
    /// The files the entry needs, relative to the samples root.
    pub fn inputs(&self) -> impl Iterator<Item = &PathBuf> {
        [Some(&self.source), self.preset.as_ref(), Some(&self.reference)].into_iter().flatten()
    }

    /// `settings` as XMP properties.
    pub fn props(&self) -> Result<Props, String> {
        let mut props = Props::default();
        for (key, value) in &self.settings {
            if !DevelopSettings::is_known_key(key) {
                return Err(format!("unknown Camera Raw key {key:?}"));
            }
            let text = match value {
                toml::Value::Integer(v) => v.to_string(),
                toml::Value::Float(v) => v.to_string(),
                toml::Value::String(v) => v.clone(),
                toml::Value::Boolean(v) => if *v { "True" } else { "False" }.to_string(),
                _ => return Err(format!("{key}: expected a number, a string or a boolean")),
            };
            props.set_crs(key, Value::Text(text));
        }
        Ok(props)
    }

    /// The settings to develop with: Camera Raw's defaults for the kind of
    /// source, then the preset, then the entry's own keys.
    pub fn settings(&self, kind: SourceKind, preset: Option<&Preset>) -> Result<DevelopSettings, Box<dyn std::error::Error>> {
        let mut s = base_settings(kind);
        if let Some(p) = preset {
            s.try_apply(&p.packet.props)?;
        }
        s.try_apply(&self.props()?)?;
        Ok(s)
    }
}

/// Camera Raw's defaults for the kind of source, as the app starts from:
/// raw files get the default sharpening and colour noise reduction,
/// rendered ones none.
pub fn base_settings(kind: SourceKind) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    if kind == SourceKind::Rendered {
        s.detail.sharpness = 0.0;
        s.detail.color_nr = 0.0;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(entry: &str) -> Result<Manifest, Box<dyn std::error::Error>> {
        Manifest::parse(&format!("[[reference]]\nsource = \"a.jpg\"\nreference = \"b.jpg\"\nkind = \"jpeg\"\n{entry}"))
    }

    #[test]
    fn keys_become_settings() {
        let m = one("name = \"x\"\nsettings = { Contrast2012 = -50, HueAdjustmentGreen = 50, ColorGradeMidtoneHue = 60.0 }").unwrap();
        let e = &m.references[0];
        assert_eq!((m.size, m.max_delta_e, e.max_delta_e), (480, 3.0, None));
        let s = e.settings(SourceKind::Rendered, None).unwrap();
        assert_eq!(s.basic.contrast, -50.0);
        assert_eq!(s.hsl.hue[3], 50.0);
        assert_eq!(s.color_grade.midtone.hue, 60.0);
        // A JPEG starts with no sharpening, as in Lightroom.
        assert_eq!(s.detail.sharpness, 0.0);
        assert_eq!(e.settings(SourceKind::Raw, None).unwrap().detail.sharpness, 40.0);
    }

    #[test]
    fn keys_override_the_preset() {
        let xml = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
            <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Contrast2012="+20" crs:Shadows2012="+30"/>
            </rdf:RDF></x:xmpmeta>"#;
        let preset = Preset::from_bytes(xml.as_bytes()).unwrap();
        let m = one("name = \"x\"\nsettings = { Contrast2012 = -50 }").unwrap();
        let s = m.references[0].settings(SourceKind::Rendered, Some(&preset)).unwrap();
        assert_eq!((s.basic.contrast, s.basic.shadows), (-50.0, 30.0));
    }

    #[test]
    fn typos_and_duplicates_are_refused() {
        assert!(one("name = \"x\"\nsettings = { Contrast = -50 }").is_err());
        assert!(one("name = \"x\"\nsettings = { Contrast2012 = [1] }").is_err());
        assert!(one("name = \"x\"\nkid = \"raw\"").is_err());
        assert!(Manifest::parse("[[reference]]\nname = \"x\"\nsource = \"a\"\nreference = \"b\"\nkind = \"png\"").is_err());
        let twice = "[[reference]]\nname = \"x\"\nsource = \"a\"\nreference = \"b\"\nkind = \"jpeg\"\n";
        assert!(Manifest::parse(&format!("{twice}{twice}")).is_err());
    }

    /// The manifest in the repository parses, and every entry has its own
    /// reference file.
    #[test]
    fn shipped_manifest_is_valid() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("references.toml");
        let m = Manifest::load(&path).unwrap();
        assert!(m.references.len() > 20);
        for (i, e) in m.references.iter().enumerate() {
            assert!(m.references[..i].iter().all(|o| o.reference != e.reference), "{} shares its reference", e.name);
        }
    }
}
