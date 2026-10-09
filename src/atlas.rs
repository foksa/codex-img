//! `codex-img atlas`: pack many small images into texture atlas pages, each with a TexturePacker
//! "hash" JSON (PixiJS, Phaser). Local and deterministic: the same inputs give the same bytes.
use crate::cli;
use crate::convert;
use crate::error::{Error, Result};
use crate::images::{self, Encoding, Format};
use codex_img_core::atlas::{self, AtlasOptions as Packing, Sprite, MAX_PAGE_SIDE};
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtlasOptions {
    pub inputs: Vec<String>,
    pub output: String,
    pub format: Format,
    pub encoding: Encoding,
    pub packing: Packing,
    pub prefix: String,
    pub no_bleed: bool,
    pub force: bool,
    pub json: bool,
    pub quiet: bool,
}

pub fn help() -> &'static str {
    r#"Usage:
  codex-img atlas <input or dir>... -o <atlas.webp|.png> [options]

Packs PNG, JPEG, WebP or GIF images into texture atlas pages, each with a JSON file in
TexturePacker's "hash" format, which PixiJS, Phaser and most engines load (no login,
no quota). Images in a directory are found recursively and named by their path
under it without the extension (Germans/infantry); a file given directly is named
by its file name without the extension. Other files are skipped with a warning,
but an image in a format codex-img can't read (BMP, TIFF, …) fails the atlas.

Output: atlas.webp + atlas.json, then atlas-1.webp + atlas-1.json and so on when
one page isn't enough. The first JSON lists the others in meta.related_multi_packs,
so PixiJS's Assets.load of atlas.json loads every page.

Options:
  -o, --output <file>       First page; png or webp by extension (required)
      --prefix <text>       Put this in front of every frame name, e.g. units/ for
                            units/Germans/infantry; keeps names from different
                            atlases apart in a global texture cache
      --trim                Pack only the pixels with any alpha. The JSON keeps the
                            original size and offset, so sprites stay aligned
      --padding <px>        Transparent space between sprites and at the page
                            edges (default 2)
      --extrude <px>        Repeat each sprite's edge pixels this far outward,
                            outside its frame, so filtering at the edges doesn't
                            pick up neighbours (default 0; 1 is usually enough)
      --max-size <px>       Largest page side (default 2048)
      --pot                 Power-of-two page sides (needs a power-of-two
                            --max-size)
      --lossless            Lossless webp (recommended for sprites)
      --output-quality <n>  Lossy webp quality, 1-100 (default 80)
  -c, --colors <n>          Quantize PNG pages to n colours (2-256)
      --dither              Dither when quantizing
      --no-bleed            Leave fully transparent pixels black. By default PNG
                            and lossless webp pages get the nearest visible colour
                            there, so texture filtering can't pull in a dark halo
      --force               Replace existing pages and JSON files
      --json                Print a JSON report to stdout
      --quiet               No progress on stderr

Pages are never rotated. Frames are sorted by name, and the same inputs and options
always give the same bytes.

Examples:
  codex-img atlas maps/units/ -o web/units.webp --prefix units/ --lossless --trim --extrude 1
  codex-img atlas flags/ -o web/flags.png --max-size 1024 --pot --force"#
}

fn number(name: &str, value: &str, range: std::ops::RangeInclusive<u32>) -> Result<u32> {
    value.parse::<u32>().ok().filter(|n| range.contains(n)).ok_or_else(|| Error::usage(format!("{name} must be an integer from {} to {}.", range.start(), range.end())))
}

