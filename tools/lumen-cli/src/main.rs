//! Develop RAW files from the command line.
//!
//! usage: see `USAGE`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use export::{ExportOptions, Format, Size};
use pipeline::adobe::{self, AdobeProfiles};
use pipeline::{Pipeline, RenderOptions, SourceKind};
use raw::{DecodeOptions, Photo, TempTint};
use xmp::{DevelopSettings, Preset, WhiteBalance};

const USAGE: &str = "usage:
  lumen-cli info    <photo>
  lumen-cli develop <photo>... -o <out.jpg | dir/> [options]
  lumen-cli preview <photo> -o out.jpg       (extracts the preview embedded in a RAW file)
  lumen-cli cube    <preset.xmp> -o out.cube [--grid 33]

develop options:
  --preset P.xmp      Lightroom/Camera Raw preset (.xmp, or a .dng/.jpg carrying one)
  --lut L.cube        3D LUT applied last, on sRGB values; --lut-amount 0..1 (default 1)
  --size S            original (default), 2048, 50% or 1920x1080
  --quality Q         JPEG quality, default 92
  --sharpen N         output sharpening 0..100 after resizing, default 0
  --format F          jpg (default), png or tif, when -o is a directory
  --name T            file name template in a directory, default {name}; also {preset}, {size}
  --exposure EV       added to the preset's exposure
  --temp K [--tint T] white balance, overriding the preset
  --fast              develop at the output size instead of full size (previews)
  --half              decode at half resolution (2x2 binning)
  --no-adobe          do not use Adobe's camera/look profiles from a local Lightroom or
                      Camera Raw install (found automatically, or in LUMEN_ADOBE_DIR)

  .jpg is 8 bit, .png/.tif 16 bit; all carry an sRGB ICC profile";

#[derive(PartialEq)]
enum Command {
    Info,
    Develop,
    Preview,
    Cube,
}

