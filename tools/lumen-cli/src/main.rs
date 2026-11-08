//! Develop RAW files from the command line.
//!
//! usage: see `USAGE`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use export::{ExportOptions, Format, Size};
use pipeline::adobe::{self, AdobeProfiles};
use pipeline::{Pipeline, RenderOptions, SourceKind};
use raw::{CameraData, ColorSource, DecodeOptions, DngEncoding, LinearDng, Orientation, Photo, PhotoInfo, TempTint};
use xmp::{DevelopSettings, Preset, WhiteBalance};

const USAGE: &str = "usage:
  lumen-cli info    <photo>
  lumen-cli develop <photo>... -o <out.jpg | dir/> [options]
  lumen-cli preview <photo> -o out.jpg       (extracts the preview embedded in a RAW file)
  lumen-cli dng     <raw> -o out.dng [--half] [--uncompressed] [--no-adobe]
                    (rewrites a RAW as a linear floating point DNG, the format of HDR merges)
  lumen-cli cube    <preset.xmp> -o out.cube [--grid 33]
  lumen-cli hdr     <raw> <raw>... [-o out.dng] [--no-align] [--auto-crop] [--uncompressed]
                    [--timings] [--no-adobe]
                    (merges 2–9 bracketed RAW files into a floating point DNG; without -o,
                    <first>-HDR.dng next to the first file)

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

  .jpg is 8 bit, .png/.tif 16 bit; all carry an sRGB ICC profile

hdr options:
  --no-align          do not align the shots (tripod)
  --auto-crop         crop (crs:Crop*, editable) to the area every aligned shot covers
  --timings           print the time of each step

dng options:
  --half              decode at half resolution first
  --uncompressed      32 bit floats in plain strips instead of compressed half floats (debugging)
  --no-adobe          render the embedded preview without Adobe's profiles";

