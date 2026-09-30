//! `codex-img tile`: make a panorama wrap around seamlessly, so it can repeat side by side
//! without a mirror. The image is rolled by half its width, which moves the wrap seam to the
//! centre, and sent as an edit asking the model to join it. The Codex endpoint ignores masks, so
//! the edit comes back redrawn everywhere, if only slightly: only the band around the seam is taken
//! from it, cut along the paths where edit and original agree best, colour-matched, and the image
//! is rolled back to its original framing.
use crate::auth;
use crate::backend::{Backend, Request, Transport};
use crate::cli;
use crate::convert;
use crate::error::{Error, Result};
use crate::images::{self, Encoding, Format, InputImage};
use crate::transform;
use crate::util;
use base64::Engine;
use image::{Rgba, RgbaImage};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Instant;

const PROMPT: &str = "This is a wide panorama whose left and right halves were swapped, so there is a hard vertical seam \
down the exact centre. Repaint only a narrow vertical band around the centre so the scenery continues naturally across it \
with no visible seam: the sky, clouds and landscape must flow smoothly from the left half into the right half. Keep \
everything else exactly as it is, pixel for pixel: same framing, same scale, same colours, same style. Do not move, resize \
or redraw anything outside the centre band.";

/// The cut may move this many pixels sideways per row, so it can follow a shallow slope.
const STEP: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileOptions {
    pub input: String,
    pub output: String,
    pub format: Format,
    /// What the picture shows, to help the model continue it.
    pub prompt: Option<String>,
    pub quality: Option<String>,
    pub preview: Option<String>,
    pub keep_edit: bool,
    pub force: bool,
    pub json: bool,
    pub quiet: bool,
}

pub fn help() -> &'static str {
    r#"Usage:
  codex-img tile <input> -o <output> [options]

Makes a panorama wrap around seamlessly, so it can repeat side by side without a
mirror (skies and backdrops in side-scrolling and racing games). Uses one image of
quota. The image is rolled by half its width, which moves the wrap seam to the
centre, and the model repaints a band there to join it. Only that band is taken
from the edit, cut where the edit and the original agree, and the result keeps the
original framing: every pixel outside the band stays as it was.

Options:
  -o, --output <file>       The seamless image (png, jpeg or webp by extension)
      --prompt <text>       What the picture shows, e.g. "a coastline with a town
                            on a cliff"; helps the model continue it
  -q, --quality <q>         low | medium | high | auto (a hint to the backend)
      --preview <file>      Also write the result twice side by side, cropped
                            around the join, to check it in one image
      --keep-edit           Keep the model's whole edit as <output>.edit.png
      --force               Replace existing output files
      --json                Print a JSON object describing the result
      --quiet               No progress on stderr

Landmarks that were cut in half at the edges (a mountain, a sun) come out whole.
The tile keeps its size; resize it afterwards with `codex-img convert`."#
}

pub fn parse(args: &[String]) -> Result<Option<TileOptions>> {
    let (mut input, mut output, mut prompt, mut quality, mut preview) = (None, None, None, None, None);
    let (mut keep_edit, mut force, mut json, mut quiet) = (false, false, false, false);
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if !arg.starts_with('-') {
            if input.replace(arg.clone()).is_some() {
                return Err(Error::usage("tile takes one input image."));
            }
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if arg.starts_with("--") => (n, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| iter.next().cloned()).ok_or_else(|| Error::usage(format!("{name} needs a value.")));
        match name {
            "-o" | "--output" => output = Some(value()?),
            "--prompt" => prompt = Some(value()?),
            "-q" | "--quality" => quality = cli::one_of("quality", Some(value()?), &["low", "medium", "high", "auto"])?,
            "--preview" => preview = Some(value()?),
            "--keep-edit" => keep_edit = true,
            "--force" => force = true,
            "--json" => json = true,
            "--quiet" => quiet = true,
            "-h" | "--help" => return Ok(None),
            _ => return Err(Error::usage(format!("Unknown tile option: {arg}"))),
        }
    }
    let input = input.ok_or_else(|| Error::usage("tile needs an input image."))?;
    let output = output.ok_or_else(|| Error::usage("tile needs -o <file>, e.g. -o sky-tile.png."))?;
    let format_of = |path: &str| Path::new(path).extension().and_then(|e| e.to_str()).and_then(Format::parse);
    let format = format_of(&output).ok_or_else(|| Error::usage("tile's -o must end in .png, .jpg or .webp."))?;
    if preview.as_deref().is_some_and(|p| format_of(p).is_none()) {
        return Err(Error::usage("--preview must end in .png, .jpg or .webp."));
    }
    if prompt.as_deref().is_some_and(|p| p.chars().count() > 4000) {
        return Err(Error::usage("--prompt must be at most 4,000 characters."));
    }
    Ok(Some(TileOptions { input, output, format, prompt, quality, preview, keep_edit, force, json, quiet }))
}

