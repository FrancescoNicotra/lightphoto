//! The develop panels on the right: Base, Curve, HSL, Color grading,
//! Detail, Effects, Calibration, Profile/LUT.

use eframe::egui::{self, Color32, RichText, Ui};
use pipeline::tone::{self, Lut1};
use pipeline::SourceKind;
use xmp::{Curve, WhiteBalance, Wheel, NS_CRS};

use crate::i18n::{tr, trf};
use crate::state::Edit;
use crate::theme;
use crate::widgets::{color_wheel, curve_editor, hue_color, Mapping, Slider};
use crate::worker::Histogram;

/// What the panels need to know about the photo.
pub struct PanelInput<'a> {
    pub kind: SourceKind,
    /// White balance in effect for raw files (temperature, tint).
    pub white_balance: Option<(f32, f32)>,
    pub histogram: Option<&'a Histogram>,
    /// The camera's white balance, for the preset amount.
    pub as_shot: Option<(f32, f32)>,
}

/// Panel choices that are not part of the edit.
#[derive(Default)]
pub struct PanelState {
    pub curve_tab: usize,
    pub hsl_tab: usize,
    pub grading_tab: usize,
    /// Opens this panel and closes the others (scripted runs).
    pub force_open: Option<String>,
}

pub enum PanelAction {
    LoadLut,
    ResetAll,
}

/// HSL bands: English and Italian name, centre hue.
const BANDS: [(&str, &str, f32); 8] = [
    ("Red", "Rosso", 0.0),
    ("Orange", "Arancione", 30.0),
    ("Yellow", "Giallo", 60.0),
    ("Green", "Verde", 120.0),
    ("Aqua", "Acqua", 180.0),
    ("Blue", "Blu", 235.0),
    ("Purple", "Viola", 270.0),
    ("Magenta", "Magenta", 310.0),
];

/// A collapsible panel. `key` is its stable name (ids, scripts), `title`
/// what is shown.
fn section(ui: &mut Ui, key: &str, title: &str, open: bool, force: Option<&str>, add: impl FnOnce(&mut Ui)) {
    egui::CollapsingHeader::new(RichText::new(title).strong().size(13.5))
        .id_salt(key)
        .default_open(open)
        .open(force.map(|f| f == key))
        .show(ui, |ui| {
            ui.add_space(2.0);
            add(ui);
            ui.add_space(4.0);
        });
    ui.separator();
}

fn subheading(ui: &mut Ui, text: &str) {
    ui.add_space(3.0);
    ui.label(RichText::new(text).color(theme::TEXT_DIM).size(11.5));
}

fn tabs(ui: &mut Ui, current: &mut usize, names: &[&str]) {
    ui.horizontal(|ui| {
        for (i, n) in names.iter().enumerate() {
            ui.selectable_value(current, i, *n);
        }
    });
    ui.add_space(2.0);
}

pub fn develop_panels(ui: &mut Ui, edit: &mut Edit, input: &PanelInput, st: &mut PanelState) -> Vec<PanelAction> {
    let mut actions = Vec::new();
    ui.horizontal(|ui| {
        ui.label(RichText::new(tr("Develop", "Sviluppo")).strong().size(15.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(tr("Reset", "Reimposta")).on_hover_text(tr("Reset every setting to its default", "Riporta tutte le impostazioni ai valori predefiniti")).clicked() {
                actions.push(PanelAction::ResetAll);
            }
        });
    });
    ui.separator();

    preset_amount(ui, edit, input);
    let force = st.force_open.take();
    let f = force.as_deref();
    section(ui, "profile", tr("Profile & LUT", "Profilo e LUT"), true, f, |ui| profile(ui, edit, &mut actions));
    section(ui, "basic", tr("Basic", "Base"), true, f, |ui| basic(ui, edit, input));
    section(ui, "curve", tr("Tone curve", "Curva di viraggio"), false, f, |ui| curve(ui, edit, input, &mut st.curve_tab));
    section(ui, "hsl", tr("HSL / Color", "HSL / Colore"), false, f, |ui| hsl(ui, edit, &mut st.hsl_tab));
    section(ui, "grading", "Color grading", false, f, |ui| grading(ui, edit, &mut st.grading_tab));
    section(ui, "detail", tr("Detail", "Dettaglio"), false, f, |ui| detail(ui, edit));
    section(ui, "effects", tr("Effects", "Effetti"), false, f, |ui| effects(ui, edit));
    section(ui, "calibration", tr("Calibration", "Calibrazione"), false, f, |ui| calibration(ui, edit));
    actions
}

