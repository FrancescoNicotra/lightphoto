//! The image in the centre: fit / 100% zoom, pan, zoom around the cursor
//! and the before/after toggle.

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, TextureHandle, Ui, Vec2};

use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zoom {
    Fit,
    /// Physical screen pixels per full-size image pixel (1.0 = 100%).
    Ratio(f32),
}

pub struct Viewer {
    pub zoom: Zoom,
    /// Image point (0..1) at the centre of the view.
    center: Vec2,
    pub show_before: bool,
}

impl Default for Viewer {
    fn default() -> Self {
        Viewer { zoom: Zoom::Fit, center: vec2(0.5, 0.5), show_before: false }
    }
}

pub struct ViewInput<'a> {
    /// The best texture available for the current view.
    pub texture: Option<&'a TextureHandle>,
    /// Full-size dimensions of the photo.
    pub full_size: [usize; 2],
    /// Width of the interactive preview render.
    pub preview_width: usize,
    /// A message instead of the image (loading, errors, no photo).
    pub message: Option<String>,
}

pub struct ViewOutput {
    /// The view shows more pixels than the preview has.
    pub wants_detail: bool,
}

impl Viewer {
    pub fn reset(&mut self) {
        self.zoom = Zoom::Fit;
        self.center = vec2(0.5, 0.5);
    }

    pub fn toolbar(&mut self, ui: &mut Ui, has_photo: bool) {
        ui.horizontal(|ui| {
            ui.add_enabled_ui(has_photo, |ui| {
                if ui.selectable_label(self.zoom == Zoom::Fit, "Adatta").clicked() {
                    self.zoom = Zoom::Fit;
                }
                for (label, r) in [("100%", 1.0), ("200%", 2.0)] {
                    if ui.selectable_label(self.zoom == Zoom::Ratio(r), label).clicked() {
                        self.zoom = Zoom::Ratio(r);
                    }
                }
                ui.separator();
                ui.toggle_value(&mut self.show_before, "Prima / Dopo  \\").on_hover_text("Mostra la foto senza modifiche (tasto \\)");
            });
        });
    }

    pub fn show(&mut self, ui: &mut Ui, input: &ViewInput) -> ViewOutput {
        let rect = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme::BG);
        let mut out = ViewOutput { wants_detail: false };

        let Some(tex) = input.texture.filter(|_| input.message.is_none()) else {
            let msg = input.message.clone().unwrap_or_default();
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(15.0), theme::TEXT_DIM);
            return out;
        };

        let ppp = ui.ctx().pixels_per_point();
        let [w, h] = input.full_size.map(|v| v.max(1) as f32);
        let full = vec2(w, h);
        let view = rect.shrink(10.0);
        let fit = (view.width() / w).min(view.height() / h);
        let max_scale = 8.0 / ppp;
        let mut scale = match self.zoom {
            Zoom::Fit => fit,
            Zoom::Ratio(r) => r / ppp,
        };

        // Zoom around the cursor (pinch, Ctrl/Cmd + wheel); pan with the
        // wheel or by dragging.
        if resp.hovered() {
            let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta()));
            if zoom != 1.0 {
                let new = (scale * zoom).clamp(fit.min(max_scale), max_scale);
                if let Some(p) = resp.hover_pos() {
                    let img_min = self.image_min(rect, full * scale);
                    let at = (p - img_min) / (full * scale);
                    let new_min = p - at * full * new;
                    self.center = (rect.center() - new_min) / (full * new);
                }
                scale = new;
                self.zoom = if (scale - fit).abs() < 1e-4 { Zoom::Fit } else { Zoom::Ratio(scale * ppp) };
            } else if scroll != Vec2::ZERO && self.zoom != Zoom::Fit {
                self.center -= scroll / (full * scale);
            }
        }
        if resp.dragged() && self.zoom != Zoom::Fit {
            self.center -= resp.drag_delta() / (full * scale);
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if resp.hovered() {
            ui.ctx().set_cursor_icon(if self.zoom == Zoom::Fit { egui::CursorIcon::ZoomIn } else { egui::CursorIcon::Grab });
        }
        if resp.clicked() {
            if self.zoom == Zoom::Fit {
                if let Some(p) = resp.interact_pointer_pos() {
                    let size = full * fit;
                    self.center = ((p - self.image_min(rect, size)) / size).clamp(Vec2::ZERO, Vec2::splat(1.0));
                }
                self.zoom = Zoom::Ratio(1.0);
                scale = 1.0 / ppp;
            } else {
                self.zoom = Zoom::Fit;
                scale = fit;
            }
        }

        let size = full * scale;
        // Keep the image filling the view on each axis where it is larger.
        for axis in 0..2 {
            let (vs, is) = (rect.size()[axis], size[axis]);
            self.center[axis] = if is <= vs { 0.5 } else { self.center[axis].clamp(vs / 2.0 / is, 1.0 - vs / 2.0 / is) };
        }
        let img_rect = Rect::from_min_size(self.image_min(rect, size), size);
        painter.image(tex.id(), img_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);

        out.wants_detail = size.x * ppp > input.preview_width as f32 * 1.05;

        let pct = format!("{:.0}%", scale * ppp * 100.0);
        let badge = if self.show_before { format!("Prima · {pct}") } else { pct };
        let pos = rect.left_top() + vec2(10.0, 10.0);
        let galley = painter.layout_no_wrap(badge, egui::FontId::proportional(12.0), theme::TEXT);
        painter.rect_filled(Rect::from_min_size(pos - vec2(6.0, 3.0), galley.size() + vec2(12.0, 6.0)), 3.0, Color32::from_black_alpha(150));
        painter.galley(pos, galley, theme::TEXT);
        out
    }

    fn image_min(&self, rect: Rect, size: Vec2) -> egui::Pos2 {
        rect.center() - self.center * size
    }
}

/// Text for an empty view.
pub fn empty_message() -> String {
    "Apri una cartella di foto (Ctrl+O) o trascina qui un file".into()
}
