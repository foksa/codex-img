//! Pixel edits for `codex-img convert`: key out an unwanted background, trim to the visible
//! content, resize with premultiplied alpha, and edge bleed under fully transparent pixels.
use crate::error::{Error, Result};
use crate::images::{Encoding, Format};
use image::{imageops, ImageBuffer, Rgba, RgbaImage};

pub const MAX_SIDE: u32 = 8192;
const MAX_TRIM_PADDING: u32 = 1024;
/// Alpha at or below this doesn't count as visible for --trim, and is the --hard-alpha default.
/// Generated images scatter faint specks (alpha 1-16) over their transparent background, which
/// would otherwise stop --trim from cropping at all.
pub const FAINT_ALPHA: u8 = 16;
/// After resampling, --hard-alpha makes pixels at least half covered solid, keeping the shape's area.
const RESAMPLED_HARD_ALPHA: u8 = 127;
/// Catmull-Rom rather than Lanczos3: one negative lobe and no positive outer one, so hard alpha
/// edges don't grow a faint ring of barely visible pixels.
const FILTER: imageops::FilterType = imageops::FilterType::CatmullRom;

/// How `--resize WxH` treats an image whose aspect ratio differs from the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Scale to fit inside the box, keeping the aspect ratio; one side may come out smaller.
    Inside,
    /// Scale to fill the box and crop the overflow from the centre: exactly WxH.
    Cover,
    /// Scale to fit inside the box and pad with transparency, centred: exactly WxH.
    Contain,
    /// Stretch to exactly WxH, ignoring the aspect ratio.
    Fill,
}

impl Fit {
    pub fn parse(value: &str) -> Result<Fit> {
        match value.to_ascii_lowercase().as_str() {
            "inside" => Ok(Fit::Inside),
            "cover" => Ok(Fit::Cover),
            "contain" => Ok(Fit::Contain),
            "fill" => Ok(Fit::Fill),
            _ => Err(Error::usage("--fit must be one of: inside, cover, contain, fill")),
        }
    }
}

/// `--resize`: `WxH`, `Wx` or `xH`. A missing side follows the aspect ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resize {
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl Resize {
    pub fn parse(value: &str) -> Result<Resize> {
        let invalid = || Error::usage(format!("--resize must be WxH, Wx or xH with sides from 1 to {MAX_SIDE}, e.g. 512x512 or 400x."));
        let (w, h) = value.split_once(['x', 'X']).ok_or_else(invalid)?;
        let side = |s: &str| -> Result<Option<u32>> {
            if s.is_empty() {
                return Ok(None);
            }
            s.parse::<u32>().ok().filter(|n| (1..=MAX_SIDE).contains(n)).map(Some).ok_or_else(invalid)
        };
        let resize = Resize { width: side(w)?, height: side(h)? };
        if resize.width.is_none() && resize.height.is_none() {
            return Err(invalid());
        }
        Ok(resize)
    }
}

/// `--key`: which colours count as painted-in background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// The background's own colours, sampled along the edges of the visible content (see
    /// `resolve_keys`), each matched within `tolerance` per channel.
    Auto { tolerance: u8 },
    /// A named colour: a range of hue, saturation and brightness.
    Named(Colour),
    /// Within `tolerance` of `rgb` on every channel.
    Rgb { rgb: [u8; 3], tolerance: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    Red,
    Orange,
    Yellow,
    Green,
    Cyan,
    Blue,
    Purple,
    Pink,
    White,
    Gray,
    Black,
}

const COLOURS: [(&str, Colour); 11] = [
    ("red", Colour::Red),
    ("orange", Colour::Orange),
    ("yellow", Colour::Yellow),
    ("green", Colour::Green),
    ("cyan", Colour::Cyan),
    ("blue", Colour::Blue),
    ("purple", Colour::Purple),
    ("pink", Colour::Pink),
    ("white", Colour::White),
    ("gray", Colour::Gray),
    ("black", Colour::Black),
];

/// Per-channel tolerance for `#rrggbb` and `auto` keys when none is given.
const KEY_TOLERANCE: u8 = 32;

impl Colour {
    /// Hue sectors in degrees for the chromatic colours; the neutral ones go by brightness. A
    /// colour needs some saturation (chroma at least a quarter of its brightest channel) to have a
    /// hue, so dark wave shading is still blue but a grey hull isn't.
    fn matches(self, [r, g, b]: [u8; 3]) -> bool {
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let (max16, chroma) = (u16::from(max), u16::from(max - min));
        let neutral = chroma * 5 <= max16;
        let sector = match self {
            Colour::White => return neutral && max >= 190,
            Colour::Gray => return neutral && (64..190).contains(&max),
            Colour::Black => return max < 64,
            Colour::Red => (345.0, 15.0),
            Colour::Orange => (15.0, 45.0),
            Colour::Yellow => (45.0, 70.0),
            Colour::Green => (70.0, 165.0),
            Colour::Cyan => (165.0, 195.0),
            Colour::Blue => (195.0, 260.0),
            Colour::Purple => (260.0, 300.0),
            Colour::Pink => (300.0, 345.0),
        };
        if max < 48 || chroma * 4 < max16 {
            return false;
        }
        let hue = hue([r, g, b]);
        match sector {
            (from, to) if from > to => hue >= from || hue < to,
            (from, to) => (from..to).contains(&hue),
        }
    }
}

/// Hue in degrees, 0 to 360 (red at 0, green at 120, blue at 240).
fn hue([r, g, b]: [u8; 3]) -> f32 {
    let (r, g, b) = (f32::from(r), f32::from(g), f32::from(b));
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let chroma = max - min;
    if chroma == 0.0 {
        return 0.0;
    }
    let sixths = if max == r {
        ((g - b) / chroma).rem_euclid(6.0)
    } else if max == g {
        (b - r) / chroma + 2.0
    } else {
        (r - g) / chroma + 4.0
    };
    sixths * 60.0
}

impl Key {
    pub fn parse(value: &str) -> Result<Key> {
        let names: Vec<&str> = COLOURS.iter().map(|(name, _)| *name).collect();
        let invalid = || {
            Error::usage(format!(
                "--key must be auto, a colour name ({}), or #rrggbb; auto and #rrggbb take a tolerance per channel (default {KEY_TOLERANCE}), e.g. auto:48 or #3070c0:40.",
                names.join(", ")
            ))
        };
        let lower = value.to_ascii_lowercase();
        if let Some((_, colour)) = COLOURS.iter().find(|(name, _)| *name == lower || (*name == "gray" && lower == "grey")) {
            return Ok(Key::Named(*colour));
        }
        let (what, tolerance) = match lower.split_once(':') {
            Some((w, t)) => (w, t.parse::<u8>().map_err(|_| invalid())?),
            None => (lower.as_str(), KEY_TOLERANCE),
        };
        if what == "auto" {
            return Ok(Key::Auto { tolerance });
        }
        let hex = what.strip_prefix('#').unwrap_or(what);
        let channel = |i: usize| hex.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (hex.len(), channel(0), channel(2), channel(4)) {
            (6, Some(r), Some(g), Some(b)) => Ok(Key::Rgb { rgb: [r, g, b], tolerance }),
            _ => Err(invalid()),
        }
    }

    /// `Auto` must have been resolved into `Rgb` keys by `resolve_keys` first; it matches nothing.
    fn matches(self, [r, g, b, _]: [u8; 4]) -> bool {
        match self {
            Key::Auto { .. } => false,
            Key::Named(colour) => colour.matches([r, g, b]),
            Key::Rgb { rgb, tolerance } => [r, g, b].iter().zip(rgb).all(|(&c, k)| c.abs_diff(k) <= tolerance),
        }
    }
}