#[derive(PartialEq)]
enum Command {
    Info,
    Develop,
    Preview,
    Cube,
    Dng,
    Hdr,
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
    uncompressed: bool,
    grid: usize,
    no_align: bool,
    auto_crop: bool,
    timings: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let command = match it.next().as_deref() {
        Some("info") => Command::Info,
        Some("develop") => Command::Develop,
        Some("preview") => Command::Preview,
        Some("cube") => Command::Cube,
        Some("dng") => Command::Dng,
        Some("hdr") => Command::Hdr,
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
        uncompressed: false,
        grid: 33,
        no_align: false,
        auto_crop: false,
        timings: false,
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
            "--uncompressed" => args.uncompressed = true,
            "--no-align" => args.no_align = true,
            "--auto-crop" => args.auto_crop = true,
            "--timings" => args.timings = true,
            "--grid" => args.grid = num::<usize>("--grid", it.next())?.clamp(2, 129),
            "-h" | "--help" => return Err(String::new()),
            _ if a.starts_with("--") => return Err(format!("unknown option {a:?}")),
            _ => args.inputs.push(a.into()),
        }
    }
    if args.inputs.is_empty() {
        return Err("missing input file".into());
    }
    if !matches!(args.command, Command::Develop | Command::Hdr) && args.inputs.len() > 1 {
        return Err("this command takes a single input".into());
    }
    if !matches!(args.command, Command::Info | Command::Hdr) && args.output.is_none() {
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
        Command::Dng => dng(&args),
        Command::Hdr => hdr(&args),
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
        let (cw, ch) = pipeline::geometry::cropped_size(photo.data.width, photo.data.height, &settings.crop, photo.info.orientation);
        let (w, h) = args.size.dimensions(cw, ch);
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

/// A RAW file rewritten as the DNG an HDR merge of that single frame would
/// give: linear camera RGB in half floats, the source's colour tags and EXIF,
/// and a preview rendered with the default settings.
fn dng(args: &Args) -> CliResult<()> {
    let input = &args.inputs[0];
    let out = args.output.as_ref().unwrap();
    let t = Instant::now();
    let capture = raw::load_capture(input, &DecodeOptions { half_size: args.half })?;
    let t_decode = t.elapsed();

    // The preview is stored in sensor orientation, like the raw data.
    let full_size = capture.data.width.max(capture.data.height);
    let mut photo = Photo {
        info: PhotoInfo { orientation: Orientation::Normal, ..capture.info.clone() },
        data: capture.data.downscale(raw::dng_write::PREVIEW_SIZE as usize),
        color: ColorSource::Camera(Box::new(CameraData { profile: capture.profile.clone(), as_shot_neutral: capture.as_shot_neutral })),
    };
    let mut settings = DevelopSettings::default();
    let adobe_profiles = if args.no_adobe { AdobeProfiles::default() } else { AdobeProfiles::discover() };
    let prepared = adobe::prepare(&adobe_profiles, &mut photo, &mut settings);
    let pipeline = Pipeline::new(&settings, SourceKind::of(&photo));
    let rendered = pipeline.render(&photo, &RenderOptions { max_size: None, full_size: Some(full_size) });
    let preview = image::RgbImage::from_raw(rendered.width as u32, rendered.height as u32, raw::encode_srgb8(&rendered, 0.0))
        .ok_or("preview size mismatch")?;
    if let Some(p) = &prepared.camera_profile {
        eprintln!("preview uses Adobe profile {}", p.display());
    }
    let t_preview = t.elapsed() - t_decode;

    let xmp = format!(
        r#"<?xpacket begin="{bom}" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:CreatorTool="Lumen {version}"/>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#,
        bom = '\u{feff}',
        version = env!("CARGO_PKG_VERSION")
    );
    let dng = LinearDng { xmp: Some(xmp.as_bytes()), preview: Some(&preview), ..LinearDng::from_capture(&capture) };
    let encoding = if args.uncompressed { DngEncoding::F32Uncompressed } else { DngEncoding::HalfDeflate };
    raw::write_linear_dng(out, &dng, encoding)?;
    let t_write = t.elapsed() - t_decode - t_preview;

    let bytes = std::fs::metadata(out)?.len();
    println!(
        "wrote {} ({} × {}, {}, {:.1} MB, {:.1} bytes/pixel) — decode {} ms, preview {} ms, write {} ms",
        out.display(),
        capture.data.width,
        capture.data.height,
        if args.uncompressed { "32 bit float" } else { "16 bit half float" },
        bytes as f64 / 1e6,
        bytes as f64 / (capture.data.width * capture.data.height) as f64,
        t_decode.as_millis(),
        t_preview.as_millis(),
        t_write.as_millis()
    );
    Ok(())
}

/// Merges a bracket into a floating point HDR DNG.
fn hdr(args: &Args) -> CliResult<()> {
    let t = Instant::now();
    let opts = merge::MergeOptions {
        align: !args.no_align,
        auto_crop: args.auto_crop,
        encoding: if args.uncompressed { DngEncoding::F32Uncompressed } else { DngEncoding::HalfDeflate },
        adobe: if args.no_adobe { AdobeProfiles::default() } else { AdobeProfiles::discover() },
    };
    let sources = args.inputs.iter().cloned().map(merge::Source::File).collect();
    let (merged, written) = merge::merge_hdr(sources, args.output.as_deref(), &opts, &merge::Control::default())?;

    for f in &merged.frames {
        let exif = f.exif_ev.map_or_else(|| "no EXIF".to_string(), |e| format!("EXIF {e:+.2}"));
        let align = match &f.alignment {
            merge::AlignStatus::Reference => "reference".to_string(),
            merge::AlignStatus::Disabled => "not aligned".to_string(),
            merge::AlignStatus::Failed => "alignment failed".to_string(),
            merge::AlignStatus::Aligned { correlation, model } => {
                let (dx, dy) = f.transform.apply(0.5, 0.5);
                format!("aligned ({model:?}, ρ {correlation:.4}, shift {:+.1} {:+.1} px)", dx - 0.5, dy - 0.5)
            }
        };
        println!("  {:<24} {:+.2} EV ({exif}, {:?}), {align}", f.name, f.ev, f.ev_source);
    }
    for w in &merged.warnings {
        eprintln!("warning: {w}");
    }
    if let Some(c) = merged.crop {
        println!("  auto crop: left {:.3}, top {:.3}, right {:.3}, bottom {:.3}", c.left, c.top, c.right, c.bottom);
    }
    println!(
        "wrote {} ({} × {}, {:.1} MB), BaselineExposure {:+.2} EV, BaselineNoise {:.2} — {} ms",
        written.path.display(),
        written.width,
        written.height,
        written.bytes as f64 / 1e6,
        written.baseline_exposure,
        merged.baseline_noise,
        t.elapsed().as_millis()
    );
    if args.timings {
        let tm = &merged.timings;
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
        println!("timings (ms):");
        for (name, d) in [
            ("decode, half size", tm.decode_half),
            ("align + warp, half size", tm.align),
            ("exposures", tm.exposure),
            ("ghosts", tm.ghosts),
            ("decode, full size", tm.decode_full),
            ("warp + accumulate", tm.accumulate),
            ("scale + preview", tm.preview),
            ("write", tm.write),
        ] {
            println!("  {name:<26}{:>8.0}", ms(d));
        }
        println!("  {:<26}{:>8.0}", "total", ms(tm.total()));
    }
    Ok(())
}
