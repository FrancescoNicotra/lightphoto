//! Small toolbar icons, drawn with shapes rather than font glyphs so they
//! look the same on every system.

use eframe::egui::{self, pos2, vec2, Color32, Rect, Response, Sense, Shape, Stroke, Ui};

use crate::theme;

#[derive(Clone, Copy)]
pub enum Icon {
    /// One image.
    Single,
    /// Before | after with a divider.
    Split,
    /// Two images next to each other.
    SideBySide,
    /// The photo before the edits.
    Before,
    Crop,
    /// White balance eyedropper.
    Picker,
    /// Navigate: pan and zoom.
    Move,
    Brush,
    Eraser,
    LinearGradient,
    RadialGradient,
}

/// A toggle-style button with an icon; the tooltip names it.
pub fn icon_button(ui: &mut Ui, icon: Icon, selected: bool, tooltip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 22.0), Sense::click());
    let visuals = ui.style().interact_selectable(&resp, selected);
    let painter = ui.painter_at(rect.expand(1.0));
    if selected || resp.hovered() {
        painter.rect_filled(rect, 3.0, if selected { ui.visuals().selection.bg_fill } else { visuals.weak_bg_fill });
    }
    let color = if !ui.is_enabled() {
        theme::TEXT_DIM.gamma_multiply(0.5)
    } else if selected {
        Color32::WHITE
    } else {
        theme::TEXT
    };
    paint(&painter, icon, Rect::from_center_size(rect.center(), vec2(18.0, 13.0)), color);
    resp.on_hover_text(tooltip)
}

fn paint(p: &egui::Painter, icon: Icon, r: Rect, color: Color32) {
    let line = Stroke::new(1.4, color);
    let fill = color.gamma_multiply(0.55);
    match icon {
        Icon::Single => {
            p.rect_stroke(r, 1.5, line, egui::StrokeKind::Inside);
        }
        Icon::Split => {
            p.rect_filled(Rect::from_min_max(r.min, pos2(r.center().x, r.max.y)), 1.5, fill);
            p.rect_stroke(r, 1.5, line, egui::StrokeKind::Inside);
            p.line_segment([pos2(r.center().x, r.top() - 2.0), pos2(r.center().x, r.bottom() + 2.0)], line);
        }
        Icon::SideBySide => {
            let w = (r.width() - 3.0) / 2.0;
            let left = Rect::from_min_size(r.min, vec2(w, r.height()));
            let right = Rect::from_min_size(pos2(r.max.x - w, r.min.y), vec2(w, r.height()));
            p.rect_filled(left, 1.0, fill);
            p.rect_stroke(left, 1.0, line, egui::StrokeKind::Inside);
            p.rect_stroke(right, 1.0, line, egui::StrokeKind::Inside);
        }
        Icon::Before => {
            // A circle half filled: before / after.
            let c = r.center();
            let radius = r.height() / 2.0;
            let half: Vec<_> = (0..=16).map(|i| std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / 16.0).map(|a| c + vec2(a.cos(), a.sin()) * radius).collect();
            p.add(Shape::convex_polygon(half, fill, Stroke::NONE));
            p.circle_stroke(c, radius, line);
        }
        Icon::Crop => {
            // Two crop marks.
            let (l, t, rr, b) = (r.left() + 1.0, r.top() - 1.0, r.right() - 1.0, r.bottom() + 1.0);
            p.add(Shape::line(vec![pos2(l + 4.0, t), pos2(l + 4.0, b - 4.0), pos2(rr, b - 4.0)], line));
            p.add(Shape::line(vec![pos2(l, t + 4.0), pos2(rr - 4.0, t + 4.0), pos2(rr - 4.0, b)], line));
        }
        Icon::Move => {
            // Four arrows.
            let c = r.center();
            let k = r.height() / 2.0 + 1.0;
            for d in [vec2(1.0, 0.0), vec2(-1.0, 0.0), vec2(0.0, 1.0), vec2(0.0, -1.0)] {
                let tip = c + d * k;
                p.line_segment([c, tip], line);
                let side = vec2(-d.y, d.x) * 2.5;
                p.add(Shape::convex_polygon(vec![tip + d * 1.5, tip - d * 2.5 + side, tip - d * 2.5 - side], color, Stroke::NONE));
            }
        }
        Icon::Brush => {
            // Handle up right, a round tip down left.
            let tip = pos2(r.left() + 4.5, r.bottom() - 3.0);
            p.line_segment([tip + vec2(3.0, -3.0), pos2(r.right(), r.top() - 1.0)], Stroke::new(2.0, color));
            p.circle_filled(tip, 3.6, color);
        }
        Icon::Eraser => {
            // A tilted block, half filled.
            let c = r.center();
            let (u, v) = (vec2(0.75, -0.66), vec2(0.66, 0.75));
            let corner = |a: f32, b: f32| c + u * a + v * b;
            let body = vec![corner(-8.0, -3.5), corner(8.0, -3.5), corner(8.0, 3.5), corner(-8.0, 3.5)];
            p.add(Shape::convex_polygon(vec![corner(-8.0, -3.5), corner(-1.0, -3.5), corner(-1.0, 3.5), corner(-8.0, 3.5)], fill, Stroke::NONE));
            p.add(Shape::closed_line(body, line));
        }
        Icon::LinearGradient => {
            // A frame crossed by the three gradient lines.
            p.rect_stroke(r, 1.5, Stroke::new(1.0, color.gamma_multiply(0.6)), egui::StrokeKind::Inside);
            for (i, y) in [0.25f32, 0.5, 0.75].iter().enumerate() {
                let y = r.top() + r.height() * y;
                p.line_segment([pos2(r.left() - 1.0, y), pos2(r.right() + 1.0, y)], if i == 1 { line } else { Stroke::new(1.0, color) });
            }
        }
        Icon::RadialGradient => {
            let c = r.center();
            p.circle_stroke(c, r.height() / 2.0 + 0.5, line);
            p.circle_stroke(c, r.height() / 4.0, Stroke::new(1.0, color.gamma_multiply(0.7)));
            p.circle_filled(c, 1.5, color);
        }
        Icon::Picker => {
            // An eyedropper: bulb top right, glass tube down to the tip.
            let tip = pos2(r.left() + 3.0, r.bottom());
            let neck = pos2(r.right() - 6.0, r.top() + 5.0);
            p.line_segment([tip, neck], Stroke::new(2.2, color));
            p.circle_filled(pos2(r.right() - 3.5, r.top() + 2.5), 3.0, color);
            p.line_segment([neck + vec2(-3.0, -3.0), neck + vec2(3.0, 3.0)], line);
        }
    }
}