/// Depth of the strip along each edge of the visible content that `--key auto` samples, as a
/// share of the content's height (top, bottom) or width (left, right); at least 3 pixels.
const AUTO_SAMPLE_PERCENT: u32 = 5;
/// `--key auto` keeps the most common colours until they cover this share of the samples, so
/// the few pixels of the object itself that reach into the strip don't become keys.
const AUTO_COVERAGE_PERCENT: u64 = 95;
const AUTO_MAX_COLOURS: usize = 64;

/// Replace each `Auto` key with the colours of the background to remove, sampled along the outer
/// edges of the visible content: the region's edges (painted ground under a sprite is at the
/// bottom, sky behind a building at the top and sides), or all four without a region. Colours are
/// grouped into bins of 16 levels per channel, and the most common bins become `Rgb` keys at their
/// average colour. Nothing here can tell that background from the object's own edge: sampled at
/// the bottom of a sprite without painted ground, a trunk, wheels or a pole would be keyed. So
/// `auto` is for images that do have something to remove along those edges.
fn resolve_keys(rgba: &RgbaImage, keys: &[Key], region: Option<Region>) -> Vec<Key> {
    let mut resolved = Vec::new();
    for &key in keys {
        match key {
            Key::Auto { tolerance } => resolved.extend(sample_edges(rgba, region).into_iter().map(|rgb| Key::Rgb { rgb, tolerance })),
            other => resolved.push(other),
        }
    }
    resolved
}

fn sample_edges(rgba: &RgbaImage, region: Option<Region>) -> Vec<[u8; 3]> {
    let Some(bounds) = visible_bounds(rgba) else { return Vec::new() };
    let (bx, by, bw, bh) = (bounds.x as u32, bounds.y as u32, bounds.width, bounds.height);
    let depth = |side: u32| (side * AUTO_SAMPLE_PERCENT).div_ceil(100).clamp(3, side);
    let edges = region.map_or([true; 4], |r| r.bands.map(|percent| percent > 0));
    let in_strip = |x: u32, y: u32| {
        (edges[Edge::Top as usize] && y < by + depth(bh))
            || (edges[Edge::Bottom as usize] && y >= by + bh - depth(bh))
            || (edges[Edge::Left as usize] && x < bx + depth(bw))
            || (edges[Edge::Right as usize] && x >= bx + bw - depth(bw))
    };
    let mut bins: std::collections::BTreeMap<[u8; 3], ([u64; 3], u64)> = std::collections::BTreeMap::new();
    for y in by..by + bh {
        for x in (bx..bx + bw).filter(|&x| in_strip(x, y)) {
            let [r, g, b, a] = rgba.get_pixel(x, y).0;
            if a > FAINT_ALPHA {
                let (sum, count) = bins.entry([r >> 4, g >> 4, b >> 4]).or_default();
                for (s, c) in sum.iter_mut().zip([r, g, b]) {
                    *s += u64::from(c);
                }
                *count += 1;
            }
        }
    }
    let total: u64 = bins.values().map(|(_, count)| count).sum();
    let mut ranked: Vec<([u64; 3], u64)> = bins.into_values().collect();
    // Most common first; ties keep the bins' colour order, so the result is deterministic.
    ranked.sort_by_key(|&(_, count)| std::cmp::Reverse(count));
    let mut covered = 0;
    let mut colours = Vec::new();
    for (sum, count) in ranked.into_iter().take(AUTO_MAX_COLOURS) {
        if covered * 100 >= total * AUTO_COVERAGE_PERCENT {
            break;
        }
        covered += count;
        colours.push(sum.map(|s| (s / count) as u8));
    }
    colours
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

impl Edge {
    fn parse(value: &str) -> Option<Edge> {
        match value {
            "top" => Some(Edge::Top),
            "bottom" => Some(Edge::Bottom),
            "left" => Some(Edge::Left),
            "right" => Some(Edge::Right),
            _ => None,
        }
    }
}

const EDGES: [Edge; 4] = [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right];

/// `--key-region bottom:30%`, `top:40%,left:15%` or `all:20%`: bands along edges of the visible
/// content that --key may touch, each a share of the content's height (top, bottom) or width
/// (left, right). Indexed by `Edge`; 0 means no band on that edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub bands: [u8; 4],
}

impl Region {
    pub fn parse(value: &str) -> Result<Region> {
        let invalid = || Error::usage("--key-region must be edges with a share of the visible content, e.g. bottom:30%, top:40%,left:15% or all:20%.");
        let mut bands = [0u8; 4];
        for part in value.split(',') {
            let (edge, amount) = part.split_once(':').ok_or_else(invalid)?;
            let percent = parse_percent(amount).ok_or_else(invalid)?;
            if edge == "all" {
                bands = [percent; 4];
            } else {
                bands[Edge::parse(edge).ok_or_else(invalid)? as usize] = percent;
            }
        }
        Ok(Region { bands })
    }
}

/// `--trim-density [edges:]fraction`: along these edges, --trim also drops rows (or columns) whose
/// visible pixels cover less than `percent` of the fullest row (or column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Density {
    pub edges: [bool; 4],
    pub percent: u8,
}

impl Density {
    pub fn parse(value: &str) -> Result<Density> {
        let invalid = || Error::usage("--trim-density must be a fraction such as 0.15 (or 15%), optionally after edges: bottom:0.15, bottom,top:0.15, all:0.15.");
        let (edges, amount) = value.rsplit_once(':').unwrap_or(("bottom", value));
        let mut set = [false; 4];
        for edge in edges.split(',') {
            if edge == "all" {
                set = [true; 4];
            } else {
                set[Edge::parse(edge).ok_or_else(invalid)? as usize] = true;
            }
        }
        Ok(Density { edges: set, percent: parse_percent(amount).ok_or_else(invalid)? })
    }

    fn has(&self, edge: Edge) -> bool {
        self.edges[edge as usize]
    }
}

pub fn parse_key_spread(value: &str) -> Result<u8> {
    value.parse::<u8>().ok().filter(|n| (1..=128).contains(n)).ok_or_else(|| Error::usage("--key-spread must be a colour step from 1 to 128, e.g. 24."))
}

/// Default `--key-cut`: lines at least 40% key colour are background, not the object.
pub const KEY_CUT: u8 = 40;

pub fn parse_key_cut(value: &str) -> Result<u8> {
    parse_percent(value).ok_or_else(|| Error::usage("--key-cut must be a fraction such as 0.4 (or 40%), e.g. --key-cut=0.4."))
}

/// `0.15`, `15%` -> 15. From 1 to 100.
fn parse_percent(value: &str) -> Option<u8> {
    let percent = match value.strip_suffix('%') {
        Some(p) => p.parse::<f64>().ok()?,
        None => value.parse::<f64>().ok()? * 100.0,
    };
    (percent.is_finite() && (0.5..=100.0).contains(&percent)).then(|| percent.round() as u8)
}

pub fn parse_hard_alpha(value: &str) -> Result<u8> {
    value
        .parse::<u8>()
        .ok()
        .filter(|n| *n < 255)
        .ok_or_else(|| Error::usage("--hard-alpha threshold must be an integer from 0 to 254, e.g. --hard-alpha=16."))
}

pub fn parse_trim_padding(value: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .ok()
        .filter(|n| *n <= MAX_TRIM_PADDING)
        .ok_or_else(|| Error::usage(format!("--trim padding must be an integer from 0 to {MAX_TRIM_PADDING}, e.g. --trim=8.")))
}

