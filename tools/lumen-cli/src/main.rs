//! Develop RAW files from the command line.
//!
//! usage:
//!   lumen-cli info    <photo>
//!   lumen-cli develop <photo> -o out.jpg [--size N] [--quality Q] [--half] [--exposure EV] [--temp K [--tint T]]
//!   lumen-cli preview <photo> -o out.jpg

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use raw::{DecodeOptions, Image, Photo, TempTint};

const USAGE: &str = "usage:
  lumen-cli info    <photo>
  lumen-cli develop <photo> -o out.jpg [--size N] [--quality Q] [--half] [--exposure EV] [--temp K [--tint T]]
  lumen-cli preview <photo> -o out.jpg      (extracts the preview embedded in a RAW file)

  out: .jpg (8 bit), .png / .tif (16 bit)";

enum Command {
    Info,
    Develop,
    Preview,
}

struct Args {
    command: Command,
    input: PathBuf,
    output: Option<PathBuf>,
    size: Option<usize>,
    quality: u8,
    half: bool,
    exposure: f64,
    temperature: Option<f64>,
    tint: f64,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let command = match it.next().as_deref() {
        Some("info") => Command::Info,
        Some("develop") => Command::Develop,
        Some("preview") => Command::Preview,
        Some("-h" | "--help") | None => return Err(String::new()),
        Some(c) => return Err(format!("unknown command {c:?}")),
    };
    let mut args = Args {
        command,
        input: PathBuf::new(),
        output: None,
        size: None,
        quality: 92,
        half: false,
        exposure: 0.0,
        temperature: None,
        tint: 0.0,
    };
    fn num<T: std::str::FromStr>(flag: &str, v: Option<String>) -> Result<T, String> {
        v.as_deref().and_then(|s| s.parse().ok()).ok_or(format!("{flag} needs a number"))
    }
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" | "--output" => args.output = Some(it.next().ok_or("-o needs a path")?.into()),
            "--size" => args.size = Some(num("--size", it.next())?),
            "--quality" => args.quality = num::<u8>("--quality", it.next())?.min(100),
            "--half" => args.half = true,
            "--exposure" => args.exposure = num("--exposure", it.next())?,
            "--temp" => args.temperature = Some(num("--temp", it.next())?),
            "--tint" => args.tint = num("--tint", it.next())?,
            "-h" | "--help" => return Err(String::new()),
            _ if args.input.as_os_str().is_empty() => args.input = a.into(),
            _ => return Err(format!("unexpected argument {a:?}")),
        }
    }
    if args.input.as_os_str().is_empty() {
        return Err("missing input file".into());
    }
    if !matches!(args.command, Command::Info) && args.output.is_none() {
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
        Command::Info => info(&args),
        Command::Develop => develop(&args),
        Command::Preview => preview(&args),
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

fn info(args: &Args) -> CliResult<()> {
    let t = Instant::now();
    let photo = Photo::load(&args.input, &DecodeOptions::default())?;
    let decode_ms = t.elapsed().as_millis();
    let i = &photo.info;

    println!("File         {}", args.input.display());
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

    if !raw::is_rendered_image(&args.input) {
        if let Ok(Some(p)) = raw::embedded_preview(&args.input) {
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

fn develop(args: &Args) -> CliResult<()> {
    let out = args.output.as_ref().unwrap();
    let t = Instant::now();
    let photo = Photo::load(&args.input, &DecodeOptions { half_size: args.half })?;
    let t_decode = t.elapsed();

    let white = args.temperature.map(|temperature| TempTint { temperature, tint: args.tint }.to_xy());
    let mut img = photo.to_working(white);
    if let Some(size) = args.size {
        img = img.downscale(size);
    }
    let t_develop = t.elapsed() - t_decode;

    let exposure = photo.info.baseline_exposure + args.exposure;
    save(&img, exposure, out, args.quality)?;
    println!(
        "wrote {} ({} × {}) — decode {} ms, develop {} ms, total {} ms",
        out.display(),
        img.width,
        img.height,
        t_decode.as_millis(),
        t_develop.as_millis(),
        t.elapsed().as_millis()
    );
    Ok(())
}

fn preview(args: &Args) -> CliResult<()> {
    let out = args.output.as_ref().unwrap();
    let img = raw::embedded_preview(&args.input)?.ok_or("the file has no embedded preview")?;
    img.to_rgb8().save(out)?;
    println!("wrote {} ({} × {})", out.display(), img.width(), img.height());
    Ok(())
}

fn save(img: &Image, exposure: f64, path: &Path, quality: u8) -> CliResult<()> {
    let (w, h) = (img.width as u32, img.height as u32);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => {
            let buf = raw::encode_srgb8(img, exposure);
            let file = std::io::BufWriter::new(std::fs::File::create(path)?);
            image::codecs::jpeg::JpegEncoder::new_with_quality(file, quality).encode(
                &buf,
                w,
                h,
                image::ExtendedColorType::Rgb8,
            )?;
        }
        "png" | "tif" | "tiff" => {
            let buf = raw::encode_srgb16(img, exposure);
            image::ImageBuffer::<image::Rgb<u16>, _>::from_raw(w, h, buf).ok_or("buffer size mismatch")?.save(path)?;
        }
        _ => return Err(format!("unsupported output format {ext:?} (use .jpg, .png or .tif)").into()),
    }
    Ok(())
}
