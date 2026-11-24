//! Texture atlases: pack many small images into a few pages, with a TexturePacker "hash" JSON per
//! page (the format PixiJS, Phaser and most engines load). Packing is MaxRects, best of several
//! tries: every placement heuristic (best short side fit, best area fit, bottom-left, contact
//! point) with every sort order (longest side, area, height, width, perimeter) fills full pages.
//! Of the layouts with the fewest pages, the 3 with the least total page area each have their last
//! page shrunk to a smaller sheet, and the squarest of those within 10% of the least area wins
//! (the same rule the shrinking uses; the first one tried on a tie). Every step has a fixed order
//! and no threads, so the same sprites and options always give the same pages. Identical images
//! (after trimming) are stored once, with every name's frame pointing at the shared pixels. With
//! `rotate` (off by default), every heuristic also tries each non-square cell turned 90° and keeps
//! the better spot, upright on a tie; turned pixels are stored the way TexturePacker's loaders
//! expect (see `Frame`).
use crate::error::{Error, Result};
use image::{imageops, RgbaImage};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

pub const DEFAULT_MAX_SIZE: u32 = 2048;
pub const MAX_PAGE_SIDE: u32 = 16_384;
pub const DEFAULT_PADDING: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasOptions {
    /// Cut fully transparent borders; the frame keeps its original size for alignment.
    pub trim: bool,
    /// Transparent pixels between sprites and around the page's edges.
    pub padding: u32,
    /// Edge pixels repeated outward around each sprite, outside its frame, against bleeding.
    pub extrude: u32,
    pub max_size: u32,
    /// Round page sides up to powers of two.
    pub pot: bool,
    /// Let packing store a sprite turned 90° clockwise where that packs tighter (`"rotated": true`).
    pub rotate: bool,
}

impl Default for AtlasOptions {
    fn default() -> Self {
        AtlasOptions { trim: false, padding: DEFAULT_PADDING, extrude: 0, max_size: DEFAULT_MAX_SIZE, pot: false, rotate: false }
    }
}

pub struct Sprite {
    pub name: String,
    pub image: RgbaImage,
}

/// Where a sprite is on its page, in TexturePacker's terms. A rotated sprite is stored turned 90°
/// clockwise: `x`, `y` are the top-left corner of the `height` x `width` area it takes on the page,
/// and `width` x `height` stay the sprite's own (trimmed, unrotated) size. That is what the loaders
/// read, checked against their sources:
/// - PixiJS v8.21.0 `src/spritesheet/Spritesheet.ts:407-443`: a rotated frame's page rectangle is
///   `frame.x, frame.y, frame.h, frame.w`, the trim is `spriteSourceSize.x, .y` with `frame.w, frame.h`,
///   and the texture gets `rotate: 2` (groupD8 S). `src/rendering/renderers/shared/texture/Texture.ts:322-373`
///   (`updateUvs`) then samples the sprite's top-left corner at the page area's top-right, its
///   top-right at the bottom-right, and so on round: the pixels were turned clockwise.
/// - Phaser v3.90.0 `src/textures/parsers/JSONHash.js:52,64-77`: `frame.w`, `frame.h` are the cut
///   size and `updateUVsInverted` (`src/textures/Frame.js:766-777`) spans `cutHeight` across the page
///   and `cutWidth` down it, the same footprint; but `Frame.js:214` marks `rotated` "CURRENTLY
///   UNSUPPORTED" and its quad (`src/renderer/webgl/WebGLPipeline.js:1747`) draws such a frame mirrored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// Where the sprite's (trimmed) pixels are on the page, and their unrotated size.
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Where those pixels sat in the original image, and its size.
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    /// Stored turned 90° clockwise.
    pub rotated: bool,
}

impl Frame {
    pub fn trimmed(&self) -> bool {
        (self.width, self.height) != (self.source_width, self.source_height)
    }

    /// The width and height the sprite takes on the page.
    pub fn packed_size(&self) -> (u32, u32) {
        if self.rotated { (self.height, self.width) } else { (self.width, self.height) }
    }
}

#[derive(Debug)]
pub struct Page {
    pub image: RgbaImage,
    /// Sorted by name.
    pub frames: Vec<(String, Frame)>,
}

/// The smallest rectangle holding every pixel with any alpha; a blank image keeps one pixel.
fn opaque_bounds(image: &RgbaImage) -> (u32, u32, u32, u32) {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel.0[3] > 0 {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
    }
    if x0 == u32::MAX { (0, 0, 1, 1) } else { (x0, y0, x1 - x0 + 1, y1 - y0 + 1) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl Rect {
    fn contains(&self, other: &Rect) -> bool {
        other.x >= self.x && other.y >= self.y && other.x + other.w <= self.x + self.w && other.y + other.h <= self.y + self.h
    }
    fn overlaps(&self, other: &Rect) -> bool {
        self.x < other.x + other.w && other.x < self.x + self.w && self.y < other.y + other.h && other.y < self.y + self.h
    }
}

/// Where a MaxRects bin puts the next cell, among the free rectangles it fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Heuristic {
    /// Least leftover on the free rectangle's short side.
    ShortSide,
    /// Least leftover area.
    Area,
    /// Lowest bottom edge (here: the cell's far edge from the top), then leftmost.
    BottomLeft,
    /// Most edge length touching the bin's sides and the cells already placed.
    ContactPoint,
}

/// Tried in this order; the first one, with the first sort order, is the layout from before best-of.
/// No best long side fit: it wins the full-page score with a strip along one edge, and shrinking
/// with it stacks full-height columns, so the last page came out a strip (49 equal sprites: 102 x
/// 2002 instead of near-square) while it never won on the units corpus.
const HEURISTICS: [Heuristic; 4] = [Heuristic::ShortSide, Heuristic::Area, Heuristic::BottomLeft, Heuristic::ContactPoint];

/// The order cells go into the bin, biggest first by some measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    /// Longest side, then area.
    LongestSide,
    Area,
    Height,
    Width,
    Perimeter,
}