/// Strength of the preset just applied, while it can still be changed.
fn preset_amount(ui: &mut Ui, edit: &mut Edit, input: &PanelInput) {
    let Some(p) = &edit.preset else { return };
    let mut amount = p.amount;
    let name = p.name.clone();
    egui::Frame::new().fill(theme::PANEL_DARK).corner_radius(4).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Preset").color(theme::TEXT_DIM));
            ui.label(RichText::new(&name).strong());
        });
        let r = Slider::new(tr("Amount", "Quantità"), 0.0..=200.0, 100.0).suffix("%").show(ui, &mut amount);
        if r.changed() {
            edit.set_preset_amount(amount, input.as_shot);
        }
        r.on_hover_text(tr("Strength of the preset: 0% = as before the preset, 100% = as the preset. Available until another setting is changed", "Intensità del preset: 0% = prima del preset, 100% = come il preset. Resta finché non si modifica altro"));
    });
    ui.add_space(4.0);
}

fn profile(ui: &mut Ui, edit: &mut Edit, actions: &mut Vec<PanelAction>) {
    let s = &mut edit.settings;
    let mut remove_look = false;
    match &mut s.look {
        Some(look) => {
            ui.horizontal(|ui| {
                ui.label(tr("Profile", "Profilo"));
                ui.label(RichText::new(&look.name).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    remove_look = ui.small_button("×").on_hover_text(tr("Back to the default profile", "Torna al profilo predefinito")).clicked();
                });
            });
            let mut amount = look.amount * 100.0;
            if Slider::new(tr("Amount", "Fattore"), 0.0..=200.0, 100.0).suffix("%").show(ui, &mut amount).changed() {
                look.amount = amount / 100.0;
            }
        }
        None => {
            ui.horizontal(|ui| {
                ui.label(tr("Profile", "Profilo"));
                ui.label(RichText::new(tr("Default", "Predefinito")).color(theme::TEXT_DIM))
                    .on_hover_text(tr("Adobe Standard when Lightroom is installed, otherwise the file's own matrices and the ACR3 curve", "Adobe Standard se Lightroom è installato, altrimenti le matrici del file e la curva ACR3"));
            });
        }
    }
    if remove_look {
        s.look = None;
        edit.opaque.remove(NS_CRS, "Look");
    }

    ui.add_space(4.0);
    let mut remove_lut = false;
    ui.horizontal(|ui| {
        ui.label("LUT");
        match &edit.lut {
            Some(l) => {
                ui.label(RichText::new(&l.name).strong()).on_hover_text(l.path.display().to_string());
            }
            None => {
                ui.label(RichText::new(tr("none", "nessuna")).color(theme::TEXT_DIM));
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if edit.lut.is_some() {
                remove_lut = ui.small_button("×").on_hover_text(tr("Remove the LUT", "Rimuovi la LUT")).clicked();
            }
            if ui.small_button(tr("Load…", "Carica…")).on_hover_text(tr("Load a .cube or .3dl LUT", "Carica una LUT .cube o .3dl")).clicked() {
                actions.push(PanelAction::LoadLut);
            }
        });
    });
    if edit.lut.is_some() {
        Slider::new(tr("LUT amount", "Fattore LUT"), 0.0..=100.0, 100.0).suffix("%").show(ui, &mut edit.lut_amount);
    }
    if remove_lut {
        edit.lut = None;
    }
}

const TEMP_COLORS: [Color32; 2] = [Color32::from_rgb(60, 110, 220), Color32::from_rgb(225, 200, 70)];
const TINT_COLORS: [Color32; 2] = [Color32::from_rgb(70, 180, 70), Color32::from_rgb(210, 70, 200)];