pub fn run(opts: &TileOptions) -> Result<i32> {
    execute(opts, &Backend::default(), &auth::load_credentials)
}

fn execute(opts: &TileOptions, backend: &Backend, credentials: &dyn Fn() -> Result<auth::Credentials>) -> Result<i32> {
    let started = Instant::now();
    let log = |message: &str| {
        if !opts.quiet {
            eprintln!("{message}");
        }
    };
    let output = PathBuf::from(&opts.output);
    let edit_path = output.with_file_name(format!("{}.edit.png", output.file_stem().unwrap_or_default().to_string_lossy()));
    let mut targets = vec![output.clone()];
    targets.extend(opts.preview.as_ref().map(PathBuf::from));
    if opts.keep_edit {
        targets.push(edit_path.clone());
    }
    // Check before spending quota.
    if let Some(taken) = targets.iter().find(|p| !opts.force && p.exists()) {
        return Err(Error::other(format!("{} already exists; add --force to replace it.", taken.display())));
    }
    let (bytes, _) = convert::read_image(Path::new(&opts.input))?;
    let original = images::decode(&bytes)?;
    let (w, h) = original.dimensions();
    if w < 64 || h < 16 || w > 9999 || h > 9999 {
        return Err(Error::usage("tile needs an image from 64x16 to 9999x9999 pixels."));
    }
    let credentials = credentials()?;
    let rolled = roll(&original, w / 2);
    let reference = InputImage { data_b64: base64::engine::general_purpose::STANDARD.encode(images::encode(&rolled, Format::Png, &Encoding::default())?), format: Format::Png };
    let prompt = match &opts.prompt {
        Some(scene) => format!("{PROMPT} The picture shows: {scene}"),
        None => PROMPT.to_string(),
    };
    let size = format!("{w}x{h}");
    let session_id = util::random_id();
    log("Requesting the seam repair...");
    let request = Request {
        prompt: &prompt,
        transport: Transport::Direct,
        model: None,
        output_format: Format::Png,
        size: Some(&size),
        quality: opts.quality.as_deref(),
        background: Some("opaque"),
        input_images: std::slice::from_ref(&reference),
        session_id: &session_id,
    };
    let generated = backend.generate(&request, &credentials, &|stage| log(stage))?;
    // The quota is spent: keep the edit first if asked, so a failure below doesn't lose it.
    if opts.keep_edit {
        cli::write_output(&edit_path, &generated.bytes, opts.force)?;
    }
    let edited = images::decode(&generated.bytes)?;
    let edited_size = edited.dimensions();
    let edited = if edited_size == (w, h) { edited } else { transform::resample(&edited, w, h) };
    let spliced = splice(&rolled, &edited);
    let tile = roll(&spliced.image, w - w / 2);

    let mut encoded = images::encode(&tile, opts.format, &Encoding::default())?;
    if opts.format == Format::Png {
        encoded = images::optimize_png(&encoded).unwrap_or(encoded);
    }
    cli::write_output(&output, &encoded, opts.force)?;
    if let Some(preview) = &opts.preview {
        let format = Path::new(preview).extension().and_then(|e| e.to_str()).and_then(Format::parse).unwrap_or(Format::Png);
        cli::write_output(Path::new(preview), &images::encode(&join_preview(&tile), format, &Encoding::default())?, opts.force)?;
    }
    let (from, to) = spliced.band;
    let info = json!({
        "path": output.display().to_string(),
        "input": opts.input,
        "size": size,
        "editSize": format!("{}x{}", edited_size.0, edited_size.1),
        // The repaired band now straddles the left and right edges, where the tile meets itself.
        "bandWidth": to - from,
        "preview": opts.preview,
        "edit": opts.keep_edit.then(|| edit_path.display().to_string()),
        "durationMs": started.elapsed().as_millis() as u64,
    });
    if opts.json {
        println!("{info}");
    } else {
        println!("{}", output.display());
    }
    log(&format!("saved {} ({size}, repaired band {} px wide)", output.display(), to - from));
    Ok(0)
}

