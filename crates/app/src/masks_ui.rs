//! The Masks panel and the mask editor on the image: linear and radial
//! gradient handles, the brush (with eraser), the red overlay.

use eframe::egui::{self, pos2, vec2, Color32, Painter, Pos2, Rect, Response, RichText, Stroke as Line, Ui};
use pipeline::masks::Frame;
use xmp::masks::{BlendMode, LocalCorrection, MaskComponent, MaskShape, Stroke};

use crate::i18n::{tr, trf};
use crate::theme;
use crate::viewer::ViewTool;
use crate::widgets::Slider;

/// Brush settings, shared by all masks.
#[derive(Clone, Debug)]
pub struct Brush {
    /// Radius, as a fraction of the photo's long edge.
    pub size: f32,
    /// 0..100: soft edge.
    pub feather: f32,
    /// 0..100.
    pub flow: f32,
    /// Eraser chosen in the panel (Alt erases too).
    pub erase: bool,
}

impl Default for Brush {
    fn default() -> Self {
        Brush { size: 0.04, feather: 50.0, flow: 100.0, erase: false }
    }
}

/// Panel state, not part of the edit.
#[derive(Default)]
pub struct MaskUi {
    pub selected: Option<usize>,
    /// Component of the selected mask that the handles and the brush act on.
    pub component: Option<usize>,
    /// Red overlay of the selected mask (O).
    pub overlay: bool,
    pub brush: Brush,
    /// The panel is open: the editor is on the image.
    pub open: bool,
    /// The Brush or Eraser tool is active: dragging paints.
    pub painting: bool,
    drag: Option<Drag>,
    last_dab: Option<(f32, f32)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    LinearZero,
    LinearFull,
    LinearMove,
    RadialMove,
    RadialEdgeX,
    RadialEdgeY,
    RadialRotate,
    Paint,
    Pan,
}

pub enum Kind {
    Linear,
    Radial,
    Brush,
}

/// A new component of each kind, placed where it is easy to see.
pub fn new_component(kind: Kind) -> MaskComponent {
    MaskComponent::new(match kind {
        Kind::Linear => MaskShape::Linear { zero: (0.5, 0.6), full: (0.5, 0.3) },
        Kind::Radial => MaskShape::Radial { top: 0.3, left: 0.3, bottom: 0.7, right: 0.7, angle: 0.0, feather: 50.0, roundness: 0.0, midpoint: 50.0 },
        Kind::Brush => MaskShape::Brush { strokes: Vec::new() },
    })
}

fn kind_label(c: &MaskComponent) -> &'static str {
    match &c.shape {
        MaskShape::Linear { .. } => tr("Linear gradient", "Sfumatura lineare"),
        MaskShape::Radial { .. } => tr("Radial gradient", "Sfumatura radiale"),
        MaskShape::Brush { .. } => tr("Brush", "Pennello"),
        MaskShape::Other(p) => match p.crs("What").and_then(xmp::Value::as_text) {
            Some("Mask/Image") => tr("AI mask (not supported)", "Maschera IA (non supportata)"),
            _ => tr("Unsupported mask", "Maschera non supportata"),
        },
    }
}

impl MaskUi {
    /// Adds a mask with one component and selects it.
    pub fn new_mask(&mut self, masks: &mut Vec<LocalCorrection>, kind: Kind) {
        let n = masks.len() + 1;
        masks.push(LocalCorrection::new(trf!("Mask {n}", "Maschera {n}"), vec![new_component(kind)]));
        self.selected = Some(masks.len() - 1);
        self.component = Some(0);
    }

    /// Makes a brush the target of painting: the selected mask's brush, a
    /// new brush in it, or a new mask with a brush.
    pub fn ensure_brush(&mut self, masks: &mut Vec<LocalCorrection>) {
        match self.selected.and_then(|i| masks.get(i)) {
            Some(m) => match m.components.iter().position(|c| matches!(c.shape, MaskShape::Brush { .. })) {
                Some(j) => self.component = Some(j),
                None => self.add_component(masks, Kind::Brush, BlendMode::Add),
            },
            None => self.new_mask(masks, Kind::Brush),
        }
    }