fn basic(ui: &mut Ui, edit: &mut Edit, input: &PanelInput) {
    let s = &mut edit.settings;
    if input.kind == SourceKind::Raw {
        let names: [(&str, WhiteBalance); 9] = [
            (tr("As Shot", "Come scattato"), WhiteBalance::AsShot),
            (tr("Auto", "Automatico"), WhiteBalance::Auto),
            (tr("Daylight", "Luce diurna"), WhiteBalance::Preset("Daylight".into())),
            (tr("Cloudy", "Nuvoloso"), WhiteBalance::Preset("Cloudy".into())),
            (tr("Shade", "Ombra"), WhiteBalance::Preset("Shade".into())),
            (tr("Tungsten", "Tungsteno"), WhiteBalance::Preset("Tungsten".into())),
            (tr("Fluorescent", "Fluorescente"), WhiteBalance::Preset("Fluorescent".into())),
            ("Flash", WhiteBalance::Preset("Flash".into())),
            (tr("Custom", "Personalizzato"), WhiteBalance::Custom),
        ];
        let current = names.iter().find(|(_, wb)| *wb == s.white_balance).map_or(tr("Custom", "Personalizzato"), |(n, _)| n);
        ui.horizontal(|ui| {
            ui.label(tr("White balance", "Bil. bianco"));
            egui::ComboBox::from_id_salt("wb").selected_text(current).show_ui(ui, |ui| {
                for (name, wb) in names {
                    ui.selectable_value(&mut s.white_balance, wb, name);
                }
            });
        });
        let (eff_t, eff_tint) = input.white_balance.unwrap_or((5500.0, 0.0));
        let custom = s.white_balance == WhiteBalance::Custom;
        let mut t = if custom { s.temperature.unwrap_or(eff_t) } else { eff_t.round() };
        let mut tint = if custom { s.tint.unwrap_or(eff_tint) } else { eff_tint.round() };
        let rt = Slider::new("Temp.", 2000.0..=50000.0, eff_t.round())
            .mapping(Mapping::Reciprocal)
            .colors(&TEMP_COLORS)
            .suffix(" K")
            .show(ui, &mut t);
        let rn = Slider::new(tr("Tint", "Tinta"), -150.0..=150.0, eff_tint.round()).colors(&TINT_COLORS).show(ui, &mut tint);
        if rt.changed() || rn.changed() {
            s.white_balance = WhiteBalance::Custom;
            s.temperature = Some(t);
            s.tint = Some(tint);
        }
        // Double-click on Temp/Tint returns to the camera's white balance.
        if (rt.double_clicked() || rn.double_clicked()) && custom {
            s.white_balance = WhiteBalance::AsShot;
            s.temperature = None;
            s.tint = None;
        }
    } else {
        Slider::new("Temp.", -100.0..=100.0, 0.0).colors(&TEMP_COLORS).show(ui, &mut s.incremental_temperature);
        Slider::new(tr("Tint", "Tinta"), -100.0..=100.0, 0.0).colors(&TINT_COLORS).show(ui, &mut s.incremental_tint);
    }

    subheading(ui, tr("Tone", "Tono"));
    let b = &mut s.basic;
    Slider::new(tr("Exposure", "Esposizione"), -5.0..=5.0, 0.0).decimals(2).show(ui, &mut b.exposure);
    Slider::new(tr("Contrast", "Contrasto"), -100.0..=100.0, 0.0).show(ui, &mut b.contrast);
    Slider::new(tr("Highlights", "Luci"), -100.0..=100.0, 0.0).show(ui, &mut b.highlights);
    Slider::new(tr("Shadows", "Ombre"), -100.0..=100.0, 0.0).show(ui, &mut b.shadows);
    Slider::new(tr("Whites", "Bianchi"), -100.0..=100.0, 0.0).show(ui, &mut b.whites);
    Slider::new(tr("Blacks", "Neri"), -100.0..=100.0, 0.0).show(ui, &mut b.blacks);

    subheading(ui, tr("Presence", "Presenza"));
    let p = &mut s.presence;
    Slider::new("Texture", -100.0..=100.0, 0.0).show(ui, &mut p.texture);
    Slider::new(tr("Clarity", "Chiarezza"), -100.0..=100.0, 0.0).show(ui, &mut p.clarity);
    Slider::new(tr("Dehaze", "Foschia"), -100.0..=100.0, 0.0).show(ui, &mut p.dehaze);
    let sat_colors = [Color32::from_gray(128), Color32::from_rgb(220, 60, 160)];
    Slider::new(tr("Vibrance", "Vividezza"), -100.0..=100.0, 0.0).colors(&sat_colors).show(ui, &mut p.vibrance);
    Slider::new(tr("Saturation", "Saturazione"), -100.0..=100.0, 0.0).colors(&sat_colors).show(ui, &mut p.saturation);
}

