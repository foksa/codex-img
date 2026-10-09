//! `codex-img pyramid`: overview levels for a tiled map (x_y tiles, as TripleA writes them). Each
//! level halves the map and is cut on the same grid; a manifest lists every level and tile.
use crate::cli;
use crate::convert;
use crate::error::{Error, Result};
use crate::images::{self, Encoding, Format};
use codex_img_core::pyramid;
use image::{imageops, RgbaImage};
use serde_json::{json, Map};
use std::path::{Path, PathBuf};

pub const DEFAULT_TILE: u32 = 256;
const MAX_MAP_SIDE: u32 = 65_536;
const MAX_MAP_PIXELS: u64 = 400_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyramidOptions {
    pub input: String,
    pub output: String,
    /// The map's size; pixels past it in the edge tiles are padding. Default: the tiles' extent.
    pub map_size: Option<(u32, u32)>,
    pub tile: u32,
    /// Output tile size, a multiple of `tile`. Default: `tile`.
    pub merge: Option<u32>,
    pub levels: Option<u32>,
    pub format: Format,
    pub encoding: Encoding,
    pub no_bleed: bool,
    pub force: bool,
    pub json: bool,
    pub quiet: bool,
}

pub fn help() -> &'static str {
    r#"Usage:
  codex-img pyramid <tile dir> -o <out dir>/ [options]

Builds zoomed-out levels of a tiled map (no login, no quota). The input holds tiles
named x_y.png (x the column, y the row, as TripleA's baseTiles and reliefTiles),
in PNG, JPEG or WebP. Level 0 is the full map, level 1 half its size, and so on,
until a level fits in one tile. Each level is resampled from the full map and cut
on the same x_y grid; tiles at the right and bottom edges are cropped to the map.

Output: <out>/0/x_y.webp, <out>/1/x_y.webp, …, and <out>/pyramid.json:
  {"mapWidth", "mapHeight", "tileSize", "format",
   "levels": [{"level", "width", "height", "cols", "rows", "tiles": {"x_y": "0/x_y.webp"}}]}
Tiles with no visible pixel (empty relief) aren't written and aren't listed.

Options:
  -o, --output <dir>        Output directory (required)
      --map-size <WxH>      The map's size, e.g. 3500x2000 (map.width x map.height
                            in TripleA's map.properties); pixels past it are padding.
                            Default: the extent of the tiles
      --tile <px>           Input tile size (default 256)
      --merge <px>          Output tile size, a multiple of --tile (512, 1024):
                            fewer, bigger tiles
      --levels <n>          At most n levels, level 0 included
  -f, --format <fmt>        webp (default) or png
      --lossless            Lossless webp
      --output-quality <n>  Lossy webp quality, 1-100 (default 80)
  -c, --colors <n>          Quantize PNG tiles to n colours (2-256)
      --dither              Dither when quantizing
      --no-bleed            Leave fully transparent pixels as they are (see convert)
      --force               Replace existing files; identical ones are left untouched
      --json                Print a JSON report to stdout
      --quiet               No progress on stderr

The same tiles and options always give the same bytes.

Examples:
  codex-img pyramid map/baseTiles -o web/base/ --map-size 3500x2000 --merge 512
  codex-img pyramid map/reliefTiles -o web/relief/ --map-size 3500x2000 --output-quality 85"#
}

fn size_value(value: &str) -> Result<(u32, u32)> {
    let invalid = || Error::usage("--map-size must look like 3500x2000.");
    let (w, h) = value.split_once('x').ok_or_else(invalid)?;
    let side = |s: &str| s.parse::<u32>().ok().filter(|n| (1..=MAX_MAP_SIDE).contains(n)).ok_or_else(invalid);
    Ok((side(w)?, side(h)?))
}

fn number(name: &str, value: &str, range: std::ops::RangeInclusive<u32>) -> Result<u32> {
    value.parse::<u32>().ok().filter(|n| range.contains(n)).ok_or_else(|| Error::usage(format!("{name} must be an integer from {} to {}.", range.start(), range.end())))
}

