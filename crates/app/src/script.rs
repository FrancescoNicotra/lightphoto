//! Scripted runs for development: `LUMEN_SCRIPT` drives the app through a
//! sequence of steps and saves screenshots, so the UI can be checked (and
//! the docs' screenshots remade) without clicking by hand.
//!
//! Steps are separated by `;` or new lines:
//!
//! ```text
//! wait                      until the photo, thumbnails and render are ready
//! preset <name>             apply the first preset whose name contains <name>
//! crs <Key> <value>         set a Camera Raw setting, e.g. `crs Exposure2012 0.5`
//! drag <Key> <from> <to> <n> move a setting over n frames as a slider drag
//! undo | redo | reset
//! before on|off             before/after view
//! zoom fit|100|200
//! panel <title>             open only this develop panel ("Base", "Curva"…)
//! tab curve|hsl|grading <n> select a tab inside a panel
//! exportdialog | closedialog
//! export                    export with the dialog's settings
//! next | prev               filmstrip navigation
//! shot <file.png>           save a screenshot of the window
//! quit
//! ```

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use xmp::{Props, Value};

pub struct Script {
    steps: VecDeque<String>,
    /// No step before this time (lets the UI settle and animations end).
    pause_until: Instant,
    drag: Option<Drag>,
    shot: Option<(PathBuf, Receiver<Arc<egui::ColorImage>>)>,
}

struct Drag {
    key: String,
    from: f32,
    to: f32,
    steps: u32,
    done: u32,
}

/// What a step asks of the app.
pub enum Step {
    Wait,
    Preset(String),
    Set(Props),
    Undo,
    Redo,
    Reset,
    Before(bool),
    Zoom(Option<f32>),
    Panel(String),
    Tab(String, usize),
    ExportDialog(bool),
    Export,
    Photo(isize),
    Quit,
}

impl Script {
    pub fn from_env() -> Option<Script> {
        let text = std::env::var("LUMEN_SCRIPT").ok()?;
        let steps = text.split([';', '\n']).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
        Some(Script { steps, pause_until: Instant::now(), drag: None, shot: None })
    }

    /// True while a scripted drag is moving a value (renders as drafts).
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The next step to run this frame, if any. `idle` says whether the app
    /// has finished loading and rendering.
    pub fn next(&mut self, ctx: &egui::Context, idle: bool) -> Option<Step> {
        ctx.request_repaint();
        if let Some((path, rx)) = &self.shot {
            match rx.try_recv() {
                Ok(img) => {
                    save_png(&img, path);
                    self.shot = None;
                }
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => self.shot = None,
            }
        }
        if let Some(d) = &mut self.drag {
            d.done += 1;
            let t = d.done as f32 / d.steps.max(1) as f32;
            let v = d.from + (d.to - d.from) * t.min(1.0);
            let mut props = Props::default();
            props.set_crs(&d.key, Value::Text(v.to_string()));
            if d.done >= d.steps {
                self.drag = None;
            }
            return Some(Step::Set(props));
        }
        if Instant::now() < self.pause_until {
            return None;
        }
        let line = self.steps.front()?.clone();
        let mut words = line.split_whitespace();
        let cmd = words.next().unwrap_or("");
        let rest: Vec<&str> = words.collect();
        if cmd == "wait" && !idle {
            return None;
        }
        self.steps.pop_front();
        // Let panel animations finish before the next screenshot.
        let ms = if matches!(cmd, "panel" | "tab" | "exportdialog" | "closedialog") { 600 } else { 50 };
        self.pause_until = Instant::now() + Duration::from_millis(ms);
        eprintln!("script: {line}");
        let arg = rest.join(" ");
        Some(match cmd {
            "wait" => Step::Wait,
            "preset" => Step::Preset(arg),
            "crs" if rest.len() == 2 => {
                let mut props = Props::default();
                props.set_crs(rest[0], Value::Text(rest[1].into()));
                Step::Set(props)
            }
            "drag" if rest.len() == 4 => {
                let num = |s: &str| s.parse::<f32>().unwrap_or(0.0);
                self.drag = Some(Drag { key: rest[0].into(), from: num(rest[1]), to: num(rest[2]), steps: rest[3].parse().unwrap_or(10), done: 0 });
                Step::Wait
            }
            "undo" => Step::Undo,
            "redo" => Step::Redo,
            "reset" => Step::Reset,
            "before" => Step::Before(arg != "off"),
            "zoom" => Step::Zoom(arg.parse::<f32>().ok().map(|p| p / 100.0)),
            "panel" => Step::Panel(arg),
            "tab" if rest.len() == 2 => Step::Tab(rest[0].into(), rest[1].parse().unwrap_or(0)),
            "exportdialog" => Step::ExportDialog(true),
            "closedialog" => Step::ExportDialog(false),
            "export" => Step::Export,
            "next" => Step::Photo(1),
            "prev" => Step::Photo(-1),
            "shot" => {
                let (tx, rx) = mpsc::channel();
                let c = ctx.clone();
                ctx.request_screenshot(move |img| {
                    tx.send(img).ok();
                    c.request_repaint();
                });
                self.shot = Some((PathBuf::from(arg), rx));
                Step::Wait
            }
            "quit" => Step::Quit,
            _ => {
                eprintln!("script: unknown step {line:?}");
                Step::Wait
            }
        })
    }
}

fn save_png(img: &egui::ColorImage, path: &std::path::Path) {
    let [w, h] = img.size;
    let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
    match image::RgbaImage::from_raw(w as u32, h as u32, bytes).map(|i| i.save(path)) {
        Some(Ok(())) => eprintln!("script: saved {}", path.display()),
        _ => eprintln!("script: could not save {}", path.display()),
    }
}
