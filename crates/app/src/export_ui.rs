//! The export dialog (Ctrl+Shift+E) and full-resolution export in the
//! background.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Instant;

use eframe::egui::{self, RichText, Ui};
use export::{ExportOptions, Format, Size};
use pipeline::adobe::{self, AdobeProfiles};
use pipeline::{Pipeline, RenderOptions, SourceKind};
use raw::{DecodeOptions, Photo};

use crate::state::Edit;
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeMode {
    Original,
    LongEdge,
    Percent,
    Fit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileFormat {
    Jpeg,
    Png,
    Tiff,
}

impl FileFormat {
    fn ext(self) -> &'static str {
        match self {
            FileFormat::Jpeg => "jpg",
            FileFormat::Png => "png",
            FileFormat::Tiff => "tif",
        }
    }
}

/// The dialog's settings; remembered between sessions.
#[derive(Clone, Debug)]
pub struct ExportSettings {
    pub size_mode: SizeMode,
    pub long_edge: usize,
    pub percent: f32,
    pub fit: [usize; 2],
    pub format: FileFormat,
    pub quality: u8,
    pub sharpen: f32,
    pub template: String,
    /// Output folder; the photo's own folder when empty.
    pub folder: Option<PathBuf>,
}

impl Default for ExportSettings {
    fn default() -> Self {
        ExportSettings {
            size_mode: SizeMode::LongEdge,
            long_edge: 2048,
            percent: 50.0,
            fit: [1920, 1080],
            format: FileFormat::Jpeg,
            quality: 92,
            sharpen: 0.0,
            template: "{name}-lumen".into(),
            folder: None,
        }
    }
}

impl ExportSettings {
    pub fn size(&self) -> Size {
        match self.size_mode {
            SizeMode::Original => Size::Original,
            SizeMode::LongEdge => Size::LongEdge(self.long_edge.max(1)),
            SizeMode::Percent => Size::Percent(self.percent.max(1.0)),
            SizeMode::Fit => Size::Fit(self.fit[0].max(1), self.fit[1].max(1)),
        }
    }

    pub fn format(&self) -> Format {
        match self.format {
            FileFormat::Jpeg => Format::Jpeg { quality: self.quality },
            FileFormat::Png => Format::Png,
            FileFormat::Tiff => Format::Tiff,
        }
    }

    pub fn output_path(&self, photo: &Path, full: [usize; 2], preset: Option<&str>) -> PathBuf {
        let (w, h) = self.size().dimensions(full[0], full[1]);
        let name = export::file_name(&self.template, photo, w.max(h), preset);
        let dir = self.folder.clone().or_else(|| photo.parent().map(Path::to_path_buf)).unwrap_or_default();
        dir.join(format!("{name}.{}", self.format.ext()))
    }

    /// One line per setting, for eframe's storage.
    pub fn to_storage(&self) -> String {
        let mode = match self.size_mode {
            SizeMode::Original => "original",
            SizeMode::LongEdge => "long",
            SizeMode::Percent => "percent",
            SizeMode::Fit => "fit",
        };
        let folder = self.folder.as_ref().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        [
            format!("mode={mode}"),
            format!("long={}", self.long_edge),
            format!("percent={}", self.percent),
            format!("fit={}x{}", self.fit[0], self.fit[1]),
            format!("format={}", self.format.ext()),
            format!("quality={}", self.quality),
            format!("sharpen={}", self.sharpen),
            format!("template={}", self.template),
            format!("folder={folder}"),
        ]
        .join("\n")
    }

    pub fn from_storage(s: &str) -> ExportSettings {
        let mut e = ExportSettings::default();
        for line in s.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k {
                "mode" => {
                    e.size_mode = match v {
                        "original" => SizeMode::Original,
                        "percent" => SizeMode::Percent,
                        "fit" => SizeMode::Fit,
                        _ => SizeMode::LongEdge,
                    }
                }
                "long" => e.long_edge = v.parse().unwrap_or(e.long_edge),
                "percent" => e.percent = v.parse().unwrap_or(e.percent),
                "fit" => {
                    if let Some((w, h)) = v.split_once('x') {
                        e.fit = [w.parse().unwrap_or(e.fit[0]), h.parse().unwrap_or(e.fit[1])];
                    }
                }
                "format" => {
                    e.format = match v {
                        "png" => FileFormat::Png,
                        "tif" => FileFormat::Tiff,
                        _ => FileFormat::Jpeg,
                    }
                }
                "quality" => e.quality = v.parse().unwrap_or(e.quality),
                "sharpen" => e.sharpen = v.parse().unwrap_or(e.sharpen),
                "template" if !v.is_empty() => e.template = v.into(),
                "folder" if !v.is_empty() => e.folder = Some(v.into()),
                _ => {}
            }
        }
        e
    }
}

