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

/// Convert PNG to JPEG, compositing any transparency onto white (JPEG has no alpha).
pub fn png_to_jpeg(png: &[u8]) -> Result<Vec<u8>> {
    let failed = |e: image::ImageError| Error::other(format!("Could not convert PNG to JPEG: {e}"));
    let rgba = image::load_from_memory_with_format(png, image::ImageFormat::Png).map_err(failed)?.to_rgba8();
    let rgb = image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let blend = |c: u8| ((u16::from(c) * u16::from(a) + 255 * (255 - u16::from(a))) / 255) as u8;
        image::Rgb([blend(r), blend(g), blend(b)])
    });
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY).encode_image(&rgb).map_err(failed)?;
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
        let jpeg = png_to_jpeg(&png).unwrap();
        assert_eq!(sniff(&jpeg), Some(Format::Jpeg));
        let pixel = image::load_from_memory(&jpeg).unwrap().to_rgb8().get_pixel(0, 0).0;
        assert!(pixel.iter().all(|&c| c > 245), "transparent pixel should become white, got {pixel:?}");
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
