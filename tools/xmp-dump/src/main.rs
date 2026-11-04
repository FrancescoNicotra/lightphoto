//! Inspect Lightroom / Camera Raw presets.
//!
//! usage: xmp-dump <preset.xmp|photo.dng|photo.jpg> [--raw] [--cube out.cube] [--sidecar out.xmp]

use std::process::ExitCode;

use pipeline::colour::table_to_lut;
use pipeline::Pipeline;
use xmp::{DevelopSettings, Preset, Value, NS_CRS};

struct Args {
    input: String,
    raw: bool,
    cube: Option<String>,
    sidecar: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut args = Args { input: String::new(), raw: false, cube: None, sidecar: None };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--raw" => args.raw = true,
            "--cube" => args.cube = Some(it.next().ok_or("--cube needs a path")?),
            "--sidecar" => args.sidecar = Some(it.next().ok_or("--sidecar needs a path")?),
            "-h" | "--help" => return Err(String::new()),
            _ if args.input.is_empty() => args.input = a,
            _ => return Err(format!("unexpected argument {a:?}")),
        }
    }
    if args.input.is_empty() {
        return Err("missing input file".into());
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
            eprintln!("usage: xmp-dump <preset.xmp|photo.dng|photo.jpg> [--raw] [--cube out.cube] [--sidecar out.xmp]");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let preset = Preset::load(&args.input)?;
    let s = &preset.settings;

    println!("File     {}", args.input);
    println!("Name     {}", preset.name.as_deref().unwrap_or("-"));
    println!("Type     {}", preset.preset_type.as_deref().unwrap_or("-"));
    println!("UUID     {}", preset.uuid.as_deref().unwrap_or("-"));
    println!("Process  {}", s.process_version.as_deref().unwrap_or("-"));

    for (panel, rows) in s.panels() {
        if rows.is_empty() {
            continue;
        }
        println!("\n[{panel}]");
        for (k, v) in rows {
            println!("  {k:<32} {v}");
        }
    }

    let unknown: Vec<_> = preset
        .packet
        .props
        .iter()
        .filter(|p| p.ns == NS_CRS && !DevelopSettings::is_known_key(&p.name))
        .collect();
    if !unknown.is_empty() {
        println!("\n[Not mapped yet]");
        for p in unknown {
            println!("  {:<32} {}", p.name, short(&p.value));
        }
    }

    if args.raw {
        println!("\n[All properties]");
        for p in preset.packet.props.iter() {
            println!("  {:<40} {}", p.name, short(&p.value));
        }
    }

    if let Some(out) = &args.cube {
        let table = s.look.as_ref().and_then(|l| l.rgb_table.as_ref()).or(s.rgb_table.as_ref());
        if let Some(table) = table {
            let title = format!(
                "{} ({:?} primaries, {:?} gamma)",
                preset.name.as_deref().unwrap_or("RGB table"),
                table.primaries,
                table.gamma
            );
            table_to_lut(table).write_cube(out, Some(&title))?;
            println!("\nwrote {out}: {}^3, values are in the table's own space ({title})", table.divisions);
        } else {
            // A parametric preset: sample the point-wise part of the pipeline.
            let title = format!("{} (point-wise part only)", preset.name.as_deref().unwrap_or("preset"));
            Pipeline::to_cube(s, 33).write_cube(out, Some(&title))?;
            println!("\nwrote {out}: 33^3, sRGB in / sRGB out; white balance and spatial effects are not included");
        }
    }

    if let Some(out) = &args.sidecar {
        std::fs::write(out, preset.packet.to_xml())?;
        println!("\nwrote {out}");
    }
    Ok(())
}

fn short(v: &Value) -> String {
    let s = match v {
        Value::Text(t) => t.clone(),
        Value::Seq(items) | Value::Bag(items) => {
            format!("[{}]", items.iter().map(short).collect::<Vec<_>>().join("; "))
        }
        Value::Alt(_) => v.as_text().unwrap_or("").to_string(),
        Value::Struct(p) => format!("{{{} fields}}", p.len()),
    };
    if s.chars().count() > 80 {
        format!("{}…", s.chars().take(80).collect::<String>())
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmp::RgbTable;

    #[test]
    fn identity_table_becomes_identity_cube() {
        let lut = table_to_lut(&RgbTable::identity(9));
        for p in [[0.1, 0.5, 0.9], [1.0, 0.0, 0.25]] {
            let out = lut.apply(p);
            assert!(out.iter().zip(p).all(|(a, b)| (a - b).abs() < 1e-4), "{p:?} -> {out:?}");
        }
    }
}