const ORDERS: [Order; 5] = [Order::LongestSide, Order::Area, Order::Height, Order::Width, Order::Perimeter];

/// The unique images by `order`'s measure of their cells, descending; ties go to the lower index.
fn sorted(cells: &[(u32, u32)], order: Order) -> Vec<usize> {
    let mut sorted: Vec<usize> = (0..cells.len()).collect();
    sorted.sort_by_key(|&u| {
        let (w, h) = (u64::from(cells[u].0), u64::from(cells[u].1));
        let key = match order {
            Order::LongestSide => (w.max(h), w * h),
            Order::Area => (w * h, 0),
            Order::Height => (h, 0),
            Order::Width => (w, 0),
            Order::Perimeter => (w + h, 0),
        };
        (std::cmp::Reverse(key), u)
    });
    sorted
}

/// Length of the overlap of [a, a + a_len) and [b, b + b_len).
fn overlap(a: u32, a_len: u32, b: u32, b_len: u32) -> u64 {
    u64::from((a + a_len).min(b + b_len).saturating_sub(a.max(b)))
}

/// MaxRects bin: the maximal free rectangles, and the cells placed so far.
struct Bin {
    w: u32,
    h: u32,
    free: Vec<Rect>,
    used: Vec<Rect>,
}

impl Bin {
    fn new(w: u32, h: u32) -> Self {
        Bin { w, h, free: vec![Rect { x: 0, y: 0, w, h }], used: Vec::new() }
    }

    /// Best spot for a w x h cell by `heuristic`; with `rotate`, a non-square cell may also go in
    /// turned (h x w), and the rect says which. Every score ends in unturned first, then topmost,
    /// then leftmost, so ties are broken the same way every time.
    fn find(&self, w: u32, h: u32, heuristic: Heuristic, rotate: bool) -> Option<Rect> {
        let turns = if rotate && w != h { 2 } else { 1 };
        [(w, h), (h, w)][..turns]
            .iter()
            .enumerate()
            .flat_map(|(turned, &(w, h))| {
                self.free.iter().filter(move |f| f.w >= w && f.h >= h).map(move |f| {
                    let (dw, dh) = (u64::from(f.w - w), u64::from(f.h - h));
                    let rect = Rect { x: f.x, y: f.y, w, h };
                    let score = match heuristic {
                        Heuristic::ShortSide => (dw.min(dh), dw.max(dh)),
                        Heuristic::Area => (u64::from(f.w) * u64::from(f.h) - u64::from(w) * u64::from(h), dw.min(dh)),
                        Heuristic::BottomLeft => (u64::from(f.y + h), u64::from(f.x)),
                        Heuristic::ContactPoint => (u64::MAX - self.contact(&rect), 0),
                    };
                    ((score, turned, f.y, f.x), rect)
                })
            })
            .min_by_key(|(score, _)| *score)
            .map(|(_, rect)| rect)
    }

    /// Edge length `r` would share with the bin's sides and the cells already placed.
    fn contact(&self, r: &Rect) -> u64 {
        let mut length = 0;
        if r.x == 0 || r.x + r.w == self.w {
            length += u64::from(r.h);
        }
        if r.y == 0 || r.y + r.h == self.h {
            length += u64::from(r.w);
        }
        for u in &self.used {
            if u.x == r.x + r.w || u.x + u.w == r.x {
                length += overlap(u.y, u.h, r.y, r.h);
            }
            if u.y == r.y + r.h || u.y + u.h == r.y {
                length += overlap(u.x, u.w, r.x, r.w);
            }
        }
        length
    }

    /// Take `used` out of the free rectangles: split each one it overlaps into the up to four
    /// maximal pieces around it, then drop pieces inside other free rectangles.
    fn place(&mut self, used: Rect) {
        // Each entry says whether it is a new piece. Untouched rectangles never need checking:
        // they were not inside one another before, and a piece lies inside the rectangle it was
        // cut from, which did overlap `used`, so it can't hold an untouched one either.
        let mut next: Vec<(Rect, bool)> = Vec::with_capacity(self.free.len() + 4);
        for f in &self.free {
            if !f.overlaps(&used) {
                next.push((*f, false));
                continue;
            }
            if used.x > f.x {
                next.push((Rect { x: f.x, y: f.y, w: used.x - f.x, h: f.h }, true));
            }
            if used.x + used.w < f.x + f.w {
                next.push((Rect { x: used.x + used.w, y: f.y, w: f.x + f.w - used.x - used.w, h: f.h }, true));
            }
            if used.y > f.y {
                next.push((Rect { x: f.x, y: f.y, w: f.w, h: used.y - f.y }, true));
            }
            if used.y + used.h < f.y + f.h {
                next.push((Rect { x: f.x, y: used.y + used.h, w: f.w, h: f.y + f.h - used.y - used.h }, true));
            }
        }
        // Drop pieces inside others (and duplicates, keeping the first).
        let keep: Vec<bool> = (0..next.len())
            .map(|i| {
                let (rect, piece) = next[i];
                !piece || !next.iter().enumerate().any(|(j, (other, _))| j != i && other.contains(&rect) && (rect != *other || j < i))
            })
            .collect();
        self.free = next.into_iter().zip(keep).filter_map(|((r, _), k)| k.then_some(r)).collect();
        self.used.push(used);
    }
}