fn curve(ui: &mut Ui, edit: &mut Edit, input: &PanelInput, tab: &mut usize) {
    tabs(ui, tab, &[tr("Parametric", "Parametrica"), tr("Point", "Punti"), "R", "G", "B"]);
    let s = &mut edit.settings;
    let parametric = tone::parametric(&s.parametric);
    let luma_hist = input.histogram.map(|h| {
        let mut l = [0u32; 256];
        for (i, v) in l.iter_mut().enumerate() {
            *v = (h[0][i] + h[1][i] + h[2][i]) / 3;
        }
        l
    });
    let channel = |c: usize| input.histogram.map(|h| h[c]);
    match *tab {
        0 => {
            // Read-only view of parametric + point curve; edited with sliders.
            let mut shown = s.curves.master.clone();
            curve_view(ui, &mut shown, &parametric, luma_hist.as_ref());
            let p = &mut s.parametric;
            Slider::new(tr("Highlights", "Luci"), -100.0..=100.0, 0.0).show(ui, &mut p.highlights);
            Slider::new(tr("Lights", "Chiari"), -100.0..=100.0, 0.0).show(ui, &mut p.lights);
            Slider::new(tr("Darks", "Scuri"), -100.0..=100.0, 0.0).show(ui, &mut p.darks);
            Slider::new(tr("Shadows", "Ombre"), -100.0..=100.0, 0.0).show(ui, &mut p.shadows);
            subheading(ui, tr("Splits", "Divisioni"));
            let (a, b, c) = (p.shadow_split, p.midtone_split, p.highlight_split);
            ui.push_id("splits", |ui| {
                Slider::new(tr("Shadows", "Ombre"), 5.0..=(b - 5.0).max(5.0), 25.0).show(ui, &mut p.shadow_split);
                Slider::new(tr("Midtones", "Mezzitoni"), (a + 5.0)..=(c - 5.0).max(a + 5.0), 50.0).show(ui, &mut p.midtone_split);
                Slider::new(tr("Highlights", "Luci"), (b + 5.0).min(95.0)..=95.0, 75.0).show(ui, &mut p.highlight_split);
            });
        }
        1 => {
            curve_editor(ui, &mut s.curves.master, &parametric, theme::TEXT, luma_hist.as_ref());
            curve_footer(ui, &mut s.curves.master);
        }
        n => {
            let (curve, color, hist) = match n {
                2 => (&mut s.curves.red, Color32::from_rgb(235, 80, 80), channel(0)),
                3 => (&mut s.curves.green, Color32::from_rgb(80, 210, 100), channel(1)),
                _ => (&mut s.curves.blue, Color32::from_rgb(90, 140, 245), channel(2)),
            };
            curve_editor(ui, curve, &Lut1::identity(), color, hist.as_ref());
            curve_footer(ui, curve);
        }
    }
    if s.curves.master.is_identity() && s.curves.red.is_identity() && s.curves.green.is_identity() && s.curves.blue.is_identity() {
        s.curves.name = "Linear".into();
    } else if s.curves.name == "Linear" {
        s.curves.name = "Custom".into();
    }
}

fn curve_view(ui: &mut Ui, curve: &mut Curve, base: &Lut1, hist: Option<&[u32; 256]>) {
    ui.add_enabled_ui(false, |ui| {
        curve_editor(ui, curve, base, theme::TEXT, hist);
    });
}

fn curve_footer(ui: &mut Ui, curve: &mut Curve) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(tr("Click: add · drag · double-click: remove", "Clic: aggiungi · trascina · doppio clic: rimuovi")).color(theme::TEXT_DIM).size(11.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button(tr("Linear", "Lineare")).clicked() {
                *curve = Curve::default();
            }
        });
    });
}

