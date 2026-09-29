use crate::error::{Error, Result};
use base64::Engine;

pub const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_EDIT_IMAGES: usize = 5;
const MAX_INPUT_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_TOTAL_INPUT_BYTES: u64 = 50 * 1024 * 1024;
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

/// Decode and validate backend image data, returning the format the bytes actually have.
pub fn decode_image_data(base64_data: &str) -> Result<(Vec<u8>, Format)> {
    if base64_data.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
        return Err(Error::other("Codex image exceeded the 32 MiB size limit."));
    }
    let value = base64_data.trim();
    let invalid = || Error::other("Codex returned invalid base64 image data.");
    if value.is_empty() || value.len() % 4 != 0 {
        return Err(invalid());
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(value).map_err(|_| invalid())?;
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return Err(invalid());
    }
    let format = sniff(&bytes).ok_or_else(|| Error::other("Codex returned image data that is not PNG, JPEG or WebP."))?;
    Ok((bytes, format))
}

#[derive(Debug, Clone)]
pub struct InputImage {
    pub data_b64: String,
    pub format: Format,
}

impl InputImage {
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.format.mime(), self.data_b64)
    }
}

fn read_input_image(path: &str) -> std::result::Result<Vec<u8>, String> {
    // Check metadata first so named pipes and directories are never opened for reading.
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("Referenced images must be regular files.".into());
    }
    if meta.len() > MAX_INPUT_IMAGE_BYTES {
        return Err("Referenced image exceeds 20 MiB.".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_INPUT_IMAGE_BYTES {
        return Err("Referenced image exceeds 20 MiB.".into());
    }
    Ok(bytes)
}

pub fn load_input_images(paths: &[String]) -> Result<Vec<InputImage>> {
    if paths.len() > MAX_EDIT_IMAGES {
        return Err(Error::usage(format!("At most {MAX_EDIT_IMAGES} reference images are supported.")));
    }
    let mut images = Vec::with_capacity(paths.len());
    let mut total = 0u64;
    for path in paths {
        let bytes = read_input_image(path).map_err(|e| Error::other(format!("Unable to read reference image {path}: {e}")))?;
        let format = sniff(&bytes).ok_or_else(|| Error::other(format!("Reference image is not PNG, JPEG or WebP: {path}")))?;
        total += bytes.len() as u64;
        if total > MAX_TOTAL_INPUT_BYTES {
            return Err(Error::other("Reference images exceed 50 MiB in total."));
        }
        images.push(InputImage { data_b64: base64::engine::general_purpose::STANDARD.encode(&bytes), format });
    }
    Ok(images)
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
fn quantize_png(rgba: &image::RgbaImage, colors: u16, dither: bool) -> std::result::Result<Vec<u8>, String> {
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
    let (palette, indices) = if indices.len() == pixels.len() {
        (exact, indices)
    } else {
        let (width, colors) = (rgba.width() as usize, usize::from(colors));
        if dither {
            exoquant::convert_to_indexed(&pixels, width, colors, &optimizer::KMeans, &ditherer::FloydSteinberg::new())
        } else {
            exoquant::convert_to_indexed(&pixels, width, colors, &optimizer::KMeans, &ditherer::None)
        }
    };

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
    pub const PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

    #[test]
    fn decode_validates_base64_and_magic_bytes() {
        let (bytes, format) = decode_image_data(PNG_B64).unwrap();
        assert_eq!(format, Format::Png);
        assert!(bytes.len() > 8);
        let gif = base64::engine::general_purpose::STANDARD.encode("GIF89a-not-supported");
        assert!(decode_image_data(&gif).unwrap_err().message.contains("not PNG, JPEG or WebP"));
        assert!(decode_image_data("not base64!").unwrap_err().message.contains("invalid base64"));
    }

    #[test]
    fn converts_png_to_jpeg_on_white() {
        let png = base64::engine::general_purpose::STANDARD.decode(PNG_B64).unwrap();
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

        // Few distinct colours are kept exactly, with no dithering.
        let exact = convert(&base64::engine::general_purpose::STANDARD.decode(PNG_B64).unwrap(), Format::Png, &enc(256, false)).unwrap();
        assert_eq!(image::load_from_memory(&exact).unwrap().to_rgba8().get_pixel(0, 0).0[3], 0);
    }

    #[test]
    fn optimizes_png_losslessly() {
        let png = gradient();
        let optimized = optimize_png(&png).unwrap();
        assert!(optimized.len() <= png.len());
        assert_eq!(image::load_from_memory(&png).unwrap().to_rgba8(), image::load_from_memory(&optimized).unwrap().to_rgba8());
    }

    #[test]
    fn loads_reference_images_and_rejects_non_images() {
        let dir = crate::auth::tests::temp_dir("inputs");
        let png = dir.join("a.png");
        std::fs::write(&png, base64::engine::general_purpose::STANDARD.decode(PNG_B64).unwrap()).unwrap();
        let text = dir.join("b.txt");
        std::fs::write(&text, "hello").unwrap();
        let images = load_input_images(&[png.display().to_string()]).unwrap();
        assert_eq!(images[0].format, Format::Png);
        assert!(images[0].data_url().starts_with("data:image/png;base64,iVBOR"));
        assert!(load_input_images(&[text.display().to_string()]).is_err());
        assert!(load_input_images(&[dir.display().to_string()]).unwrap_err().message.contains("regular files"));
    }
}
