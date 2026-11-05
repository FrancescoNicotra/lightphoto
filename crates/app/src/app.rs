//! The application: layout (menu, presets, image, develop panels,
//! filmstrip), events from the background threads, shortcuts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, RichText, TextureHandle, TextureOptions, Ui};
use export::ExportOptions;
use pipeline::adobe::AdobeProfiles;
use pipeline::SourceKind;
use raw::PhotoInfo;

use crate::export_ui::{self, DialogResult, ExportEvent, ExportSettings};
use crate::library::{self, PresetEntry, PresetKind};
use crate::panels::{self, PanelAction, PanelInput, PanelState};
use crate::script::{Script, Step};
use crate::state::{self, Edit, History, LutRef};
use crate::theme;
use crate::viewer::{self, ViewInput, Viewer};
use crate::worker::{Event, Histogram, Quality, RenderJob, Worker, PREVIEW_SIZE};
use crate::Args;

const KEY_PHOTO_FOLDER: &str = "photo_folder";
const KEY_PRESET_FOLDER: &str = "preset_folder";
const KEY_EXPORT: &str = "export";

/// The open photo.
struct Current {
    path: PathBuf,
    generation: u64,
    kind: SourceKind,
    loaded: bool,
    error: Option<String>,
    info: Option<PhotoInfo>,
    full_size: [usize; 2],
    decode_ms: u128,
    edit: Edit,
    history: History,
}

/// A texture and the render request it shows.
struct Shown {
    tex: TextureHandle,
    id: u64,
}

pub struct LumenApp {
    adobe: AdobeProfiles,
    worker: Worker,

    folder: Option<PathBuf>,
    photos: Vec<PathBuf>,
    thumbs: Vec<Option<TextureHandle>>,
    thumb_generation: Arc<AtomicU64>,
    thumb_tx: Sender<(u64, usize, Option<egui::ColorImage>)>,
    thumb_rx: Receiver<(u64, usize, Option<egui::ColorImage>)>,
    /// Thumbnails not loaded (or failed) yet.
    thumbs_pending: usize,
    selected: Option<usize>,
    scroll_to_selected: bool,

    current: Option<Current>,
    /// Edits of the other photos opened in this session.
    edits: HashMap<PathBuf, (Edit, History)>,
    generation: u64,

    job_id: u64,
    /// The last render request: edit, detail, draft.
    last_sent: Option<(Edit, bool, bool)>,
    after: Option<Shown>,
    detail: Option<Shown>,
    before: Option<TextureHandle>,
    histogram: Option<Histogram>,
    render_ms: u128,
    render_quality: Quality,
    white_balance: Option<(f32, f32)>,
    notes: Vec<String>,
    wants_detail: bool,

    preset_folder: Option<PathBuf>,
    presets: Vec<PresetEntry>,
    preset_rx: Option<Receiver<Vec<PresetEntry>>>,
    preset_filter: String,
    hover_preset: Option<usize>,
    hover_cache: Option<(usize, Edit, Edit)>,

    viewer: Viewer,
    panel_state: PanelState,

    export: ExportSettings,
    export_open: bool,
    export_tx: Sender<ExportEvent>,
    export_rx: Receiver<ExportEvent>,
    exporting: usize,

    message: Option<(String, Instant, bool)>,
    title: String,
    script: Option<Script>,
}

impl LumenApp {
    pub fn new(cc: &eframe::CreationContext<'_>, args: Args) -> LumenApp {
        theme::apply(&cc.egui_ctx);
        let adobe = AdobeProfiles::discover();
        let worker = Worker::spawn(cc.egui_ctx.clone(), adobe.clone());
        let (thumb_tx, thumb_rx) = mpsc::channel();
        let (export_tx, export_rx) = mpsc::channel();
        let storage = cc.storage;
        let get = |k: &str| storage.and_then(|s| s.get_string(k)).filter(|v| !v.is_empty());

        let mut app = LumenApp {
            adobe,
            worker,
            folder: None,
            photos: Vec::new(),
            thumbs: Vec::new(),
            thumb_generation: Arc::new(AtomicU64::new(0)),
            thumb_tx,
            thumb_rx,
            thumbs_pending: 0,
            selected: None,
            scroll_to_selected: false,
            current: None,
            edits: HashMap::new(),
            generation: 0,
            job_id: 0,
            last_sent: None,
            after: None,
            detail: None,
            before: None,
            histogram: None,
            render_ms: 0,
            render_quality: Quality::Preview,
            white_balance: None,
            notes: Vec::new(),
            wants_detail: false,
            preset_folder: None,
            presets: Vec::new(),
            preset_rx: None,
            preset_filter: String::new(),
            hover_preset: None,
            hover_cache: None,
            viewer: Viewer::default(),
            panel_state: PanelState::default(),
            export: get(KEY_EXPORT).map(|s| ExportSettings::from_storage(&s)).unwrap_or_default(),
            export_open: false,
            export_tx,
            export_rx,
            exporting: 0,
            message: None,
            title: String::new(),
            script: Script::from_env(),
        };

        if let Some(dir) = args.presets.or_else(|| get(KEY_PRESET_FOLDER).map(PathBuf::from)) {
            app.set_preset_folder(dir, &cc.egui_ctx);
        }
        match args.open.or_else(|| get(KEY_PHOTO_FOLDER).map(PathBuf::from).filter(|p| p.is_dir())) {
            Some(p) if p.is_dir() => app.open_folder(p, None, &cc.egui_ctx),
            Some(p) if p.is_file() => app.open_file(p, &cc.egui_ctx),
            Some(p) => app.say(format!("{} non esiste", p.display()), true),
            None => {}
        }
        if app.adobe.is_empty() {
            app.say("Nessuna installazione di Lightroom/Camera Raw trovata: si usano i profili del file", false);
        }
        app
    }