    /// Adds a component to the selected mask.
    pub fn add_component(&mut self, masks: &mut [LocalCorrection], kind: Kind, mode: BlendMode) {
        let Some(c) = self.selected.and_then(|i| masks.get_mut(i)) else { return };
        let mut comp = new_component(kind);
        comp.mode = mode;
        c.components.push(comp);
        self.component = Some(c.components.len() - 1);
    }

    /// The panel. `masks` is the edit's list.
    pub fn panel(&mut self, ui: &mut Ui, masks: &mut Vec<LocalCorrection>) {
        if self.selected.is_some_and(|i| i >= masks.len()) {
            self.selected = None;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(tr("New:", "Nuova:"));
            if ui.button(tr("Linear", "Lineare")).clicked() {
                self.new_mask(masks, Kind::Linear);
            }
            if ui.button(tr("Radial", "Radiale")).clicked() {
                self.new_mask(masks, Kind::Radial);
            }
            if ui.button(tr("Brush", "Pennello")).clicked() {
                self.new_mask(masks, Kind::Brush);
            }
        });
        if masks.is_empty() {
            ui.label(RichText::new(tr("No masks. A mask applies its own sliders to part of the photo.", "Nessuna maschera. Una maschera applica i suoi cursori a una parte della foto.")).color(theme::TEXT_DIM).size(11.5));
            return;
        }
        let mut delete = None;
        for (i, c) in masks.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let selected = self.selected == Some(i);
                ui.checkbox(&mut c.active, "").on_hover_text(tr("Show / hide the effect", "Mostra / nascondi l'effetto"));
                let mut label = c.name.clone();
                if !c.is_supported() {
                    label.push_str("  ⚠");
                }
                let r = ui.selectable_label(selected, label);
                if !c.is_supported() {
                    r.clone().on_hover_text(tr("Contains a mask Lumen cannot render (AI or range): it is kept in the file but not applied", "Contiene una maschera che Lumen non sa applicare (IA o intervallo): resta nel file ma non viene applicata"));
                }
                if r.clicked() {
                    self.selected = if selected { None } else { Some(i) };
                    self.component = Some(0);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("×").on_hover_text(tr("Delete the mask", "Elimina la maschera")).clicked() {
                        delete = Some(i);
                    }
                });
            });
        }
        if let Some(i) = delete {
            masks.remove(i);
            self.selected = None;
            return;
        }
        let Some(sel) = self.selected else { return };
        ui.separator();
        ui.push_id(("mask", sel), |ui| self.selected_mask(ui, &mut masks[sel]));
    }

    fn selected_mask(&mut self, ui: &mut Ui, c: &mut LocalCorrection) {
        ui.horizontal(|ui| {
            ui.label(tr("Name", "Nome"));
            ui.add(egui::TextEdit::singleline(&mut c.name).desired_width(f32::INFINITY));
        });
        ui.toggle_value(&mut self.overlay, tr("Show overlay  O", "Mostra sovrapposizione  O")).on_hover_text(tr("Show the mask in red (O)", "Mostra la maschera in rosso (O)"));
        let mut amount = c.amount * 100.0;
        if Slider::new(tr("Amount", "Quantità"), 0.0..=100.0, 100.0).suffix("%").show(ui, &mut amount).changed() {
            c.amount = amount / 100.0;
        }

        ui.label(RichText::new(tr("Components", "Componenti")).color(theme::TEXT_DIM).size(11.5));
        let mut remove = None;
        for (i, comp) in c.components.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let selected = self.component == Some(i);
                if ui.selectable_label(selected, kind_label(comp)).clicked() {
                    self.component = Some(i);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("×").on_hover_text(tr("Remove", "Rimuovi")).clicked() {
                        remove = Some(i);
                    }
                    ui.checkbox(&mut comp.inverted, tr("Inv.", "Inv.")).on_hover_text(tr("Invert", "Inverti"));
                    if i > 0 {
                        egui::ComboBox::from_id_salt(("mode", i))
                            .width(80.0)
                            .selected_text(mode_label(comp.mode))
                            .show_ui(ui, |ui| {
                                for m in [BlendMode::Add, BlendMode::Subtract, BlendMode::Intersect] {
                                    ui.selectable_value(&mut comp.mode, m, mode_label(m));
                                }
                            });
                    }
                });
            });
        }
        if let Some(i) = remove {
            c.components.remove(i);
            self.component = (!c.components.is_empty()).then_some(0);
        }
        ui.horizontal(|ui| {
            for (mode, label) in [(BlendMode::Add, tr("Add", "Aggiungi")), (BlendMode::Subtract, tr("Subtract", "Sottrai")), (BlendMode::Intersect, tr("Intersect", "Interseca"))] {
                ui.menu_button(format!("{label} ▾"), |ui| {
                    for (kind, name) in [(Kind::Linear, tr("Linear gradient", "Sfumatura lineare")), (Kind::Radial, tr("Radial gradient", "Sfumatura radiale")), (Kind::Brush, tr("Brush", "Pennello"))] {
                        if ui.button(name).clicked() {
                            ui.close();
                            let mut comp = new_component(kind);
                            comp.mode = mode;
                            c.components.push(comp);
                            self.component = Some(c.components.len() - 1);
                        }
                    }
                });
            }
        });

        // Settings of the selected component.
        match self.component.and_then(|i| c.components.get_mut(i)).map(|c| &mut c.shape) {
            Some(MaskShape::Radial { feather, .. }) => {
                Slider::new(tr("Feather", "Sfumatura"), 0.0..=100.0, 50.0).show(ui, feather);
            }
            Some(MaskShape::Brush { .. }) => {
                ui.label(RichText::new(tr("Brush: drag on the photo; Alt or Eraser to erase; [ ] or the wheel for the size", "Pennello: trascina sulla foto; Alt o Gomma per cancellare; [ ] o la rotella per la dimensione")).color(theme::TEXT_DIM).size(11.0));
                let b = &mut self.brush;
                let mut size = b.size * 100.0;
                if Slider::new(tr("Size", "Dimensione"), 0.5..=30.0, 4.0).decimals(1).suffix("%").show(ui, &mut size).changed() {
                    b.size = size / 100.0;
                }
                Slider::new(tr("Feather", "Sfumatura"), 0.0..=100.0, 50.0).show(ui, &mut b.feather);
                Slider::new(tr("Flow", "Flusso"), 1.0..=100.0, 100.0).show(ui, &mut b.flow);
                ui.toggle_value(&mut b.erase, tr("Eraser", "Gomma"));
            }
            _ => {}
        }

        ui.add_space(4.0);
        ui.label(RichText::new(tr("Adjustments", "Regolazioni")).color(theme::TEXT_DIM).size(11.5));
        let a = &mut c.adjust;
        let temp = [Color32::from_rgb(60, 110, 220), Color32::from_rgb(225, 200, 70)];
        let tint = [Color32::from_rgb(70, 180, 70), Color32::from_rgb(210, 70, 200)];
        Slider::new(tr("Temp.", "Temp."), -100.0..=100.0, 0.0).colors(&temp).show(ui, &mut a.temperature);
        Slider::new(tr("Tint", "Tinta"), -100.0..=100.0, 0.0).colors(&tint).show(ui, &mut a.tint);
        Slider::new(tr("Exposure", "Esposizione"), -4.0..=4.0, 0.0).decimals(2).show(ui, &mut a.exposure);
        Slider::new(tr("Contrast", "Contrasto"), -100.0..=100.0, 0.0).show(ui, &mut a.contrast);
        Slider::new(tr("Highlights", "Luci"), -100.0..=100.0, 0.0).show(ui, &mut a.highlights);
        Slider::new(tr("Shadows", "Ombre"), -100.0..=100.0, 0.0).show(ui, &mut a.shadows);
        Slider::new(tr("Whites", "Bianchi"), -100.0..=100.0, 0.0).show(ui, &mut a.whites);
        Slider::new(tr("Blacks", "Neri"), -100.0..=100.0, 0.0).show(ui, &mut a.blacks);
        Slider::new(tr("Saturation", "Saturazione"), -100.0..=100.0, 0.0).show(ui, &mut a.saturation);
        Slider::new(tr("Texture", "Texture"), -100.0..=100.0, 0.0).show(ui, &mut a.texture);
        Slider::new(tr("Clarity", "Chiarezza"), -100.0..=100.0, 0.0).show(ui, &mut a.clarity);
        Slider::new(tr("Dehaze", "Foschia"), -100.0..=100.0, 0.0).show(ui, &mut a.dehaze);
        Slider::new(tr("Sharpness", "Nitidezza"), 0.0..=100.0, 0.0).show(ui, &mut a.sharpness);
    }
}

