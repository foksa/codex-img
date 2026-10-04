use crate::error::{Error, Result};

const JPEG_QUALITY: u8 = 90;
const WEBP_QUALITY: u8 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Webp,
}

impl Format {

    pub fn parse(value: &str) -> Option<Format> {
        match value.to_ascii_lowercase().as_str() {
            "png" => Some(Format::Png),
            "jpeg" | "jpg" => Some(Format::Jpeg),
            "webp" => Some(Format::Webp),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpeg",
            Format::Webp => "webp",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Jpeg => "jpg",
            other => other.name(),
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Format::Png => "image/png",
            Format::Jpeg => "image/jpeg",
            Format::Webp => "image/webp",
        }
    }
}

pub fn sniff(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some(Format::Png)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(Format::Jpeg)
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(Format::Webp)
    } else {
        None
    }
}

/// How to encode local output. Kept in one place so every entry point validates it the same way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Encoding {
    /// Palette size for PNG quantization.
    pub colors: Option<u16>,
    pub dither: bool,
    /// 1-100 for JPEG and lossy WebP. Defaults: JPEG 90, WebP 80.
    pub quality: Option<u8>,
    /// Lossless WebP instead of lossy.
    pub lossless: bool,
}

impl Encoding {
    /// Reject settings the output format can't use. With `None` (format not known yet) only the
    /// format-independent rules are checked; call again once the format is resolved.
    pub fn check(&self, format: Option<Format>) -> Result<()> {
        if self.dither && self.colors.is_none() {
            return Err(Error::usage("--dither only applies with --colors."));
        }
        if self.lossless && self.quality.is_some() {
            return Err(Error::usage("--lossless and --output-quality can't be combined."));
        }
        let Some(format) = format else { return Ok(()) };
        if self.colors.is_some() && format != Format::Png {
            return Err(Error::usage(format!("--colors only applies to PNG output, not {}; add -f png.", format.name())));
        }
        if self.lossless && format != Format::Webp {
            return Err(Error::usage(format!("--lossless only applies to WebP output, not {}.", format.name())));
        }
        if self.quality.is_some() && format == Format::Png {
            return Err(Error::usage("--output-quality applies to JPEG and WebP; PNG is always lossless (use --colors to shrink it)."));
        }
        Ok(())
    }

    /// Whether `bytes`, already in `format`, satisfy these settings as they are. Re-encoding a
    /// lossy file only loses more, so it happens when the settings ask for something the file
    /// isn't: a palette, an explicit quality, or the other kind of WebP (lossy is the default).
    fn is_satisfied_by(&self, bytes: &[u8], format: Format) -> bool {
        match format {
            Format::Png => self.colors.is_none(),
            Format::Jpeg => self.quality.is_none(),
            Format::Webp => self.quality.is_none() && webp_is_lossless(bytes) == Some(self.lossless),
        }
    }

    /// The quality a lossy encode of `format` uses with these settings, or None if it's lossless.
    pub fn lossy_quality(&self, format: Format) -> Option<u8> {
        match format {
            Format::Jpeg => Some(self.quality.unwrap_or(JPEG_QUALITY)),
            Format::Webp if !self.lossless => Some(self.quality.unwrap_or(WEBP_QUALITY)),
            _ => None,
        }
    }
}

/// Top-level chunk ids of a WebP file, in order. Stops at the first truncated chunk.
fn webp_chunk_ids(bytes: &[u8]) -> Vec<[u8; 4]> {
    let mut ids = Vec::new();
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return ids;
    }
    let mut at = 12;
    while let Some(header) = bytes.get(at..at + 8) {
        ids.push([header[0], header[1], header[2], header[3]]);
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        match at.checked_add(8 + size + (size & 1)) {
            Some(next) => at = next,
            None => break,
        }
    }
    ids
}

/// Lossless (VP8L) or lossy (VP8) WebP; None if neither chunk is at the top level. Extended files
/// (VP8X) put ICCP/ALPH/... chunks first, and animated ones nest frames in ANMF chunks.
fn webp_is_lossless(bytes: &[u8]) -> Option<bool> {
    webp_chunk_ids(bytes).iter().find_map(|id| match id {
        b"VP8L" => Some(true),
        b"VP8 " => Some(false),
        _ => None,
    })
}

