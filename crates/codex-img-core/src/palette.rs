//! Fixed palettes: reading one (hex codes, a .gpl/.hex file, a swatch image or a preset), the
//! sentence that asks the model for it, and snapping an image's colours to it.
//!
//! Tests showed the prompt gets colours close but never exact (81-88% of pixels within 16 of a
//! palette colour, thousands of distinct colours), so the colours are snapped afterwards, in
//! OKLab. Images generated with the palette in their prompt need only that and a 3x3 despeckle;
//! `clean` is for images that weren't: they're reduced to fewer colours first and matched hue first,
//! so shading that falls between palette colours doesn't flicker into speckles and streaks.
use crate::error::{Error, Result};
use crate::images;
use image::RgbaImage;
use std::collections::HashMap;
use std::path::Path;

pub type Rgb = [u8; 3];

/// One palette index stays free for transparency.
pub const MAX_COLORS: usize = 255;
/// With a palette, alpha is hardened at this threshold unless --hard-alpha says otherwise.
pub const PALETTE_ALPHA: u8 = 127;
/// `clean` first reduces the image to twice the palette's colours, but at least this many: a
/// fixed 32 left a 64-colour palette using 22 of its colours, where the plain snap used 44.
const CLEAN_COLORS: usize = 32;
/// `clean` weighs lightness at this share of hue and chroma, so a dark brown snaps to the
/// palette's dark neutral rather than to a different hue of the same lightness.
const CLEAN_LIGHTNESS: f32 = 0.5;

pub fn hex(c: Rgb) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// The sentence added to a prompt; in tests, hex codes worked where a palette's name didn't.
pub fn sentence(colors: &[Rgb]) -> String {
    let list: Vec<String> = colors.iter().map(|&c| hex(c)).collect();
    format!("Use only these {} colours, exactly, and no others: {}. No gradients, no colours in between.", colors.len(), list.join(", "))
}

fn parse_hex(token: &str) -> Option<Rgb> {
    let digits = token.strip_prefix('#').unwrap_or(token);
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

fn tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty())
}

/// Whether `spec` is a list of hex colours rather than a file or a preset name.
pub fn is_list(spec: &str) -> bool {
    tokens(spec).count() > 0 && tokens(spec).all(|t| parse_hex(t).is_some())
}

/// `#2B1D14, #6B3E26 ...`: commas, spaces or newlines between, `#` optional.
pub fn parse_list(text: &str) -> Result<Vec<Rgb>> {
    let colors = tokens(text).map(|t| parse_hex(t).ok_or_else(|| Error::usage(format!("\"{t}\" is not a colour; use #rrggbb.")))).collect::<Result<Vec<_>>>()?;
    checked(colors)
}

fn checked(mut colors: Vec<Rgb>) -> Result<Vec<Rgb>> {
    let mut seen = std::collections::HashSet::new();
    colors.retain(|c| seen.insert(*c));
    if !(2..=MAX_COLORS).contains(&colors.len()) {
        return Err(Error::usage(format!("A palette needs 2 to {MAX_COLORS} different colours, not {}.", colors.len())));
    }
    Ok(colors)
}

/// A GIMP .gpl palette, a swatch image (its distinct opaque colours, in reading order), or a
/// text file of hex codes (Lospec's .hex).
pub fn from_file(path: &Path) -> Result<Vec<Rgb>> {
    let bytes = std::fs::read(path).map_err(|e| Error::usage(format!("Unable to read palette {}: {e}", path.display())))?;
    let context = |e: Error| Error::usage(format!("{}: {}", path.display(), e.message));
    if images::sniff(&bytes).is_some() {
        let rgba = images::decode(&bytes).map_err(context)?;
        let mut colors = Vec::new();
        for p in rgba.pixels().filter(|p| p.0[3] >= 128) {
            let c = [p.0[0], p.0[1], p.0[2]];
            if !colors.contains(&c) {
                colors.push(c);
                if colors.len() > MAX_COLORS {
                    return Err(context(Error::usage(format!("has more than {MAX_COLORS} colours; a swatch image should have one flat patch per colour."))));
                }
            }
        }
        return checked(colors).map_err(context);
    }
    let text = String::from_utf8(bytes).map_err(|_| context(Error::usage("is not a palette: use hex codes, a .gpl file or a swatch image.")))?;
    if text.starts_with("GIMP Palette") {
        let mut colors = Vec::new();
        for line in text.lines().skip(1).map(str::trim) {
            if line.is_empty() || line.starts_with('#') || line.contains(':') {
                continue;
            }
            let rgb: Vec<u8> = line.split_whitespace().take(3).filter_map(|v| v.parse().ok()).collect();
            match rgb[..] {
                [r, g, b] => colors.push([r, g, b]),
                _ => return Err(context(Error::usage(format!("can't read the line \"{line}\"")))),
            }
        }
        return checked(colors).map_err(context);
    }
    parse_list(&text).map_err(context)
}