pub fn parse(args: &[String]) -> Result<Option<PyramidOptions>> {
    let mut inputs = Vec::new();
    let (mut output, mut map_size, mut tile, mut merge, mut levels) = (None, None, DEFAULT_TILE, None, None);
    let (mut format, mut encoding) = (Format::Webp, Encoding::default());
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
            "--map-size" => map_size = Some(size_value(&value()?)?),
            "--tile" => tile = number(name, &value()?, 16..=4096)?,
            "--merge" => merge = Some(number(name, &value()?, 16..=8192)?),
            "--levels" => levels = Some(number(name, &value()?, 1..=32)?),
            "-f" | "--format" => {
                format = match Format::parse(&value()?) {
                    Some(f @ (Format::Png | Format::Webp)) => f,
                    _ => return Err(Error::usage("--format must be webp or png; map tiles may need transparency.")),
                }
            }
            "--lossless" if inline.is_none() => encoding.lossless = true,
            "--output-quality" => encoding.quality = Some(cli::parse_output_quality(&value()?)?),
            "-c" | "--colors" => encoding.colors = Some(cli::parse_colors(&value()?)?),
            "--dither" if inline.is_none() => encoding.dither = true,
            "--no-bleed" if inline.is_none() => no_bleed = true,
            "--force" if inline.is_none() => force = true,
            "--json" if inline.is_none() => json = true,
            "--quiet" if inline.is_none() => quiet = true,
            "-h" | "--help" => return Ok(None),
            _ => return Err(Error::usage(format!("Unknown pyramid option: {arg}"))),
        }
    }
    let input = match inputs.as_slice() {
        [one] => one.clone(),
        [] => return Err(Error::usage("pyramid needs a tile directory.")),
        _ => return Err(Error::usage("pyramid takes one tile directory; run it once per layer.")),
    };
    let output = output.ok_or_else(|| Error::usage("pyramid needs -o <out dir>/."))?;
    if merge.is_some_and(|m| m % tile != 0) {
        return Err(Error::usage("--merge must be a multiple of --tile, such as 512 or 1024 for 256 px tiles."));
    }
    encoding.check(Some(format))?;
    Ok(Some(PyramidOptions { input, output, map_size, tile, merge, levels, format, encoding, no_bleed, force, json, quiet }))
}