/// Animated WebP (ANIM/ANMF chunks) or APNG (an acTL chunk before the image data).
fn is_animated(bytes: &[u8]) -> bool {
    match sniff(bytes) {
        Some(Format::Webp) => webp_chunk_ids(bytes).iter().any(|id| id == b"ANIM" || id == b"ANMF"),
        Some(Format::Png) => {
            let mut at = 8;
            while let Some(header) = bytes.get(at..at + 8) {
                match &header[4..8] {
                    b"acTL" => return true,
                    b"IDAT" => return false,
                    _ => {}
                }
                let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
                match at.checked_add(12 + len) {
                    Some(next) => at = next,
                    None => return false,
                }
            }
            false
        }
        _ => false,
    }
}

/// Decode `bytes` (PNG, JPEG or WebP) and re-encode as `wanted`. JPEG has no alpha, so transparency
/// is flattened onto white. WebP is lossy unless `lossless` is set; both keep transparency.
pub fn convert(bytes: &[u8], wanted: Format, enc: &Encoding) -> Result<Vec<u8>> {
    let rgba = image::load_from_memory(bytes)
        .map_err(|e| Error::other(format!("Could not convert image to {}: {e}", wanted.name())))?
        .to_rgba8();
    encode(&rgba, wanted, enc)
}

/// Encode decoded pixels as `wanted`, with the same rules as `convert`.
pub fn encode(rgba: &image::RgbaImage, wanted: Format, enc: &Encoding) -> Result<Vec<u8>> {
    let failed = |e: String| Error::other(format!("Could not convert image to {}: {e}", wanted.name()));
    match (wanted, enc.colors) {
        (Format::Png, Some(colors)) => quantize_png(rgba, colors, enc.dither).map_err(failed),
        (Format::Png, None) => encode_png(rgba).map_err(failed),
        (Format::Jpeg, _) => encode_jpeg(rgba, enc.quality.unwrap_or(JPEG_QUALITY)).map_err(failed),
        (Format::Webp, _) => encode_webp(rgba, enc.lossless, enc.quality.unwrap_or(WEBP_QUALITY)).map_err(failed),
    }
}

/// Whether `bytes` in `actual` need re-encoding to satisfy `wanted` and `enc`.
pub fn needs_encoding(bytes: &[u8], actual: Format, wanted: Format, enc: &Encoding) -> bool {
    actual != wanted || !enc.is_satisfied_by(bytes, wanted)
}

/// Pixel size from the header alone, without decoding.
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()
}

/// Fully decode the image (header checks alone miss damaged pixel data).
/// Animated input is refused: decoding keeps only the first frame, and nothing here can write
/// animation back, so any conversion (even same-format) would silently drop the rest.
pub fn decode(bytes: &[u8]) -> Result<image::RgbaImage> {
    if is_animated(bytes) {
        return Err(Error::other(
            "Input is animated; codex-img converts still images only and would keep just the first frame.",
        ));
    }
    let image = image::load_from_memory(bytes).map_err(|e| Error::other(format!("Input image is damaged or unsupported: {e}")))?;
    Ok(image.to_rgba8())
}

/// Lossless PNG recompression (oxipng): typically halves backend PNGs without touching a pixel.
/// Level 2 is the sweet spot; higher presets took twice as long for the same size.
pub fn optimize_png(png: &[u8]) -> Result<Vec<u8>> {
    oxipng::optimize_from_memory(png, &oxipng::Options::from_preset(2))
        .map_err(|e| Error::other(format!("Could not optimize PNG: {e}")))
}

fn is_opaque(rgba: &image::RgbaImage) -> bool {
    rgba.pixels().all(|p| p.0[3] == 255)
}

/// Lossless PNG, dropping the alpha channel when every pixel is opaque (smaller files).
fn encode_png(rgba: &image::RgbaImage) -> std::result::Result<Vec<u8>, String> {
    let image = if is_opaque(rgba) {
        image::DynamicImage::ImageRgb8(image::DynamicImage::ImageRgba8(rgba.clone()).to_rgb8())
    } else {
        image::DynamicImage::ImageRgba8(rgba.clone())
    };
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(out.into_inner())
}