pub enum DialogResult {
    None,
    Export,
    Cancel,
}

/// The dialog body. `full` is the photo's full-size dimensions.
pub fn dialog(ui: &mut Ui, s: &mut ExportSettings, photo: &Path, full: [usize; 2], preset: Option<&str>) -> DialogResult {
    let mut result = DialogResult::None;
    ui.label(RichText::new("Esporta").strong().size(16.0));
    ui.add_space(6.0);
    egui::Grid::new("export-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Dimensioni");
        ui.vertical(|ui| {
            ui.radio_value(&mut s.size_mode, SizeMode::Original, "Originale");
            ui.horizontal(|ui| {
                ui.radio_value(&mut s.size_mode, SizeMode::LongEdge, "Lato lungo");
                ui.add_enabled(s.size_mode == SizeMode::LongEdge, egui::DragValue::new(&mut s.long_edge).range(16..=20000).suffix(" px"));
            });
            ui.horizontal(|ui| {
                ui.radio_value(&mut s.size_mode, SizeMode::Percent, "Percentuale");
                ui.add_enabled(s.size_mode == SizeMode::Percent, egui::DragValue::new(&mut s.percent).range(1.0..=100.0).suffix(" %"));
            });
            ui.horizontal(|ui| {
                ui.radio_value(&mut s.size_mode, SizeMode::Fit, "Riquadro");
                ui.add_enabled_ui(s.size_mode == SizeMode::Fit, |ui| {
                    ui.add(egui::DragValue::new(&mut s.fit[0]).range(16..=20000));
                    ui.label("×");
                    ui.add(egui::DragValue::new(&mut s.fit[1]).range(16..=20000));
                });
            });
        });
        ui.end_row();

        ui.label("Formato");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut s.format, FileFormat::Jpeg, "JPEG");
            ui.selectable_value(&mut s.format, FileFormat::Png, "PNG 16 bit");
            ui.selectable_value(&mut s.format, FileFormat::Tiff, "TIFF 16 bit");
        });
        ui.end_row();

        if s.format == FileFormat::Jpeg {
            ui.label("Qualità");
            ui.add(egui::Slider::new(&mut s.quality, 1..=100));
            ui.end_row();
        }

        ui.label("Nitidezza output");
        ui.add(egui::Slider::new(&mut s.sharpen, 0.0..=100.0).fixed_decimals(0)).on_hover_text("Nitidezza per lo schermo, dopo il ridimensionamento (0 = nessuna)");
        ui.end_row();

        ui.label("Nome file");
        ui.vertical(|ui| {
            ui.add(egui::TextEdit::singleline(&mut s.template).desired_width(240.0));
            ui.label(RichText::new("{name} nome della foto · {preset} · {size} lato lungo").color(theme::TEXT_DIM).size(11.0));
        });
        ui.end_row();

        ui.label("Cartella");
        ui.horizontal(|ui| {
            let shown = s.folder.as_ref().map_or("quella della foto".to_string(), |f| f.display().to_string());
            ui.label(RichText::new(shown).monospace().size(11.5));
            if ui.small_button("Scegli…").clicked() {
                if let Some(dir) = rfd::FileDialog::new().set_title("Cartella di destinazione").pick_folder() {
                    s.folder = Some(dir);
                }
            }
            if s.folder.is_some() && ui.small_button("×").on_hover_text("Usa la cartella della foto").clicked() {
                s.folder = None;
            }
        });
        ui.end_row();
    });

    ui.add_space(8.0);
    let (w, h) = s.size().dimensions(full[0], full[1]);
    let out = s.output_path(photo, full, preset);
    ui.label(RichText::new(format!("{} — {w} × {h} px", out.display())).color(theme::TEXT_DIM).size(11.5));
    if out.exists() {
        ui.label(RichText::new("Il file esiste già e verrà sovrascritto").color(ui.visuals().warn_fg_color).size(11.5));
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button(RichText::new("Esporta").strong()).clicked() {
            result = DialogResult::Export;
        }
        if ui.button("Annulla").clicked() {
            result = DialogResult::Cancel;
        }
    });
    result
}

