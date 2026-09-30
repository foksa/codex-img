use crate::backend::{Generated, DEFAULT_ROUTING_MODEL};
use crate::error::{Error, Result};
use crate::images::{self, Format, MAX_EDIT_IMAGES};
use crate::transform;
use crate::util;
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const MAX_PROMPT_CHARS: usize = 32_000;

pub fn help() -> String {
    format!(
        r#"codex-img {VERSION} - generate images with your ChatGPT/Codex subscription

Usage:
  codex-img [options] "<prompt>"
  echo "<prompt>" | codex-img [options] -
  codex-img status [--json]   Check the Codex login offline (uses no quota)
  codex-img convert <input>... [-o path] [-f fmt] [-c n] [--trim] [--resize WxH]
                              Convert, trim or resize existing images locally (no quota);
                              see `codex-img convert --help`
  codex-img sheet <input>... -o sheet.png
                              Lay images out in one labelled grid to review a batch;
                              see `codex-img sheet --help`
  codex-img batch <spec.json> [key or folder...]
                              Generate the missing images of a JSON asset spec, then
                              convert them all; see `codex-img batch --help`
  codex-img tile <panorama> -o tile.png
                              Make a panorama wrap around seamlessly (one image of
                              quota); see `codex-img tile --help`

Options:
  -o, --output <path>       Output file or directory (default: current directory)
  -i, --image <path>        Reference image to edit/compose (repeatable, max {MAX_EDIT_IMAGES})
  -f, --format <fmt>        png | jpeg | webp (default: from -o extension, else png).
                            The backend returns PNG; jpeg and webp are converted
                            locally (webp is lossy unless --lossless)
      --output-quality <n>  1-100 for jpeg (default 90) and lossy webp (default 80).
                            Not the same as -q, which is a hint to the backend
      --lossless            Lossless webp (bigger; exact pixels)
  -c, --colors <n>          Quantize PNG output to a palette of n colours (2-256);
                            keeps transparency, often 10x+ smaller on flat art.
                            PNG output is always recompressed losslessly
      --dither              Dither when quantizing (smoother gradients/photos,
                            larger files)
      --trim[=pad]          Crop transparent borders to the visible pixels
      --hard-alpha[=n]      Every pixel fully solid (alpha above n, default 16) or
                            fully transparent; for pixel art
      --key <colour>        Remove an unwanted background (painted ground, sky)
                            connected to the transparent area or the border: auto
                            (sampled along the edges), a colour name (blue, white,
                            green, ...) or #rrggbb[:tol]; repeatable. --key-region
                            bottom:30% (or top:40%,left:15%) limits it to bands;
                            --key-cut[=f] first cuts lines that are mostly key colour;
                            --key-spread <step> follows gradients from what's removed
      --trim-density <f>    With --trim, also drop sparse bottom rows (under f of the
                            fullest row), so leftover specks don't make sprites float
      --resize <size>       WxH, Wx or xH, after --trim; --fit inside (default) |
                            cover | contain | fill for WxH. With --trim, --resize or
                            --hard-alpha the original is also kept, as <name>.raw.png.
                            --no-enlarge makes --resize only shrink
      --no-bleed            Keep the colour under fully transparent pixels (by
                            default PNG and lossless webp get the nearest edge colour)
  -s, --size <WxH>          Shape hint, e.g. 1536x1024, 1024x1536, auto. For exact
                            pixels add --resize WxH --fit cover
  -q, --quality <q>         low | medium | high | auto
  -b, --background <bg>     transparent | opaque | auto
      --via-responses       Fallback route: a routing model calls the image tool
                            (the prompt may be rewritten)
  -m, --model <model>       Routing model for --via-responses (default: {DEFAULT_ROUTING_MODEL})
  -n, --count <n>           Number of images, generated in parallel (default: 1)
      --json                Print one JSON object per image to stdout
      --quiet               No progress on stderr
  -h, --help                Show help
  -v, --version             Show version

Uses the ChatGPT login stored by `codex login` ($CODEX_HOME/auth.json).
Exit codes: 0 ok, 1 error, 2 auth, 3 quota, 4 moderation, 64 usage.

Examples:
  codex-img "flat vector red fox in snow" -o fox.png --json
  codex-img "race car sprite, side view" -b transparent --trim=4 --resize 400x -o car.png"#
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub prompt: String,
    pub output: Option<String>,
    pub images: Vec<String>,
    pub format: Option<Format>,
    pub size: Option<String>,
    pub quality: Option<String>,
    pub background: Option<String>,
    /// Local encoding: palette, dithering, JPEG/WebP quality, lossless WebP.
    pub encoding: images::Encoding,
    /// Trim, resize and edge bleed applied to each generated image before saving.
    pub transform: transform::Transform,
    pub model: Option<String>,
    pub via_responses: bool,
    pub count: usize,
    pub json: bool,
    pub quiet: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Status { json: bool },
    Convert(crate::convert::ConvertOptions),
    ConvertHelp,
    Sheet(crate::sheet::SheetOptions),
    SheetHelp,
    Batch(crate::batch::BatchOptions),
    BatchHelp,
    Tile(crate::tile::TileOptions),
    TileHelp,
    Run(Options),
}

pub fn one_of(name: &str, value: Option<String>, allowed: &[&str]) -> Result<Option<String>> {
    match value {
        Some(v) if !allowed.contains(&v.as_str()) => Err(Error::usage(format!("--{name} must be one of: {}", allowed.join(", ")))),
        other => Ok(other),
    }
}

pub fn is_size(value: &str) -> bool {
    let valid = |p: &str| (2..=5).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()) && !p.starts_with('0');
    value == "auto" || value.split_once('x').is_some_and(|(w, h)| valid(w) && valid(h))
}

