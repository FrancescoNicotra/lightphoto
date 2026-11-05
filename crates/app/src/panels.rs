//! The develop panels on the right: Base, Curve, HSL, Color grading,
//! Detail, Effects, Calibration, Profile/LUT.

use eframe::egui::{self, Color32, RichText, Ui};
use pipeline::tone::{self, Lut1};
use pipeline::SourceKind;
use xmp::{Curve, WhiteBalance, Wheel, NS_CRS};

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

const BANDS: [(&str, f32); 8] = [
    ("Rosso", 0.0),
    ("Arancione", 30.0),
    ("Giallo", 60.0),
    ("Verde", 120.0),
    ("Acqua", 180.0),
    ("Blu", 235.0),
    ("Viola", 270.0),
    ("Magenta", 310.0),
];

fn section(ui: &mut Ui, title: &str, open: bool, force: Option<&str>, add: impl FnOnce(&mut Ui)) {
    egui::CollapsingHeader::new(RichText::new(title).strong().size(13.5))
        .id_salt(title)
        .default_open(open)
        .open(force.map(|f| title.starts_with(f)))
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
        ui.label(RichText::new("Sviluppo").strong().size(15.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Reimposta").on_hover_text("Riporta tutte le impostazioni ai valori predefiniti").clicked() {
                actions.push(PanelAction::ResetAll);
            }
        });
    });
    ui.separator();

    let force = st.force_open.take();
    let f = force.as_deref();
    section(ui, "Profilo e LUT", true, f, |ui| profile(ui, edit, &mut actions));
    section(ui, "Base", true, f, |ui| basic(ui, edit, input));
    section(ui, "Curva di viraggio", false, f, |ui| curve(ui, edit, input, &mut st.curve_tab));
    section(ui, "HSL / Colore", false, f, |ui| hsl(ui, edit, &mut st.hsl_tab));
    section(ui, "Color grading", false, f, |ui| grading(ui, edit, &mut st.grading_tab));
    section(ui, "Dettaglio", false, f, |ui| detail(ui, edit));
    section(ui, "Effetti", false, f, |ui| effects(ui, edit));
    section(ui, "Calibrazione", false, f, |ui| calibration(ui, edit));
    actions
}

fn profile(ui: &mut Ui, edit: &mut Edit, actions: &mut Vec<PanelAction>) {
    let s = &mut edit.settings;
    let mut remove_look = false;
    match &mut s.look {
        Some(look) => {
            ui.horizontal(|ui| {
                ui.label("Profilo");
                ui.label(RichText::new(&look.name).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    remove_look = ui.small_button("×").on_hover_text("Torna al profilo predefinito").clicked();
                });
            });
            let mut amount = look.amount * 100.0;
            if Slider::new("Fattore", 0.0..=200.0, 100.0).suffix("%").show(ui, &mut amount).changed() {
                look.amount = amount / 100.0;
            }
        }
        None => {
            ui.horizontal(|ui| {
                ui.label("Profilo");
                ui.label(RichText::new("Predefinito").color(theme::TEXT_DIM))
                    .on_hover_text("Adobe Standard se Lightroom è installato, altrimenti le matrici del file e la curva ACR3");
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
                ui.label(RichText::new("nessuna").color(theme::TEXT_DIM));
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if edit.lut.is_some() {
                remove_lut = ui.small_button("×").on_hover_text("Rimuovi la LUT").clicked();
            }
            if ui.small_button("Carica…").on_hover_text("Carica una LUT .cube o .3dl").clicked() {
                actions.push(PanelAction::LoadLut);
            }
        });
    });
    if edit.lut.is_some() {
        Slider::new("Fattore LUT", 0.0..=100.0, 100.0).suffix("%").show(ui, &mut edit.lut_amount);
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
            ("Come scattato", WhiteBalance::AsShot),
            ("Automatico", WhiteBalance::Auto),
            ("Luce diurna", WhiteBalance::Preset("Daylight".into())),
            ("Nuvoloso", WhiteBalance::Preset("Cloudy".into())),
            ("Ombra", WhiteBalance::Preset("Shade".into())),
            ("Tungsteno", WhiteBalance::Preset("Tungsten".into())),
            ("Fluorescente", WhiteBalance::Preset("Fluorescent".into())),
            ("Flash", WhiteBalance::Preset("Flash".into())),
            ("Personalizzato", WhiteBalance::Custom),
        ];
        let current = names.iter().find(|(_, wb)| *wb == s.white_balance).map_or("Personalizzato", |(n, _)| n);
        ui.horizontal(|ui| {
            ui.label("Bil. bianco");
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
        let rn = Slider::new("Tinta", -150.0..=150.0, eff_tint.round()).colors(&TINT_COLORS).show(ui, &mut tint);
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
        Slider::new("Tinta", -100.0..=100.0, 0.0).colors(&TINT_COLORS).show(ui, &mut s.incremental_tint);
    }

    subheading(ui, "Tono");
    let b = &mut s.basic;
    Slider::new("Esposizione", -5.0..=5.0, 0.0).decimals(2).show(ui, &mut b.exposure);
    Slider::new("Contrasto", -100.0..=100.0, 0.0).show(ui, &mut b.contrast);
    Slider::new("Luci", -100.0..=100.0, 0.0).show(ui, &mut b.highlights);
    Slider::new("Ombre", -100.0..=100.0, 0.0).show(ui, &mut b.shadows);
    Slider::new("Bianchi", -100.0..=100.0, 0.0).show(ui, &mut b.whites);
    Slider::new("Neri", -100.0..=100.0, 0.0).show(ui, &mut b.blacks);

    subheading(ui, "Presenza");
    let p = &mut s.presence;
    Slider::new("Texture", -100.0..=100.0, 0.0).show(ui, &mut p.texture);
    Slider::new("Chiarezza", -100.0..=100.0, 0.0).show(ui, &mut p.clarity);
    Slider::new("Foschia", -100.0..=100.0, 0.0).show(ui, &mut p.dehaze);
    let sat_colors = [Color32::from_gray(128), Color32::from_rgb(220, 60, 160)];
    Slider::new("Vividezza", -100.0..=100.0, 0.0).colors(&sat_colors).show(ui, &mut p.vibrance);
    Slider::new("Saturazione", -100.0..=100.0, 0.0).colors(&sat_colors).show(ui, &mut p.saturation);
}

