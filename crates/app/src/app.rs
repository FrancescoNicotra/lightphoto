//! The application: layout (menu, presets, image, develop panels,
//! filmstrip), events from the background threads, shortcuts.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, RichText, TextureHandle, TextureOptions, Ui};
use pipeline::adobe::AdobeProfiles;
use pipeline::SourceKind;
use raw::PhotoInfo;

use crate::i18n::{tr, trf};
use crate::crop_tool::{CropTool, ASPECTS};
use crate::export_ui::{self, DialogResult, ExportEvent, ExportJob, ExportSettings};
use crate::library::{self, PresetEntry, PresetKind};
use crate::panels::{self, PanelAction, PanelInput, PanelState};
use crate::script::{Script, Selection, Step};
use crate::state::{self, Edit, History, LutRef};
use crate::theme;
use crate::viewer::{self, ViewInput, Viewer};
use crate::worker::{Event, Histogram, Quality, RenderJob, ThumbJob, Worker, PREVIEW_SIZE};
use crate::widgets::Slider;
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
    /// The edit as last written to (or read from) the sidecar.
    saved: Edit,
    /// The edit at the last change, and when that was (for autosave).
    seen: Edit,
    changed_at: Instant,
}

/// Wait after the last change before saving the sidecar.
const AUTOSAVE_DELAY: Duration = Duration::from_secs(1);

/// What the filmstrip knows about a photo besides its thumbnail.
#[derive(Clone, Default)]
struct PhotoMeta {
    /// Program that wrote a JPEG/PNG/TIFF (an edit or export already).
    creator: Option<String>,
    /// Its sidecar on disk holds an edit.
    saved_edit: bool,
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
    thumb_tx: Sender<library::Thumb>,
    thumb_rx: Receiver<library::Thumb>,
    meta: Vec<PhotoMeta>,
    /// Thumbnails not loaded (or failed) yet.
    thumbs_pending: usize,
    /// The open photo's index.
    selected: Option<usize>,
    /// Photos selected in the filmstrip (always includes the open one).
    selection: BTreeSet<usize>,
    /// Start of a ⇧-click range.
    anchor: Option<usize>,
    scroll_to_selected: bool,
    /// Copied settings (⌘⇧C).
    clipboard: Option<Edit>,

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
    as_shot: Option<(f32, f32)>,
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
    /// Crop & straighten tool, while open.
    crop_tool: Option<CropTool>,
    /// White balance eyedropper active.
    picking: bool,
    /// Preset thumbnails: (photo, preset) → texture.
    preset_thumbs: HashMap<(PathBuf, PathBuf), TextureHandle>,
    /// Photo and preset count the thumbnails were last asked for.
    thumbs_asked: Option<(PathBuf, usize)>,

