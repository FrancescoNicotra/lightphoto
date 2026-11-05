//! Lightroom-style controls: labelled sliders with reset and an editable
//! value, the histogram, the tone curve and the colour grading wheels.

use std::ops::RangeInclusive;

use eframe::egui::{self, ecolor::Hsva, pos2, vec2, Color32, Mesh, Pos2, Rect, Response, Sense, Shape, Stroke, Ui};
use pipeline::tone::{self, Lut1};
use xmp::Curve;

use crate::theme;
use crate::worker::Histogram;

const ROW_HEIGHT: f32 = 20.0;
const LABEL_WIDTH: f32 = 84.0;
const VALUE_WIDTH: f32 = 50.0;

/// A slider row: label, track and value. Double-click the label or the
/// track to reset; click the value to type one.
pub struct Slider<'a> {
    label: &'a str,
    range: RangeInclusive<f32>,
    default: f32,
    decimals: usize,
    colors: &'a [Color32],
    signed: bool,
    suffix: &'a str,
    /// Maps the track position non-linearly (e.g. white balance in mired).
    mapping: Mapping,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mapping {
    Linear,
    /// Position proportional to 1/value: even steps in mired, as the
    /// temperature slider in Camera Raw.
    Reciprocal,
}

impl<'a> Slider<'a> {
    pub fn new(label: &'a str, range: RangeInclusive<f32>, default: f32) -> Self {
        let signed = *range.start() < 0.0;
        Slider { label, range, default, decimals: 0, colors: &[], signed, suffix: "", mapping: Mapping::Linear }
    }

    pub fn decimals(mut self, d: usize) -> Self {
        self.decimals = d;
        self
    }

    /// Gradient along the track, left to right.
    pub fn colors(mut self, c: &'a [Color32]) -> Self {
        self.colors = c;
        self
    }

    pub fn suffix(mut self, s: &'a str) -> Self {
        self.suffix = s;
        self
    }

    pub fn mapping(mut self, m: Mapping) -> Self {
        self.mapping = m;
        self
    }

    fn position_of(&self, v: f32) -> f32 {
        let (a, b) = (*self.range.start(), *self.range.end());
        match self.mapping {
            Mapping::Linear => (v - a) / (b - a),
            Mapping::Reciprocal => (1.0 / a - 1.0 / v) / (1.0 / a - 1.0 / b),
        }
        .clamp(0.0, 1.0)
    }

    fn value_at(&self, t: f32) -> f32 {
        let (a, b) = (*self.range.start(), *self.range.end());
        match self.mapping {
            Mapping::Linear => a + (b - a) * t,
            Mapping::Reciprocal => 1.0 / (1.0 / a - t * (1.0 / a - 1.0 / b)),
        }
    }

    fn round(&self, v: f32) -> f32 {
        let s = 10f32.powi(self.decimals as i32);
        ((v * s).round() / s).clamp(*self.range.start(), *self.range.end())
    }

