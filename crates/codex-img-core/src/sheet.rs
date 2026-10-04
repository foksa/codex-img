use crate::{error::{Error, Result}, images::Format, transform};
use image::{imageops, Rgba, RgbaImage};
use std::path::Path;
pub const DEFAULT_CELL: u32 = 240;
pub const MAX_CELL: u32 = 2048;
const MAX_SHEET_SIDE: u32 = 16_384;
pub const DEFAULT_BG: [u8; 3] = [150, 190, 150];
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

pub fn label_for(input: &str, labels: Labels) -> String {
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

pub fn compose(sprites: &[(String, RgbaImage)], opts: &SheetOptions) -> Result<(RgbaImage, u32, u32)> {
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
        draw_text(&mut sheet, &fit_label(label, (cell - 2 * MARGIN) / (GLYPH_ADVANCE * scale), opts.labels), x0 + MARGIN, y0 + cell + scale, scale, ink);
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

/// Shorten `label` to `max` characters, marking the cut with `~`. A name keeps its start; a path
/// keeps its end, where the file name is.
fn fit_label(label: &str, max: u32, labels: Labels) -> String {
    let (max, count) = (max as usize, label.chars().count());
    if count <= max {
        return label.to_string();
    }
    let keep = max.saturating_sub(1);
    match labels {
        Labels::Path => std::iter::once('~').chain(label.chars().skip(count - keep)).collect(),
        _ => label.chars().take(keep).chain(std::iter::once('~')).collect(),
    }
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
    #[test]
    fn stands_sprites_on_the_baseline_and_labels_them() {
        let bg = DEFAULT_BG;
        let opts = SheetOptions { inputs: vec!["x".into()], output: "s.png".into(), format: Format::Png, cols: None, cell: 64, bg: DEFAULT_BG, labels: Labels::Name, center: false, same_scale: false, force: false, json: false, quiet: false };
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
        assert_eq!(fit_label("fishing-boat.png", 20, Labels::Name), "fishing-boat.png");
        assert_eq!(fit_label("fishing-boat.png", 8, Labels::Name), "fishing~");
        assert_eq!(fit_label("art/raw/harbor/boat.png", 12, Labels::Path), "~or/boat.png");
        assert_eq!(contrasting([250, 250, 250]).0, [0, 0, 0, 255]);
        assert_eq!(contrasting([20, 20, 40]).0, [255, 255, 255, 255]);
        assert_eq!(label_for("art/raw/boat.png", Labels::Name), "boat.png");
        assert_eq!(label_for("art/raw/boat.png", Labels::Path), "art/raw/boat.png");
        assert_eq!(GLYPHS[usize::from(b'A') - 32], [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11, 0x00]);
    }

}