/// Copy `image` to (x, y) on `page`, repeating its edge pixels `extrude` times outward.
fn blit_extruded(page: &mut RgbaImage, image: &RgbaImage, x: u32, y: u32, extrude: u32) {
    let (w, h) = image.dimensions();
    let e = i64::from(extrude);
    for dy in -e..i64::from(h) + e {
        for dx in -e..i64::from(w) + e {
            let sx = dx.clamp(0, i64::from(w) - 1) as u32;
            let sy = dy.clamp(0, i64::from(h) - 1) as u32;
            page.put_pixel((i64::from(x) + dx) as u32, (i64::from(y) + dy) as u32, *image.get_pixel(sx, sy));
        }
    }
}

/// Pack `sprites` into as few pages as fit within `max_size`. Names must be unique.
pub fn pack(sprites: Vec<Sprite>, opts: &AtlasOptions) -> Result<Vec<Page>> {
    if opts.pot && !opts.max_size.is_power_of_two() {
        return Err(Error::usage("--pot needs a power-of-two --max-size, such as 1024 or 2048."));
    }
    let (pad, extrude) = (opts.padding, opts.extrude);
    // The bin leaves `pad` along the top and left page edges; each cell carries its own padding
    // on the right and bottom, which spaces sprites apart and pads the far edges.
    let side = opts.max_size.checked_sub(pad).filter(|s| *s > 0).ok_or_else(|| Error::usage("--padding is larger than --max-size."))?;
    let mut items: Vec<(String, RgbaImage, Frame)> = Vec::with_capacity(sprites.len());
    for sprite in sprites {
        let (sw, sh) = sprite.image.dimensions();
        let (bx, by, bw, bh) = if opts.trim { opaque_bounds(&sprite.image) } else { (0, 0, sw, sh) };
        let mut image = if (bw, bh) == (sw, sh) { sprite.image } else { imageops::crop_imm(&sprite.image, bx, by, bw, bh).to_image() };
        // Invisible colour under fully transparent pixels would keep identical sprites apart.
        image.pixels_mut().filter(|p| p.0[3] == 0).for_each(|p| p.0 = [0; 4]);
        let frame = Frame { x: 0, y: 0, width: bw, height: bh, source_x: bx, source_y: by, source_width: sw, source_height: sh, rotated: false };
        let cell = |n: u32| n + 2 * extrude + pad;
        if cell(bw) > side || cell(bh) > side {
            return Err(Error::usage(format!("{} is {bw}x{bh} after trimming; with padding and extrusion it doesn't fit a {} px page (raise --max-size).", sprite.name, opts.max_size)));
        }
        items.push((sprite.name, image, frame));
    }
    items.sort_by(|a, b| a.0.cmp(&b.0));
    if let Some(pair) = items.windows(2).find(|pair| pair[0].0 == pair[1].0) {
        return Err(Error::usage(format!("Two images are both named {}; frame names must be unique.", pair[0].0)));
    }
    // One packed image per distinct set of visible pixels, in order of the first name using it; `users[u]`
    // lists the items drawn from unique image `u`.
    let mut unique: Vec<RgbaImage> = Vec::new();
    let mut users: Vec<Vec<usize>> = Vec::new();
    let mut by_pixels: HashMap<(u32, u32, &[u8]), usize> = HashMap::new();
    for (i, (_, image, _)) in items.iter().enumerate() {
        let u = *by_pixels.entry((image.width(), image.height(), image.as_raw().as_slice())).or_insert_with(|| {
            unique.push(image.clone());
            users.push(Vec::new());
            unique.len() - 1
        });
        users[u].push(i);
    }
    let cells: Vec<(u32, u32)> = unique.iter().map(|im| (im.width() + 2 * extrude + pad, im.height() + 2 * extrude + pad)).collect();
    let layout = best_layout(&cells, side, pad, opts.pot, opts.rotate);

    let mut pages = Vec::new();
    for placed in layout {
        let (w, h) = page_size(&placed, pad, opts.pot);
        let mut image = RgbaImage::new(w, h);
        let mut frames = Vec::new();
        for (u, rect) in placed {
            let (x, y) = (pad + rect.x + extrude, pad + rect.y + extrude);
            // A cell placed turned is h x w; square cells are never turned. Turned pixels go on the
            // page clockwise, the way the loaders turn them back (see `Frame`), and the extrusion
            // wraps them as placed. Every name sharing the image shares its turn.
            let rotated = (rect.w, rect.h) != cells[u];
            if rotated {
                blit_extruded(&mut image, &imageops::rotate90(&unique[u]), x, y, extrude);
            } else {
                blit_extruded(&mut image, &unique[u], x, y, extrude);
            }
            frames.extend(users[u].iter().map(|&i| (items[i].0.clone(), Frame { x, y, rotated, ..items[i].2 })));
        }
        frames.sort_by(|a, b| a.0.cmp(&b.0));
        pages.push(Page { image, frames });
    }
    Ok(pages)
}

