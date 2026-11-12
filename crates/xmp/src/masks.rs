//! Local corrections ("masks"): Camera Raw's `MaskGroupBasedCorrections`
//! (Lightroom 11+) and the older `GradientBasedCorrections`,
//! `CircularGradientBasedCorrections` and `PaintBasedCorrections`.
//!
//! See `docs/xmp-format.md` §8 for what is verified and what is not. In
//! short: the field names come from ExifTool's Camera Raw tables; the
//! value scales, blend modes and the brush layout come from other
//! projects' observations of Lightroom and are not confirmed by Adobe.
//!
//! Everything this module does not understand is kept: unknown fields of
//! a correction or of a mask component, and whole components of unknown
//! kinds (AI masks, range masks), are written back as they were read.

use crate::packet::{Props, Value, NS_CRS};
use crate::settings::fmt_num;

/// Where a correction was found, and where it is written back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskContainer {
    /// `crs:MaskGroupBasedCorrections` (Lightroom 11+): what Lumen writes
    /// for new masks.
    Group,
    Gradient,
    Circular,
    Paint,
}

impl MaskContainer {
    pub const ALL: [MaskContainer; 4] = [MaskContainer::Group, MaskContainer::Gradient, MaskContainer::Circular, MaskContainer::Paint];

    pub fn key(self) -> &'static str {
        match self {
            MaskContainer::Group => "MaskGroupBasedCorrections",
            MaskContainer::Gradient => "GradientBasedCorrections",
            MaskContainer::Circular => "CircularGradientBasedCorrections",
            MaskContainer::Paint => "PaintBasedCorrections",
        }
    }
}

/// The sliders of a local correction, in the UI's units: EV for exposure,
/// −100..100 for the others.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalAdjust {
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub temperature: f32,
    pub tint: f32,
    pub saturation: f32,
    pub texture: f32,
    pub clarity: f32,
    pub dehaze: f32,
    pub sharpness: f32,
}

/// XMP key and the factor from the stored value to the UI value. Stored
/// values are −1..1 (exposure: EV / 4).
const LOCAL_KEYS: [(&str, f32); 13] = [
    ("LocalExposure2012", 4.0),
    ("LocalContrast2012", 100.0),
    ("LocalHighlights2012", 100.0),
    ("LocalShadows2012", 100.0),
    ("LocalWhites2012", 100.0),
    ("LocalBlacks2012", 100.0),
    ("LocalTemperature", 100.0),
    ("LocalTint", 100.0),
    ("LocalSaturation", 100.0),
    ("LocalTexture", 100.0),
    ("LocalClarity2012", 100.0),
    ("LocalDehaze", 100.0),
    ("LocalSharpness", 100.0),
];

impl LocalAdjust {
    fn fields_mut(&mut self) -> [&mut f32; 13] {
        [
            &mut self.exposure,
            &mut self.contrast,
            &mut self.highlights,
            &mut self.shadows,
            &mut self.whites,
            &mut self.blacks,
            &mut self.temperature,
            &mut self.tint,
            &mut self.saturation,
            &mut self.texture,
            &mut self.clarity,
            &mut self.dehaze,
            &mut self.sharpness,
        ]
    }

    pub fn is_zero(&self) -> bool {
        self.clone().fields_mut().iter().all(|v| **v == 0.0)
    }
}

/// How a component combines with the ones before it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BlendMode {
    #[default]
    Add,
    Subtract,
    Intersect,
}

impl BlendMode {
    fn from_xmp(v: Option<f32>) -> BlendMode {
        match v.map(|v| v.round() as i32) {
            Some(1) => BlendMode::Subtract,
            Some(2) => BlendMode::Intersect,
            _ => BlendMode::Add,
        }
    }

    fn to_xmp(self) -> &'static str {
        match self {
            BlendMode::Add => "0",
            BlendMode::Subtract => "1",
            BlendMode::Intersect => "2",
        }
    }
}