    pub fn show(self, ui: &mut Ui, value: &mut f32) -> Response {
        let before = *value;
        let id = ui.scope_id().with(self.label);
        let width = ui.available_width().max(LABEL_WIDTH + VALUE_WIDTH + 40.0);
        let (rect, _) = ui.allocate_exact_size(vec2(width, ROW_HEIGHT), Sense::hover());
        let label_rect = Rect::from_min_size(rect.min, vec2(LABEL_WIDTH, ROW_HEIGHT));
        let value_rect = Rect::from_min_max(pos2(rect.right() - VALUE_WIDTH, rect.top() + 1.0), pos2(rect.right(), rect.bottom() - 1.0));
        let track_rect = Rect::from_min_max(pos2(label_rect.right() + 6.0, rect.top()), pos2(value_rect.left() - 10.0, rect.bottom()));

        let label_resp = ui.interact(label_rect, id.with("label"), Sense::click());
        let track_resp = ui.interact(track_rect, id.with("track"), Sense::click_and_drag());
        if track_resp.dragged() || track_resp.clicked() {
            if let Some(p) = track_resp.interact_pointer_pos() {
                let t = (p.x - track_rect.left()) / track_rect.width();
                *value = self.round(self.value_at(t.clamp(0.0, 1.0)));
            }
        }
        if label_resp.double_clicked() || track_resp.double_clicked() {
            *value = self.default;
        }

        let decimals = self.decimals;
        let signed = self.signed;
        let speed = (*self.range.end() - *self.range.start()) as f64 / 400.0;
        let value_resp = ui.put(
            value_rect,
            egui::DragValue::new(value)
                .range(self.range.clone())
                .speed(speed)
                .custom_formatter(move |v, _| if signed && v > 0.0 { format!("+{v:.decimals$}") } else { format!("{v:.decimals$}") })
                .suffix(self.suffix),
        );

        // Painting.
        let painter = ui.painter();
        let changed_from_default = (*value - self.default).abs() > 1e-6;
        let text_color = if changed_from_default { theme::TEXT } else { theme::TEXT_DIM };
        painter.text(label_rect.left_center(), egui::Align2::LEFT_CENTER, self.label, egui::FontId::proportional(12.5), text_color);

        let cy = track_rect.center().y;
        let line = Rect::from_min_max(pos2(track_rect.left(), cy - 1.5), pos2(track_rect.right(), cy + 1.5));
        if self.colors.len() >= 2 {
            painter.add(gradient(line, self.colors));
        } else {
            painter.rect_filled(line, 1.5, theme::TRACK);
        }
        // Default mark for bipolar sliders.
        if *self.range.start() < self.default && self.default < *self.range.end() {
            let x = track_rect.left() + self.position_of(self.default) * track_rect.width();
            painter.line_segment([pos2(x, cy - 4.0), pos2(x, cy + 4.0)], Stroke::new(1.0, theme::TRACK_MARK));
        }
        let x = track_rect.left() + self.position_of(*value) * track_rect.width();
        let active = track_resp.dragged() || track_resp.hovered();
        let handle = if active { Color32::WHITE } else { theme::HANDLE };
        let tri = vec![pos2(x, cy + 1.0), pos2(x - 5.0, cy + 8.0), pos2(x + 5.0, cy + 8.0)];
        painter.add(Shape::convex_polygon(tri, handle, Stroke::NONE));
        painter.circle_filled(pos2(x, cy), 3.0, handle);

        let mut resp = label_resp | track_resp | value_resp;
        if *value != before {
            resp.mark_changed();
        }
        resp
    }
}

/// A horizontal gradient through `colors`, evenly spaced.
pub fn gradient(rect: Rect, colors: &[Color32]) -> Shape {
    let mut mesh = Mesh::default();
    let n = colors.len() - 1;
    for (i, c) in colors.iter().enumerate() {
        let x = rect.left() + rect.width() * i as f32 / n as f32;
        mesh.colored_vertex(pos2(x, rect.top()), *c);
        mesh.colored_vertex(pos2(x, rect.bottom()), *c);
        if i > 0 {
            let k = (2 * i) as u32;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k, k + 1);
        }
    }
    Shape::mesh(mesh)
}

/// The colour at hue `deg` (0..360) and saturation `s` (0..1), for tracks
/// and wheels.
pub fn hue_color(deg: f32, s: f32) -> Color32 {
    Hsva::new(deg.rem_euclid(360.0) / 360.0, s, 0.85, 1.0).into()
}