fn curve(ui: &mut Ui, edit: &mut Edit, input: &PanelInput, tab: &mut usize) {
    tabs(ui, tab, &["Parametrica", "Punti", "R", "G", "B"]);
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
            Slider::new("Luci", -100.0..=100.0, 0.0).show(ui, &mut p.highlights);
            Slider::new("Chiari", -100.0..=100.0, 0.0).show(ui, &mut p.lights);
            Slider::new("Scuri", -100.0..=100.0, 0.0).show(ui, &mut p.darks);
            Slider::new("Ombre", -100.0..=100.0, 0.0).show(ui, &mut p.shadows);
            subheading(ui, "Divisioni");
            let (a, b, c) = (p.shadow_split, p.midtone_split, p.highlight_split);
            ui.push_id("splits", |ui| {
                Slider::new("Ombre", 5.0..=(b - 5.0).max(5.0), 25.0).show(ui, &mut p.shadow_split);
                Slider::new("Mezzitoni", (a + 5.0)..=(c - 5.0).max(a + 5.0), 50.0).show(ui, &mut p.midtone_split);
                Slider::new("Luci", (b + 5.0).min(95.0)..=95.0, 75.0).show(ui, &mut p.highlight_split);
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
        ui.label(RichText::new("Clic: aggiungi · trascina · doppio clic: rimuovi").color(theme::TEXT_DIM).size(11.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("Lineare").clicked() {
                *curve = Curve::default();
            }
        });
    });
}

