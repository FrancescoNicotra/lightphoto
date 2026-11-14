//! The render worker: decodes the open photo and develops it off the UI
//! thread. Requests go into a one-slot mailbox, so a request that has not
//! started yet is replaced by the next one: while a slider moves only the
//! latest value is rendered.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use eframe::egui;
use pipeline::adobe::{self, AdobeProfiles};
use pipeline::{RenderCache, Pipeline, RenderOptions, SourceKind};
use raw::{ColorSource, DecodeOptions, Image, Photo, PhotoInfo, TempTint};
use xmp::{Basic, DevelopSettings, Look};

use crate::state::Edit;

/// Long edge of the interactive preview.
pub const PREVIEW_SIZE: usize = 2048;
/// Long edge of the quick render shown while a slider is being dragged.
pub const DRAFT_SIZE: usize = 1024;
/// Long edge of the preset thumbnails.
pub const THUMB_SIZE: usize = 160;
/// Long edge of the copy the thumbnails are rendered from.
const SMALL_SIZE: usize = 320;

/// A preset thumbnail to render: the photo with `edit`.
pub struct ThumbJob {
    /// Identifies the thumbnail in the reply (the preset's path).
    pub key: PathBuf,
    pub edit: Edit,
}

#[derive(Clone, Debug)]
pub struct RenderJob {
    /// Increases with every request; replies carry it back.
    pub id: u64,
    /// Which loaded photo this is for.
    pub generation: u64,
    pub edit: Edit,
    /// Only a quick, smaller draft: a slider is moving. The UI asks for the
    /// full preview when the gesture ends.
    pub draft: bool,
    /// Also render at full resolution (the view is zoomed past the preview).
    pub detail: bool,
    /// Show this mask in red over the photo.
    pub overlay: Option<usize>,
}

/// Per-channel 256-bin histogram of the displayed (sRGB 8-bit) image.
pub type Histogram = [[u32; 256]; 3];

pub enum Event {
    Loaded {
        generation: u64,
        path: PathBuf,
        info: Box<PhotoInfo>,
        kind: SourceKind,
        /// Full-size dimensions, as displayed (after orientation).
        size: [usize; 2],
        decode_ms: u128,
    },
    Failed {
        generation: u64,
        error: String,
    },
    /// The photo with default settings, for the before/after view.
    Before { generation: u64, image: egui::ColorImage },
    Rendered(Box<Rendered>),
    /// White balance that makes the picked point neutral: absolute
    /// temperature/tint for raw files, incremental values for rendered ones.
    WhitePicked { generation: u64, temperature: f32, tint: f32, incremental: bool },
    /// The picked point was clipped or black: no white balance from it.
    PickFailed { generation: u64 },
    /// Tone settings proposed by Auto.
    AutoTone { generation: u64, basic: Basic },
    /// A preset thumbnail.
    PresetThumb { generation: u64, key: PathBuf, image: egui::ColorImage },
}

pub struct Rendered {
    pub generation: u64,
    pub id: u64,
    pub image: egui::ColorImage,
    pub histogram: Histogram,
    pub ms: u128,
    pub quality: Quality,
    /// White balance in effect (raw files), for the Temp/Tint sliders when
    /// the setting is "As Shot", "Auto" or a named preset.
    pub white_balance: Option<(f32, f32)>,
    /// The camera's white balance (raw files), for the preset amount.
    pub as_shot: Option<(f32, f32)>,
    /// What is not reproduced, and which Adobe profiles are in use.
    pub notes: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Quality {
    /// Preset thumbnails.
    Thumb,
    Draft,
    Preview,
    Full,
}

#[derive(Default)]
struct Mailbox {
    load: Option<(PathBuf, u64)>,
    render: Option<RenderJob>,
    /// Eyedropper: generation and point in fractions of the oriented,
    /// uncropped photo.
    pick: Option<(u64, (f32, f32))>,
    /// Auto tone for this edit.
    auto: Option<(u64, Edit)>,
    /// Preset thumbnails, done when nothing else is waiting.
    thumbs: VecDeque<(u64, ThumbJob)>,
    quit: bool,
}

pub struct Worker {
    mailbox: Arc<(Mutex<Mailbox>, Condvar)>,
    pub events: Receiver<Event>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    pub fn spawn(ctx: egui::Context, adobe: AdobeProfiles) -> Worker {
        let mailbox = Arc::new((Mutex::new(Mailbox::default()), Condvar::new()));
        let (tx, events) = mpsc::channel();
        let mb = mailbox.clone();
        let thread = std::thread::Builder::new()
            .name("render".into())
            .spawn(move || {
                let mut state = WorkerState {
                    adobe,
                    tx,
                    ctx,
                    photo: None,
                    draft_cache: RenderCache::new(),
                    preview_cache: RenderCache::new(),
                };
                state.run(&mb);
            })
            .expect("spawn render thread");
        Worker { mailbox, events, thread: Some(thread) }
    }