pub fn parse(args: &[String]) -> Result<Option<AtlasOptions>> {
    let mut inputs = Vec::new();
    let mut output = None;
    let mut encoding = Encoding::default();
    let mut packing = Packing::default();
    let mut prefix = String::new();
    let (mut no_bleed, mut force, mut json, mut quiet) = (false, false, false, false);
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
            "--prefix" => prefix = value()?,
            "--trim" if inline.is_none() => packing.trim = true,
            "--padding" => packing.padding = number(name, &value()?, 0..=64)?,
            "--extrude" => packing.extrude = number(name, &value()?, 0..=16)?,
            "--max-size" => packing.max_size = number(name, &value()?, 16..=MAX_PAGE_SIDE)?,
            "--pot" if inline.is_none() => packing.pot = true,
            "--lossless" if inline.is_none() => encoding.lossless = true,
            "--output-quality" => encoding.quality = Some(cli::parse_output_quality(&value()?)?),
            "-c" | "--colors" => encoding.colors = Some(cli::parse_colors(&value()?)?),
            "--dither" if inline.is_none() => encoding.dither = true,
            "--no-bleed" if inline.is_none() => no_bleed = true,
            "--force" if inline.is_none() => force = true,
            "--json" if inline.is_none() => json = true,
            "--quiet" if inline.is_none() => quiet = true,
            "-h" | "--help" => return Ok(None),
            _ => return Err(Error::usage(format!("Unknown atlas option: {arg}"))),
        }
    }
    if inputs.is_empty() {
        return Err(Error::usage("atlas needs at least one input file or directory."));
    }
    let output = output.ok_or_else(|| Error::usage("atlas needs -o <atlas.webp|atlas.png>."))?;
    let ext = Path::new(&output).extension().and_then(|e| e.to_str()).unwrap_or_default();
    let format = match Format::parse(ext) {
        Some(format @ (Format::Png | Format::Webp)) => format,
        _ => return Err(Error::usage("-o must end in .png or .webp; atlas pages need transparency.")),
    };
    if packing.pot && !packing.max_size.is_power_of_two() {
        return Err(Error::usage("--pot needs a power-of-two --max-size, such as 1024 or 2048."));
    }
    encoding.check(Some(format))?;
    Ok(Some(AtlasOptions { inputs, output, format, encoding, packing, prefix, no_bleed, force, json, quiet }))
}