fn hsl(ui: &mut Ui, edit: &mut Edit, tab: &mut usize) {
    tabs(ui, tab, &["Tonalità", "Saturazione", "Luminanza"]);
    let h = &mut edit.settings.hsl;
    let values = match *tab {
        0 => &mut h.hue,
        1 => &mut h.saturation,
        _ => &mut h.luminance,
    };
    ui.push_id(*tab, |ui| {
        for (i, (name, deg)) in BANDS.iter().enumerate() {
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
    tabs(ui, tab, &["3 vie", "Ombre", "Mezzitoni", "Luci", "Globale"]);
    let g = &mut edit.settings.color_grade;
    if *tab == 0 {
        let size = ((ui.available_width() - 16.0) / 3.0).min(96.0);
        ui.columns(3, |cols| {
            for (col, (name, wheel)) in cols.iter_mut().zip([("Ombre", &mut g.shadow), ("Mezzitoni", &mut g.midtone), ("Luci", &mut g.highlight)]) {
                col.vertical_centered(|ui| {
                    ui.label(RichText::new(name).size(11.5));
                    ui.push_id(name, |ui| wheel_with_tooltip(ui, wheel, size));
                });
            }
        });
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Globale").size(11.5));
            ui.push_id("global", |ui| wheel_with_tooltip(ui, &mut g.global, size));
        });
        ui.push_id("lums", |ui| {
            Slider::new("Lum. ombre", -100.0..=100.0, 0.0).show(ui, &mut g.shadow.lum);
            Slider::new("Lum. mezzitoni", -100.0..=100.0, 0.0).show(ui, &mut g.midtone.lum);
            Slider::new("Lum. luci", -100.0..=100.0, 0.0).show(ui, &mut g.highlight.lum);
            Slider::new("Lum. globale", -100.0..=100.0, 0.0).show(ui, &mut g.global.lum);
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
            Slider::new("Tonalità", 0.0..=359.0, 0.0).colors(&hues).show(ui, &mut wheel.hue);
            let sat = [Color32::from_gray(128), hue_color(wheel.hue, 0.9)];
            Slider::new("Saturazione", 0.0..=100.0, 0.0).colors(&sat).show(ui, &mut wheel.sat);
            Slider::new("Luminanza", -100.0..=100.0, 0.0).colors(&[Color32::from_gray(20), Color32::from_gray(235)]).show(ui, &mut wheel.lum);
        });
    }
    ui.add_space(4.0);
    Slider::new("Fusione", 0.0..=100.0, 50.0).show(ui, &mut g.blending);
    Slider::new("Bilanciamento", -100.0..=100.0, 0.0).show(ui, &mut g.balance);
}

fn wheel_with_tooltip(ui: &mut Ui, w: &mut Wheel, size: f32) {
    let (h, s) = (w.hue, w.sat);
    color_wheel(ui, &mut w.hue, &mut w.sat, size).on_hover_text(format!("Tonalità {h:.0}° · Saturazione {s:.0}\nDoppio clic per azzerare"));
}

fn detail(ui: &mut Ui, edit: &mut Edit) {
    let d = &mut edit.settings.detail;
    subheading(ui, "Nitidezza");
    Slider::new("Fattore", 0.0..=150.0, 40.0).show(ui, &mut d.sharpness);
    Slider::new("Raggio", 0.5..=3.0, 1.0).decimals(1).show(ui, &mut d.sharpen_radius);
    Slider::new("Dettaglio", 0.0..=100.0, 25.0).show(ui, &mut d.sharpen_detail);
    Slider::new("Mascheratura", 0.0..=100.0, 0.0).show(ui, &mut d.sharpen_edge_masking);
    subheading(ui, "Riduzione disturbo");
    Slider::new("Luminanza", 0.0..=100.0, 0.0)
        .show(ui, &mut d.luminance_nr)
        .on_hover_text("Non ancora implementata: il valore viene salvato ma non applicato");
    Slider::new("Colore", 0.0..=100.0, 25.0).show(ui, &mut d.color_nr);
    Slider::new("Dett. colore", 0.0..=100.0, 50.0).show(ui, &mut d.color_nr_detail);
    Slider::new("Unif. colore", 0.0..=100.0, 50.0).show(ui, &mut d.color_nr_smoothness);
}

fn effects(ui: &mut Ui, edit: &mut Edit) {
    let e = &mut edit.settings.effects;
    subheading(ui, "Vignettatura post-ritaglio");
    Slider::new("Fattore", -100.0..=100.0, 0.0).colors(&[Color32::from_gray(25), Color32::from_gray(230)]).show(ui, &mut e.vignette_amount);
    Slider::new("Punto medio", 0.0..=100.0, 50.0).show(ui, &mut e.vignette_midpoint);
    Slider::new("Rotondità", -100.0..=100.0, 0.0).show(ui, &mut e.vignette_roundness);
    Slider::new("Sfumatura", 0.0..=100.0, 50.0).show(ui, &mut e.vignette_feather);
    subheading(ui, "Grana");
    ui.push_id("grain", |ui| {
        Slider::new("Fattore", 0.0..=100.0, 0.0).show(ui, &mut e.grain_amount);
        Slider::new("Dimensione", 0.0..=100.0, 25.0).show(ui, &mut e.grain_size);
        Slider::new("Ruvidità", 0.0..=100.0, 50.0).show(ui, &mut e.grain_frequency);
    });
}

fn calibration(ui: &mut Ui, edit: &mut Edit) {
    let c = &mut edit.settings.calibration;
    Slider::new("Tinta ombre", -100.0..=100.0, 0.0).colors(&TINT_COLORS).show(ui, &mut c.shadow_tint);
    for (name, deg, hue, sat) in [
        ("Primario rosso", 0.0, &mut c.red_hue, &mut c.red_sat),
        ("Primario verde", 120.0, &mut c.green_hue, &mut c.green_sat),
        ("Primario blu", 235.0, &mut c.blue_hue, &mut c.blue_sat),
    ] {
        subheading(ui, name);
        ui.push_id(name, |ui| {
            let hues = [hue_color(deg - 35.0, 0.8), hue_color(deg, 0.8), hue_color(deg + 35.0, 0.8)];
            Slider::new("Tonalità", -100.0..=100.0, 0.0).colors(&hues).show(ui, hue);
            Slider::new("Saturazione", -100.0..=100.0, 0.0).colors(&[hue_color(deg, 0.0), hue_color(deg, 0.9)]).show(ui, sat);
        });
    }
}