fn hsl(ui: &mut Ui, edit: &mut Edit, tab: &mut usize) {
    tabs(ui, tab, &[tr("Hue", "Tonalità"), tr("Saturation", "Saturazione"), tr("Luminance", "Luminanza")]);
    let h = &mut edit.settings.hsl;
    let values = match *tab {
        0 => &mut h.hue,
        1 => &mut h.saturation,
        _ => &mut h.luminance,
    };
    ui.push_id(*tab, |ui| {
        for (i, (en, it, deg)) in BANDS.iter().enumerate() {
            let name = tr(en, it);
            let colors: Vec<Color32> = match *tab {
                0 => vec![hue_color(deg - 35.0, 0.8), hue_color(*deg, 0.8), hue_color(deg + 35.0, 0.8)],
                1 => vec![hue_color(*deg, 0.0), hue_color(*deg, 0.9)],
                _ => vec![Color32::from_gray(20), hue_color(*deg, 0.8), Color32::from_gray(235)],
            };
            Slider::new(name, -100.0..=100.0, 0.0).colors(&colors).show(ui, &mut values[i]);
        }
    });
}

fn grading(ui: &mut Ui, edit: &mut Edit, tab: &mut usize) {
    tabs(ui, tab, &[tr("3-way", "3 vie"), tr("Shadows", "Ombre"), tr("Midtones", "Mezzitoni"), tr("Highlights", "Luci"), tr("Global", "Globale")]);
    let g = &mut edit.settings.color_grade;
    if *tab == 0 {
        let size = ((ui.available_width() - 16.0) / 3.0).min(96.0);
        ui.columns(3, |cols| {
            for (col, (name, wheel)) in cols.iter_mut().zip([(tr("Shadows", "Ombre"), &mut g.shadow), (tr("Midtones", "Mezzitoni"), &mut g.midtone), (tr("Highlights", "Luci"), &mut g.highlight)]) {
                col.vertical_centered(|ui| {
                    ui.label(RichText::new(name).size(11.5));
                    ui.push_id(name, |ui| wheel_with_tooltip(ui, wheel, size));
                });
            }
        });
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(tr("Global", "Globale")).size(11.5));
            ui.push_id("global", |ui| wheel_with_tooltip(ui, &mut g.global, size));
        });
        ui.push_id("lums", |ui| {
            Slider::new(tr("Shadows lum.", "Lum. ombre"), -100.0..=100.0, 0.0).show(ui, &mut g.shadow.lum);
            Slider::new(tr("Midtones lum.", "Lum. mezzitoni"), -100.0..=100.0, 0.0).show(ui, &mut g.midtone.lum);
            Slider::new(tr("Highlights lum.", "Lum. luci"), -100.0..=100.0, 0.0).show(ui, &mut g.highlight.lum);
            Slider::new(tr("Global lum.", "Lum. globale"), -100.0..=100.0, 0.0).show(ui, &mut g.global.lum);
        });
    } else {
        let wheel = match *tab {
            1 => &mut g.shadow,
            2 => &mut g.midtone,
            3 => &mut g.highlight,
            _ => &mut g.global,
        };
        let size = ui.available_width().min(170.0);
        ui.vertical_centered(|ui| {
            color_wheel(ui, &mut wheel.hue, &mut wheel.sat, size);
        });
        ui.push_id(*tab, |ui| {
            let hues: Vec<Color32> = (0..=6).map(|i| hue_color(i as f32 * 60.0, 0.8)).collect();
            Slider::new(tr("Hue", "Tonalità"), 0.0..=359.0, 0.0).colors(&hues).show(ui, &mut wheel.hue);
            let sat = [Color32::from_gray(128), hue_color(wheel.hue, 0.9)];
            Slider::new(tr("Saturation", "Saturazione"), 0.0..=100.0, 0.0).colors(&sat).show(ui, &mut wheel.sat);
            Slider::new(tr("Luminance", "Luminanza"), -100.0..=100.0, 0.0).colors(&[Color32::from_gray(20), Color32::from_gray(235)]).show(ui, &mut wheel.lum);
        });
    }
    ui.add_space(4.0);
    Slider::new(tr("Blending", "Fusione"), 0.0..=100.0, 50.0).show(ui, &mut g.blending);
    Slider::new(tr("Balance", "Bilanciamento"), -100.0..=100.0, 0.0).show(ui, &mut g.balance);
}

fn wheel_with_tooltip(ui: &mut Ui, w: &mut Wheel, size: f32) {
    let (h, s) = (w.hue, w.sat);
    color_wheel(ui, &mut w.hue, &mut w.sat, size).on_hover_text(trf!(
        "Hue {h:.0}° · Saturation {s:.0}\nDouble-click to reset",
        "Tonalità {h:.0}° · Saturazione {s:.0}\nDoppio clic per azzerare"
    ));
}

