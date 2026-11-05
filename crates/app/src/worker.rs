//! The render worker: decodes the open photo and develops it off the UI
//! thread. Requests go into a one-slot mailbox, so a request that has not
//! started yet is replaced by the next one: while a slider moves only the
//! latest value is rendered.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use eframe::egui;
use pipeline::adobe::{self, AdobeProfiles};
use pipeline::{Pipeline, RenderOptions, SourceKind};
use raw::{ColorSource, DecodeOptions, Image, Photo, PhotoInfo, TempTint};
use xmp::{DevelopSettings, Look};

use crate::state::Edit;

/// Long edge of the interactive preview.
pub const PREVIEW_SIZE: usize = 2048;
/// Long edge of the quick render shown while a slider is being dragged.
pub const DRAFT_SIZE: usize = 1024;

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
    /// What is not reproduced, and which Adobe profiles are in use.
    pub notes: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Quality {
    Draft,
    Preview,
    Full,
}

#[derive(Default)]
struct Mailbox {
    load: Option<(PathBuf, u64)>,
    render: Option<RenderJob>,
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
                let mut state = WorkerState { adobe, tx, ctx, photo: None };
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
        let (lock, cv) = &*self.mailbox;
        lock.lock().unwrap().render = Some(job);
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
}

struct WorkerState {
    adobe: AdobeProfiles,
    tx: Sender<Event>,
    ctx: egui::Context,
    photo: Option<OpenPhoto>,
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
    /// Long edge of the full-size photo, for pixel radii.
    full_size: usize,
    profiles: ProfileCache,
    /// Which cache entry `preview` / `full` currently carry.
    applied: Option<Option<Look>>,
    applied_full: Option<Option<Look>>,
    before_done: bool,
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
                    if let Some(job) = m.render.take() {
                        break Task::Render(Box::new(job));
                    }
                    m = cv.wait(m).unwrap();
                }
            };
            let pending = || {
                let m = mailbox.0.lock().unwrap();
                m.load.is_some() || m.render.is_some() || m.quit
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
                        self.render_before(job.generation);
                    }
                    if job.detail && !pending() {
                        self.render(&job, Quality::Full);
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
        let t = Instant::now();
        let photo = match Photo::load(&path, &DecodeOptions { half_size: true }) {
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
        let profiles = ProfileCache::new(&preview);
        self.photo = Some(OpenPhoto {
            generation,
            path: path.clone(),
            preview,
            full: None,
            full_size: size[0].max(size[1]),
            profiles,
            applied: None,
            applied_full: None,
            before_done: false,
        });
        self.send(Event::Loaded { generation, path, info: Box::new(info), kind, size, decode_ms });
    }

    /// The default rendering for this kind of file, once per photo, after
    /// the first render of the edit.
    fn render_before(&mut self, generation: u64) {
        let Some(open) = self.photo.as_mut().filter(|p| p.generation == generation && !p.before_done) else { return };
        open.before_done = true;
        let kind = SourceKind::of(&open.preview);
        let before = RenderJob { id: 0, generation, edit: Edit::defaults(kind), draft: false, detail: false };
        if let Some(d) = self.develop(&before, Quality::Preview) {
            let (image, _) = to_color_image(&d.img);
            self.send(Event::Before { generation, image });
        }
    }

    fn render(&mut self, job: &RenderJob, quality: Quality) {
        let t = Instant::now();
        let Some(Developed { img, white_balance, notes }) = self.develop(job, quality) else { return };
        let (image, histogram) = to_color_image(&img);
        let ms = t.elapsed().as_millis();
        let r = Rendered { generation: job.generation, id: job.id, image, histogram, ms, quality, white_balance, notes };
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

        let photo = if quality == Quality::Full {
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
        let max_size = (quality == Quality::Draft).then_some(DRAFT_SIZE);
        let mut img = pipeline.render(photo, &RenderOptions { max_size, full_size: Some(open.full_size) });
        pipeline.grain(&mut img);

        let white_balance = photo.as_shot_white().map(|as_shot| {
            let xy = pipeline.white_point(photo).unwrap_or(as_shot);
            let tt = TempTint::from_xy(xy);
            (tt.temperature as f32, tt.tint as f32)
        });
        let mut notes = notes;
        notes.extend(pipeline.warnings.iter().cloned());
        Some(Developed { img, white_balance, notes })
    }
}

/// A developed image (with grain), the white balance in effect and notes
/// for the user.
struct Developed {
    img: Image,
    white_balance: Option<(f32, f32)>,
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