/// `x_y.<png|jpg|jpeg|webp>` → (x, y).
fn tile_coords(path: &Path) -> Option<(u32, u32)> {
    let ext = path.extension()?.to_string_lossy();
    if Format::parse(&ext).is_none() && !ext.eq_ignore_ascii_case("gif") {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let (x, y) = stem.split_once('_')?;
    let digits = |s: &str| (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse::<u32>().ok()).flatten();
    Some((digits(x)?, digits(y)?))
}

/// The input tiles, sorted, and the full map assembled from them (cropped to `map_size`).
fn assemble(opts: &PyramidOptions) -> Result<(RgbaImage, usize, u64)> {
    let dir = Path::new(&opts.input);
    let fail = |e: std::io::Error| Error::other(format!("Unable to read {}: {e}", dir.display()));
    let mut tiles: Vec<((u32, u32), PathBuf)> = std::fs::read_dir(dir)
        .map_err(fail)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter_map(|path| tile_coords(&path).map(|c| (c, path)))
        .collect();
    tiles.sort();
    if let Some(pair) = tiles.windows(2).find(|pair| pair[0].0 == pair[1].0) {
        return Err(Error::usage(format!("{} and {} are the same tile.", pair[0].1.display(), pair[1].1.display())));
    }
    if tiles.is_empty() {
        return Err(Error::usage(format!("{} has no x_y tiles (such as 0_0.png).", dir.display())));
    }
    let t = opts.tile;
    let extent = |pick: fn(&(u32, u32)) -> u32| tiles.iter().map(|(c, _)| (pick(c) + 1) * t).max().unwrap_or(t);
    let (width, height) = opts.map_size.unwrap_or_else(|| (extent(|c| c.0), extent(|c| c.1)));
    if u64::from(width) * u64::from(height) > MAX_MAP_PIXELS {
        return Err(Error::usage(format!("A {width}x{height} map is more than {MAX_MAP_PIXELS} pixels; that's too big to assemble.")));
    }
    let mut map = RgbaImage::new(width, height);
    let mut input_bytes = 0;
    let mut used = 0;
    for ((x, y), path) in &tiles {
        let (left, top) = (x * t, y * t);
        if left >= width || top >= height {
            continue;
        }
        let (bytes, _) = convert::read_image(path)?;
        input_bytes += std::fs::metadata(path).map_or(bytes.len() as u64, |m| m.len());
        let tile = images::decode(&bytes).map_err(|e| Error::other(format!("{}: {}", path.display(), e.message)))?;
        if tile.width() > t || tile.height() > t {
            return Err(Error::usage(format!("{} is {}x{}, bigger than --tile {t}.", path.display(), tile.width(), tile.height())));
        }
        imageops::replace(&mut map, &tile, i64::from(left), i64::from(top));
        used += 1;
    }
    Ok((map, used, input_bytes))
}

struct Written {
    x: u32,
    y: u32,
    file: String,
    bytes: usize,
    changed: bool,
}

/// Encode and write one level's tiles, a few threads at a time.
fn write_level(image: &RgbaImage, level: u32, opts: &PyramidOptions) -> Result<Vec<Written>> {
    let tile = opts.merge.unwrap_or(opts.tile);
    let tiles: Vec<(u32, u32, RgbaImage)> = pyramid::cut(image, tile).into_iter().filter_map(|(x, y, t)| t.map(|t| (x, y, t))).collect();
    let bleed = !opts.no_bleed && codex_img_core::transform::bleeds(opts.format, &opts.encoding);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
    let chunk = tiles.len().div_ceil(threads).max(1);
    let results: Vec<Result<Vec<Written>>> = std::thread::scope(|scope| {
        let workers: Vec<_> = tiles
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .map(|(x, y, pixels)| {
                            let mut pixels = pixels.clone();
                            if bleed {
                                codex_img_core::transform::bleed(&mut pixels);
                            }
                            let mut bytes = images::encode(&pixels, opts.format, &opts.encoding)?;
                            if opts.format == Format::Png {
                                bytes = images::optimize_png(&bytes).unwrap_or(bytes);
                            }
                            let file = format!("{level}/{x}_{y}.{}", opts.format.extension());
                            let changed = cli::write_output(&Path::new(&opts.output).join(&file), &bytes, opts.force)?;
                            Ok(Written { x: *x, y: *y, file, bytes: bytes.len(), changed })
                        })
                        .collect::<Result<Vec<_>>>()
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().unwrap_or_else(|_| Err(Error::other("A tile encoder crashed.")))).collect()
    });
    let mut written = Vec::with_capacity(tiles.len());
    for part in results {
        written.extend(part?);
    }
    Ok(written)
}