struct Args {
    command: Command,
    inputs: Vec<PathBuf>,
    output: Option<PathBuf>,
    preset: Option<PathBuf>,
    lut: Option<PathBuf>,
    lut_amount: f32,
    size: Size,
    quality: u8,
    sharpen: f32,
    format: Option<String>,
    name: String,
    exposure: f32,
    temperature: Option<f32>,
    tint: f32,
    fast: bool,
    half: bool,
    no_adobe: bool,
    grid: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let command = match it.next().as_deref() {
        Some("info") => Command::Info,
        Some("develop") => Command::Develop,
        Some("preview") => Command::Preview,
        Some("cube") => Command::Cube,
        Some("-h" | "--help") | None => return Err(String::new()),
        Some(c) => return Err(format!("unknown command {c:?}")),
    };
    let mut args = Args {
        command,
        inputs: Vec::new(),
        output: None,
        preset: None,
        lut: None,
        lut_amount: 1.0,
        size: Size::Original,
        quality: 92,
        sharpen: 0.0,
        format: None,
        name: "{name}".into(),
        exposure: 0.0,
        temperature: None,
        tint: 0.0,
        fast: false,
        half: false,
        no_adobe: false,
        grid: 33,
    };
    fn num<T: std::str::FromStr>(flag: &str, v: Option<String>) -> Result<T, String> {
        v.as_deref().and_then(|s| s.parse().ok()).ok_or(format!("{flag} needs a number"))
    }
    fn value(flag: &str, v: Option<String>) -> Result<String, String> {
        v.ok_or(format!("{flag} needs a value"))
    }
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" | "--output" => args.output = Some(value("-o", it.next())?.into()),
            "--preset" => args.preset = Some(value("--preset", it.next())?.into()),
            "--lut" => args.lut = Some(value("--lut", it.next())?.into()),
            "--lut-amount" => args.lut_amount = num("--lut-amount", it.next())?,
            "--size" => args.size = value("--size", it.next())?.parse()?,
            "--quality" => args.quality = num::<u8>("--quality", it.next())?.min(100),
            "--sharpen" => args.sharpen = num("--sharpen", it.next())?,
            "--format" => args.format = Some(value("--format", it.next())?),
            "--name" => args.name = value("--name", it.next())?,
            "--exposure" => args.exposure = num("--exposure", it.next())?,
            "--temp" => args.temperature = Some(num("--temp", it.next())?),
            "--tint" => args.tint = num("--tint", it.next())?,
            "--fast" => args.fast = true,
            "--half" => args.half = true,
            "--no-adobe" => args.no_adobe = true,
            "--grid" => args.grid = num::<usize>("--grid", it.next())?.clamp(2, 129),
            "-h" | "--help" => return Err(String::new()),
            _ if a.starts_with("--") => return Err(format!("unknown option {a:?}")),
            _ => args.inputs.push(a.into()),
        }
    }
    if args.inputs.is_empty() {
        return Err("missing input file".into());
    }
    if args.command != Command::Develop && args.inputs.len() > 1 {
        return Err("this command takes a single input".into());
    }
    if args.command != Command::Info && args.output.is_none() {
        return Err("missing -o <output>".into());
    }
    Ok(args)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("error: {msg}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = match args.command {
        Command::Info => info(&args.inputs[0]),
        Command::Develop => develop(&args),
        Command::Preview => preview(&args),
        Command::Cube => cube(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

type CliResult<T> = Result<T, Box<dyn std::error::Error>>;

fn info(input: &Path) -> CliResult<()> {
    let t = Instant::now();
    let photo = Photo::load(input, &DecodeOptions::default())?;
    let decode_ms = t.elapsed().as_millis();
    let i = &photo.info;

    println!("File         {}", input.display());
    if !i.make.is_empty() {
        println!("Camera       {} {}", i.make, i.model);
    }
    if let Some(lens) = &i.lens {
        println!("Lens         {lens}");
    }
    let mut exposure = Vec::new();
    if let Some(t) = i.exposure_time {
        exposure.push(if t < 1.0 { format!("1/{:.0} s", 1.0 / t) } else { format!("{t} s") });
    }
    if let Some(f) = i.f_number {
        exposure.push(format!("f/{f:.1}"));
    }
    if let Some(iso) = i.iso {
        exposure.push(format!("ISO {iso}"));
    }
    if let Some(fl) = i.focal_length {
        exposure.push(format!("{fl:.0} mm"));
    }
    if !exposure.is_empty() {
        println!("Exposure     {}", exposure.join(", "));
    }
    if let Some(d) = &i.date_time {
        println!("Taken        {d}");
    }
    println!("Image        {} × {}  ({:?})", photo.data.width, photo.data.height, i.orientation);
    println!("Decoded in   {decode_ms} ms");

    if let (Some(r), raw::ColorSource::Camera(cam)) = (&i.raw, &photo.color) {
        let [x, y, w, h] = r.crop;
        println!("\n[Sensor]");
        println!("  Format             {}", if r.is_dng { "DNG" } else { "camera RAW" });
        println!("  Size               {} × {}", r.sensor_size.0, r.sensor_size.1);
        println!("  Default crop       {w} × {h} at ({x}, {y})");
        println!("  CFA                {}", r.cfa);
        println!("  Black / white      {:?} / {:?}", r.black_level, r.white_level);
        println!("  Baseline exposure  {:+.2} EV", i.baseline_exposure);

        let tt = TempTint::from_xy(r.as_shot_white);
        let n = r.as_shot_neutral;
        println!("\n[White balance]");
        println!("  As shot neutral    {:.4} {:.4} {:.4}", n[0], n[1], n[2]);
        println!("  As shot white      x {:.4}  y {:.4}", r.as_shot_white.x, r.as_shot_white.y);
        println!("  Temperature/Tint   {:.0} K / {:+.0}", tt.temperature, tt.tint);

        println!("\n[Profile]");
        println!("  Forward matrix     {}", if cam.profile.has_forward_matrix() { "yes" } else { "no" });
        for c in &cam.profile.calibrations {
            println!("  Calibration        {:.0} K", c.temperature);
            print_matrix("    ColorMatrix", &c.color_matrix);
            if let Some(fm) = &c.forward_matrix {
                print_matrix("    ForwardMatrix", fm);
            }
        }
    }

    if !raw::is_rendered_image(input) {
        if let Ok(Some(p)) = raw::embedded_preview(input) {
            println!("\nEmbedded preview  {} × {}", p.width(), p.height());
        }
    }
    Ok(())
}

fn print_matrix(label: &str, m: &raw::color::Mat3) {
    for (i, row) in m.iter().enumerate() {
        let name = if i == 0 { label } else { "" };
        println!("{name:<21}{:>8.4} {:>8.4} {:>8.4}", row[0], row[1], row[2]);
    }
}

/// Default settings plus the preset, if any.
fn load_settings(preset: Option<&Path>) -> CliResult<(DevelopSettings, Option<String>)> {
    let mut settings = DevelopSettings::default();
    let Some(path) = preset else { return Ok((settings, None)) };
    let preset = Preset::load(path)?;
    settings.try_apply(&preset.packet.props)?;
    let name = preset.name.or_else(|| path.file_stem().and_then(|s| s.to_str()).map(str::to_string));
    Ok((settings, name))
}

fn develop(args: &Args) -> CliResult<()> {
    let (mut settings, preset_name) = load_settings(args.preset.as_deref())?;
    settings.basic.exposure += args.exposure;
    if let Some(t) = args.temperature {
        settings.white_balance = WhiteBalance::Custom;
        settings.temperature = Some(t);
        settings.tint = Some(args.tint);
    }
    let cube = args.lut.as_ref().map(lut::load).transpose()?;

    let out = args.output.as_ref().unwrap();
    let to_dir = args.inputs.len() > 1 || out.is_dir() || out.to_string_lossy().ends_with(['/', '\\']);
    if to_dir {
        std::fs::create_dir_all(out)?;
    }
    let ext = args.format.clone().unwrap_or_else(|| "jpg".into());
    let adobe_profiles = if args.no_adobe { AdobeProfiles::default() } else { AdobeProfiles::discover() };

    for input in &args.inputs {
        let t = Instant::now();
        let mut photo = Photo::load(input, &DecodeOptions { half_size: args.half })?;
        let mut settings = settings.clone();
        let prepared = adobe::prepare(&adobe_profiles, &mut photo, &mut settings);
        let t_decode = t.elapsed();

        let mut pipeline = Pipeline::new(&settings, SourceKind::of(&photo));
        if let Some(c) = &cube {
            pipeline = pipeline.with_cube(c.clone(), args.lut_amount);
        }
        let (w, h) = args.size.dimensions(photo.data.width, photo.data.height);
        let max_size = args.fast.then_some(w.max(h));
        let rendered = pipeline.render(&photo, &RenderOptions { max_size, ..Default::default() });
        let t_render = t.elapsed() - t_decode;

        let path = if to_dir {
            let name = export::file_name(&args.name, input, w.max(h), preset_name.as_deref());
            out.join(format!("{name}.{ext}"))
        } else {
            out.clone()
        };
        let format = Format::from_path(&path, args.quality)
            .ok_or_else(|| format!("unsupported output format {:?} (use .jpg, .png or .tif)", path.display()))?;
        let opts = ExportOptions { size: args.size, format, sharpen: args.sharpen };
        let finished = export::finish(&rendered, &pipeline, &opts);
        export::save(&finished, &photo.info, format, &path)?;

        if input == &args.inputs[0] {
            for p in prepared.camera_profile.iter().chain(&prepared.look) {
                eprintln!("using Adobe profile {}", p.display());
            }
            for w in prepared.notes.iter().chain(&pipeline.warnings) {
                eprintln!("note: {w}");
            }
        }
        println!(
            "wrote {} ({} × {}) — decode {} ms, develop {} ms, total {} ms",
            path.display(),
            finished.width,
            finished.height,
            t_decode.as_millis(),
            t_render.as_millis(),
            t.elapsed().as_millis()
        );
    }
    Ok(())
}

fn preview(args: &Args) -> CliResult<()> {
    let out = args.output.as_ref().unwrap();
    let img = raw::embedded_preview(&args.inputs[0])?.ok_or("the file has no embedded preview")?;
    img.to_rgb8().save(out)?;
    println!("wrote {} ({} × {})", out.display(), img.width(), img.height());
    Ok(())
}

fn cube(args: &Args) -> CliResult<()> {
    let (settings, name) = load_settings(Some(&args.inputs[0]))?;
    let out = args.output.as_ref().unwrap();
    let title = format!("{} (Lumen, point-wise part only)", name.as_deref().unwrap_or("preset"));
    Pipeline::to_cube(&settings, args.grid).write_cube(out, Some(&title))?;
    println!("wrote {out:?}: {0}×{0}×{0}, sRGB in / sRGB out", args.grid);
    println!("note: white balance, Texture/Clarity/Dehaze, sharpening, noise reduction, vignette and grain are not in the LUT");
    Ok(())
}