fn detail(ui: &mut Ui, edit: &mut Edit) {
    let d = &mut edit.settings.detail;
    subheading(ui, tr("Sharpening", "Nitidezza"));
    Slider::new(tr("Amount", "Fattore"), 0.0..=150.0, 40.0).show(ui, &mut d.sharpness);
    Slider::new(tr("Radius", "Raggio"), 0.5..=3.0, 1.0).decimals(1).show(ui, &mut d.sharpen_radius);
    Slider::new(tr("Detail", "Dettaglio"), 0.0..=100.0, 25.0).show(ui, &mut d.sharpen_detail);
    Slider::new(tr("Masking", "Mascheratura"), 0.0..=100.0, 0.0).show(ui, &mut d.sharpen_edge_masking);
    subheading(ui, tr("Noise reduction", "Riduzione disturbo"));
    Slider::new(tr("Luminance", "Luminanza"), 0.0..=100.0, 0.0)
        .show(ui, &mut d.luminance_nr)
        .on_hover_text(tr("Not implemented yet: the value is saved but not applied", "Non ancora implementata: il valore viene salvato ma non applicato"));
    Slider::new(tr("Color", "Colore"), 0.0..=100.0, 25.0).show(ui, &mut d.color_nr);
    Slider::new(tr("Color detail", "Dett. colore"), 0.0..=100.0, 50.0).show(ui, &mut d.color_nr_detail);
    Slider::new(tr("Color smooth.", "Unif. colore"), 0.0..=100.0, 50.0).show(ui, &mut d.color_nr_smoothness);
}

fn effects(ui: &mut Ui, edit: &mut Edit) {
    let e = &mut edit.settings.effects;
    subheading(ui, tr("Post-crop vignetting", "Vignettatura post-ritaglio"));
    Slider::new(tr("Amount", "Fattore"), -100.0..=100.0, 0.0).colors(&[Color32::from_gray(25), Color32::from_gray(230)]).show(ui, &mut e.vignette_amount);
    Slider::new(tr("Midpoint", "Punto medio"), 0.0..=100.0, 50.0).show(ui, &mut e.vignette_midpoint);
    Slider::new(tr("Roundness", "Rotondità"), -100.0..=100.0, 0.0).show(ui, &mut e.vignette_roundness);
    Slider::new(tr("Feather", "Sfumatura"), 0.0..=100.0, 50.0).show(ui, &mut e.vignette_feather);
    subheading(ui, tr("Grain", "Grana"));
    ui.push_id("grain", |ui| {
        Slider::new(tr("Amount", "Fattore"), 0.0..=100.0, 0.0).show(ui, &mut e.grain_amount);
        Slider::new(tr("Size", "Dimensione"), 0.0..=100.0, 25.0).show(ui, &mut e.grain_size);
        Slider::new(tr("Roughness", "Ruvidità"), 0.0..=100.0, 50.0).show(ui, &mut e.grain_frequency);
    });
}

fn calibration(ui: &mut Ui, edit: &mut Edit) {
    let c = &mut edit.settings.calibration;
    Slider::new(tr("Shadows tint", "Tinta ombre"), -100.0..=100.0, 0.0).colors(&TINT_COLORS).show(ui, &mut c.shadow_tint);
    for (name, deg, hue, sat) in [
        (tr("Red primary", "Primario rosso"), 0.0, &mut c.red_hue, &mut c.red_sat),
        (tr("Green primary", "Primario verde"), 120.0, &mut c.green_hue, &mut c.green_sat),
        (tr("Blue primary", "Primario blu"), 235.0, &mut c.blue_hue, &mut c.blue_sat),
    ] {
        subheading(ui, name);
        ui.push_id(name, |ui| {
            let hues = [hue_color(deg - 35.0, 0.8), hue_color(deg, 0.8), hue_color(deg + 35.0, 0.8)];
            Slider::new(tr("Hue", "Tonalità"), -100.0..=100.0, 0.0).colors(&hues).show(ui, hue);
            Slider::new(tr("Saturation", "Saturazione"), -100.0..=100.0, 0.0).colors(&[hue_color(deg, 0.0), hue_color(deg, 0.9)]).show(ui, sat);
        });
    }
}