    /// Opens a photo; drops any render still waiting for the previous one.
    pub fn load(&self, path: PathBuf, generation: u64) {
        let (lock, cv) = &*self.mailbox;
        let mut m = lock.lock().unwrap();
        m.load = Some((path, generation));
        m.render = None;
        cv.notify_one();
    }

    /// Queues a render, replacing one that has not started yet.
    pub fn render(&self, job: RenderJob) {
        self.post(|m| m.render = Some(job));
    }

    /// White balance from the point (u, v) of the oriented, uncropped photo.
    pub fn pick_white(&self, generation: u64, point: (f32, f32)) {
        self.post(|m| m.pick = Some((generation, point)));
    }

    pub fn auto_tone(&self, generation: u64, edit: Edit) {
        self.post(|m| m.auto = Some((generation, edit)));
    }

    /// Replaces the queue of preset thumbnails.
    pub fn preset_thumbs(&self, generation: u64, jobs: Vec<ThumbJob>) {
        self.post(|m| m.thumbs = jobs.into_iter().map(|j| (generation, j)).collect());
    }

    fn post(&self, f: impl FnOnce(&mut Mailbox)) {
        let (lock, cv) = &*self.mailbox;
        f(&mut lock.lock().unwrap());
        cv.notify_one();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let (lock, cv) = &*self.mailbox;
        lock.lock().unwrap().quit = true;
        cv.notify_one();
        if let Some(t) = self.thread.take() {
            // A render in progress finishes first; that is at most a second.
            t.join().ok();
        }
    }
}

enum Task {
    Load(PathBuf, u64),
    Render(Box<RenderJob>),
    Pick(u64, (f32, f32)),
    Auto(u64, Box<Edit>),
    Thumb(u64, Box<ThumbJob>),
}

struct WorkerState {
    adobe: AdobeProfiles,
    tx: Sender<Event>,
    ctx: egui::Context,
    photo: Option<OpenPhoto>,
    /// Intermediate stages of the last draft and preview renders, so a
    /// slider only redoes the stages it affects. Full-size and thumbnail
    /// renders are not cached: they would hold hundreds of MB, or change
    /// settings every time.
    draft_cache: RenderCache,
    preview_cache: RenderCache,
}

/// The photo being edited, as the worker keeps it.
struct OpenPhoto {
    generation: u64,
    path: PathBuf,
    /// Half-size decode reduced to the preview size. Its colour data and
    /// info are swapped to match the profile the current look asks for.
    preview: Photo,
    /// Full-size decode, made on demand for 100% views.
    full: Option<Photo>,
    /// A small copy for the preset thumbnails.
    small: Photo,
    /// Long edge of the full-size photo, for pixel radii.
    full_size: usize,
    profiles: ProfileCache,
    /// Which cache entry `preview` / `full` currently carry.
    applied: Option<Option<Look>>,
    applied_full: Option<Option<Look>>,
    applied_small: Option<Option<Look>>,
    /// Crop of the last "before" render (it follows the edit's crop).
    before_crop: Option<xmp::Crop>,
}

impl WorkerState {
    fn run(&mut self, mailbox: &(Mutex<Mailbox>, Condvar)) {
        loop {
            let task = {
                let (lock, cv) = mailbox;
                let mut m = lock.lock().unwrap();
                loop {
                    if m.quit {
                        return;
                    }
                    if let Some((p, g)) = m.load.take() {
                        break Task::Load(p, g);
                    }
                    if let Some((g, p)) = m.pick.take() {
                        break Task::Pick(g, p);
                    }
                    if let Some(job) = m.render.take() {
                        break Task::Render(Box::new(job));
                    }
                    if let Some((g, e)) = m.auto.take() {
                        break Task::Auto(g, Box::new(e));
                    }
                    // Lowest priority: one thumbnail, then look again.
                    if let Some((g, job)) = m.thumbs.pop_front() {
                        break Task::Thumb(g, Box::new(job));
                    }
                    m = cv.wait(m).unwrap();
                }
            };
            let pending = || {
                let m = mailbox.0.lock().unwrap();
                m.load.is_some() || m.render.is_some() || m.pick.is_some() || m.auto.is_some() || m.quit
            };
            match task {
                Task::Load(path, generation) => self.load(path, generation),
                Task::Render(job) => {
                    if job.draft {
                        self.render(&job, Quality::Draft);
                        continue;
                    }
                    self.render(&job, Quality::Preview);
                    if !pending() {
                        self.render_before(&job);
                    }
                    if job.detail && !pending() {
                        self.render(&job, Quality::Full);
                    }
                }
                Task::Pick(generation, point) => self.pick_white(generation, point),
                Task::Auto(generation, edit) => self.auto_tone(generation, &edit),
                Task::Thumb(generation, job) => {
                    let render = RenderJob { id: 0, generation, edit: job.edit, draft: false, detail: false, overlay: None };
                    if let Some(d) = self.develop(&render, Quality::Thumb) {
                        let (image, _) = to_color_image(&d.img);
                        self.send(Event::PresetThumb { generation, key: job.key, image });
                    }
                }
            }
        }
    }

