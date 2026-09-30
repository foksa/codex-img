//! `codex-img sheet`: lay images out in one labelled grid (a contact sheet), to review a batch of
//! sprites at a glance. Sprites stand on a common baseline, so one that floats shows a gap.
use crate::cli;
use crate::convert;
use crate::error::{Error, Result};
use crate::images::{self, Encoding, Format};
use crate::transform;
use image::{imageops, Rgba, RgbaImage};
use serde_json::json;
use std::path::Path;

const DEFAULT_CELL: u32 = 240;
const MAX_CELL: u32 = 2048;
const MAX_SHEET_SIDE: u32 = 16_384;
const DEFAULT_BG: [u8; 3] = [150, 190, 150];
/// Space between a sprite and its cell's edges.
const MARGIN: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Labels {
    Name,
    Path,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetOptions {
    pub inputs: Vec<String>,
    pub output: String,
    pub format: Format,
    pub cols: Option<u32>,
    pub cell: u32,
    pub bg: [u8; 3],
    pub labels: Labels,
    /// Centre sprites in their cells instead of standing them on the baseline.
    pub center: bool,
    /// One scale for every sprite, so their sizes stay comparable.
    pub same_scale: bool,
    pub force: bool,
    pub json: bool,
    pub quiet: bool,
}

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
        match convert::read_image(Path::new(input)).and_then(|(bytes, _)| images::decode(&bytes)) {
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
    let written = cli::write_output(target, &bytes, opts.force)?;
    let (width, height) = sheet.dimensions();
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

fn label_for(input: &str, labels: Labels) -> String {
    match labels {
        Labels::None => String::new(),
        Labels::Path => input.to_string(),
        Labels::Name => Path::new(input).file_name().map_or_else(|| input.to_string(), |n| n.to_string_lossy().into_owned()),
    }
}

/// Glyph scale for a cell size: 2 (10x16 px per character) in the default 240 px cells.
fn text_scale(cell: u32) -> u32 {
    if cell >= 160 {
        2
    } else {
        1
    }
}

fn compose(sprites: &[(String, RgbaImage)], opts: &SheetOptions) -> Result<(RgbaImage, u32, u32)> {
    let count = sprites.len() as u32;
    let cols = opts.cols.unwrap_or_else(|| (f64::from(count).sqrt().ceil() as u32).max(1)).min(count);
    let rows = count.div_ceil(cols);
    let cell = opts.cell;
    let scale = text_scale(cell);
    let label_height = if opts.labels == Labels::None { 0 } else { (GLYPH_HEIGHT + 2) * scale };
    let pitch = cell + label_height;
    let (width, height) = (u64::from(cols) * u64::from(cell), u64::from(rows) * u64::from(pitch));
    if width > u64::from(MAX_SHEET_SIDE) || height > u64::from(MAX_SHEET_SIDE) {
        return Err(Error::usage(format!(
            "The sheet would be {width}x{height} px (at most {MAX_SHEET_SIDE} per side); use a smaller --cell, other --cols, or fewer images."
        )));
    }
    let [r, g, b] = opts.bg;
    let mut sheet = RgbaImage::from_pixel(width as u32, height as u32, Rgba([r, g, b, 255]));
    let room = cell - 2 * MARGIN;
    let fit = |image: &RgbaImage| {
        let (w, h) = image.dimensions();
        (f64::from(room) / f64::from(w)).min(f64::from(room) / f64::from(h)).min(1.0)
    };
    let shared = sprites.iter().map(|(_, image)| fit(image)).fold(1.0, f64::min);
    let ink = contrasting(opts.bg);
    let baseline = shade(opts.bg);
    for (index, (label, image)) in sprites.iter().enumerate() {
        let (x0, y0) = ((index as u32 % cols) * cell, (index as u32 / cols) * pitch);
        let factor = if opts.same_scale { shared } else { fit(image) };
        let (w, h) = image.dimensions();
        let size = |side: u32| ((f64::from(side) * factor).round() as u32).clamp(1, room);
        let scaled = transform::resample(image, size(w), size(h));
        let x = x0 + (cell - scaled.width()) / 2;
        let y = if opts.center { y0 + (cell - scaled.height()) / 2 } else { y0 + cell - MARGIN - scaled.height() };
        if !opts.center {
            // The line sprites stand on; anything floating shows a gap above it.
            for bx in x0 + MARGIN..x0 + cell - MARGIN {
                sheet.put_pixel(bx, y0 + cell - MARGIN, baseline);
            }
        }
        imageops::overlay(&mut sheet, &scaled, i64::from(x), i64::from(y));
        draw_text(&mut sheet, &fit_label(label, (cell - 2 * MARGIN) / (GLYPH_ADVANCE * scale)), x0 + MARGIN, y0 + cell + scale, scale, ink);
    }
    Ok((sheet, cols, rows))
}

/// Black on light backgrounds, white on dark ones.
fn contrasting([r, g, b]: [u8; 3]) -> Rgba<u8> {
    let luma = 299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b);
    if luma > 128_000 {
        Rgba([0, 0, 0, 255])
    } else {
        Rgba([255, 255, 255, 255])
    }
}

/// The background a little darker (or lighter, on dark backgrounds): visible but quiet.
fn shade(bg: [u8; 3]) -> Rgba<u8> {
    let dark = contrasting(bg).0[0] == 0;
    let [r, g, b] = bg.map(|c| if dark { c - c / 4 } else { c + (255 - c) / 4 });
    Rgba([r, g, b, 255])
}

/// Shorten `label` to `max` characters, marking the cut with `~`.
fn fit_label(label: &str, max: u32) -> String {
    let max = max as usize;
    if label.chars().count() <= max {
        return label.to_string();
    }
    let mut short: String = label.chars().take(max.saturating_sub(1)).collect();
    short.push('~');
    short
}

const GLYPH_HEIGHT: u32 = 8;
/// Five pixels of glyph plus one of space.
const GLYPH_ADVANCE: u32 = 6;

fn draw_text(image: &mut RgbaImage, text: &str, x: u32, y: u32, scale: u32, ink: Rgba<u8>) {
    for (n, c) in text.chars().enumerate() {
        let code = c as usize;
        let glyph = if (32..127).contains(&code) { &GLYPHS[code - 32] } else { &GLYPHS[usize::from(b'?') - 32] };
        let left = x + n as u32 * GLYPH_ADVANCE * scale;
        for (row, bits) in glyph.iter().enumerate() {
            for column in 0..5 {
                if bits & (0x10 >> column) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let (px, py) = (left + column * scale + dx, y + row as u32 * scale + dy);
                        if px < image.width() && py < image.height() {
                            image.put_pixel(px, py, ink);
                        }
                    }
                }
            }
        }
    }
}

