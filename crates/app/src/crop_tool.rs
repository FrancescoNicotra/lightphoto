//! The crop & straighten tool: a rectangle with handles over the
//! straightened photo, aspect ratios, and rotation by dragging outside the
//! rectangle. It works on the oriented photo; the edit stores the crop in
//! Camera Raw's terms (see `pipeline::geometry`).

use eframe::egui::{self, pos2, vec2, Color32, Painter, Pos2, Response, Stroke};
use pipeline::geometry::{self, Rect};
use raw::Orientation;
use xmp::Crop;

use crate::i18n::tr;

/// English label, Italian label, long and short side (`None` = free,
/// zeros = the photo's own).
pub type Aspect = (&'static str, &'static str, Option<(f32, f32)>);

/// Aspect ratio choices.
pub const ASPECTS: [Aspect; 6] = [
    ("Free", "Libero", None),
    ("Original", "Originale", Some((0.0, 0.0))),
    ("1 : 1", "1 : 1", Some((1.0, 1.0))),
    ("4 : 5", "4 : 5", Some((5.0, 4.0))),
    ("3 : 2", "3 : 2", Some((3.0, 2.0))),
    ("16 : 9", "16 : 9", Some((16.0, 9.0))),
];

/// Smallest crop side, in pixels of the full photo.
const MIN_SIDE: f32 = 32.0;

pub struct CropTool {
    /// In fractions of the straightened, oriented photo.
    pub rect: Rect,
    /// Clockwise degrees on the oriented photo.
    pub angle: f32,
    /// Index into [`ASPECTS`].
    pub aspect: usize,
    pub portrait: bool,
    orientation: Orientation,
    /// Oriented full size, pixels.
    size: [f32; 2],
    /// The crop when the tool was opened, for Cancel.
    pub original: Crop,
    drag: Option<Drag>,
}

#[derive(Clone, Copy)]
enum Drag {
    Move,
    /// Edge or corner: −1, 0 or 1 on each axis.
    Handle(i8, i8),
    Rotate { from_angle: f32, from_pointer: f32 },
}

impl CropTool {
    pub fn new(crop: &Crop, orientation: Orientation, size: [usize; 2]) -> CropTool {
        let (rect, angle) = geometry::to_oriented(crop, orientation);
        let size = [size[0].max(1) as f32, size[1].max(1) as f32];
        CropTool { rect, angle, aspect: 0, portrait: size[1] > size[0], orientation, size, original: crop.clone(), drag: None }
    }

    /// The crop to store in the edit.
    pub fn result(&self) -> Crop {
        geometry::from_oriented(self.rect, self.angle, self.orientation)
    }

    /// What to render while the tool is open: the straightened photo,
    /// uncropped, so the whole frame can be chosen.
    pub fn preview(&self) -> Crop {
        geometry::from_oriented(Rect::FULL, self.angle, self.orientation)
    }

    /// Width / height of the chosen aspect, in pixels.
    fn ratio(&self) -> Option<f32> {
        let (a, b) = ASPECTS[self.aspect].2?;
        let landscape = if a == 0.0 { self.size[0].max(self.size[1]) / self.size[0].min(self.size[1]) } else { a / b };
        Some(if self.portrait { 1.0 / landscape } else { landscape })
    }

    pub fn set_aspect(&mut self, aspect: usize, portrait: bool) {
        self.aspect = aspect.min(ASPECTS.len() - 1);
        self.portrait = portrait;
        if let Some(r) = self.ratio() {
            // The largest rectangle of that shape around the current centre.
            let (cx, cy) = self.rect.center();
            let (w, h) = (self.size[0], self.size[1]);
            let (mut pw, mut ph) = (w, h);
            if pw / ph > r {
                pw = ph * r;
            } else {
                ph = pw / r;
            }
            let (hw, hh) = (pw / w / 2.0, ph / h / 2.0);
            let (cx, cy) = (cx.clamp(hw, 1.0 - hw), cy.clamp(hh, 1.0 - hh));
            self.rect = Rect { left: cx - hw, right: cx + hw, top: cy - hh, bottom: cy + hh };
        }
        self.fit();
    }

    pub fn set_angle(&mut self, angle: f32) {
        self.angle = angle.clamp(-45.0, 45.0);
        self.fit();
    }

    /// Back to the whole photo, unrotated.
    pub fn reset(&mut self) {
        self.angle = 0.0;
        self.rect = Rect::FULL;
        self.set_aspect(self.aspect, self.portrait);
    }

    fn fit(&mut self) {
        self.rect = geometry::fit_inside(self.rect, self.angle, self.size[0], self.size[1]);
    }

