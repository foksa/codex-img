use crate::{error::{Error, Result}, images::{self, Format}, transform};
/// Encode `wanted` from `bytes`; PNG output is also recompressed losslessly (best effort).
/// Also returns the lossy quality applied, if codex-img did a lossy encode.
fn encode_output(bytes: &[u8], actual: Format, wanted: Format, enc: &images::Encoding) -> Result<(Vec<u8>, Option<u8>)> {
    let (converted, quality) = if images::needs_encoding(bytes, actual, wanted, enc) {
        (images::convert(bytes, wanted, enc)?, enc.lossy_quality(wanted))
    } else {
        (bytes.to_vec(), None)
    };
    let out = if wanted == Format::Png { images::optimize_png(&converted).unwrap_or(converted) } else { converted };
    Ok((out, quality))
}

/// Result of `process`: the encoded file, plus what the transform did.
pub struct Processed {
    pub bytes: Vec<u8>,
    /// Lossy quality codex-img applied, if it did a lossy encode.
    pub output_quality: Option<u8>,
    /// Input and output pixel size; None when the bytes were passed on without decoding.
    pub sizes: Option<((u32, u32), (u32, u32))>,
    pub trim: Option<transform::Rect>,
    pub changes: transform::Changes,
}

/// Apply `transform` and encode as `wanted`; PNG output is also recompressed losslessly.
/// `lenient` is for generated images: if they don't decode and nothing asked to reshape them, they
/// go through `encode_output` untouched rather than failing.
pub fn process(bytes: &[u8], actual: Format, wanted: Format, enc: &images::Encoding, transform: &transform::Transform, lenient: bool) -> Result<Processed> {
    let enc = &transform.encoding(wanted, enc);
    let rgba = match images::decode(bytes) {
        Ok(rgba) => rgba,
        Err(_) if lenient && !transform.edits() => {
            let (bytes, output_quality) = encode_output(bytes, actual, wanted, enc)?;
            return Ok(Processed { bytes, output_quality, sizes: None, trim: None, changes: transform::Changes::default() });
        }
        Err(e) => return Err(e),
    };
    process_decoded(bytes, actual, wanted, enc, transform, rgba)
}

/// The same pipeline with a previously decoded input; keep original bytes for exact passthrough.
pub fn process_decoded(bytes: &[u8], actual: Format, wanted: Format, enc: &images::Encoding, transform: &transform::Transform, rgba: image::RgbaImage) -> Result<Processed> {
    let enc = &transform.encoding(wanted, enc);
    let input_size = rgba.dimensions();
    let applied = transform.apply(rgba, wanted, enc)?;
    let sizes = Some((input_size, applied.image.dimensions()));
    let (bytes, output_quality) = if applied.changed || images::needs_encoding(bytes, actual, wanted, enc) {
        let encoded = images::encode(&applied.image, wanted, enc)?;
        let out = if wanted == Format::Png { images::optimize_png(&encoded).unwrap_or(encoded) } else { encoded };
        (out, enc.lossy_quality(wanted))
    } else {
        encode_output(bytes, actual, wanted, enc)?
    };
    Ok(Processed { bytes, output_quality, sizes, trim: applied.trim, changes: applied.changes })
}


use std::path::{Path, PathBuf};
/// What `save_converted` wrote.
pub struct Converted {
    pub path: PathBuf,
    pub input_size: (u32, u32),
    pub size: (u32, u32),
    /// Lossy quality codex-img applied, if it did a lossy encode.
    pub output_quality: Option<u8>,
    pub trim: Option<transform::Rect>,
    /// With `overwrite`: the file already held exactly these bytes, so it was left alone.
    pub unchanged: bool,
    pub changes: transform::Changes,
}

/// `convert` subcommand: apply `transform` and write `bytes` as `wanted` to a new file, or with
/// `overwrite` replace an existing one.
/// Unlike generated images, a local input that doesn't fully decode is an error, not something to
/// copy through: the fast paths (same format, best-effort optimization) would otherwise pass it on.
pub fn save_converted(bytes: &[u8], wanted: Format, enc: &images::Encoding, transform: &transform::Transform, path: &Path, overwrite: bool) -> Result<Converted> {
    let actual = images::sniff(bytes).ok_or_else(|| Error::other("Input is not a PNG, JPEG or WebP image."))?;
    let processed = process(bytes, actual, wanted, enc, transform, false)?;
    let (input_size, size) = processed.sizes.ok_or_else(|| Error::other("Input image could not be decoded."))?;
    let unchanged = !crate::output::write_output(path, &processed.bytes, overwrite)?;
    Ok(Converted { path: path.to_path_buf(), input_size, size, output_quality: processed.output_quality, trim: processed.trim, unchanged, changes: processed.changes })
}