    fn say(&mut self, text: impl Into<String>, error: bool) {
        self.message = Some((text.into(), Instant::now(), error));
    }

    // ------------------------------------------------------------------
    // Folders and photos

    fn open_folder(&mut self, dir: PathBuf, select: Option<&Path>, ctx: &egui::Context) {
        self.stash_current();
        self.photos = library::scan_photos(&dir);
        self.thumbs = vec![None; self.photos.len()];
        self.thumbs_pending = self.photos.len();
        let generation = self.thumb_generation.fetch_add(1, Ordering::Relaxed) + 1;
        library::spawn_thumbnails(self.photos.clone(), generation, self.thumb_generation.clone(), self.thumb_tx.clone(), ctx.clone());
        self.folder = Some(dir);
        let index = select.and_then(|s| self.photos.iter().position(|p| p == s)).or((!self.photos.is_empty()).then_some(0));
        match index {
            Some(i) => self.open_photo(i),
            None => {
                self.current = None;
                self.selected = None;
                self.say("Nessuna foto nella cartella", false);
            }
        }
    }

    fn open_file(&mut self, file: PathBuf, ctx: &egui::Context) {
        let dir = file.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let file = std::fs::canonicalize(&file).unwrap_or(file);
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        self.open_folder(dir, Some(&file), ctx);
    }

    /// Keeps the edit of the open photo for when it is opened again.
    fn stash_current(&mut self) {
        if let Some(c) = self.current.take() {
            self.edits.insert(c.path, (c.edit, c.history));
        }
    }

    fn open_photo(&mut self, index: usize) {
        let Some(path) = self.photos.get(index).cloned() else { return };
        if self.current.as_ref().is_some_and(|c| c.path == path) {
            return;
        }
        self.stash_current();
        let kind = if raw::is_rendered_image(&path) { SourceKind::Rendered } else { SourceKind::Raw };
        let (edit, history) = self.edits.remove(&path).unwrap_or_else(|| {
            let edit = state::load_sidecar(&path, kind).unwrap_or_else(|| Edit::defaults(kind));
            let history = History::new(&edit);
            (edit, history)
        });
        self.generation += 1;
        self.worker.load(path.clone(), self.generation);
        self.current = Some(Current {
            path,
            generation: self.generation,
            kind,
            loaded: false,
            error: None,
            info: None,
            full_size: [0, 0],
            decode_ms: 0,
            edit,
            history,
        });
        self.selected = Some(index);
        self.scroll_to_selected = true;
        self.after = None;
        self.detail = None;
        self.before = None;
        self.histogram = None;
        self.white_balance = None;
        self.notes.clear();
        self.last_sent = None;
        self.hover_cache = None;
        self.viewer.reset();
    }

    fn step_photo(&mut self, delta: isize) {
        if self.photos.is_empty() {
            return;
        }
        let i = self.selected.map_or(0, |i| (i as isize + delta).clamp(0, self.photos.len() as isize - 1) as usize);
        self.open_photo(i);
    }

    fn set_preset_folder(&mut self, dir: PathBuf, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let d = dir.clone();
        std::thread::spawn(move || {
            tx.send(library::scan_presets(&d)).ok();
            ctx.request_repaint();
        });
        self.preset_rx = Some(rx);
        self.preset_folder = Some(dir);
        self.hover_cache = None;
    }

    // ------------------------------------------------------------------
    // Editing

