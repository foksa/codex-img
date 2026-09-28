use crate::error::{Error, Result};
use base64::Engine;

pub const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_EDIT_IMAGES: usize = 5;
const MAX_INPUT_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_TOTAL_INPUT_BYTES: u64 = 50 * 1024 * 1024;
const JPEG_QUALITY: u8 = 90;

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

/// Decode `bytes` (PNG, JPEG or WebP) and re-encode as `wanted`. JPEG has no alpha, so transparency
/// is flattened onto white. `colors` reduces a PNG to an indexed palette of at most that many colours.
pub fn convert(bytes: &[u8], wanted: Format, colors: Option<u16>, dither: bool) -> Result<Vec<u8>> {
    let failed = |e: String| Error::other(format!("Could not convert image to {}: {e}", wanted.name()));
    let rgba = image::load_from_memory(bytes).map_err(|e| failed(e.to_string()))?.to_rgba8();
    match (wanted, colors) {
        (Format::Png, Some(colors)) => quantize_png(&rgba, colors, dither).map_err(failed),
        (Format::Png, None) => encode(&rgba, image::ImageFormat::Png).map_err(failed),
        (Format::Jpeg, _) => encode_jpeg(&rgba).map_err(failed),
        (Format::Webp, _) => encode(&rgba, image::ImageFormat::WebP).map_err(failed),
    }
}

/// Fully decode the image (header checks alone miss damaged pixel data); returns its pixel size.
pub fn validate(bytes: &[u8]) -> Result<(u32, u32)> {
    let image = image::load_from_memory(bytes).map_err(|e| Error::other(format!("Input image is damaged or unsupported: {e}")))?;
    Ok((image.width(), image.height()))
}

/// Lossless PNG recompression (oxipng): typically halves backend PNGs without touching a pixel.
/// Level 2 is the sweet spot; higher presets took twice as long for the same size.
pub fn optimize_png(png: &[u8]) -> Result<Vec<u8>> {
    oxipng::optimize_from_memory(png, &oxipng::Options::from_preset(2))
        .map_err(|e| Error::other(format!("Could not optimize PNG: {e}")))
}

/// Lossless encode, dropping the alpha channel when every pixel is opaque (smaller files).
fn encode(rgba: &image::RgbaImage, format: image::ImageFormat) -> std::result::Result<Vec<u8>, String> {
    let image = if rgba.pixels().all(|p| p.0[3] == 255) {
        image::DynamicImage::ImageRgb8(image::DynamicImage::ImageRgba8(rgba.clone()).to_rgb8())
    } else {
        image::DynamicImage::ImageRgba8(rgba.clone())
    };
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, format).map_err(|e| e.to_string())?;
    Ok(out.into_inner())
}

fn encode_jpeg(rgba: &image::RgbaImage) -> std::result::Result<Vec<u8>, String> {
    let rgb = image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let blend = |c: u8| ((u16::from(c) * u16::from(a) + 255 * (255 - u16::from(a))) / 255) as u8;
        image::Rgb([blend(r), blend(g), blend(b)])
    });
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY).encode_image(&rgb).map_err(|e| e.to_string())?;
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
        let jpeg = convert(&png, Format::Jpeg, None, false).unwrap();
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
    fn converts_png_to_lossless_webp() {
        let png = gradient();
        let webp = convert(&png, Format::Webp, None, false).unwrap();
        assert_eq!(sniff(&webp), Some(Format::Webp));
        let (a, b) = (image::load_from_memory(&png).unwrap().to_rgba8(), image::load_from_memory(&webp).unwrap().to_rgba8());
        assert_eq!(a, b, "webp output must be lossless");
    }

    #[test]
    fn quantizes_png_to_a_palette_and_keeps_alpha() {
        let png = gradient();
        assert_eq!(sniff(&convert(&png, Format::Png, Some(16), true).unwrap()), Some(Format::Png));
        let small = convert(&png, Format::Png, Some(16), false).unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(&small));
        let reader = decoder.read_info().unwrap();
        let info = reader.info();
        assert_eq!(info.color_type, png::ColorType::Indexed);
        assert!(info.palette.as_ref().unwrap().len() <= 16 * 3);
        assert!(info.trns.is_some(), "transparent pixels need a tRNS chunk");
        let decoded = image::load_from_memory(&small).unwrap().to_rgba8();
        assert_eq!((decoded.get_pixel(0, 0).0[3], decoded.get_pixel(63, 63).0[3]), (0, 255));

        // Few distinct colours are kept exactly, with no dithering.
        let exact = convert(&base64::engine::general_purpose::STANDARD.decode(PNG_B64).unwrap(), Format::Png, Some(256), false).unwrap();
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