    fn send(&self, e: Event) {
        self.tx.send(e).ok();
        self.ctx.request_repaint();
    }

    fn load(&mut self, path: PathBuf, generation: u64) {
        self.photo = None;
        // The caches would be missed anyway; free their memory now.
        self.draft_cache.clear();
        self.preview_cache.clear();
        let t = Instant::now();
        let photo = match Photo::load(&path, &DecodeOptions { half_size: true, ..Default::default() }) {
            Ok(p) => p,
            Err(e) => {
                self.send(Event::Failed { generation, error: e.to_string() });
                return;
            }
        };
        let decode_ms = t.elapsed().as_millis();
        let kind = SourceKind::of(&photo);
        let size = match &photo.info.raw {
            Some(r) if photo.info.orientation.swaps_axes() => [r.crop[3], r.crop[2]],
            Some(r) => [r.crop[2], r.crop[3]],
            None => [photo.data.width, photo.data.height],
        };
        let info = photo.info.clone();
        let mut preview = photo;
        preview.data = preview.data.downscale(PREVIEW_SIZE);
        let small = Photo { data: preview.data.downscale(SMALL_SIZE), info: preview.info.clone(), color: preview.color.clone() };
        let profiles = ProfileCache::new(&preview);
        self.photo = Some(OpenPhoto {
            generation,
            path: path.clone(),
            preview,
            full: None,
            small,
            full_size: size[0].max(size[1]),
            profiles,
            applied: None,
            applied_full: None,
            applied_small: None,
            before_crop: None,
        });
        self.send(Event::Loaded { generation, path, info: Box::new(info), kind, size, decode_ms });
    }

    /// The default rendering for this kind of file, with the edit's crop so
    /// before and after line up; redone when the crop changes.
    fn render_before(&mut self, job: &RenderJob) {
        let generation = job.generation;
        let crop = &job.edit.settings.crop;
        let Some(open) = self.photo.as_mut().filter(|p| p.generation == generation && p.before_crop.as_ref() != Some(crop)) else { return };
        open.before_crop = Some(crop.clone());
        let mut edit = Edit::defaults(SourceKind::of(&open.preview));
        edit.settings.crop = crop.clone();
        let before = RenderJob { id: 0, generation, edit, draft: false, detail: false, overlay: None };
        if let Some(d) = self.develop(&before, Quality::Preview) {
            let (image, _) = to_color_image(&d.img);
            self.send(Event::Before { generation, image });
        }
    }

