//! Typed view of the Camera Raw develop settings (crs:*).
//!
//! A preset is *partial*: it only overrides the keys it contains, so
//! settings are built as `DevelopSettings::default()` + `apply(preset)`.

use crate::packet::{Props, Value, NS_CRS};
use crate::rgb_table::RgbTable;
use crate::Error;

/// Lightroom's eight HSL / color mixer bands, in XMP key order.
pub const HSL_BANDS: [&str; 8] = ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"];

/// Tone curve points in 0..255 input/output space.
#[derive(Clone, Debug, PartialEq)]
pub struct Curve(pub Vec<(f32, f32)>);

impl Default for Curve {
    fn default() -> Self {
        Curve(vec![(0.0, 0.0), (255.0, 255.0)])
    }
}

impl Curve {
    pub fn is_identity(&self) -> bool {
        self.0.iter().all(|(x, y)| x == y)
    }

    fn from_value(v: &Value) -> Option<Curve> {
        let pts: Vec<(f32, f32)> = v
            .items()
            .iter()
            .filter_map(|item| {
                let (x, y) = item.as_text()?.split_once(',')?;
                Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
            })
            .collect();
        (pts.len() >= 2).then_some(Curve(pts))
    }

    fn to_value(&self) -> Value {
        Value::Seq(self.0.iter().map(|(x, y)| Value::Text(format!("{}, {}", fmt_num(*x), fmt_num(*y)))).collect())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum WhiteBalance {
    AsShot,
    Auto,
    Custom,
    /// Named presets such as "Daylight", "Tungsten"...
    Preset(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Basic {
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Presence {
    pub texture: f32,
    pub clarity: f32,
    pub dehaze: f32,
    pub vibrance: f32,
    pub saturation: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParametricCurve {
    pub shadows: f32,
    pub darks: f32,
    pub lights: f32,
    pub highlights: f32,
    pub shadow_split: f32,
    pub midtone_split: f32,
    pub highlight_split: f32,
}

impl Default for ParametricCurve {
    fn default() -> Self {
        ParametricCurve {
            shadows: 0.0,
            darks: 0.0,
            lights: 0.0,
            highlights: 0.0,
            shadow_split: 25.0,
            midtone_split: 50.0,
            highlight_split: 75.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToneCurves {
    pub name: String,
    pub master: Curve,
    pub red: Curve,
    pub green: Curve,
    pub blue: Curve,
}

impl Default for ToneCurves {
    fn default() -> Self {
        ToneCurves {
            name: "Linear".into(),
            master: Curve::default(),
            red: Curve::default(),
            green: Curve::default(),
            blue: Curve::default(),
        }
    }
}

/// Per-band adjustments, indexed like [`HSL_BANDS`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hsl {
    pub hue: [f32; 8],
    pub saturation: [f32; 8],
    pub luminance: [f32; 8],
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wheel {
    pub hue: f32,
    pub sat: f32,
    pub lum: f32,
}

/// Color grading (PV5+). Shadow/highlight hue and saturation are still
/// stored under the legacy SplitToning* keys.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorGrade {
    pub shadow: Wheel,
    pub midtone: Wheel,
    pub highlight: Wheel,
    pub global: Wheel,
    pub blending: f32,
    pub balance: f32,
}

impl Default for ColorGrade {
    fn default() -> Self {
        ColorGrade {
            shadow: Wheel::default(),
            midtone: Wheel::default(),
            highlight: Wheel::default(),
            global: Wheel::default(),
            blending: 50.0,
            balance: 0.0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Calibration {
    pub shadow_tint: f32,
    pub red_hue: f32,
    pub red_sat: f32,
    pub green_hue: f32,
    pub green_sat: f32,
    pub blue_hue: f32,
    pub blue_sat: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Detail {
    pub sharpness: f32,
    pub sharpen_radius: f32,
    pub sharpen_detail: f32,
    pub sharpen_edge_masking: f32,
    pub luminance_nr: f32,
    pub color_nr: f32,
    pub color_nr_detail: f32,
    pub color_nr_smoothness: f32,
}

impl Default for Detail {
    // Camera Raw defaults for raw files.
    fn default() -> Self {
        Detail {
            sharpness: 40.0,
            sharpen_radius: 1.0,
            sharpen_detail: 25.0,
            sharpen_edge_masking: 0.0,
            luminance_nr: 0.0,
            color_nr: 25.0,
            color_nr_detail: 50.0,
            color_nr_smoothness: 50.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Effects {
    pub grain_amount: f32,
    pub grain_size: f32,
    /// "Roughness" in the UI.
    pub grain_frequency: f32,
    pub vignette_amount: f32,
    pub vignette_midpoint: f32,
    pub vignette_roundness: f32,
    pub vignette_feather: f32,
}

impl Default for Effects {
    fn default() -> Self {
        Effects {
            grain_amount: 0.0,
            grain_size: 25.0,
            grain_frequency: 50.0,
            vignette_amount: 0.0,
            vignette_midpoint: 50.0,
            vignette_roundness: 0.0,
            vignette_feather: 50.0,
        }
    }
}

/// A profile ("Look"): a named bundle of parameters that may carry an RGB
/// table (creative profiles) or reference Adobe-internal tables by hash.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub name: String,
    pub amount: f32,
    pub uuid: Option<String>,
    pub group: Option<String>,
    pub supports_amount: bool,
    /// Built-in profile written by reference only.
    pub stubbed: bool,
    /// Settings the profile applies (camera profile, tone curve, tables).
    pub parameters: Props,
    pub rgb_table: Option<RgbTable>,
}

impl Look {
    fn from_props(p: &Props) -> Result<Look, Error> {
        let parameters = p.crs("Parameters").and_then(Value::as_struct).cloned().unwrap_or_default();
        Ok(Look {
            name: text(p, "Name").unwrap_or_default(),
            amount: p.crs("Amount").and_then(Value::as_f32).unwrap_or(1.0),
            uuid: text(p, "UUID"),
            group: text(p, "Group"),
            supports_amount: p.crs("SupportsAmount").and_then(Value::as_bool).unwrap_or(false),
            stubbed: p.crs("Stubbed").and_then(Value::as_bool).unwrap_or(false),
            rgb_table: find_rgb_table(&parameters)?,
            parameters,
        })
    }

    /// The develop settings carried by the profile itself (e.g. Adobe
    /// Color's own tone curve).
    pub fn parameter_settings(&self) -> DevelopSettings {
        let mut s = DevelopSettings::default();
        s.apply(&self.parameters);
        s
    }

    /// Hash of an Adobe-internal hue/sat look table, if referenced.
    pub fn look_table_ref(&self) -> Option<String> {
        text(&self.parameters, "LookTable")
    }
}

/// `crs:RGBTable` names the md5 of the table; the data sits next to it in
/// `crs:Table_<md5>`.
pub fn find_rgb_table(p: &Props) -> Result<Option<RgbTable>, Error> {
    let Some(md5) = text(p, "RGBTable") else { return Ok(None) };
    let blob = text(p, &format!("Table_{md5}"))
        .ok_or_else(|| Error::Invalid(format!("RGBTable {md5} referenced but Table_{md5} missing")))?;
    RgbTable::from_blob(&blob).map(Some)
}

#[derive(Clone, Debug, PartialEq)]
pub struct DevelopSettings {
    pub process_version: Option<String>,
    pub white_balance: WhiteBalance,
    /// Absolute Kelvin / tint, meaningful for raw files only.
    pub temperature: Option<f32>,
    pub tint: Option<f32>,
    /// Relative WB used for JPEG/TIFF sources.
    pub incremental_temperature: f32,
    pub incremental_tint: f32,
    pub basic: Basic,
    pub presence: Presence,
    pub parametric: ParametricCurve,
    pub curves: ToneCurves,
    pub hsl: Hsl,
    pub color_grade: ColorGrade,
    pub calibration: Calibration,
    pub detail: Detail,
    pub effects: Effects,
    pub look: Option<Look>,
    /// Profile-level RGB table when the preset *is* a profile.
    pub rgb_table: Option<RgbTable>,
}

impl Default for DevelopSettings {
    fn default() -> Self {
        DevelopSettings {
            process_version: None,
            white_balance: WhiteBalance::AsShot,
            temperature: None,
            tint: None,
            incremental_temperature: 0.0,
            incremental_tint: 0.0,
            basic: Basic::default(),
            presence: Presence::default(),
            parametric: ParametricCurve::default(),
            curves: ToneCurves::default(),
            hsl: Hsl::default(),
            color_grade: ColorGrade::default(),
            calibration: Calibration::default(),
            detail: Detail::default(),
            effects: Effects::default(),
            look: None,
            rgb_table: None,
        }
    }
}

/// Every plain numeric setting with its XMP key. One table drives
/// reading, writing and the dump tool, so they cannot drift apart.
macro_rules! scalar_keys {
    ($($key:literal => $($path:ident).+;)*) => {
        fn scalars_mut(s: &mut DevelopSettings) -> Vec<(&'static str, &mut f32)> {
            vec![$(($key, &mut s.$($path).+),)*]
        }
        fn scalars(s: &DevelopSettings) -> Vec<(&'static str, f32)> {
            vec![$(($key, s.$($path).+),)*]
        }
    };
}

scalar_keys! {
    "IncrementalTemperature" => incremental_temperature;
    "IncrementalTint" => incremental_tint;
    "Exposure2012" => basic.exposure;
    "Contrast2012" => basic.contrast;
    "Highlights2012" => basic.highlights;
    "Shadows2012" => basic.shadows;
    "Whites2012" => basic.whites;
    "Blacks2012" => basic.blacks;
    "Texture" => presence.texture;
    "Clarity2012" => presence.clarity;
    "Dehaze" => presence.dehaze;
    "Vibrance" => presence.vibrance;
    "Saturation" => presence.saturation;
    "ParametricShadows" => parametric.shadows;
    "ParametricDarks" => parametric.darks;
    "ParametricLights" => parametric.lights;
    "ParametricHighlights" => parametric.highlights;
    "ParametricShadowSplit" => parametric.shadow_split;
    "ParametricMidtoneSplit" => parametric.midtone_split;
    "ParametricHighlightSplit" => parametric.highlight_split;
    "SplitToningShadowHue" => color_grade.shadow.hue;
    "SplitToningShadowSaturation" => color_grade.shadow.sat;
    "SplitToningHighlightHue" => color_grade.highlight.hue;
    "SplitToningHighlightSaturation" => color_grade.highlight.sat;
    "SplitToningBalance" => color_grade.balance;
    "ColorGradeShadowLum" => color_grade.shadow.lum;
    "ColorGradeMidtoneHue" => color_grade.midtone.hue;
    "ColorGradeMidtoneSat" => color_grade.midtone.sat;
    "ColorGradeMidtoneLum" => color_grade.midtone.lum;
    "ColorGradeHighlightLum" => color_grade.highlight.lum;
    "ColorGradeGlobalHue" => color_grade.global.hue;
    "ColorGradeGlobalSat" => color_grade.global.sat;
    "ColorGradeGlobalLum" => color_grade.global.lum;
    "ColorGradeBlending" => color_grade.blending;
    "ShadowTint" => calibration.shadow_tint;
    "RedHue" => calibration.red_hue;
    "RedSaturation" => calibration.red_sat;
    "GreenHue" => calibration.green_hue;
    "GreenSaturation" => calibration.green_sat;
    "BlueHue" => calibration.blue_hue;
    "BlueSaturation" => calibration.blue_sat;
    "Sharpness" => detail.sharpness;
    "SharpenRadius" => detail.sharpen_radius;
    "SharpenDetail" => detail.sharpen_detail;
    "SharpenEdgeMasking" => detail.sharpen_edge_masking;
    "LuminanceSmoothing" => detail.luminance_nr;
    "ColorNoiseReduction" => detail.color_nr;
    "ColorNoiseReductionDetail" => detail.color_nr_detail;
    "ColorNoiseReductionSmoothness" => detail.color_nr_smoothness;
    "GrainAmount" => effects.grain_amount;
    "GrainSize" => effects.grain_size;
    "GrainFrequency" => effects.grain_frequency;
    "PostCropVignetteAmount" => effects.vignette_amount;
    "PostCropVignetteMidpoint" => effects.vignette_midpoint;
    "PostCropVignetteRoundness" => effects.vignette_roundness;
    "PostCropVignetteFeather" => effects.vignette_feather;
}

const CURVE_KEYS: [&str; 4] = ["ToneCurvePV2012", "ToneCurvePV2012Red", "ToneCurvePV2012Green", "ToneCurvePV2012Blue"];

impl DevelopSettings {
    pub fn from_props(props: &Props) -> Result<Self, Error> {
        let mut s = Self::default();
        s.try_apply(props)?;
        Ok(s)
    }

    /// Like [`try_apply`](Self::try_apply), but an undecodable embedded
    /// table is dropped instead of failing the whole preset.
    pub fn apply(&mut self, props: &Props) {
        if self.try_apply(props).is_err() {
            self.apply_without_tables(props);
        }
    }

    /// Overrides only the settings present in `props`.
    pub fn try_apply(&mut self, props: &Props) -> Result<(), Error> {
        self.apply_without_tables(props);
        if let Some(look) = props.crs("Look").and_then(Value::as_struct) {
            self.look = Some(Look::from_props(look)?);
        }
        if let Some(table) = find_rgb_table(props)? {
            self.rgb_table = Some(table);
        }
        Ok(())
    }

    fn apply_without_tables(&mut self, props: &Props) {
        for (key, field) in scalars_mut(self) {
            if let Some(v) = props.crs(key).and_then(Value::as_f32) {
                *field = v;
            }
        }
        for (i, band) in HSL_BANDS.iter().enumerate() {
            for (prefix, arr) in [
                ("HueAdjustment", &mut self.hsl.hue),
                ("SaturationAdjustment", &mut self.hsl.saturation),
                ("LuminanceAdjustment", &mut self.hsl.luminance),
            ] {
                if let Some(v) = props.crs(&format!("{prefix}{band}")).and_then(Value::as_f32) {
                    arr[i] = v;
                }
            }
        }
        let curves = &mut self.curves;
        for (key, curve) in CURVE_KEYS.iter().zip([&mut curves.master, &mut curves.red, &mut curves.green, &mut curves.blue]) {
            if let Some(c) = props.crs(key).and_then(Curve::from_value) {
                *curve = c;
            }
        }
        if let Some(name) = text(props, "ToneCurveName2012") {
            curves.name = name;
        }
        if let Some(pv) = text(props, "ProcessVersion") {
            self.process_version = Some(pv);
        }
        if let Some(wb) = text(props, "WhiteBalance") {
            self.white_balance = match wb.as_str() {
                "As Shot" => WhiteBalance::AsShot,
                "Auto" => WhiteBalance::Auto,
                "Custom" => WhiteBalance::Custom,
                other => WhiteBalance::Preset(other.into()),
            };
        }
        if let Some(t) = props.crs("Temperature").and_then(Value::as_f32) {
            self.temperature = Some(t);
        }
        if let Some(t) = props.crs("Tint").and_then(Value::as_f32) {
            self.tint = Some(t);
        }
    }

    /// Writes every typed setting into `props` (sidecar style: complete,
    /// not partial). Unknown keys already in `props` are left untouched.
    pub fn write_into(&self, props: &mut Props) {
        if let Some(pv) = &self.process_version {
            props.set_crs("ProcessVersion", Value::Text(pv.clone()));
        }
        let wb = match &self.white_balance {
            WhiteBalance::AsShot => "As Shot",
            WhiteBalance::Auto => "Auto",
            WhiteBalance::Custom => "Custom",
            WhiteBalance::Preset(p) => p,
        };
        props.set_crs("WhiteBalance", Value::Text(wb.into()));
        if let Some(t) = self.temperature {
            props.set_crs("Temperature", num(t));
        }
        if let Some(t) = self.tint {
            props.set_crs("Tint", num(t));
        }
        for (key, v) in scalars(self) {
            props.set_crs(key, num(v));
        }
        for (i, band) in HSL_BANDS.iter().enumerate() {
            props.set_crs(&format!("HueAdjustment{band}"), num(self.hsl.hue[i]));
            props.set_crs(&format!("SaturationAdjustment{band}"), num(self.hsl.saturation[i]));
            props.set_crs(&format!("LuminanceAdjustment{band}"), num(self.hsl.luminance[i]));
        }
        props.set_crs("ToneCurveName2012", Value::Text(self.curves.name.clone()));
        let c = &self.curves;
        for (key, curve) in CURVE_KEYS.iter().zip([&c.master, &c.red, &c.green, &c.blue]) {
            props.set_crs(key, curve.to_value());
        }
        // Look and tables are kept verbatim from the source props: they
        // are opaque to the writer for now.
    }

    /// Whether a top-level crs key is understood by [`DevelopSettings`]
    /// (or is preset metadata). Used to report what is still unmapped.
    pub fn is_known_key(key: &str) -> bool {
        const OTHER: &[&str] = &[
            "ProcessVersion", "WhiteBalance", "Temperature", "Tint", "ToneCurveName2012", "Look", "RGBTable",
            "Name", "ShortName", "SortName", "Group", "Description", "UUID", "PresetType", "Cluster", "Version",
            "HasSettings", "Copyright", "ContactInfo", "CameraModelRestriction", "SupportsAmount", "SupportsAmount2",
            "SupportsColor", "SupportsMonochrome", "SupportsHighDynamicRange", "SupportsNormalDynamicRange",
            "SupportsSceneReferred", "SupportsOutputReferred", "RequiresRGBTables",
        ];
        let hsl = ["HueAdjustment", "SaturationAdjustment", "LuminanceAdjustment"]
            .iter()
            .any(|p| key.strip_prefix(p).is_some_and(|band| HSL_BANDS.contains(&band)));
        hsl || OTHER.contains(&key)
            || CURVE_KEYS.contains(&key)
            || key.starts_with("Table_")
            || scalars(&DevelopSettings::default()).iter().any(|(k, _)| *k == key)
    }

    /// Settings grouped as in Lightroom's develop panels, for display.
    pub fn panels(&self) -> Vec<(&'static str, Vec<(String, String)>)> {
        let all: Vec<(&str, f32)> = scalars(self);
        let pick = |keys: &[&str]| -> Vec<(String, String)> {
            keys.iter()
                .filter_map(|k| all.iter().find(|(n, _)| n == k))
                .map(|(k, v)| (k.to_string(), fmt_num(*v)))
                .collect()
        };

        let mut wb = vec![("WhiteBalance".to_string(), format!("{:?}", self.white_balance))];
        if let Some(t) = self.temperature {
            wb.push(("Temperature".into(), fmt_num(t)));
        }
        if let Some(t) = self.tint {
            wb.push(("Tint".into(), fmt_num(t)));
        }
        let mut basic = wb;
        basic.extend(pick(&["Exposure2012", "Contrast2012", "Highlights2012", "Shadows2012", "Whites2012", "Blacks2012"]));
        basic.extend(pick(&["Texture", "Clarity2012", "Dehaze", "Vibrance", "Saturation"]));

        let c = &self.curves;
        let mut curves = vec![("ToneCurveName2012".to_string(), c.name.clone())];
        for (name, curve) in [("Master", &c.master), ("Red", &c.red), ("Green", &c.green), ("Blue", &c.blue)] {
            curves.push((name.into(), fmt_curve(curve)));
        }
        curves.extend(pick(&[
            "ParametricShadows",
            "ParametricDarks",
            "ParametricLights",
            "ParametricHighlights",
            "ParametricShadowSplit",
            "ParametricMidtoneSplit",
            "ParametricHighlightSplit",
        ]));

        let hsl = HSL_BANDS
            .iter()
            .enumerate()
            .map(|(i, b)| {
                (b.to_string(), format!("H {:>4}  S {:>4}  L {:>4}", fmt_num(self.hsl.hue[i]), fmt_num(self.hsl.saturation[i]), fmt_num(self.hsl.luminance[i])))
            })
            .collect();

        let g = &self.color_grade;
        let wheel = |w: &Wheel| format!("hue {:>3}  sat {:>3}  lum {:>3}", fmt_num(w.hue), fmt_num(w.sat), fmt_num(w.lum));
        let grading = vec![
            ("Shadows".into(), wheel(&g.shadow)),
            ("Midtones".into(), wheel(&g.midtone)),
            ("Highlights".into(), wheel(&g.highlight)),
            ("Global".into(), wheel(&g.global)),
            ("Blending".into(), fmt_num(g.blending)),
            ("Balance".into(), fmt_num(g.balance)),
        ];

        let mut profile = Vec::new();
        if let Some(look) = &self.look {
            profile.push(("Look".into(), format!("{} (amount {}{})", look.name, fmt_num(look.amount), if look.stubbed { ", stubbed" } else { "" })));
            if let Some(p) = text(&look.parameters, "CameraProfile") {
                profile.push(("  CameraProfile".into(), p));
            }
            if let Some(h) = look.look_table_ref() {
                profile.push(("  LookTable (Adobe-internal)".into(), h));
            }
            let ps = look.parameter_settings();
            if !ps.curves.master.is_identity() {
                profile.push(("  ToneCurve".into(), fmt_curve(&ps.curves.master)));
            }
            if let Some(t) = &look.rgb_table {
                profile.push(("  RGBTable".into(), fmt_table(t)));
            }
        }
        if let Some(t) = &self.rgb_table {
            profile.push(("RGBTable".into(), fmt_table(t)));
        }

        vec![
            ("Basic", basic),
            ("Tone curve", curves),
            ("HSL / Color mixer", hsl),
            ("Color grading", grading),
            ("Detail", pick(&["Sharpness", "SharpenRadius", "SharpenDetail", "SharpenEdgeMasking", "LuminanceSmoothing", "ColorNoiseReduction", "ColorNoiseReductionDetail", "ColorNoiseReductionSmoothness"])),
            ("Effects", pick(&["GrainAmount", "GrainSize", "GrainFrequency", "PostCropVignetteAmount", "PostCropVignetteMidpoint", "PostCropVignetteRoundness", "PostCropVignetteFeather"])),
            ("Calibration", pick(&["ShadowTint", "RedHue", "RedSaturation", "GreenHue", "GreenSaturation", "BlueHue", "BlueSaturation"])),
            ("Profile", profile),
        ]
    }
}

fn text(p: &Props, key: &str) -> Option<String> {
    p.get(NS_CRS, key).and_then(Value::as_text).map(str::to_string)
}

fn num(v: f32) -> Value {
    Value::Text(fmt_num(v))
}

pub fn fmt_num(v: f32) -> String {
    if (v - v.round()).abs() < 1e-4 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{v:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn fmt_curve(c: &Curve) -> String {
    if c.is_identity() {
        return "linear".into();
    }
    c.0.iter().map(|(x, y)| format!("({},{})", fmt_num(*x), fmt_num(*y))).collect::<Vec<_>>().join(" ")
}

fn fmt_table(t: &RgbTable) -> String {
    format!(
        "{}D x {} divisions, {:?} primaries, {:?} gamma, gamut {:?}, amount {}..{}",
        t.dimensions, t.divisions, t.primaries, t.gamma, t.gamut, t.min_amount, t.max_amount
    )
}