/// One brush stroke: dabs painted (or erased) with the same brush.
#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    /// Erasing strokes paint towards 0 (`MaskValue` 0).
    pub erase: bool,
    /// Brush radius, as a fraction of the image's long edge.
    pub radius: f32,
    /// 0..1: how much each dab moves the mask towards its value.
    pub flow: f32,
    /// 0..1: the part of the radius at full strength (1 − feather).
    pub center_weight: f32,
    /// Dab centres, in fractions of the image.
    pub dabs: Vec<(f32, f32)>,
    /// Fields kept as read.
    pub extra: Props,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MaskShape {
    /// Linear gradient: no effect at `zero`, full effect from `full` on.
    Linear { zero: (f32, f32), full: (f32, f32) },
    /// Radial gradient: the ellipse inside (top, left, bottom, right),
    /// turned by `angle` degrees, with a soft edge of `feather` (0..100).
    Radial { top: f32, left: f32, bottom: f32, right: f32, angle: f32, feather: f32, roundness: f32, midpoint: f32 },
    Brush { strokes: Vec<Stroke> },
    /// A kind Lumen does not render (AI, range…): kept as read.
    Other(Props),
}

#[derive(Clone, Debug, PartialEq)]
pub struct MaskComponent {
    pub shape: MaskShape,
    pub mode: BlendMode,
    pub inverted: bool,
    /// Opacity of the component, 0..1 (`MaskValue`).
    pub value: f32,
    pub extra: Props,
}

impl MaskComponent {
    pub fn new(shape: MaskShape) -> MaskComponent {
        MaskComponent { shape, mode: BlendMode::Add, inverted: false, value: 1.0, extra: Props::default() }
    }

