//! The image in the centre: fit / 100% zoom, pan, zoom around the cursor,
//! the before/after views (toggle, split with a draggable divider, side by
//! side), the white balance eyedropper and the crop overlay.

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, TextureHandle, Ui, Vec2};

use crate::i18n::tr;
use crate::icons::{icon_button, Icon};
use crate::theme;

/// A tool working on the shown image (crop, masks).
pub trait ViewTool {
    /// Handles the pointer on the image at `img` (screen rect). Returns
    /// whether the tool took the drag (no panning) and whether it changed
    /// something.
    fn interact(&mut self, ui: &Ui, resp: &egui::Response, img: Rect) -> (bool, bool);
    fn paint(&self, ui: &Ui, painter: &egui::Painter, img: Rect);
    /// The wheel resizes something instead of panning.
    fn takes_scroll(&self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zoom {
    Fit,
    /// Physical screen pixels per full-size image pixel (1.0 = 100%).
    Ratio(f32),
}

/// How the before and after are compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compare {
    /// One image; `\` switches to the before.
    Single,
    /// Before on the left of a draggable divider, after on the right.
    Split,
    /// Before and after next to each other.
    SideBySide,
}

pub struct Viewer {
    pub zoom: Zoom,
    /// Image point (0..1) at the centre of the view.
    center: Vec2,
    pub show_before: bool,
    pub compare: Compare,
    /// Split divider, as a fraction of the view width.
    split: f32,
    dragging_divider: bool,
    /// Zoom shown last frame (physical pixels per image pixel) and the
    /// fit zoom, for the toolbar's − / + / percentage.
    last_ratio: f32,
    fit_ratio: f32,
}

impl Default for Viewer {
    fn default() -> Self {
        Viewer {
            zoom: Zoom::Fit,
            center: vec2(0.5, 0.5),
            show_before: false,
            compare: Compare::Single,
            split: 0.5,
            dragging_divider: false,
            last_ratio: 1.0,
            fit_ratio: 1.0,
        }
    }
}

pub struct ViewInput<'a> {
    pub after: Option<&'a TextureHandle>,
    pub before: Option<&'a TextureHandle>,
    /// Full-size dimensions of the photo as shown (cropped).
    pub full_size: [usize; 2],
    /// Width of the interactive preview render, for the same framing.
    pub preview_width: usize,
    /// A message instead of the image (loading, errors, no photo).
    pub message: Option<String>,
    /// The eyedropper is active: a click picks a point.
    pub picking: bool,
}

#[derive(Default)]
pub struct ViewOutput {
    /// The view shows more pixels than the preview has.
    pub wants_detail: bool,
    /// Point clicked with the eyedropper, in fractions of the shown image.
    pub picked: Option<(f32, f32)>,
    /// The tool on the image changed something.
    pub tool_changed: bool,
}

impl Viewer {
    pub fn reset(&mut self) {
        self.zoom = Zoom::Fit;
        self.center = vec2(0.5, 0.5);
    }

    pub fn toolbar(&mut self, ui: &mut Ui, has_photo: bool) {
        ui.add_enabled_ui(has_photo, |ui| {
            self.zoom_control(ui);
            ui.separator();
            if icon_button(ui, Icon::Before, self.show_before, tr("Before: the photo without edits (\\)", "Prima: la foto senza modifiche (\\)")).clicked() {
                self.show_before = !self.show_before;
            }
            for (mode, icon, en, it) in [
                (Compare::Single, Icon::Single, "Single view", "Vista singola"),
                (Compare::Split, Icon::Split, "Before / after split: drag the divider", "Prima / dopo a tendina: trascina la linea"),
                (Compare::SideBySide, Icon::SideBySide, "Before / after side by side", "Prima / dopo affiancate"),
            ] {
                if icon_button(ui, icon, self.compare == mode, tr(en, it)).clicked() {
                    self.compare = mode;
                }
            }
        });
    }

    /// Fit, −, the zoom in percent (with common values) and +.
    fn zoom_control(&mut self, ui: &mut Ui) {
        if ui.selectable_label(self.zoom == Zoom::Fit, tr("Fit", "Adatta")).on_hover_text(tr("Fit the photo in the view (Z)", "Adatta la foto alla vista (Z)")).clicked() {
            self.zoom = Zoom::Fit;
        }
        let step = 1.25;
        if ui.button("−").on_hover_text(tr("Zoom out", "Riduci")).clicked() {
            self.set_ratio(self.last_ratio / step);
        }
        let mut chosen = None;
        egui::ComboBox::from_id_salt("zoom")
            .width(58.0)
            .selected_text(format!("{:.0}%", self.last_ratio * 100.0))
            .show_ui(ui, |ui| {
                for p in [25.0f32, 50.0, 100.0, 200.0, 400.0] {
                    if ui.selectable_label((self.last_ratio * 100.0 - p).abs() < 0.5, format!("{p:.0}%")).clicked() {
                        chosen = Some(p / 100.0);
                    }
                }
            });
        if let Some(r) = chosen {
            self.set_ratio(r);
        }
        if ui.button("+").on_hover_text(tr("Zoom in", "Ingrandisci")).clicked() {
            self.set_ratio(self.last_ratio * step);
        }
    }