/// Page `index` of the atlas and its JSON: atlas.webp/atlas.json, then atlas-1.webp/atlas-1.json.
fn page_paths(output: &Path, index: usize) -> (PathBuf, PathBuf) {
    let stem = output.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = output.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = if index == 0 { stem } else { format!("{stem}-{index}") };
    (output.with_file_name(format!("{stem}.{ext}")), output.with_file_name(format!("{stem}.json")))
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Every input image with its frame name, in a fixed order.
fn collect(opts: &AtlasOptions) -> Result<(Vec<(String, PathBuf)>, u64)> {
    let mut named = Vec::new();
    for input in &opts.inputs {
        let root = Path::new(input);
        if root.is_dir() {
            let (mut files, mut others) = (Vec::new(), Vec::new());
            convert::walk(root, &mut files, &mut others)?;
            if let Some(image) = others.iter().find(|p| convert::unreadable_image(p)) {
                return Err(Error::usage(format!("{} is in a format codex-img can't read (PNG, JPEG, WebP and GIF are); convert it or move it out.", image.display())));
            }
            if !opts.quiet {
                for other in &others {
                    eprintln!("codex-img: skipping {}: not an image", other.display());
                }
            }
            if files.is_empty() {
                return Err(Error::usage(format!("{input} has no PNG, JPEG, WebP or GIF files.")));
            }
            for file in files {
                let relative = file.strip_prefix(root).unwrap_or(&file).with_extension("");
                let parts: Vec<String> = relative.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
                named.push((format!("{}{}", opts.prefix, parts.join("/")), file));
            }
        } else {
            let stem = root.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            named.push((format!("{}{stem}", opts.prefix), root.to_path_buf()));
        }
    }
    let mut input_bytes = 0;
    for (_, path) in &named {
        input_bytes += std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    }
    Ok((named, input_bytes))
}

/// Runs `atlas`. Any unreadable input fails the whole atlas: a build must not quietly lose frames.
pub fn run(opts: &AtlasOptions) -> Result<i32> {
    let (named, input_bytes) = collect(opts)?;
    let mut sprites = Vec::with_capacity(named.len());
    for (name, path) in &named {
        let (bytes, _) = convert::read_image(path)?;
        let image = images::decode(&bytes).map_err(|e| Error::other(format!("{}: {}", path.display(), e.message)))?;
        sprites.push(Sprite { name: name.clone(), image });
    }
    let mut pages = atlas::pack(sprites, &opts.packing)?;

    let output = Path::new(&opts.output);
    let paths: Vec<(PathBuf, PathBuf)> = (0..pages.len()).map(|i| page_paths(output, i)).collect();
    if !opts.force {
        if let Some(existing) = paths.iter().flat_map(|(image, json)| [image, json]).find(|p| p.exists()) {
            return Err(Error::other(format!("{} already exists; add --force to replace it.", existing.display())));
        }
    }
    let bleed = !opts.no_bleed && codex_img_core::transform::bleeds(opts.format, &opts.encoding);
    let related: Vec<String> = paths.iter().skip(1).map(|(_, json)| file_name(json)).collect();
    let mut reports = Vec::new();
    let mut total_bytes = 0;
    for (i, (page, (image_path, json_path))) in pages.iter_mut().zip(&paths).enumerate() {
        if bleed {
            codex_img_core::transform::bleed(&mut page.image);
        }
        let mut bytes = images::encode(&page.image, opts.format, &opts.encoding)?;
        if opts.format == Format::Png {
            bytes = images::optimize_png(&bytes).unwrap_or(bytes);
        }
        let data = atlas::page_json(page, &file_name(image_path), if i == 0 { &related } else { &[] });
        let mut text = serde_json::to_string_pretty(&data).map_err(|e| Error::other(e.to_string()))?;
        text.push('\n');
        let wrote_image = cli::write_output(image_path, &bytes, opts.force)?;
        let wrote_json = cli::write_output(json_path, text.as_bytes(), opts.force)?;
        total_bytes += bytes.len() as u64;
        let (w, h) = page.image.dimensions();
        let mut report = json!({"image": image_path.display().to_string(), "json": json_path.display().to_string(), "size": format!("{w}x{h}"), "frames": page.frames.len(), "bytes": bytes.len()});
        if !wrote_image && !wrote_json {
            report["unchanged"] = json!(true);
        }
        reports.push(report);
    }
    // A run that needs fewer pages than the last one leaves its extra pages behind.
    let stale = page_paths(output, pages.len());
    if !opts.quiet && (stale.0.exists() || stale.1.exists()) {
        eprintln!("codex-img: warning: {} is left from an earlier run with more pages; delete it and the pages after it.", stale.1.display());
    }
    if opts.json {
        println!("{}", json!({"pages": reports, "frames": named.len(), "format": opts.format.name(), "inputBytes": input_bytes, "bytes": total_bytes}));
    } else {
        for (image, json) in &paths {
            println!("{}\n{}", image.display(), json.display());
        }
    }
    if !opts.quiet {
        eprintln!("{} images -> {} page(s), {} KB -> {} KB", named.len(), pages.len(), input_bytes / 1024, total_bytes / 1024);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_img_core::atlas::{DEFAULT_MAX_SIZE, DEFAULT_PADDING};
    use image::{Rgba, RgbaImage};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_and_validates_options() {
        let o = parse(&args(&["units/", "-o", "u.webp", "--trim", "--padding=1", "--extrude", "1", "--max-size", "1024", "--pot", "--lossless", "--prefix", "units/"])).unwrap().unwrap();
        assert_eq!((o.format, o.packing, o.prefix.as_str(), o.encoding.lossless), (Format::Webp, Packing { trim: true, padding: 1, extrude: 1, max_size: 1024, pot: true }, "units/", true));
        let o = parse(&args(&["a.png", "-o", "a.png"])).unwrap().unwrap();
        assert_eq!((o.packing.padding, o.packing.max_size), (DEFAULT_PADDING, DEFAULT_MAX_SIZE));
        assert_eq!(parse(&args(&["-h"])).unwrap(), None);
        let error = |list: &[&str]| parse(&args(list)).unwrap_err().message;
        assert!(error(&["a.png"]).contains("-o"));
        assert!(error(&["-o", "a.png"]).contains("at least one input"));
        assert!(error(&["a.png", "-o", "a.jpg"]).contains(".png or .webp"));
        assert!(error(&["a.png", "-o", "a.png", "--pot", "--max-size", "1000"]).contains("power-of-two"));
        assert!(error(&["a.png", "-o", "a.webp", "-c", "16"]).contains("PNG"));
        assert!(error(&["a.png", "-o", "a.png", "--extrude", "99"]).contains("--extrude"));
    }

    #[test]
    fn writes_pages_and_json_named_by_relative_path() {
        let dir = crate::auth::tests::temp_dir("atlas-run");
        let source = dir.join("units");
        for (name, colour) in [("Germans/infantry.png", [200, 0, 0, 255]), ("Germans/tank.png", [0, 200, 0, 255]), ("Russians/infantry.png", [0, 0, 200, 255])] {
            std::fs::create_dir_all(source.join(name).parent().unwrap()).unwrap();
            let mut image = RgbaImage::new(40, 40);
            for y in 10..30 { for x in 5..25 { image.put_pixel(x, y, Rgba(colour)); } }
            image.save(source.join(name)).unwrap();
        }
        let out = dir.join("web/units.png");
        let list = [source.display().to_string(), "-o".into(), out.display().to_string(), "--prefix".into(), "units/".into(), "--trim".into(), "--extrude=1".into(), "--max-size=48".into(), "--quiet".into()];
        let opts = parse(&list).unwrap().unwrap();
        assert_eq!(run(&opts).unwrap(), 0);
        let read = |name: &str| -> serde_json::Value { serde_json::from_slice(&std::fs::read(dir.join("web").join(name)).unwrap()).unwrap() };
        let pages = [read("units.json"), read("units-1.json"), read("units-2.json")];
        let first = &pages[0];
        assert_eq!(first["meta"]["image"], "units.png");
        assert_eq!(first["meta"]["related_multi_packs"], json!(["units-1.json", "units-2.json"]));
        assert!(pages[1]["meta"].get("related_multi_packs").is_none());
        let mut names: Vec<String> = pages.iter().flat_map(|j| j["frames"].as_object().unwrap().keys().cloned().collect::<Vec<_>>()).collect();
        names.sort();
        assert_eq!(names, ["units/Germans/infantry", "units/Germans/tank", "units/Russians/infantry"]);
        let frame = first["frames"].as_object().unwrap().values().next().unwrap();
        assert_eq!(frame["spriteSourceSize"], json!({"x": 5, "y": 10, "w": 20, "h": 20}));
        assert_eq!(frame["sourceSize"], json!({"w": 40, "h": 40}));

        // An image codex-img can't read fails the atlas instead of going missing.
        std::fs::write(source.join("Germans/fighter.bmp"), b"BM").unwrap();
        assert!(run(&AtlasOptions { force: true, ..opts.clone() }).unwrap_err().message.contains("fighter.bmp"));
        std::fs::remove_file(source.join("Germans/fighter.bmp")).unwrap();

        let before = std::fs::read(&out).unwrap();
        assert!(run(&opts).unwrap_err().message.contains("--force"));
        assert_eq!(run(&AtlasOptions { force: true, ..opts.clone() }).unwrap(), 0);
        assert_eq!(std::fs::read(&out).unwrap(), before, "same inputs, same bytes");
    }
}