/// RGB histogram, the three channels overlaid as in Lightroom.
pub fn histogram(ui: &mut Ui, hist: Option<&Histogram>) -> Response {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 96.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, theme::HIST_BG);
    let Some(h) = hist else { return resp };

    // Scale to the tallest bin, ignoring the clipped ends.
    let max = h.iter().flat_map(|c| c[1..255].iter()).copied().max().unwrap_or(1).max(1) as f32;
    let colors = [
        Color32::from_rgba_unmultiplied(230, 60, 60, 110),
        Color32::from_rgba_unmultiplied(60, 200, 80, 110),
        Color32::from_rgba_unmultiplied(70, 110, 240, 110),
    ];
    let inner = rect.shrink(2.0);
    for (c, color) in colors.iter().enumerate() {
        let mut mesh = Mesh::default();
        for (i, &n) in h[c].iter().enumerate() {
            let x = inner.left() + inner.width() * i as f32 / 255.0;
            let v = (n as f32 / max).min(1.0);
            let y = inner.bottom() - inner.height() * v;
            mesh.colored_vertex(pos2(x, inner.bottom()), *color);
            mesh.colored_vertex(pos2(x, y), *color);
            if i > 0 {
                let k = (2 * i) as u32;
                mesh.add_triangle(k - 2, k - 1, k);
                mesh.add_triangle(k - 1, k, k + 1);
            }
        }
        painter.add(Shape::mesh(mesh));
    }
    // Clipping warnings: a triangle lights up when a channel clips.
    let total: u32 = h[0].iter().sum::<u32>().max(1);
    let clip = |i: usize| h.iter().map(|c| c[i]).max().unwrap_or(0) as f32 / total as f32 > 0.002;
    for (i, x, dir) in [(0usize, inner.left() + 6.0, 1.0f32), (255, inner.right() - 6.0, -1.0)] {
        let col = if clip(i) { Color32::WHITE } else { Color32::from_gray(70) };
        let y = inner.top() + 6.0;
        painter.add(Shape::convex_polygon(vec![pos2(x - 4.0 * dir, y - 4.0), pos2(x + 4.0 * dir, y), pos2(x - 4.0 * dir, y + 4.0)], col, Stroke::NONE));
    }
    resp
}

/// The tone curve: shows `base` (the parametric curve, or identity)
/// followed by the point curve, with the points draggable. Click on the
/// curve to add a point, double-click a point to remove it.
pub fn curve_editor(ui: &mut Ui, curve: &mut Curve, base: &Lut1, color: Color32, hist: Option<&[u32; 256]>) -> Response {
    let side = ui.available_width().min(260.0);
    let (rect, mut resp) = ui.allocate_exact_size(vec2(side, side), Sense::click_and_drag());
    let painter = ui.painter_at(rect.expand(4.0));
    painter.rect_filled(rect, 2.0, theme::HIST_BG);

    let to_screen = |x: f32, y: f32| pos2(rect.left() + rect.width() * x / 255.0, rect.bottom() - rect.height() * y / 255.0);
    let from_screen = |p: Pos2| (((p.x - rect.left()) / rect.width() * 255.0).clamp(0.0, 255.0), ((rect.bottom() - p.y) / rect.height() * 255.0).clamp(0.0, 255.0));

    if let Some(h) = hist {
        let max = h[1..255].iter().copied().max().unwrap_or(1).max(1) as f32;
        let fill = Color32::from_gray(48);
        for (i, &n) in h.iter().enumerate() {
            let x = rect.left() + rect.width() * i as f32 / 255.0;
            let y = rect.bottom() - rect.height() * 0.9 * (n as f32 / max).min(1.0);
            painter.line_segment([pos2(x, rect.bottom()), pos2(x, y)], Stroke::new(rect.width() / 255.0 + 0.3, fill));
        }
    }
    let grid = Stroke::new(1.0, Color32::from_gray(58));
    for i in 1..4 {
        let f = i as f32 / 4.0;
        painter.line_segment([pos2(rect.left() + rect.width() * f, rect.top()), pos2(rect.left() + rect.width() * f, rect.bottom())], grid);
        painter.line_segment([pos2(rect.left(), rect.top() + rect.height() * f), pos2(rect.right(), rect.top() + rect.height() * f)], grid);
    }
    painter.line_segment([rect.left_bottom(), rect.right_top()], Stroke::new(1.0, Color32::from_gray(75)));

    // Interaction: pick the nearest point, or add one on the curve.
    let drag_id = resp.id.with("point");
    let mut dragging: Option<usize> = ui.data(|d| d.get_temp(drag_id));
    let near = |p: Pos2, pts: &[(f32, f32)]| {
        pts.iter()
            .enumerate()
            .map(|(i, &(x, y))| (i, to_screen(x, y).distance(p)))
            .filter(|(_, d)| *d < 10.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };
    if resp.double_clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            if let Some(i) = near(p, &curve.0) {
                if i > 0 && i + 1 < curve.0.len() {
                    curve.0.remove(i);
                    resp.mark_changed();
                }
            }
        }
        dragging = None;
    } else if resp.drag_started() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            dragging = near(p, &curve.0);
            if dragging.is_none() {
                let (x, _) = from_screen(p);
                let lut = tone::spline(curve);
                let y = lut.get(x / 255.0) * 255.0;
                let i = curve.0.iter().position(|&(px, _)| px > x).unwrap_or(curve.0.len());
                if i > 0 && i < curve.0.len() && (to_screen(x, y).y - p.y).abs() < 24.0 {
                    curve.0.insert(i, (x.round(), y.round()));
                    dragging = Some(i);
                    resp.mark_changed();
                }
            }
        }
    }
    if resp.dragged() {
        if let (Some(i), Some(p)) = (dragging, resp.interact_pointer_pos()) {
            if i < curve.0.len() {
                let (mut x, y) = from_screen(p);
                let lo = if i > 0 { curve.0[i - 1].0 + 1.0 } else { 0.0 };
                let hi = if i + 1 < curve.0.len() { curve.0[i + 1].0 - 1.0 } else { 255.0 };
                x = x.clamp(lo, hi.max(lo));
                let new = (x.round(), y.round());
                if curve.0[i] != new {
                    curve.0[i] = new;
                    resp.mark_changed();
                }
            }
        }
    }
    if !resp.dragged() && !resp.drag_started() {
        dragging = None;
    }
    ui.data_mut(|d| d.insert_temp(drag_id, dragging));

    // The resulting curve.
    let lut = base.then(&tone::spline(curve));
    let pts: Vec<Pos2> = (0..=128).map(|i| i as f32 / 128.0).map(|x| to_screen(x * 255.0, lut.get(x) * 255.0)).collect();
    painter.add(Shape::line(pts, Stroke::new(1.6, color)));
    // Points are in the point curve's own coordinates: with a parametric
    // curve as well they sit off the combined line, as in Lightroom.
    for (i, &(x, y)) in curve.0.iter().enumerate() {
        let r = if dragging == Some(i) { 5.0 } else { 4.0 };
        painter.circle(to_screen(x, y), r, theme::PANEL, Stroke::new(1.5, color));
    }
    resp
}