// --- Snapping ---

fn oklab(c: Rgb) -> [f32; 3] {
    let lin = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin(c[0]), lin(c[1]), lin(c[2]));
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// Nearest palette colour in OKLab, remembered per input colour.
struct Matcher<'a> {
    colors: &'a [Rgb],
    lab: Vec<[f32; 3]>,
    lightness: f32,
    cache: HashMap<Rgb, Rgb>,
}

impl<'a> Matcher<'a> {
    fn new(colors: &'a [Rgb], lightness: f32) -> Self {
        Matcher { colors, lab: colors.iter().map(|&c| oklab(c)).collect(), lightness, cache: HashMap::new() }
    }

    fn nearest(&mut self, c: Rgb) -> Rgb {
        if let Some(&hit) = self.cache.get(&c) {
            return hit;
        }
        let [l, a, b] = oklab(c);
        let distance = |q: &[f32; 3]| (self.lightness * (l - q[0])).powi(2) + (a - q[1]).powi(2) + (b - q[2]).powi(2);
        // Ties go to the earlier palette colour, so the result never depends on float noise order.
        let best = (0..self.colors.len()).fold(0, |best, i| if distance(&self.lab[i]) < distance(&self.lab[best]) { i } else { best });
        self.cache.insert(c, self.colors[best]);
        self.colors[best]
    }
}

/// Every visible pixel becomes its nearest palette colour, every transparent one (0,0,0,0).
/// Expects hard alpha (0 or 255).
pub fn snap_exact(rgba: &mut RgbaImage, colors: &[Rgb]) {
    let mut matcher = Matcher::new(colors, 1.0);
    for p in rgba.pixels_mut() {
        p.0 = if p.0[3] < 128 { [0; 4] } else {
            let [r, g, b] = matcher.nearest([p.0[0], p.0[1], p.0[2]]);
            [r, g, b, 255]
        };
    }
}

/// The full-size pass: snap (after reducing to 32 colours and matching hue first when `clean`),
/// then despeckle.
pub fn snap(rgba: &mut RgbaImage, colors: &[Rgb], clean: bool) {
    if clean {
        reduce(rgba, (colors.len() * 2).clamp(CLEAN_COLORS, 256));
        let mut matcher = Matcher::new(colors, CLEAN_LIGHTNESS);
        for p in rgba.pixels_mut() {
            p.0 = if p.0[3] < 128 { [0; 4] } else {
                let [r, g, b] = matcher.nearest([p.0[0], p.0[1], p.0[2]]);
                [r, g, b, 255]
            };
        }
    } else {
        snap_exact(rgba, colors);
    }
    despeckle(rgba);
}

/// Reduce the visible pixels to `count` colours (exoquant k-means, as -c uses), so each region of
/// similar shading is snapped as one.
fn reduce(rgba: &mut RgbaImage, count: usize) {
    use exoquant::{ditherer, optimizer, Color};
    let pixels: Vec<Color> = rgba.pixels().map(|p| if p.0[3] < 128 { Color::new(0, 0, 0, 0) } else { Color::new(p.0[0], p.0[1], p.0[2], 255) }).collect();
    let (palette, indices) = exoquant::convert_to_indexed(&pixels, rgba.width() as usize, count, &optimizer::KMeans, &ditherer::None);
    for (p, &i) in rgba.pixels_mut().zip(&indices) {
        let c = palette[usize::from(i)];
        p.0 = if p.0[3] < 128 { [0; 4] } else { [c.r, c.g, c.b, 255] };
    }
}

