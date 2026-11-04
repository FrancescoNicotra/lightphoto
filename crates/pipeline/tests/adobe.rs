//! Adobe's installed profiles (skipped without a Lightroom / Camera Raw
//! install, see `pipeline::adobe`).

use pipeline::adobe::AdobeProfiles;
use raw::color::Mat3;
use xmp::{Preset, Value, NS_CRS};

/// The GX80's StdA ColorMatrix (as in rawler's camera table and Adobe's DCP).
const GX80_CM_A: Mat3 = [[0.8795, -0.4348, 0.0193], [-0.4128, 1.1726, 0.2733], [-0.0567, 0.1447, 0.7409]];

fn adobe() -> Option<AdobeProfiles> {
    let a = AdobeProfiles::discover();
    if a.is_empty() {
        eprintln!("skipping: no Adobe install found");
        return None;
    }
    Some(a)
}

#[test]
fn finds_the_gx80_profile_under_its_us_name() {
    let Some(adobe) = adobe() else { return };
    let Some(path) = adobe.camera_profile("Panasonic", "DMC-GX80", Some(&GX80_CM_A), "Adobe Standard") else {
        eprintln!("skipping: no Panasonic GX80/GX85 profile installed");
        return;
    };
    let dcp = raw::dcp::load(&path).unwrap();
    assert_eq!(dcp.camera, "Panasonic DMC-GX85");
    let p = &dcp.profile;
    assert_eq!(p.name.as_deref(), Some("Adobe Standard"));
    assert!(p.has_forward_matrix());
    let map = p.hue_sat_map(raw::color::D65).expect("Adobe Standard has a HueSatMap");
    assert!(map.hue_divisions > 1 && map.sat_divisions > 1);
    // The map is a correction, not a rewrite: hue shifts stay small.
    assert!(map.data.iter().all(|e| e[0].abs() < 30.0 && e[1] > 0.0 && e[2] > 0.0));
}

#[test]
fn resolves_adobe_color_look_table() {
    let Some(adobe) = adobe() else { return };
    let preset = Preset::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../samples/Dreamy days in spring.xmp")).unwrap();
    let mut look = preset.settings.look.clone().unwrap();
    assert!(look.stubbed && look.look_table.is_none());
    let Some(path) = adobe.look(&look.name, look.uuid.as_deref()) else {
        eprintln!("skipping: Adobe Color not installed");
        return;
    };
    look.resolve_from(&Preset::load(&path).unwrap().packet.props).unwrap();
    let t = look.look_table.as_ref().expect("Adobe Color carries a look table");
    assert_eq!(t.data.len(), t.hue_divisions * t.sat_divisions * t.val_divisions);
    // Same hash as the one Lightroom writes into the DNG.
    assert_eq!(look.look_table_ref().as_deref(), Some("E1095149FDB39D7A057BAB208837E2E1"));
    assert_eq!(look.parameters.get(NS_CRS, "CameraProfile").and_then(Value::as_text), Some("Adobe Standard"));
    // And its tone curve now comes from the file.
    assert_eq!(look.parameter_settings().curves.master.0[1], (22.0, 16.0));
}