    fn apply_preset(&mut self, index: usize) {
        let Some(entry) = self.presets.get(index).cloned() else { return };
        let Some(c) = &mut self.current else { return };
        match apply_entry(&mut c.edit, &entry) {
            Ok(()) => {
                c.history.checkpoint(&c.edit);
                self.say(format!("Preset applicato: {}", entry.name), false);
            }
            Err(e) => self.say(format!("{}: {e}", entry.name), true),
        }
    }

    fn load_lut_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("LUT", &["cube", "3dl"]).set_title("Carica LUT").pick_file() else { return };
        match lut::load(&path) {
            Ok(cube) => {
                if let Some(c) = &mut self.current {
                    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    c.edit.lut = Some(LutRef { path, name, cube: Arc::new(cube) });
                    c.edit.lut_amount = 100.0;
                    c.history.checkpoint(&c.edit);
                }
            }
            Err(e) => self.say(format!("{}: {e}", path.display()), true),
        }
    }

    fn undo(&mut self) {
        if let Some(c) = &mut self.current {
            if !c.history.undo(&mut c.edit) {
                self.say("Niente da annullare", false);
            }
        }
    }

    fn redo(&mut self) {
        if let Some(c) = &mut self.current {
            if !c.history.redo(&mut c.edit) {
                self.say("Niente da ripetere", false);
            }
        }
    }

    fn reset_edit(&mut self) {
        if let Some(c) = &mut self.current {
            c.edit.reset(c.kind);
            c.history.checkpoint(&c.edit);
        }
    }

    fn save_sidecar(&mut self) {
        let Some(c) = &self.current else { return };
        match state::save_sidecar(&c.path, &c.edit) {
            Ok(p) => self.say(format!("Impostazioni salvate in {}", p.display()), false),
            Err(e) => self.say(e, true),
        }
    }

    fn start_export(&mut self, ctx: &egui::Context) {
        let Some(c) = &self.current else { return };
        if !c.loaded {
            return;
        }
        let out = self.export.output_path(&c.path, c.full_size, c.edit.preset_name.as_deref());
        let opts = ExportOptions { size: self.export.size(), format: self.export.format(), sharpen: self.export.sharpen };
        export_ui::spawn_export(c.path.clone(), c.edit.clone(), out.clone(), opts, self.adobe.clone(), self.export_tx.clone(), ctx.clone());
        self.exporting += 1;
        self.say(format!("Esportazione di {}…", file_name(&out)), false);
    }

    // ------------------------------------------------------------------
    // Background events

    fn poll(&mut self, ctx: &egui::Context) {
        let tex_opts = TextureOptions { mipmap_mode: Some(egui::TextureFilter::Linear), ..TextureOptions::LINEAR };
        while let Ok(event) = self.worker.events.try_recv() {
            let Some(c) = &mut self.current else { continue };
            match event {
                Event::Loaded { generation, path, info, kind, size, decode_ms } if generation == c.generation && path == c.path => {
                    if kind != c.kind {
                        // The file is not what its extension says.
                        c.kind = kind;
                        c.edit = Edit::defaults(kind);
                        c.history = History::new(&c.edit);
                    }
                    c.loaded = true;
                    c.info = Some(*info);
                    c.full_size = size;
                    c.decode_ms = decode_ms;
                }
                Event::Failed { generation, error } if generation == c.generation => {
                    c.error = Some(error);
                }
                Event::Before { generation, image } if generation == c.generation => {
                    set_texture(ctx, &mut self.before, "before", image, tex_opts);
                }
                Event::Rendered(r) if r.generation == c.generation => {
                    if self.script.is_some() {
                        eprintln!("render {:?} {} × {}: {} ms", r.quality, r.image.size[0], r.image.size[1], r.ms);
                    }
                    let slot = if r.quality == Quality::Full { &mut self.detail } else { &mut self.after };
                    let mut tex = slot.take().map(|s| s.tex);
                    set_texture(ctx, &mut tex, "photo", r.image, tex_opts);
                    *slot = tex.map(|tex| Shown { tex, id: r.id });
                    if r.quality != Quality::Full {
                        self.histogram = Some(r.histogram);
                    }
                    self.render_ms = r.ms;
                    self.render_quality = r.quality;
                    self.white_balance = r.white_balance;
                    self.notes = r.notes;
                }
                _ => {}
            }
        }
        while let Ok((generation, i, image)) = self.thumb_rx.try_recv() {
            if generation == self.thumb_generation.load(Ordering::Relaxed) && i < self.thumbs.len() {
                self.thumbs_pending = self.thumbs_pending.saturating_sub(1);
                self.thumbs[i] = image.map(|image| ctx.load_texture(format!("thumb-{i}"), image, tex_opts));
            }
        }
        if let Some(rx) = &self.preset_rx {
            if let Ok(list) = rx.try_recv() {
                self.presets = list;
                self.preset_rx = None;
            }
        }
        while let Ok(e) = self.export_rx.try_recv() {
            self.exporting = self.exporting.saturating_sub(1);
            match e {
                ExportEvent::Done { path, size, ms } => {
                    self.say(format!("Esportato {} ({} × {}, {:.1} s)", path.display(), size[0], size[1], ms as f64 / 1000.0), false)
                }
                ExportEvent::Failed { path, error } => self.say(format!("Esportazione di {} fallita: {error}", path.display()), true),
            }
        }
    }

    /// Sends a render when what should be on screen changed.
    fn request_render(&mut self, ctx: &egui::Context) {
        let Some(c) = &self.current else { return };
        if c.error.is_some() {
            return;
        }
        let pointer_down = ctx.input(|i| i.pointer.any_down());
        // Hovering a preset previews it, as in Lightroom.
        let hover = self.hover_preset.filter(|_| !pointer_down).and_then(|i| {
            let fresh = self.hover_cache.as_ref().is_some_and(|(j, base, _)| *j == i && *base == c.edit);
            if !fresh {
                let mut e = c.edit.clone();
                apply_entry(&mut e, self.presets.get(i)?).ok()?;
                self.hover_cache = Some((i, c.edit.clone(), e));
            }
            self.hover_cache.as_ref().map(|(_, _, e)| e)
        });
        let target = hover.unwrap_or(&c.edit);
        let detail = self.wants_detail && !self.viewer.show_before;
        let draft = (pointer_down && ctx.dragged_id().is_some()) || self.script.as_ref().is_some_and(Script::dragging);
        if let Some((e, d, was_draft)) = &self.last_sent {
            // Same request: resend only to replace a draft by the preview.
            if e == target && *d == detail && (draft || !*was_draft) {
                return;
            }
        }
        self.job_id += 1;
        let job = RenderJob { id: self.job_id, generation: c.generation, edit: target.clone(), draft, detail };
        self.last_sent = Some((target.clone(), detail, draft));
        self.worker.render(job);
    }

    // ------------------------------------------------------------------
    // Input

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = Modifiers::COMMAND;
        let shortcut = |m, k| KeyboardShortcut::new(m, k);
        let pressed = |s: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&s));
        // Menu shortcuts work even while a text field has focus.
        if pressed(shortcut(cmd | Modifiers::SHIFT, Key::E)) {
            self.export_open = self.current.is_some();
        }
        if pressed(shortcut(cmd, Key::O)) {
            self.pick_folder(ctx);
        }
        if pressed(shortcut(cmd, Key::S)) {
            self.save_sidecar();
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        if pressed(shortcut(cmd | Modifiers::SHIFT, Key::Z)) || pressed(shortcut(cmd, Key::Y)) {
            self.redo();
        } else if pressed(shortcut(cmd, Key::Z)) {
            self.undo();
        }
        if pressed(shortcut(Modifiers::NONE, Key::Backslash)) {
            self.viewer.show_before = !self.viewer.show_before;
        }
        if pressed(shortcut(Modifiers::NONE, Key::Z)) {
            self.viewer.zoom = if self.viewer.zoom == viewer::Zoom::Fit { viewer::Zoom::Ratio(1.0) } else { viewer::Zoom::Fit };
        }
        if pressed(shortcut(Modifiers::NONE, Key::ArrowRight)) {
            self.step_photo(1);
        }
        if pressed(shortcut(Modifiers::NONE, Key::ArrowLeft)) {
            self.step_photo(-1);
        }
        if pressed(shortcut(Modifiers::NONE, Key::Escape)) {
            self.export_open = false;
        }
    }

    fn dropped_files(&mut self, ctx: &egui::Context) {
        let files: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        for f in files {
            let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if f.is_dir() {
                self.open_folder(f, None, ctx);
            } else if matches!(ext.as_str(), "xmp" | "cube" | "3dl") {
                let entry = library::scan_presets(f.parent().unwrap_or(Path::new("."))).into_iter().find(|e| e.path == f);
                match (entry, &mut self.current) {
                    (Some(entry), Some(c)) => match apply_entry(&mut c.edit, &entry) {
                        Ok(()) => c.history.checkpoint(&c.edit),
                        Err(e) => self.say(e, true),
                    },
                    (None, _) => self.say(format!("{} non è un preset", f.display()), true),
                    _ => {}
                }
            } else if library::is_photo(&f) {
                self.open_file(f, ctx);
            }
        }
    }

    fn pick_folder(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new().set_title("Apri cartella");
        if let Some(d) = &self.folder {
            dialog = dialog.set_directory(d);
        }
        if let Some(dir) = dialog.pick_folder() {
            self.open_folder(dir, None, ctx);
        }
    }

    fn pick_file(&mut self, ctx: &egui::Context) {
        let exts = ["dng", "rw2", "cr2", "cr3", "nef", "arw", "raf", "orf", "pef", "srw", "jpg", "jpeg", "png", "tif", "tiff"];
        let mut dialog = rfd::FileDialog::new().set_title("Apri foto").add_filter("Foto", &exts);
        if let Some(d) = &self.folder {
            dialog = dialog.set_directory(d);
        }
        if let Some(f) = dialog.pick_file() {
            self.open_file(f, ctx);
        }
    }

    fn pick_preset_folder(&mut self, ctx: &egui::Context) {
        if let Some(dir) = rfd::FileDialog::new().set_title("Cartella dei preset").pick_folder() {
            self.set_preset_folder(dir, ctx);
        }
    }

    // ------------------------------------------------------------------
    // Layout

    fn menu(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Apri cartella…   ⌘O").clicked() {
                    ui.close();
                    self.pick_folder(&ctx);
                }
                if ui.button("Apri file…").clicked() {
                    ui.close();
                    self.pick_file(&ctx);
                }
                if ui.button("Cartella preset…").clicked() {
                    ui.close();
                    self.pick_preset_folder(&ctx);
                }
                if ui.add_enabled(self.current.is_some(), egui::Button::new("Carica LUT…")).clicked() {
                    ui.close();
                    self.load_lut_dialog();
                }
                ui.separator();
                let raw = self.current.as_ref().is_some_and(|c| state::sidecar_path(&c.path).is_some());
                if ui.add_enabled(raw, egui::Button::new("Salva impostazioni (.xmp)   ⌘S")).clicked() {
                    ui.close();
                    self.save_sidecar();
                }
                if ui.add_enabled(self.current.is_some(), egui::Button::new("Esporta…   ⇧⌘E")).clicked() {
                    ui.close();
                    self.export_open = true;
                }
                ui.separator();
                if ui.button("Esci").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Modifica", |ui| {
                let (can_undo, can_redo) = self.current.as_ref().map_or((false, false), |c| (c.history.can_undo(&c.edit), c.history.can_redo()));
                if ui.add_enabled(can_undo, egui::Button::new("Annulla   ⌘Z")).clicked() {
                    ui.close();
                    self.undo();
                }
                if ui.add_enabled(can_redo, egui::Button::new("Ripeti   ⇧⌘Z")).clicked() {
                    ui.close();
                    self.redo();
                }
                ui.separator();
                if ui.add_enabled(self.current.is_some(), egui::Button::new("Reimposta tutto")).clicked() {
                    ui.close();
                    self.reset_edit();
                }
            });
            ui.menu_button("Vista", |ui| {
                if ui.button("Adatta").clicked() {
                    ui.close();
                    self.viewer.zoom = viewer::Zoom::Fit;
                }
                if ui.button("100%   Z").clicked() {
                    ui.close();
                    self.viewer.zoom = viewer::Zoom::Ratio(1.0);
                }
                if ui.button("Prima / Dopo   \\").clicked() {
                    ui.close();
                    self.viewer.show_before = !self.viewer.show_before;
                }
            });
        });
    }

    fn preset_panel(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Preset").strong().size(15.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Cartella…").on_hover_text("Scegli la cartella dei preset (.xmp, .dng, .cube)").clicked() {
                    self.pick_preset_folder(&ctx);
                }
            });
        });
        ui.add(egui::TextEdit::singleline(&mut self.preset_filter).hint_text("Cerca…").desired_width(f32::INFINITY));
        ui.separator();
        self.hover_preset = None;
        if self.preset_folder.is_none() {
            ui.label(RichText::new("Scegli una cartella di preset Lightroom (.xmp, .dng \"mobile\") o LUT (.cube).").color(theme::TEXT_DIM));
            return;
        }
        if self.preset_rx.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Lettura dei preset…");
            });
            return;
        }
        if self.presets.is_empty() {
            ui.label(RichText::new("Nessun preset nella cartella").color(theme::TEXT_DIM));
            return;
        }
        let filter = self.preset_filter.to_lowercase();
        let enabled = self.current.as_ref().is_some_and(|c| c.loaded);
        let applied = self.current.as_ref().map(|c| (c.edit.preset_name.clone(), c.edit.lut.as_ref().map(|l| l.path.clone())));
        let mut clicked = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut i = 0;
            while i < self.presets.len() {
                let group = self.presets[i].group.clone();
                let end = self.presets[i..].iter().position(|p| p.group != group).map_or(self.presets.len(), |n| i + n);
                let title = if group.is_empty() { "Preset".to_string() } else { group.clone() };
                egui::CollapsingHeader::new(RichText::new(title).color(theme::TEXT_DIM)).id_salt(("group", &group)).default_open(true).show(ui, |ui| {
                    for (j, p) in self.presets[i..end].iter().enumerate() {
                        if !filter.is_empty() && !p.name.to_lowercase().contains(&filter) {
                            continue;
                        }
                        let (icon, tip) = match &p.kind {
                            PresetKind::Develop(pr) if pr.preset_type.as_deref() == Some("Look") => ("◐", "Profilo".to_string()),
                            PresetKind::Develop(_) => ("●", "Preset di sviluppo".to_string()),
                            PresetKind::Lut(_) => ("▦", "LUT 3D".to_string()),
                            PresetKind::Broken(e) => ("⚠", e.clone()),
                        };
                        let selected = applied.as_ref().is_some_and(|(name, lut)| match p.kind {
                            PresetKind::Lut(_) => lut.as_ref() == Some(&p.path),
                            _ => name.as_ref() == Some(&p.name),
                        });
                        let broken = matches!(p.kind, PresetKind::Broken(_));
                        let text = RichText::new(format!("{icon}  {}", p.name)).color(if broken { theme::TEXT_DIM } else { theme::TEXT });
                        let r = ui.add_enabled(enabled && !broken, egui::Button::selectable(selected, text));
                        let r = r.on_hover_text(format!("{tip}\n{}", p.path.display()));
                        if r.hovered() {
                            self.hover_preset = Some(i + j);
                        }
                        if r.clicked() {
                            clicked = Some(i + j);
                        }
                    }
                });
                i = end;
            }
        });
        if let Some(i) = clicked {
            self.apply_preset(i);
        }
    }

    fn develop_panel(&mut self, ui: &mut Ui) {
        widgets_histogram(ui, self.histogram.as_ref());
        if let Some(info) = self.current.as_ref().and_then(|c| c.info.as_ref()) {
            ui.label(RichText::new(shooting_info(info)).color(theme::TEXT_DIM).size(11.5));
        }
        ui.separator();
        let Some(c) = &mut self.current else {
            ui.label(RichText::new("Nessuna foto aperta").color(theme::TEXT_DIM));
            return;
        };
        let input = PanelInput { kind: c.kind, white_balance: self.white_balance, histogram: self.histogram.as_ref() };
        let mut actions = Vec::new();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_enabled_ui(c.loaded, |ui| {
                actions = panels::develop_panels(ui, &mut c.edit, &input, &mut self.panel_state);
            });
        });
        for a in actions {
            match a {
                PanelAction::LoadLut => self.load_lut_dialog(),
                PanelAction::ResetAll => self.reset_edit(),
            }
        }
    }

    fn filmstrip(&mut self, ui: &mut Ui) {
        let height = ui.available_height() - 4.0;
        let mut clicked = None;
        egui::ScrollArea::horizontal().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, path) in self.photos.iter().enumerate() {
                    let cell = egui::vec2(height * 1.4, height);
                    let (rect, resp) = ui.allocate_exact_size(cell, egui::Sense::click());
                    let selected = self.selected == Some(i);
                    let painter = ui.painter_at(rect);
                    painter.rect_filled(rect, 3.0, if selected { Color32::from_gray(70) } else { theme::PANEL_DARK });
                    let inner = rect.shrink(5.0);
                    match &self.thumbs[i] {
                        Some(t) => {
                            let [w, h] = t.size().map(|v| v as f32);
                            let s = (inner.width() / w).min(inner.height() / h);
                            let r = egui::Rect::from_center_size(inner.center(), egui::vec2(w * s, h * s));
                            painter.image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                        }
                        None => {
                            painter.text(inner.center(), egui::Align2::CENTER_CENTER, "…", egui::FontId::proportional(16.0), theme::TEXT_DIM);
                        }
                    }
                    if selected {
                        painter.rect_stroke(rect.shrink(1.0), 3.0, egui::Stroke::new(2.0, theme::ACCENT), egui::StrokeKind::Inside);
                        if self.scroll_to_selected {
                            resp.scroll_to_me(Some(egui::Align::Center));
                        }
                    } else if resp.hovered() {
                        painter.rect_stroke(rect.shrink(1.0), 3.0, egui::Stroke::new(1.0, Color32::from_gray(120)), egui::StrokeKind::Inside);
                    }
                    let resp = resp.on_hover_text(file_name(path));
                    if resp.clicked() {
                        clicked = Some(i);
                    }
                }
            });
        });
        self.scroll_to_selected = false;
        if let Some(i) = clicked {
            self.open_photo(i);
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if let Some(c) = &self.current {
                ui.label(RichText::new(file_name(&c.path)).strong());
                if c.loaded {
                    ui.label(RichText::new(format!("{} × {}", c.full_size[0], c.full_size[1])).color(theme::TEXT_DIM));
                    let q = match self.render_quality {
                        Quality::Draft => "bozza",
                        Quality::Preview => "anteprima",
                        Quality::Full => "piena risoluzione",
                    };
                    ui.label(RichText::new(format!("decodifica {} ms · sviluppo {} ms ({q})", c.decode_ms, self.render_ms)).color(theme::TEXT_DIM));
                }
                if !self.notes.is_empty() {
                    ui.label(RichText::new(format!("ℹ {} note", self.notes.len())).color(theme::TEXT_DIM)).on_hover_text(self.notes.join("\n"));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.exporting > 0 {
                    ui.spinner();
                }
                if let Some((text, at, error)) = &self.message {
                    if at.elapsed() < Duration::from_secs(8) || self.exporting > 0 {
                        let color = if *error { ui.visuals().error_fg_color } else { theme::TEXT };
                        ui.label(RichText::new(text).color(color));
                    }
                }
            });
        });
    }

    fn central(&mut self, ui: &mut Ui) {
        egui::Frame::new().fill(theme::PANEL_DARK).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            self.viewer.toolbar(ui, self.current.is_some());
        });
        let message = match &self.current {
            None => Some(viewer::empty_message()),
            Some(c) if c.error.is_some() => Some(format!("Impossibile aprire {}: {}", file_name(&c.path), c.error.as_deref().unwrap_or(""))),
            Some(c) if !c.loaded || self.after.is_none() => Some(format!("Caricamento di {}…", file_name(&c.path))),
            _ => None,
        };
        let after = self.after.as_ref();
        let texture = if self.viewer.show_before {
            self.before.as_ref().or(after.map(|s| &s.tex))
        } else {
            match (&self.detail, after) {
                (Some(d), Some(a)) if self.wants_detail && d.id == a.id => Some(&d.tex),
                _ => after.map(|s| &s.tex),
            }
        };
        let full_size = self.current.as_ref().map_or([1, 1], |c| c.full_size);
        let long = full_size[0].max(full_size[1]).max(1);
        let preview_width = full_size[0] * PREVIEW_SIZE.min(long) / long;
        let out = self.viewer.show(ui, &ViewInput { texture, full_size, preview_width, message });
        self.wants_detail = out.wants_detail;
    }

    fn export_dialog(&mut self, ctx: &egui::Context) {
        if !self.export_open {
            return;
        }
        let Some(c) = &self.current else {
            self.export_open = false;
            return;
        };
        let (path, full, preset) = (c.path.clone(), c.full_size, c.edit.preset_name.clone());
        let modal = egui::Modal::new(egui::Id::unique("export")).show(ctx, |ui| {
            ui.set_width(460.0);
            export_ui::dialog(ui, &mut self.export, &path, full, preset.as_deref())
        });
        match modal.inner {
            DialogResult::Export => {
                self.export_open = false;
                self.start_export(ctx);
            }
            DialogResult::Cancel => self.export_open = false,
            DialogResult::None if modal.should_close() => self.export_open = false,
            DialogResult::None => {}
        }
    }

    /// Nothing is loading or rendering: what is on screen is final.
    fn is_idle(&self) -> bool {
        let Some(c) = &self.current else { return self.preset_rx.is_none() };
        let rendered = |s: &Option<Shown>| s.as_ref().is_some_and(|s| s.id == self.job_id);
        let detail = self.last_sent.as_ref().is_some_and(|(_, d, _)| *d);
        (c.error.is_some() || (c.loaded && rendered(&self.after) && self.render_quality != Quality::Draft))
            && (!detail || rendered(&self.detail))
            && (!self.viewer.show_before || self.before.is_some())
            && self.preset_rx.is_none()
            && self.thumbs_pending == 0
            && self.exporting == 0
    }

    fn run_script(&mut self, ctx: &egui::Context) {
        let idle = self.is_idle();
        let Some(step) = self.script.as_mut().and_then(|s| s.next(ctx, idle)) else { return };
        match step {
            Step::Wait => {}
            Step::Preset(name) => {
                let name = name.to_lowercase();
                match self.presets.iter().position(|p| p.name.to_lowercase().contains(&name)) {
                    Some(i) => self.apply_preset(i),
                    None => eprintln!("script: no preset matches {name:?}"),
                }
            }
            Step::Set(props) => {
                if let Some(c) = &mut self.current {
                    c.edit.settings.apply(&props);
                }
            }
            Step::Undo => self.undo(),
            Step::Redo => self.redo(),
            Step::Reset => self.reset_edit(),
            Step::Before(on) => self.viewer.show_before = on,
            Step::Zoom(z) => self.viewer.zoom = z.map_or(viewer::Zoom::Fit, viewer::Zoom::Ratio),
            Step::Panel(title) => self.panel_state.force_open = Some(title),
            Step::Tab(which, n) => match which.as_str() {
                "curve" => self.panel_state.curve_tab = n,
                "hsl" => self.panel_state.hsl_tab = n,
                _ => self.panel_state.grading_tab = n,
            },
            Step::ExportDialog(open) => self.export_open = open,
            Step::Export => self.start_export(ctx),
            Step::Photo(d) => self.step_photo(d),
            Step::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match &self.current {
            Some(c) => format!("Lumen — {}", file_name(&c.path)),
            None => "Lumen".into(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

impl eframe::App for LumenApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        self.shortcuts(&ctx);
        self.dropped_files(&ctx);

        let panel = |fill| egui::Frame::new().fill(fill).inner_margin(egui::Margin::symmetric(8, 6));
        egui::Panel::top("menu").frame(panel(theme::PANEL_DARK)).show(ui, |ui| self.menu(ui));
        egui::Panel::bottom("status").frame(panel(theme::PANEL_DARK)).show(ui, |ui| self.status_bar(ui));
        egui::Panel::bottom("filmstrip")
            .frame(panel(theme::PANEL))
            .resizable(true)
            .default_size(118.0)
            .size_range(70.0..=240.0)
            .show(ui, |ui| self.filmstrip(ui));
        egui::Panel::left("presets")
            .frame(panel(theme::PANEL))
            .resizable(true)
            .default_size(240.0)
            .size_range(170.0..=420.0)
            .show(ui, |ui| self.preset_panel(ui));
        egui::Panel::right("develop")
            .frame(panel(theme::PANEL))
            .resizable(true)
            .default_size(340.0)
            .size_range(300.0..=520.0)
            .show(ui, |ui| self.develop_panel(ui));
        egui::CentralPanel::no_frame().show(ui, |ui| self.central(ui));
        self.export_dialog(&ctx);

        // One undo step per gesture: record when nothing is being dragged
        // or typed.
        let scripted_drag = self.script.as_ref().is_some_and(Script::dragging);
        let idle = !ctx.input(|i| i.pointer.any_down()) && !ctx.egui_wants_keyboard_input() && !scripted_drag;
        if let Some(c) = &mut self.current {
            if idle {
                c.history.checkpoint(&c.edit);
            }
        }
        self.run_script(&ctx);
        self.request_render(&ctx);
        self.update_title(&ctx);
        if self.message.as_ref().is_some_and(|(_, at, _)| at.elapsed() < Duration::from_secs(9)) {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Some(f) = &self.folder {
            storage.set_string(KEY_PHOTO_FOLDER, f.to_string_lossy().into_owned());
        }
        if let Some(f) = &self.preset_folder {
            storage.set_string(KEY_PRESET_FOLDER, f.to_string_lossy().into_owned());
        }
        storage.set_string(KEY_EXPORT, self.export.to_storage());
    }
}

/// Applies a preset-panel entry to an edit.
fn apply_entry(edit: &mut Edit, entry: &PresetEntry) -> Result<(), String> {
    match &entry.kind {
        PresetKind::Develop(p) => edit.apply_preset(p, &entry.name).map_err(|e| e.to_string()),
        PresetKind::Lut(cube) => {
            edit.lut = Some(LutRef { path: entry.path.clone(), name: entry.name.clone(), cube: cube.clone() });
            edit.lut_amount = 100.0;
            Ok(())
        }
        PresetKind::Broken(e) => Err(e.clone()),
    }
}

fn set_texture(ctx: &egui::Context, slot: &mut Option<TextureHandle>, name: &str, image: egui::ColorImage, opts: TextureOptions) {
    match slot {
        Some(t) => t.set(image, opts),
        None => *slot = Some(ctx.load_texture(name, image, opts)),
    }
}

fn widgets_histogram(ui: &mut Ui, h: Option<&Histogram>) {
    crate::widgets::histogram(ui, h);
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn shooting_info(i: &PhotoInfo) -> String {
    let mut parts = Vec::new();
    if !i.model.is_empty() {
        parts.push(format!("{} {}", i.make, i.model));
    }
    if let Some(iso) = i.iso {
        parts.push(format!("ISO {iso}"));
    }
    if let Some(fl) = i.focal_length {
        parts.push(format!("{fl:.0} mm"));
    }
    if let Some(f) = i.f_number {
        parts.push(format!("f/{f:.1}"));
    }
    if let Some(t) = i.exposure_time {
        parts.push(if t < 1.0 { format!("1/{:.0} s", 1.0 / t) } else { format!("{t} s") });
    }
    parts.join("  ·  ")
}