/// One page's cells: each unique image and where it went.
type Placed = Vec<(usize, Rect)>;

/// How many of the best full-page layouts get their last page shrunk.
const FINALISTS: usize = 3;

/// Every heuristic with every sort order fills full `side` pages. Of the layouts with the fewest
/// pages, the `FINALISTS` with the least total page area (the first tried on a tie) each get their
/// last page shrunk with their own heuristic, and `squarest` picks among them, so the search stays
/// cheap: the full-page fills are quick, `shrink` is the costly step.
fn best_layout(cells: &[(u32, u32)], side: u32, pad: u32, pot: bool, rotate: bool) -> Vec<Placed> {
    if cells.is_empty() {
        return Vec::new();
    }
    let orders: Vec<Vec<usize>> = ORDERS.iter().map(|&order| sorted(cells, order)).collect();
    let mut candidates: Vec<(Heuristic, Vec<Placed>, u64)> = Vec::new();
    for heuristic in HEURISTICS {
        for order in &orders {
            let layout = paginate(order, cells, side, heuristic, rotate);
            let area = layout.iter().map(|placed| page_area(placed, pad, pot)).sum();
            candidates.push((heuristic, layout, area));
        }
    }
    let fewest = candidates.iter().map(|(_, layout, _)| layout.len()).min().unwrap_or(0);
    candidates.retain(|(_, layout, _)| layout.len() == fewest);
    // A stable sort keeps the order tried among equal areas; the finalists then go back to that
    // order, so a tie in `squarest` goes to the one tried first too.
    let mut ranked: Vec<usize> = (0..candidates.len()).collect();
    ranked.sort_by_key(|&i| candidates[i].2);
    ranked.truncate(FINALISTS);
    ranked.sort_unstable();
    let finalists = candidates.into_iter().enumerate().filter(|(i, _)| ranked.contains(i)).map(|(_, (heuristic, mut layout, _))| {
        if let Some(last) = layout.pop() {
            // Everything on the last page fits on a full page: find a smaller page for it. Its
            // cells were placed in their sort order, so they go into the smaller bins in that order.
            let order: Vec<usize> = last.iter().map(|(u, _)| *u).collect();
            layout.push(shrink(&order, cells, side, pad, pot, heuristic, rotate).unwrap_or(last));
        }
        // Squareness is the shrunk last page's; the area is every page's.
        let (w, h) = layout.last().map(|placed| page_size(placed, pad, pot)).unwrap_or((0, 0));
        let area = layout.iter().map(|placed| page_area(placed, pad, pot)).sum();
        ((w.max(h), area, w), layout)
    });
    squarest(finalists.collect()).unwrap_or_default()
}

/// Fill `side` x `side` pages greedily: each takes what fits of what the ones before it left.
fn paginate(order: &[usize], cells: &[(u32, u32)], side: u32, heuristic: Heuristic, rotate: bool) -> Vec<Placed> {
    let mut pages = Vec::new();
    let mut order = order.to_vec();
    while !order.is_empty() {
        let (placed, rest) = fill(&order, cells, side, side, heuristic, rotate);
        pages.push(placed);
        order = rest;
    }
    pages
}

/// Pack `order` into a w x h bin: what went where (in `order`; a turned cell's rect is h x w),
/// and what didn't fit.
fn fill(order: &[usize], cells: &[(u32, u32)], w: u32, h: u32, heuristic: Heuristic, rotate: bool) -> (Placed, Vec<usize>) {
    let mut bin = Bin::new(w, h);
    let (mut placed, mut rest) = (Vec::new(), Vec::new());
    for &u in order {
        match bin.find(cells[u].0, cells[u].1, heuristic, rotate) {
            Some(rect) => {
                bin.place(rect);
                placed.push((u, rect));
            }
            None => rest.push(u),
        }
    }
    (placed, rest)
}

/// Page size used by `placed`, including the top and left padding.
fn extent(placed: &[(usize, Rect)], pad: u32) -> (u32, u32) {
    let w = placed.iter().map(|(_, r)| r.x + r.w).max().unwrap_or(0);
    let h = placed.iter().map(|(_, r)| r.y + r.h).max().unwrap_or(0);
    (pad + w, pad + h)
}

/// The page `placed` makes: its extent, sides rounded up to powers of two with `pot`.
fn page_size(placed: &[(usize, Rect)], pad: u32, pot: bool) -> (u32, u32) {
    let (w, h) = extent(placed, pad);
    if pot { (w.next_power_of_two(), h.next_power_of_two()) } else { (w, h) }
}

fn page_area(placed: &[(usize, Rect)], pad: u32, pot: bool) -> u64 {
    let (w, h) = page_size(placed, pad, pot);
    u64::from(w) * u64::from(h)
}

/// `shrink`'s rule for picking a layout, each scored (longest side, area, width): among those
/// within 10% of the least area, the squarest (shortest longest side), then the least area, then
/// the narrowest, then the first one.
fn squarest<T>(layouts: Vec<((u32, u64, u32), T)>) -> Option<T> {
    let least = layouts.iter().map(|((_, area, _), _)| *area).min()?;
    layouts.into_iter().filter(|((_, area, _), _)| *area * 10 <= least * 11).min_by_key(|(score, _)| *score).map(|(_, layout)| layout)
}

