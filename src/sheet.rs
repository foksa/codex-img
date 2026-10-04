//! `codex-img sheet`: lay images out in one labelled grid (a contact sheet), to review a batch of
//! sprites at a glance. Sprites stand on a common baseline, so one that floats shows a gap.
use crate::cli;
use crate::convert;
use crate::error::{Error, Result};
use crate::images::{self, Encoding, Format};
#[cfg(test)]
use image::{Rgba, RgbaImage};
use serde_json::json;
use std::path::Path;

pub use codex_img_core::sheet::{SheetOptions, Labels};
use codex_img_core::sheet::{DEFAULT_CELL, MAX_CELL, DEFAULT_BG, compose, label_for};
pub fn help() -> &'static str {
    r#"Usage:
  codex-img sheet <input>... -o <sheet.png> [options]

Lays PNG, JPEG or WebP images out in one grid on a solid background, each labelled
with its file name, to review a batch at a glance (no login, no quota). Sprites stand
on a line at the bottom of their cell, so one with empty rows under it floats.

Options:
  -o, --output <file>       The sheet (required); png, jpeg or webp by extension
      --cols <n>            Columns (default: a roughly square grid)
      --cell <px>           Cell size (default 240)
      --bg <colour>         Background, #rrggbb (default #96be96, a muted green)
      --labels <what>       name (default) | path (as given) | none
      --center              Centre sprites in their cells instead
      --same-scale          Shrink every sprite by the same factor, so sizes stay
                            comparable (default: each shrinks to fit its cell).
                            Sprites are never enlarged
      --force               Replace an existing sheet
      --json                Print a JSON object describing the sheet to stdout
      --quiet               No progress on stderr