    export: ExportSettings,
    export_open: bool,
    export_tx: Sender<ExportEvent>,
    export_rx: Receiver<ExportEvent>,
    /// Export batches running.
    exporting: usize,
    /// Photo being exported and batch size.
    export_progress: Option<(usize, usize)>,

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
            meta: Vec::new(),
            selected: None,
            selection: BTreeSet::new(),
            anchor: None,
            scroll_to_selected: false,
            clipboard: None,
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
            as_shot: None,
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
            crop_tool: None,
            picking: false,
            preset_thumbs: HashMap::new(),
            thumbs_asked: None,
            export: get(KEY_EXPORT).map(|s| ExportSettings::from_storage(&s)).unwrap_or_default(),
            export_open: false,
            export_tx,
            export_rx,
            exporting: 0,
            export_progress: None,
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
            Some(p) => app.say(trf!("{} does not exist", "{} non esiste", p.display()), true),
            None => {}
        }
        if app.adobe.is_empty() {
            app.say(tr("No Lightroom/Camera Raw install found: using the profiles in the files", "Nessuna installazione di Lightroom/Camera Raw trovata: si usano i profili del file"), false);
        }
        app
    }

    fn say(&mut self, text: impl Into<String>, error: bool) {
        self.message = Some((text.into(), Instant::now(), error));
    }

    // ------------------------------------------------------------------
    // Folders and photos

    fn open_folder(&mut self, dir: PathBuf, select: Option<&Path>, ctx: &egui::Context) {
        // Absolute, so the folder is found again from anywhere.
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        self.stash_current();
        self.photos = library::scan_photos(&dir);
        self.thumbs = vec![None; self.photos.len()];
        self.meta = vec![PhotoMeta::default(); self.photos.len()];
        self.thumbs_pending = self.photos.len();
        self.selection.clear();
        self.anchor = None;
        self.preset_thumbs.clear();
        let generation = self.thumb_generation.fetch_add(1, Ordering::Relaxed) + 1;
        library::spawn_thumbnails(self.photos.clone(), generation, self.thumb_generation.clone(), self.thumb_tx.clone(), ctx.clone());
        self.folder = Some(dir);
        let index = select.and_then(|s| self.photos.iter().position(|p| p == s)).or((!self.photos.is_empty()).then_some(0));
        match index {
            Some(i) => self.select_only(i),
            None => {
                self.current = None;
                self.selected = None;
                self.say(tr("No photos in this folder", "Nessuna foto nella cartella"), false);
            }
        }
    }

    fn open_file(&mut self, file: PathBuf, ctx: &egui::Context) {
        let dir = file.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let file = std::fs::canonicalize(&file).unwrap_or(file);
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        self.open_folder(dir, Some(&file), ctx);
    }

    /// Saves the edit of the open photo and keeps it, with its history, for
    /// when the photo is opened again.
    fn stash_current(&mut self) {
        self.autosave(true);
        if let Some(c) = self.current.take() {
            self.edits.insert(c.path, (c.edit, c.history));
        }
    }

    /// Writes the open photo's sidecar once the edit has been still for a
    /// moment (or now, with `force`). Nothing is written for a photo that
    /// was never edited.
    fn autosave(&mut self, force: bool) {
        let Some(c) = &mut self.current else { return };
        if c.edit != c.seen {
            c.seen = c.edit.clone();
            c.changed_at = Instant::now();
        }
        if c.edit == c.saved || !(force || c.changed_at.elapsed() >= AUTOSAVE_DELAY) {
            return;
        }
        let result = write_sidecar(&c.path, &c.edit, c.kind);
        c.saved = c.edit.clone();
        if let Err(e) = result {
            self.say(trf!("Could not save: {e}", "Salvataggio non riuscito: {e}"), true);
        }
    }

    /// Opens photo `i` and makes it the whole selection (a plain click).
    fn select_only(&mut self, i: usize) {
        self.selection = BTreeSet::from([i]);
        self.anchor = Some(i);
        self.open_photo(i);
    }

    /// A click in the filmstrip, with ⌘/Ctrl to add or remove one photo
    /// and ⇧ for a range, as in Lightroom.
    fn click_photo(&mut self, i: usize, modifiers: Modifiers) {
        if modifiers.shift {
            let a = self.anchor.or(self.selected).unwrap_or(i);
            self.selection = (a.min(i)..=a.max(i)).collect();
            self.open_photo(i);
        } else if modifiers.command {
            if self.selection.contains(&i) && self.selection.len() > 1 {
                self.selection.remove(&i);
                if self.selected == Some(i) {
                    let next = *self.selection.iter().next().unwrap();
                    self.open_photo(next);
                }
            } else {
                self.selection.insert(i);
                self.open_photo(i);
            }
            self.anchor = Some(i);
        } else {
            self.select_only(i);
        }
    }

    fn select_all(&mut self) {
        self.selection = (0..self.photos.len()).collect();
    }

    /// The photos an action applies to: the selection when several are
    /// selected, otherwise the open one.
    fn targets(&self) -> Vec<usize> {
        if self.selection.len() > 1 {
            self.selection.iter().copied().collect()
        } else {
            self.selected.into_iter().collect()
        }
    }

    /// The edit of any photo of the folder: the open one, one opened
    /// earlier, its sidecar, or the defaults.
    fn edit_of(&self, path: &Path) -> Edit {
        match &self.current {
            Some(c) if c.path == path => c.edit.clone(),
            _ => match self.edits.get(path) {
                Some((e, _)) => e.clone(),
                None => {
                    let kind = library::source_kind(path);
                    state::load_sidecar(path, kind).unwrap_or_else(|| Edit::defaults(kind))
                }
            },
        }
    }

    /// Changes the edit of every target photo. The open photo's change is
    /// an undo step and is saved by autosave; the others are saved now.
    /// Returns how many photos changed and the errors.
    fn modify_targets(&mut self, f: impl Fn(&mut Edit) -> Result<(), String>) -> (usize, Vec<String>) {
        let (mut changed, mut errors) = (0, Vec::new());
        for i in self.targets() {
            let Some(path) = self.photos.get(i).cloned() else { continue };
            if let Some(c) = self.current.as_mut().filter(|c| c.path == path) {
                match f(&mut c.edit) {
                    Ok(()) => {
                        c.history.checkpoint(&c.edit);
                        changed += 1;
                    }
                    Err(e) => errors.push(format!("{}: {e}", file_name(&path))),
                }
                continue;
            }
            let kind = library::source_kind(&path);
            let (mut edit, mut history) = self.edits.remove(&path).unwrap_or_else(|| {
                let e = self.edit_of(&path);
                let h = History::new(&e);
                (e, h)
            });
            match f(&mut edit) {
                Ok(()) => {
                    history.checkpoint(&edit);
                    changed += 1;
                    if let Err(e) = write_sidecar(&path, &edit, kind) {
                        errors.push(e);
                    }
                }
                Err(e) => errors.push(format!("{}: {e}", file_name(&path))),
            }
            self.edits.insert(path, (edit, history));
        }
        (changed, errors)
    }

    fn copy_settings(&mut self) {
        if let Some(c) = &self.current {
            self.clipboard = Some(c.edit.clone());
            self.say(trf!("Settings of {} copied", "Impostazioni di {} copiate", file_name(&c.path)), false);
        }
    }

    fn paste_settings(&mut self) {
        let Some(copied) = self.clipboard.clone() else {
            self.say(tr("No settings copied yet (⌘⇧C / Ctrl+Shift+C)", "Nessuna impostazione copiata (⌘⇧C / Ctrl+Shift+C)"), true);
            return;
        };
        let (n, errors) = self.modify_targets(|e| {
            e.paste(&copied);
            Ok(())
        });
        self.report(trf!("Settings pasted to {n} photos", "Impostazioni incollate su {n} foto"), errors);
    }

    fn report(&mut self, ok: String, errors: Vec<String>) {
        match errors.first() {
            Some(e) => self.say(trf!("{ok}; {} errors: {e}", "{ok}; {} errori: {e}", errors.len()), true),
            None => self.say(ok, false),
        }
    }

    /// Whether photo `i` carries an edit (in this session or on disk).
    fn is_modified(&self, i: usize) -> bool {
        let path = &self.photos[i];
        match &self.current {
            Some(c) if &c.path == path => c.edit.is_modified(c.kind),
            _ => match self.edits.get(path) {
                Some((e, _)) => e.is_modified(library::source_kind(path)),
                None => self.meta[i].saved_edit,
            },
        }
    }

    fn open_photo(&mut self, index: usize) {
        let Some(path) = self.photos.get(index).cloned() else { return };
        if self.current.as_ref().is_some_and(|c| c.path == path) {
            return;
        }
        self.stash_current();
        let kind = library::source_kind(&path);
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
            saved: edit.clone(),
            seen: edit.clone(),
            changed_at: Instant::now(),
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
        self.as_shot = None;
        self.notes.clear();
        self.last_sent = None;
        self.hover_cache = None;
        self.crop_tool = None;
        self.picking = false;
        self.viewer.reset();
    }

    fn step_photo(&mut self, delta: isize) {
        if self.photos.is_empty() {
            return;
        }
        let i = self.selected.map_or(0, |i| (i as isize + delta).clamp(0, self.photos.len() as isize - 1) as usize);
        self.select_only(i);
    }

    fn set_preset_folder(&mut self, dir: PathBuf, ctx: &egui::Context) {
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
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
        self.preset_thumbs.clear();
        self.thumbs_asked = None;
    }

    // ------------------------------------------------------------------
    // Editing

    /// Applies a preset to the open photo, or to all the selected ones.
    fn apply_preset(&mut self, index: usize) {
        let Some(entry) = self.presets.get(index).cloned() else { return };
        let (n, errors) = self.modify_targets(|e| apply_entry(e, &entry));
        let ok = if n > 1 { trf!("Preset {} applied to {n} photos", "Preset {} applicato a {n} foto", entry.name) } else { trf!("Preset applied: {}", "Preset applicato: {}", entry.name) };
        self.report(ok, errors);
    }

    fn load_lut_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("LUT", &["cube", "3dl"]).set_title(tr("Load LUT", "Carica LUT")).pick_file() else { return };
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
                self.say(tr("Nothing to undo", "Niente da annullare"), false);
            }
        }
    }

    fn redo(&mut self) {
        if let Some(c) = &mut self.current {
            if !c.history.redo(&mut c.edit) {
                self.say(tr("Nothing to redo", "Niente da ripetere"), false);
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
        let Some(c) = &mut self.current else { return };
        match state::save_sidecar(&c.path, &c.edit) {
            Ok(p) => {
                c.saved = c.edit.clone();
                self.say(trf!("Settings saved to {}", "Impostazioni salvate in {}", p.display()), false)
            }
            Err(e) => self.say(e, true),
        }
    }

    /// Exports the open photo, or all the selected ones, in the background.
    fn start_export(&mut self, ctx: &egui::Context) {
        let jobs: Vec<ExportJob> = self
            .targets()
            .into_iter()
            .filter_map(|i| self.photos.get(i))
            .map(|p| ExportJob { photo: p.clone(), edit: self.edit_of(p) })
            .collect();
        if jobs.is_empty() {
            return;
        }
        let n = jobs.len();
        export_ui::spawn_export(jobs, self.export.clone(), self.adobe.clone(), self.export_tx.clone(), ctx.clone());
        self.exporting += 1;
        self.export_progress = Some((0, n));
        self.say(if n > 1 { trf!("Exporting {n} photos…", "Esportazione di {n} foto…") } else { tr("Exporting…", "Esportazione…").to_string() }, false);
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
                Event::WhitePicked { generation, temperature, tint, incremental } if generation == c.generation => {
                    let s = &mut c.edit.settings;
                    if incremental {
                        (s.incremental_temperature, s.incremental_tint) = (temperature, tint);
                    } else {
                        s.white_balance = xmp::WhiteBalance::Custom;
                        s.temperature = Some(temperature.clamp(2000.0, 50000.0).round());
                        s.tint = Some(tint.clamp(-150.0, 150.0).round());
                    }
                    c.history.checkpoint(&c.edit);
                }
                Event::PickFailed { generation } if generation == c.generation => {
                    self.message = Some((
                        tr("That point is clipped or black: pick a grey or white point with detail", "Il punto è bruciato o nero: scegli un punto grigio o bianco con dettaglio").into(),
                        Instant::now(),
                        true,
                    ));
                }
                Event::AutoTone { generation, basic } if generation == c.generation => {
                    c.edit.settings.basic = basic;
                    c.history.checkpoint(&c.edit);
                }
                Event::PresetThumb { generation, key, image } if generation == c.generation => {
                    let tex = ctx.load_texture(format!("preset-{}", key.display()), image, tex_opts);
                    self.preset_thumbs.insert((c.path.clone(), key), tex);
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
                    self.as_shot = r.as_shot;
                    self.notes = r.notes;
                }
                _ => {}
            }
        }
        while let Ok(t) = self.thumb_rx.try_recv() {
            let i = t.index;
            if t.generation == self.thumb_generation.load(Ordering::Relaxed) && i < self.thumbs.len() {
                self.thumbs_pending = self.thumbs_pending.saturating_sub(1);
                self.thumbs[i] = t.image.map(|image| ctx.load_texture(format!("thumb-{i}"), image, tex_opts));
                self.meta[i] = PhotoMeta { creator: t.creator, saved_edit: t.saved_edit };
            }
        }
        if let Some(rx) = &self.preset_rx {
            if let Ok(list) = rx.try_recv() {
                self.presets = list;
                self.preset_rx = None;
            }
        }
        while let Ok(e) = self.export_rx.try_recv() {
            match e {
                ExportEvent::Started { index, total, photo } => {
                    self.export_progress = Some((index, total));
                    self.say(trf!("Exporting {}/{total}: {}…", "Esportazione {}/{total}: {}…", index + 1, file_name(&photo)), false);
                }
                ExportEvent::Done { path, size } => {
                    if self.script.is_some() {
                        eprintln!("exported {} ({} × {})", path.display(), size[0], size[1]);
                    }
                }
                ExportEvent::Failed { photo, error } => {
                    self.say(trf!("Export of {} failed: {error}", "Esportazione di {} fallita: {error}", file_name(&photo)), true);
                }
                ExportEvent::Finished { written, failed, ms } => {
                    self.exporting = self.exporting.saturating_sub(1);
                    if self.exporting == 0 {
                        self.export_progress = None;
                    }
                    let secs = ms as f64 / 1000.0;
                    let folder = self.export.folder.clone().or_else(|| self.folder.clone()).unwrap_or_default();
                    if failed == 0 {
                        self.say(trf!("Exported {written} photos to {} ({secs:.1} s)", "Esportate {written} foto in {} ({secs:.1} s)", file_name(&folder)), false);
                    } else {
                        self.say(trf!("Exported {written} photos, {failed} failed ({secs:.1} s)", "Esportate {written} foto, {failed} non riuscite ({secs:.1} s)"), true);
                    }
                }
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
        let cropping;
        let target = match &self.crop_tool {
            // While cropping: the straightened, uncropped photo.
            Some(tool) => {
                let mut e = c.edit.clone();
                e.settings.crop = tool.preview();
                cropping = e;
                &cropping
            }
            None => hover.unwrap_or(&c.edit),
        };
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
        if pressed(shortcut(cmd | Modifiers::SHIFT, Key::C)) {
            self.copy_settings();
        }
        if pressed(shortcut(cmd | Modifiers::SHIFT, Key::V)) {
            self.paste_settings();
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
        if pressed(shortcut(cmd, Key::A)) {
            self.select_all();
        }
        if pressed(shortcut(Modifiers::NONE, Key::R)) {
            self.toggle_crop_tool(false);
        }
        if pressed(shortcut(Modifiers::NONE, Key::W)) && self.crop_tool.is_none() {
            self.picking = !self.picking;
        }
        if self.crop_tool.is_some() && pressed(shortcut(Modifiers::NONE, Key::Enter)) {
            self.toggle_crop_tool(false);
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
            if self.crop_tool.is_some() {
                self.toggle_crop_tool(true);
            }
            self.picking = false;
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
                    (None, _) => self.say(trf!("{} is not a preset", "{} non è un preset", f.display()), true),
                    _ => {}
                }
            } else if library::is_photo(&f) {
                self.open_file(f, ctx);
            }
        }
    }

    fn pick_folder(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new().set_title(tr("Open folder", "Apri cartella"));
        if let Some(d) = &self.folder {
            dialog = dialog.set_directory(d);
        }
        if let Some(dir) = dialog.pick_folder() {
            self.open_folder(dir, None, ctx);
        }
    }

    fn pick_file(&mut self, ctx: &egui::Context) {
        let exts = ["dng", "rw2", "cr2", "cr3", "nef", "arw", "raf", "orf", "pef", "srw", "jpg", "jpeg", "png", "tif", "tiff"];
        let mut dialog = rfd::FileDialog::new().set_title(tr("Open photo", "Apri foto")).add_filter(tr("Photos", "Foto"), &exts);
        if let Some(d) = &self.folder {
            dialog = dialog.set_directory(d);
        }
        if let Some(f) = dialog.pick_file() {
            self.open_file(f, ctx);
        }
    }

    fn pick_preset_folder(&mut self, ctx: &egui::Context) {
        if let Some(dir) = rfd::FileDialog::new().set_title(tr("Preset folder", "Cartella dei preset")).pick_folder() {
            self.set_preset_folder(dir, ctx);
        }
    }

    // ------------------------------------------------------------------
    // Layout

    fn menu(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        // ⌘ on macOS, Ctrl elsewhere.
        let keys = |m: Modifiers, k: Key| ctx.format_shortcut(&KeyboardShortcut::new(m, k));
        let (cmd, shift) = (Modifiers::COMMAND, Modifiers::COMMAND | Modifiers::SHIFT);
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.add(egui::Button::new(tr("Open folder…", "Apri cartella…")).shortcut_text(keys(cmd, Key::O))).clicked() {
                    ui.close();
                    self.pick_folder(&ctx);
                }
                if ui.button(tr("Open file…", "Apri file…")).clicked() {
                    ui.close();
                    self.pick_file(&ctx);
                }
                if ui.button(tr("Preset folder…", "Cartella preset…")).clicked() {
                    ui.close();
                    self.pick_preset_folder(&ctx);
                }
                if ui.add_enabled(self.current.is_some(), egui::Button::new(tr("Load LUT…", "Carica LUT…"))).clicked() {
                    ui.close();
                    self.load_lut_dialog();
                }
                ui.separator();
                if ui.add_enabled(self.current.is_some(), egui::Button::new(tr("Save settings (.xmp)", "Salva impostazioni (.xmp)")).shortcut_text(keys(cmd, Key::S))).clicked() {
                    ui.close();
                    self.save_sidecar();
                }
                let n = self.targets().len();
                let label = if n > 1 { trf!("Export {n} photos…", "Esporta {n} foto…") } else { tr("Export…", "Esporta…").to_string() };
                if ui.add_enabled(self.current.is_some(), egui::Button::new(label).shortcut_text(keys(shift, Key::E))).clicked() {
                    ui.close();
                    self.export_open = true;
                }
                ui.separator();
                if ui.button(tr("Quit", "Esci")).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button(tr("Edit", "Modifica"), |ui| {
                let (can_undo, can_redo) = self.current.as_ref().map_or((false, false), |c| (c.history.can_undo(&c.edit), c.history.can_redo()));
                if ui.add_enabled(can_undo, egui::Button::new(tr("Undo", "Annulla")).shortcut_text(keys(cmd, Key::Z))).clicked() {
                    ui.close();
                    self.undo();
                }
                if ui.add_enabled(can_redo, egui::Button::new(tr("Redo", "Ripeti")).shortcut_text(keys(shift, Key::Z))).clicked() {
                    ui.close();
                    self.redo();
                }
                ui.separator();
                if ui.add_enabled(self.current.is_some(), egui::Button::new(tr("Copy settings", "Copia impostazioni")).shortcut_text(keys(shift, Key::C))).clicked() {
                    ui.close();
                    self.copy_settings();
                }
                let n = self.targets().len();
                let label = if n > 1 { trf!("Paste settings to {n} photos", "Incolla impostazioni su {n} foto") } else { tr("Paste settings", "Incolla impostazioni").to_string() };
                if ui.add_enabled(self.clipboard.is_some(), egui::Button::new(label).shortcut_text(keys(shift, Key::V))).clicked() {
                    ui.close();
                    self.paste_settings();
                }
                if ui.add_enabled(!self.photos.is_empty(), egui::Button::new(tr("Select all", "Seleziona tutte")).shortcut_text(keys(cmd, Key::A))).clicked() {
                    ui.close();
                    self.select_all();
                }
                ui.separator();
                if ui.add_enabled(self.current.is_some(), egui::Button::new(tr("Reset all", "Reimposta tutto"))).clicked() {
                    ui.close();
                    self.reset_edit();
                }
            });
            ui.menu_button(tr("View", "Vista"), |ui| {
                if ui.button(tr("Fit", "Adatta")).clicked() {
                    ui.close();
                    self.viewer.zoom = viewer::Zoom::Fit;
                }
                if ui.add(egui::Button::new("100%").shortcut_text("Z")).clicked() {
                    ui.close();
                    self.viewer.zoom = viewer::Zoom::Ratio(1.0);
                }
                if ui.add(egui::Button::new(tr("Before / After", "Prima / Dopo")).shortcut_text("\\")).clicked() {
                    ui.close();
                    self.viewer.show_before = !self.viewer.show_before;
                }
                ui.separator();
                for (mode, en, it) in [
                    (viewer::Compare::Single, "Single view", "Vista singola"),
                    (viewer::Compare::Split, "Before / After split", "Prima / Dopo a tendina"),
                    (viewer::Compare::SideBySide, "Before / After side by side", "Prima / Dopo affiancate"),
                ] {
                    if ui.radio(self.viewer.compare == mode, tr(en, it)).clicked() {
                        ui.close();
                        self.viewer.compare = mode;
                    }
                }
            });
            ui.menu_button(tr("Tools", "Strumenti"), |ui| {
                let loaded = self.current.as_ref().is_some_and(|c| c.loaded);
                if ui.add_enabled(loaded, egui::Button::new(tr("Crop & straighten", "Ritaglia e raddrizza")).shortcut_text("R")).clicked() {
                    ui.close();
                    self.toggle_crop_tool(false);
                }
                if ui.add_enabled(loaded, egui::Button::new(tr("White balance picker", "Contagocce bilanciamento")).shortcut_text("W")).clicked() {
                    ui.close();
                    self.picking = true;
                }
                if ui.add_enabled(loaded, egui::Button::new(tr("Auto tone", "Tono automatico"))).clicked() {
                    ui.close();
                    self.auto_tone();
                }
            });
        });
    }

    fn preset_panel(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Preset").strong().size(15.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(tr("Folder…", "Cartella…")).on_hover_text(tr("Choose the preset folder (.xmp, .dng, .cube)", "Scegli la cartella dei preset (.xmp, .dng, .cube)")).clicked() {
                    self.pick_preset_folder(&ctx);
                }
            });
        });
        ui.add(egui::TextEdit::singleline(&mut self.preset_filter).hint_text(tr("Search…", "Cerca…")).desired_width(f32::INFINITY));
        ui.separator();
        self.hover_preset = None;
        if self.preset_folder.is_none() {
            ui.label(RichText::new(tr("Choose a folder of Lightroom presets (.xmp, mobile .dng) or LUTs (.cube).", "Scegli una cartella di preset Lightroom (.xmp, .dng \"mobile\") o LUT (.cube).")).color(theme::TEXT_DIM));
            return;
        }
        if self.preset_rx.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tr("Reading presets…", "Lettura dei preset…"));
            });
            return;
        }
        if self.presets.is_empty() {
            ui.label(RichText::new(tr("No presets in this folder", "Nessun preset nella cartella")).color(theme::TEXT_DIM));
            return;
        }
        if self.selection.len() > 1 {
            ui.label(RichText::new(trf!("A click applies the preset to the {} selected photos", "Il clic applica il preset alle {} foto selezionate", self.selection.len())).color(theme::ACCENT).size(11.5));
        }
        let filter = self.preset_filter.to_lowercase();
        let enabled = self.current.as_ref().is_some_and(|c| c.loaded);
        let applied = self.current.as_ref().map(|c| (c.edit.preset_name.clone(), c.edit.lut.as_ref().map(|l| l.path.clone())));
        let photo = self.current.as_ref().map(|c| c.path.clone());
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
                            PresetKind::Develop(pr) if pr.preset_type.as_deref() == Some("Look") => ("◐", tr("Profile", "Profilo").to_string()),
                            PresetKind::Develop(_) => ("●", tr("Develop preset", "Preset di sviluppo").to_string()),
                            PresetKind::Lut(_) => ("▦", "LUT 3D".to_string()),
                            PresetKind::Broken(e) => ("⚠", e.clone()),
                        };
                        let selected = applied.as_ref().is_some_and(|(name, lut)| match p.kind {
                            PresetKind::Lut(_) => lut.as_ref() == Some(&p.path),
                            _ => name.as_ref() == Some(&p.name),
                        });
                        let broken = matches!(p.kind, PresetKind::Broken(_));
                        let thumb = photo.as_ref().and_then(|ph| self.preset_thumbs.get(&(ph.clone(), p.path.clone())));
                        let r = preset_row(ui, &p.name, icon, thumb, selected, enabled && !broken);
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
        let Some(kind) = self.current.as_ref().map(|c| c.kind) else {
            ui.label(RichText::new(tr("No photo open", "Nessuna foto aperta")).color(theme::TEXT_DIM));
            return;
        };
        let histogram = self.histogram;
        let input = PanelInput { kind, white_balance: self.white_balance, histogram: histogram.as_ref(), as_shot: self.as_shot, picking: self.picking };
        let mut actions = Vec::new();
        let tool_open = self.crop_tool.is_some();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if tool_open {
                self.crop_controls(ui);
            }
            let Some(c) = &mut self.current else { return };
            ui.add_enabled_ui(c.loaded && !tool_open, |ui| {
                actions = panels::develop_panels(ui, &mut c.edit, &input, &mut self.panel_state);
            });
        });
        for a in actions {
            match a {
                PanelAction::LoadLut => self.load_lut_dialog(),
                PanelAction::ResetAll => self.reset_edit(),
                PanelAction::PickWhite => self.picking = !self.picking,
                PanelAction::AutoTone => self.auto_tone(),
            }
        }
    }

    fn filmstrip(&mut self, ui: &mut Ui) {
        let height = ui.available_height() - 4.0;
        let label_h = 16.0;
        let mut clicked = None;
        let modifiers = ui.input(|i| i.modifiers);
        egui::ScrollArea::horizontal().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal(|ui| {
                for i in 0..self.photos.len() {
                    let path = &self.photos[i];
                    let cell = egui::vec2((height - label_h) * 1.4, height);
                    let (rect, resp) = ui.allocate_exact_size(cell, egui::Sense::click());
                    let active = self.selected == Some(i);
                    let in_selection = self.selection.contains(&i);
                    let painter = ui.painter_at(rect);
                    let bg = if in_selection { Color32::from_gray(72) } else { theme::PANEL_DARK };
                    painter.rect_filled(rect, 3.0, bg);
                    let pic = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.max.y - label_h)).shrink(5.0);
                    match &self.thumbs[i] {
                        Some(t) => {
                            let [w, h] = t.size().map(|v| v as f32);
                            let s = (pic.width() / w).min(pic.height() / h);
                            let r = egui::Rect::from_center_size(pic.center(), egui::vec2(w * s, h * s));
                            painter.image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                        }
                        None => {
                            painter.text(pic.center(), egui::Align2::CENTER_CENTER, "…", egui::FontId::proportional(16.0), theme::TEXT_DIM);
                        }
                    }

                    // File type, top left: RAW formats stand out from JPEGs.
                    let ext = path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
                    let is_raw = library::source_kind(path) == SourceKind::Raw;
                    badge(&painter, pic.left_top() + egui::vec2(2.0, 2.0), &ext, if is_raw { theme::ACCENT } else { Color32::from_gray(90) });
                    // Edited here (sidecar or this session), and files that
                    // come out of Lightroom/Photoshop already.
                    let mut tip = file_name(path);
                    let mut x = pic.right() - 2.0;
                    if self.is_modified(i) {
                        x = badge_right(&painter, egui::pos2(x, pic.top() + 2.0), tr("edited", "modificata"), Color32::from_rgb(196, 140, 40));
                        tip.push_str(tr("\nEdited (settings in the .xmp sidecar)", "\nModificata (impostazioni nel sidecar .xmp)"));
                    }
                    if let Some(c) = &self.meta[i].creator {
                        let short = if c.contains("Lightroom") { "LR" } else if c.contains("Photoshop") { "PS" } else { "XMP" };
                        badge_right(&painter, egui::pos2(x - 3.0, pic.top() + 2.0), short, Color32::from_rgb(120, 80, 160));
                        tip.push_str(&format!("\n{}: {c}", tr("Written by", "Creata da")));
                    }

                    let name = egui::Rect::from_min_max(egui::pos2(rect.left() + 4.0, rect.bottom() - label_h - 1.0), egui::pos2(rect.right() - 4.0, rect.bottom()));
                    // One line, elided in the middle of long names.
                    let mut job = egui::text::LayoutJob::simple_singleline(file_name(path), egui::FontId::proportional(11.0), if active { theme::TEXT } else { theme::TEXT_DIM });
                    job.wrap = egui::text::TextWrapping { max_width: name.width(), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
                    let galley = painter.layout_job(job);
                    let galley_rect = egui::Align2::CENTER_TOP.anchor_size(name.center_top(), galley.size());
                    painter.with_clip_rect(name).galley(galley_rect.min, galley, theme::TEXT);

                    if active {
                        painter.rect_stroke(rect.shrink(1.0), 3.0, egui::Stroke::new(2.0, theme::ACCENT), egui::StrokeKind::Inside);
                        if self.scroll_to_selected {
                            resp.scroll_to_me(Some(egui::Align::Center));
                        }
                    } else if resp.hovered() {
                        painter.rect_stroke(rect.shrink(1.0), 3.0, egui::Stroke::new(1.0, Color32::from_gray(120)), egui::StrokeKind::Inside);
                    }
                    let resp = resp.on_hover_text(tip);
                    if resp.clicked() {
                        clicked = Some(i);
                    }
                }
            });
        });
        self.scroll_to_selected = false;
        if let Some(i) = clicked {
            self.click_photo(i, modifiers);
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if let Some(c) = &self.current {
                ui.label(RichText::new(file_name(&c.path)).strong());
                if c.loaded {
                    ui.label(RichText::new(format!("{} × {}", c.full_size[0], c.full_size[1])).color(theme::TEXT_DIM));
                    let q = match self.render_quality {
                        Quality::Thumb | Quality::Draft => tr("draft", "bozza"),
                        Quality::Preview => tr("preview", "anteprima"),
                        Quality::Full => tr("full resolution", "piena risoluzione"),
                    };
                    ui.label(RichText::new(trf!("decode {} ms · develop {} ms ({q})", "decodifica {} ms · sviluppo {} ms ({q})", c.decode_ms, self.render_ms)).color(theme::TEXT_DIM));
                }
                if !self.notes.is_empty() {
                    ui.label(RichText::new(trf!("ℹ {} notes", "ℹ {} note", self.notes.len())).color(theme::TEXT_DIM)).on_hover_text(self.notes.join("\n"));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some((i, n)) = self.export_progress {
                    ui.add(egui::ProgressBar::new(i as f32 / n.max(1) as f32).desired_width(120.0).text(format!("{}/{n}", i + 1)));
                }
                if self.exporting > 0 {
                    ui.spinner();
                }
                if let Some((text, at, error)) = &self.message {
                    if at.elapsed() < Duration::from_secs(8) || self.exporting > 0 {
                        let color = if *error { ui.visuals().error_fg_color } else { theme::TEXT };
                        // Shortened to the room left by the photo's details.
                        let text = RichText::new(text).color(color);
                        ui.add(egui::Label::new(text.clone()).truncate()).on_hover_text(text);
                    }
                }
            });
        });
    }

    fn central(&mut self, ui: &mut Ui) {
        let mut toggle_crop = false;
        egui::Frame::new().fill(theme::PANEL_DARK).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Wraps to a second line when the view is narrow.
            ui.horizontal_wrapped(|ui| {
                self.viewer.toolbar(ui, self.current.is_some());
                ui.separator();
                ui.add_enabled_ui(self.current.as_ref().is_some_and(|c| c.loaded), |ui| {
                    let r = ui.selectable_label(self.crop_tool.is_some(), tr("Crop  R", "Ritaglia  R"));
                    toggle_crop = r.on_hover_text(tr("Crop and straighten (R)", "Ritaglia e raddrizza (R)")).clicked();
                    ui.toggle_value(&mut self.picking, tr("WB picker  W", "Contagocce  W"))
                        .on_hover_text(tr("Click a neutral point to set the white balance (W)", "Clic su un punto neutro per il bilanciamento del bianco (W)"));
                });
            });
        });
        if toggle_crop {
            self.toggle_crop_tool(false);
        }
        let message = match &self.current {
            None => Some(viewer::empty_message()),
            Some(c) if c.error.is_some() => Some(trf!("Cannot open {}: {}", "Impossibile aprire {}: {}", file_name(&c.path), c.error.as_deref().unwrap_or(""))),
            Some(c) if !c.loaded || self.after.is_none() => Some(trf!("Loading {}…", "Caricamento di {}…", file_name(&c.path))),
            _ => None,
        };
        let after = self.after.as_ref();
        let texture = match (&self.detail, after) {
            (Some(d), Some(a)) if self.wants_detail && d.id == a.id => Some(&d.tex),
            _ => after.map(|s| &s.tex),
        };
        let full_size = self.shown_size();
        let preview_width = self.current.as_ref().map_or(1, |c| {
            let long = c.full_size[0].max(c.full_size[1]).max(1);
            full_size[0] * PREVIEW_SIZE.min(long) / long
        });
        let input = ViewInput { after: texture, before: self.before.as_ref(), full_size, preview_width, message, picking: self.picking };
        let out = self.viewer.show(ui, &input, self.crop_tool.as_mut());
        self.wants_detail = out.wants_detail;
        if let Some(p) = out.picked {
            self.pick_white_at(p);
        }
    }

    /// Size of the photo as shown: cropped, or whole while cropping.
    fn shown_size(&self) -> [usize; 2] {
        let Some(c) = &self.current else { return [1, 1] };
        if self.crop_tool.is_some() {
            return c.full_size;
        }
        let o = c.info.as_ref().map(|i| i.orientation).unwrap_or_default();
        let (w, h) = pipeline::geometry::cropped_size(c.full_size[0], c.full_size[1], &c.edit.settings.crop, o);
        [w, h]
    }

    /// The eyedropper clicked (u, v) of the shown photo: find that point in
    /// the uncropped photo and ask the worker for the white balance.
    fn pick_white_at(&mut self, (u, v): (f32, f32)) {
        let Some(c) = &self.current else { return };
        let o = c.info.as_ref().map(|i| i.orientation).unwrap_or_default();
        let (rect, angle) = pipeline::geometry::to_oriented(&c.edit.settings.crop, o);
        let [w, h] = c.full_size.map(|v| v as f32);
        let (x, y) = ((rect.left + u * rect.width()) * w, (rect.top + v * rect.height()) * h);
        let (sx, sy) = pipeline::geometry::source_of(x, y, w, h, angle);
        self.worker.pick_white(c.generation, ((sx / w).clamp(0.0, 1.0), (sy / h).clamp(0.0, 1.0)));
        self.picking = false;
    }

    fn auto_tone(&mut self) {
        if let Some(c) = &self.current {
            self.worker.auto_tone(c.generation, c.edit.clone());
        }
    }

    /// Opens the crop tool, or closes it keeping the crop (or, with
    /// `cancel`, the crop it started from).
    fn toggle_crop_tool(&mut self, cancel: bool) {
        let Some(c) = &mut self.current else { return };
        match self.crop_tool.take() {
            Some(tool) => {
                c.edit.settings.crop = if cancel { tool.original.clone() } else { tool.result() };
                c.history.checkpoint(&c.edit);
            }
            None if c.loaded && !cancel => {
                let o = c.info.as_ref().map(|i| i.orientation).unwrap_or_default();
                self.crop_tool = Some(CropTool::new(&c.edit.settings.crop, o, c.full_size));
                self.picking = false;
                self.viewer.compare = viewer::Compare::Single;
                self.viewer.show_before = false;
            }
            None => {}
        }
    }

    /// Aspect, orientation and angle of the crop tool, above the panels.
    fn crop_controls(&mut self, ui: &mut Ui) {
        let Some(tool) = &mut self.crop_tool else { return };
        let mut done = None;
        egui::Frame::new().fill(theme::PANEL_DARK).corner_radius(4).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
            ui.label(RichText::new(tr("Crop & straighten", "Ritaglio e raddrizzamento")).strong());
            let (mut aspect, mut portrait) = (tool.aspect, tool.portrait);
            ui.horizontal(|ui| {
                ui.label(tr("Aspect", "Proporzioni"));
                egui::ComboBox::from_id_salt("aspect").selected_text(tr(ASPECTS[aspect].0, ASPECTS[aspect].1)).show_ui(ui, |ui| {
                    for (i, (en, it, _)) in ASPECTS.iter().enumerate() {
                        ui.selectable_value(&mut aspect, i, tr(en, it));
                    }
                });
                ui.selectable_value(&mut portrait, false, tr("Landscape", "Orizzontale"));
                ui.selectable_value(&mut portrait, true, tr("Portrait", "Verticale"));
            });
            if (aspect, portrait) != (tool.aspect, tool.portrait) {
                tool.set_aspect(aspect, portrait);
            }
            let mut angle = tool.angle;
            if Slider::new(tr("Angle", "Angolo"), -45.0..=45.0, 0.0).decimals(1).suffix("°").show(ui, &mut angle).changed() {
                tool.set_angle(angle);
            }
            ui.label(RichText::new(tr("Drag outside the frame to rotate", "Trascina fuori dal riquadro per ruotare")).color(theme::TEXT_DIM).size(11.0));
            ui.horizontal(|ui| {
                if ui.button(tr("Reset", "Reimposta")).clicked() {
                    tool.reset();
                }
                if ui.button(RichText::new(tr("Done  ↵", "Fatto  ↵")).strong()).clicked() {
                    done = Some(false);
                }
                if ui.button(tr("Cancel  Esc", "Annulla  Esc")).clicked() {
                    done = Some(true);
                }
            });
        });
        ui.add_space(4.0);
        if let Some(cancel) = done {
            self.toggle_crop_tool(cancel);
        }
    }

    /// Asks the worker for the thumbnails of the presets on the open photo
    /// (each preset on the photo's default settings), once per photo.
    fn request_preset_thumbs(&mut self) {
        let Some(c) = self.current.as_ref().filter(|c| c.loaded) else { return };
        if self.preset_rx.is_some() || self.thumbs_asked.as_ref().is_some_and(|(p, n)| *p == c.path && *n == self.presets.len()) {
            return;
        }
        let jobs: Vec<ThumbJob> = self
            .presets
            .iter()
            .filter(|p| !self.preset_thumbs.contains_key(&(c.path.clone(), p.path.clone())))
            .filter_map(|p| {
                let mut edit = Edit::defaults(c.kind);
                apply_entry(&mut edit, p).ok()?;
                Some(ThumbJob { key: p.path.clone(), edit })
            })
            .collect();
        self.worker.preset_thumbs(c.generation, jobs);
        self.thumbs_asked = Some((c.path.clone(), self.presets.len()));
    }

    fn export_dialog(&mut self, ctx: &egui::Context) {
        if !self.export_open {
            return;
        }
        let Some(c) = &self.current else {
            self.export_open = false;
            return;
        };
        let (path, preset) = (c.path.clone(), c.edit.preset_name.clone());
        let full = self.shown_size();
        let count = self.targets().len();
        let modal = egui::Modal::new(egui::Id::unique("export")).show(ctx, |ui| {
            ui.set_width(460.0);
            export_ui::dialog(ui, &mut self.export, &path, full, preset.as_deref(), count)
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
            && self.current.as_ref().is_some_and(|c| c.edit == c.saved)
            && self.presets.iter().filter(|p| !matches!(p.kind, PresetKind::Broken(_))).all(|p| self.preset_thumbs.contains_key(&(c.path.clone(), p.path.clone())))
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
            Step::Open(i) => self.select_only(i.min(self.photos.len().saturating_sub(1))),
            Step::Select(sel) => match sel {
                Selection::All => self.select_all(),
                Selection::None => {
                    if let Some(i) = self.selected {
                        self.select_only(i);
                    }
                }
                Selection::Only(list) => {
                    if let Some(&first) = list.first() {
                        self.select_only(first);
                        for i in list.into_iter().filter(|i| *i < self.photos.len()) {
                            self.selection.insert(i);
                        }
                    }
                }
                Selection::Add(i) => self.click_photo(i, Modifiers::COMMAND),
                Selection::Range(i) => self.click_photo(i, Modifiers::SHIFT),
            },
            Step::Copy => self.copy_settings(),
            Step::Paste => self.paste_settings(),
            Step::Amount(a) => {
                let as_shot = self.as_shot;
                if let Some(c) = &mut self.current {
                    c.edit.set_preset_amount(a, as_shot);
                }
            }
            Step::ExportDir(dir) => self.export.folder = Some(dir),
            Step::Crop { rect, angle, aspect } => {
                if self.crop_tool.is_none() {
                    self.toggle_crop_tool(false);
                }
                if let Some(tool) = &mut self.crop_tool {
                    if let Some((i, portrait)) = aspect {
                        tool.set_aspect(i, portrait);
                    }
                    if let Some(a) = angle {
                        tool.set_angle(a);
                    }
                    if let Some(r) = rect {
                        tool.rect = r;
                        tool.set_angle(tool.angle);
                    }
                }
            }
            Step::CropDone(cancel) => {
                if self.crop_tool.is_some() {
                    self.toggle_crop_tool(cancel);
                }
            }
            Step::Compare(mode) => self.viewer.compare = mode,
            Step::Pick(u, v) => self.pick_white_at((u, v)),
            Step::Auto => self.auto_tone(),
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
            c.edit.drop_stale_preset();
            if idle {
                c.history.checkpoint(&c.edit);
            }
        }
        if idle {
            self.autosave(false);
            if self.current.as_ref().is_some_and(|c| c.edit != c.saved) {
                ctx.request_repaint_after(AUTOSAVE_DELAY);
            }
        }
        self.run_script(&ctx);
        self.request_render(&ctx);
        self.request_preset_thumbs();
        self.update_title(&ctx);
        if self.message.as_ref().is_some_and(|(_, at, _)| at.elapsed() < Duration::from_secs(9)) {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn on_exit(&mut self) {
        self.autosave(true);
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

/// Saves an edit to the photo's sidecar; an unedited photo without a
/// sidecar gets none.
fn write_sidecar(path: &Path, edit: &Edit, kind: SourceKind) -> Result<(), String> {
    if edit.is_modified(kind) || state::sidecar_path(path).exists() {
        state::save_sidecar(path, edit)?;
    }
    Ok(())
}

/// A row of the preset list: the open photo with the preset (once its
/// thumbnail is ready), the icon and the name.
fn preset_row(ui: &mut Ui, name: &str, icon: &str, thumb: Option<&TextureHandle>, selected: bool, enabled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 50.0), if enabled { egui::Sense::click() } else { egui::Sense::hover() });
    let painter = ui.painter_at(rect);
    if selected {
        painter.rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
    } else if resp.hovered() && enabled {
        painter.rect_filled(rect, 3.0, Color32::from_gray(58));
    }
    let pic = egui::Rect::from_min_size(rect.min + egui::vec2(4.0, 4.0), egui::vec2(63.0, 42.0));
    match thumb {
        Some(t) => {
            let [w, h] = t.size().map(|v| v as f32);
            let s = (pic.width() / w).min(pic.height() / h);
            let r = egui::Rect::from_center_size(pic.center(), egui::vec2(w * s, h * s));
            painter.image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            painter.rect_filled(pic, 2.0, theme::PANEL_DARK);
        }
    }
    let color = if enabled { theme::TEXT } else { theme::TEXT_DIM };
    let mut job = egui::text::LayoutJob::simple_singleline(format!("{icon}  {name}"), egui::FontId::proportional(13.0), color);
    job.wrap = egui::text::TextWrapping { max_width: rect.right() - pic.right() - 12.0, max_rows: 2, break_anywhere: false, overflow_character: Some('…') };
    let galley = painter.layout_job(job);
    let pos = egui::pos2(pic.right() + 8.0, rect.center().y - galley.size().y / 2.0);
    painter.galley(pos, galley, color);
    resp
}

/// A small label with a coloured background at `pos` (top left).
fn badge(painter: &egui::Painter, pos: egui::Pos2, text: &str, fill: Color32) -> egui::Rect {
    let galley = painter.layout_no_wrap(text.to_string(), egui::FontId::proportional(9.5), Color32::WHITE);
    let rect = egui::Rect::from_min_size(pos, galley.size() + egui::vec2(6.0, 2.0));
    painter.rect_filled(rect, 2.0, fill);
    painter.galley(pos + egui::vec2(3.0, 1.0), galley, Color32::WHITE);
    rect
}

/// As [`badge`], right-aligned at `pos`; returns where the next one goes.
fn badge_right(painter: &egui::Painter, pos: egui::Pos2, text: &str, fill: Color32) -> f32 {
    let w = painter.layout_no_wrap(text.to_string(), egui::FontId::proportional(9.5), Color32::WHITE).size().x + 6.0;
    badge(painter, egui::pos2(pos.x - w, pos.y), text, fill).left()
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
