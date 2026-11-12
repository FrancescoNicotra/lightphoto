//! Where the tests find their sample photos.
//!
//! ```text
//! samples/
//! ├ Dreamy days in spring.xmp       versioned: required
//! ├ ci/                             versioned: required (crops, ~1.5 MB)
//! │ ├ P1010041-crop.dng                1024 × 768 Bayer crop of the RW2
//! │ ├ P1010041-camera-crop.jpg         800 × 600 of the camera JPEG
//! │ └ P1010041-camera-lightroom-dreamy-crop.jpg   the same, by Lightroom
//! └ pixls/                          not versioned: the full files (optional)
//!   ├ P1010041.RW2                     raw.pixls.us, CC0
//!   ├ P1010041-camera.jpg
//!   └ P1010041-camera-lightroom-dreamy.jpg
//! ```
//!
//! [`gx80`] picks the full files when `samples/pixls` has them and the crops
//! otherwise; `LUMEN_SAMPLES=ci` or `LUMEN_SAMPLES=full` forces one set. A
//! missing versioned file, or a missing full file with `LUMEN_SAMPLES=full`,
//! is a test failure, never a skip. `cargo run -p raw --example
//! make_ci_samples` rebuilds `samples/ci` from `samples/pixls`.

use std::path::{Path, PathBuf};

/// The repository's `samples/` folder.
pub fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples")
}

/// A versioned file in `samples/`: panics if it is missing.
pub fn require(name: &str) -> PathBuf {
    let p = dir().join(name);
    assert!(p.exists(), "sample {} missing: it is versioned, check out the whole repository", p.display());
    p
}

/// A file that only exists on some machines (not versioned, e.g. a photo
/// with a camera serial number): `None`, with a note on stderr, if missing.
pub fn optional(name: &str) -> Option<PathBuf> {
    let p = dir().join(name);
    p.exists().then_some(p).or_else(|| {
        eprintln!("skipping: {name} not found in samples/ (a local file, not versioned)");
        None
    })
}

/// Which version of the GX80 sample a test runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Set {
    /// `samples/pixls`: the whole 4592 × 3448 frame.
    Full,
    /// `samples/ci`: the versioned crops.
    Ci,
}

/// The Panasonic GX80 sample (P1010041 from raw.pixls.us) in one version.
#[derive(Clone, Debug)]
pub struct Gx80 {
    pub set: Set,
    /// The RAW: the RW2, or the Bayer DNG cropped from it.
    pub raw: PathBuf,
    /// The camera's JPEG (1920 × 1440), or the same 800 × 600 crop of it.
    pub camera_jpeg: PathBuf,
    /// Lightroom 12 for iOS: the camera JPEG with "Dreamy days in spring",
    /// exported as sRGB JPEG at full size; or its 800 × 600 crop.
    pub lightroom_dreamy: PathBuf,
}

impl Gx80 {
    fn full() -> Gx80 {
        let d = dir().join("pixls");
        Gx80 {
            set: Set::Full,
            raw: d.join("P1010041.RW2"),
            camera_jpeg: d.join("P1010041-camera.jpg"),
            lightroom_dreamy: d.join("P1010041-camera-lightroom-dreamy.jpg"),
        }
    }

    fn ci() -> Gx80 {
        let d = dir().join("ci");
        Gx80 {
            set: Set::Ci,
            raw: d.join("P1010041-crop.dng"),
            camera_jpeg: d.join("P1010041-camera-crop.jpg"),
            lightroom_dreamy: d.join("P1010041-camera-lightroom-dreamy-crop.jpg"),
        }
    }

    fn files(&self) -> [&Path; 3] {
        [&self.raw, &self.camera_jpeg, &self.lightroom_dreamy]
    }

    fn missing(&self) -> Vec<String> {
        self.files().into_iter().filter(|p| !p.exists()).map(|p| p.display().to_string()).collect()
    }
}

/// The GX80 sample: the full files if all present (unless `LUMEN_SAMPLES=ci`),
/// else the crops. Panics if the chosen set is incomplete.
pub fn gx80() -> Gx80 {
    let forced = std::env::var("LUMEN_SAMPLES").ok();
    let full = Gx80::full();
    let chosen = match forced.as_deref() {
        Some("ci") => Gx80::ci(),
        Some("full") => full,
        None | Some("") => {
            if full.missing().is_empty() {
                full
            } else {
                Gx80::ci()
            }
        }
        Some(other) => panic!("LUMEN_SAMPLES={other}: expected `ci` or `full`"),
    };
    let missing = chosen.missing();
    assert!(missing.is_empty(), "{:?} samples missing: {}", chosen.set, missing.join(", "));
    eprintln!("GX80 sample: {:?} ({})", chosen.set, chosen.raw.display());
    chosen
}
