use crate::{error::{Error, Result}, images::{self, Format}, pixel_grid, transform};
use serde_json::{json, Value};
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
/// A `--pixel-grid` refusal leaves the image un-snapped (see `refusal`); batch conversion keeps
/// that with a warning, `convert` checks with `prepare` first and writes nothing.
pub fn save_converted(bytes: &[u8], wanted: Format, enc: &images::Encoding, transform: &transform::Transform, path: &Path, overwrite: bool) -> Result<Converted> {
    write_converted(prepare(bytes, wanted, enc, transform)?, path, overwrite)
}

/// The conversion of `save_converted`, without writing it.
pub fn prepare(bytes: &[u8], wanted: Format, enc: &images::Encoding, transform: &transform::Transform) -> Result<Processed> {
    let actual = images::sniff(bytes).ok_or_else(|| Error::other("Input is not a PNG, JPEG or WebP image."))?;
    let processed = process(bytes, actual, wanted, enc, transform, false)?;
    if processed.sizes.is_none() { return Err(Error::other("Input image could not be decoded.")); }
    Ok(processed)
}

/// Write what `prepare` made.
pub fn write_converted(processed: Processed, path: &Path, overwrite: bool) -> Result<Converted> {
    let (input_size, size) = processed.sizes.ok_or_else(|| Error::other("Input image could not be decoded."))?;
    let unchanged = !crate::output::write_output(path, &processed.bytes, overwrite)?;
    Ok(Converted { path: path.to_path_buf(), input_size, size, output_quality: processed.output_quality, trim: processed.trim, unchanged, changes: processed.changes })
}

/// The `--pixel-grid` refusal in `changes`, if the grid was asked for and not found.
pub fn refusal(changes: &transform::Changes) -> Option<&pixel_grid::Refusal> {
    changes.pixel_grid.as_ref().and_then(|found| found.as_ref().err())
}

/// `pixelGrid` in `--json` output: the grid's measurements, or the refusal's reasons and the
/// measurements of the best grid it rejected (null when there was none).
pub fn pixel_grid_json(found: &std::result::Result<pixel_grid::Grid, pixel_grid::Refusal>) -> Value {
    fn measurements(grid: &pixel_grid::Grid) -> Value {
        let full = grid.to_json();
        let mut out = serde_json::Map::new();
        for key in ["period", "offset", "mode", "cells", "capture", "purity", "coherence"] { out.insert(key.into(), full[key].clone()); }
        Value::Object(out)
    }
    match found {
        Ok(grid) => measurements(grid),
        Err(refusal) => json!({
            "refused": true,
            "reasons": refusal.reasons.iter().map(|r| r.name()).collect::<Vec<_>>(),
            "measurements": refusal.grid.as_deref().map(measurements),
        }),
    }
}

/// A refusal for people: why, what the best grid measured, and the way out.
pub fn refusal_message(refusal: &pixel_grid::Refusal) -> String {
    let measured = refusal.grid.as_deref().map(|g| format!(
        " (best grid: {:.2} px cells, {}x{} cells, capture {:.3}, purity {:.3})",
        g.period[0], g.cells[0], g.cells[1], g.capture, g.purity,
    )).unwrap_or_default();
    format!("--pixel-grid: {refusal}{measured}. Not pixel art, or not on a grid it could find; if it is pixel art, pass the cell size, e.g. --pixel-grid 8")
}