/// 5x8 bitmap font for printable ASCII, one byte per row with bit 4 as the leftmost pixel.
/// Characters without a glyph of their own draw as `?`.
#[rustfmt::skip]
const GLYPHS: [[u8; 8]; 95] = [
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // ' '
    [0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04, 0x00], // '!'
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04, 0x00], // '"' (not drawn: ?)
    [0x0a, 0x0a, 0x1f, 0x0a, 0x1f, 0x0a, 0x0a, 0x00], // '#'
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04, 0x00], // '$' (not drawn: ?)
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04, 0x00], // '%' (not drawn: ?)
    [0x0c, 0x12, 0x14, 0x08, 0x15, 0x12, 0x0d, 0x00], // '&'
    [0x04, 0x04, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00], // "'"
    [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02, 0x00], // '('
    [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08, 0x00], // ')'
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04, 0x00], // '*' (not drawn: ?)
    [0x00, 0x04, 0x04, 0x1f, 0x04, 0x04, 0x00, 0x00], // '+'
    [0x00, 0x00, 0x00, 0x00, 0x0c, 0x04, 0x08, 0x00], // ','
    [0x00, 0x00, 0x00, 0x1f, 0x00, 0x00, 0x00, 0x00], // '-'
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x0c, 0x00], // '.'
    [0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x00, 0x00], // '/'
    [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e, 0x00], // '0'
    [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e, 0x00], // '1'
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f, 0x00], // '2'
    [0x1f, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0e, 0x00], // '3'
    [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02, 0x00], // '4'
    [0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e, 0x00], // '5'
    [0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e, 0x00], // '6'
    [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08, 0x00], // '7'
    [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e, 0x00], // '8'
    [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c, 0x00], // '9'
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04, 0x00], // ':' (not drawn: ?)
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04, 0x00], // ';' (not drawn: ?)
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04, 0x00], // '<' (not drawn: ?)
    [0x00, 0x00, 0x1f, 0x00, 0x1f, 0x00, 0x00, 0x00], // '='
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04, 0x00], // '>' (not drawn: ?)
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04, 0x00], // '?'
    [0x0e, 0x11, 0x17, 0x15, 0x17, 0x10, 0x0e, 0x00], // '@'
    [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11, 0x00], // 'A'
    [0x1e, 0x11, 0x11, 0x1e, 0x11, 0x11, 0x1e, 0x00], // 'B'
    [0x0e, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0e, 0x00], // 'C'
    [0x1e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1e, 0x00], // 'D'
    [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f, 0x00], // 'E'
    [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10, 0x00], // 'F'
    [0x0e, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0f, 0x00], // 'G'
    [0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11, 0x00], // 'H'
    [0x0e, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0e, 0x00], // 'I'
    [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0c, 0x00], // 'J'
    [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11, 0x00], // 'K'
    [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1f, 0x00], // 'L'
    [0x11, 0x1b, 0x15, 0x15, 0x11, 0x11, 0x11, 0x00], // 'M'
    [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x00], // 'N'
    [0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e, 0x00], // 'O'
    [0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10, 0x10, 0x00], // 'P'
    [0x0e, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0d, 0x00], // 'Q'
    [0x1e, 0x11, 0x11, 0x1e, 0x14, 0x12, 0x11, 0x00], // 'R'
    [0x0f, 0x10, 0x10, 0x0e, 0x01, 0x01, 0x1e, 0x00], // 'S'
    [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x00], // 'T'
    [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e, 0x00], // 'U'
    [0x11, 0x11, 0x11, 0x11, 0x11, 0x0a, 0x04, 0x00], // 'V'
    [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0a, 0x00], // 'W'
    [0x11, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x11, 0x00], // 'X'
    [0x11, 0x11, 0x0a, 0x04, 0x04, 0x04, 0x04, 0x00], // 'Y'
    [0x1f, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1f, 0x00], // 'Z'
    [0x0e, 0x08, 0x08, 0x08, 0x08, 0x08, 0x0e, 0x00], // '['
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04, 0x00], // '\\' (not drawn: ?)
    [0x0e, 0x02, 0x02, 0x02, 0x02, 0x02, 0x0e, 0x00], // ']'
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04, 0x00], // '^' (not drawn: ?)
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1f], // '_'
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04, 0x00], // '`' (not drawn: ?)
    [0x00, 0x00, 0x0e, 0x01, 0x0f, 0x11, 0x0f, 0x00], // 'a'
    [0x10, 0x10, 0x1e, 0x11, 0x11, 0x11, 0x1e, 0x00], // 'b'
    [0x00, 0x00, 0x0e, 0x10, 0x10, 0x11, 0x0e, 0x00], // 'c'
    [0x01, 0x01, 0x0f, 0x11, 0x11, 0x11, 0x0f, 0x00], // 'd'
    [0x00, 0x00, 0x0e, 0x11, 0x1f, 0x10, 0x0e, 0x00], // 'e'
    [0x06, 0x09, 0x08, 0x1c, 0x08, 0x08, 0x08, 0x00], // 'f'
    [0x00, 0x00, 0x0f, 0x11, 0x11, 0x0f, 0x01, 0x0e], // 'g'
    [0x10, 0x10, 0x1e, 0x11, 0x11, 0x11, 0x11, 0x00], // 'h'
    [0x04, 0x00, 0x0c, 0x04, 0x04, 0x04, 0x0e, 0x00], // 'i'
    [0x02, 0x00, 0x06, 0x02, 0x02, 0x02, 0x12, 0x0c], // 'j'
    [0x10, 0x10, 0x12, 0x14, 0x18, 0x14, 0x12, 0x00], // 'k'
    [0x0c, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0e, 0x00], // 'l'
    [0x00, 0x00, 0x1a, 0x15, 0x15, 0x15, 0x15, 0x00], // 'm'
    [0x00, 0x00, 0x1e, 0x11, 0x11, 0x11, 0x11, 0x00], // 'n'
    [0x00, 0x00, 0x0e, 0x11, 0x11, 0x11, 0x0e, 0x00], // 'o'
    [0x00, 0x00, 0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10], // 'p'
    [0x00, 0x00, 0x0f, 0x11, 0x11, 0x0f, 0x01, 0x01], // 'q'
    [0x00, 0x00, 0x16, 0x19, 0x10, 0x10, 0x10, 0x00], // 'r'
    [0x00, 0x00, 0x0f, 0x10, 0x0e, 0x01, 0x1e, 0x00], // 's'
    [0x08, 0x08, 0x1c, 0x08, 0x08, 0x09, 0x06, 0x00], // 't'
    [0x00, 0x00, 0x11, 0x11, 0x11, 0x13, 0x0d, 0x00], // 'u'
    [0x00, 0x00, 0x11, 0x11, 0x11, 0x0a, 0x04, 0x00], // 'v'
    [0x00, 0x00, 0x11, 0x11, 0x15, 0x15, 0x0a, 0x00], // 'w'
    [0x00, 0x00, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x00], // 'x'
    [0x00, 0x00, 0x11, 0x11, 0x11, 0x0f, 0x01, 0x0e], // 'y'
    [0x00, 0x00, 0x1f, 0x02, 0x04, 0x08, 0x1f, 0x00], // 'z'
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04, 0x00], // '{' (not drawn: ?)
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04, 0x00], // '|' (not drawn: ?)
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04, 0x00], // '}' (not drawn: ?)
    [0x00, 0x00, 0x08, 0x15, 0x02, 0x00, 0x00, 0x00], // '~'
];

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
    fn stands_sprites_on_the_baseline_and_labels_them() {
        let bg = DEFAULT_BG;
        let opts = parse(&args(&["x", "-o", "s.png", "--cell", "64"])).unwrap().unwrap();
        let solid = RgbaImage::from_pixel(20, 10, Rgba([200, 30, 30, 255]));
        // Floating: the lower half is transparent, so the red ends above the baseline.
        let floating = RgbaImage::from_fn(20, 20, |_, y| if y < 10 { Rgba([30, 30, 200, 255]) } else { Rgba([0, 0, 0, 0]) });
        let big = RgbaImage::from_pixel(400, 100, Rgba([30, 200, 30, 255]));
        let sprites = vec![("a".to_string(), solid), ("b".to_string(), floating), ("big".to_string(), big)];
        let (sheet, cols, rows) = compose(&sprites, &opts).unwrap();
        assert_eq!((cols, rows), (2, 2));
        let pitch = 64 + (GLYPH_HEIGHT + 2);
        assert_eq!(sheet.dimensions(), (128, 2 * pitch));

        let above_baseline = 64 - MARGIN - 1;
        assert_eq!(sheet.get_pixel(32, above_baseline).0, [200, 30, 30, 255], "solid sprite touches the baseline");
        assert_eq!(sheet.get_pixel(64 + 32, above_baseline).0, [bg[0], bg[1], bg[2], 255], "floating sprite leaves a gap");
        assert_eq!(sheet.get_pixel(64 + 32, above_baseline - 10).0, [30, 30, 200, 255]);
        assert_eq!(sheet.get_pixel(32, 64 - MARGIN).0, shade(bg).0, "baseline drawn");

        // Too big for its cell: shrunk to fit, keeping the aspect ratio, and centred.
        let row = pitch;
        assert_eq!(sheet.get_pixel(MARGIN, row + above_baseline).0, [30, 200, 30, 255]);
        assert_eq!(sheet.get_pixel(32, row + 64 - MARGIN - 15).0, [bg[0], bg[1], bg[2], 255], "56x14 after shrinking");

        // Labels are drawn in ink under each cell.
        let label_rows = 64..pitch;
        assert!(label_rows.clone().any(|y| (0..64).any(|x| sheet.get_pixel(x, y).0 == [0, 0, 0, 255])));
        assert!(label_rows.clone().all(|y| (64..128).all(|x| sheet.get_pixel(x, y + pitch).0 == [bg[0], bg[1], bg[2], 255])), "empty cell has no label");

        // --same-scale shrinks everything by the big sprite's factor.
        let same = SheetOptions { same_scale: true, ..opts.clone() };
        let (sheet, _, _) = compose(&sprites, &same).unwrap();
        let red = sheet.pixels().filter(|p| p.0 == [200, 30, 30, 255]).count();
        assert_eq!(red, 3, "20x10 at 56/400 is 3x1");
    }

    #[test]
    fn fits_labels_and_picks_ink() {
        assert_eq!(fit_label("fishing-boat.png", 20), "fishing-boat.png");
        assert_eq!(fit_label("fishing-boat.png", 8), "fishing~");
        assert_eq!(contrasting([250, 250, 250]).0, [0, 0, 0, 255]);
        assert_eq!(contrasting([20, 20, 40]).0, [255, 255, 255, 255]);
        assert_eq!(label_for("art/raw/boat.png", Labels::Name), "boat.png");
        assert_eq!(label_for("art/raw/boat.png", Labels::Path), "art/raw/boat.png");
        assert_eq!(GLYPHS[usize::from(b'A') - 32], [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11, 0x00]);
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