/// A rectangle in input pixels. With trim padding it can reach past the input's edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transform {
    /// Make alpha all-or-nothing: above this threshold solid, else fully transparent.
    pub hard_alpha: Option<u8>,
    /// Make painted-in background of these colours transparent, where it connects to the
    /// transparent background or the image's border.
    pub keys: Vec<Key>,
    pub key_region: Option<Region>,
    /// Before keying, cut off whole rows (or columns) from the key region's edges while at least
    /// this percentage of their visible pixels match the keys, like the sea below a boat's waterline.
    pub key_cut: Option<u8>,
    /// Also remove neighbours of removed pixels whose colour is within this of theirs, step by
    /// step: keying then follows gradients, like a sky fading from blue to gold, and stops at outlines.
    pub key_spread: Option<u8>,
    /// Crop to the visible (alpha > FAINT_ALPHA) pixels, plus this much transparent padding on each side.
    pub trim: Option<u32>,
    pub trim_density: Option<Density>,
    pub resize: Option<Resize>,
    pub fit: Option<Fit>,
    /// Keep the colour stored under fully transparent pixels instead of bleeding edge colours in.
    pub no_bleed: bool,
    /// Never scale up: --resize only shrinks, and leaves smaller images at their size.
    pub no_enlarge: bool,
}

pub struct Applied {
    pub image: RgbaImage,
    /// Whether any pixel (visible or not) differs from the input.
    pub changed: bool,
    /// The trim rectangle, in input pixels.
    pub trim: Option<Rect>,
}

impl Transform {
    pub fn check(&self) -> Result<()> {
        match (self.fit, self.resize) {
            _ if self.no_enlarge && self.resize.is_none() => Err(Error::usage("--no-enlarge only applies with --resize.")),
            _ if self.trim_density.is_some() && self.trim.is_none() => Err(Error::usage("--trim-density only applies with --trim.")),
            _ if self.key_region.is_some() && self.keys.is_empty() => Err(Error::usage("--key-region only applies with --key.")),
            _ if self.key_cut.is_some() && self.keys.is_empty() => Err(Error::usage("--key-cut only applies with --key.")),
            _ if self.key_spread.is_some() && self.keys.is_empty() => Err(Error::usage("--key-spread only applies with --key.")),
            _ if self.key_cut.is_some() && self.key_region.is_none() => Err(Error::usage("--key-cut needs --key-region, to say which edges to cut from.")),
            (Some(_), None) => Err(Error::usage("--fit only applies with --resize.")),
            (Some(_), Some(Resize { width: None, .. } | Resize { height: None, .. })) => {
                Err(Error::usage("--fit needs both sides in --resize (WxH); with one side the aspect ratio decides the other."))
            }
            _ => Ok(()),
        }
    }

    /// Whether this edits visible pixels (key, trim, resize or hard alpha was asked for), as
    /// opposed to only the colour under transparent ones.
    pub fn edits(&self) -> bool {
        self.trim.is_some() || self.resize.is_some() || self.hard_alpha.is_some() || !self.keys.is_empty()
    }

    pub fn apply(&self, mut rgba: RgbaImage, format: Format, enc: &Encoding) -> Result<Applied> {
        let mut changed = false;
        let mut trim = None;
        if let Some(threshold) = self.hard_alpha {
            changed |= harden_alpha(&mut rgba, threshold);
        }
        if let Some(bounds) = visible_bounds(&rgba).filter(|_| !self.keys.is_empty()) {
            // The region is measured once, on the content as it came in: --key-cut shrinks the
            // visible bounds, and a band recomputed after it would reach past the requested one.
            let keys = resolve_keys(&rgba, &self.keys, self.key_region);
            let band = Band::new(bounds, rgba.dimensions(), self.key_region);
            if let (Some(percent), Some(region)) = (self.key_cut, self.key_region) {
                changed |= key_cut(&mut rgba, &keys, region, bounds, percent);
            }
            changed |= key_out(&mut rgba, &keys, &band, self.key_spread);
        }
        if let Some(padding) = self.trim {
            let mut rect = visible_bounds(&rgba).ok_or_else(|| Error::other("--trim: the image has no visible pixels."))?;
            if let Some(density) = self.trim_density {
                rect = dense_bounds(&rgba, rect, density);
            }
            // No transparent border (every opaque image, JPEGs included): nothing to trim, and
            // padding alone would only add a border the input never had.
            if (rect.width, rect.height) != rgba.dimensions() {
                rect = Rect {
                    x: rect.x - i64::from(padding),
                    y: rect.y - i64::from(padding),
                    width: rect.width + 2 * padding,
                    height: rect.height + 2 * padding,
                };
                rgba = crop_padded(&rgba, rect);
                changed = true;
            }
            trim = Some(rect);
        }
        if let Some(size) = self.resize {
            if let Some(resized) = resize(&rgba, size, self.fit.unwrap_or(Fit::Inside), self.no_enlarge) {
                rgba = resized;
                changed = true;
                if self.hard_alpha.is_some() {
                    harden_alpha(&mut rgba, RESAMPLED_HARD_ALPHA);
                }
            }
        }
        if !self.no_bleed && bleeds(format, enc) {
            changed |= bleed(&mut rgba);
        }
        Ok(Applied { image: rgba, changed, trim })
    }
}

/// Formats that store the colour of fully transparent pixels as it is. Lossy WebP already
/// replaces it (libwebp's default), JPEG has no alpha, and in a palette PNG bled colours would
/// take palette entries from the visible ones.
fn bleeds(format: Format, enc: &Encoding) -> bool {
    match format {
        Format::Png => enc.colors.is_none(),
        Format::Webp => enc.lossless,
        Format::Jpeg => false,
    }
}

/// Alpha above `threshold` becomes 255, the rest 0. Returns whether any pixel changed.
fn harden_alpha(rgba: &mut RgbaImage, threshold: u8) -> bool {
    let mut changed = false;
    for pixel in rgba.pixels_mut() {
        let alpha = if pixel.0[3] > threshold { 255 } else { 0 };
        changed |= pixel.0[3] != alpha;
        pixel.0[3] = alpha;
    }
    changed
}

/// Bounding box of the pixels with alpha > FAINT_ALPHA; None if nothing is visible.
fn visible_bounds(rgba: &RgbaImage) -> Option<Rect> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, pixel) in rgba.enumerate_pixels() {
        if pixel.0[3] > FAINT_ALPHA {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
    }
    (x0 != u32::MAX).then(|| Rect { x: i64::from(x0), y: i64::from(y0), width: x1 - x0 + 1, height: y1 - y0 + 1 })
}

/// Shrink `rect` from the edges `density` names while the outermost row (or column) is sparse:
/// fewer visible pixels than `density.percent` of the fullest one. Specks left under a sprite
/// would otherwise become its bottom edge, and a sprite that stands on its bottom edge would float.
fn dense_bounds(rgba: &RgbaImage, rect: Rect, density: Density) -> Rect {
    let (x0, y0) = (rect.x as u32, rect.y as u32);
    let (x1, y1) = (x0 + rect.width, y0 + rect.height);
    let visible = |x: u32, y: u32| rgba.get_pixel(x, y).0[3] > FAINT_ALPHA;
    let rows: Vec<u32> = (y0..y1).map(|y| (x0..x1).filter(|&x| visible(x, y)).count() as u32).collect();
    let cols: Vec<u32> = (x0..x1).map(|x| (y0..y1).filter(|&y| visible(x, y)).count() as u32).collect();
    // Sparse: under `percent` of the fullest line. Compared in integers, so 15% of 7 is 1.05.
    let sparse = |line: &[u32], i: usize| {
        let full = line.iter().copied().max().unwrap_or(0);
        line[i] * 100 < u32::from(density.percent) * full
    };
    let (mut top, mut bottom, mut left, mut right) = (0, rows.len() - 1, 0, cols.len() - 1);
    if density.has(Edge::Bottom) {
        while bottom > top && sparse(&rows, bottom) {
            bottom -= 1;
        }
    }
    if density.has(Edge::Top) {
        while top < bottom && sparse(&rows, top) {
            top += 1;
        }
    }
    if density.has(Edge::Right) {
        while right > left && sparse(&cols, right) {
            right -= 1;
        }
    }
    if density.has(Edge::Left) {
        while left < right && sparse(&cols, left) {
            left += 1;
        }
    }
    Rect { x: rect.x + left as i64, y: rect.y + top as i64, width: (right - left + 1) as u32, height: (bottom - top + 1) as u32 }
}

