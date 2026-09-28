use crate::backend::{Generated, DEFAULT_ROUTING_MODEL};
use crate::error::{Error, Result};
use crate::images::{self, Format, MAX_EDIT_IMAGES};
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
  codex-img convert <input>... [-o path] [-f fmt] [-c n]
                              Convert existing images locally (no quota);
                              see `codex-img convert --help`

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
  -s, --size <WxH>          Shape hint, e.g. 1536x1024, 1024x1536, auto
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

Example:
  codex-img "flat vector red fox in snow" -o fox.png --json"#
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
    Run(Options),
}

fn one_of(name: &str, value: Option<String>, allowed: &[&str]) -> Result<Option<String>> {
    match value {
        Some(v) if !allowed.contains(&v.as_str()) => Err(Error::usage(format!("--{name} must be one of: {}", allowed.join(", ")))),
        other => Ok(other),
    }
}

fn is_size(value: &str) -> bool {
    let valid = |p: &str| (2..=5).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()) && !p.starts_with('0');
    value == "auto" || value.split_once('x').is_some_and(|(w, h)| valid(w) && valid(h))
}

pub fn parse(args: &[String]) -> Result<Command> {
    if args.first().is_some_and(|a| a == "status") && args[1..].iter().all(|a| a == "--json") {
        return Ok(Command::Status { json: args.len() > 1 });
    }
    // Always a subcommand: a prompt starting with the word must be quoted ("convert ...").
    if args.first().is_some_and(|a| a == "convert") {
        return Ok(crate::convert::parse(&args[1..])?.map_or(Command::ConvertHelp, Command::Convert));
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
            "-h" | "--help" => "help",
            "-v" | "--version" => "version",
            _ => return Err(Error::usage(format!("Unknown option: {arg}"))),
        };
        if matches!(key, "json" | "quiet" | "via-responses" | "dither" | "lossless" | "help" | "version") {
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

/// `convert` subcommand: write `bytes` as `wanted` to a new file. Returns the path, pixel size and
/// applied lossy quality.
/// Unlike generated images, a local input that doesn't fully decode is an error, not something to
/// copy through: the fast paths (same format, best-effort optimization) would otherwise pass it on.
pub fn save_converted(bytes: &[u8], wanted: Format, enc: &images::Encoding, path: &Path) -> Result<(PathBuf, (u32, u32), Option<u8>)> {
    let actual = images::sniff(bytes).ok_or_else(|| Error::other("Input is not a PNG, JPEG or WebP image."))?;
    let dimensions = images::validate(bytes)?;
    let (out, quality) = encode_output(bytes, actual, wanted, enc)?;
    create_parent(path)?;
    write_new(path, &out)?;
    Ok((path.to_path_buf(), dimensions, quality))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| Error::other(format!("Could not create {}: {e}", path.display())))?;
    file.write_all(bytes).map_err(|e| Error::other(format!("Could not write {}: {e}", path.display())))
}

/// Write the image as `wanted`, converting (and quantizing, with `colors`) when needed; PNG output is
/// also recompressed losslessly. If the bytes can't be converted, keep them under their real
/// extension: the quota is already spent.
pub struct Saved {
    pub path: PathBuf,
    pub warning: Option<String>,
    /// Lossy quality codex-img applied (JPEG or lossy WebP), not the backend's quality hint.
    pub output_quality: Option<u8>,
}

pub fn save_image(bytes: &[u8], actual: Format, wanted: Format, enc: &images::Encoding, path: &Path) -> Result<Saved> {
    create_parent(path)?;
    let error = match encode_output(bytes, actual, wanted, enc) {
        Ok((out, output_quality)) => {
            write_new(path, &out)?;
            return Ok(Saved { path: path.to_path_buf(), warning: None, output_quality });
        }
        Err(e) => e.message,
    };
    let fallback = path.with_extension(actual.extension());
    write_new(&fallback, bytes)?;
    let warning = format!("{error}; saved the original {} as {}", actual.name(), fallback.display());
    Ok(Saved { path: fallback, warning: Some(warning), output_quality: None })
}

pub fn describe(path: &Path, image: &Generated, output_quality: Option<u8>) -> Value {
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
    put("size", reported.size.as_ref().map(|v| json!(v)));
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
        assert_eq!(parse(&args(&["-h"])).unwrap(), Command::Help);
        assert_eq!(parse(&args(&["--version"])).unwrap(), Command::Version);
    }

    #[test]
    fn output_path_handles_files_suffixes_and_directories() {
        let now = 1_767_323_045; // 2026-01-02T03:04:05Z
        let p = |o: &str, f, id, i, n| output_path(Some(o), f, id, i, n, now);
        assert_eq!(p("/x/out.png", Format::Png, "ig_1", 0, 1), PathBuf::from("/x/out.png"));
        assert_eq!(p("/x/out", Format::Png, "ig_1", 0, 1), PathBuf::from("/x/out.png"));
        assert_eq!(p("/x/out.png", Format::Png, "ig_1", 1, 3), PathBuf::from("/x/out-2.png"));
        assert_eq!(p("/x/dir/", Format::Jpeg, "ig_abc", 0, 1), PathBuf::from("/x/dir/codex-img-20260102T030405-ig_abc.jpg"));
        assert_eq!(sanitize_id("a1b2c3d4-e5f6-7890-abcd-ef0123456789"), "ef0123456789");
    }

    #[test]
    fn save_image_converts_and_never_discards_output() {
        use base64::Engine;
        let dir = crate::auth::tests::temp_dir("save");
        let png = base64::engine::general_purpose::STANDARD.decode(crate::images::tests::PNG_B64).unwrap();

        for (name, format) in [("a.jpg", Format::Jpeg), ("a.webp", Format::Webp)] {
            let saved = save_image(&png, Format::Png, format, &images::Encoding::default(), &dir.join(name)).unwrap();
            let path = saved.path.clone();
            assert_eq!((saved.path, saved.warning), (dir.join(name), None));
            assert_eq!(saved.output_quality, Some(if format == Format::Jpeg { 90 } else { 80 }));
            assert_eq!(images::sniff(&std::fs::read(&path).unwrap()), Some(format));
        }
        let saved = save_image(&png, Format::Png, Format::Png, &images::Encoding { colors: Some(8), dither: true, ..Default::default() }, &dir.join("q.png")).unwrap();
        assert_eq!((saved.path, saved.warning, saved.output_quality), (dir.join("q.png"), None, None));

        // Bytes that can't be decoded are still kept, under their real extension.
        let fake = b"\x89PNG\r\n\x1a\nbroken";
        let saved = save_image(fake, Format::Png, Format::Png, &images::Encoding::default(), &dir.join("c.png")).unwrap();
        assert_eq!((std::fs::read(saved.path).unwrap(), saved.warning), (fake.to_vec(), None), "unoptimizable PNG is kept as is");
        let saved = save_image(fake, Format::Png, Format::Webp, &images::Encoding::default(), &dir.join("b.webp")).unwrap();
        assert_eq!((saved.path, saved.output_quality), (dir.join("b.png"), None));
        assert!(saved.warning.unwrap().contains("saved the original png"));

        assert!(save_image(&png, Format::Png, Format::Png, &images::Encoding::default(), &dir.join("b.png")).is_err(), "must not overwrite");
    }
}