/// `out[x] = image[(x + shift) % w]`: rolling by half the width moves the edges to the centre.
fn roll(image: &RgbaImage, shift: u32) -> RgbaImage {
    let w = image.width();
    RgbaImage::from_fn(w, image.height(), |x, y| *image.get_pixel((x + shift) % w, y))
}

/// Two copies of the tile side by side, cropped around where they meet (up to 900 px each way).
fn join_preview(tile: &RgbaImage) -> RgbaImage {
    let (w, h) = tile.dimensions();
    let half = (w / 2).min(900);
    RgbaImage::from_fn(2 * half, h, |x, y| *tile.get_pixel((w - half + x) % w, y))
}

struct Spliced {
    image: RgbaImage,
    /// The columns the repaired band spans at its widest, in the rolled image.
    band: (u32, u32),
}

/// Take the band around the centre from `edited` and the rest from `rolled`. The band is where the
/// edit clearly changed things; on each side, the cut follows the path of least disagreement
/// within a wide zone, so it goes around anything the edit redrew (a whole mountain) instead of
/// through it. The spliced pixels are shifted by the edit's tone offset from the original,
/// measured just outside each cut, so no step shows in smooth gradients.
fn splice(rolled: &RgbaImage, edited: &RgbaImage) -> Spliced {
    let (w, h) = rolled.dimensions();
    let (wu, hu) = (w as usize, h as usize);
    let cost: Vec<u32> = rolled
        .pixels()
        .zip(edited.pixels())
        .map(|(a, b)| (0..3).map(|c| u32::from(a.0[c].abs_diff(b.0[c]))).sum())
        .collect();

    // The band: columns around the centre whose mean change stands out from the median column.
    let columns: Vec<f64> = (0..wu).map(|x| (0..hu).map(|y| f64::from(cost[y * wu + x])).sum::<f64>() / hu as f64).collect();
    let smooth: Vec<f64> = (0..wu).map(|x| {
        let (lo, hi) = (x.saturating_sub(4), (x + 5).min(wu));
        columns[lo..hi].iter().sum::<f64>() / (hi - lo) as f64
    }).collect();
    let mut sorted = smooth.clone();
    sorted.sort_by(f64::total_cmp);
    let threshold = (3.0 * sorted[wu / 2]).max(30.0);
    let (mut a, mut b) = (wu / 2, wu / 2);
    while a > 0 && smooth[a - 1] > threshold {
        a -= 1;
    }
    while b + 1 < wu && smooth[b + 1] > threshold {
        b += 1;
    }
    let min_half = wu / 25;
    let (a, b) = (a.min(wu / 2 - min_half), b.max(wu / 2 + min_half));

    let zone = wu / 4;
    let left = best_path(&cost, wu, hu, a.saturating_sub(zone), wu / 2 - 2, a);
    let right = best_path(&cost, wu, hu, wu / 2 + 2, (b + zone).min(wu), b);

    let probe = (wu / 32).max(2);
    let offsets = |columns: &dyn Fn(usize) -> std::ops::Range<usize>| smooth_rows(&(0..hu).map(|y| tone_offset(rolled, edited, y, columns(y))).collect::<Vec<_>>());
    let left_offsets = offsets(&|y| left[y].saturating_sub(probe)..left[y]);
    let right_offsets = offsets(&|y| right[y]..(right[y] + probe).min(wu));

    let mut image = rolled.clone();
    for y in 0..hu {
        let span = (right[y] - left[y]) as f64;
        for x in left[y]..right[y] {
            let t = (x - left[y]) as f64 / span;
            let p = edited.get_pixel(x as u32, y as u32).0;
            let mut out = [0u8; 4];
            for c in 0..3 {
                let offset = left_offsets[y][c] * (1.0 - t) + right_offsets[y][c] * t;
                out[c] = (f64::from(p[c]) + offset).round().clamp(0.0, 255.0) as u8;
            }
            out[3] = p[3];
            image.put_pixel(x as u32, y as u32, Rgba(out));
        }
    }
    let band = (*left.iter().min().unwrap_or(&a) as u32, *right.iter().max().unwrap_or(&b) as u32);
    Spliced { image, band }
}