pub fn parse(args: &[String]) -> Result<Command> {
    if args.first().is_some_and(|a| a == "status") && args[1..].iter().all(|a| a == "--json") {
        return Ok(Command::Status { json: args.len() > 1 });
    }
    // Always subcommands: a prompt starting with one of these words must be quoted ("convert ...").
    if args.first().is_some_and(|a| a == "convert") {
        return Ok(crate::convert::parse(&args[1..])?.map_or(Command::ConvertHelp, Command::Convert));
    }
    if args.first().is_some_and(|a| a == "sheet") {
        return Ok(crate::sheet::parse(&args[1..])?.map_or(Command::SheetHelp, Command::Sheet));
    }
    if args.first().is_some_and(|a| a == "batch") {
        return Ok(crate::batch::parse(&args[1..])?.map_or(Command::BatchHelp, Command::Batch));
    }
    if args.first().is_some_and(|a| a == "tile") {
        return Ok(crate::tile::parse(&args[1..])?.map_or(Command::TileHelp, Command::Tile));
    }
    let mut values: Vec<(&'static str, String)> = Vec::new();
    let mut flags: Vec<&'static str> = Vec::new();
    let mut positionals: Vec<String> = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            positionals.extend(iter.by_ref().cloned());
            break;
        }
        if arg == "-" || !arg.starts_with('-') {
            positionals.push(arg.clone());
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if arg.starts_with("--") => (n, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let key = match name {
            "-o" | "--output" => "output",
            "-i" | "--image" => "image",
            "-f" | "--format" => "format",
            "-s" | "--size" => "size",
            "-q" | "--quality" => "quality",
            "-b" | "--background" => "background",
            "-m" | "--model" => "model",
            "-n" | "--count" => "count",
            "-c" | "--colors" => "colors",
            "--output-quality" => "output-quality",
            "--json" => "json",
            "--quiet" => "quiet",
            "--via-responses" => "via-responses",
            "--dither" => "dither",
            "--lossless" => "lossless",
            "--trim" => "trim",
            "--hard-alpha" => "hard-alpha",
            "--resize" => "resize",
            "--fit" => "fit",
            "--no-bleed" => "no-bleed",
            "--no-enlarge" => "no-enlarge",
            "--key" => "key",
            "--key-region" => "key-region",
            "--trim-density" => "trim-density",
            "--key-cut" => "key-cut",
            "--key-spread" => "key-spread",
            "-h" | "--help" => "help",
            "-v" | "--version" => "version",
            _ => return Err(Error::usage(format!("Unknown option: {arg}"))),
        };
        // --trim and --hard-alpha take values only inline (--trim=8): a bare word after them is the prompt.
        if matches!(key, "json" | "quiet" | "via-responses" | "dither" | "lossless" | "no-bleed" | "no-enlarge" | "help" | "version")
            || (matches!(key, "trim" | "hard-alpha" | "key-cut") && inline.is_none())
        {
            flags.push(key);
        } else {
            let value = match inline {
                Some(v) => v,
                None => iter.next().cloned().ok_or_else(|| Error::usage(format!("{name} needs a value.")))?,
            };
            values.push((key, value));
        }
    }
    if flags.contains(&"help") {
        return Ok(Command::Help);
    }
    if flags.contains(&"version") {
        return Ok(Command::Version);
    }
    if positionals.is_empty() {
        return Err(Error::usage("Missing prompt."));
    }
    let last = |key: &str| values.iter().rev().find(|(k, _)| *k == key).map(|(_, v)| v.clone());
    let output = last("output");
    let format = match last("format") {
        Some(f) => Some(Format::parse(&f).ok_or_else(|| Error::usage("--format must be one of: png, jpeg, webp"))?),
        None => output.as_deref().and_then(|o| Path::new(o).extension()).and_then(|e| e.to_str()).and_then(Format::parse),
    };
    let size = last("size");
    if size.as_deref().is_some_and(|s| !is_size(s)) {
        return Err(Error::usage("--size must be WIDTHxHEIGHT or auto."));
    }
    let count = match last("count") {
        None => 1,
        Some(n) => n.parse::<usize>().ok().filter(|n| (1..=10).contains(n)).ok_or_else(|| Error::usage("--count must be an integer from 1 to 10."))?,
    };
    let encoding = images::Encoding {
        colors: last("colors").map(|n| parse_colors(&n)).transpose()?,
        dither: flags.contains(&"dither"),
        quality: last("output-quality").map(|n| parse_output_quality(&n)).transpose()?,
        lossless: flags.contains(&"lossless"),
    };
    // The format may still be unknown here (it defaults to PNG); generate() checks again.
    encoding.check(format)?;
    let transform = transform::Transform {
        hard_alpha: match last("hard-alpha") {
            Some(threshold) => Some(transform::parse_hard_alpha(&threshold)?),
            None => flags.contains(&"hard-alpha").then_some(transform::FAINT_ALPHA),
        },
        keys: values.iter().filter(|(k, _)| *k == "key").map(|(_, v)| transform::Key::parse(v)).collect::<Result<_>>()?,
        key_region: last("key-region").map(|v| transform::Region::parse(&v)).transpose()?,
        key_spread: last("key-spread").map(|v| transform::parse_key_spread(&v)).transpose()?,
        key_cut: match last("key-cut") {
            Some(share) => Some(transform::parse_key_cut(&share)?),
            None => flags.contains(&"key-cut").then_some(transform::KEY_CUT),
        },
        trim: match last("trim") {
            Some(padding) => Some(transform::parse_trim_padding(&padding)?),
            None => flags.contains(&"trim").then_some(0),
        },
        trim_density: last("trim-density").map(|v| transform::Density::parse(&v)).transpose()?,
        resize: last("resize").map(|v| transform::Resize::parse(&v)).transpose()?,
        fit: last("fit").map(|v| transform::Fit::parse(&v)).transpose()?,
        no_bleed: flags.contains(&"no-bleed"),
        no_enlarge: flags.contains(&"no-enlarge"),
    };
    transform.check()?;
    let via_responses = flags.contains(&"via-responses");
    let model = last("model");
    if model.is_some() && !via_responses {
        return Err(Error::usage("--model only applies with --via-responses; the direct route has no routing model."));
    }
    let images: Vec<String> = values.iter().filter(|(k, _)| *k == "image").map(|(_, v)| v.clone()).collect();
    if images.len() > MAX_EDIT_IMAGES {
        return Err(Error::usage(format!("At most {MAX_EDIT_IMAGES} --image references are supported.")));
    }
    Ok(Command::Run(Options {
        prompt: positionals.join(" "),
        output,
        images,
        format,
        size,
        quality: one_of("quality", last("quality"), &["low", "medium", "high", "auto"])?,
        background: one_of("background", last("background"), &["transparent", "opaque", "auto"])?,
        encoding,
        transform,
        model,
        via_responses,
        count,
        json: flags.contains(&"json"),
        quiet: flags.contains(&"quiet"),
    }))
}

fn sanitize_id(id: &str) -> String {
    let clean: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    clean.chars().skip(clean.chars().count().saturating_sub(12)).collect()
}

/// Resolve where image `index` of `count` goes. Directories get generated names; files get -N suffixes when count > 1.
pub fn output_path(output: Option<&str>, format: Format, id: &str, index: usize, count: usize, now: i64) -> PathBuf {
    let ext = format.extension();
    let generated = format!("codex-img-{}-{}.{ext}", util::stamp(now), sanitize_id(id));
    let cwd = std::env::current_dir().unwrap_or_default();
    let Some(output) = output else { return cwd.join(generated) };
    let path = cwd.join(output);
    if output.ends_with('/') || path.is_dir() {
        return path.join(generated);
    }
    let current = path.extension().map(|e| e.to_string_lossy().into_owned());
    if count == 1 {
        return if current.is_some() { path } else { path.with_extension(ext) };
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!("{stem}-{}.{}", index + 1, current.as_deref().unwrap_or(ext)))
}

/// Checked before any quota is spent, so a generated image isn't thrown away: every file `-o`
/// names (with `-N` suffixes, and the kept original when `transform` edits pixels) must be free,
/// and its folder creatable. Names made up in a folder are unique. `save_image` still refuses to
/// overwrite a file that appears while the request runs.
pub fn check_output(output: Option<&str>, format: Format, count: usize, transform: &transform::Transform) -> Result<()> {
    let Some(output) = output else { return Ok(()) };
    if output.ends_with('/') || std::env::current_dir().unwrap_or_default().join(output).is_dir() {
        return Ok(());
    }
    for index in 0..count {
        let target = output_path(Some(output), format, "", index, count, 0);
        let mut targets = vec![target.clone()];
        if transform.edits() {
            // The original keeps the backend's format, which is PNG unless asked for otherwise.
            targets.push(raw_path(&target, Format::Png));
            targets.push(raw_path(&target, format));
        }
        // symlink_metadata, so a dangling link counts as taken too: creating the file would fail.
        if let Some(taken) = targets.iter().find(|p| p.symlink_metadata().is_ok()) {
            return Err(Error::other(format!("{} already exists; codex-img never overwrites it. Choose another -o or delete it.", taken.display())));
        }
        create_parent(&target)?;
    }
    Ok(())
}

pub fn parse_colors(value: &str) -> Result<u16> {
    value.parse::<u16>().ok().filter(|n| (2..=256).contains(n)).ok_or_else(|| Error::usage("--colors must be an integer from 2 to 256."))
}

pub fn parse_output_quality(value: &str) -> Result<u8> {
    value.parse::<u8>().ok().filter(|n| (1..=100).contains(n)).ok_or_else(|| Error::usage("--output-quality must be an integer from 1 to 100."))
}

fn create_parent(path: &Path) -> Result<()> {
    match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(parent) => std::fs::create_dir_all(parent).map_err(|e| Error::other(format!("Could not create {}: {e}", parent.display()))),
        None => Ok(()),
    }
}

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
struct Processed {
    bytes: Vec<u8>,
    /// Lossy quality codex-img applied, if it did a lossy encode.
    output_quality: Option<u8>,
    /// Input and output pixel size; None when the bytes were passed on without decoding.
    sizes: Option<((u32, u32), (u32, u32))>,
    trim: Option<transform::Rect>,
}