    fn valid(&self, r: Rect) -> bool {
        r.width() * self.size[0] >= MIN_SIDE.min(self.size[0]) - 0.5
            && r.height() * self.size[1] >= MIN_SIDE.min(self.size[1]) - 0.5
            && geometry::inside(r, self.angle, self.size[0], self.size[1])
    }

    /// Handles the pointer on the photo shown in `img`. Returns whether the
    /// crop changed.
    pub fn interact(&mut self, resp: &Response, img: egui::Rect) -> bool {
        let to_screen = |u: f32, v: f32| pos2(img.left() + u * img.width(), img.top() + v * img.height());
        let r = self.rect;
        if resp.drag_started() {
            let Some(p) = resp.interact_pointer_pos() else { return false };
            let handle = handles(r).into_iter().find(|&(_, _, u, v)| to_screen(u, v).distance(p) < 12.0);
            let inside = img.contains(p) && {
                let (u, v) = ((p.x - img.left()) / img.width(), (p.y - img.top()) / img.height());
                u > r.left && u < r.right && v > r.top && v < r.bottom
            };
            self.drag = Some(match handle {
                Some((hx, hy, ..)) => Drag::Handle(hx, hy),
                None if inside => Drag::Move,
                None => Drag::Rotate { from_angle: self.angle, from_pointer: pointer_angle(img.center(), p) },
            });
        }
        if !resp.dragged() {
            self.drag = None;
            return false;
        }
        let (Some(drag), Some(p)) = (self.drag, resp.interact_pointer_pos()) else { return false };
        let before = (self.rect, self.angle);
        match drag {
            Drag::Move => {
                let d = resp.drag_delta();
                let (du, dv) = (d.x / img.width(), d.y / img.height());
                let moved = Rect { left: r.left + du, right: r.right + du, top: r.top + dv, bottom: r.bottom + dv };
                if self.valid(moved) {
                    self.rect = moved;
                } else {
                    // Slide along an edge.
                    let mx = Rect { left: r.left + du, right: r.right + du, ..r };
                    let my = Rect { top: r.top + dv, bottom: r.bottom + dv, ..r };
                    if self.valid(mx) {
                        self.rect = mx;
                    } else if self.valid(my) {
                        self.rect = my;
                    }
                }
            }
            Drag::Handle(hx, hy) => {
                let (u, v) = (((p.x - img.left()) / img.width()).clamp(0.0, 1.0), ((p.y - img.top()) / img.height()).clamp(0.0, 1.0));
                let new = self.resized(r, hx, hy, u, v);
                if self.valid(new) {
                    self.rect = new;
                }
            }
            Drag::Rotate { from_angle, from_pointer } => {
                let a = from_angle + (pointer_angle(img.center(), p) - from_pointer);
                self.set_angle(a.round_to(0.1));
            }
        }
        (self.rect, self.angle) != before
    }

    /// The rectangle with an edge or corner moved to (u, v), keeping the
    /// aspect ratio when one is chosen.
    fn resized(&self, r: Rect, hx: i8, hy: i8, u: f32, v: f32) -> Rect {
        let mut n = r;
        match hx {
            -1 => n.left = u.min(r.right),
            1 => n.right = u.max(r.left),
            _ => {}
        }
        match hy {
            -1 => n.top = v.min(r.bottom),
            1 => n.bottom = v.max(r.top),
            _ => {}
        }
        let Some(ratio) = self.ratio() else { return n };
        // Pixel sizes; the dragged dimension decides, the other follows.
        let (w, h) = (n.width() * self.size[0], n.height() * self.size[1]);
        let (cx, cy) = r.center();
        if hy == 0 {
            let nh = w / ratio / self.size[1];
            n.top = cy - nh / 2.0;
            n.bottom = cy + nh / 2.0;
        } else if hx == 0 {
            let nw = h * ratio / self.size[0];
            n.left = cx - nw / 2.0;
            n.right = cx + nw / 2.0;
        } else {
            // Corner: keep the opposite corner fixed.
            let (nw, nh) = if w / h > ratio { (h * ratio, h) } else { (w, w / ratio) };
            let (nw, nh) = (nw / self.size[0], nh / self.size[1]);
            if hx < 0 {
                n.left = n.right - nw;
            } else {
                n.right = n.left + nw;
            }
            if hy < 0 {
                n.top = n.bottom - nh;
            } else {
                n.bottom = n.top + nh;
            }
        }
        n
    }