/// A vertical path (one x per row) within `[lo, hi)` crossing the least disagreement. Moving
/// sideways by d pixels also costs the pixels passed in that row, since the cut runs along them.
/// Each pixel also costs a quarter of its distance from `band`, the band's edge on this side
/// (nothing inside the band): where the edit and the original agree equally, the cut stays close
/// to the band and takes as little of the edit as it can.
fn best_path(cost: &[u32], w: usize, h: usize, lo: usize, hi: usize, band: usize) -> Vec<usize> {
    let n = hi - lo;
    let outside = |x: usize| if band < w / 2 { band.saturating_sub(x) } else { x.saturating_sub(band) };
    let bias: Vec<u64> = (lo..hi).map(|x| (outside(x) / 4) as u64).collect();
    let pixel = |y: usize, i: usize| u64::from(cost[y * w + lo + i]) + bias[i];
    let mut acc: Vec<u64> = (0..n).map(|i| pixel(0, i)).collect();
    let mut back = vec![0usize; n * h];
    let mut prefix = vec![0u64; n + 1];
    for y in 1..h {
        for i in 0..n {
            prefix[i + 1] = prefix[i] + pixel(y, i);
        }
        let mut next = vec![u64::MAX; n];
        for (i, slot) in next.iter_mut().enumerate() {
            let mut best = (u64::MAX, i);
            for src in i.saturating_sub(STEP)..(i + STEP + 1).min(n) {
                let total = acc[src] + prefix[src.max(i)] - prefix[src.min(i)];
                if total < best.0 {
                    best = (total, src);
                }
            }
            *slot = best.0 + pixel(y, i);
            back[y * n + i] = best.1;
        }
        acc = next;
    }
    let mut i = (0..n).min_by_key(|&i| acc[i]).unwrap_or(0);
    let mut path = vec![0; h];
    for y in (0..h).rev() {
        path[y] = lo + i;
        i = back[y * n + i];
    }
    path
}

/// Median of `original - edited` per channel over `columns` of row `y`.
fn tone_offset(rolled: &RgbaImage, edited: &RgbaImage, y: usize, columns: std::ops::Range<usize>) -> [f64; 3] {
    let mut out = [0.0; 3];
    if columns.is_empty() {
        return out;
    }
    for (c, slot) in out.iter_mut().enumerate() {
        let mut diffs: Vec<i32> = columns.clone().map(|x| i32::from(rolled.get_pixel(x as u32, y as u32).0[c]) - i32::from(edited.get_pixel(x as u32, y as u32).0[c])).collect();
        diffs.sort_unstable();
        *slot = f64::from(diffs[diffs.len() / 2]);
    }
    out
}

