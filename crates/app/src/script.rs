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
//! panel <key>               open only this develop panel: profile, basic, curve,
//!                           hsl, grading, detail, effects, calibration
//! tab curve|hsl|grading <n> select a tab inside a panel
//! amount <0..200>           strength of the preset just applied (%)
//! exportdialog | closedialog
//! exportdir <folder>        output folder of the export dialog
//! export                    export the selection with the dialog's settings
//! next | prev               filmstrip navigation
//! open <n>                  open photo n of the filmstrip (from 0)
//! select all | none | <n>,<m>… | add <n> | range <n>
//!                           filmstrip selection (as ⌘A, click, ⌘-click, ⇧-click)
//! copy | paste              copy the open photo's settings, paste to the selection
//! crop [<l> <t> <r> <b>] [angle <deg>] [aspect <n> [portrait]]
//!                           open the crop tool and set it (fractions of the photo,
//!                           aspect index as in the tool's list)
//! cropdone | cropcancel     close the crop tool
//! compare off|split|side    before/after view
//! pick <u> <v>              white balance eyedropper at (u, v) of the shown photo
//! auto                      Auto tone
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

pub enum Selection {
    All,
    None,
    Only(Vec<usize>),
    Add(usize),
    Range(usize),
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
    Open(usize),
    Select(Selection),
    Copy,
    Paste,
    Amount(f32),
    ExportDir(PathBuf),
    Crop { rect: Option<pipeline::geometry::Rect>, angle: Option<f32>, aspect: Option<(usize, bool)> },
    CropDone(bool),
    Compare(crate::viewer::Compare),
    Pick(f32, f32),
    Auto,
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
        let ms = if matches!(cmd, "panel" | "tab" | "exportdialog" | "closedialog" | "compare" | "crop" | "cropdone") { 600 } else { 50 };
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
            "open" => Step::Open(arg.parse().unwrap_or(0)),
            "select" => Step::Select(match rest.first().copied() {
                Some("all") => Selection::All,
                Some("none") => Selection::None,
                Some("add") => Selection::Add(rest.get(1).and_then(|n| n.parse().ok()).unwrap_or(0)),
                Some("range") => Selection::Range(rest.get(1).and_then(|n| n.parse().ok()).unwrap_or(0)),
                _ => Selection::Only(arg.split(',').filter_map(|n| n.trim().parse().ok()).collect()),
            }),
            "copy" => Step::Copy,
            "paste" => Step::Paste,
            "amount" => Step::Amount(arg.parse().unwrap_or(100.0)),
            "exportdir" => Step::ExportDir(PathBuf::from(arg)),
            "crop" => {
                let num = |i: usize| rest.get(i).and_then(|s| s.parse::<f32>().ok());
                let rect = match (num(0), num(1), num(2), num(3)) {
                    (Some(left), Some(top), Some(right), Some(bottom)) => Some(pipeline::geometry::Rect { left, top, right, bottom }),
                    _ => None,
                };
                let after = |key: &str| rest.iter().position(|w| *w == key).map(|i| i + 1);
                let angle = after("angle").and_then(num);
                let aspect = after("aspect").and_then(|i| rest.get(i)?.parse().ok()).map(|n| (n, rest.contains(&"portrait")));
                Step::Crop { rect, angle, aspect }
            }
            "cropdone" => Step::CropDone(false),
            "cropcancel" => Step::CropDone(true),
            "compare" => Step::Compare(match arg.as_str() {
                "split" => crate::viewer::Compare::Split,
                "side" => crate::viewer::Compare::SideBySide,
                _ => crate::viewer::Compare::Single,
            }),
            "pick" if rest.len() == 2 => Step::Pick(rest[0].parse().unwrap_or(0.5), rest[1].parse().unwrap_or(0.5)),
            "auto" => Step::Auto,
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