    pub fn is_supported(&self) -> bool {
        !matches!(self.shape, MaskShape::Other(_))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocalCorrection {
    pub name: String,
    pub active: bool,
    /// 0..1, the mask's Amount.
    pub amount: f32,
    pub adjust: LocalAdjust,
    pub components: Vec<MaskComponent>,
    pub container: MaskContainer,
    /// Fields kept as read (sync ids, range masks…).
    pub extra: Props,
}

impl LocalCorrection {
    pub fn new(name: impl Into<String>, components: Vec<MaskComponent>) -> LocalCorrection {
        LocalCorrection {
            name: name.into(),
            active: true,
            amount: 1.0,
            adjust: LocalAdjust::default(),
            components,
            container: MaskContainer::Group,
            extra: Props::default(),
        }
    }

    /// Whether Lumen can render every component.
    pub fn is_supported(&self) -> bool {
        self.components.iter().all(MaskComponent::is_supported) && !self.extra.iter().any(|p| p.name == "CorrectionRangeMask" || p.name == "CorrRangeMask")
    }
}

// ---------------------------------------------------------------------------
// Reading

fn f(p: &Props, key: &str) -> Option<f32> {
    p.crs(key).and_then(Value::as_f32)
}

fn text(p: &Props, key: &str) -> Option<String> {
    p.crs(key).and_then(Value::as_text).map(str::to_string)
}

fn b(p: &Props, key: &str) -> Option<bool> {
    p.crs(key).and_then(Value::as_bool)
}

/// The rest of `p` once `known` keys are taken out.
fn rest(p: &Props, known: &[&str]) -> Props {
    Props(p.iter().filter(|x| !(x.ns == NS_CRS && known.contains(&x.name.as_str()))).cloned().collect())
}

/// Items of a value that may be a sequence or a single struct.
fn structs(v: &Value) -> Vec<&Props> {
    match v {
        Value::Struct(p) => vec![p],
        other => other.items().iter().filter_map(Value::as_struct).collect(),
    }
}

/// Every local correction in a settings packet.
pub fn read_corrections(props: &Props) -> Vec<LocalCorrection> {
    let mut out = Vec::new();
    for container in MaskContainer::ALL {
        let Some(v) = props.crs(container.key()) else { continue };
        for c in structs(v) {
            out.push(read_correction(c, container));
        }
    }
    out
}

const CORRECTION_KEYS: &[&str] = &["What", "CorrectionName", "CorrectionActive", "CorrectionAmount", "CorrectionMasks"];
const COMPONENT_KEYS: &[&str] = &["What", "MaskBlendMode", "MaskInverted", "MaskValue", "Flipped"];

fn read_correction(p: &Props, container: MaskContainer) -> LocalCorrection {
    let mut adjust = LocalAdjust::default();
    for ((key, scale), field) in LOCAL_KEYS.iter().zip(adjust.fields_mut()) {
        // Older files use the names without 2012.
        let v = f(p, key).or_else(|| key.strip_suffix("2012").and_then(|k| f(p, k)));
        // Rounded so that /100 and ×100 give back the value written.
        *field = (v.unwrap_or(0.0) * scale * 1e4).round() / 1e4;
    }
    let mut known: Vec<&str> = CORRECTION_KEYS.to_vec();
    known.extend(LOCAL_KEYS.iter().map(|(k, _)| *k));
    let components = read_components(p.crs("CorrectionMasks").map(structs).unwrap_or_default());
    LocalCorrection {
        name: text(p, "CorrectionName").unwrap_or_default(),
        active: b(p, "CorrectionActive").unwrap_or(true),
        amount: f(p, "CorrectionAmount").unwrap_or(1.0),
        adjust,
        components,
        container,
        extra: rest(p, &known),
    }
}

/// Mask components; consecutive top-level paint strokes (the older layout)
/// make one brush component.
fn read_components(items: Vec<&Props>) -> Vec<MaskComponent> {
    let mut out: Vec<MaskComponent> = Vec::new();
    for m in items {
        let what = text(m, "What").unwrap_or_default();
        let mode = BlendMode::from_xmp(f(m, "MaskBlendMode"));
        let inverted = b(m, "MaskInverted").unwrap_or(false);
        match what.as_str() {
            "Mask/Paint" => {
                let stroke = read_stroke(m);
                match out.last_mut() {
                    Some(MaskComponent { shape: MaskShape::Brush { strokes }, extra, .. }) if extra.crs("LumenLoose").is_some() => strokes.push(stroke),
                    _ => {
                        let mut c = MaskComponent::new(MaskShape::Brush { strokes: vec![stroke] });
                        // Marks a brush made of loose strokes, until the
                        // next component.
                        c.extra.set_crs("LumenLoose", Value::Text("1".into()));
                        out.push(c);
                    }
                }
                continue;
            }
            "Mask/Aggregate" => {
                let strokes: Vec<Stroke> = m.crs("Masks").map(structs).unwrap_or_default().into_iter().map(read_stroke).collect();
                let mut c = MaskComponent::new(MaskShape::Brush { strokes });
                c.mode = mode;
                c.inverted = inverted;
                c.value = f(m, "MaskValue").unwrap_or(1.0);
                let mut known = COMPONENT_KEYS.to_vec();
                known.push("Masks");
                c.extra = rest(m, &known);
                out.push(c);
            }
            "Mask/Gradient" => {
                let shape = MaskShape::Linear {
                    zero: (f(m, "ZeroX").unwrap_or(0.5), f(m, "ZeroY").unwrap_or(0.0)),
                    full: (f(m, "FullX").unwrap_or(0.5), f(m, "FullY").unwrap_or(1.0)),
                };
                out.push(component(m, shape, mode, inverted, &["ZeroX", "ZeroY", "FullX", "FullY"]));
            }
            "Mask/CircularGradient" => {
                let shape = MaskShape::Radial {
                    top: f(m, "Top").unwrap_or(0.25),
                    left: f(m, "Left").unwrap_or(0.25),
                    bottom: f(m, "Bottom").unwrap_or(0.75),
                    right: f(m, "Right").unwrap_or(0.75),
                    angle: f(m, "Angle").unwrap_or(0.0),
                    feather: f(m, "Feather").unwrap_or(50.0),
                    roundness: f(m, "Roundness").unwrap_or(0.0),
                    midpoint: f(m, "Midpoint").unwrap_or(50.0),
                };
                // The older radial filter worked outside the ellipse unless
                // "Flipped"; MaskInverted is what newer files use.
                let inverted = match (m.crs("MaskInverted"), b(m, "Flipped")) {
                    (Some(_), _) => inverted,
                    (None, Some(flipped)) => !flipped,
                    (None, None) => false,
                };
                out.push(component(m, shape, mode, inverted, &["Top", "Left", "Bottom", "Right", "Angle", "Feather", "Roundness", "Midpoint"]));
            }
            _ => {
                let mut c = MaskComponent::new(MaskShape::Other(m.clone()));
                c.mode = mode;
                out.push(c);
            }
        }
    }
    for c in &mut out {
        c.extra.remove(NS_CRS, "LumenLoose");
    }
    out
}

fn component(m: &Props, shape: MaskShape, mode: BlendMode, inverted: bool, geometry: &[&str]) -> MaskComponent {
    let mut known = COMPONENT_KEYS.to_vec();
    known.extend_from_slice(geometry);
    MaskComponent { shape, mode, inverted, value: f(m, "MaskValue").unwrap_or(1.0), extra: rest(m, &known) }
}

fn read_stroke(m: &Props) -> Stroke {
    let mut flow = f(m, "Flow").unwrap_or(1.0);
    let mut dabs = Vec::new();
    for item in m.crs("Dabs").map(|v| v.items().to_vec()).unwrap_or_default() {
        let Some(t) = item.as_text() else { continue };
        let words: Vec<&str> = t.split_whitespace().collect();
        match words.as_slice() {
            ["d", x, y, ..] => {
                if let (Ok(x), Ok(y)) = (x.parse(), y.parse()) {
                    dabs.push((x, y));
                }
            }
            // Flow can also come as a leading "f 0.3" item.
            ["f", v, ..] => flow = v.parse().unwrap_or(flow),
            _ => {}
        }
    }
    Stroke {
        erase: f(m, "MaskValue").is_some_and(|v| v < 0.5),
        radius: f(m, "Radius").unwrap_or(0.05),
        flow,
        center_weight: f(m, "CenterWeight").unwrap_or(0.5),
        dabs,
        extra: rest(m, &["What", "MaskValue", "Radius", "Flow", "CenterWeight", "Dabs"]),
    }
}

// ---------------------------------------------------------------------------
// Writing

/// Up to six decimals: mask geometry and the /100 local values need more
/// than the three of the other settings.
fn num(v: f32) -> Value {
    if (v - v.round()).abs() < 1e-6 {
        return Value::Text(fmt_num(v));
    }
    let s = format!("{v:.6}");
    Value::Text(s.trim_end_matches('0').trim_end_matches('.').to_string())
}

fn txt(s: &str) -> Value {
    Value::Text(s.into())
}

/// Writes the corrections back into their containers, replacing what
/// was there.
pub fn write_corrections(corrections: &[LocalCorrection], props: &mut Props) {
    for container in MaskContainer::ALL {
        let items: Vec<Value> = corrections.iter().filter(|c| c.container == container).map(|c| Value::Struct(write_correction(c))).collect();
        if items.is_empty() {
            props.remove(NS_CRS, container.key());
        } else {
            props.set_crs(container.key(), Value::Seq(items));
        }
    }
}

fn write_correction(c: &LocalCorrection) -> Props {
    let mut p = Props::default();
    p.set_crs("What", txt("Correction"));
    if !c.name.is_empty() {
        p.set_crs("CorrectionName", txt(&c.name));
    }
    p.set_crs("CorrectionActive", txt(if c.active { "true" } else { "false" }));
    p.set_crs("CorrectionAmount", num(c.amount));
    let mut a = c.adjust.clone();
    for ((key, scale), v) in LOCAL_KEYS.iter().zip(a.fields_mut()) {
        p.set_crs(key, num(*v / scale));
    }
    for e in c.extra.iter() {
        p.set(&e.ns, &e.name, e.value.clone());
    }
    let masks: Vec<Value> = c.components.iter().flat_map(|m| write_component(m, c.container)).collect();
    p.set_crs("CorrectionMasks", Value::Seq(masks));
    p
}

fn write_component(m: &MaskComponent, container: MaskContainer) -> Vec<Value> {
    let mut p = Props::default();
    let set_common = |p: &mut Props, what: &str| {
        p.set_crs("What", txt(what));
        p.set_crs("MaskValue", num(m.value));
        p.set_crs("MaskBlendMode", txt(m.mode.to_xmp()));
        p.set_crs("MaskInverted", txt(if m.inverted { "true" } else { "false" }));
    };
    match &m.shape {
        MaskShape::Linear { zero, full } => {
            set_common(&mut p, "Mask/Gradient");
            p.set_crs("ZeroX", num(zero.0));
            p.set_crs("ZeroY", num(zero.1));
            p.set_crs("FullX", num(full.0));
            p.set_crs("FullY", num(full.1));
        }
        MaskShape::Radial { top, left, bottom, right, angle, feather, roundness, midpoint } => {
            set_common(&mut p, "Mask/CircularGradient");
            for (k, v) in [("Top", top), ("Left", left), ("Bottom", bottom), ("Right", right), ("Angle", angle), ("Midpoint", midpoint), ("Roundness", roundness), ("Feather", feather)] {
                p.set_crs(k, num(*v));
            }
            // Older readers look at Flipped (effect inside the ellipse).
            p.set_crs("Flipped", txt(if m.inverted { "false" } else { "true" }));
        }
        MaskShape::Brush { strokes } => {
            let paints: Vec<Value> = strokes.iter().map(|s| Value::Struct(write_stroke(s))).collect();
            if container != MaskContainer::Group {
                // The older layout: strokes straight in CorrectionMasks.
                return paints;
            }
            set_common(&mut p, "Mask/Aggregate");
            p.set_crs("Masks", Value::Seq(paints));
        }
        MaskShape::Other(props) => return vec![Value::Struct(props.clone())],
    }
    for e in m.extra.iter() {
        p.set(&e.ns, &e.name, e.value.clone());
    }
    vec![Value::Struct(p)]
}

fn write_stroke(s: &Stroke) -> Props {
    let mut p = Props::default();
    p.set_crs("What", txt("Mask/Paint"));
    p.set_crs("MaskValue", num(if s.erase { 0.0 } else { 1.0 }));
    p.set_crs("Radius", num(s.radius));
    p.set_crs("Flow", num(s.flow));
    p.set_crs("CenterWeight", num(s.center_weight));
    p.set_crs("Dabs", Value::Seq(s.dabs.iter().map(|(x, y)| Value::Text(format!("d {x:.6} {y:.6}"))).collect()));
    for e in s.extra.iter() {
        p.set(&e.ns, &e.name, e.value.clone());
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::XmpPacket;

    fn sample() -> Vec<LocalCorrection> {
        let mut linear = LocalCorrection::new("Cielo", vec![MaskComponent::new(MaskShape::Linear { zero: (0.5, 0.6), full: (0.5, 0.2) })]);
        linear.adjust.exposure = -0.75;
        linear.adjust.temperature = -20.0;
        let mut radial = MaskComponent::new(MaskShape::Radial { top: 0.2, left: 0.3, bottom: 0.7, right: 0.8, angle: 15.0, feather: 60.0, roundness: 0.0, midpoint: 50.0 });
        radial.inverted = true;
        let brush = MaskComponent {
            mode: BlendMode::Subtract,
            ..MaskComponent::new(MaskShape::Brush {
                strokes: vec![
                    Stroke { erase: false, radius: 0.03, flow: 0.8, center_weight: 0.4, dabs: vec![(0.1, 0.2), (0.12, 0.21)], extra: Props::default() },
                    Stroke { erase: true, radius: 0.02, flow: 1.0, center_weight: 0.5, dabs: vec![(0.11, 0.2)], extra: Props::default() },
                ],
            })
        };
        let mut second = LocalCorrection::new("Soggetto", vec![radial, brush]);
        second.amount = 0.6;
        second.adjust.shadows = 30.0;
        second.adjust.clarity = 15.0;
        vec![linear, second]
    }

    #[test]
    fn corrections_round_trip_through_xml() {
        let corrections = sample();
        let mut packet = XmpPacket::default();
        write_corrections(&corrections, &mut packet.props);
        let xml = packet.to_xml();
        assert!(xml.contains("MaskGroupBasedCorrections") && xml.contains("Mask/Aggregate") && xml.contains("d 0.100000 0.200000"));
        let back = read_corrections(&XmpPacket::parse(&xml).unwrap().props);
        assert_eq!(back, corrections);
        // Exposure is stored as EV / 4.
        assert!(xml.contains("crs:LocalExposure2012=\"-0.188\"") || xml.contains("-0.1875"), "{xml}");
    }

    #[test]
    fn unknown_masks_and_fields_are_kept() {
        let xml = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
          <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
           <crs:MaskGroupBasedCorrections><rdf:Seq><rdf:li><rdf:Description crs:What="Correction" crs:CorrectionAmount="1"
               crs:CorrectionActive="true" crs:CorrectionSyncID="ABC" crs:LocalExposure2012="0.25">
             <crs:CorrectionMasks><rdf:Seq><rdf:li><rdf:Description crs:What="Mask/Image" crs:MaskSubType="1" crs:MaskDigest="D1"/></rdf:li></rdf:Seq></crs:CorrectionMasks>
           </rdf:Description></rdf:li></rdf:Seq></crs:MaskGroupBasedCorrections>
           <crs:GradientBasedCorrections><rdf:Seq><rdf:li><rdf:Description crs:What="Correction" crs:CorrectionAmount="1" crs:LocalExposure2012="-0.5">
             <crs:CorrectionMasks><rdf:Seq><rdf:li><rdf:Description crs:What="Mask/Gradient" crs:MaskValue="1" crs:ZeroX="0.5" crs:ZeroY="0.4" crs:FullX="0.5" crs:FullY="0.1"/></rdf:li></rdf:Seq></crs:CorrectionMasks>
           </rdf:Description></rdf:li></rdf:Seq></crs:GradientBasedCorrections>
          </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let props = XmpPacket::parse(xml).unwrap().props;
        let read = read_corrections(&props);
        assert_eq!(read.len(), 2);
        assert!(!read[0].is_supported());
        assert_eq!(read[0].adjust.exposure, 1.0);
        assert_eq!(read[1].container, MaskContainer::Gradient);
        assert_eq!(read[1].adjust.exposure, -2.0);
        assert!(read[1].is_supported());

        let mut out = XmpPacket::default();
        write_corrections(&read, &mut out.props);
        let again = read_corrections(&XmpPacket::parse(&out.to_xml()).unwrap().props);
        assert_eq!(again, read);
        let xml = out.to_xml();
        assert!(xml.contains("Mask/Image") && xml.contains("MaskDigest=\"D1\"") && xml.contains("CorrectionSyncID=\"ABC\""));
        assert!(xml.contains("GradientBasedCorrections"));
    }

    #[test]
    fn older_paint_layout() {
        let xml = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
          <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
           <crs:PaintBasedCorrections><rdf:Seq><rdf:li><rdf:Description crs:What="Correction" crs:CorrectionAmount="1" crs:LocalExposure2012="0.1">
             <crs:CorrectionMasks><rdf:Seq>
              <rdf:li><rdf:Description crs:What="Mask/Paint" crs:MaskValue="1" crs:Radius="0.05" crs:Flow="1" crs:CenterWeight="0">
                <crs:Dabs><rdf:Seq><rdf:li>d 0.1 0.1</rdf:li><rdf:li>d 0.2 0.1</rdf:li></rdf:Seq></crs:Dabs></rdf:Description></rdf:li>
              <rdf:li><rdf:Description crs:What="Mask/Paint" crs:MaskValue="0" crs:Radius="0.02" crs:Flow="1" crs:CenterWeight="0">
                <crs:Dabs><rdf:Seq><rdf:li>d 0.15 0.1</rdf:li></rdf:Seq></crs:Dabs></rdf:Description></rdf:li>
             </rdf:Seq></crs:CorrectionMasks>
           </rdf:Description></rdf:li></rdf:Seq></crs:PaintBasedCorrections>
          </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let read = read_corrections(&XmpPacket::parse(xml).unwrap().props);
        let MaskShape::Brush { strokes } = &read[0].components[0].shape else { panic!("{read:?}") };
        assert_eq!(strokes.len(), 2);
        assert!(!strokes[0].erase && strokes[1].erase);
        assert_eq!(strokes[0].dabs, vec![(0.1, 0.1), (0.2, 0.1)]);
        let mut out = XmpPacket::default();
        write_corrections(&read, &mut out.props);
        assert_eq!(read_corrections(&XmpPacket::parse(&out.to_xml()).unwrap().props), read);
    }
}
