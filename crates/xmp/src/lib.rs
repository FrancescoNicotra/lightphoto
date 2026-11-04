//! Adobe XMP / Camera Raw settings: parsing, writing and typed access.
//!
//! See `docs/xmp-format.md` for the format notes behind this crate.

mod container;
pub mod look_table;
mod packet;
pub mod rgb_table;
mod settings;

use std::path::Path;

pub use container::extract_xmp;
pub use packet::{Prop, Props, Value, XmpPacket, NS_CRS, NS_RDF};
pub use look_table::LookTable;
pub use rgb_table::RgbTable;
pub use settings::*;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Xml(String),
    /// The file contains no XMP packet.
    NoXmp,
    Invalid(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Xml(e) => write!(f, "XML error: {e}"),
            Error::NoXmp => write!(f, "no XMP packet found"),
            Error::Invalid(e) => write!(f, "invalid data: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// A preset or a develop-settings sidecar, loaded from .xmp, .dng, .tif or .jpg.
#[derive(Clone, Debug)]
pub struct Preset {
    pub name: Option<String>,
    pub uuid: Option<String>,
    /// "Normal" for develop presets, "Look" for profiles.
    pub preset_type: Option<String>,
    pub group: Option<String>,
    pub packet: XmpPacket,
    pub settings: DevelopSettings,
}

impl Preset {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::from_bytes(&std::fs::read(path)?)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let xml = extract_xmp(bytes).ok_or(Error::NoXmp)?;
        Self::from_packet(XmpPacket::parse(&xml)?)
    }

    pub fn from_packet(packet: XmpPacket) -> Result<Self, Error> {
        let p = &packet.props;
        let text = |k: &str| p.crs(k).and_then(Value::as_text).filter(|s| !s.is_empty()).map(str::to_string);
        Ok(Preset {
            name: text("Name"),
            uuid: text("UUID"),
            preset_type: text("PresetType"),
            group: text("Group"),
            settings: DevelopSettings::from_props(p)?,
            packet,
        })
    }
}
