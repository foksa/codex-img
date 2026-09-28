//! `codex-img convert`: the local conversion pipeline (re-encode, quantize, lossless PNG
//! recompression) applied to existing files. No login, no network, no quota.
use crate::cli;
use crate::error::{Error, Result};
use crate::images::{self, Format};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Instant;

const MAX_CONVERT_INPUT_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertOptions {
    pub inputs: Vec<String>,
    pub output: Option<String>,
    pub format: Option<Format>,
    pub encoding: images::Encoding,
    pub json: bool,
    pub quiet: bool,
}

pub fn help() -> &'static str {
    r#"Usage:
  codex-img convert <input>... [options]

Converts existing PNG, JPEG or WebP files locally (no login, no quota).

Options:
  -o, --output <path>       Output file (one input) or directory. Default: next to
                            each input, as <name>.<ext>, or <name>.min.<ext> when
                            that would be the input itself
  -f, --format <fmt>        png | jpeg | webp (default: from -o extension, else the
                            input's format)
  -c, --colors <n>          Quantize PNG output to a palette of n colours (2-256)
      --dither              Dither when quantizing
      --output-quality <n>  1-100 for jpeg (default 90) and lossy webp (default 80)
      --lossless            Lossless webp instead of lossy
      --json                Print one JSON object per file to stdout
      --quiet               No progress on stderr

PNG output is always recompressed losslessly. Existing files are never overwritten.

Examples:
  codex-img convert hero.png -o hero.webp     # lossy, quality 80
  codex-img convert icon.png -c 64            # -> icon.min.png
  codex-img convert shots/*.png -f jpeg -o out/"#
}

pub fn parse(args: &[String]) -> Result<Option<ConvertOptions>> {
    let mut opts = ConvertOptions { inputs: Vec::new(), output: None, format: None, encoding: images::Encoding::default(), json: false, quiet: false };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            opts.inputs.extend(iter.by_ref().cloned());
            break;
        }
        if !arg.starts_with('-') {
            opts.inputs.push(arg.clone());
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if arg.starts_with("--") => (n, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| iter.next().cloned()).ok_or_else(|| Error::usage(format!("{name} needs a value.")));
        match name {
            "-o" | "--output" => opts.output = Some(value()?),
            "-f" | "--format" => {
                opts.format = Some(Format::parse(&value()?).ok_or_else(|| Error::usage("--format must be one of: png, jpeg, webp"))?)
            }
            "-c" | "--colors" => opts.encoding.colors = Some(cli::parse_colors(&value()?)?),
            "--dither" => opts.encoding.dither = true,
            "--output-quality" => opts.encoding.quality = Some(cli::parse_output_quality(&value()?)?),
            "--lossless" => opts.encoding.lossless = true,
            "--json" => opts.json = true,
            "--quiet" => opts.quiet = true,
            "-h" | "--help" => return Ok(None),
            _ => return Err(Error::usage(format!("Unknown convert option: {arg}"))),
        }
    }
    if opts.inputs.is_empty() {
        return Err(Error::usage("convert needs at least one input file."));
    }
    let output_is_file = opts.output.as_deref().is_some_and(|o| !o.ends_with('/') && !Path::new(o).is_dir());
    if output_is_file && opts.inputs.len() > 1 {
        return Err(Error::usage("With several inputs, -o must be a directory (end it with /)."));
    }
    if opts.format.is_none() && output_is_file {
        let ext = Path::new(opts.output.as_deref().unwrap_or_default()).extension().and_then(|e| e.to_str()).unwrap_or_default();
        opts.format = Some(Format::parse(ext).ok_or_else(|| Error::usage("Can't tell the output format from -o; add -f png|jpeg|webp."))?);
    }
    // Without -f/-o the format comes from each input; convert_one checks again then.
    opts.encoding.check(opts.format)?;
    Ok(Some(opts))
}

/// Where `input` goes: the explicit file, a directory, or next to the input. Never the input itself.
pub fn target_path(input: &Path, output: Option<&str>, format: Format) -> PathBuf {
    let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "image".into());
    let named = |dir: &Path| {
        let path = dir.join(format!("{stem}.{}", format.extension()));
        if same_file(&path, input) {
            dir.join(format!("{stem}.min.{}", format.extension()))
        } else {
            path
        }
    };
    match output {
        Some(o) if o.ends_with('/') || Path::new(o).is_dir() => named(Path::new(o)),
        Some(o) => PathBuf::from(o),
        None => named(input.parent().unwrap_or(Path::new(""))),
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn read_image(path: &Path) -> Result<(Vec<u8>, Format)> {
    let fail = |e: String| Error::other(format!("Unable to read {}: {e}", path.display()));
    let meta = std::fs::metadata(path).map_err(|e| fail(e.to_string()))?;
    if !meta.is_file() {
        return Err(fail("not a regular file".into()));
    }
    if meta.len() > MAX_CONVERT_INPUT_BYTES {
        return Err(fail("larger than 100 MiB".into()));
    }
    let bytes = std::fs::read(path).map_err(|e| fail(e.to_string()))?;
    let format = images::sniff(&bytes).ok_or_else(|| fail("not a PNG, JPEG or WebP image".into()))?;
    Ok((bytes, format))
}

fn convert_one(input: &Path, opts: &ConvertOptions) -> Result<serde_json::Value> {
    let started = Instant::now();
    let (bytes, actual) = read_image(input)?;
    let format = opts.format.unwrap_or(actual);
    // parse() can only check -f/-o; without them the output takes the input's format.
    opts.encoding.check(Some(format)).map_err(|e| Error::usage(format!("{}: {}", input.display(), e.message)))?;
    let target = target_path(input, opts.output.as_deref(), format);
    if same_file(&target, input) {
        return Err(Error::usage(format!("Output would overwrite the input {}; choose another -o.", input.display())));
    }
    let (path, dimensions) = cli::save_converted(&bytes, format, &opts.encoding, &target)?;
    let written = std::fs::metadata(&path).map(|m| m.len()).unwrap_or_default();
    let mut info = json!({
        "path": path.display().to_string(),
        "input": input.display().to_string(),
        "format": format.name(),
        "bytes": written,
        "inputBytes": bytes.len(),
        "size": format!("{}x{}", dimensions.0, dimensions.1),
        "durationMs": started.elapsed().as_millis() as u64,
    });
    if let Some(colors) = opts.encoding.colors {
        info["colors"] = json!(colors);
    }
    if let Some(quality) = opts.encoding.quality {
        info["quality"] = json!(quality);
    }
    Ok(info)
}

/// Converts each input in turn; a failure is reported and the rest still run. Returns the exit code.
pub fn run(opts: &ConvertOptions) -> i32 {
    let mut exit_code = 0;
    for input in &opts.inputs {
        match convert_one(Path::new(input), opts) {
            Ok(info) => {
                if opts.json {
                    println!("{info}");
                } else {
                    println!("{}", info["path"].as_str().unwrap_or_default());
                }
                if !opts.quiet {
                    let (from, to) = (info["inputBytes"].as_u64().unwrap_or(0), info["bytes"].as_u64().unwrap_or(0));
                    eprintln!("{input} -> {} ({} KB -> {} KB)", info["path"].as_str().unwrap_or_default(), from / 1024, to / 1024);
                }
            }
            Err(error) => {
                eprintln!("codex-img: {error}");
                exit_code = exit_code.max(error.kind.exit_code());
            }
        }
    }
    exit_code
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn usage_error(list: &[&str]) -> String {
        parse(&args(list)).unwrap_err().message
    }

    #[test]
    fn parses_and_validates_options() {
        let o = parse(&args(&["a.png", "-o", "a.webp"])).unwrap().unwrap();
        assert_eq!((o.inputs, o.format), (vec!["a.png".to_string()], Some(Format::Webp)));
        assert_eq!(parse(&args(&["a.png", "--colors=32"])).unwrap().unwrap().encoding.colors, Some(32));
        assert_eq!(parse(&args(&["--help"])).unwrap(), None);
        assert!(usage_error(&[]).contains("at least one input"));
        assert!(usage_error(&["a.png", "b.png", "-o", "c.png"]).contains("directory"));
        assert!(usage_error(&["a.png", "-o", "c.gif"]).contains("-f"));
        assert!(usage_error(&["a.png", "-c", "8", "-f", "jpeg"]).contains("PNG"));
        assert!(usage_error(&["a.png", "--dither"]).contains("--colors"));
        assert!(usage_error(&["a.png", "-o", "b.png", "--output-quality", "50"]).contains("PNG"));
        assert!(usage_error(&["a.png", "-f", "jpeg", "--lossless"]).contains("WebP"));
        assert!(usage_error(&["a.png", "-n", "2"]).contains("Unknown convert option"));
    }

    fn png_file(dir: &Path, name: &str) -> PathBuf {
        let image = image::RgbImage::from_fn(32, 32, |x, y| image::Rgb([(x * 8) as u8, (y * 8) as u8, 90]));
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, out.into_inner()).unwrap();
        path
    }

    #[test]
    fn colors_is_rejected_for_non_png_inputs_without_explicit_format() {
        let dir = crate::auth::tests::temp_dir("convert-colors");
        let png = png_file(&dir, "a.png");
        let jpeg = dir.join("a.jpg");
        let opts = parse(&args(&[&png.display().to_string(), "-f", "jpeg", "--quiet"])).unwrap().unwrap();
        assert_eq!(run(&opts), 0);
        // No -f and no -o: the output format is the input's (JPEG), so --colors can't apply.
        let opts = parse(&args(&[&jpeg.display().to_string(), "-c", "4", "--quiet"])).unwrap().unwrap();
        assert_eq!(run(&opts), 64);
        assert!(!dir.join("a.min.jpg").exists(), "nothing may be written");
    }

    #[test]
    fn damaged_png_is_rejected_not_copied() {
        let dir = crate::auth::tests::temp_dir("convert-damaged");
        let path = png_file(&dir, "bad.png");
        let mut bytes = std::fs::read(&path).unwrap();
        let idat = bytes.windows(4).position(|w| w == b"IDAT").unwrap();
        bytes[idat + 10] ^= 0xff; // corrupt compressed pixel data, header stays valid
        std::fs::write(&path, &bytes).unwrap();
        for extra in [&[][..], &["-f", "webp"][..]] {
            let mut list = vec![path.to_str().unwrap(), "--quiet"];
            list.extend_from_slice(extra);
            assert_eq!(run(&parse(&args(&list)).unwrap().unwrap()), 1, "{extra:?}");
        }
        assert!(!dir.join("bad.min.png").exists() && !dir.join("bad.webp").exists());
    }

    #[test]
    fn converts_files_without_touching_inputs() {
        let dir = crate::auth::tests::temp_dir("convert");
        let input = dir.join("in.png");
        let png = base64::engine::general_purpose::STANDARD.decode(crate::images::tests::PNG_B64).unwrap();
        std::fs::write(&input, &png).unwrap();
        let input_str = input.display().to_string();

        // Same format, no -o: never the input itself.
        assert_eq!(target_path(&input, None, Format::Png), dir.join("in.min.png"));
        assert_eq!(target_path(&input, None, Format::Webp), dir.join("in.webp"));

        let opts = parse(&args(&[&input_str, "-f", "webp", "--quiet"])).unwrap().unwrap();
        assert_eq!(run(&opts), 0);
        assert_eq!(images::sniff(&std::fs::read(dir.join("in.webp")).unwrap()), Some(Format::Webp));

        let opts = parse(&args(&[&input_str, "-c", "4", "--quiet"])).unwrap().unwrap();
        assert_eq!(run(&opts), 0);
        assert!(dir.join("in.min.png").is_file());
        assert_eq!(run(&opts), 1, "second run must not overwrite in.min.png");

        let opts = parse(&args(&[&input_str, "-o", &input_str, "--quiet"])).unwrap().unwrap();
        assert_eq!(run(&opts), 64, "refuses to overwrite the input");
        assert_eq!(std::fs::read(&input).unwrap(), png);

        let text = dir.join("notes.txt");
        std::fs::write(&text, "hi").unwrap();
        assert_eq!(run(&parse(&args(&[&text.display().to_string(), "--quiet"])).unwrap().unwrap()), 1);
    }
}