pub enum ExportEvent {
    Done { path: PathBuf, size: [usize; 2], ms: u128 },
    Failed { path: PathBuf, error: String },
}

/// Develops `photo` at full resolution and writes it, on its own thread.
pub fn spawn_export(photo: PathBuf, edit: Edit, out: PathBuf, opts: ExportOptions, adobe: AdobeProfiles, tx: Sender<ExportEvent>, ctx: egui::Context) {
    std::thread::Builder::new()
        .name("export".into())
        .spawn(move || {
            let t = Instant::now();
            let event = match run_export(&photo, &edit, &out, &opts, &adobe) {
                Ok(size) => ExportEvent::Done { path: out, size, ms: t.elapsed().as_millis() },
                Err(error) => ExportEvent::Failed { path: out, error },
            };
            tx.send(event).ok();
            ctx.request_repaint();
        })
        .expect("spawn export thread");
}

pub fn run_export(photo: &Path, edit: &Edit, out: &Path, opts: &ExportOptions, adobe: &AdobeProfiles) -> Result<[usize; 2], String> {
    let mut img = Photo::load(photo, &DecodeOptions::default()).map_err(|e| e.to_string())?;
    let mut settings = edit.settings.clone();
    adobe::prepare(adobe, &mut img, &mut settings);
    let mut pipeline = Pipeline::new(&settings, SourceKind::of(&img));
    if let Some(l) = &edit.lut {
        pipeline = pipeline.with_cube((*l.cube).clone(), edit.lut_amount / 100.0);
    }
    let rendered = pipeline.render(&img, &RenderOptions::default());
    let finished = export::finish(&rendered, &pipeline, opts);
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    export::save(&finished, &img.info, opts.format, out).map_err(|e| e.to_string())?;
    Ok([finished.width, finished.height])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_storage() {
        let s = ExportSettings {
            size_mode: SizeMode::Fit,
            fit: [1600, 900],
            format: FileFormat::Png,
            quality: 80,
            sharpen: 25.0,
            template: "{name}-{preset}".into(),
            folder: Some("/tmp/out".into()),
            ..Default::default()
        };
        let back = ExportSettings::from_storage(&s.to_storage());
        assert_eq!(back.size(), Size::Fit(1600, 900));
        assert_eq!(back.format, FileFormat::Png);
        assert_eq!((back.quality, back.sharpen), (80, 25.0));
        assert_eq!(back.template, s.template);
        assert_eq!(back.folder, s.folder);
    }

    #[test]
    fn output_names() {
        let s = ExportSettings { template: "{name}-{preset}-{size}".into(), ..Default::default() };
        let p = s.output_path(Path::new("/photos/P1.RW2"), [4592, 3448], Some("Dreamy"));
        assert_eq!(p, PathBuf::from("/photos/P1-Dreamy-2048.jpg"));
    }

    /// Full export of the sample RW2 with the sample preset, if present.
    #[test]
    fn exports_the_sample_raw() {
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let photo = root.join("samples/pixls/P1010041.RW2");
        if !photo.exists() {
            eprintln!("skipping: {} not found", photo.display());
            return;
        }
        let preset = xmp::Preset::load(root.join("samples/Dreamy days in spring.xmp")).unwrap();
        let mut edit = Edit::defaults(SourceKind::Raw);
        edit.apply_preset(&preset, "Dreamy").unwrap();
        let out = std::env::temp_dir().join(format!("lumen-app-export-{}.jpg", std::process::id()));
        let opts = ExportOptions { size: Size::LongEdge(800), format: Format::Jpeg { quality: 90 }, sharpen: 20.0 };
        let size = run_export(&photo, &edit, &out, &opts, &AdobeProfiles::default()).unwrap();
        assert_eq!(size[0].max(size[1]), 800);
        let back = image::open(&out).unwrap();
        assert_eq!((back.width() as usize, back.height() as usize), (size[0], size[1]));
        std::fs::remove_file(out).ok();
    }
}