/// A small page holding all of `order`: try bins of many widths (powers of two with `pot`), each
/// `side` tall, and keep the `squarest` layout, so a sheet of same-sized sprites comes out
/// near-square rather than as a slightly tighter strip.
fn shrink(order: &[usize], cells: &[(u32, u32)], side: u32, pad: u32, pot: bool, heuristic: Heuristic, rotate: bool) -> Option<Placed> {
    // The narrowest bin worth trying holds the widest cell, turned to its short side with `rotate`.
    let widest = order.iter().map(|&u| if rotate { cells[u].0.min(cells[u].1) } else { cells[u].0 }).max()?;
    let area: u64 = order.iter().map(|&u| u64::from(cells[u].0) * u64::from(cells[u].1)).sum();
    let narrowest = widest.max((area as f64).sqrt() as u32 / 4).min(side);
    let widths: Vec<u32> = if pot {
        (0..32).map(|b| 1u32 << b).filter(|&w| w > pad && w - pad >= widest && w - pad <= side).map(|w| w - pad).collect()
    } else {
        let step = (side / 128).max(4);
        (narrowest..side).step_by(step as usize).chain([side]).collect()
    };
    let mut layouts = Vec::new();
    for w in widths {
        let (placed, rest) = fill(order, cells, w, side, heuristic, rotate);
        if !rest.is_empty() {
            continue;
        }
        let (pw, ph) = page_size(&placed, pad, pot);
        layouts.push(((pw.max(ph), u64::from(pw) * u64::from(ph), pw), placed));
    }
    squarest(layouts)
}