/// Box-average each channel over 31 rows, so row noise doesn't become stripes.
fn smooth_rows(rows: &[[f64; 3]]) -> Vec<[f64; 3]> {
    let n = rows.len();
    (0..n)
        .map(|y| {
            let (lo, hi) = (y.saturating_sub(15), (y + 16).min(n));
            let mut sum = [0.0; 3];
            for row in &rows[lo..hi] {
                for c in 0..3 {
                    sum[c] += row[c];
                }
            }
            sum.map(|s| s / (hi - lo) as f64)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::tests::{creds, direct_response, serve};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_options() {
        let o = parse(&args(&["sky.png", "-o", "sky-tile.webp", "--prompt", "a harbour", "--preview", "p.png", "--keep-edit"])).unwrap().unwrap();
        assert_eq!((o.format, o.prompt.as_deref(), o.preview.as_deref(), o.keep_edit), (Format::Webp, Some("a harbour"), Some("p.png"), true));
        assert_eq!(parse(&args(&["-h"])).unwrap(), None);
        let usage = |list: &[&str]| parse(&args(list)).unwrap_err().message;
        assert!(usage(&["-o", "t.png"]).contains("input"));
        assert!(usage(&["a.png", "b.png", "-o", "t.png"]).contains("one input"));
        assert!(usage(&["a.png"]).contains("-o"));
        assert!(usage(&["a.png", "-o", "t.gif"]).contains(".png"));
        assert!(usage(&["a.png", "-o", "t.png", "--preview", "p.gif"]).contains("--preview"));
        assert!(usage(&["a.png", "-o", "t.png", "-q", "max"]).contains("--quality"));
    }

    #[test]
    fn rolls_and_rolls_back() {
        let image = RgbaImage::from_fn(10, 2, |x, y| Rgba([x as u8, y as u8, 0, 255]));
        let rolled = roll(&image, 5);
        assert_eq!(rolled.get_pixel(0, 0).0[0], 5);
        assert_eq!(roll(&rolled, 10 - 5), image);
        assert_eq!(join_preview(&image).width(), 10);
    }

    /// A 200x60 rolled panorama: a smooth gradient with a hard seam at the centre, and an edit
    /// that joined it with a wide "mountain" reaching past the centre band on the right, drawn a
    /// shade brighter overall, as the model does.
    fn scene() -> (RgbaImage, RgbaImage) {
        let rolled = RgbaImage::from_fn(200, 60, |x, y| {
            let seam = if x < 100 { 0 } else { 40 };
            Rgba([(60 + y) as u8, (80 + seam) as u8, 200, 255])
        });
        let edited = RgbaImage::from_fn(200, 60, |x, y| {
            let mountain = y > 30 && (x as i32 - 110).unsigned_abs() < (y - 30) * 2;
            let joined = (60 + y + 4) as u8;
            if mountain {
                Rgba([240, 240, 250, 255])
            } else if (90..110).contains(&x) {
                Rgba([joined, 100, 204, 255])
            } else {
                let p = rolled.get_pixel(x, y).0;
                Rgba([p[0] + 4, p[1] + 4, p[2] + 4, 255])
            }
        });
        (rolled, edited)
    }

    #[test]
    fn splices_the_repaired_band_around_what_the_edit_redrew() {
        let (rolled, edited) = scene();
        let spliced = splice(&rolled, &edited).image;
        // Far from the centre, the original stays exactly.
        for x in (0..40).chain(170..200) {
            for y in 0..60 {
                assert_eq!(spliced.get_pixel(x, y), rolled.get_pixel(x, y), "({x}, {y})");
            }
        }
        // The whole mountain comes from the edit, including the part past the band on the right.
        let mountain = |x: u32, y: u32| y > 30 && (x as i32 - 110).unsigned_abs() < (y - 30) * 2;
        for (x, y) in (0..200).flat_map(|x| (0..60).map(move |y| (x, y))).filter(|&(x, y)| mountain(x, y)) {
            let p = spliced.get_pixel(x, y).0;
            assert!(p[0] >= 230 && p[2] >= 240, "mountain pixel ({x}, {y}) was cut: {p:?}");
        }
        // The edit's +4 tone shift is taken out, so the band matches the original's tone.
        assert_eq!(spliced.get_pixel(100, 10).0, [70, 96, 200, 255]);
        // The hard seam at the centre is gone: no column of the top rows jumps by 40.
        for x in 1..200 {
            let (p, q) = (spliced.get_pixel(x - 1, 5).0, spliced.get_pixel(x, 5).0);
            assert!(p[1].abs_diff(q[1]) < 40, "step at x={x}");
        }
    }

    #[test]
    fn repairs_a_panorama_through_the_edits_endpoint() {
        let dir = crate::auth::tests::temp_dir("tile");
        let input = dir.join("sky.png");
        let sky = RgbaImage::from_fn(128, 32, |x, _| Rgba([(x * 2) as u8, 90, 200, 255]));
        sky.save(&input).unwrap();
        let (out, preview) = (dir.join("sky-tile.png"), dir.join("join.png"));
        let opts = parse(&args(&[input.to_str().unwrap(), "-o", out.to_str().unwrap(), "--preview", preview.to_str().unwrap(), "--keep-edit", "--quiet"]))
            .unwrap()
            .unwrap();
        let (backend, captured) = serve(vec![(200, "application/json", direct_response())]);
        assert_eq!(execute(&opts, &backend, &|| Ok(creds())).unwrap(), 0);
        let calls = captured.lock().unwrap();
        assert_eq!(calls[0].path, "/images/edits");
        assert!(calls[0].body["prompt"].as_str().unwrap().contains("halves were swapped"));
        assert_eq!(calls[0].body["size"], "128x32");
        let tile = image::open(&out).unwrap().to_rgba8();
        assert_eq!(tile.dimensions(), (128, 32));
        assert_eq!(tile.get_pixel(64, 0), sky.get_pixel(64, 0), "the middle of the original framing is untouched");
        assert!(preview.is_file() && dir.join("sky-tile.edit.png").is_file());
        assert!(execute(&opts, &backend, &|| Ok(creds())).unwrap_err().message.contains("--force"), "checked before spending quota");
    }
}
