//! `codex-img convert`: the local conversion pipeline (re-encode, quantize, lossless PNG
//! recompression) applied to existing files. No login, no network, no quota.
use crate::cli;
use crate::error::{Error, Result};
use crate::images::{self, Format};
use crate::transform::{self, Fit, Resize, Transform};
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
    pub transform: Transform,
    pub json: bool,
    pub quiet: bool,
    /// Replace existing output files (never the input).
    pub force: bool,
    pub mask_out: Option<String>,
    /// Directory inputs are walked, and their images mirror the tree under -o.
    pub recursive: bool,
}

pub fn help() -> &'static str {
    r#"Usage:
  codex-img convert <input>... [options]

Converts existing PNG, JPEG or WebP files locally (no login, no quota).
With --recursive, inputs can be directories.

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
      --trim[=pad]          Crop transparent borders to the visible pixels (alpha
                            above 16; fainter specks don't count), keeping pad
                            transparent pixels around them (default 0). An image
                            with no transparent border is left as it is
      --hard-alpha[=n]      Make every pixel fully solid (alpha above n, default
                            16) or fully transparent, before --trim and again after
                            --resize. For pixel art and crisp sprite edges
      --resize <size>       WxH, Wx or xH (one side keeps the aspect ratio).
                            Resampled with premultiplied alpha, after --trim
      --fit <mode>          How WxH handles another aspect ratio: inside (default,
                            fits in the box), cover (fills it, crops the centre),
                            contain (fits, pads with transparency), fill (stretches)
      --nearest             Copy pixels for whole-number upscaling of pixel art
      --no-enlarge          Only shrink: --resize leaves smaller images at their size
      --key <colour>        Make an unwanted background transparent: the sea painted
                            under a boat, the sky behind a building. Pixels of this
                            colour connected to the transparent background or the
                            image's border go; it's a flood fill, so matching paint
                            enclosed by the object's outline survives, and small
                            islands the removed background leaves behind (foam,
                            spray) go too. Repeatable. Runs after --hard-alpha,
                            before --trim. The colour is one of:
                              auto[:tol]     the background's own colours, sampled
                                             along the outer 5% of the --key-region
                                             edges (all four without a region). Only
                                             where those edges really are background:
                                             otherwise it keys the object's own edge
                              <name>         red, orange (browns too), yellow, green,
                                             cyan, blue, purple, pink, white, gray,
                                             black
                              #rrggbb[:tol]  tol per channel (default 32)
      --key-region <bands>  Only key out within bands along edges of the visible
                            content, each a share of its height or width:
                            bottom:30% (ground under a sprite), top:40%,left:15%,
                            right:15% (sky around a building), or all:20%. Matching
                            colours elsewhere stay safe, like sky-blue windows
      --key-spread <step>   Let keying spread from each removed pixel into neighbours
                            whose colour differs by at most step per channel (e.g.
                            24), step by step. It then follows gradients the key
                            colours don't cover, like a sky fading from blue to gold
                            and the shading of clouds, and stops at outlines. Higher
                            steps also take low-contrast scenery, like distant hills
      --key-cut[=f]         Before --key, from each --key-region edge inward, cut
                            off whole rows (or columns) while at least f (default
                            0.4) of their visible pixels match the key: below a
                            boat's waterline, or above a roofline. Needs --key-region
      --trim-density [edges:]f
                            With --trim, also drop sparse rows at the bottom: those
                            with fewer visible pixels than f (e.g. 0.15) of the
                            fullest row. Leftover specks under a sprite then don't
                            become its bottom edge. Other edges: top:0.15,
                            bottom,left:0.15, all:0.15 (careful: a thin mast, pole or
                            trunk is sparse too)
      --palette <colours>   Snap every colour to a palette: '#2B1D14,#6B3E26,...', a
                            .gpl/.hex file, a swatch image, or a palette preset
                            (built in: pico-8, game-boy, nes, c64, sweetie-16,
                            resurrect-64 and more; `codex-img presets` lists them).
                            Alpha is hardened (at 127 unless --hard-alpha says
                            otherwise), stray pixels are cleaned at full size, and
                            the colours are snapped again after --resize, so the
                            output has exactly the palette's colours. PNG output
                            is a palette PNG; WebP needs --lossless
      --palette-clean       Stronger cleanup, for art not generated with the
                            palette: reduce to 32 colours, then match hue before
                            lightness, so shading between palette colours doesn't
                            turn into speckles and streaks of another hue
      --no-bleed            Keep the colour stored under fully transparent pixels.
                            By default PNG and lossless webp output gets the nearest
                            visible colour there, so filtering in game engines and
                            other tools can't pull a dark halo into the edges
      --force               Replace existing output files (never an input). A file
                            that already holds the same bytes is left untouched
      --mask-out <png>      Write a black/white mask of pixels removed by --key,
                            at the input size (white is removed). One input only
  -r, --recursive           Convert every PNG, JPEG and WebP under each directory
                            input (hidden files and other files are skipped),
                            keeping its relative path under -o. With --json, a
                            last line {"total":{...}} sums files and bytes
      --json                Print one JSON object per file to stdout
      --quiet               No progress on stderr

PNG output is always recompressed losslessly, and the same input and options always
give the same bytes. Without --force, existing files are never overwritten.

Examples:
  codex-img convert hero.png -o hero.webp     # lossy, quality 80
  codex-img convert icon.png -c 64            # -> icon.min.png
  codex-img convert shots/*.png -f jpeg -o out/
  codex-img convert car.png --trim=4 --resize 400x -o sprites/
  codex-img convert car.png --hard-alpha --trim --resize 400x300 --no-enlarge -c 160
  codex-img convert cockpit.png --resize 1920x1080 --fit cover
  codex-img convert raw/*.png --trim -c 160 -o public/ --force
  codex-img convert -r maps/units/ -f webp --lossless -o web/units/ --json
  codex-img convert boat.png --hard-alpha --key auto --key-region bottom:30% --key-cut --trim --trim-density 0.15
  codex-img convert house.png --key auto --key-region top:80% --key-spread 24 --trim"#
}

pub fn parse(args: &[String]) -> Result<Option<ConvertOptions>> {
    let mut opts = ConvertOptions { inputs: Vec::new(), output: None, format: None, encoding: images::Encoding::default(), transform: Transform::default(), json: false, quiet: false, force: false, mask_out: None, recursive: false };
    let (mut palette, mut palette_clean) = (None, false);
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
            "--trim" => opts.transform.trim = Some(inline.as_deref().map(transform::parse_trim_padding).transpose()?.unwrap_or(0)),
            "--resize" => opts.transform.resize = Some(Resize::parse(&value()?)?),
            "--fit" => opts.transform.fit = Some(Fit::parse(&value()?)?),
            "--no-bleed" => opts.transform.no_bleed = true,
            "--no-enlarge" => opts.transform.no_enlarge = true,
            "--nearest" if inline.is_none() => opts.transform.nearest = true,
            "--key" => opts.transform.keys.push(transform::Key::parse(&value()?)?),
            "--key-region" => opts.transform.key_region = Some(transform::Region::parse(&value()?)?),
            "--key-spread" => opts.transform.key_spread = Some(transform::parse_key_spread(&value()?)?),
            "--key-cut" => opts.transform.key_cut = Some(inline.as_deref().map(transform::parse_key_cut).transpose()?.unwrap_or(transform::KEY_CUT)),
            "--trim-density" => opts.transform.trim_density = Some(transform::Density::parse(&value()?)?),
            "--hard-alpha" => {
                opts.transform.hard_alpha =
                    Some(inline.as_deref().map(transform::parse_hard_alpha).transpose()?.unwrap_or(transform::FAINT_ALPHA))
            }
            "--palette" => palette = Some(value()?),
            "--palette-clean" => palette_clean = true,
            "--json" => opts.json = true,
            "--quiet" => opts.quiet = true,
            "--force" => opts.force = true,
            "-r" | "--recursive" => opts.recursive = true,
            "--mask-out" => opts.mask_out = Some(value()?),
            "-h" | "--help" => return Ok(None),
            _ => return Err(Error::usage(format!("Unknown convert option: {arg}"))),
        }
    }
    if opts.inputs.is_empty() {
        return Err(Error::usage("convert needs at least one input file."));
    }
    if opts.mask_out.is_some() && (opts.inputs.len() != 1 || opts.transform.keys.is_empty()) {
        return Err(Error::usage("--mask-out needs one input and --key."));
    }
    if opts.mask_out.as_deref().is_some_and(|path| !Path::new(path).extension().is_some_and(|ext| ext.eq_ignore_ascii_case("png"))) {
        return Err(Error::usage("--mask-out must name a PNG file."));
    }
    let output_is_file = opts.output.as_deref().is_some_and(|o| !o.ends_with('/') && !Path::new(o).is_dir());
    if output_is_file && opts.inputs.len() > 1 {
        return Err(Error::usage("With several inputs, -o must be a directory (end it with /)."));
    }
    if opts.recursive && (output_is_file || opts.mask_out.is_some()) {
        return Err(Error::usage("--recursive needs -o to be a directory (end it with /), and can't use --mask-out."));
    }
    if opts.format.is_none() && output_is_file {
        let ext = Path::new(opts.output.as_deref().unwrap_or_default()).extension().and_then(|e| e.to_str()).unwrap_or_default();
        opts.format = Some(Format::parse(ext).ok_or_else(|| Error::usage("Can't tell the output format from -o; add -f png|jpeg|webp."))?);
    }
    if palette_clean && palette.is_none() {
        return Err(Error::usage("--palette-clean only applies with --palette."));
    }
    if let Some(spec) = palette {
        let cwd = std::env::current_dir().map_err(|e| Error::other(format!("No current folder: {e}")))?;
        let places = crate::presets::Places { cwd: cwd.clone(), global_dir: crate::presets::global_dir() };
        let colors = crate::palette::resolve(&spec, &cwd, || places.library())?;
        opts.transform.palette = Some(transform::PaletteFit { colors, clean: palette_clean });
    }
    // Without -f/-o the format comes from each input; convert_one checks again then.
    opts.encoding.check(opts.format)?;
    opts.transform.check_output(opts.format, &opts.encoding)?;
    opts.transform.check()?;
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

pub fn read_image(path: &Path) -> Result<(Vec<u8>, Format)> {
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

/// One file to convert, and the directory its output goes to when --recursive mirrors a tree.
#[derive(Debug)]
struct Planned {
    input: PathBuf,
    output: Option<String>,
}

/// The inputs as files. With --recursive a directory becomes its images, sorted by path, each
/// going to the same relative place under -o (or next to itself without -o). Fails before any
/// conversion if two inputs would write the same output file.
fn plan(opts: &ConvertOptions) -> Result<Vec<Planned>> {
    let mut planned = Vec::new();
    for input in &opts.inputs {
        let root = Path::new(input);
        if !opts.recursive || !root.is_dir() {
            planned.push(Planned { input: root.to_path_buf(), output: opts.output.clone() });
            continue;
        }
        let mut files = Vec::new();
        walk(root, &mut files)?;
        if files.is_empty() {
            return Err(Error::usage(format!("{input} has no PNG, JPEG or WebP files.")));
        }
        for file in files {
            let output = opts.output.as_deref().map(|o| {
                let relative = file.parent().and_then(|p| p.strip_prefix(root).ok()).unwrap_or(Path::new(""));
                let dir = if relative.as_os_str().is_empty() { o.to_string() } else { Path::new(o).join(relative).display().to_string() };
                if dir.ends_with(['/', '\\']) { dir } else { format!("{dir}/") }
            });
            planned.push(Planned { input: file, output });
        }
    }
    if opts.recursive {
        let mut seen = std::collections::HashMap::new();
        for item in &planned {
            let format = opts.format.or_else(|| item.input.extension().and_then(|e| Format::parse(&e.to_string_lossy())));
            let Some(format) = format else { continue };
            let target = target_path(&item.input, item.output.as_deref(), format);
            if let Some(other) = seen.insert(target.clone(), item.input.clone()) {
                return Err(Error::usage(format!("{} and {} would both write {}.", other.display(), item.input.display(), target.display())));
            }
        }
    }
    Ok(planned)
}

/// Images under `dir`, in sorted path order so runs and reports are reproducible. Hidden entries
/// and symlinked directories are skipped.
fn walk(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let fail = |e: std::io::Error| Error::other(format!("Unable to read {}: {e}", dir.display()));
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir).map_err(fail)?.map(|e| e.map(|e| e.path())).collect::<std::io::Result<_>>().map_err(fail)?;
    entries.sort();
    for path in entries {
        if path.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
            continue;
        }
        let is_link = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());
        if path.is_dir() {
            if !is_link {
                walk(&path, files)?;
            }
        } else if path.extension().is_some_and(|e| Format::parse(&e.to_string_lossy()).is_some()) {
            files.push(path);
        }
    }
    Ok(())
}

fn convert_one(input: &Path, output: Option<&str>, opts: &ConvertOptions) -> Result<serde_json::Value> {
    let started = Instant::now();
    let (bytes, actual) = read_image(input)?;
    let format = opts.format.unwrap_or(actual);
    // parse() can only check -f/-o; without them the output takes the input's format.
    opts.encoding.check(Some(format)).map_err(|e| Error::usage(format!("{}: {}", input.display(), e.message)))?;
    opts.transform.check_output(Some(format), &opts.encoding).map_err(|e| Error::usage(format!("{}: {}", input.display(), e.message)))?;
    let target = target_path(input, output, format);
    if same_file(&target, input) {
        return Err(Error::usage(format!("Output would overwrite the input {}; choose another -o.", input.display())));
    }
    if !opts.force && target.exists() {
        return Err(Error::other(format!("{} already exists; add --force to replace it.", target.display())));
    }
    if let Some(mask) = &opts.mask_out {
        let mask = Path::new(mask);
        if same_file(mask, input) || same_file(mask, &target) { return Err(Error::usage("The mask must be separate from the input and output.")); }
        if !opts.force && mask.exists() { return Err(Error::other(format!("{} already exists.", mask.display()))); }
    }
    let converted = cli::save_converted(&bytes, format, &opts.encoding, &opts.transform, &target, opts.force)?;
    let written = std::fs::metadata(&converted.path).map(|m| m.len()).unwrap_or_default();
    let size = |(w, h): (u32, u32)| format!("{w}x{h}");
    let mut info = json!({
        "path": converted.path.display().to_string(),
        "input": input.display().to_string(),
        "format": format.name(),
        "bytes": written,
        "inputBytes": bytes.len(),
        "size": size(converted.size),
        "inputSize": size(converted.input_size),
        "durationMs": started.elapsed().as_millis() as u64,
    });
    info["hardAlphaPixels"] = json!(converted.changes.hard_alpha_pixels);
    info["keyedOutPixels"] = json!(converted.changes.keyed_out_pixels);
    if let Some(colors) = converted.changes.palette_colors { info["paletteColors"] = json!(colors); }
    if let Some(path) = &opts.mask_out {
        let mask = converted.changes.key_mask.as_ref().ok_or_else(|| Error::other("The key mask was not produced."))?;
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(mask.clone()).write_to(&mut bytes, image::ImageFormat::Png).map_err(|e| Error::other(e.to_string()))?;
        cli::write_output(Path::new(path), bytes.get_ref(), opts.force)?;
        info["maskPath"] = json!(path);
    }
    // Where the trimmed content sat in the input, to keep sprite anchors in place.
    if let Some(rect) = converted.trim {
        info["trim"] = json!({"x": rect.x, "y": rect.y, "width": rect.width, "height": rect.height});
    }
    if let Some(colors) = opts.encoding.colors {
        info["colors"] = json!(colors);
    }
    if converted.unchanged {
        info["unchanged"] = json!(true);
    }
    // Same key as generation's --json: the lossy quality actually applied, defaults included.
    if let Some(quality) = converted.output_quality {
        info["outputQuality"] = json!(quality);
    }
    Ok(info)
}

/// Converts each input in turn; a failure is reported and the rest still run. Returns the exit code.
pub fn run(opts: &ConvertOptions) -> i32 {
    let planned = match plan(opts) {
        Ok(planned) => planned,
        Err(error) => {
            eprintln!("codex-img: {error}");
            return error.kind.exit_code();
        }
    };
    let mut exit_code = 0;
    let (mut done, mut failed, mut input_bytes, mut output_bytes) = (0u64, 0u64, 0u64, 0u64);
    let run = crate::events::Run::start("convert", planned.len(), json!({}));
    for Planned { input, output } in &planned {
        let input = input.as_path();
        let job = run.job(json!({"prompt":"Local conversion","parent":crate::events::absolute(input),"inputs":[{"path":crate::events::absolute(input),"role":"input"}],"request":{}}));
        match convert_one(input, output.as_deref(), opts) {
            Ok(info) => {
                job.done(json!({"path":crate::events::absolute(Path::new(info["path"].as_str().unwrap())),"rawPath":crate::events::absolute(input),"durationMs":info["durationMs"],"size":info["size"]}));
                let (from, to) = (info["inputBytes"].as_u64().unwrap_or(0), info["bytes"].as_u64().unwrap_or(0));
                (done, input_bytes, output_bytes) = (done + 1, input_bytes + from, output_bytes + to);
                if opts.json {
                    println!("{info}");
                } else {
                    println!("{}", info["path"].as_str().unwrap_or_default());
                }
                if !opts.quiet {
                    let note = if info["unchanged"] == true { ", unchanged" } else { "" };
                    eprintln!("{} -> {} ({} KB -> {} KB{note})", input.display(), info["path"].as_str().unwrap_or_default(), from / 1024, to / 1024);
                }
            }
            Err(error) => {
                job.failed(&error);
                eprintln!("codex-img: {error}");
                failed += 1;
                exit_code = exit_code.max(error.kind.exit_code());
            }
        }
    }
    run.end(None);
    if opts.recursive {
        if opts.json {
            println!("{}", json!({"total": {"files": done, "failed": failed, "inputBytes": input_bytes, "bytes": output_bytes}}));
        }
        if !opts.quiet {
            eprintln!("{done} converted, {failed} failed ({} KB -> {} KB)", input_bytes / 1024, output_bytes / 1024);
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

    #[test]
    fn reports_key_and_alpha_changes_and_masks_before_trim_and_resize() {
        let dir = crate::auth::tests::temp_dir("convert-mask");
        let input = dir.join("input.png"); let output = dir.join("output.png"); let mask = dir.join("mask.png");
        let mut pixels = image::RgbaImage::from_pixel(4, 3, image::Rgba([0, 0, 255, 255]));
        pixels.put_pixel(1, 1, image::Rgba([255, 0, 0, 255]));
        pixels.put_pixel(2, 1, image::Rgba([255, 0, 0, 10]));
        pixels.save(&input).unwrap();
        let options = parse(&[input.display().to_string(), "-o".into(), output.display().to_string(), "--key=blue".into(), "--hard-alpha".into(), "--trim".into(), "--resize=2x2".into(), "--mask-out".into(), mask.display().to_string()]).unwrap().unwrap();
        let info = convert_one(&input, options.output.as_deref(), &options).unwrap();
        assert_eq!(info["keyedOutPixels"], 10); assert_eq!(info["hardAlphaPixels"], 1); assert!(info["trim"].is_object());
        let removed = image::open(&mask).unwrap().to_luma8(); assert_eq!(removed.dimensions(), (4, 3));
        assert_eq!(removed.get_pixel(0, 0)[0], 255); assert_eq!(removed.get_pixel(1, 1)[0], 0); assert_eq!(removed.get_pixel(2, 1)[0], 0);
        assert_eq!(image::open(input).unwrap().to_rgba8(), pixels);
        assert!(parse(&args(&["a.png", "b.png", "--key=blue", "--mask-out=m.png"])).is_err());
        assert!(parse(&args(&["a.png", "--mask-out=m.png"])).is_err());
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

        let t = parse(&args(&["a.png", "--trim=8", "--resize", "512x512", "--fit=cover", "--no-bleed"])).unwrap().unwrap().transform;
        assert_eq!((t.trim, t.resize.and_then(|r| r.width), t.fit, t.no_bleed), (Some(8), Some(512), Some(Fit::Cover), true));
        assert_eq!(parse(&args(&["a.png", "--trim"])).unwrap().unwrap().transform.trim, Some(0));
        assert_eq!(parse(&args(&["a.png", "--hard-alpha=40"])).unwrap().unwrap().transform.hard_alpha, Some(40));
        assert!(parse(&args(&["a.png", "--resize", "9x", "--no-enlarge"])).unwrap().unwrap().transform.no_enlarge);
        assert!(usage_error(&["a.png", "--no-enlarge"]).contains("--resize"));
        assert!(usage_error(&["a.png", "--fit", "cover"]).contains("--resize"));
        assert!(usage_error(&["a.png", "--resize", "big"]).contains("WxH"));
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

        // --force replaces the file, and leaves it alone when the bytes would be the same.
        let min = dir.join("in.min.png");
        std::fs::write(&min, b"stale").unwrap();
        let forced = parse(&args(&[&input_str, "-c", "4", "--quiet", "--force"])).unwrap().unwrap();
        let info = convert_one(&input, forced.output.as_deref(), &forced).unwrap();
        assert!(info.get("unchanged").is_none());
        let written = std::fs::read(&min).unwrap();
        assert_eq!(images::sniff(&written), Some(Format::Png));
        let modified = std::fs::metadata(&min).unwrap().modified().unwrap();
        assert_eq!(convert_one(&input, forced.output.as_deref(), &forced).unwrap()["unchanged"], true);
        assert_eq!(std::fs::metadata(&min).unwrap().modified().unwrap(), modified, "same bytes: not rewritten");
        assert_eq!(std::fs::read_dir(&dir).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".tmp")).count(), 0);

        let opts = parse(&args(&[&input_str, "-o", &input_str, "--quiet"])).unwrap().unwrap();
        assert_eq!(run(&opts), 64, "refuses to overwrite the input");
        let opts = parse(&args(&[&input_str, "-o", &input_str, "--quiet", "--force"])).unwrap().unwrap();
        assert_eq!(run(&opts), 64, "even with --force");
        assert_eq!(std::fs::read(&input).unwrap(), png);

        let text = dir.join("notes.txt");
        std::fs::write(&text, "hi").unwrap();
        assert_eq!(run(&parse(&args(&[&text.display().to_string(), "--quiet"])).unwrap().unwrap()), 1);
    }

    #[test]
    fn recursive_mirrors_the_tree_in_sorted_order_and_refuses_collisions() {
        let dir = crate::auth::tests::temp_dir("convert-recursive");
        let source = dir.join("units");
        for name in ["Germans/infantry.png", "Germans/tank.png", "Russians/infantry.png", "flag.png", ".hidden/x.png"] {
            std::fs::create_dir_all(source.join(name).parent().unwrap()).unwrap();
            image::RgbaImage::from_pixel(2, 2, image::Rgba([200, 0, 0, 255])).save(source.join(name)).unwrap();
        }
        std::fs::write(source.join("map.properties"), "x").unwrap();
        let out = dir.join("web");
        let options = parse(&args(&["-r", &source.display().to_string(), "-f", "webp", "-o", &format!("{}/", out.display())])).unwrap().unwrap();
        let planned: Vec<_> = plan(&options).unwrap().into_iter().map(|p| p.input.strip_prefix(&source).unwrap().display().to_string()).collect();
        assert_eq!(planned, ["Germans/infantry.png", "Germans/tank.png", "Russians/infantry.png", "flag.png"].map(|p| p.replace('/', std::path::MAIN_SEPARATOR_STR)));
        assert_eq!(run(&ConvertOptions { quiet: true, ..options.clone() }), 0);
        for name in ["Germans/infantry.webp", "Germans/tank.webp", "Russians/infantry.webp", "flag.webp"] { assert!(out.join(name).is_file(), "{name}"); }
        assert!(!out.join(".hidden").exists());

        image::RgbImage::new(2, 2).save(source.join("flag.jpg")).unwrap();
        assert!(plan(&options).unwrap_err().message.contains("would both write"));
        assert!(usage_error(&["-r", "a", "-o", "b.png"]).contains("--recursive"));
    }
}