/// The TexturePacker "hash" JSON for one page. `related` lists the other pages' JSON files, for
/// PixiJS's `related_multi_packs` (given on the first page only, so each page is loaded once).
pub fn page_json(page: &Page, image_name: &str, related: &[String]) -> Value {
    let mut frames = Map::new();
    for (name, f) in &page.frames {
        frames.insert(name.clone(), json!({
            "frame": {"x": f.x, "y": f.y, "w": f.width, "h": f.height},
            "rotated": f.rotated,
            "trimmed": f.trimmed(),
            "spriteSourceSize": {"x": f.source_x, "y": f.source_y, "w": f.width, "h": f.height},
            "sourceSize": {"w": f.source_width, "h": f.source_height},
        }));
    }
    let mut meta = json!({
        "app": "codex-img",
        "image": image_name,
        "format": "RGBA8888",
        "size": {"w": page.image.width(), "h": page.image.height()},
        "scale": "1",
    });
    if !related.is_empty() {
        meta["related_multi_packs"] = json!(related);
    }
    json!({"frames": frames, "meta": meta})
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn sprite(name: &str, w: u32, h: u32, colour: [u8; 4]) -> Sprite {
        Sprite { name: name.into(), image: RgbaImage::from_pixel(w, h, Rgba(colour)) }
    }

    fn no_overlap(page: &Page, gap: u32) {
        for (i, (_, a)) in page.frames.iter().enumerate() {
            let (aw, ah) = a.packed_size();
            assert!(a.x >= gap && a.y >= gap && a.x + aw + gap <= page.image.width() && a.y + ah + gap <= page.image.height());
            for (_, b) in &page.frames[i + 1..] {
                if (a.x, a.y) == (b.x, b.y) && a.packed_size() == b.packed_size() {
                    continue; // one shared image
                }
                let (bw, bh) = b.packed_size();
                let apart = a.x + aw + gap <= b.x || b.x + bw + gap <= a.x || a.y + ah + gap <= b.y || b.y + bh + gap <= a.y;
                assert!(apart, "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn packs_without_overlap_and_splits_into_pages() {
        let sprites: Vec<Sprite> = (0..40).map(|i| sprite(&format!("s{i:02}"), 10 + i % 7 * 5, 12 + i % 5 * 6, [i as u8, 0, 0, 255])).collect();
        let opts = AtlasOptions { padding: 2, extrude: 1, max_size: 128, ..Default::default() };
        let pages = pack(sprites, &opts).unwrap();
        assert!(pages.len() > 1);
        assert_eq!(pages.iter().map(|p| p.frames.len()).sum::<usize>(), 40);
        for page in &pages {
            assert!(page.image.width() <= 128 && page.image.height() <= 128);
            // Padding 2 plus one pixel of extrusion on each side.
            no_overlap(page, 2 + 1);
            for (name, f) in &page.frames {
                let i: u8 = name[1..].parse().unwrap();
                assert_eq!(page.image.get_pixel(f.x, f.y).0, [i, 0, 0, 255]);
                assert_eq!(page.image.get_pixel(f.x - 1, f.y - 1).0, [i, 0, 0, 255], "extruded corner");
            }
        }
    }

    #[test]
    fn trims_keeps_the_source_size_and_is_deterministic() {
        let mut image = RgbaImage::new(20, 10);
        image.put_pixel(5, 3, Rgba([1, 2, 3, 255]));
        image.put_pixel(7, 6, Rgba([1, 2, 3, 1]));
        let make = || vec![Sprite { name: "a/b".into(), image: image.clone() }, sprite("c", 4, 4, [9, 9, 9, 255])];
        let opts = AtlasOptions { trim: true, padding: 0, ..Default::default() };
        let pages = pack(make(), &opts).unwrap();
        let (_, f) = pages[0].frames.iter().find(|(n, _)| n == "a/b").unwrap();
        assert_eq!((f.width, f.height, f.source_x, f.source_y, f.source_width, f.source_height), (3, 4, 5, 3, 20, 10));
        let json = page_json(&pages[0], "units.webp", &[]);
        assert_eq!(json["frames"]["a/b"]["trimmed"], true);
        assert_eq!(json["frames"]["c"]["trimmed"], false);
        assert_eq!(json["frames"]["a/b"]["sourceSize"], json!({"w": 20, "h": 10}));
        assert!(json["meta"].get("related_multi_packs").is_none());
        let again = pack(make(), &opts).unwrap();
        assert_eq!(again[0].image, pages[0].image);
        assert_eq!(page_json(&again[0], "units.webp", &[]), json);
    }

    #[test]
    fn stores_identical_images_once() {
        let mut hidden = RgbaImage::from_pixel(6, 6, Rgba([0, 0, 0, 0]));
        hidden.put_pixel(2, 2, Rgba([5, 5, 5, 255]));
        let mut shifted = RgbaImage::from_pixel(9, 9, Rgba([77, 0, 0, 0]));
        shifted.put_pixel(4, 7, Rgba([5, 5, 5, 255]));
        let sprites = vec![
            Sprite { name: "a".into(), image: hidden },
            Sprite { name: "b".into(), image: shifted },
            sprite("c", 8, 8, [1, 2, 3, 255]),
            sprite("d", 8, 8, [1, 2, 3, 255]),
            sprite("e", 8, 8, [1, 2, 4, 255]),
        ];
        let pages = pack(sprites, &AtlasOptions { trim: true, padding: 1, ..Default::default() }).unwrap();
        assert_eq!(pages.len(), 1);
        let f: HashMap<&str, Frame> = pages[0].frames.iter().map(|(n, f)| (n.as_str(), *f)).collect();
        assert_eq!(f.len(), 5);
        // Same visible pixels share a rect but keep their own trim offsets and source sizes.
        assert_eq!((f["a"].x, f["a"].y), (f["b"].x, f["b"].y));
        assert_eq!((f["a"].source_x, f["a"].source_y, f["a"].source_width), (2, 2, 6));
        assert_eq!((f["b"].source_x, f["b"].source_y, f["b"].source_width), (4, 7, 9));
        assert_eq!((f["c"].x, f["c"].y), (f["d"].x, f["d"].y));
        assert_ne!((f["c"].x, f["c"].y), (f["e"].x, f["e"].y));
    }

    #[test]
    fn same_sized_sprites_make_a_near_square_page() {
        let sprites: Vec<Sprite> = (0..49).map(|i| sprite(&format!("s{i:02}"), 48, 48, [i as u8, 1, 0, 255])).collect();
        let pages = pack(sprites, &AtlasOptions::default()).unwrap();
        let (w, h) = pages[0].image.dimensions();
        assert!(w.max(h) <= 2 * w.min(h), "{w}x{h}");
        no_overlap(&pages[0], 2);
    }

    #[test]
    fn every_heuristic_places_inside_the_bin_without_overlap() {
        // Mixed sizes, more than one 64 x 48 bin holds, so the bin fills up and some are left over.
        let cells: Vec<(u32, u32)> = (0..60).map(|i| (3 + i * 7 % 13, 2 + i * 5 % 11)).collect();
        for heuristic in HEURISTICS {
            for order in ORDERS {
                let (placed, rest) = fill(&sorted(&cells, order), &cells, 64, 48, heuristic, false);
                assert!(placed.len() > 10 && !rest.is_empty(), "{heuristic:?} {order:?}: {} placed, {} left", placed.len(), rest.len());
                assert_eq!(placed.len() + rest.len(), cells.len());
                for (i, (u, a)) in placed.iter().enumerate() {
                    assert_eq!((a.w, a.h), cells[*u], "{heuristic:?}");
                    assert!(a.x + a.w <= 64 && a.y + a.h <= 48, "{heuristic:?} {order:?}: {a:?} outside the bin");
                    for (_, b) in &placed[i + 1..] {
                        assert!(!a.overlaps(b), "{heuristic:?} {order:?}: {a:?} {b:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn best_of_fits_on_one_page_what_the_old_packing_split_in_two() {
        // Two columns of five-wide cells fill a 10 x 10 page exactly: 7 + 3 and 6 + 2 + 2. Longest
        // side first with best short side fit puts the 5x3 under the 5x6 and strands a 5x2.
        let sizes = [(5, 7), (5, 6), (5, 2), (5, 3), (5, 2)];
        let cells: Vec<(u32, u32)> = sizes.to_vec();
        assert_eq!(paginate(&sorted(&cells, Order::LongestSide), &cells, 10, Heuristic::ShortSide, false).len(), 2, "the old packing needs two pages");
        let sprites: Vec<Sprite> = sizes.iter().enumerate().map(|(i, &(w, h))| sprite(&format!("s{i}"), w, h, [i as u8, 7, 7, 255])).collect();
        let pages = pack(sprites, &AtlasOptions { padding: 0, max_size: 10, ..Default::default() }).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].frames.len(), 5);
        assert_eq!(pages[0].image.dimensions(), (10, 10));
        no_overlap(&pages[0], 0);
    }

    #[test]
    fn two_runs_give_identical_pages() {
        let make = || -> Vec<Sprite> { (0..90).map(|i| sprite(&format!("s{i:02}"), 4 + i * 7 % 23, 3 + i * 11 % 19, [i as u8, 3, 9, 255])).collect() };
        let opts = AtlasOptions { padding: 1, max_size: 128, ..Default::default() };
        let (first, second) = (pack(make(), &opts).unwrap(), pack(make(), &opts).unwrap());
        assert!(first.len() > 1, "several pages, so the page split is compared too");
        assert_eq!(first.len(), second.len());
        for (a, b) in first.iter().zip(&second) {
            assert_eq!(a.frames, b.frames);
            assert_eq!(a.image, b.image);
        }
    }

    /// Non-square sprites whose pixels differ under every turn and mirror: a gradient over x and y,
    /// a transparent border of a different width on each side (so trimming cuts unevenly), and a
    /// marker in one corner.
    fn asymmetric(name: &str, w: u32, h: u32, seed: u8) -> Sprite {
        let mut image = RgbaImage::new(w, h);
        for y in 1..h.saturating_sub(2) {
            for x in 2..w.saturating_sub(1) {
                image.put_pixel(x, y, Rgba([seed, (x * 255 / w) as u8, (y * 255 / h) as u8, 255]));
            }
        }
        image.put_pixel(2, 1, Rgba([255, 255, 255, 255]));
        Sprite { name: name.into(), image }
    }

    fn mixed_sprites() -> Vec<Sprite> {
        (0..60u32).map(|i| asymmetric(&format!("m{i:02}"), 6 + i * 7 % 29, 5 + i * 13 % 23, i as u8)).collect()
    }

    /// FNV-1a over every page's size, pixels and JSON.
    fn digest(pages: &[Page]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |bytes: &[u8]| {
            for b in bytes {
                hash = (hash ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
            }
        };
        for page in pages {
            eat(&page.image.width().to_le_bytes());
            eat(&page.image.height().to_le_bytes());
            eat(page.image.as_raw());
            eat(page_json(page, "a.png", &[]).to_string().as_bytes());
        }
        hash
    }

    #[test]
    fn without_rotate_no_frame_turns_and_the_output_matches_the_packing_before_rotation() {
        // Digests of this sprite set packed by the code before `rotate` existed.
        let cases = [
            (AtlasOptions::default(), 1, 0x8733_d6e5_84a3_ac49),
            (AtlasOptions { trim: true, extrude: 1, ..Default::default() }, 1, 0x2c16_8240_ef37_d469),
            (AtlasOptions { trim: true, padding: 0, pot: true, max_size: 128, ..Default::default() }, 1, 0xd4a4_f05a_4c91_c2d3),
            (AtlasOptions { padding: 1, extrude: 2, max_size: 96, ..Default::default() }, 4, 0x7291_3431_a5a2_33f3),
        ];
        for (opts, page_count, expected) in cases {
            assert!(!opts.rotate);
            let pages = pack(mixed_sprites(), &opts).unwrap();
            assert_eq!(pages.len(), page_count, "{opts:?}");
            assert_eq!(pages.iter().map(|p| p.frames.len()).sum::<usize>(), 60);
            assert!(pages.iter().flat_map(|p| &p.frames).all(|(_, f)| !f.rotated), "{opts:?}");
            assert!(pages.iter().all(|p| page_json(p, "a.png", &[])["frames"].as_object().unwrap().values().all(|f| f["rotated"] == false)));
            assert_eq!(digest(&pages), expected, "{opts:?}: the pages changed");
        }
    }

    /// Rebuild a sprite at its source size from the page and its JSON frame, the way PixiJS draws a
    /// texture (see `Frame` for the source lines): the sprite's pixels sit at `spriteSourceSize.x, .y`
    /// in a `sourceSize` canvas; a rotated frame's sprite pixel (sx, sy) is sampled from the page
    /// at (frame.x + frame.h - 1 - sy, frame.y + sx), because its UVs put the sprite's top-left corner
    /// at the page area's top-right and its top-right at the bottom-right.
    fn rebuild(page: &RgbaImage, json: &Value) -> RgbaImage {
        let n = |v: &Value| v.as_u64().unwrap() as u32;
        let (fx, fy, fw, fh) = (n(&json["frame"]["x"]), n(&json["frame"]["y"]), n(&json["frame"]["w"]), n(&json["frame"]["h"]));
        let mut sprite = RgbaImage::new(n(&json["sourceSize"]["w"]), n(&json["sourceSize"]["h"]));
        let (ox, oy) = (n(&json["spriteSourceSize"]["x"]), n(&json["spriteSourceSize"]["y"]));
        assert_eq!((n(&json["spriteSourceSize"]["w"]), n(&json["spriteSourceSize"]["h"])), (fw, fh));
        for sy in 0..fh {
            for sx in 0..fw {
                let (px, py) = if json["rotated"] == true { (fx + fh - 1 - sy, fy + sx) } else { (fx + sx, fy + sy) };
                sprite.put_pixel(ox + sx, oy + sy, *page.get_pixel(px, py));
            }
        }
        sprite
    }

    #[test]
    fn rotated_sprites_rebuild_pixel_for_pixel_the_way_loaders_read_them_with_and_without_trim_and_extrude() {
        let make = || -> Vec<Sprite> {
            let mut sprites: Vec<Sprite> = (0..40u32).map(|i| asymmetric(&format!("r{i:02}"), 5 + i * 11 % 31, 4 + i * 17 % 27, 10 + i as u8)).collect();
            sprites.push(asymmetric("square", 12, 12, 200));
            // r01's size and seed under another name: same pixels, stored once.
            sprites.push(asymmetric("twin", 16, 21, 11));
            sprites
        };
        let sources: HashMap<String, RgbaImage> = make().into_iter().map(|s| (s.name, s.image)).collect();
        for trim in [false, true] {
            for extrude in [0, 2] {
                let opts = AtlasOptions { trim, extrude, padding: 1, max_size: 128, rotate: true, ..Default::default() };
                let pages = pack(make(), &opts).unwrap();
                let frames: Vec<(&Page, &str, &Frame)> = pages.iter().flat_map(|p| p.frames.iter().map(move |(n, f)| (p, n.as_str(), f))).collect();
                assert_eq!(frames.len(), sources.len(), "{opts:?}");
                assert!(frames.iter().any(|(_, _, f)| f.rotated), "{opts:?}: nothing was rotated, so nothing is tested");
                assert!(frames.iter().any(|(_, _, f)| !f.rotated), "{opts:?}");
                for page in &pages {
                    no_overlap(page, 1 + extrude);
                    let json = page_json(page, "a.png", &[]);
                    for (name, f) in &page.frames {
                        let frame = &json["frames"][name.as_str()];
                        assert_eq!(frame["rotated"], f.rotated);
                        assert_eq!(rebuild(&page.image, frame), sources[name], "{opts:?}: {name} (rotated: {})", f.rotated);
                        // The extrusion ring repeats the nearest edge pixel of the area as placed.
                        let (w, h) = f.packed_size();
                        let e = i64::from(extrude);
                        for dy in -e..i64::from(h) + e {
                            for dx in -e..i64::from(w) + e {
                                let at = |x: i64, y: i64| *page.image.get_pixel((i64::from(f.x) + x) as u32, (i64::from(f.y) + y) as u32);
                                assert_eq!(at(dx, dy), at(dx.clamp(0, i64::from(w) - 1), dy.clamp(0, i64::from(h) - 1)), "{opts:?}: {name} at {dx},{dy}");
                            }
                        }
                    }
                }
                let by_name: HashMap<&str, &Frame> = frames.iter().map(|(_, n, f)| (*n, *f)).collect();
                assert!(!by_name["square"].rotated, "square sprites never turn");
                let (twin, original) = (by_name["twin"], by_name["r01"]);
                assert_eq!((twin.x, twin.y, twin.rotated), (original.x, original.y, original.rotated), "a shared image has one rotation");
            }
        }
    }

    #[test]
    fn rotation_fits_on_one_page_what_needs_two_unrotated() {
        // A 10 x 6 across the top leaves 10 x 4: the 4 x 10 only fits there turned.
        let make = || vec![asymmetric("tall", 4, 10, 1), asymmetric("wide", 10, 6, 2)];
        let opts = AtlasOptions { padding: 0, max_size: 10, ..Default::default() };
        let flat = pack(make(), &opts).unwrap();
        assert_eq!(flat.len(), 2, "unrotated, the two need two pages");
        let turned = pack(make(), &AtlasOptions { rotate: true, ..opts }).unwrap();
        assert_eq!(turned.len(), 1);
        assert_eq!(turned[0].image.dimensions(), (10, 10));
        assert_eq!(turned[0].frames.iter().filter(|(_, f)| f.rotated).count(), 1);
        no_overlap(&turned[0], 0);
    }

    #[test]
    fn with_rotate_every_heuristic_places_cells_upright_or_turned_inside_the_bin() {
        let cells: Vec<(u32, u32)> = (0..60).map(|i| (3 + i * 7 % 13, 2 + i * 5 % 11)).collect();
        let mut turned = 0;
        for heuristic in HEURISTICS {
            for order in ORDERS {
                let (placed, rest) = fill(&sorted(&cells, order), &cells, 64, 48, heuristic, true);
                assert_eq!(placed.len() + rest.len(), cells.len());
                for (i, (u, a)) in placed.iter().enumerate() {
                    let (w, h) = cells[*u];
                    assert!((a.w, a.h) == (w, h) || (a.w, a.h) == (h, w), "{heuristic:?}");
                    turned += usize::from((a.w, a.h) != (w, h));
                    assert!(a.x + a.w <= 64 && a.y + a.h <= 48, "{heuristic:?} {order:?}: {a:?} outside the bin");
                    for (_, b) in &placed[i + 1..] {
                        assert!(!a.overlaps(b), "{heuristic:?} {order:?}: {a:?} {b:?}");
                    }
                }
            }
        }
        assert!(turned > 0);
    }

    #[test]
    fn refuses_duplicates_oversized_sprites_and_odd_pot_sizes() {
        assert!(pack(vec![sprite("a", 2, 2, [0; 4]), sprite("a", 3, 3, [0; 4])], &AtlasOptions::default()).unwrap_err().message.contains("unique"));
        assert!(pack(vec![sprite("big", 70, 2, [0; 4])], &AtlasOptions { max_size: 64, ..Default::default() }).unwrap_err().message.contains("--max-size"));
        assert!(pack(vec![], &AtlasOptions { pot: true, max_size: 1000, ..Default::default() }).unwrap_err().message.contains("power-of-two"));
        let pages = pack(vec![sprite("a", 30, 3, [1; 4])], &AtlasOptions { pot: true, padding: 1, ..Default::default() }).unwrap();
        assert_eq!(pages[0].image.dimensions(), (32, 8));
    }
}