/// Apply `transform` and encode as `wanted`; PNG output is also recompressed losslessly.
/// `lenient` is for generated images: if they don't decode and nothing asked to reshape them, they
/// go through `encode_output` untouched rather than failing.
fn process(bytes: &[u8], actual: Format, wanted: Format, enc: &images::Encoding, transform: &transform::Transform, lenient: bool) -> Result<Processed> {
    let rgba = match images::decode(bytes) {
        Ok(rgba) => rgba,
        Err(_) if lenient && !transform.edits() => {
            let (bytes, output_quality) = encode_output(bytes, actual, wanted, enc)?;
            return Ok(Processed { bytes, output_quality, sizes: None, trim: None });
        }
        Err(e) => return Err(e),
    };
    let input_size = rgba.dimensions();
    let applied = transform.apply(rgba, wanted, enc)?;
    let sizes = Some((input_size, applied.image.dimensions()));
    let (bytes, output_quality) = if applied.changed {
        let encoded = images::encode(&applied.image, wanted, enc)?;
        let out = if wanted == Format::Png { images::optimize_png(&encoded).unwrap_or(encoded) } else { encoded };
        (out, enc.lossy_quality(wanted))
    } else {
        encode_output(bytes, actual, wanted, enc)?
    };
    Ok(Processed { bytes, output_quality, sizes, trim: applied.trim })
}

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
}