/// The pixels `--key` may touch: the union of the region's bands, each along one edge of the
/// visible content and reaching out to the image's edge, or the whole image without a region.
struct Band {
    /// (x0, y0, x1, y1), end exclusive.
    rects: Vec<(u32, u32, u32, u32)>,
}

impl Band {
    fn new(bounds: Rect, (w, h): (u32, u32), region: Option<Region>) -> Band {
        let Some(region) = region else { return Band { rects: vec![(0, 0, w, h)] } };
        let depth = |side: u32, percent: u8| (u64::from(side) * u64::from(percent)).div_ceil(100) as u32;
        let (bx, by, bw, bh) = (bounds.x as u32, bounds.y as u32, bounds.width, bounds.height);
        let rects = EDGES
            .iter()
            .filter(|&&edge| region.bands[edge as usize] > 0)
            .map(|&edge| {
                let percent = region.bands[edge as usize];
                match edge {
                    Edge::Top => (0, 0, w, by + depth(bh, percent)),
                    Edge::Bottom => (0, by + bh - depth(bh, percent), w, h),
                    Edge::Left => (0, 0, bx + depth(bw, percent), h),
                    Edge::Right => (bx + bw - depth(bw, percent), 0, w, h),
                }
            })
            .collect();
        Band { rects }
    }

    fn contains(&self, x: u32, y: u32) -> bool {
        self.rects.iter().any(|&(x0, y0, x1, y1)| (x0..x1).contains(&x) && (y0..y1).contains(&y))
    }
}

/// `--key-cut`: from each edge of the region, inward through its band, make whole rows (top,
/// bottom) or columns (left, right) transparent while at least `percent` of their visible pixels
/// match `keys`. Stops at the first line that is mostly the object: below a boat's waterline, or
/// above a building's roofline. Returns whether any pixel changed.
fn key_cut(rgba: &mut RgbaImage, keys: &[Key], region: Region, bounds: Rect, percent: u8) -> bool {
    let (w, h) = rgba.dimensions();
    let (bx, by, bw, bh) = (bounds.x as u32, bounds.y as u32, bounds.width, bounds.height);
    let mut changed = false;
    for edge in EDGES.into_iter().filter(|&edge| region.bands[edge as usize] > 0) {
        let horizontal = matches!(edge, Edge::Top | Edge::Bottom);
        let side = if horizontal { bh } else { bw };
        let depth = ((u64::from(side) * u64::from(region.bands[edge as usize])).div_ceil(100) as u32).min(side);
        // Lines from the edge inward: row or column indices.
        let lines: Vec<u32> = match edge {
            Edge::Top => (by..by + depth).collect(),
            Edge::Bottom => (by + bh - depth..by + bh).rev().collect(),
            Edge::Left => (bx..bx + depth).collect(),
            Edge::Right => (bx + bw - depth..bx + bw).rev().collect(),
        };
        let pixels_of = |line: u32| -> Vec<(u32, u32)> { if horizontal { (0..w).map(|x| (x, line)).collect() } else { (0..h).map(|y| (line, y)).collect() } };
        for line in lines {
            let pixels = pixels_of(line);
            let (visible, keyed) = pixels.iter().map(|&(x, y)| rgba.get_pixel(x, y).0).filter(|p| p[3] > FAINT_ALPHA).fold((0u32, 0u32), |(v, k), p| {
                (v + 1, k + u32::from(keys.iter().any(|key| key.matches(p))))
            });
            if visible > 0 && keyed * 100 < u32::from(percent) * visible {
                break;
            }
            for (x, y) in pixels {
                let alpha = &mut rgba.get_pixel_mut(x, y).0[3];
                changed |= *alpha != 0;
                *alpha = 0;
            }
        }
    }
    changed
}

/// Islands of visible pixels smaller than this share (per mille) of the largest one count as
/// leftover ground when --key removed pixels around them: spray and specks in painted water.
const ISLAND_PER_MILLE: usize = 10;

/// `--key`: make pixels matching `keys` fully transparent where they connect (4-neighbour) to the
/// transparent background or the image's border, within the region's bands. Key
/// colours enclosed by the sprite, like a blue stripe inside a hull's outline, are never reached.
/// Then the small islands the removed ground leaves behind go too (see `clear_islands`).
/// Returns whether any pixel changed.
fn key_out(rgba: &mut RgbaImage, keys: &[Key], band: &Band, spread: Option<u8>) -> bool {
    let (w, h) = rgba.dimensions();
    let index = |x: u32, y: u32| (y * w + x) as usize;
    let background = |p: [u8; 4]| p[3] <= FAINT_ALPHA;
    let mut seen = vec![false; (w * h) as usize];
    let mut removed = vec![false; (w * h) as usize];
    // Each entry carries the colour of the removed pixel it was reached from, for --key-spread.
    let mut queue: std::collections::VecDeque<(u32, u32, Option<[u8; 3]>)> = std::collections::VecDeque::new();
    for y in 0..h {
        for x in (0..w).filter(|&x| band.contains(x, y)) {
            if background(rgba.get_pixel(x, y).0) || x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                seen[index(x, y)] = true;
                queue.push_back((x, y, None));
            }
        }
    }
    let mut changed = false;
    while let Some((x, y, from)) = queue.pop_front() {
        let pixel = rgba.get_pixel_mut(x, y);
        let [r, g, b, _] = pixel.0;
        let solid = !background(pixel.0);
        if solid {
            let near = spread.zip(from).is_some_and(|(tolerance, f)| [r, g, b].iter().zip(f).all(|(&c, k)| c.abs_diff(k) <= tolerance));
            if !near && !keys.iter().any(|k| k.matches(pixel.0)) {
                // With --key-spread, a neighbour of a closer colour may still reach it later.
                seen[index(x, y)] = spread.is_none();
                continue;
            }
            pixel.0[3] = 0;
            removed[index(x, y)] = true;
            changed = true;
        }
        let from = solid.then_some([r, g, b]);
        let neighbours = [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)];
        for (nx, ny) in neighbours {
            if nx < w && ny < h && band.contains(nx, ny) && !seen[index(nx, ny)] {
                seen[index(nx, ny)] = true;
                queue.push_back((nx, ny, from));
            }
        }
    }
    if changed {
        clear_islands(rgba, &removed, band);
    }
    changed
}

/// Make transparent every island of visible pixels (8-connected) that lies within the band, touches
/// a pixel --key removed, and is smaller than ISLAND_PER_MILLE of the largest island. Ground colours
/// that no key matched, like foam and spray on painted water, survive keying as such islands; they
/// would widen the trim and float under the sprite. Detached parts of the object that never touched
/// the removed ground are left alone.
fn clear_islands(rgba: &mut RgbaImage, removed: &[bool], band: &Band) {
    let (w, h) = rgba.dimensions();
    let visible: Vec<bool> = rgba.pixels().map(|p| p.0[3] > FAINT_ALPHA).collect();
    let mut island = vec![u32::MAX; visible.len()];
    // Per island: its pixels, and whether it stays within the band and touches removed ground.
    let mut islands: Vec<(Vec<usize>, bool, bool)> = Vec::new();
    for start in 0..visible.len() {
        if !visible[start] || island[start] != u32::MAX {
            continue;
        }
        let id = islands.len() as u32;
        let (mut pixels, mut inside, mut touches) = (vec![start], true, false);
        island[start] = id;
        let mut next = 0;
        while let Some(&i) = pixels.get(next) {
            next += 1;
            let (x, y) = ((i as u32 % w) as i64, (i as u32 / w) as i64);
            inside &= band.contains(x as u32, y as u32);
            for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= i64::from(w) || ny >= i64::from(h) {
                    continue;
                }
                let j = (ny as u32 * w + nx as u32) as usize;
                touches |= removed[j];
                if visible[j] && island[j] == u32::MAX {
                    island[j] = id;
                    pixels.push(j);
                }
            }
        }
        islands.push((pixels, inside, touches));
    }
    let largest = islands.iter().map(|(pixels, _, _)| pixels.len()).max().unwrap_or(0);
    let pixels: &mut [u8] = &mut *rgba;
    for (members, inside, touches) in islands {
        if inside && touches && members.len() * 1000 < largest * ISLAND_PER_MILLE {
            for i in members {
                pixels[i * 4 + 3] = 0;
            }
        }
    }
}

