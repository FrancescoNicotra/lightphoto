//! Local corrections: a weight map (0..1) per mask, at the resolution of
//! the render, and the local sliders applied through it.
//!
//! Mask geometry is in fractions of the uncropped image in the file's
//! stored orientation, as Camera Raw keeps it, so a mask stays on the same
//! part of the picture when the crop changes. [`Frame`] maps the pixels of
//! a developed (oriented, straightened, cropped) image back to those
//! coordinates.

use rayon::prelude::*;

use raw::{Image, Orientation};
use xmp::masks::{BlendMode, LocalAdjust, LocalCorrection, MaskComponent, MaskShape, Stroke};
use xmp::{Basic, Crop};

use crate::geometry::{self, Rect};
use crate::local;
use crate::tone::{self, Lut1};

/// How the pixels of a developed image relate to the stored image.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    orientation: Orientation,
    rect: Rect,
    angle: f32,
    /// Oriented, uncropped size (any scale: only the shape matters).
    canvas: (f32, f32),
    /// Size of the developed image, pixels.
    out: (usize, usize),
}

impl Frame {
    /// For an oriented `canvas_w`×`canvas_h` photo developed with `crop`
    /// into an `out_w`×`out_h` image.
    pub fn new(canvas_w: usize, canvas_h: usize, crop: &Crop, orientation: Orientation, out_w: usize, out_h: usize) -> Frame {
        let (rect, angle) = if crop.is_active() { geometry::to_oriented(crop, orientation) } else { (Rect::FULL, 0.0) };
        Frame { orientation, rect, angle, canvas: (canvas_w.max(1) as f32, canvas_h.max(1) as f32), out: (out_w, out_h) }
    }

    /// Stored-orientation pixel size (same scale as the canvas).
    fn stored(&self) -> (f32, f32) {
        if self.orientation.swaps_axes() {
            (self.canvas.1, self.canvas.0)
        } else {
            self.canvas
        }
    }

    /// Output pixel centre → fractions of the stored image.
    pub fn to_stored(&self, x: f32, y: f32) -> (f32, f32) {
        let (w, h) = self.canvas;
        let cx = (self.rect.left + x / self.out.0 as f32 * self.rect.width()) * w;
        let cy = (self.rect.top + y / self.out.1 as f32 * self.rect.height()) * h;
        let (sx, sy) = geometry::source_of(cx, cy, w, h, self.angle);
        geometry::unorient_point(self.orientation, (sx / w, sy / h))
    }

    /// Fractions of the stored image → output pixels.
    pub fn to_output(&self, u: f32, v: f32) -> (f32, f32) {
        let (w, h) = self.canvas;
        let (ou, ov) = geometry::orient_point(self.orientation, (u, v));
        // Forward rotation: the inverse of `source_of`.
        let (px, py) = geometry::source_of(ou * w, ov * h, w, h, -self.angle);
        ((px / w - self.rect.left) / self.rect.width() * self.out.0 as f32, (py / h - self.rect.top) / self.rect.height() * self.out.1 as f32)
    }