    /// Eyedropper: the white balance under which the average around
    /// (u, v) is neutral.
    fn pick_white(&mut self, generation: u64, (u, v): (f32, f32)) {
        let Some(open) = self.photo.as_ref().filter(|p| p.generation == generation) else { return };
        let img = &open.preview.data;
        let (cx, cy) = ((u * img.width as f32) as i64, (v * img.height as f32) as i64);
        let r = (img.width.max(img.height) as i64 / 500).max(2);
        let (mut sum, mut n, mut clipped) = ([0.0f64; 3], 0, false);
        for y in (cy - r).max(0)..=(cy + r).min(img.height as i64 - 1) {
            for x in (cx - r).max(0)..=(cx + r).min(img.width as i64 - 1) {
                let p = img.get(x as usize, y as usize);
                clipped |= p.iter().any(|c| *c >= 0.99);
                (0..3).for_each(|c| sum[c] += p[c] as f64);
                n += 1;
            }
        }
        if n == 0 || clipped {
            self.send(Event::PickFailed { generation });
            return;
        }
        let mean = sum.map(|s| s / n as f64);
        let event = match &open.preview.color {
            ColorSource::Camera(cam) if mean.iter().all(|c| *c > 1e-5) => {
                let tt = TempTint::from_xy(cam.profile.neutral_to_xy(raw::profile::normalize_max(mean)));
                Event::WhitePicked { generation, temperature: tt.temperature as f32, tint: tt.tint as f32, incremental: false }
            }
            ColorSource::Working => match crate::auto::incremental_white(mean.map(|c| c as f32)) {
                Some((temperature, tint)) => Event::WhitePicked { generation, temperature, tint, incremental: true },
                None => Event::PickFailed { generation },
            },
            _ => Event::PickFailed { generation },
        };
        self.send(event);
    }

    /// Auto tone: develops a small copy with the tone sliders at zero and
    /// reads its lightness.
    fn auto_tone(&mut self, generation: u64, edit: &Edit) {
        let mut edit = edit.clone();
        edit.settings.basic = Basic::default();
        edit.settings.effects.grain_amount = 0.0;
        let job = RenderJob { id: 0, generation, edit, draft: true, detail: false, overlay: None };
        let Some(d) = self.develop(&job, Quality::Draft) else { return };
        let rgb = raw::encode_srgb8(&d.img, 0.0);
        let basic = crate::auto::auto_tone(&crate::auto::luma8(&rgb));
        self.send(Event::AutoTone { generation, basic });
    }

    fn render(&mut self, job: &RenderJob, quality: Quality) {
        let t = Instant::now();
        let Some(Developed { img, white_balance, as_shot, notes }) = self.develop(job, quality) else { return };
        let (image, histogram) = to_color_image(&img);
        let ms = t.elapsed().as_millis();
        let r = Rendered { generation: job.generation, id: job.id, image, histogram, ms, quality, white_balance, as_shot, notes };
        self.send(Event::Rendered(Box::new(r)));
    }