fn mode_label(m: BlendMode) -> &'static str {
    match m {
        BlendMode::Add => tr("Add", "Aggiungi"),
        BlendMode::Subtract => tr("Subtract", "Sottrai"),
        BlendMode::Intersect => tr("Intersect", "Interseca"),
    }
}

/// The selected mask on the image: handles for gradients, the brush.
pub struct MaskEditor<'a> {
    pub mask: &'a mut LocalCorrection,
    pub ui: &'a mut MaskUi,
    /// Maps the shown (developed) image to the stored image.
    pub frame: Frame,
    /// Size of the shown image, pixels.
    pub shown: [usize; 2],
    /// Stored-orientation size of the photo, pixels (for shapes).
    pub stored: [f32; 2],
}

impl MaskEditor<'_> {
    fn to_screen(&self, img: Rect, (u, v): (f32, f32)) -> Pos2 {
        let (x, y) = self.frame.to_output(u, v);
        pos2(img.left() + x / self.shown[0] as f32 * img.width(), img.top() + y / self.shown[1] as f32 * img.height())
    }

    fn to_stored(&self, img: Rect, p: Pos2) -> (f32, f32) {
        let x = (p.x - img.left()) / img.width() * self.shown[0] as f32;
        let y = (p.y - img.top()) / img.height() * self.shown[1] as f32;
        self.frame.to_stored(x, y)
    }

    /// Screen pixels per stored-image long edge.
    fn screen_scale(&self, img: Rect) -> f32 {
        self.frame.scale() * img.width() / self.shown[0] as f32
    }

    fn component(&mut self) -> Option<&mut MaskComponent> {
        let i = self.ui.component?;
        self.mask.components.get_mut(i)
    }

    /// Points of a radial gradient's ellipse in stored fractions.
    fn ellipse(&self, (top, left, bottom, right, angle): (f32, f32, f32, f32, f32), scale: f32) -> Vec<(f32, f32)> {
        let [sw, sh] = self.stored;
        let (cx, cy) = ((left + right) / 2.0 * sw, (top + bottom) / 2.0 * sh);
        let (a, b) = ((right - left) / 2.0 * sw * scale, (bottom - top) / 2.0 * sh * scale);
        let (s, c) = angle.to_radians().sin_cos();
        (0..=64)
            .map(|i| {
                let t = i as f32 / 64.0 * std::f32::consts::TAU;
                let (x, y) = (a * t.cos(), b * t.sin());
                ((cx + x * c - y * s) / sw, (cy + x * s + y * c) / sh)
            })
            .collect()
    }
}