    /// A zoom ratio; at or below the fit one it is Fit.
    pub fn set_ratio(&mut self, r: f32) {
        self.zoom = if r <= self.fit_ratio * 1.001 { Zoom::Fit } else { Zoom::Ratio(r.min(8.0)) };
    }

    pub fn show(&mut self, ui: &mut Ui, input: &ViewInput, mut tool: Option<&mut dyn ViewTool>) -> ViewOutput {
        let rect = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme::BG);
        let mut out = ViewOutput::default();

        let Some(after) = input.after.filter(|_| input.message.is_none()) else {
            let msg = input.message.clone().unwrap_or_default();
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(15.0), theme::TEXT_DIM);
            return out;
        };
        let before = input.before.unwrap_or(after);
        let compare = if tool.is_some() { Compare::Single } else { self.compare };

        // One pane, or two side by side sharing zoom and position.
        let panes: Vec<Rect> = match compare {
            Compare::SideBySide => {
                let half = (rect.width() - 4.0) / 2.0;
                vec![Rect::from_min_size(rect.min, vec2(half, rect.height())), Rect::from_min_size(pos2(rect.min.x + half + 4.0, rect.min.y), vec2(half, rect.height()))]
            }
            _ => vec![rect],
        };
        let pointer = resp.hover_pos().or(resp.interact_pointer_pos());
        let pane = pointer.and_then(|p| panes.iter().copied().find(|r| r.contains(p))).unwrap_or(panes[0]);

        let ppp = ui.ctx().pixels_per_point();
        let [w, h] = input.full_size.map(|v| v.max(1) as f32);
        let full = vec2(w, h);
        let view = panes[0].shrink(10.0);
        let fit = (view.width() / w).min(view.height() / h);
        let max_scale = 8.0 / ppp;
        let mut scale = match self.zoom {
            Zoom::Ratio(r) if r / ppp > fit => r / ppp,
            _ => {
                self.zoom = Zoom::Fit;
                fit
            }
        };

        // Zoom around the cursor (pinch, Ctrl/Cmd + wheel); pan with the
        // wheel or by dragging.
        if resp.hovered() {
            let (mut zoom, mut scroll, alt) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta(), i.modifiers.alt));
            // Alt (⌥ on macOS) + wheel zooms too; egui already turns a
            // trackpad pinch and Ctrl/⌘ + wheel into a zoom.
            if alt && scroll != Vec2::ZERO {
                let amount = if scroll.y != 0.0 { scroll.y } else { scroll.x };
                zoom *= (amount * 0.004).exp();
                scroll = Vec2::ZERO;
            }
            if zoom != 1.0 {
                let new = (scale * zoom).clamp(fit.min(max_scale), max_scale);
                if let Some(p) = resp.hover_pos() {
                    let img_min = self.image_min(pane, full * scale);
                    let at = (p - img_min) / (full * scale);
                    let new_min = p - at * full * new;
                    self.center = (pane.center() - new_min) / (full * new);
                }
                scale = new;
                self.zoom = if (scale - fit).abs() < 1e-4 { Zoom::Fit } else { Zoom::Ratio(scale * ppp) };
            } else if scroll != Vec2::ZERO && self.zoom != Zoom::Fit && !tool.as_ref().is_some_and(|t| t.takes_scroll()) {
                self.center -= scroll / (full * scale);
            }
        }

        let divider_x = rect.left() + rect.width() * self.split;
        if resp.drag_started() {
            self.dragging_divider = compare == Compare::Split && pointer.is_some_and(|p| (p.x - divider_x).abs() < 8.0);
        }
        let size = full * scale;
        let img_in = |pane: Rect, this: &Viewer| Rect::from_min_size(this.image_min(pane, size), size);

        let mut taken = false;
        if let Some(tool) = tool.as_deref_mut() {
            let (t, changed) = tool.interact(ui, &resp, img_in(pane, self));
            taken = t;
            out.tool_changed = changed;
        }
        if taken {
            // The tool sets its own cursor.
        } else if resp.dragged() && self.dragging_divider {
            if let Some(p) = resp.interact_pointer_pos() {
                self.split = ((p.x - rect.left()) / rect.width()).clamp(0.02, 0.98);
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        } else if resp.dragged() && !input.picking {
            // The hand: drag to move the photo (when it is larger than the view).
            if self.zoom != Zoom::Fit {
                self.center -= resp.drag_delta() / (full * scale);
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if resp.hovered() {
            let near_divider = compare == Compare::Split && pointer.is_some_and(|p| (p.x - divider_x).abs() < 8.0);
            ui.ctx().set_cursor_icon(if input.picking {
                egui::CursorIcon::Crosshair
            } else if near_divider {
                egui::CursorIcon::ResizeHorizontal
            } else {
                egui::CursorIcon::Grab
            });
        }
        if resp.clicked() && tool.is_none() && !self.dragging_divider {
            let p = resp.interact_pointer_pos();
            if input.picking {
                if let Some(p) = p {
                    let r = img_in(pane, self);
                    let (u, v) = ((p.x - r.left()) / r.width(), (p.y - r.top()) / r.height());
                    if (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v) {
                        out.picked = Some((u, v));
                    }
                }
            }
        }
        self.last_ratio = scale * ppp;
        self.fit_ratio = fit * ppp;

        let size = full * scale;
        // Keep the image filling the view on each axis where it is larger.
        for axis in 0..2 {
            let (vs, is) = (panes[0].size()[axis], size[axis]);
            self.center[axis] = if is <= vs { 0.5 } else { self.center[axis].clamp(vs / 2.0 / is, 1.0 - vs / 2.0 / is) };
        }
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        let img_rect = |pane: Rect| Rect::from_min_size(self.image_min(pane, size), size);
        match compare {
            Compare::Single => {
                let tex = if self.show_before && tool.is_none() { before } else { after };
                painter.image(tex.id(), img_rect(rect), uv, Color32::WHITE);
            }
            Compare::Split => {
                let r = img_rect(rect);
                painter.image(after.id(), r, uv, Color32::WHITE);
                let left = Rect::from_min_max(rect.min, pos2(divider_x, rect.max.y));
                painter.with_clip_rect(left).image(before.id(), r, uv, Color32::WHITE);
                painter.line_segment([pos2(divider_x, rect.top()), pos2(divider_x, rect.bottom())], egui::Stroke::new(2.0, Color32::WHITE));
                painter.circle(pos2(divider_x, rect.center().y), 9.0, Color32::from_black_alpha(170), egui::Stroke::new(1.5, Color32::WHITE));
                label(&painter, pos2(rect.left() + 10.0, rect.bottom() - 30.0), tr("Before", "Prima"));
                label(&painter, pos2(rect.right() - 60.0, rect.bottom() - 30.0), tr("After", "Dopo"));
            }
            Compare::SideBySide => {
                for (pane, tex, text) in [(panes[0], before, tr("Before", "Prima")), (panes[1], after, tr("After", "Dopo"))] {
                    let p = painter.with_clip_rect(pane);
                    p.image(tex.id(), img_rect(pane), uv, Color32::WHITE);
                    label(&p, pos2(pane.left() + 10.0, pane.bottom() - 30.0), text);
                }
            }
        }
        if let Some(tool) = tool.as_deref() {
            tool.paint(ui, &painter, img_rect(rect));
        }

        out.wants_detail = size.x * ppp > input.preview_width as f32 * 1.05;

        if self.show_before && compare == Compare::Single && tool.is_none() {
            label(&painter, rect.left_top() + vec2(10.0, 10.0), tr("Before", "Prima"));
        }
        if input.picking {
            label(&painter, rect.center_top() + vec2(-150.0, 10.0), tr("Click a neutral grey or white point (Esc to cancel)", "Clic su un punto grigio o bianco neutro (Esc per annullare)"));
        }
        out
    }

    fn image_min(&self, pane: Rect, size: Vec2) -> egui::Pos2 {
        pane.center() - self.center * size
    }
}

/// Text on a dark rounded background.
fn label(painter: &egui::Painter, pos: egui::Pos2, text: &str) {
    let galley = painter.layout_no_wrap(text.to_string(), egui::FontId::proportional(12.0), theme::TEXT);
    painter.rect_filled(Rect::from_min_size(pos - vec2(6.0, 3.0), galley.size() + vec2(12.0, 6.0)), 3.0, Color32::from_black_alpha(150));
    painter.galley(pos, galley, theme::TEXT);
}

/// Text for an empty view.
pub fn empty_message() -> String {
    tr("Open a folder of photos (⌘O / Ctrl+O) or drop a file here", "Apri una cartella di foto (⌘O / Ctrl+O) o trascina qui un file").into()
}