/// Copy `rect` out of `rgba`; parts outside the image become transparent. Pixels are copied, not
/// blended, so hidden colours survive for --no-bleed.
fn crop_padded(rgba: &RgbaImage, rect: Rect) -> RgbaImage {
    let mut out = RgbaImage::new(rect.width, rect.height);
    imageops::replace(&mut out, rgba, -rect.x, -rect.y);
    out
}

fn scale(side: u32, to: u32, from: u32) -> u32 {
    ((f64::from(side) * f64::from(to) / f64::from(from)).round() as u32).clamp(1, MAX_SIDE)
}

/// The largest size with `rgba`'s aspect ratio that fits in `width`x`height`.
fn inside(from: (u32, u32), width: u32, height: u32) -> (u32, u32) {
    let (w, h) = from;
    if u64::from(w) * u64::from(height) >= u64::from(width) * u64::from(h) {
        (width, scale(h, width, w).min(height))
    } else {
        (scale(w, height, h).min(width), height)
    }
}

/// None when the image already has the requested size. With `no_enlarge` the scale factor is at
/// most 1: `inside` keeps a smaller image as it is, `cover` still crops to the box's aspect ratio,
/// `contain` pads the unscaled image, and `fill` caps each side on its own.
fn resize(rgba: &RgbaImage, size: Resize, fit: Fit, no_enlarge: bool) -> Option<RgbaImage> {
    let (w, h) = rgba.dimensions();
    let (width, height) = match (size.width, size.height) {
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => (width, scale(h, width, w)),
        (None, Some(height)) => (scale(w, height, h), height),
        (None, None) => return None,
    };
    let capped = |(iw, ih): (u32, u32)| if no_enlarge && (iw > w || ih > h) { (w, h) } else { (iw, ih) };
    let out = match fit {
        Fit::Fill if no_enlarge => resample(rgba, width.min(w), height.min(h)),
        Fit::Fill => resample(rgba, width, height),
        Fit::Inside => {
            let (iw, ih) = capped(inside((w, h), width, height));
            resample(rgba, iw, ih)
        }
        Fit::Contain => {
            let (iw, ih) = capped(inside((w, h), width, height));
            let scaled = resample(rgba, iw, ih);
            let mut canvas = RgbaImage::new(width, height);
            imageops::replace(&mut canvas, &scaled, i64::from((width - iw) / 2), i64::from((height - ih) / 2));
            canvas
        }
        Fit::Cover => {
            // Crop the source to the box's aspect ratio first, then scale: no rounding overflow.
            let (cw, ch) = if u64::from(w) * u64::from(height) > u64::from(width) * u64::from(h) {
                (scale(h, width, height).min(w), h)
            } else {
                (w, scale(w, height, width).min(h))
            };
            let cropped = imageops::crop_imm(rgba, (w - cw) / 2, (h - ch) / 2, cw, ch).to_image();
            if no_enlarge && (width > cw || height > ch) {
                cropped
            } else {
                resample(&cropped, width, height)
            }
        }
    };
    (out != *rgba).then_some(out)
}

/// Resample with premultiplied alpha, so the colour under transparent pixels can't bleed into the
/// edges. (imageops::resize assumes premultiplied input and doesn't premultiply itself.)
pub fn resample(rgba: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    if rgba.dimensions() == (width, height) {
        return rgba.clone();
    }
    if rgba.pixels().all(|p| p.0[3] == 255) {
        return imageops::resize(rgba, width, height, FILTER);
    }
    let premultiplied: ImageBuffer<Rgba<f32>, Vec<f32>> = ImageBuffer::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0.map(|c| f32::from(c) / 255.0);
        Rgba([r * a, g * a, b * a, a])
    });
    let resized = imageops::resize(&premultiplied, width, height, FILTER);
    RgbaImage::from_fn(width, height, |x, y| {
        let [r, g, b, a] = resized.get_pixel(x, y).0;
        let a = a.clamp(0.0, 1.0);
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let colour = |c: f32| if a > 0.0 { byte(c / a) } else { 0 };
        Rgba([colour(r), colour(g), colour(b), byte(a)])
    })
}