/// WebP through libwebp: lossy at `quality`, or lossless. Alpha is kept either way (lossy WebP
/// stores it losslessly by default). `exact` stops lossless mode from zeroing the colour of fully
/// transparent pixels, so --lossless really keeps every pixel. The webp crate's `encode*` helpers
/// unwrap internally, which would abort the process (panic = "abort"); encode_advanced doesn't.
fn encode_webp(rgba: &image::RgbaImage, lossless: bool, quality: u8) -> std::result::Result<Vec<u8>, String> {
    let (width, height) = rgba.dimensions();
    let rgb;
    let encoder = if is_opaque(rgba) {
        rgb = image::DynamicImage::ImageRgba8(rgba.clone()).to_rgb8();
        webp::Encoder::from_rgb(rgb.as_raw(), width, height)
    } else {
        webp::Encoder::from_rgba(rgba.as_raw(), width, height)
    };
    let mut config = webp::WebPConfig::new().map_err(|_| "could not initialise the WebP encoder".to_string())?;
    config.lossless = i32::from(lossless);
    config.exact = i32::from(lossless);
    config.alpha_compression = i32::from(!lossless);
    config.quality = f32::from(quality);
    let memory = encoder.encode_advanced(&config).map_err(|e| format!("WebP encoding failed ({e:?})"))?;
    Ok(memory.to_vec())
}