/// `convert` subcommand: apply `transform` and write `bytes` as `wanted` to a new file, or with
/// `overwrite` replace an existing one.
/// Unlike generated images, a local input that doesn't fully decode is an error, not something to
/// copy through: the fast paths (same format, best-effort optimization) would otherwise pass it on.
pub fn save_converted(bytes: &[u8], wanted: Format, enc: &images::Encoding, transform: &transform::Transform, path: &Path, overwrite: bool) -> Result<Converted> {
    let actual = images::sniff(bytes).ok_or_else(|| Error::other("Input is not a PNG, JPEG or WebP image."))?;
    let processed = process(bytes, actual, wanted, enc, transform, false)?;
    let (input_size, size) = processed.sizes.ok_or_else(|| Error::other("Input image could not be decoded."))?;
    let unchanged = !write_output(path, &processed.bytes, overwrite)?;
    Ok(Converted { path: path.to_path_buf(), input_size, size, output_quality: processed.output_quality, trim: processed.trim, unchanged })
}

/// Write `bytes` to a new file at `path`, creating its directory. With `overwrite`, replace an
/// existing file instead; returns false when it already held exactly these bytes.
pub fn write_output(path: &Path, bytes: &[u8], overwrite: bool) -> Result<bool> {
    create_parent(path)?;
    if overwrite {
        write_replacing(path, bytes)
    } else {
        write_new(path, bytes).map(|()| true)
    }
}

/// Replace `path` with `bytes` through a temporary file and a rename, so nothing ever sees half a
/// file. Returns false without writing when the file already holds exactly these bytes: output is
/// deterministic, so re-running a pipeline leaves untouched files alone (git, CDN uploads).
fn write_replacing(path: &Path, bytes: &[u8]) -> Result<bool> {
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(false);
    }
    let temp = temp_path(path);
    write_file(&temp, bytes)?;
    std::fs::rename(&temp, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        Error::other(format!("Could not replace {}: {e}", path.display()))
    })?;
    Ok(true)
}

/// Write `bytes` to `path`, which must not exist yet. The file only appears under its name once
/// it's complete: a half-written raw image would look already generated to `batch`. So the bytes go
/// to a temporary file first, which is then hard-linked into place; unlike a rename, a link never
/// replaces an existing file.
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = temp_path(path);
    write_file(&temp, bytes)?;
    let linked = std::fs::hard_link(&temp, path);
    let _ = std::fs::remove_file(&temp);
    match linked {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(Error::other(format!("Could not create {}: {e}", path.display()))),
        // A filesystem without hard links (FAT, some network shares): create the file directly.
        // write_file still removes it if the write fails.
        Err(_) => write_file(path, bytes),
    }
}