pub fn run(opts: &PyramidOptions) -> Result<i32> {
    let (map, used, input_bytes) = assemble(opts)?;
    let (width, height) = map.dimensions();
    let tile = opts.merge.unwrap_or(opts.tile);
    let count = pyramid::level_count(width, height, tile, opts.levels);
    let out = Path::new(&opts.output);
    let manifest_path = out.join("pyramid.json");
    if !opts.force {
        if let Some(existing) = std::iter::once(manifest_path.clone()).chain((0..count).map(|l| out.join(l.to_string()))).find(|p| p.exists()) {
            return Err(Error::other(format!("{} already exists; add --force to replace the pyramid.", existing.display())));
        }
    }
    let mut levels = Vec::new();
    let mut reports = Vec::new();
    let mut total = 0;
    for level in 0..count {
        let image = if level == 0 { map.clone() } else { pyramid::level(&map, level) };
        let (w, h) = image.dimensions();
        let written = write_level(&image, level, opts)?;
        let bytes: usize = written.iter().map(|t| t.bytes).sum();
        total += bytes as u64;
        let mut tiles = Map::new();
        for t in &written {
            tiles.insert(format!("{}_{}", t.x, t.y), json!(t.file));
        }
        levels.push(json!({"level": level, "width": w, "height": h, "cols": w.div_ceil(tile), "rows": h.div_ceil(tile), "tiles": tiles}));
        reports.push(json!({"level": level, "size": format!("{w}x{h}"), "tiles": written.len(), "bytes": bytes, "changed": written.iter().filter(|t| t.changed).count()}));
        if !opts.quiet {
            eprintln!("level {level}: {w}x{h}, {} tiles, {} KB", written.len(), bytes / 1024);
        }
    }
    let manifest = json!({"mapWidth": width, "mapHeight": height, "tileSize": tile, "format": opts.format.name(), "levels": levels});
    let mut text = serde_json::to_string_pretty(&manifest).map_err(|e| Error::other(e.to_string()))?;
    text.push('\n');
    cli::write_output(&manifest_path, text.as_bytes(), opts.force)?;
    if opts.json {
        println!("{}", json!({"manifest": manifest_path.display().to_string(), "levels": reports, "inputTiles": used, "inputBytes": input_bytes, "bytes": total}));
    } else {
        println!("{}", manifest_path.display());
    }
    if !opts.quiet {
        eprintln!("{used} tiles -> {count} levels, {} KB -> {} KB", input_bytes / 1024, total / 1024);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_and_validates_options() {
        let o = parse(&args(&["base", "-o", "out/", "--map-size", "3500x2000", "--merge", "512", "--levels=3", "--lossless"])).unwrap().unwrap();
        assert_eq!((o.map_size, o.tile, o.merge, o.levels, o.format, o.encoding.lossless), (Some((3500, 2000)), 256, Some(512), Some(3), Format::Webp, true));
        let error = |list: &[&str]| parse(&args(list)).unwrap_err().message;
        assert!(error(&["base"]).contains("-o"));
        assert!(error(&["a", "b", "-o", "o/"]).contains("one tile directory"));
        assert!(error(&["a", "-o", "o/", "--merge", "300"]).contains("multiple"));
        assert!(error(&["a", "-o", "o/", "--map-size", "35x"]).contains("3500x2000"));
        assert!(error(&["a", "-o", "o/", "-f", "jpeg"]).contains("webp or png"));
        assert_eq!(tile_coords(Path::new("d/13_7.png")), Some((13, 7)));
        assert_eq!(tile_coords(Path::new("d/a_7.png")), None);
        assert_eq!(tile_coords(Path::new("d/1_7.txt")), None);
    }

    #[test]
    fn builds_levels_crops_padding_and_lists_tiles() {
        let dir = crate::auth::tests::temp_dir("pyramid-run");
        let tiles = dir.join("baseTiles");
        std::fs::create_dir_all(&tiles).unwrap();
        // A 3x2 grid of 16 px tiles for a 40x20 map: the last column and row are partly padding.
        for x in 0..3u8 {
            for y in 0..2u8 {
                RgbaImage::from_pixel(16, 16, Rgba([x * 80, y * 120, 50, 255])).save(tiles.join(format!("{x}_{y}.png"))).unwrap();
            }
        }
        std::fs::write(tiles.join(".DS_Store"), "x").unwrap();
        let out = dir.join("web/base");
        let list = [tiles.display().to_string(), "-o".into(), format!("{}/", out.display()), "--map-size".into(), "40x20".into(), "--tile".into(), "16".into(), "-f".into(), "png".into(), "--quiet".into()];
        let opts = parse(&list).unwrap().unwrap();
        assert_eq!(run(&opts).unwrap(), 0);
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(out.join("pyramid.json")).unwrap()).unwrap();
        assert_eq!((manifest["mapWidth"].as_u64(), manifest["mapHeight"].as_u64(), manifest["tileSize"].as_u64()), (Some(40), Some(20), Some(16)));
        let levels = manifest["levels"].as_array().unwrap();
        // 40x20 → 20x10 → 10x5: the third fits one 16 px tile.
        assert_eq!(levels.iter().map(|l| (l["width"].as_u64().unwrap(), l["height"].as_u64().unwrap())).collect::<Vec<_>>(), [(40, 20), (20, 10), (10, 5)]);
        assert_eq!(levels[0]["tiles"].as_object().unwrap().len(), 6);
        assert_eq!(levels[1]["tiles"], json!({"0_0": "1/0_0.png", "1_0": "1/1_0.png"}));
        let edge = image::open(out.join("0/2_1.png")).unwrap();
        assert_eq!((edge.width(), edge.height()), (8, 4));
        assert_eq!(image::open(out.join("2/0_0.png")).unwrap().width(), 10);

        let before = std::fs::read(out.join("1/1_0.png")).unwrap();
        assert!(run(&opts).unwrap_err().message.contains("--force"));
        assert_eq!(run(&PyramidOptions { force: true, ..opts }).unwrap(), 0);
        assert_eq!(std::fs::read(out.join("1/1_0.png")).unwrap(), before);
    }
}