    /// Dims the outside, draws the frame, thirds and handles.
    pub fn paint(&self, painter: &Painter, img: egui::Rect) {
        let to_screen = |u: f32, v: f32| pos2(img.left() + u * img.width(), img.top() + v * img.height());
        let r = self.rect;
        let (a, b) = (to_screen(r.left, r.top), to_screen(r.right, r.bottom));
        let shade = Color32::from_black_alpha(150);
        let full = img;
        for band in [
            egui::Rect::from_min_max(full.min, pos2(full.max.x, a.y)),
            egui::Rect::from_min_max(pos2(full.min.x, b.y), full.max),
            egui::Rect::from_min_max(pos2(full.min.x, a.y), pos2(a.x, b.y)),
            egui::Rect::from_min_max(pos2(b.x, a.y), pos2(full.max.x, b.y)),
        ] {
            painter.rect_filled(band, 0.0, shade);
        }
        let frame = egui::Rect::from_min_max(a, b);
        let thin = Stroke::new(1.0, Color32::from_white_alpha(110));
        for i in 1..3 {
            let f = i as f32 / 3.0;
            painter.line_segment([pos2(frame.left() + frame.width() * f, frame.top()), pos2(frame.left() + frame.width() * f, frame.bottom())], thin);
            painter.line_segment([pos2(frame.left(), frame.top() + frame.height() * f), pos2(frame.right(), frame.top() + frame.height() * f)], thin);
        }
        painter.rect_stroke(frame, 0.0, Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Middle);
        for (_, _, u, v) in handles(r) {
            painter.rect_filled(egui::Rect::from_center_size(to_screen(u, v), vec2(9.0, 9.0)), 1.0, Color32::WHITE);
        }
        if matches!(self.drag, Some(Drag::Rotate { .. })) || self.angle != 0.0 {
            let text = format!("{} {:+.1}°", tr("Angle", "Angolo"), self.angle);
            let pos = frame.center_top() + vec2(0.0, 10.0);
            let galley = painter.layout_no_wrap(text, egui::FontId::proportional(12.0), Color32::WHITE);
            let at = pos - vec2(galley.size().x / 2.0, 0.0);
            painter.rect_filled(egui::Rect::from_min_size(at - vec2(6.0, 3.0), galley.size() + vec2(12.0, 6.0)), 3.0, Color32::from_black_alpha(160));
            painter.galley(at, galley, Color32::WHITE);
        }
    }
}

/// The eight handles: direction and position.
fn handles(r: Rect) -> [(i8, i8, f32, f32); 8] {
    let (cx, cy) = r.center();
    [
        (-1, -1, r.left, r.top),
        (0, -1, cx, r.top),
        (1, -1, r.right, r.top),
        (1, 0, r.right, cy),
        (1, 1, r.right, r.bottom),
        (0, 1, cx, r.bottom),
        (-1, 1, r.left, r.bottom),
        (-1, 0, r.left, cy),
    ]
}

/// Clockwise angle of `p` around `c`, in degrees (screen y points down).
fn pointer_angle(c: Pos2, p: Pos2) -> f32 {
    (p.y - c.y).atan2(p.x - c.x).to_degrees()
}

trait RoundTo {
    fn round_to(self, step: f32) -> f32;
}

impl RoundTo for f32 {
    fn round_to(self, step: f32) -> f32 {
        (self / step).round() * step
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aspect_ratios_and_angles_keep_the_crop_inside() {
        let mut t = CropTool::new(&Crop::default(), Orientation::Normal, [4592, 3448]);
        assert_eq!(t.rect, Rect::FULL);
        t.set_aspect(2, false); // 1:1
        assert!((t.rect.width() * 4592.0 - t.rect.height() * 3448.0).abs() < 1.0);
        t.set_aspect(4, true); // 3:2 portrait
        let r = t.rect.width() * 4592.0 / (t.rect.height() * 3448.0);
        assert!((r - 2.0 / 3.0).abs() < 1e-3, "{r}");
        t.set_angle(8.0);
        assert!(geometry::inside(t.rect, 8.0, 4592.0, 3448.0));
        let c = t.result();
        assert!(c.has_crop && c.angle == 8.0);
        t.reset();
        assert_eq!(t.angle, 0.0);
    }

    #[test]
    fn corner_drag_keeps_the_ratio() {
        let mut t = CropTool::new(&Crop::default(), Orientation::Normal, [3000, 2000]);
        t.set_aspect(4, false); // 3:2, the photo's own shape
        let n = t.resized(t.rect, 1, 1, 0.5, 0.9);
        let ratio = n.width() * 3000.0 / (n.height() * 2000.0);
        assert!((ratio - 1.5).abs() < 1e-3, "{ratio}");
        assert_eq!((n.left, n.top), (0.0, 0.0));
    }
}