impl ViewTool for MaskEditor<'_> {
    fn takes_scroll(&self) -> bool {
        self.ui.painting
            && matches!(self.ui.component.and_then(|i| self.mask.components.get(i)).map(|c| &c.shape), Some(MaskShape::Brush { .. }))
    }

    fn interact(&mut self, ui: &Ui, resp: &Response, img: Rect) -> (bool, bool) {
        let alt = ui.input(|i| i.modifiers.alt);
        let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
        let [sw, sh] = self.stored;
        let mut changed = false;

        // Brush size with the wheel and [ ].
        // (Alt + wheel zooms instead.)
        if self.takes_scroll() && resp.hovered() && !alt {
            let (scroll, smaller, bigger) = ui.input(|i| (i.smooth_scroll_delta.y, i.key_pressed(egui::Key::OpenBracket), i.key_pressed(egui::Key::CloseBracket)));
            let mut k = (1.0 + scroll * 0.004).clamp(0.5, 2.0);
            if smaller {
                k *= 0.85;
            }
            if bigger {
                k /= 0.85;
            }
            self.ui.brush.size = (self.ui.brush.size * k).clamp(0.005, 0.3);
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
        }

        if resp.drag_started() {
            let Some(p) = pointer else { return (false, false) };
            let near = |q: Pos2| q.distance(p) < 12.0;
            let shape = self.ui.component.and_then(|i| self.mask.components.get(i)).map(|c| c.shape.clone());
            self.ui.drag = match shape {
                Some(MaskShape::Linear { zero, full }) => {
                    let (z, f) = (self.to_screen(img, zero), self.to_screen(img, full));
                    if near(z) {
                        Some(Drag::LinearZero)
                    } else if near(f) {
                        Some(Drag::LinearFull)
                    } else if near(z.lerp(f, 0.5)) {
                        Some(Drag::LinearMove)
                    } else {
                        Some(Drag::Pan)
                    }
                }
                Some(MaskShape::Radial { top, left, bottom, right, angle, .. }) => {
                    let e = self.ellipse((top, left, bottom, right, angle), 1.0);
                    let c = self.to_screen(img, ((left + right) / 2.0, (top + bottom) / 2.0));
                    let ex = self.to_screen(img, e[0]);
                    let ey = self.to_screen(img, e[16]);
                    let rot = self.to_screen(img, e[48]);
                    let rot = rot + (rot - c).normalized() * 18.0;
                    if near(c) {
                        Some(Drag::RadialMove)
                    } else if near(ex) {
                        Some(Drag::RadialEdgeX)
                    } else if near(ey) {
                        Some(Drag::RadialEdgeY)
                    } else if near(rot) {
                        Some(Drag::RadialRotate)
                    } else {
                        Some(Drag::Pan)
                    }
                }
                Some(MaskShape::Brush { .. }) if img.contains(p) && self.ui.painting => {
                    let b = &self.ui.brush;
                    let stroke = Stroke {
                        erase: b.erase || alt,
                        radius: b.size,
                        flow: b.flow / 100.0,
                        center_weight: 1.0 - b.feather / 100.0,
                        dabs: vec![self.to_stored(img, p)],
                        extra: Default::default(),
                    };
                    self.ui.last_dab = stroke.dabs.first().copied();
                    if let Some(MaskComponent { shape: MaskShape::Brush { strokes }, .. }) = self.component() {
                        strokes.push(stroke);
                        changed = true;
                    }
                    Some(Drag::Paint)
                }
                _ => Some(Drag::Pan),
            };
        }
        if !resp.dragged() {
            self.ui.drag = None;
            return (false, changed);
        }
        let (Some(drag), Some(p)) = (self.ui.drag, pointer) else { return (false, changed) };
        if drag == Drag::Pan {
            return (false, changed);
        }
        let at = self.to_stored(img, p);
        let delta = resp.drag_delta();
        let before = self.to_stored(img, p - delta);
        let (du, dv) = (at.0 - before.0, at.1 - before.1);
        let size = self.ui.brush.size;
        let last = self.ui.last_dab;
        let Some(comp) = self.component() else { return (true, changed) };
        match (&mut comp.shape, drag) {
            (MaskShape::Linear { zero, .. }, Drag::LinearZero) => *zero = at,
            (MaskShape::Linear { full, .. }, Drag::LinearFull) => *full = at,
            (MaskShape::Linear { zero, full }, Drag::LinearMove) => {
                *zero = (zero.0 + du, zero.1 + dv);
                *full = (full.0 + du, full.1 + dv);
            }
            (MaskShape::Radial { top, left, bottom, right, .. }, Drag::RadialMove) => {
                *top += dv;
                *bottom += dv;
                *left += du;
                *right += du;
            }
            (MaskShape::Radial { top, left, bottom, right, angle, .. }, Drag::RadialEdgeX | Drag::RadialEdgeY) => {
                let (cx, cy) = ((*left + *right) / 2.0 * sw, (*top + *bottom) / 2.0 * sh);
                let (s, c) = angle.to_radians().sin_cos();
                let (px, py) = (at.0 * sw - cx, at.1 * sh - cy);
                // Distance along the ellipse's own axes.
                let (ax, ay) = (px * c + py * s, -px * s + py * c);
                if drag == Drag::RadialEdgeX {
                    let a = ax.abs().max(4.0) / sw;
                    (*left, *right) = (cx / sw - a, cx / sw + a);
                } else {
                    let b = ay.abs().max(4.0) / sh;
                    (*top, *bottom) = (cy / sh - b, cy / sh + b);
                }
            }
            (MaskShape::Radial { top, left, bottom, right, angle, .. }, Drag::RadialRotate) => {
                let (cx, cy) = ((*left + *right) / 2.0 * sw, (*top + *bottom) / 2.0 * sh);
                // The handle sits on the ellipse's −y axis.
                *angle = ((at.1 * sh - cy).atan2(at.0 * sw - cx).to_degrees() + 90.0).rem_euclid(360.0);
                if *angle > 180.0 {
                    *angle -= 360.0;
                }
            }
            (MaskShape::Brush { strokes }, Drag::Paint) => {
                if let (Some(s), Some(l)) = (strokes.last_mut(), last) {
                    // Dabs every quarter of the radius along the path.
                    let long = sw.max(sh);
                    let step = (size * long / 4.0).max(1.0);
                    let d = ((at.0 - l.0) * sw).hypot((at.1 - l.1) * sh);
                    let n = (d / step).floor() as usize;
                    for k in 1..=n {
                        let t = k as f32 * step / d;
                        s.dabs.push((l.0 + (at.0 - l.0) * t, l.1 + (at.1 - l.1) * t));
                    }
                    if n > 0 {
                        self.ui.last_dab = s.dabs.last().copied();
                    }
                }
            }
            _ => return (true, changed),
        }
        (true, true)
    }

    fn paint(&self, ui: &Ui, painter: &Painter, img: Rect) {
        let white = Line::new(1.5, Color32::WHITE);
        let faint = Line::new(1.0, Color32::from_white_alpha(120));
        let handle = |p: Pos2, filled: bool| {
            painter.circle(p, 5.0, if filled { theme::ACCENT } else { Color32::from_black_alpha(160) }, Line::new(1.5, Color32::WHITE));
        };
        let selected = self.ui.component;
        for (i, comp) in self.mask.components.iter().enumerate() {
            let active = selected == Some(i);
            match &comp.shape {
                MaskShape::Linear { zero, full } => {
                    let (z, f) = (self.to_screen(img, *zero), self.to_screen(img, *full));
                    let dir = (f - z).normalized();
                    let normal = vec2(-dir.y, dir.x) * (img.width() + img.height());
                    let clip = painter.with_clip_rect(img);
                    for (p, line) in [(z, faint), (z.lerp(f, 0.5), if active { white } else { faint }), (f, faint)] {
                        clip.line_segment([p - normal, p + normal], line);
                    }
                    if active {
                        handle(z, false);
                        handle(f, false);
                        handle(z.lerp(f, 0.5), true);
                    }
                }
                MaskShape::Radial { top, left, bottom, right, angle, feather, .. } => {
                    let params = (*top, *left, *bottom, *right, *angle);
                    let outer: Vec<Pos2> = self.ellipse(params, 1.0).into_iter().map(|q| self.to_screen(img, q)).collect();
                    painter.add(egui::Shape::line(outer.clone(), if active { white } else { faint }));
                    if active {
                        let inner_k = (1.0 - feather / 100.0).max(0.02);
                        let inner: Vec<Pos2> = self.ellipse(params, inner_k).into_iter().map(|q| self.to_screen(img, q)).collect();
                        painter.add(egui::Shape::dashed_line(&inner, faint, 6.0, 4.0));
                        let c = self.to_screen(img, ((left + right) / 2.0, (top + bottom) / 2.0));
                        handle(c, true);
                        handle(outer[0], false);
                        handle(outer[16], false);
                        let rot = outer[48] + (outer[48] - c).normalized() * 18.0;
                        painter.line_segment([outer[48], rot], faint);
                        handle(rot, false);
                    }
                }
                MaskShape::Brush { .. } if active && self.ui.painting => {
                    if let Some(p) = ui.input(|i| i.pointer.hover_pos()).filter(|p| img.contains(*p)) {
                        let r = self.ui.brush.size * self.screen_scale(img);
                        let erase = self.ui.brush.erase || ui.input(|i| i.modifiers.alt);
                        painter.circle_stroke(p, r, Line::new(1.5, if erase { Color32::from_rgb(255, 120, 120) } else { Color32::WHITE }));
                        painter.circle_stroke(p, r * (1.0 - self.ui.brush.feather / 100.0), faint);
                        if erase {
                            painter.text(p + vec2(r + 4.0, 0.0), egui::Align2::LEFT_CENTER, "−", egui::FontId::proportional(16.0), Color32::WHITE);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_masks_and_components() {
        let mut ui = MaskUi::default();
        let mut masks = Vec::new();
        ui.new_mask(&mut masks, Kind::Radial);
        ui.add_component(&mut masks, Kind::Brush, BlendMode::Subtract);
        assert_eq!(masks.len(), 1);
        assert_eq!(masks[0].components.len(), 2);
        assert_eq!(masks[0].components[1].mode, BlendMode::Subtract);
        assert_eq!(ui.component, Some(1));
    }
}