Examples:
  codex-img sheet public/assets/tracks/harbor/*.png -o harbor-sheet.png
  codex-img sheet sprites/*.png -o sheet.png --same-scale --cols 8 --force"#
}

pub fn parse(args: &[String]) -> Result<Option<SheetOptions>> {
    let mut inputs = Vec::new();
    let (mut output, mut cols, mut cell, mut bg, mut labels) = (None, None, DEFAULT_CELL, DEFAULT_BG, Labels::Name);
    let (mut center, mut same_scale, mut force, mut json, mut quiet) = (false, false, false, false, false);
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            inputs.extend(iter.by_ref().cloned());
            break;
        }
        if !arg.starts_with('-') {
            inputs.push(arg.clone());
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if arg.starts_with("--") => (n, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| iter.next().cloned()).ok_or_else(|| Error::usage(format!("{name} needs a value.")));
        match name {
            "-o" | "--output" => output = Some(value()?),
            "--cols" => {
                cols = Some(value()?.parse::<u32>().ok().filter(|n| (1..=256).contains(n)).ok_or_else(|| Error::usage("--cols must be an integer from 1 to 256."))?)
            }
            "--cell" => {
                cell = value()?
                    .parse::<u32>()
                    .ok()
                    .filter(|n| (32..=MAX_CELL).contains(n))
                    .ok_or_else(|| Error::usage(format!("--cell must be an integer from 32 to {MAX_CELL}.")))?
            }
            "--bg" => bg = parse_colour(&value()?)?,
            "--labels" => {
                labels = match value()?.as_str() {
                    "name" => Labels::Name,
                    "path" => Labels::Path,
                    "none" => Labels::None,
                    _ => return Err(Error::usage("--labels must be one of: name, path, none")),
                }
            }
            "--center" => center = true,
            "--same-scale" => same_scale = true,
            "--force" => force = true,
            "--json" => json = true,
            "--quiet" => quiet = true,
            "-h" | "--help" => return Ok(None),
            _ => return Err(Error::usage(format!("Unknown sheet option: {arg}"))),
        }
    }
    if inputs.is_empty() {
        return Err(Error::usage("sheet needs at least one input file."));
    }
    let output = output.ok_or_else(|| Error::usage("sheet needs -o <file>, e.g. -o sheet.png."))?;
    let ext = Path::new(&output).extension().and_then(|e| e.to_str()).unwrap_or_default();
    let format = Format::parse(ext).ok_or_else(|| Error::usage("The sheet's -o must end in .png, .jpg or .webp."))?;
    Ok(Some(SheetOptions { inputs, output, format, cols, cell, bg, labels, center, same_scale, force, json, quiet }))
}

/// `#rrggbb` or `rrggbb`.
fn parse_colour(value: &str) -> Result<[u8; 3]> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    let channel = |i: usize| hex.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok());
    match (hex.len(), channel(0), channel(2), channel(4)) {
        (6, Some(r), Some(g), Some(b)) => Ok([r, g, b]),
        _ => Err(Error::usage("--bg must be a colour like #96be96.")),
    }
}

/// Runs `sheet`. An input that can't be read is reported and left out; the rest still make a sheet.
pub fn run(opts: &SheetOptions) -> Result<i32> {
    let mut exit_code = 0;
    let mut sprites = Vec::new();
    for input in &opts.inputs {
        match convert::read_image(Path::new(input)).and_then(|(bytes, _)| images::decode(&bytes).map_err(Into::into)) {
            Ok(rgba) => sprites.push((label_for(input, opts.labels), rgba)),
            Err(error) => {
                eprintln!("codex-img: {input}: {error}");
                exit_code = error.kind.exit_code();
            }
        }
    }
    if sprites.is_empty() {
        return Err(Error::other("No input could be read; nothing to lay out."));
    }
    let target = Path::new(&opts.output);
    if !opts.force && target.exists() {
        return Err(Error::other(format!("{} already exists; add --force to replace it.", target.display())));
    }
    let (sheet, cols, rows) = compose(&sprites, opts)?;
    let encoding = Encoding::default();
    let mut bytes = images::encode(&sheet, opts.format, &encoding)?;
    if opts.format == Format::Png {
        bytes = images::optimize_png(&bytes).unwrap_or(bytes);
    }
    // Sheets belong in the feed, but must never count as subscription generations.
    let run = crate::events::Run::start("sheet", 1, json!({}));
    let job = run.job(json!({"prompt":"Contact sheet","inputs":opts.inputs.iter().map(|path|
        json!({"path":crate::events::absolute(Path::new(path)),"role":"input"})).collect::<Vec<_>>(),"request":{},"output":crate::events::absolute(target)}));
    let written = match cli::write_output(target, &bytes, opts.force) {
        Ok(written) => written,
        Err(error) => { job.failed(&error); run.end(Some(&error)); return Err(error); }
    };
    let (width, height) = sheet.dimensions();
    job.done(json!({"path":crate::events::absolute(target),"size":format!("{width}x{height}"),"durationMs":0}));
    run.end((exit_code != 0).then(|| Error::other("Some input images could not be read.")).as_ref());
    if opts.json {
        let mut info = json!({
            "path": target.display().to_string(),
            "format": opts.format.name(),
            "bytes": bytes.len(),
            "size": format!("{width}x{height}"),
            "images": sprites.len(),
            "cols": cols,
            "rows": rows,
        });
        if !written {
            info["unchanged"] = json!(true);
        }
        println!("{info}");
    } else {
        println!("{}", target.display());
    }
    if !opts.quiet {
        eprintln!("{} images in {cols}x{rows} cells -> {} ({width}x{height})", sprites.len(), target.display());
    }
    Ok(exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn usage_error(list: &[&str]) -> String {
        parse(&args(list)).unwrap_err().message
    }

    #[test]
    fn parses_and_validates_options() {
        let o = parse(&args(&["a.png", "b.png", "-o", "s.webp", "--cols=3", "--bg", "102030", "--labels", "none", "--same-scale"])).unwrap().unwrap();
        assert_eq!((o.inputs.len(), o.format, o.cols, o.bg, o.labels, o.same_scale), (2, Format::Webp, Some(3), [0x10, 0x20, 0x30], Labels::None, true));
        assert_eq!(parse(&args(&["-h"])).unwrap(), None);
        assert!(usage_error(&["-o", "s.png"]).contains("at least one input"));
        assert!(usage_error(&["a.png"]).contains("-o"));
        assert!(usage_error(&["a.png", "-o", "s.gif"]).contains(".png"));
        assert!(usage_error(&["a.png", "-o", "s.png", "--cols", "0"]).contains("--cols"));
        assert!(usage_error(&["a.png", "-o", "s.png", "--cell", "8"]).contains("--cell"));
        assert!(usage_error(&["a.png", "-o", "s.png", "--bg", "green"]).contains("--bg"));
        assert!(usage_error(&["a.png", "-o", "s.png", "--labels", "all"]).contains("--labels"));
        assert!(usage_error(&["a.png", "-o", "s.png", "-c", "8"]).contains("Unknown sheet option"));
    }

    #[test]
    fn writes_the_sheet_and_respects_force() {
        let dir = crate::auth::tests::temp_dir("sheet");
        let sprite = dir.join("car.png");
        let mut png = std::io::Cursor::new(Vec::new());
        RgbaImage::from_pixel(30, 12, Rgba([200, 30, 30, 255])).write_to(&mut png, image::ImageFormat::Png).unwrap();
        std::fs::write(&sprite, png.into_inner()).unwrap();
        let out = dir.join("sheet.png");
        let (sprite, out) = (sprite.display().to_string(), out.display().to_string());
        let opts = parse(&args(&[&sprite, "missing.png", "-o", &out, "--quiet"])).unwrap().unwrap();
        assert_eq!(run(&opts).unwrap(), 1, "a missing input is reported, the rest still laid out");
        let decoded = image::open(&out).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (240, 240 + 20));
        assert!(run(&opts).unwrap_err().message.contains("--force"));
        let forced = parse(&args(&[&sprite, "-o", &out, "--quiet", "--force"])).unwrap().unwrap();
        assert_eq!(run(&forced).unwrap(), 0);
    }
}