/// A colour grading wheel: angle = hue, distance from the centre =
/// saturation (0..100). Double-click resets.
pub fn color_wheel(ui: &mut Ui, hue: &mut f32, sat: &mut f32, size: f32) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(size, size), Sense::click_and_drag());
    let c = rect.center();
    let r = size / 2.0 - 3.0;
    if resp.double_clicked() {
        *hue = 0.0;
        *sat = 0.0;
        resp.mark_changed();
    } else if resp.dragged() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let d = p - c;
            let s = (d.length() / r * 100.0).clamp(0.0, 100.0).round();
            let h = (-d.y).atan2(d.x).to_degrees().rem_euclid(360.0).round();
            if (s, h) != (*sat, *hue) {
                (*sat, *hue) = (s, h);
                resp.mark_changed();
            }
        }
    }

    let painter = ui.painter_at(rect.expand(2.0));
    let mut mesh = Mesh::default();
    let n = 72;
    mesh.colored_vertex(c, Color32::from_gray(110));
    for i in 0..=n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        let deg = a.to_degrees();
        mesh.colored_vertex(c + vec2(a.cos(), -a.sin()) * r * 0.5, hue_color(deg, 0.35));
        mesh.colored_vertex(c + vec2(a.cos(), -a.sin()) * r, hue_color(deg, 0.75));
        if i > 0 {
            let k = (1 + 2 * i) as u32;
            mesh.add_triangle(0, k - 2, k);
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k, k + 1);
        }
    }
    painter.add(Shape::mesh(mesh));
    painter.circle_stroke(c, r, Stroke::new(1.0, Color32::from_gray(40)));
    let a = hue.to_radians();
    let p = c + vec2(a.cos(), -a.sin()) * r * (*sat / 100.0);
    painter.circle(p, 4.0, hue_color(*hue, *sat / 100.0), Stroke::new(1.5, Color32::WHITE));
    resp
}