    /// Output pixels per stored-image long edge.
    pub fn scale(&self) -> f32 {
        let (w, h) = self.canvas;
        self.out.0 as f32 / (self.rect.width() * w) * w.max(h)
    }
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    if e1 <= e0 {
        return if x < e0 { 0.0 } else { 1.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The weight map (0..1, row-major, the frame's output size) of one
/// correction, including its Amount. `None` when it has no effect.
pub fn correction_weights(c: &LocalCorrection, frame: &Frame) -> Option<Vec<f32>> {
    if !c.active || c.amount <= 0.0 || c.components.is_empty() || !c.is_supported() {
        return None;
    }
    let (w, h) = frame.out;
    let mut acc = vec![0.0f32; w * h];
    for (i, comp) in c.components.iter().enumerate() {
        let layer = component_weights(comp, frame);
        let mode = if i == 0 { BlendMode::Add } else { comp.mode };
        acc.par_iter_mut().zip(layer.par_iter()).for_each(|(a, &v)| {
            let v = if comp.inverted { 1.0 - v } else { v } * comp.value;
            *a = match mode {
                BlendMode::Add => a.max(v),
                BlendMode::Subtract => *a * (1.0 - v),
                BlendMode::Intersect => *a * v,
            };
        });
    }
    let amount = c.amount.min(1.0);
    acc.par_iter_mut().for_each(|a| *a *= amount);
    Some(acc)
}

fn component_weights(comp: &MaskComponent, frame: &Frame) -> Vec<f32> {
    let (w, h) = frame.out;
    let (sw, sh) = frame.stored();
    let analytic = |f: &(dyn Fn(f32, f32) -> f32 + Sync)| -> Vec<f32> {
        let mut out = vec![0.0f32; w * h];
        out.par_chunks_mut(w.max(1)).enumerate().for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                let (u, v) = frame.to_stored(x as f32 + 0.5, y as f32 + 0.5);
                *o = f(u * sw, v * sh);
            }
        });
        out
    };
    match &comp.shape {
        MaskShape::Linear { zero, full } => {
            let (zx, zy) = (zero.0 * sw, zero.1 * sh);
            let (dx, dy) = (full.0 * sw - zx, full.1 * sh - zy);
            let len2 = (dx * dx + dy * dy).max(1e-6);
            analytic(&|x, y| smoothstep(0.0, 1.0, ((x - zx) * dx + (y - zy) * dy) / len2))
        }
        MaskShape::Radial { top, left, bottom, right, angle, feather, .. } => {
            let (cx, cy) = ((left + right) / 2.0 * sw, (top + bottom) / 2.0 * sh);
            let (a, b) = (((right - left) / 2.0 * sw).abs().max(1e-3), ((bottom - top) / 2.0 * sh).abs().max(1e-3));
            let (s, c) = (-angle).to_radians().sin_cos();
            let inner = 1.0 - (feather / 100.0).clamp(0.0, 1.0);
            analytic(&|x, y| {
                let (px, py) = (x - cx, y - cy);
                let (rx, ry) = (px * c - py * s, px * s + py * c);
                let d = ((rx / a).powi(2) + (ry / b).powi(2)).sqrt();
                1.0 - smoothstep(inner, 1.0, d)
            })
        }
        MaskShape::Brush { strokes } => brush_weights(strokes, frame),
        MaskShape::Other(_) => vec![0.0; w * h],
    }
}

/// Paints the strokes in order: each dab moves the mask towards 1 (or 0
/// for erasing strokes) by its flow, full strength inside the centre
/// weight and fading to the radius.
fn brush_weights(strokes: &[Stroke], frame: &Frame) -> Vec<f32> {
    let (w, h) = frame.out;
    let mut map = vec![0.0f32; w * h];
    let scale = frame.scale();
    for s in strokes {
        let target = if s.erase { 0.0 } else { 1.0 };
        let r = (s.radius * scale).max(0.5);
        let core = s.center_weight.clamp(0.0, 1.0);
        let flow = s.flow.clamp(0.0, 1.0);
        for &(u, v) in &s.dabs {
            let (x, y) = frame.to_output(u, v);
            let (x0, x1) = (((x - r).floor().max(0.0)) as usize, ((x + r).ceil().min(w as f32 - 1.0)).max(0.0) as usize);
            let (y0, y1) = (((y - r).floor().max(0.0)) as usize, ((y + r).ceil().min(h as f32 - 1.0)).max(0.0) as usize);
            if x - r > w as f32 || y - r > h as f32 || x + r < 0.0 || y + r < 0.0 {
                continue;
            }
            for py in y0..=y1 {
                let row = &mut map[py * w..(py + 1) * w];
                for (px, m) in row.iter_mut().enumerate().take(x1 + 1).skip(x0) {
                    let d = ((px as f32 + 0.5 - x).powi(2) + (py as f32 + 0.5 - y).powi(2)).sqrt() / r;
                    if d >= 1.0 {
                        continue;
                    }
                    let a = flow * (1.0 - smoothstep(core, 1.0, d));
                    *m += (target - *m) * a;
                }
            }
        }
    }
    map
}

/// A correction prepared for the pixel loop.
#[derive(Clone, Debug)]
pub struct LocalOp {
    /// Index in `settings.masks`.
    pub index: usize,
    gain: f32,
    tone: Option<Lut1>,
    temperature: f32,
    tint: f32,
    saturation: f32,
    adjust: LocalAdjust,
}

impl LocalOp {
    pub fn new(index: usize, c: &LocalCorrection) -> Option<LocalOp> {
        if !c.active || c.amount <= 0.0 || c.adjust.is_zero() || !c.is_supported() {
            return None;
        }
        let a = &c.adjust;
        let basic = Basic { exposure: 0.0, contrast: a.contrast, highlights: a.highlights, shadows: a.shadows, whites: a.whites, blacks: a.blacks };
        let tone = (basic != Basic::default()).then(|| tone::basic(&basic));
        Some(LocalOp {
            index,
            gain: 2f32.powf(a.exposure),
            tone,
            temperature: a.temperature / 100.0,
            tint: a.tint / 100.0,
            saturation: a.saturation / 100.0,
            adjust: a.clone(),
        })
    }