    /// Develops the open photo with `job`'s settings.
    fn develop(&mut self, job: &RenderJob, quality: Quality) -> Option<Developed> {
        let adobe = &self.adobe;
        let open = self.photo.as_mut().filter(|p| p.generation == job.generation)?;

        let mut settings = job.edit.settings.clone();
        let key = look_key(&settings.look);
        let prepared = open.profiles.get(adobe, &key);
        if let Some(look) = &mut settings.look {
            if let Some(resolved) = &prepared.look {
                *look = Look { amount: look.amount, ..resolved.clone() };
            }
        }
        let notes = prepared.notes.clone();
        let (color, info) = (prepared.color.clone(), prepared.info.clone());

        let photo = if quality == Quality::Thumb {
            if open.applied_small.as_ref() != Some(&key) {
                (open.small.color, open.small.info) = (color, info);
                open.applied_small = Some(key);
            }
            &open.small
        } else if quality == Quality::Full {
            if open.full.is_none() {
                open.full = Photo::load(&open.path, &DecodeOptions::default()).ok();
                open.applied_full = None;
            }
            let full = open.full.as_mut()?;
            if open.applied_full.as_ref() != Some(&key) {
                (full.color, full.info) = (color, info);
                open.applied_full = Some(key);
            }
            &*full
        } else {
            if open.applied.as_ref() != Some(&key) {
                (open.preview.color, open.preview.info) = (color, info);
                open.applied = Some(key);
            }
            &open.preview
        };

        let mut pipeline = Pipeline::new(&settings, SourceKind::of(photo));
        if let Some(l) = &job.edit.lut {
            pipeline = pipeline.with_cube((*l.cube).clone(), job.edit.lut_amount / 100.0);
        }
        let max_size = match quality {
            Quality::Draft => Some(DRAFT_SIZE),
            Quality::Thumb => Some(THUMB_SIZE),
            _ => None,
        };
        let opts = RenderOptions { max_size, full_size: Some(open.full_size) };
        let mut img = match quality {
            Quality::Draft => pipeline.render_cached(photo, &opts, &mut self.draft_cache),
            Quality::Preview => pipeline.render_cached(photo, &opts, &mut self.preview_cache),
            Quality::Thumb | Quality::Full => pipeline.render(photo, &opts),
        };
        pipeline.grain(&mut img);
        if let Some(w) = job.overlay.and_then(|i| pipeline.mask_overlay(photo, i, img.width, img.height)) {
            // Red at half strength where the mask is.
            let red = [0.75f32, 0.05, 0.05];
            img.pixels.iter_mut().zip(&w).for_each(|(p, &w)| *p = std::array::from_fn(|c| p[c] + (red[c] - p[c]) * w * 0.55));
        }

        let temp_tint = |xy| {
            let tt = TempTint::from_xy(xy);
            (tt.temperature as f32, tt.tint as f32)
        };
        let as_shot = photo.as_shot_white();
        let white_balance = as_shot.map(|a| temp_tint(pipeline.white_point(photo).unwrap_or(a)));
        let mut notes = notes;
        notes.extend(pipeline.warnings.iter().cloned());
        Some(Developed { img, white_balance, as_shot: as_shot.map(temp_tint), notes })
    }
}

/// A developed image (with grain), the white balance in effect, the
/// camera's, and notes for the user.
struct Developed {
    img: Image,
    white_balance: Option<(f32, f32)>,
    as_shot: Option<(f32, f32)>,
    notes: Vec<String>,
}

/// Looks differ by amount without needing different profiles.
fn look_key(look: &Option<Look>) -> Option<Look> {
    look.as_ref().map(|l| Look { amount: 1.0, ..l.clone() })
}

/// Adobe profiles prepared for one photo, per look: which camera profile it
/// gets and the look's resolved tables. Preparing reads files from the
/// Adobe install, so each look is prepared once.
struct ProfileCache {
    base_color: ColorSource,
    base_info: PhotoInfo,
    entries: Vec<(Option<Look>, Prepared)>,
}

#[derive(Clone)]
struct Prepared {
    color: ColorSource,
    info: PhotoInfo,
    look: Option<Look>,
    notes: Vec<String>,
}

impl ProfileCache {
    fn new(photo: &Photo) -> ProfileCache {
        ProfileCache { base_color: photo.color.clone(), base_info: photo.info.clone(), entries: Vec::new() }
    }

    fn get(&mut self, adobe: &AdobeProfiles, key: &Option<Look>) -> &Prepared {
        if let Some(i) = self.entries.iter().position(|(k, _)| k == key) {
            return &self.entries[i].1;
        }
        // `prepare` only needs the metadata: run it on a 1-pixel stand-in.
        let mut probe = Photo { info: self.base_info.clone(), data: Image::new(1, 1), color: self.base_color.clone() };
        let mut settings = DevelopSettings { look: key.clone(), ..DevelopSettings::default() };
        let done = adobe::prepare(adobe, &mut probe, &mut settings);
        let mut notes: Vec<String> = done
            .camera_profile
            .iter()
            .chain(&done.look)
            .filter_map(|p| p.file_stem())
            .map(|s| format!("Adobe profile: {}", s.to_string_lossy()))
            .collect();
        notes.extend(done.notes);
        let prepared = Prepared { color: probe.color, info: probe.info, look: settings.look, notes };
        if self.entries.len() >= 8 {
            self.entries.remove(0);
        }
        self.entries.push((key.clone(), prepared));
        &self.entries.last().unwrap().1
    }
}

/// Display-referred linear ProPhoto → sRGB texture and its histogram.
pub fn to_color_image(img: &Image) -> (egui::ColorImage, Histogram) {
    let rgb = raw::encode_srgb8(img, 0.0);
    let mut hist = [[0u32; 256]; 3];
    for px in rgb.as_chunks::<3>().0 {
        for c in 0..3 {
            hist[c][px[c] as usize] += 1;
        }
    }
    (egui::ColorImage::from_rgb([img.width, img.height], &rgb), hist)
}
