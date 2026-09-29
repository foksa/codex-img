//! Pixel edits for `codex-img convert`: trim to the visible content, resize with premultiplied
//! alpha, and edge bleed under fully transparent pixels.
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Transform {
    /// Make alpha all-or-nothing: above this threshold solid, else fully transparent.
    pub hard_alpha: Option<u8>,
    /// Crop to the visible (alpha > FAINT_ALPHA) pixels, plus this much transparent padding on each side.
    pub trim: Option<u32>,
    pub resize: Option<Resize>,
    pub fit: Option<Fit>,
    /// Keep the colour stored under fully transparent pixels instead of bleeding edge colours in.
    pub no_bleed: bool,
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
            (Some(_), None) => Err(Error::usage("--fit only applies with --resize.")),
            (Some(_), Some(Resize { width: None, .. } | Resize { height: None, .. })) => {
                Err(Error::usage("--fit needs both sides in --resize (WxH); with one side the aspect ratio decides the other."))
            }
            _ => Ok(()),
        }
    }

    /// Whether this edits visible pixels (trim, resize or hard alpha was asked for), as opposed to
    /// only the colour under transparent ones.
    pub fn edits(&self) -> bool {
        self.trim.is_some() || self.resize.is_some() || self.hard_alpha.is_some()
    }

    pub fn apply(&self, mut rgba: RgbaImage, format: Format, enc: &Encoding) -> Result<Applied> {
        let mut changed = false;
        let mut trim = None;
        if let Some(threshold) = self.hard_alpha {
            changed |= harden_alpha(&mut rgba, threshold);
        }
        if let Some(padding) = self.trim {
            let mut rect = visible_bounds(&rgba).ok_or_else(|| Error::other("--trim: the image has no visible pixels."))?;
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
            if let Some(resized) = resize(&rgba, size, self.fit.unwrap_or(Fit::Inside)) {
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

/// None when the image already has the requested size.
fn resize(rgba: &RgbaImage, size: Resize, fit: Fit) -> Option<RgbaImage> {
    let (w, h) = rgba.dimensions();
    let (width, height) = match (size.width, size.height) {
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => (width, scale(h, width, w)),
        (None, Some(height)) => (scale(w, height, h), height),
        (None, None) => return None,
    };
    let out = match fit {
        Fit::Fill => resample(rgba, width, height),
        Fit::Inside => {
            let (iw, ih) = inside((w, h), width, height);
            resample(rgba, iw, ih)
        }
        Fit::Contain => {
            let (iw, ih) = inside((w, h), width, height);
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
            resample(&cropped, width, height)
        }
    };
    (out != *rgba).then_some(out)
}

/// Resample with premultiplied alpha, so the colour under transparent pixels can't bleed into the
/// edges. (imageops::resize assumes premultiplied input and doesn't premultiply itself.)
fn resample(rgba: &RgbaImage, width: u32, height: u32) -> RgbaImage {
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