    /// The point-wise sliders on a Melissa-encoded pixel, at weight `w`.
    pub fn point(&self, e: [f32; 3], w: f32) -> [f32; 3] {
        use raw::color::{srgb_eotf, srgb_oetf};
        let mut e = e;
        if self.gain != 1.0 || self.temperature != 0.0 || self.tint != 0.0 {
            let mut lin = e.map(|v| srgb_eotf(v.clamp(0.0, 1.0)));
            let gain = 1.0 + (self.gain - 1.0) * w;
            // Warmer: more red, less blue; positive tint: less green.
            let (kt, kn) = (self.temperature * w * 0.2, self.tint * w * 0.15);
            lin = [lin[0] * gain * (1.0 + kt), lin[1] * gain * (1.0 - kn), lin[2] * gain * (1.0 - kt)];
            e = lin.map(|v| srgb_oetf(v.clamp(0.0, 1.0)));
        }
        if let Some(t) = &self.tone {
            e = e.map(|v| v + (t.get(v) - v) * w);
        }
        if self.saturation != 0.0 {
            let y = local::luma(e);
            let k = 1.0 + self.saturation * w;
            e = e.map(|v| (y + (v - y) * k).clamp(0.0, 1.0));
        }
        e
    }

    /// Texture, Clarity, Dehaze and Sharpness through the weights.
    pub fn spatial(&self, img: &mut Image, weights: &[f32], px: f32) {
        let a = &self.adjust;
        if a.texture == 0.0 && a.clarity == 0.0 && a.dehaze == 0.0 && a.sharpness <= 0.0 {
            return;
        }
        let mut done = img.clone();
        if a.texture != 0.0 || a.clarity != 0.0 {
            local::local_contrast(&mut done, a.texture, a.clarity);
        }
        if a.dehaze != 0.0 {
            local::dehaze(&mut done, a.dehaze);
        }
        if a.sharpness > 0.0 {
            local::sharpen(&mut done, a.sharpness * 1.5, 1.0, 25.0, 0.0, px);
        }
        img.pixels.par_iter_mut().zip(done.pixels.par_iter()).zip(weights.par_iter()).for_each(|((p, d), &w)| {
            if w > 0.0 {
                *p = std::array::from_fn(|c| p[c] + (d[c] - p[c]) * w);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmp::masks::MaskComponent;

    fn frame(w: usize, h: usize) -> Frame {
        Frame::new(w, h, &Crop::default(), Orientation::Normal, w, h)
    }

    #[test]
    fn linear_gradient_goes_from_zero_to_full() {
        let c = LocalCorrection::new("g", vec![MaskComponent::new(MaskShape::Linear { zero: (0.5, 0.75), full: (0.5, 0.25) })]);
        let w = correction_weights(&c, &frame(100, 100)).unwrap();
        assert!(w[90 * 100 + 50] < 0.01, "{}", w[90 * 100 + 50]);
        assert!(w[10 * 100 + 50] > 0.99);
        assert!((w[50 * 100 + 50] - 0.5).abs() < 0.05);
    }

    #[test]
    fn radial_inverted_and_combined() {
        let radial = MaskShape::Radial { top: 0.25, left: 0.25, bottom: 0.75, right: 0.75, angle: 0.0, feather: 0.0, roundness: 0.0, midpoint: 50.0 };
        let c = LocalCorrection::new("r", vec![MaskComponent::new(radial.clone())]);
        let w = correction_weights(&c, &frame(100, 100)).unwrap();
        assert_eq!(w[50 * 100 + 50], 1.0);
        assert_eq!(w[5 * 100 + 5], 0.0);

        let mut inv = MaskComponent::new(radial.clone());
        inv.inverted = true;
        let w = correction_weights(&LocalCorrection::new("r", vec![inv]), &frame(100, 100)).unwrap();
        assert_eq!(w[50 * 100 + 50], 0.0);

        // A linear gradient minus the ellipse: the centre is cut out.
        let mut minus = MaskComponent::new(radial);
        minus.mode = BlendMode::Subtract;
        let all = MaskComponent::new(MaskShape::Linear { zero: (0.5, 2.0), full: (0.5, 1.5) });
        let mut c = LocalCorrection::new("s", vec![all, minus]);
        c.amount = 0.5;
        let w = correction_weights(&c, &frame(100, 100)).unwrap();
        assert_eq!(w[50 * 100 + 50], 0.0);
        assert!((w[5 * 100 + 5] - 0.5).abs() < 1e-3);
    }

    #[test]
    fn brush_paints_and_erases() {
        let stroke = |erase, dabs: Vec<(f32, f32)>| Stroke { erase, radius: 0.05, flow: 1.0, center_weight: 0.5, dabs, extra: Default::default() };
        let shape = MaskShape::Brush { strokes: vec![stroke(false, vec![(0.2, 0.5), (0.25, 0.5), (0.3, 0.5)]), stroke(true, vec![(0.3, 0.5)])] };
        let w = correction_weights(&LocalCorrection::new("b", vec![MaskComponent::new(shape)]), &frame(200, 100)).unwrap();
        assert!(w[50 * 200 + 40] > 0.99, "{}", w[50 * 200 + 40]);
        assert!(w[50 * 200 + 60] < 0.01, "erased: {}", w[50 * 200 + 60]);
        assert_eq!(w[10 * 200 + 150], 0.0);
    }

    #[test]
    fn frames_follow_crop_and_orientation() {
        let crop = Crop { has_crop: true, top: 0.0, left: 0.5, bottom: 1.0, right: 1.0, angle: 0.0 };
        let f = Frame::new(200, 100, &crop, Orientation::Normal, 100, 100);
        let (u, v) = f.to_stored(0.0, 50.0);
        assert!((u - 0.5).abs() < 1e-4 && (v - 0.5).abs() < 1e-4);
        let (x, y) = f.to_output(0.75, 0.5);
        assert!((x - 50.0).abs() < 1e-3 && (y - 50.0).abs() < 1e-3);
        // Rotated photo: the stored top-left is the oriented top-right.
        let f = Frame::new(100, 200, &Crop::default(), Orientation::Rotate90, 100, 200);
        let (x, y) = f.to_output(0.0, 0.0);
        assert!((x - 100.0).abs() < 1e-3 && y.abs() < 1e-3, "{x} {y}");
        let back = f.to_stored(x, y);
        assert!(back.0.abs() < 1e-4 && back.1.abs() < 1e-4);
        // And with an angle, there and back.
        let crop = Crop { has_crop: true, top: 0.1, left: 0.1, bottom: 0.9, right: 0.9, angle: 7.0 };
        let f = Frame::new(300, 200, &crop, Orientation::Normal, 240, 160);
        let (x, y) = f.to_output(0.4, 0.6);
        let (u, v) = f.to_stored(x, y);
        assert!((u - 0.4).abs() < 1e-4 && (v - 0.6).abs() < 1e-4, "{u} {v}");
    }

    #[test]
    fn local_exposure_brightens_where_weighted() {
        let mut c = LocalCorrection::new("e", vec![]);
        c.adjust.exposure = 1.0;
        let op = LocalOp::new(0, &c).unwrap();
        let p = [0.5f32; 3];
        assert_eq!(op.point(p, 0.0), p);
        assert!(op.point(p, 1.0)[0] > 0.6);
    }
}
