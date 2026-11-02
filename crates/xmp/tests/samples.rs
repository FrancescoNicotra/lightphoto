//! Tests against the "Dreamy days in spring" preset in /samples.

use std::path::PathBuf;

use xmp::{Curve, DevelopSettings, Preset, RgbTable, WhiteBalance, XmpPacket};

fn sample(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples").join(name);
    p.exists().then_some(p).or_else(|| {
        eprintln!("skipping: {name} not found in samples/");
        None
    })
}

const XMP: &str = "Dreamy days in spring.xmp";
const DNG: &str = "Dreamy days in spring - mobile version.dng";

fn curve(pts: &[(f32, f32)]) -> Curve {
    Curve(pts.to_vec())
}

#[test]
fn reads_every_panel_of_the_xmp_preset() {
    let Some(path) = sample(XMP) else { return };
    let p = Preset::load(path).unwrap();
    assert_eq!(p.name.as_deref(), Some("Dreamy days in spring"));
    assert_eq!(p.preset_type.as_deref(), Some("Normal"));

    let s = &p.settings;
    assert_eq!(s.process_version.as_deref(), Some("15.4"));
    assert_eq!(s.white_balance, WhiteBalance::Custom);
    assert_eq!((s.temperature, s.tint), (Some(5600.0), Some(0.0)));
    assert_eq!(s.basic.contrast, -50.0);
    assert_eq!(s.basic.highlights, -25.0);
    assert_eq!(s.basic.shadows, 50.0);
    assert_eq!((s.presence.texture, s.presence.clarity, s.presence.dehaze), (-10.0, -15.0, -5.0));
    assert_eq!(s.presence.vibrance, 10.0);

    // The curves are child elements (rdf:Seq), the part xmp_to_lut.py missed.
    assert_eq!(s.curves.name, "Custom");
    assert_eq!(s.curves.master, curve(&[(0., 0.), (39., 28.), (126., 145.), (255., 255.)]));
    assert_eq!(s.curves.red, curve(&[(5., 0.), (126., 126.), (255., 250.)]));
    assert!(s.curves.green.is_identity());
    assert_eq!(s.curves.blue, curve(&[(0., 0.), (125., 116.), (250., 255.)]));

    assert_eq!(s.hsl.hue, [0., 5., 45., 45., 0., 0., 0., 0.]);
    assert_eq!(s.hsl.saturation, [0., 0., -30., -30., 5., 5., 0., 0.]);
    assert_eq!(s.hsl.luminance, [0., 0., 10., 10., 0., 0., 0., 0.]);

    let g = &s.color_grade;
    assert_eq!((g.shadow.hue, g.shadow.sat), (38.0, 20.0));
    assert_eq!((g.highlight.hue, g.highlight.sat), (222.0, 15.0));
    assert_eq!((g.midtone.hue, g.midtone.sat), (65.0, 40.0));
    assert_eq!(g.blending, 50.0);

    let c = &s.calibration;
    assert_eq!((c.red_hue, c.green_hue, c.blue_hue), (20.0, 20.0, -20.0));
    assert_eq!((s.effects.grain_amount, s.effects.grain_size, s.effects.grain_frequency), (30.0, 30.0, 60.0));
    assert_eq!(s.detail.color_nr, 25.0);

    let look = s.look.as_ref().unwrap();
    assert_eq!(look.name, "Adobe Color");
    assert!(look.stubbed);
    assert!(look.rgb_table.is_none());
}

#[test]
fn dng_mobile_preset_matches_xmp_and_exposes_adobe_color() {
    let (Some(xmp), Some(dng)) = (sample(XMP), sample(DNG)) else { return };
    let from_xmp = Preset::load(xmp).unwrap().settings;
    let from_dng = Preset::load(dng).unwrap().settings;

    // Same develop settings; only the profile is spelled out differently.
    let strip = |s: &DevelopSettings| DevelopSettings { look: None, ..s.clone() };
    assert_eq!(strip(&from_dng), strip(&from_xmp));

    // In the DNG the Adobe Color profile is not stubbed: it reveals its own
    // tone curve and the hash of Adobe's internal look table.
    let look = from_dng.look.unwrap();
    assert_eq!(look.name, "Adobe Color");
    assert_eq!(look.look_table_ref().as_deref(), Some("E1095149FDB39D7A057BAB208837E2E1"));
    assert_eq!(
        look.parameter_settings().curves.master,
        curve(&[(0., 0.), (22., 16.), (40., 35.), (127., 127.), (224., 230.), (240., 246.), (255., 255.)])
    );
}

#[test]
fn xml_round_trip_is_lossless() {
    let Some(path) = sample(XMP) else { return };
    let original = Preset::load(path).unwrap().packet;
    let written = original.to_xml();
    let reparsed = XmpPacket::parse(&written).unwrap();
    assert_eq!(reparsed.props, original.props);
}

#[test]
fn typed_settings_survive_write_into() {
    let Some(path) = sample(XMP) else { return };
    let s = Preset::load(path).unwrap().settings;
    let mut packet = XmpPacket::default();
    s.write_into(&mut packet.props);
    let back = DevelopSettings::from_props(&XmpPacket::parse(&packet.to_xml()).unwrap().props).unwrap();
    assert_eq!(DevelopSettings { look: None, ..back }, DevelopSettings { look: None, ..s });
}

#[test]
fn decodes_rgb_table_embedded_in_a_look() {
    let mut table = RgbTable::identity(5);
    table.samples[0] = [1000, 2000, 3000];
    let xml = format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
        <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:PresetType="Normal">
         <crs:Look><rdf:Description crs:Name="Test LUT" crs:Amount="0.8">
          <crs:Parameters><rdf:Description crs:RGBTable="ABC" crs:Table_ABC="{}"/></crs:Parameters>
         </rdf:Description></crs:Look>
        </rdf:Description></rdf:RDF></x:xmpmeta>"#,
        table.to_blob()
    );
    let p = Preset::from_bytes(xml.as_bytes()).unwrap();
    let look = p.settings.look.unwrap();
    assert_eq!(look.amount, 0.8);
    assert_eq!(look.rgb_table, Some(table));
}
