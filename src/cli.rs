use crate::backend::{Generated, DEFAULT_ROUTING_MODEL};
use crate::error::{Error, Result};
use crate::images::{self, Format, MAX_EDIT_IMAGES};
use crate::presets::Setup;
use crate::transform;
use crate::util;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub const VERSION: &str = env!("CODEX_IMG_VERSION");
pub const MAX_PROMPT_CHARS: usize = 32_000;

pub fn help() -> String {
    format!(
        r#"codex-img {VERSION} - generate images with your ChatGPT/Codex subscription

Usage:
  codex-img [options] "<prompt>"
  echo "<prompt>" | codex-img [options] -
  codex-img init [folder] [--no-events]  Start a project: write codex-img.json
  codex-img check <file> [--json]      Validate specs or presets (no quota)
  codex-img refine <image> <change>    Edit only the requested change (1 image of quota)
  codex-img comments [--json] [folder]   List or change review comments (no quota)
  codex-img stars [--json] [folder]      List or change image stars (no quota)
  codex-img rerun <image> [-n N] [-o out] [--anyway]
                              Generate again from its manifest (uses quota)
  codex-img status [--json]   Check the Codex login offline (uses no quota)
  codex-img convert <input>... [-o path] [-f fmt] [-c n] [--trim] [--resize WxH]
                              Convert, trim or resize existing images locally (no quota);
                              see `codex-img convert --help`
  codex-img sheet <input>... -o sheet.png
                              Lay images out in one labelled grid to review a batch;
                              see `codex-img sheet --help`
  codex-img atlas <input or dir>... -o atlas.webp
                              Pack images into texture atlas pages with TexturePacker
                              JSON (no quota); see `codex-img atlas --help`
  codex-img pyramid <tile dir> -o <dir>/
                              Zoomed-out levels of a tiled map, with a manifest (no
                              quota); see `codex-img pyramid --help`
  codex-img batch <spec.json> [key or folder...]
                              Generate the missing images of a JSON asset spec, then
                              convert them all; see `codex-img batch --help`
  codex-img tile <panorama> -o tile.png
                              Make a panorama wrap around seamlessly (one image of
                              quota); see `codex-img tile --help`
  codex-img presets [add|show|remove|promote ...]
                              List or manage view, style and character presets (no
                              quota); see `codex-img presets --help`

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
                            --nearest copies pixels for whole-number pixel-art upscaling
      --no-bleed            Keep the colour under fully transparent pixels (by
                            default PNG and lossless webp get the nearest edge colour)
  -a, --aspect <W:H>        Frame shape, 1:3 to 3:1 (16:9, 2:3, 1:1): leads the prompt
                            with a sentence asking for it, and warns if the result
                            is off. For exact pixels add --resize WxH --fit cover
  -s, --size <WxH>          Sent to the backend, which has ignored it in tests; use
                            --aspect for the shape
      --view <name>         Camera preset: side, front, top-down, three-quarter,
                            isometric, or one of your own (`codex-img presets`)
      --style <name>        Style preset: its text ends the prompt, its refs are sent
                            as style references (repeatable)
      --character <name>    Character preset: its text and identity refs (repeatable)
      --style-ref <image>   Style reference: palette and rendering, not the subject
      --character-ref <image>
                            Character reference: the same character, new pose/scene
      --composition-ref <image>
                            Composition reference: layout and framing only
                            (each repeatable; they follow -i, max {MAX_EDIT_IMAGES} images in all,
                            and each is labelled in the prompt)
      --palette <colours>   Limit the image to a palette: '#2B1D14,#6B3E26,...', a
                            .gpl/.hex file, a swatch image, or a palette preset
                            (pico-8, nes, c64, sweetie-16, ...; `codex-img presets`
                            lists them, and your own). The hex codes are added to
                            the prompt, and every colour is snapped to the nearest
                            one afterwards (alpha hardened, written as a palette PNG)
      --palette-clean       Stronger cleanup for art not made with the palette:
                            fewer stray pixels, colours matched by hue first
      --manifest            Write <image>.json: the prompt sent, presets, inputs
                            and what the backend reported (automatic in projects)
      --parent <image>      Link the new image to this earlier image
      --no-parent           Do not infer a parent from a single -i image
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
Appends progress events to ~/.local/state/codex-img/events.ndjson ($CODEX_IMG_EVENTS, `off` to stop).
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
    /// Aspect, presets and reference roles, which `presets::compose` turns into the sent prompt.
    pub setup: Setup,
    pub manifest: bool,
    pub parent: Option<String>,
    pub no_parent: bool,
    pub replay: Option<Box<crate::rerun::Saved>>,
    pub transfer: Option<crate::refine::Transfer>,
    /// What made this image from its parent: "rerun" or "edit". Recorded in the manifest and job events.
    pub kind: Option<&'static str>,
    /// --palette as given; `main` resolves it (a name may need the preset files).
    pub palette: Option<String>,
    pub palette_clean: bool,
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
    Atlas(crate::atlas::AtlasOptions),
    AtlasHelp,
    Pyramid(crate::pyramid::PyramidOptions),
    PyramidHelp,
    Batch(crate::batch::BatchOptions),
    BatchHelp,
    Tile(crate::tile::TileOptions),
    TileHelp,
    Presets(crate::presets::PresetsOptions),
    PresetsHelp,
    Check(crate::check::Options),
    CheckHelp,
    Init(crate::project::InitOptions),
    InitHelp,
    Rerun(crate::rerun::Options),
    RerunHelp,
    Refine(crate::refine::Options),
    RefineHelp,
    Review(crate::review::Options),
    ReviewHelp(bool),
    Run(Box<Options>),
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

/// A frame shape asked for in the prompt. The direct route ignores the `size` field, but keeps
/// to a ratio stated at the start of the prompt, at about the same pixel count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aspect {
    pub width: u32,
    pub height: u32,
}

impl Aspect {
    /// Larger ratios came back clamped to 3:1, so they're refused before any quota is spent.
    pub fn parse(value: &str) -> Result<Aspect> {
        let side = |s: &str| s.parse::<u32>().ok().filter(|n| (1..=100).contains(n));
        let (width, height) = value
            .split_once(':')
            .and_then(|(w, h)| Some((side(w)?, side(h)?)))
            .ok_or_else(|| Error::usage("--aspect must be W:H, such as 16:9 or 2:3."))?;
        if width > 3 * height || height > 3 * width {
            return Err(Error::usage("--aspect must be between 1:3 and 3:1; the backend clamps anything wider or taller."));
        }
        Ok(Aspect { width, height })
    }

    /// The sentence that sets the frame; `presets::compose` puts it first.
    pub fn sentence(&self) -> String {
        let (w, h) = (self.width, self.height);
        let shape = match w.cmp(&h) {
            std::cmp::Ordering::Greater => "landscape format, wider than it is tall",
            std::cmp::Ordering::Less => "portrait format, taller than it is wide",
            std::cmp::Ordering::Equal => "square format, as wide as it is tall",
        };
        format!("The frame must be in {w}:{h} {shape}.")
    }

    /// A warning when `size` is more than 2% off the ratio: wrong framing, not rounding.
    pub fn mismatch(&self, (w, h): (u32, u32)) -> Option<String> {
        let wanted = self.width as f64 / self.height as f64;
        let got = w as f64 / h.max(1) as f64;
        ((got / wanted - 1.0).abs() > 0.02).then(|| format!("asked for {}:{} but the backend returned {w}x{h}", self.width, self.height))
    }
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
    if args.first().is_some_and(|a| a == "atlas") {
        return Ok(crate::atlas::parse(&args[1..])?.map_or(Command::AtlasHelp, Command::Atlas));
    }
    if args.first().is_some_and(|a| a == "pyramid") {
        return Ok(crate::pyramid::parse(&args[1..])?.map_or(Command::PyramidHelp, Command::Pyramid));
    }
    if args.first().is_some_and(|a| a == "batch") {
        return Ok(crate::batch::parse(&args[1..])?.map_or(Command::BatchHelp, Command::Batch));
    }
    if args.first().is_some_and(|a| a == "tile") {
        return Ok(crate::tile::parse(&args[1..])?.map_or(Command::TileHelp, Command::Tile));
    }
    if args.first().is_some_and(|a| a == "check") {
        return Ok(crate::check::parse(&args[1..])?.map_or(Command::CheckHelp, Command::Check));
    }
    if args.first().is_some_and(|a| a == "init") {
        return Ok(crate::project::parse_init(&args[1..])?.map_or(Command::InitHelp, Command::Init));
    }
    if args.first().is_some_and(|a| a == "presets") {
        return Ok(crate::presets::parse(&args[1..])?.map_or(Command::PresetsHelp, Command::Presets));
    }
    if args.first().is_some_and(|a| a == "comments" || a == "stars") {
        let star = args[0] == "stars";
        return Ok(crate::review::parse(&args[1..], star)?.map_or(Command::ReviewHelp(star), Command::Review));
    }
    if args.first().is_some_and(|a| a == "refine") {
        return Ok(crate::refine::parse(&args[1..])?.map_or(Command::RefineHelp, Command::Refine));
    }
    if args.first().is_some_and(|a| a == "rerun") {
        return Ok(crate::rerun::parse(&args[1..])?.map_or(Command::RerunHelp, Command::Rerun));
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
            "-a" | "--aspect" => "aspect",
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
            "--nearest" => "nearest",
            "--key" => "key",
            "--key-region" => "key-region",
            "--trim-density" => "trim-density",
            "--key-cut" => "key-cut",
            "--key-spread" => "key-spread",
            "--view" => "view",
            "--style" => "style",
            "--character" => "character",
            "--style-ref" => "style-ref",
            "--character-ref" => "character-ref",
            "--composition-ref" => "composition-ref",
            "--manifest" => "manifest",
            "--parent" => "parent",
            "--no-parent" => "no-parent",
            "--palette" => "palette",
            "--palette-clean" => "palette-clean",
            "-h" | "--help" => "help",
            "-v" | "--version" => "version",
            _ => return Err(Error::usage(format!("Unknown option: {arg}"))),
        };
        // --trim and --hard-alpha take values only inline (--trim=8): a bare word after them is the prompt.
        if matches!(key, "json" | "quiet" | "via-responses" | "dither" | "lossless" | "no-bleed" | "no-enlarge" | "nearest" | "manifest" | "no-parent" | "palette-clean" | "help" | "version")
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
    let aspect = last("aspect").map(|v| Aspect::parse(&v)).transpose()?;
    if aspect.is_some() && size.as_deref().is_some_and(|s| s != "auto") {
        return Err(Error::usage("--aspect and --size can't be combined; the backend ignores --size, and --aspect sets the frame."));
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
        effort: None,
    };
    // The format may still be unknown here (it defaults to PNG); generate() checks again.
    encoding.check(format)?;
    let transform = transform::Transform {
        hard_alpha: match last("hard-alpha") {
            Some(threshold) => Some(transform::parse_hard_alpha(&threshold)?),
            None => flags.contains(&"hard-alpha").then_some(transform::FAINT_ALPHA),
        },
        keys: values.iter().filter(|(k, _)| *k == "key").map(|(_, v)| transform::Key::parse(v)).collect::<codex_img_core::error::Result<_>>()?,
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
        nearest: flags.contains(&"nearest"),
        palette: None,
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
    if flags.contains(&"palette-clean") && last("palette").is_none() {
        return Err(Error::usage("--palette-clean only applies with --palette."));
    }
    let all = |key: &str| values.iter().filter(|(k, _)| *k == key).map(|(_, v)| v.clone()).collect::<Vec<_>>();
    let paths = |key: &str| all(key).into_iter().map(PathBuf::from).collect::<Vec<_>>();
    if flags.contains(&"no-parent") && last("parent").is_some() { return Err(Error::usage("--parent and --no-parent cannot be combined.")); }
    let setup = Setup {
        aspect,
        view: last("view"),
        styles: all("style"),
        characters: all("character"),
        style_refs: paths("style-ref"),
        character_refs: paths("character-ref"),
        composition_refs: paths("composition-ref"),
        palette: None,
    };
    Ok(Command::Run(Box::new(Options {
        prompt: positionals.join(" "),
        output,
        images,
        format,
        size,
        setup,
        manifest: flags.contains(&"manifest"),
        parent: last("parent"),
        no_parent: flags.contains(&"no-parent"),
        replay: None,
        transfer: None,
        kind: None,
        palette: last("palette"),
        palette_clean: flags.contains(&"palette-clean"),
        quality: one_of("quality", last("quality"), &["low", "medium", "high", "auto"])?,
        background: one_of("background", last("background"), &["transparent", "opaque", "auto"])?,
        encoding,
        transform,
        model,
        via_responses,
        count,
        json: flags.contains(&"json"),
        quiet: flags.contains(&"quiet"),
    })))
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
/// and its folder creatable. A folder `-o` is created here too; names made up in it are unique.
/// `save_image` still refuses to overwrite a file that appears while the request runs.
pub fn check_output(output: Option<&str>, format: Format, count: usize, transform: &transform::Transform, manifest: bool, raw_manifest: bool) -> Result<()> {
    let Some(output) = output else { return Ok(()) };
    let path = std::env::current_dir().unwrap_or_default().join(output);
    if output.ends_with('/') || path.is_dir() {
        return std::fs::create_dir_all(&path).map_err(|e| Error::other(format!("Could not create {}: {e}", path.display())));
    }
    for index in 0..count {
        let target = output_path(Some(output), format, "", index, count, 0);
        let mut targets = vec![target.clone()];
        if transform.edits() {
            // The original keeps the backend's format, which is PNG unless asked for otherwise.
            for format in [Format::Png, format] {
                let raw = raw_path(&target, format);
                if raw_manifest {
                    targets.push(crate::manifest::path_for(&raw));
                }
                targets.push(raw);
            }
        }
        if manifest {
            targets.push(crate::manifest::path_for(&target));
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

pub fn parse_effort(value: &str) -> Result<u8> {
    value.parse::<u8>().ok().filter(|n| *n <= images::MAX_EFFORT).ok_or_else(|| Error::usage("--effort must be an integer from 0 to 9."))
}

fn create_parent(path: &Path) -> Result<()> {
    match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(parent) => std::fs::create_dir_all(parent).map_err(|e| Error::other(format!("Could not create {}: {e}", parent.display()))),
        None => Ok(()),
    }
}

pub use codex_img_core::conversion::Converted;
fn process(bytes: &[u8], actual: Format, wanted: Format, enc: &images::Encoding, transform: &transform::Transform, lenient: bool) -> Result<codex_img_core::conversion::Processed> {
    codex_img_core::conversion::process(bytes, actual, wanted, enc, transform, lenient).map_err(Into::into)
}
pub fn save_converted(bytes: &[u8], wanted: Format, enc: &images::Encoding, transform: &transform::Transform, path: &Path, overwrite: bool) -> Result<Converted> {
    codex_img_core::conversion::save_converted(bytes, wanted, enc, transform, path, overwrite).map_err(Into::into)
}
pub fn write_output(path: &Path, bytes: &[u8], overwrite: bool) -> Result<bool> {
    codex_img_core::output::write_output(path, bytes, overwrite).map_err(Into::into)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> { codex_img_core::output::write_new(path, bytes).map_err(Into::into) }

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
            Command::Run(options) => *options,
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
    fn aspect_leads_the_prompt_and_flags_a_wrong_frame() {
        assert_eq!(run(&["-a", "16:9", "x"]).setup.aspect, Some(Aspect { width: 16, height: 9 }));
        assert_eq!(run(&["--aspect=2:3", "-s", "auto", "x"]).setup.aspect, Some(Aspect { width: 2, height: 3 }));
        assert!(usage_error(&["--aspect", "16x9", "x"]).contains("W:H"));
        assert!(usage_error(&["--aspect", "0:1", "x"]).contains("W:H"));
        assert!(usage_error(&["--aspect", "4:1", "x"]).contains("1:3 and 3:1"));
        assert!(usage_error(&["-a", "2:3", "-s", "1024x1536", "x"]).contains("can't be combined"));
        let o = run(&["--view", "side", "--style", "pixel", "--style=ink", "--character", "cap", "--style-ref", "s.png", "--character-ref", "c.png", "--composition-ref", "l.png", "--manifest", "x"]);
        let paths = |list: &[&str]| list.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            o.setup,
            Setup {
                aspect: None,
                view: Some("side".into()),
                styles: vec!["pixel".into(), "ink".into()],
                characters: vec!["cap".into()],
                style_refs: paths(&["s.png"]),
                character_refs: paths(&["c.png"]),
                composition_refs: paths(&["l.png"]),
                palette: None,
            }
        );
        assert!(o.manifest && o.prompt == "x", "--manifest takes no value");
        let o = run(&["--palette", "#000000,#FFFFFF", "--palette-clean", "x"]);
        assert_eq!((o.palette.as_deref(), o.palette_clean, o.prompt.as_str()), (Some("#000000,#FFFFFF"), true, "x"));
        assert!(usage_error(&["--palette-clean", "x"]).contains("--palette"));
        let sentence = |w, h| Aspect { width: w, height: h }.sentence();
        assert_eq!(sentence(2, 3), "The frame must be in 2:3 portrait format, taller than it is wide.");
        assert_eq!(sentence(16, 9), "The frame must be in 16:9 landscape format, wider than it is tall.");
        assert_eq!(sentence(1, 1), "The frame must be in 1:1 square format, as wide as it is tall.");
        // What the backend returned in tests is within 2%; a wrong frame isn't.
        let mismatch = |w, h, size| Aspect { width: w, height: h }.mismatch(size);
        assert_eq!((mismatch(2, 3, (1024, 1536)), mismatch(16, 9, (1672, 941)), mismatch(3, 1, (2172, 724))), (None, None, None));
        assert_eq!(mismatch(2, 3, (941, 1672)).unwrap(), "asked for 2:3 but the backend returned 941x1672");
        assert!(mismatch(1, 1, (1312, 1199)).is_some());
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
        assert!(matches!(parse(&args(&["init", "--no-events"])).unwrap(), Command::Init(_)));
        assert_eq!(parse(&args(&["init", "--help"])).unwrap(), Command::InitHelp);
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
    fn check_output_refuses_taken_names_before_any_request() {
        let dir = crate::auth::tests::temp_dir("check-output");
        let out = dir.join("new/car.png");
        let at = |p: &Path| p.display().to_string();
        let none = transform::Transform::default();
        let trim = transform::Transform { trim: Some(0), ..Default::default() };
        check_output(Some(&at(&out)), Format::Png, 1, &none, false, false).unwrap();
        assert!(dir.join("new").is_dir(), "the folder is created up front");

        std::fs::write(dir.join("new/car.raw.png"), b"x").unwrap();
        check_output(Some(&at(&out)), Format::Png, 1, &none, false, false).unwrap();
        let err = check_output(Some(&at(&out)), Format::Png, 1, &trim, false, false).unwrap_err();
        assert!(err.message.contains("car.raw.png already exists"), "{}", err.message);

        std::fs::write(dir.join("new/car-2.png"), b"x").unwrap();
        check_output(Some(&at(&out)), Format::Png, 1, &none, false, false).unwrap();
        assert!(check_output(Some(&at(&out)), Format::Png, 2, &none, false, false).unwrap_err().message.contains("car-2.png"));

        // A folder gets new, unique names.
        check_output(Some(&at(&dir.join("new"))), Format::Png, 3, &trim, false, false).unwrap();
        check_output(None, Format::Png, 1, &trim, false, false).unwrap();
        std::fs::write(dir.join("noted.png.json"), b"{}").unwrap();
        assert!(check_output(Some(&at(&dir.join("noted.png"))), Format::Png, 1, &none, true, false).unwrap_err().message.contains("noted.png.json"));
        check_output(Some(&at(&dir.join("noted.png"))), Format::Png, 1, &none, false, false).unwrap();
        // A folder -o is created, and one that can't be is refused.
        check_output(Some(&format!("{}/", at(&dir.join("made")))), Format::Png, 1, &none, false, false).unwrap();
        assert!(dir.join("made").is_dir());
        // A file where the folder should be.
        std::fs::write(dir.join("blocked"), b"x").unwrap();
        assert!(check_output(Some(&at(&dir.join("blocked/car.png"))), Format::Png, 1, &none, false, false).is_err());
        assert!(check_output(Some(&format!("{}/", at(&dir.join("blocked")))), Format::Png, 1, &none, false, false).is_err());
    }
}