/// Each visible pixel takes the most common colour among the visible pixels of its 3x3
/// neighbourhood; it keeps its own on a tie. Removes stray single pixels and ragged edges between
/// two palette colours.
fn despeckle(rgba: &mut RgbaImage) {
    let source = rgba.clone();
    let (w, h) = source.dimensions();
    for y in 0..h {
        for x in 0..w {
            let own = source.get_pixel(x, y).0;
            if own[3] == 0 {
                continue;
            }
            let mut counts: [([u8; 4], u8); 9] = [([0; 4], 0); 9];
            let mut used = 0;
            for ny in y.saturating_sub(1)..(y + 2).min(h) {
                for nx in x.saturating_sub(1)..(x + 2).min(w) {
                    let c = source.get_pixel(nx, ny).0;
                    if c[3] == 0 {
                        continue;
                    }
                    match counts[..used].iter_mut().find(|(k, _)| *k == c) {
                        Some(entry) => entry.1 += 1,
                        None => {
                            counts[used] = (c, 1);
                            used += 1;
                        }
                    }
                }
            }
            let own_count = counts[..used].iter().find(|(k, _)| *k == own).map_or(0, |e| e.1);
            if let Some(&(winner, n)) = counts[..used].iter().max_by_key(|e| e.1) {
                if n > own_count {
                    rgba.get_pixel_mut(x, y).0 = winner;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_lists_files_and_swatches() {
        assert_eq!(parse_list("#2B1D14, 6b3e26\n#2B1D14").unwrap(), [[0x2B, 0x1D, 0x14], [0x6B, 0x3E, 0x26]], "duplicates go");
        assert!(parse_list("#2B1D14").unwrap_err().message.contains("2 to 255"));
        assert!(parse_list("#2B1D14 #GGGGGG").unwrap_err().message.contains("\"#GGGGGG\""));
        assert!(is_list("#000000,#FFFFFF") && !is_list("pico-8") && !is_list("art/palette.gpl"));
        assert_eq!(sentence(&[[0, 0, 0], [255, 255, 255]]), "Use only these 2 colours, exactly, and no others: #000000, #FFFFFF. No gradients, no colours in between.");

        let dir = tempfile::tempdir().unwrap().keep();
        std::fs::write(dir.join("p.gpl"), "GIMP Palette\nName: Two\nColumns: 2\n# comment\n  0   0   0\tBlack\n255 255 255 White\n").unwrap();
        assert_eq!(from_file(&dir.join("p.gpl")).unwrap(), [[0, 0, 0], [255, 255, 255]]);
        std::fs::write(dir.join("p.hex"), "ff0000\n00ff00\n").unwrap();
        assert_eq!(from_file(&dir.join("p.hex")).unwrap(), [[255, 0, 0], [0, 255, 0]]);
        let mut swatch = RgbaImage::from_pixel(4, 1, image::Rgba([10, 20, 30, 255]));
        swatch.put_pixel(2, 0, image::Rgba([200, 100, 50, 255]));
        swatch.put_pixel(3, 0, image::Rgba([1, 2, 3, 0]));
        swatch.save(dir.join("swatch.png")).unwrap();
        assert_eq!(from_file(&dir.join("swatch.png")).unwrap(), [[10, 20, 30], [200, 100, 50]], "transparent pixels don't count");
    }

    #[test]
    fn snaps_to_the_palette_and_despeckles() {
        let palette = [[0, 0, 0], [255, 255, 255], [200, 40, 40]];
        // A light-grey field with one dark speck, a reddish pixel and a transparent corner.
        let mut rgba = RgbaImage::from_pixel(5, 5, image::Rgba([230, 230, 225, 255]));
        rgba.put_pixel(2, 2, image::Rgba([40, 40, 40, 255]));
        rgba.put_pixel(0, 0, image::Rgba([9, 9, 9, 0]));
        let mut exact = rgba.clone();
        exact.put_pixel(4, 4, image::Rgba([190, 60, 50, 255]));
        snap_exact(&mut exact, &palette);
        assert_eq!((exact.get_pixel(2, 2).0, exact.get_pixel(4, 4).0, exact.get_pixel(0, 0).0), ([0, 0, 0, 255], [200, 40, 40, 255], [0; 4]));
        snap(&mut rgba, &palette, false);
        assert_eq!(rgba.get_pixel(2, 2).0, [255, 255, 255, 255], "the lone speck takes its neighbours' colour");
        assert!(rgba.pixels().all(|p| p.0 == [0; 4] || palette.iter().any(|c| p.0 == [c[0], c[1], c[2], 255])));

        // Matching hue first: a dark brown goes to the dark neutral, not to the purple of its lightness.
        let pico: Vec<Rgb> = [[0x5F, 0x57, 0x4F], [0x7E, 0x25, 0x53], [0, 0, 0]].to_vec();
        let brown = [0x5A, 0x34, 0x28];
        assert_eq!(Matcher::new(&pico, CLEAN_LIGHTNESS).nearest(brown), [0x5F, 0x57, 0x4F]);
    }
}