/// A hidden temporary name next to `path`, on the same filesystem.
fn temp_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{name}.{}.tmp", &util::random_id()[..8]))
}

/// Create `path` (never replacing a file) and write `bytes` to it, removing it again if the write
/// fails, for example on a full disk.
fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| Error::other(format!("Could not create {}: {e}", path.display())))?;
    if let Err(e) = file.write_all(bytes) {
        // Closed first: Windows can't remove an open file.
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(Error::other(format!("Could not write {}: {e}", path.display())));
    }
    Ok(())
}

/// Write the image as `wanted`, applying `transform` and converting (and quantizing, with
/// `colors`) when needed; PNG output is also recompressed losslessly. If the bytes can't be
/// processed, keep them under their real extension: the quota is already spent. For the same
/// reason, when the transform edits visible pixels (trim, resize, hard alpha), the untouched
/// original is kept next to the output as `<name>.raw.<ext>`; there is no seed to regenerate it.
pub struct Saved {
    pub path: PathBuf,
    pub warning: Option<String>,
    /// Lossy quality codex-img applied (JPEG or lossy WebP), not the backend's quality hint.
    pub output_quality: Option<u8>,
    /// Pixel size of the saved file, when the transform edited visible pixels.
    pub size: Option<(u32, u32)>,
    pub trim: Option<transform::Rect>,
    /// The untouched generated image, when the transform edited visible pixels.
    pub raw_path: Option<PathBuf>,
}

pub fn save_image(bytes: &[u8], actual: Format, wanted: Format, enc: &images::Encoding, transform: &transform::Transform, path: &Path) -> Result<Saved> {
    create_parent(path)?;
    let error = match process(bytes, actual, wanted, enc, transform, true) {
        Ok(processed) => {
            write_new(path, &processed.bytes)?;
            let mut saved = Saved {
                path: path.to_path_buf(),
                warning: None,
                output_quality: processed.output_quality,
                size: None,
                trim: processed.trim,
                raw_path: None,
            };
            if transform.edits() {
                saved.size = processed.sizes.map(|(_, size)| size);
                let raw = raw_path(path, actual);
                match write_new(&raw, bytes) {
                    Ok(()) => saved.raw_path = Some(raw),
                    Err(e) => saved.warning = Some(format!("could not keep the original image: {}", e.message)),
                }
            }
            return Ok(saved);
        }
        Err(e) => e.message,
    };
    let fallback = path.with_extension(actual.extension());
    write_new(&fallback, bytes)?;
    let warning = format!("{error}; saved the original {} as {}", actual.name(), fallback.display());
    Ok(Saved { path: fallback, warning: Some(warning), output_quality: None, size: None, trim: None, raw_path: None })
}

/// `out/car.png` -> `out/car.raw.png` (in the format the backend returned).
fn raw_path(path: &Path, actual: Format) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "image".into());
    path.with_file_name(format!("{stem}.raw.{}", actual.extension()))
}

