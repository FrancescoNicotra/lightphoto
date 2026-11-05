//! Lightroom's dark look: charcoal panels, light grey text, a blue accent.

use eframe::egui::{self, Color32, CornerRadius, Stroke};

pub const BG: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x1e);
pub const PANEL: Color32 = Color32::from_rgb(0x2b, 0x2b, 0x2b);
pub const PANEL_DARK: Color32 = Color32::from_rgb(0x24, 0x24, 0x24);
pub const HIST_BG: Color32 = Color32::from_rgb(0x1a, 0x1a, 0x1a);
pub const TEXT: Color32 = Color32::from_rgb(0xdc, 0xdc, 0xdc);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9a, 0x9a, 0x9a);
pub const ACCENT: Color32 = Color32::from_rgb(0x4a, 0x9e, 0xe8);
pub const TRACK: Color32 = Color32::from_rgb(0x50, 0x50, 0x50);
pub const TRACK_MARK: Color32 = Color32::from_rgb(0x80, 0x80, 0x80);
pub const HANDLE: Color32 = Color32::from_rgb(0xc8, 0xc8, 0xc8);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.global_style_mut(|style| {
        let v = &mut style.visuals;
        v.dark_mode = true;
        v.panel_fill = PANEL;
        v.window_fill = PANEL;
        v.extreme_bg_color = BG;
        v.faint_bg_color = PANEL_DARK;
        v.window_stroke = Stroke::new(1.0, Color32::from_gray(60));
        v.override_text_color = Some(TEXT);
        v.hyperlink_color = ACCENT;
        v.selection.bg_fill = ACCENT.linear_multiply(0.55);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.slider_trailing_fill = false;
        let r = CornerRadius::same(3);
        for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
            w.corner_radius = r;
        }
        v.widgets.noninteractive.bg_fill = PANEL;
        v.widgets.noninteractive.weak_bg_fill = PANEL;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_gray(52));
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.inactive.bg_fill = Color32::from_gray(58);
        v.widgets.inactive.weak_bg_fill = Color32::from_gray(58);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.hovered.bg_fill = Color32::from_gray(72);
        v.widgets.hovered.weak_bg_fill = Color32::from_gray(72);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_gray(110));
        v.widgets.active.bg_fill = ACCENT.linear_multiply(0.7);
        v.widgets.active.weak_bg_fill = ACCENT.linear_multiply(0.7);
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(8.0, 3.0);
    });
}