fn encode_jpeg(rgba: &image::RgbaImage, quality: u8) -> std::result::Result<Vec<u8>, String> {
    let rgb = image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let blend = |c: u8| ((u16::from(c) * u16::from(a) + 255 * (255 - u16::from(a))) / 255) as u8;
        image::Rgb([blend(r), blend(g), blend(b)])
    });
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode_image(&rgb).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Palette PNG (colour type 3) with a tRNS chunk for alpha, like pngquant. Images that already
/// have few enough colours keep them exactly; otherwise exoquant picks the palette (k-means) and
/// remaps. Dithering is off by default: it smooths gradients but speckles flat areas, which then
/// compress far worse (a flat icon at 64 colours: 13 KB plain, 109 KB dithered).
fn indexed(rgba: &image::RgbaImage, colors: u16, dither: bool) -> (Vec<exoquant::Color>, Vec<u8>) {
    use exoquant::{ditherer, optimizer, Color};
    let pixels: Vec<Color> = rgba.pixels().map(|p| Color::new(p.0[0], p.0[1], p.0[2], p.0[3])).collect();
    let mut exact: Vec<Color> = Vec::new();
    let mut lookup = std::collections::HashMap::new();
    let mut indices = Vec::with_capacity(pixels.len());
    for pixel in &pixels {
        let next = lookup.len();
        let index = *lookup.entry(*pixel).or_insert(next);
        if index == next {
            if next == usize::from(colors) {
                break;
            }
            exact.push(*pixel);
        }
        indices.push(index as u8);
    }
    let (mut palette, indices) = if indices.len() == pixels.len() {
        (exact, indices)
    } else {
        let (width, colors) = (rgba.width() as usize, usize::from(colors));
        if dither {
            exoquant::convert_to_indexed(&pixels, width, colors, &optimizer::KMeans, &ditherer::FloydSteinberg::new())
        } else {
            exoquant::convert_to_indexed(&pixels, width, colors, &optimizer::KMeans, &ditherer::None)
        }
    };
    // k-means averages alpha too, so hard (0/255) alpha could come back as 254. Keep it hard.
    if pixels.iter().all(|p| p.a == 0 || p.a == 255) {
        for color in &mut palette {
            color.a = if color.a >= 128 { 255 } else { 0 };
        }
    }

    (palette, indices)
}
/// Quantized display pixels without encoding a PNG.
pub fn quantize_rgba(rgba: &image::RgbaImage, colors: u16, dither: bool) -> image::RgbaImage {
    let (palette, indices) = indexed(rgba, colors, dither);
    image::RgbaImage::from_fn(rgba.width(), rgba.height(), |x,y| {
        let color = palette[usize::from(indices[(y * rgba.width() + x) as usize])];
        image::Rgba([color.r, color.g, color.b, color.a])
    })
}
fn quantize_png(rgba: &image::RgbaImage, colors: u16, dither: bool) -> std::result::Result<Vec<u8>, String> {
    let (palette, indices) = indexed(rgba, colors, dither);
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, rgba.width(), rgba.height());
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::High);
    encoder.set_palette(palette.iter().flat_map(|c| [c.r, c.g, c.b]).collect::<Vec<u8>>());
    let alpha: Vec<u8> = palette.iter().map(|c| c.a).collect();
    if let Some(last) = alpha.iter().rposition(|&a| a != 255) {
        encoder.set_trns(alpha[..=last].to_vec());
    }
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&indices).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // 1x1 fully transparent PNG
    pub const PNG: &[u8] = &[137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4, 0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100, 96, 0, 0, 0, 6, 0, 2, 48, 129, 208, 47, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130];


    #[test]
    fn converts_png_to_jpeg_on_white() {
        let png = PNG.to_vec();
        let jpeg = convert(&png, Format::Jpeg, &Encoding::default()).unwrap();
        assert_eq!(sniff(&jpeg), Some(Format::Jpeg));
        let pixel = image::load_from_memory(&jpeg).unwrap().to_rgb8().get_pixel(0, 0).0;
        assert!(pixel.iter().all(|&c| c > 245), "transparent pixel should become white, got {pixel:?}");
    }

    fn gradient() -> Vec<u8> {
        let image = image::RgbaImage::from_fn(64, 64, |x, y| image::Rgba([(x * 4) as u8, (y * 4) as u8, 128, if x < 8 { 0 } else { 255 }]));
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn converts_png_to_lossy_and_lossless_webp() {
        let png = gradient();
        let original = image::load_from_memory(&png).unwrap().to_rgba8();
        let lossless = convert(&png, Format::Webp, &Encoding { lossless: true, ..Default::default() }).unwrap();
        assert_eq!(image::load_from_memory(&lossless).unwrap().to_rgba8(), original, "--lossless must not change pixels");

        let lossy = convert(&png, Format::Webp, &Encoding::default()).unwrap();
        assert_eq!(sniff(&lossy), Some(Format::Webp));
        let decoded = image::load_from_memory(&lossy).unwrap().to_rgba8();
        assert_eq!((decoded.get_pixel(0, 0).0[3], decoded.get_pixel(63, 63).0[3]), (0, 255), "lossy WebP keeps alpha");
        let low = convert(&png, Format::Webp, &Encoding { quality: Some(10), ..Default::default() }).unwrap();
        assert!(low.len() < lossy.len(), "lower quality must be smaller: {} vs {}", low.len(), lossy.len());
    }

    #[test]
    fn webp_passthrough_only_when_the_file_already_matches() {
        let png = gradient();
        let lossless = convert(&png, Format::Webp, &Encoding { lossless: true, ..Default::default() }).unwrap();
        let lossy = convert(&png, Format::Webp, &Encoding::default()).unwrap();
        assert_eq!((webp_is_lossless(&lossless), webp_is_lossless(&lossy)), (Some(true), Some(false)));
        let default = Encoding::default();
        assert!(needs_encoding(&lossless, Format::Webp, Format::Webp, &default), "default is lossy: re-encode lossless input");
        assert!(!needs_encoding(&lossy, Format::Webp, Format::Webp, &default), "lossy input is already lossy");
        assert!(!needs_encoding(&lossless, Format::Webp, Format::Webp, &Encoding { lossless: true, ..default }));
        assert!(needs_encoding(&lossy, Format::Webp, Format::Webp, &Encoding { quality: Some(50), ..default }));
        assert!(needs_encoding(b"RIFF\x04\0\0\0WEBP", Format::Webp, Format::Webp, &default), "unknown kind: re-encode");
        assert_eq!((default.lossy_quality(Format::Webp), default.lossy_quality(Format::Jpeg), default.lossy_quality(Format::Png)), (Some(80), Some(90), None));
    }

    fn chunk(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    #[test]
    fn animated_input_is_refused_not_flattened() {
        // Animated WebP: VP8X + ANIM + ANMF, with the frame data nested inside ANMF.
        let mut body = b"WEBP".to_vec();
        for c in [chunk(b"VP8X", &[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0]), chunk(b"ANIM", &[0; 6]), chunk(b"ANMF", &[0; 16])] {
            body.extend(c);
        }
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&(body.len() as u32).to_le_bytes());
        webp.extend(body);
        assert!(is_animated(&webp));
        assert_eq!(webp_is_lossless(&webp), None);
        assert!(decode(&webp).unwrap_err().message.contains("animated"));

        // APNG: an acTL chunk between IHDR and IDAT.
        let png = gradient();
        let ihdr_end = 8 + 12 + 13;
        let mut apng = png[..ihdr_end].to_vec();
        apng.extend_from_slice(&8u32.to_be_bytes());
        apng.extend_from_slice(b"acTL");
        apng.extend_from_slice(&[0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0]); // 2 frames, loop forever, crc
        apng.extend_from_slice(&png[ihdr_end..]);
        assert!(is_animated(&apng));
        assert!(decode(&apng).unwrap_err().message.contains("animated"));

        // Still images are unaffected.
        let still = convert(&png, Format::Webp, &Encoding::default()).unwrap();
        assert!(!is_animated(&png) && !is_animated(&still));
        assert!(decode(&still).is_ok());
    }

    #[test]
    fn encoding_check_rejects_settings_the_format_cannot_use() {
        let enc = |colors, dither, quality, lossless| Encoding { colors, dither, quality, lossless };
        assert!(enc(None, true, None, false).check(None).unwrap_err().message.contains("--dither"));
        assert!(enc(None, false, Some(80), true).check(None).unwrap_err().message.contains("combined"));
        assert!(enc(Some(8), false, None, false).check(Some(Format::Jpeg)).unwrap_err().message.contains("PNG"));
        assert!(enc(None, false, None, true).check(Some(Format::Png)).unwrap_err().message.contains("WebP"));
        assert!(enc(None, false, Some(80), false).check(Some(Format::Png)).unwrap_err().message.contains("--colors"));
        assert!(enc(None, false, Some(80), false).check(Some(Format::Webp)).is_ok());
        assert!(enc(Some(64), true, None, false).check(Some(Format::Png)).is_ok());
    }

    #[test]
    fn quantizes_png_to_a_palette_and_keeps_alpha() {
        let png = gradient();
        let enc = |colors, dither| Encoding { colors: Some(colors), dither, ..Default::default() };
        assert_eq!(sniff(&convert(&png, Format::Png, &enc(16, true)).unwrap()), Some(Format::Png));
        let small = convert(&png, Format::Png, &enc(16, false)).unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(&small));
        let reader = decoder.read_info().unwrap();
        let info = reader.info();
        assert_eq!(info.color_type, png::ColorType::Indexed);
        assert!(info.palette.as_ref().unwrap().len() <= 16 * 3);
        assert!(info.trns.is_some(), "transparent pixels need a tRNS chunk");
        let decoded = image::load_from_memory(&small).unwrap().to_rgba8();
        assert_eq!((decoded.get_pixel(0, 0).0[3], decoded.get_pixel(63, 63).0[3]), (0, 255));

        // Hard alpha stays hard: k-means must not average it into 254.
        let hard = image::RgbaImage::from_fn(64, 64, |x, y| image::Rgba([(x * 4) as u8, (y * 4) as u8, 90, if (x + y) % 7 == 0 { 0 } else { 255 }]));
        let mut hard_png = std::io::Cursor::new(Vec::new());
        hard.write_to(&mut hard_png, image::ImageFormat::Png).unwrap();
        let quantized = convert(&hard_png.into_inner(), Format::Png, &enc(8, true)).unwrap();
        let alphas = image::load_from_memory(&quantized).unwrap().to_rgba8();
        assert!(alphas.pixels().all(|p| p.0[3] == 0 || p.0[3] == 255));

        // Few distinct colours are kept exactly, with no dithering.
        let exact = convert(PNG, Format::Png, &enc(256, false)).unwrap();
        assert_eq!(image::load_from_memory(&exact).unwrap().to_rgba8().get_pixel(0, 0).0[3], 0);
    }

    #[test]
    fn quantizer_sees_colours_in_a_fixed_order() {
        // exoquant's histogram was a HashMap, whose order changes with every map's random seed.
        // k-means then summed colours in that order, and on real sprites the rounding was enough
        // to pick another palette on each run. The vendored copy keeps it sorted.
        let colours: Vec<exoquant::Color> = (0..2000u32).map(|i| exoquant::Color::new((i * 7) as u8, (i * 13) as u8, (i / 8) as u8, 255)).collect();
        let order = |pixels: &mut dyn Iterator<Item = exoquant::Color>| -> Vec<[u8; 4]> {
            let histogram: exoquant::Histogram = pixels.collect();
            histogram.iter().map(|(c, _)| [c.r, c.g, c.b, c.a]).collect()
        };
        let forward = order(&mut colours.iter().copied());
        assert_eq!(order(&mut colours.iter().rev().copied()), forward);
        assert!(forward.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn optimizes_png_losslessly() {
        let png = gradient();
        let optimized = optimize_png(&png).unwrap();
        assert!(optimized.len() <= png.len());
        assert_eq!(image::load_from_memory(&png).unwrap().to_rgba8(), image::load_from_memory(&optimized).unwrap().to_rgba8());
    }


}

#[cfg(test)]
mod preview_tests {
    use super::*;
    #[test]
    fn raw_quantization_matches_indexed_png_pixels_with_and_without_dither() {
        let image=image::RgbaImage::from_fn(32,24,|x,y|image::Rgba([(x*7) as u8,(y*9) as u8,127,if x<3{0}else{255}]));
        for dither in [false,true] {
            let raw=quantize_rgba(&image,16,dither);
            let png=encode(&image,Format::Png,&Encoding{colors:Some(16),dither,..Default::default()}).unwrap();
            assert_eq!(raw,decode(&png).unwrap());
        }
    }
}