pub fn describe(saved: &Saved, image: &Generated) -> Value {
    let (path, output_quality) = (saved.path.as_path(), saved.output_quality);
    let format = path.extension().and_then(|e| e.to_str()).and_then(Format::parse).unwrap_or(image.format);
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(image.bytes.len() as u64);
    let mut out = Map::new();
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            out.insert(key.into(), value);
        }
    };
    let reported = &image.reported;
    put("path", Some(json!(path.display().to_string())));
    put("format", Some(json!(format.name())));
    put("bytes", Some(json!(bytes)));
    put("transport", Some(json!(image.transport.name())));
    put("imageModel", reported.model.as_ref().map(|v| json!(v)));
    put("routingModel", image.routing_model.as_ref().map(|v| json!(v)));
    // After trim, resize or hard alpha, `size` is the saved file's; the backend's goes to `rawSize`.
    match saved.size {
        Some((w, h)) => {
            put("size", Some(json!(format!("{w}x{h}"))));
            put("rawSize", reported.size.as_ref().map(|v| json!(v)));
        }
        None => put("size", reported.size.as_ref().map(|v| json!(v))),
    }
    put("rawPath", saved.raw_path.as_ref().map(|p| json!(p.display().to_string())));
    put("trim", saved.trim.map(|r| json!({"x": r.x, "y": r.y, "width": r.width, "height": r.height})));
    put("quality", reported.quality.as_ref().map(|v| json!(v)));
    put("outputQuality", output_quality.map(|q| json!(q)));
    put("background", reported.background.as_ref().map(|v| json!(v)));
    put("revisedPrompt", image.revised_prompt.as_ref().map(|v| json!(v)));
    put("generationId", Some(json!(image.id)));
    put("responseId", image.response_id.as_ref().map(|v| json!(v)));
    put("usage", image.usage.clone());
    put("durationMs", Some(json!(image.duration.as_millis() as u64)));
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn run(list: &[&str]) -> Options {
        match parse(&args(list)).unwrap() {
            Command::Run(options) => options,
            other => panic!("expected run, got {other:?}"),
        }
    }

    fn usage_error(list: &[&str]) -> String {
        parse(&args(list)).unwrap_err().message
    }

    #[test]
    fn parses_options_and_infers_format() {
        let o = run(&["-o", "out.jpg", "-i", "a.png", "--image=b.png", "-n", "2", "--size", "1024x1536", "a", "cat"]);
        assert_eq!(o.prompt, "a cat");
        assert_eq!(o.output.as_deref(), Some("out.jpg"));
        assert_eq!(o.format, Some(Format::Jpeg));
        assert_eq!(o.images, vec!["a.png", "b.png"]);
        assert_eq!((o.count, o.size.as_deref()), (2, Some("1024x1536")));
        assert_eq!(run(&["-o", "out.bin", "x"]).format, None);
        assert_eq!(run(&["-f", "jpg", "x"]).format, Some(Format::Jpeg));
        assert_eq!(run(&["-", "--json"]).prompt, "-");
        assert_eq!(run(&["--", "-starts-with-dash"]).prompt, "-starts-with-dash");
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(usage_error(&["-f", "gif", "x"]).contains("--format"));
        assert!(usage_error(&["-n", "0", "x"]).contains("--count"));
        assert!(usage_error(&["-s", "big", "x"]).contains("--size"));
        assert!(usage_error(&["-q", "max", "x"]).contains("--quality"));
        assert!(usage_error(&["--bogus", "x"]).contains("Unknown option"));
        assert!(usage_error(&["-o"]).contains("needs a value"));
        assert!(usage_error(&[]).contains("Missing prompt"));
        assert!(usage_error(&["-m", "gpt-5.5", "x"]).contains("--via-responses"));
        assert!(usage_error(&["-c", "1", "x"]).contains("--colors"));
        assert!(usage_error(&["-c", "300", "x"]).contains("--colors"));
        assert!(usage_error(&["-c", "64", "-o", "a.jpg", "x"]).contains("PNG"));
        assert_eq!(run(&["--colors=64", "-o", "a.png", "x"]).encoding.colors, Some(64));
        assert!(run(&["-c", "64", "--dither", "x"]).encoding.dither);
        let webp = run(&["-o", "a.webp", "--output-quality", "70", "x"]).encoding;
        assert_eq!((webp.quality, webp.lossless), (Some(70), false));
        assert!(run(&["-f", "webp", "--lossless", "x"]).encoding.lossless);
        assert!(usage_error(&["--output-quality", "0", "x"]).contains("1 to 100"));
        assert!(usage_error(&["-o", "a.png", "--output-quality", "70", "x"]).contains("PNG"));
        assert!(usage_error(&["-o", "a.jpg", "--lossless", "x"]).contains("WebP"));
        assert!(usage_error(&["--dither", "x"]).contains("--colors"));
        assert_eq!(run(&["-o", "a.webp", "x"]).format, Some(Format::Webp));
        let t = run(&["--trim=4", "--resize", "400x", "--no-bleed", "x"]).transform;
        assert_eq!((t.trim, t.resize.and_then(|r| r.width), t.no_bleed), (Some(4), Some(400), true));
        let o = run(&["--trim", "a", "car"]);
        assert_eq!((o.transform.trim, o.prompt.as_str()), (Some(0), "a car"), "a bare --trim takes no value");
        assert_eq!(run(&["--resize=1536x1024", "--fit", "cover", "x"]).transform.fit, Some(transform::Fit::Cover));
        assert!(usage_error(&["--fit", "cover", "x"]).contains("--resize"));
        assert!(usage_error(&["--trim=-1", "x"]).contains("--trim"));
        assert_eq!(run(&["--hard-alpha", "x"]).transform.hard_alpha, Some(16));
        assert_eq!(run(&["--hard-alpha=100", "x"]).transform.hard_alpha, Some(100));
        assert!(run(&["--resize", "400x", "--no-enlarge", "x"]).transform.no_enlarge);
        let t = run(&["--key", "auto", "--key=#102030:8", "--key-region", "bottom:30%", "--trim", "--trim-density", "0.15", "--key-cut", "x"]).transform;
        assert_eq!(t.keys, vec![transform::Key::Auto { tolerance: 32 }, transform::Key::Rgb { rgb: [16, 32, 48], tolerance: 8 }]);
        assert_eq!((t.key_region.map(|r| r.bands[1]), t.trim_density.map(|d| d.percent)), (Some(30), Some(15)));
        assert!(usage_error(&["--trim-density", "0.15", "x"]).contains("--trim"));
        assert!(usage_error(&["--key-region", "bottom:30%", "x"]).contains("--key"));
        assert!(usage_error(&["--key", "sea", "x"]).contains("--key"));
        assert_eq!(t.key_cut, Some(40));
        assert_eq!(run(&["--key", "blue", "--key-region", "top:40%", "--key-cut=0.6", "x"]).transform.key_cut, Some(60));
        assert!(usage_error(&["--key-cut", "x"]).contains("--key"));
        assert!(usage_error(&["--key", "auto", "--key-cut", "x"]).contains("--key-region"));
        assert!(usage_error(&["--no-enlarge", "x"]).contains("--resize"));
        let o = run(&["--via-responses", "-m", "gpt-6-sol", "x"]);
        assert!(o.via_responses);
        assert_eq!(o.model.as_deref(), Some("gpt-6-sol"));
    }

    #[test]
    fn parses_subcommands_and_meta_flags() {
        assert_eq!(parse(&args(&["status"])).unwrap(), Command::Status { json: false });
        assert_eq!(parse(&args(&["status", "--json"])).unwrap(), Command::Status { json: true });
        assert_eq!(run(&["status", "report", "icon"]).prompt, "status report icon");
        assert_eq!(run(&["convert this photo to night"]).prompt, "convert this photo to night");
        assert!(matches!(parse(&args(&["convert", "a.png", "-o", "a.webp"])).unwrap(), Command::Convert(_)));
        assert_eq!(parse(&args(&["convert", "-h"])).unwrap(), Command::ConvertHelp);
        assert!(matches!(parse(&args(&["sheet", "a.png", "-o", "s.png"])).unwrap(), Command::Sheet(_)));
        assert_eq!(run(&["sheet music on a piano"]).prompt, "sheet music on a piano");
        assert!(matches!(parse(&args(&["batch", "art/assets.json"])).unwrap(), Command::Batch(_)));
        assert_eq!(parse(&args(&["batch", "-h"])).unwrap(), Command::BatchHelp);
        assert!(matches!(parse(&args(&["tile", "sky.png", "-o", "t.png"])).unwrap(), Command::Tile(_)));
        assert_eq!(run(&["tile floor texture"]).prompt, "tile floor texture", "a quoted prompt starting with tile");
        assert_eq!(parse(&args(&["-h"])).unwrap(), Command::Help);
        assert_eq!(parse(&args(&["--version"])).unwrap(), Command::Version);
    }

    #[test]
    fn output_path_handles_files_suffixes_and_directories() {
        let now = 1_767_323_045; // 2026-01-02T03:04:05Z
        // An absolute folder on every OS: on Windows, "/x" has no drive and isn't absolute.
        let x = std::env::temp_dir().join("x");
        let at = |rest: &str| format!("{}/{rest}", x.display());
        let p = |o: &str, f, id, i, n| output_path(Some(o), f, id, i, n, now);
        assert_eq!(p(&at("out.png"), Format::Png, "ig_1", 0, 1), x.join("out.png"));
        assert_eq!(p(&at("out"), Format::Png, "ig_1", 0, 1), x.join("out.png"));
        assert_eq!(p(&at("out.png"), Format::Png, "ig_1", 1, 3), x.join("out-2.png"));
        assert_eq!(p(&at("dir/"), Format::Jpeg, "ig_abc", 0, 1), x.join("dir").join("codex-img-20260102T030405-ig_abc.jpg"));
        assert_eq!(sanitize_id("a1b2c3d4-e5f6-7890-abcd-ef0123456789"), "ef0123456789");
    }

    #[test]
    fn save_image_trims_resizes_and_keeps_the_original() {
        let dir = crate::auth::tests::temp_dir("save-transform");
        let sprite = image::RgbaImage::from_fn(64, 48, |x, y| {
            if (16..48).contains(&x) && (8..24).contains(&y) { image::Rgba([200, 60, 40, 255]) } else { image::Rgba([5, 5, 5, 0]) }
        });
        let mut png = std::io::Cursor::new(Vec::new());
        sprite.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let png = png.into_inner();
        let t = transform::Transform { trim: Some(0), resize: Some(transform::Resize::parse("16x").unwrap()), ..Default::default() };
        let saved = save_image(&png, Format::Png, Format::Webp, &images::Encoding::default(), &t, &dir.join("car.webp")).unwrap();
        assert_eq!((saved.size, saved.warning.as_deref()), (Some((16, 8)), None));
        assert_eq!(saved.trim, Some(transform::Rect { x: 16, y: 8, width: 32, height: 16 }));
        assert_eq!(saved.raw_path.as_deref(), Some(dir.join("car.raw.png").as_path()));
        assert_eq!(std::fs::read(dir.join("car.raw.png")).unwrap(), png, "the original is kept byte for byte");

        // Bleed alone doesn't reshape: no raw copy, and the hidden colour is replaced.
        let saved = save_image(&png, Format::Png, Format::Png, &images::Encoding::default(), &transform::Transform::default(), &dir.join("bled.png")).unwrap();
        assert!(saved.raw_path.is_none() && saved.size.is_none());
        assert_eq!(image::open(&saved.path).unwrap().to_rgba8().get_pixel(0, 0).0, [200, 60, 40, 0]);

        // A transform that fails still saves the generated image.
        let empty = {
            let mut out = std::io::Cursor::new(Vec::new());
            image::RgbaImage::new(8, 8).write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner()
        };
        let trim = transform::Transform { trim: Some(0), ..Default::default() };
        let saved = save_image(&empty, Format::Png, Format::Png, &images::Encoding::default(), &trim, &dir.join("empty.png")).unwrap();
        assert!(saved.warning.unwrap().contains("no visible pixels"));
        assert_eq!(std::fs::read(saved.path).unwrap(), empty);
    }

    #[test]
    fn save_image_converts_and_never_discards_output() {
        use base64::Engine;
        let dir = crate::auth::tests::temp_dir("save");
        let png = base64::engine::general_purpose::STANDARD.decode(crate::images::tests::PNG_B64).unwrap();

        for (name, format) in [("a.jpg", Format::Jpeg), ("a.webp", Format::Webp)] {
            let saved = save_image(&png, Format::Png, format, &images::Encoding::default(), &transform::Transform::default(), &dir.join(name)).unwrap();
            let path = saved.path.clone();
            assert_eq!((saved.path, saved.warning), (dir.join(name), None));
            assert_eq!(saved.output_quality, Some(if format == Format::Jpeg { 90 } else { 80 }));
            assert_eq!(images::sniff(&std::fs::read(&path).unwrap()), Some(format));
        }
        let saved = save_image(&png, Format::Png, Format::Png, &images::Encoding { colors: Some(8), dither: true, ..Default::default() }, &transform::Transform::default(), &dir.join("q.png")).unwrap();
        assert_eq!((saved.path, saved.warning, saved.output_quality), (dir.join("q.png"), None, None));

        // Bytes that can't be decoded are still kept, under their real extension.
        let fake = b"\x89PNG\r\n\x1a\nbroken";
        let saved = save_image(fake, Format::Png, Format::Png, &images::Encoding::default(), &transform::Transform::default(), &dir.join("c.png")).unwrap();
        assert_eq!((std::fs::read(saved.path).unwrap(), saved.warning), (fake.to_vec(), None), "unoptimizable PNG is kept as is");
        let saved = save_image(fake, Format::Png, Format::Webp, &images::Encoding::default(), &transform::Transform::default(), &dir.join("b.webp")).unwrap();
        assert_eq!((saved.path, saved.output_quality), (dir.join("b.png"), None));
        assert!(saved.warning.unwrap().contains("saved the original png"));

        assert!(save_image(&png, Format::Png, Format::Png, &images::Encoding::default(), &transform::Transform::default(), &dir.join("b.png")).is_err(), "must not overwrite");
    }

    #[test]
    fn new_files_appear_whole_and_never_replace_one() {
        let dir = crate::auth::tests::temp_dir("write-new");
        write_new(&dir.join("a.png"), b"first").unwrap();
        let err = write_new(&dir.join("a.png"), b"second").unwrap_err();
        assert!(err.message.contains("Could not create"), "{}", err.message);
        assert_eq!(std::fs::read(dir.join("a.png")).unwrap(), b"first");
        assert!(write_output(&dir.join("a.png"), b"third", true).unwrap());
        assert_eq!(std::fs::read(dir.join("a.png")).unwrap(), b"third");
        // No temporary files are left behind, whichever way it went.
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["a.png"]);
    }

    #[test]
    fn check_output_refuses_taken_names_before_any_request() {
        let dir = crate::auth::tests::temp_dir("check-output");
        let out = dir.join("new/car.png");
        let at = |p: &Path| p.display().to_string();
        let none = transform::Transform::default();
        let trim = transform::Transform { trim: Some(0), ..Default::default() };
        check_output(Some(&at(&out)), Format::Png, 1, &none).unwrap();
        assert!(dir.join("new").is_dir(), "the folder is created up front");

        std::fs::write(dir.join("new/car.raw.png"), b"x").unwrap();
        check_output(Some(&at(&out)), Format::Png, 1, &none).unwrap();
        let err = check_output(Some(&at(&out)), Format::Png, 1, &trim).unwrap_err();
        assert!(err.message.contains("car.raw.png already exists"), "{}", err.message);

        std::fs::write(dir.join("new/car-2.png"), b"x").unwrap();
        check_output(Some(&at(&out)), Format::Png, 1, &none).unwrap();
        assert!(check_output(Some(&at(&out)), Format::Png, 2, &none).unwrap_err().message.contains("car-2.png"));

        // A folder gets new, unique names.
        check_output(Some(&at(&dir.join("new"))), Format::Png, 3, &trim).unwrap();
        check_output(None, Format::Png, 1, &trim).unwrap();
        // A file where the folder should be.
        std::fs::write(dir.join("blocked"), b"x").unwrap();
        assert!(check_output(Some(&at(&dir.join("blocked/car.png"))), Format::Png, 1, &none).is_err());
    }
}
