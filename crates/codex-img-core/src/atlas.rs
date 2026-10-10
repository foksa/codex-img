//! Texture atlases: pack many small images into a few pages, with a TexturePacker "hash" JSON per
//! page (the format PixiJS, Phaser and most engines load). Packing is MaxRects with a fixed order,
//! so the same sprites and options always give the same pages. Identical images (after trimming)
//! are stored once, with every name's frame pointing at the shared pixels.
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
}

impl Default for AtlasOptions {
    fn default() -> Self {
        AtlasOptions { trim: false, padding: DEFAULT_PADDING, extrude: 0, max_size: DEFAULT_MAX_SIZE, pot: false }
    }
}

pub struct Sprite {
    pub name: String,
    pub image: RgbaImage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// Where the sprite's (trimmed) pixels are on the page.
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Where those pixels sat in the original image, and its size.
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
}

impl Frame {
    pub fn trimmed(&self) -> bool {
        (self.width, self.height) != (self.source_width, self.source_height)
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

/// MaxRects bin with best-short-side-fit placement.
struct Bin {
    free: Vec<Rect>,
}

impl Bin {
    fn new(w: u32, h: u32) -> Self {
        Bin { free: vec![Rect { x: 0, y: 0, w, h }] }
    }

    /// Best spot for a w x h cell: least leftover on the short side, then the long side, then
    /// topmost, then leftmost, so ties are broken the same way every time.
    fn find(&self, w: u32, h: u32) -> Option<Rect> {
        self.free
            .iter()
            .filter(|f| f.w >= w && f.h >= h)
            .map(|f| {
                let (dw, dh) = (f.w - w, f.h - h);
                ((dw.min(dh), dw.max(dh), f.y, f.x), Rect { x: f.x, y: f.y, w, h })
            })
            .min_by_key(|(score, _)| *score)
            .map(|(_, rect)| rect)
    }

    fn place(&mut self, used: Rect) {
        let mut next = Vec::with_capacity(self.free.len() + 4);
        for f in &self.free {
            if !f.overlaps(&used) {
                next.push(*f);
                continue;
            }
            if used.x > f.x {
                next.push(Rect { x: f.x, y: f.y, w: used.x - f.x, h: f.h });
            }
            if used.x + used.w < f.x + f.w {
                next.push(Rect { x: used.x + used.w, y: f.y, w: f.x + f.w - used.x - used.w, h: f.h });
            }
            if used.y > f.y {
                next.push(Rect { x: f.x, y: f.y, w: f.w, h: used.y - f.y });
            }
            if used.y + used.h < f.y + f.h {
                next.push(Rect { x: f.x, y: used.y + used.h, w: f.w, h: f.y + f.h - used.y - used.h });
            }
        }
        // Drop free rectangles inside others (and duplicates, keeping the first).
        let keep: Vec<bool> = (0..next.len())
            .map(|i| !next.iter().enumerate().any(|(j, other)| j != i && other.contains(&next[i]) && (next[i] != *other || j < i)))
            .collect();
        self.free = next.into_iter().zip(keep).filter_map(|(r, k)| k.then_some(r)).collect();
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
        let frame = Frame { x: 0, y: 0, width: bw, height: bh, source_x: bx, source_y: by, source_width: sw, source_height: sh };
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
    // Big and awkward first: longest side, then area, then first name.
    let mut order: Vec<usize> = (0..unique.len()).collect();
    order.sort_by_key(|&u| {
        let (w, h) = cells[u];
        (std::cmp::Reverse(w.max(h)), std::cmp::Reverse(w * h), u)
    });

    let mut pages = Vec::new();
    while !order.is_empty() {
        let (mut placed, mut rest) = fill(&order, &cells, side, side);
        if rest.is_empty() {
            // Everything left fits on this page: find a smaller page for it.
            placed = shrink(&order, &cells, side, pad, opts.pot).unwrap_or(placed);
        }
        let (used_w, used_h) = extent(&placed, pad);
        let (w, h) = if opts.pot { (used_w.next_power_of_two(), used_h.next_power_of_two()) } else { (used_w, used_h) };
        let mut image = RgbaImage::new(w, h);
        let mut frames = Vec::new();
        for (u, rect) in placed {
            let (x, y) = (pad + rect.x + extrude, pad + rect.y + extrude);
            blit_extruded(&mut image, &unique[u], x, y, extrude);
            frames.extend(users[u].iter().map(|&i| (items[i].0.clone(), Frame { x, y, ..items[i].2 })));
        }
        frames.sort_by(|a, b| a.0.cmp(&b.0));
        pages.push(Page { image, frames });
        order = std::mem::take(&mut rest);
    }
    Ok(pages)
}

/// Pack `order` into a w x h bin: what went where, and what didn't fit.
fn fill(order: &[usize], cells: &[(u32, u32)], w: u32, h: u32) -> (Vec<(usize, Rect)>, Vec<usize>) {
    let mut bin = Bin::new(w, h);
    let (mut placed, mut rest) = (Vec::new(), Vec::new());
    for &u in order {
        match bin.find(cells[u].0, cells[u].1) {
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

/// A small page holding all of `order`: try bins of many widths (powers of two with `pot`), each
/// `side` tall, and keep the squarest layout within 10% of the least page area, so a sheet of
/// same-sized sprites comes out near-square rather than as a slightly tighter strip.
fn shrink(order: &[usize], cells: &[(u32, u32)], side: u32, pad: u32, pot: bool) -> Option<Vec<(usize, Rect)>> {
    let widest = order.iter().map(|&u| cells[u].0).max()?;
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
        let (placed, rest) = fill(order, cells, w, side);
        if !rest.is_empty() {
            continue;
        }
        let (pw, ph) = extent(&placed, pad);
        let (pw, ph) = if pot { (pw.next_power_of_two(), ph.next_power_of_two()) } else { (pw, ph) };
        layouts.push(((pw.max(ph), u64::from(pw) * u64::from(ph), pw), placed));
    }
    let least = layouts.iter().map(|((_, area, _), _)| *area).min()?;
    layouts.into_iter().filter(|((_, area, _), _)| *area * 10 <= least * 11).min_by_key(|(score, _)| *score).map(|(_, placed)| placed)
}

/// The TexturePacker "hash" JSON for one page. `related` lists the other pages' JSON files, for
/// PixiJS's `related_multi_packs` (given on the first page only, so each page is loaded once).
pub fn page_json(page: &Page, image_name: &str, related: &[String]) -> Value {
    let mut frames = Map::new();
    for (name, f) in &page.frames {
        frames.insert(name.clone(), json!({
            "frame": {"x": f.x, "y": f.y, "w": f.width, "h": f.height},
            "rotated": false,
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
            assert!(a.x >= gap && a.y >= gap && a.x + a.width + gap <= page.image.width() && a.y + a.height + gap <= page.image.height());
            for (_, b) in &page.frames[i + 1..] {
                let apart = a.x + a.width + gap <= b.x || b.x + b.width + gap <= a.x || a.y + a.height + gap <= b.y || b.y + b.height + gap <= a.y;
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
    fn refuses_duplicates_oversized_sprites_and_odd_pot_sizes() {
        assert!(pack(vec![sprite("a", 2, 2, [0; 4]), sprite("a", 3, 3, [0; 4])], &AtlasOptions::default()).unwrap_err().message.contains("unique"));
        assert!(pack(vec![sprite("big", 70, 2, [0; 4])], &AtlasOptions { max_size: 64, ..Default::default() }).unwrap_err().message.contains("--max-size"));
        assert!(pack(vec![], &AtlasOptions { pot: true, max_size: 1000, ..Default::default() }).unwrap_err().message.contains("power-of-two"));
        let pages = pack(vec![sprite("a", 30, 3, [1; 4])], &AtlasOptions { pot: true, padding: 1, ..Default::default() }).unwrap();
        assert_eq!(pages[0].image.dimensions(), (32, 8));
    }
}