/// Give each fully transparent pixel the colour of the nearest visible ones (alpha stays 0), so
/// filtering that ignores alpha, like a game engine's texture sampling, blends towards the edge
/// colour instead of whatever the image stored there. Grows outwards one ring of pixels per wave,
/// each pixel averaging its already-coloured 8-neighbours. Returns whether any pixel changed.
fn bleed(rgba: &mut RgbaImage) -> bool {
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    let mut known: Vec<bool> = rgba.pixels().map(|p| p.0[3] > 0).collect();
    if known.iter().all(|&k| k) || !known.iter().any(|&k| k) {
        return false;
    }
    let neighbours = |i: usize| {
        let (x, y) = ((i % w) as isize, (i / w) as isize);
        (-1..=1isize)
            .flat_map(move |dy| (-1..=1isize).map(move |dx| (x + dx, y + dy)))
            .filter(move |&(nx, ny)| (nx, ny) != (x, y) && nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h)
            .map(move |(nx, ny)| ny as usize * w + nx as usize)
    };
    let mut queued = known.clone();
    let mut wave: Vec<usize> = (0..w * h).filter(|&i| !known[i] && neighbours(i).any(|j| known[j])).collect();
    for &i in &wave {
        queued[i] = true;
    }
    let mut changed = false;
    let pixels: &mut [u8] = &mut *rgba;
    while !wave.is_empty() {
        let colours: Vec<[u8; 3]> = wave
            .iter()
            .map(|&i| {
                let (mut sum, mut n) = ([0u32; 3], 0u32);
                for j in neighbours(i).filter(|&j| known[j]) {
                    for c in 0..3 {
                        sum[c] += u32::from(pixels[j * 4 + c]);
                    }
                    n += 1;
                }
                sum.map(|s| ((s + n / 2) / n) as u8)
            })
            .collect();
        for (&i, colour) in wave.iter().zip(&colours) {
            changed |= pixels[i * 4..i * 4 + 3] != colour[..];
            pixels[i * 4..i * 4 + 3].copy_from_slice(colour);
            known[i] = true;
        }
        let mut next = Vec::new();
        for &i in &wave {
            for j in neighbours(i) {
                if !queued[j] {
                    queued[j] = true;
                    next.push(j);
                }
            }
        }
        wave = next;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 40x30 sprite: an opaque red 10x6 block at (12, 8) on transparent pixels that hide a dark
    /// colour, like the backend's vignette under transparent backgrounds.
    fn sprite() -> RgbaImage {
        RgbaImage::from_fn(40, 30, |x, y| {
            if (12..22).contains(&x) && (8..14).contains(&y) {
                Rgba([230, 40, 30, 255])
            } else {
                Rgba([10, 10, 20, 0])
            }
        })
    }

    fn apply(t: Transform, image: RgbaImage) -> Applied {
        t.apply(image, Format::Png, &Encoding::default()).unwrap()
    }

    #[test]
    fn parses_resize_fit_and_trim() {
        assert_eq!(Resize::parse("512x256").unwrap(), Resize { width: Some(512), height: Some(256) });
        assert_eq!(Resize::parse("400x").unwrap(), Resize { width: Some(400), height: None });
        assert_eq!(Resize::parse("X300").unwrap(), Resize { width: None, height: Some(300) });
        for bad in ["x", "400", "0x10", "9000x", "ax2", "-1x5", ""] {
            assert!(Resize::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(Fit::parse("COVER").unwrap(), Fit::Cover);
        assert!(Fit::parse("crop").is_err());
        assert_eq!(parse_trim_padding("8").unwrap(), 8);
        assert!(parse_trim_padding("-1").is_err() && parse_trim_padding("5000").is_err());
        let fit_only = Transform { fit: Some(Fit::Cover), ..Default::default() };
        assert!(fit_only.check().unwrap_err().message.contains("--resize"));
        let one_side = Transform { fit: Some(Fit::Cover), resize: Some(Resize::parse("400x").unwrap()), ..Default::default() };
        assert!(one_side.check().unwrap_err().message.contains("both sides"));
    }

    #[test]
    fn trims_to_visible_pixels_with_padding() {
        let out = apply(Transform { trim: Some(0), ..Default::default() }, sprite());
        assert_eq!(out.image.dimensions(), (10, 6));
        assert_eq!(out.trim, Some(Rect { x: 12, y: 8, width: 10, height: 6 }));
        assert!(out.image.pixels().all(|p| p.0 == [230, 40, 30, 255]));

        // Padding may reach past the edges; new pixels are transparent.
        let out = apply(Transform { trim: Some(14), no_bleed: true, ..Default::default() }, sprite());
        assert_eq!(out.trim, Some(Rect { x: -2, y: -6, width: 38, height: 34 }));
        assert_eq!(out.image.get_pixel(0, 0).0, [0, 0, 0, 0]);
        assert_eq!(out.image.get_pixel(2, 6).0, [10, 10, 20, 0], "--no-bleed copies hidden colours as they are");
        assert_eq!(out.image.get_pixel(14, 14).0, [230, 40, 30, 255]);

        let empty = RgbaImage::new(4, 4);
        assert!(Transform { trim: Some(0), ..Default::default() }.apply(empty, Format::Png, &Encoding::default()).is_err());
        // No transparent border: padding must not grow the image (a JPEG would get a white frame).
        let opaque = RgbaImage::from_pixel(4, 4, Rgba([1, 2, 3, 255]));
        for padding in [0, 4] {
            let out = apply(Transform { trim: Some(padding), ..Default::default() }, opaque.clone());
            assert!(!out.changed && out.image.dimensions() == (4, 4), "padding {padding}");
            assert_eq!(out.trim, Some(Rect { x: 0, y: 0, width: 4, height: 4 }));
        }
        let framed = RgbaImage::from_fn(6, 6, |x, y| Rgba([9, 9, 9, if x == 0 || y == 5 || (x, y) == (5, 0) { 255 } else { 0 }]));
        assert_eq!(apply(Transform { trim: Some(4), ..Default::default() }, framed).image.dimensions(), (6, 6), "visible pixels reach every edge");
    }

    #[test]
    fn trim_ignores_faint_specks_and_hard_alpha_makes_edges_binary() {
        // Like generated sprites: "solid" pixels at alpha 253, a soft rim, and faint specks far out.
        let hazy = || {
            let mut image = sprite();
            for (x, y) in [(1, 1), (38, 28)] {
                image.put_pixel(x, y, Rgba([10, 10, 20, 9]));
            }
            for pixel in image.pixels_mut().filter(|p| p.0[3] == 255) {
                pixel.0[3] = 253;
            }
            for x in 12..22 {
                image.put_pixel(x, 7, Rgba([230, 40, 30, 90]));
            }
            image
        };
        let out = apply(Transform { trim: Some(0), ..Default::default() }, hazy());
        assert_eq!(out.trim, Some(Rect { x: 12, y: 7, width: 10, height: 7 }), "specks at alpha 9 don't count");
        assert_eq!(out.image.get_pixel(0, 0).0[3], 90, "soft edges stay soft without --hard-alpha");

        let hard = Transform { hard_alpha: Some(FAINT_ALPHA), trim: Some(0), ..Default::default() };
        let out = apply(hard, hazy());
        assert!(out.image.pixels().all(|p| p.0[3] == 255), "rim at 90 and body at 253 become solid");
        assert_eq!(apply(Transform { hard_alpha: Some(100), trim: Some(0), ..Default::default() }, hazy()).image.height(), 6);

        let resized = Transform { hard_alpha: Some(FAINT_ALPHA), resize: Some(Resize::parse("15x").unwrap()), ..Default::default() };
        let out = apply(resized, hazy());
        assert!(out.image.pixels().all(|p| p.0[3] == 0 || p.0[3] == 255), "resampling must not soften hard edges");
        assert!(out.image.pixels().any(|p| p.0[3] == 255));
        assert!(Transform { hard_alpha: Some(FAINT_ALPHA), ..Default::default() }.edits());
        assert_eq!((parse_hard_alpha("16").unwrap(), parse_hard_alpha("255").is_err()), (16, true));
    }

    /// A 40x30 "boat" in painted water: a dark outlined red hull (rows 8-20) with a blue stripe
    /// inside the outline, bright water (rows 17-27) around its bottom, and white foam specks in it.
    fn boat() -> RgbaImage {
        const WATER: [u8; 4] = [40, 90, 220, 255];
        RgbaImage::from_fn(40, 30, |x, y| {
            let hull = (10..30).contains(&x) && (8..=20).contains(&y);
            let outline = hull && (x == 10 || x == 29 || y == 8 || y == 20);
            Rgba(match (x, y) {
                _ if outline => [30, 30, 30, 255],
                (_, 14) if hull => WATER,
                _ if hull => [200, 40, 40, 255],
                (6, 24) | (20, 26) | (33, 26) => [250, 250, 250, 255],
                _ if (4..36).contains(&x) && (17..=27).contains(&y) => WATER,
                _ => [0, 0, 0, 0],
            })
        })
    }

    fn alpha(t: &Transform, image: RgbaImage, points: &[(u32, u32)]) -> Vec<u8> {
        let out = apply(Transform { no_bleed: true, ..t.clone() }, image).image;
        points.iter().map(|&(x, y)| out.get_pixel(x, y).0[3]).collect()
    }

    #[test]
    fn keys_out_painted_ground_but_not_paint_inside_the_outline() {
        let blue = Transform { keys: vec![Key::Named(Colour::Blue)], ..Default::default() };
        // Water beside the hull, stripe inside it, foam speck, hull.
        let points = [(5, 18), (20, 14), (6, 24), (15, 12)];
        assert_eq!(alpha(&blue, boat(), &points), [0, 255, 0, 255], "the speck is an island left in removed water");
        // auto, sampling along the bottom, keys the same pixels.
        let bottom = Some(Region::parse("bottom:50%").unwrap());
        let auto = Transform { keys: vec![Key::Auto { tolerance: 32 }], key_region: bottom, ..Default::default() };
        assert_eq!(alpha(&auto, boat(), &points), [0, 255, 0, 255]);
        assert_eq!(sample_edges(&boat(), bottom), vec![[40, 90, 220]], "one foam pixel among the water isn't sampled");
        assert!(sample_edges(&boat(), None).contains(&[30, 30, 30]), "without a region all edges are sampled, the hull's top too");

        // Only the bottom 30% of the visible content (rows 22-27 of 8-27).
        let band = Transform { key_region: Some(Region::parse("bottom:30%").unwrap()), ..blue.clone() };
        assert_eq!(alpha(&band, boat(), &[(5, 21), (5, 22)]), [255, 0]);

        let trim = Transform { trim: Some(0), ..blue.clone() };
        assert_eq!(apply(trim.clone(), boat()).trim, Some(Rect { x: 10, y: 8, width: 20, height: 13 }));
        // --key-cut clears the rows that are mostly water (21-27) before keying.
        let cut = Transform { key_cut: Some(KEY_CUT), key_region: bottom, trim: None, ..blue.clone() };
        assert_eq!(alpha(&cut, boat(), &[(20, 14), (15, 21), (5, 18)]), [255, 0, 0]);
        // A big detached part that never touched removed ground stays.
        let mut with_flag = boat();
        for (x, y) in [(2, 2), (3, 2), (2, 3), (3, 3)] {
            with_flag.put_pixel(x, y, Rgba([30, 90, 220, 255]));
        }
        assert_eq!(alpha(&Transform { key_region: Some(Region::parse("bottom:30%").unwrap()), ..blue.clone() }, with_flag, &[(2, 2)]), [255]);

        assert!(Transform { keys: vec![Key::Named(Colour::White)], ..Default::default() }.edits());
        let no_keys = |t: Transform| t.check().unwrap_err().message;
        assert!(no_keys(Transform { key_cut: Some(40), ..Default::default() }).contains("--key"));
        assert!(no_keys(Transform { key_cut: Some(40), ..blue.clone() }).contains("--key-region"));
        assert!(no_keys(Transform { key_region: Some(Region::parse("top:10%").unwrap()), ..Default::default() }).contains("--key"));
        assert!(no_keys(Transform { trim_density: Some(Density::parse("0.1").unwrap()), ..Default::default() }).contains("--trim"));
    }

    /// A 60x40 opaque picture: a sky gradient with a small white cloud, a red house with a dark
    /// outline and a sky-blue window, standing on green ground that spans the whole width.
    fn house() -> RgbaImage {
        RgbaImage::from_fn(60, 40, |x, y| {
            let house = (25..40).contains(&x) && (12..32).contains(&y);
            let outline = house && (x == 25 || x == 39 || y == 12);
            Rgba(match (x, y) {
                _ if outline => [30, 20, 20, 255],
                _ if house && (30..34).contains(&x) && (16..20).contains(&y) => [110, 160, 235, 255],
                _ if house => [200, 50, 40, 255],
                (_, 30..) => [60, 150, 50, 255],
                _ if (10..16).contains(&x) && (5..8).contains(&y) => [245, 245, 250, 255],
                _ => [100 + y as u8, 160, 235, 255],
            })
        })
    }

    #[test]
    fn keys_out_sky_from_the_top() {
        let sky = Transform { keys: vec![Key::Auto { tolerance: 32 }], key_region: Some(Region::parse("top:80%").unwrap()), ..Default::default() };
        // Sky beside the house, house, window, ground (its top rows are in the band, but green
        // was never sampled from the top).
        let points = [(5, 20), (50, 28), (27, 20), (31, 17), (5, 35), (5, 30)];
        assert_eq!(alpha(&sky, house(), &points), [0, 0, 255, 255, 255, 255]);
        let trimmed = apply(Transform { trim: Some(0), ..sky.clone() }, house());
        assert_eq!(trimmed.trim, Some(Rect { x: 0, y: 5, width: 60, height: 35 }), "the cloud is left");
        // The cloud wasn't in the sampled strip, and it's too big to count as a speck: name it.
        let with_white = Transform { keys: vec![Key::Auto { tolerance: 32 }, Key::Named(Colour::White)], trim: Some(0), ..sky };
        assert_eq!(apply(with_white, house()).trim, Some(Rect { x: 0, y: 12, width: 60, height: 28 }), "down to the roof");
    }

    #[test]
    fn key_spread_follows_a_gradient_and_stops_at_outlines() {
        // A sky from blue (top) to gold (row 29): each row a small step, far apart overall.
        let gradient = |y: u32| [(60 + y * 6) as u8, (140 + y * 2) as u8, (235 - y * 6) as u8];
        let picture = RgbaImage::from_fn(60, 40, |x, y| {
            let house = (25..40).contains(&x) && (12..32).contains(&y);
            let outline = house && (x == 25 || x == 39 || y == 12);
            Rgba(match (x, y) {
                _ if outline => [30, 20, 20, 255],
                _ if house => [200, 50, 40, 255],
                (_, 30..) => [60, 150, 50, 255],
                _ => { let [r, g, b] = gradient(y); [r, g, b, 255] }
            })
        });
        let top = Transform { keys: vec![Key::Auto { tolerance: 16 }], key_region: Some(Region::parse("top:80%").unwrap()), ..Default::default() };
        let points = [(5, 2), (5, 25), (27, 20), (5, 35)];
        assert_eq!(alpha(&top, picture.clone(), &points), [0, 255, 255, 255], "only the sampled blue goes");
        let spread = Transform { key_spread: Some(12), ..top };
        assert_eq!(alpha(&spread, picture, &points), [0, 0, 255, 255], "the gold end goes too; house and ground stay");
        assert_eq!(parse_key_spread("24").unwrap(), 24);
        assert!(parse_key_spread("0").is_err() && parse_key_spread("200").is_err());
    }

    #[test]
    fn key_cut_keeps_the_region_it_was_given() {
        // 20x100: blue rows 0-39, red below, and one blue pixel on the border at row 60. top:50%
        // ends at row 49 of the content; cutting the blue rows must not move that band down.
        let picture = RgbaImage::from_fn(20, 100, |x, y| Rgba(if y < 40 || (x, y) == (0, 60) { [40, 90, 220, 255] } else { [200, 40, 40, 255] }));
        let top = Transform { keys: vec![Key::Named(Colour::Blue)], key_region: Some(Region::parse("top:50%").unwrap()), no_bleed: true, ..Default::default() };
        let cut = Transform { key_cut: Some(KEY_CUT), ..top.clone() };
        for t in [top, cut] {
            let out = apply(t.clone(), picture.clone()).image;
            assert_eq!((out.get_pixel(5, 20).0[3], out.get_pixel(0, 60).0[3]), (0, 255), "{t:?}");
        }
    }

    #[test]
    fn key_cut_works_from_any_edge() {
        // A wide building: rows of sky above the roof go in whole lines from the top.
        let wide = RgbaImage::from_fn(60, 40, |x, y| Rgba(if (5..55).contains(&x) && y >= 12 { [200, 50, 40, 255] } else { [100, 160, 235, 255] }));
        let cut = Transform { keys: vec![Key::Named(Colour::Blue)], key_region: Some(Region::parse("top:50%").unwrap()), key_cut: Some(KEY_CUT), no_bleed: true, ..Default::default() };
        let out = apply(cut.clone(), wide.clone()).image;
        assert!((0..60).all(|x| out.get_pixel(x, 11).0[3] == 0), "a sky row above the roof");
        assert_eq!((out.get_pixel(30, 12).0[3], out.get_pixel(2, 12).0[3]), (255, 0), "the roof row stays; sky beside it is keyed");
        // From the left, columns of sky go until the wall.
        let left = Transform { key_region: Some(Region::parse("left:20%").unwrap()), ..cut };
        let out = apply(left, wide).image;
        assert_eq!((out.get_pixel(4, 30).0[3], out.get_pixel(5, 30).0[3]), (0, 255));
    }

    #[test]
    fn trim_density_drops_sparse_rows_under_the_sprite() {
        let mut specks = sprite();
        for (x, y) in [(13, 20), (20, 22)] {
            specks.put_pixel(x, y, Rgba([230, 40, 30, 255]));
        }
        let trim = Transform { trim: Some(0), ..Default::default() };
        assert_eq!(apply(trim.clone(), specks.clone()).trim, Some(Rect { x: 12, y: 8, width: 10, height: 15 }), "specks would make it float");
        let dense = Transform { trim_density: Some(Density::parse("0.15").unwrap()), ..trim };
        assert_eq!(apply(dense, specks).trim, Some(Rect { x: 12, y: 8, width: 10, height: 6 }));
    }

    #[test]
    fn parses_keys_regions_and_densities() {
        assert_eq!(Key::parse("Blue").unwrap(), Key::Named(Colour::Blue));
        assert_eq!(Key::parse("grey").unwrap(), Key::Named(Colour::Gray));
        assert_eq!(Key::parse("auto").unwrap(), Key::Auto { tolerance: 32 });
        assert_eq!(Key::parse("auto:48").unwrap(), Key::Auto { tolerance: 48 });
        assert_eq!(Key::parse("#3070c0").unwrap(), Key::Rgb { rgb: [0x30, 0x70, 0xc0], tolerance: 32 });
        assert_eq!(Key::parse("3070C0:0").unwrap(), Key::Rgb { rgb: [0x30, 0x70, 0xc0], tolerance: 0 });
        for bad in ["water", "sea", "#3070c", "#3070c0:300", "#zz70c0", "blue:10"] {
            assert!(Key::parse(bad).is_err(), "{bad}");
        }
        let key = Key::parse("#3070c0:8").unwrap();
        assert!(key.matches([0x38, 0x68, 0xc0, 255]) && !key.matches([0x39, 0x70, 0xc0, 255]));
        let is = |colour: Colour, rgb: [u8; 3]| Key::Named(colour).matches([rgb[0], rgb[1], rgb[2], 255]);
        assert!(is(Colour::Blue, [20, 40, 90]), "dark wave shading is still blue");
        assert!(is(Colour::Blue, [40, 90, 220]) && !is(Colour::Cyan, [40, 90, 220]));
        assert!(is(Colour::White, [230, 240, 250]) && !is(Colour::White, [200, 60, 60]));
        assert!(is(Colour::Gray, [128, 128, 128]) && is(Colour::Black, [10, 10, 10]) && !is(Colour::Blue, [128, 128, 140]));
        assert!(is(Colour::Green, [40, 160, 40]) && is(Colour::Orange, [120, 70, 30]), "browns are orange");
        assert!(is(Colour::Red, [200, 20, 40]) && is(Colour::Red, [200, 30, 20]), "red wraps around 0 degrees");

        assert_eq!(Region::parse("bottom:30%").unwrap(), Region { bands: [0, 30, 0, 0] });
        assert_eq!(Region::parse("left:0.25").unwrap(), Region { bands: [0, 0, 25, 0] });
        assert_eq!(Region::parse("top:40%,left:15%,right:15%").unwrap(), Region { bands: [40, 0, 15, 15] });
        assert_eq!(Region::parse("all:20%").unwrap(), Region { bands: [20; 4] });
        for bad in ["bottom", "middle:30%", "bottom:0", "bottom:150%", "top:40%,"] {
            assert!(Region::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(Density::parse("0.15").unwrap(), Density { edges: [false, true, false, false], percent: 15 });
        assert_eq!(Density::parse("bottom,top:15%").unwrap(), Density { edges: [true, true, false, false], percent: 15 });
        assert_eq!(Density::parse("all:0.2").unwrap().edges, [true; 4]);
        for bad in ["0", "1.5", "side:0.1", "bottom:"] {
            assert!(Density::parse(bad).is_err(), "{bad}");
        }
        assert_eq!((parse_key_cut("0.6").unwrap(), parse_key_cut("60%").unwrap()), (60, 60));
    }

    #[test]
    fn resizes_by_one_side_and_by_fit() {
        let wide = RgbaImage::from_pixel(1672, 941, Rgba([50, 100, 150, 255]));
        let size = |resize: &str, fit| {
            let t = Transform { resize: Some(Resize::parse(resize).unwrap()), fit, ..Default::default() };
            apply(t, wide.clone()).image.dimensions()
        };
        assert_eq!(size("400x", None), (400, 225));
        assert_eq!(size("x225", None), (400, 225));
        assert_eq!(size("1536x1024", None), (1536, 864), "inside is the default");
        assert_eq!(size("1536x1024", Some(Fit::Cover)), (1536, 1024));
        assert_eq!(size("1536x1024", Some(Fit::Contain)), (1536, 1024));
        assert_eq!(size("1536x1024", Some(Fit::Fill)), (1536, 1024));

        let t = Transform { resize: Some(Resize::parse("100x100").unwrap()), fit: Some(Fit::Contain), no_bleed: true, ..Default::default() };
        let boxed = apply(t, wide.clone()).image;
        assert_eq!(boxed.get_pixel(50, 0).0[3], 0, "contain pads with transparency");
        assert_eq!(boxed.get_pixel(50, 50).0, [50, 100, 150, 255]);

        // --no-enlarge: shrinking is unchanged, growing is capped at the original scale.
        let capped = |resize: &str, fit| {
            let t = Transform { resize: Some(Resize::parse(resize).unwrap()), fit, no_enlarge: true, ..Default::default() };
            let out = apply(t, wide.clone());
            (out.image.dimensions(), out.changed)
        };
        assert_eq!(capped("400x", None), ((400, 225), true));
        assert_eq!(capped("3000x", None), ((1672, 941), false), "already smaller: left as it is");
        assert_eq!(capped("2000x2000", None), ((1672, 941), false));
        assert_eq!(capped("2000x2000", Some(Fit::Cover)), ((941, 941), true), "still cropped to the box's aspect, not scaled up");
        assert_eq!(capped("2000x2000", Some(Fit::Contain)), ((2000, 2000), true), "padded around the unscaled image");
        assert_eq!(capped("2000x500", Some(Fit::Fill)), ((1672, 500), true), "each side capped on its own");
        let t = Transform { no_enlarge: true, ..Default::default() };
        assert!(t.check().unwrap_err().message.contains("--resize"));

        let same = Transform { resize: Some(Resize::parse("1672x").unwrap()), ..Default::default() };
        assert!(!apply(same, wide).changed, "already the right size");
    }

    #[test]
    fn resize_ignores_colour_under_transparent_pixels() {
        let t = Transform { resize: Some(Resize::parse("20x").unwrap()), no_bleed: true, ..Default::default() };
        let small = apply(t, sprite()).image;
        for pixel in small.pixels().filter(|p| p.0[3] > 0) {
            let [r, g, b, _] = pixel.0;
            assert!(r > 200 && g < 60 && b < 50, "dark hidden colour leaked into the edge: {:?}", pixel.0);
        }
    }

    #[test]
    fn bleeds_edge_colours_only_where_the_format_keeps_them() {
        let out = apply(Transform::default(), sprite());
        assert!(out.changed);
        assert_eq!(out.image.get_pixel(0, 0).0, [230, 40, 30, 0], "far corner takes the nearest visible colour");
        assert_eq!(out.image.get_pixel(15, 10).0, [230, 40, 30, 255], "visible pixels are untouched");
        assert!(!apply(Transform::default(), out.image).changed, "bleeding twice changes nothing");

        let unchanged = |format, enc: Encoding| !Transform::default().apply(sprite(), format, &enc).unwrap().changed;
        assert!(unchanged(Format::Webp, Encoding::default()), "lossy WebP replaces hidden colours itself");
        assert!(unchanged(Format::Png, Encoding { colors: Some(16), ..Default::default() }));
        assert!(unchanged(Format::Jpeg, Encoding::default()));
        assert!(!unchanged(Format::Webp, Encoding { lossless: true, ..Default::default() }));
        assert!(!apply(Transform { no_bleed: true, ..Default::default() }, sprite()).changed);
    }
}
