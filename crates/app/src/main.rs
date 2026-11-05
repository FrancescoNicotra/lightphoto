//! Lumen: a Lightroom-style RAW editor that applies Lightroom/Camera Raw
//! presets and LUTs.
//!
//! usage: lumen [folder | photo] [--presets folder]

// No console window next to the app in Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod export_ui;
mod library;
mod panels;
mod script;
mod state;
mod theme;
mod viewer;
mod widgets;
mod worker;

use std::path::PathBuf;

use eframe::egui;

/// Command-line arguments: what to open at start.
#[derive(Default)]
pub struct Args {
    pub open: Option<PathBuf>,
    pub presets: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = std::env::args_os().skip(1);
    while let Some(a) = it.next() {
        match a.to_str() {
            Some("--presets") => args.presets = Some(it.next().ok_or("--presets needs a folder")?.into()),
            Some("-h" | "--help") => return Err("usage: lumen [folder | photo] [--presets folder]".into()),
            _ => args.open = Some(a.into()),
        }
    }
    Ok(args)
}

fn main() -> eframe::Result {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Lumen")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Lumen", options, Box::new(|cc| Ok(Box::new(app::LumenApp::new(cc, args)))))
}
