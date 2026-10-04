//! `codex-img batch`: generate and convert a set of assets described in a JSON spec. Missing raw
//! images are generated (references first), then every raw image is converted into its published
//! form. Re-rolls preserve the old raw in history.
use crate::auth::{self, Credentials};
use crate::backend::{Backend, Request, Transport};
use crate::cli::{self, Aspect};
use crate::manifest;
use crate::presets::{self, Library, RoleImage, Setup, Source};
use crate::convert;
use crate::error::{Error, Kind, Result};
use crate::events;
use crate::images::{self, Encoding, Format, MAX_EDIT_IMAGES};
use crate::transform::{self, Density, Fit, Key, Region, Resize, Transform};
use crate::util;
use serde_json::{json, Map, Value};
use std::path::{Component, Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

#[path = "batch_refine.rs"]
mod refinement;
pub fn refine(opts: &crate::refine::Options) -> Result<i32> { refinement::run(opts, &Backend::default(), &auth::load_credentials) }

const DEFAULT_JOBS: usize = 4;
const MAX_JOBS: usize = 10;
const DEFAULT_RAW_DIR: &str = "raw";
const DEFAULT_OUT_DIR: &str = "out";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchOptions {
    pub spec: String,
    pub filters: Vec<String>,
    pub generate: bool,
    pub convert: bool,
    pub jobs: usize,
    pub dry_run: bool,
    pub json: bool,
    pub quiet: bool,
    pub reroll: Vec<String>,
    pub restore: Option<(String, String)>,
    pub inspect: bool,
    pub expect_images: Option<usize>,
    pub no_wait: bool,
}

pub fn help() -> &'static str {
    r#"Usage:
  codex-img batch <spec.json> [<key or folder>...] [options]

Generates the missing raw images of the assets in a JSON spec, then converts every
raw image into its published form. Raw images that exist are kept; --reroll
archives them only after a replacement succeeds. Converted files are replaced only when their bytes change.

Options:
      --inspect             Show raw/output status, edited/changed, conversion and history (free)
                            Pending images that were not activated are excluded
      --reroll <key>...     Replace exact keys, preserving their raw images in history
      --restore <key> <file> Copy any image into raw (no quota), even without a manifest
                            Keep the source and its reviews; clear/move any current raw manifest comment first
      --generate-only       Only generate missing raw images
      --convert-only        Only convert (no login, no quota)
  -j, --jobs <n>            Images generated at the same time (default 4, max 10)
      --dry-run             Show what would be generated and converted
      --expect-images <n>   Stop before quota if the generation plan count changed
      --json                One JSON object per asset and step on stdout
      --no-wait             Fail immediately if another run holds this spec’s lock
                            Conversion takes one lock per phase; workers run in parallel
      --quiet               Report failures only

A filter is an exact key (shared/trees/oak) or a folder of keys (tracks/city).

Spec:
  {
    "raw_dir": "raw",       generated images, <raw_dir>/<key>.png (default "raw")
    "out_dir": "out",       converted images, <out_dir>/<key>.<ext> (default "out");
                            both relative to the spec file
    "style": "...",         text appended to every prompt
    "views": {...},         presets of this spec, as in codex-img.json (see
    "styles": {...},        `codex-img presets --help`); they win over the
    "characters": {...},    project's and global ones
    "palettes": {...},
    "defaults": {...},      fields every asset gets unless it sets them itself
    "assets": {
      "trees/oak": {"prompt": "...", "aspect": "3:2", "max": "420x380"},
      "mill/sails": {"prompt": "Only the sails of this windmill", "reference": "mill/full"},
      "mill/full": {"prompt": "...", "publish": false}
    }
  }

Asset fields (in an asset, null or false turns a default off):
  prompt                    Required
  aspect, size, quality,    As the generation options -a, -s, -q and -b
  background
  view, style, character    Preset names, as --view, --style and --character (style
                            and character can be lists)
  style_ref, character_ref, Reference images with a role, as the --*-ref options;
  composition_ref           relative to the spec
  palette, palette_clean    As --palette and --palette-clean: hex codes (a string or a
                            list), a file relative to the spec, or a palette preset.
                            The hex codes join the prompt; conversion snaps to them
  reference                 Key(s) whose raw image is passed as -i; generated first
  images                    Other -i files, relative to the spec
  publish                   false: generate only, e.g. a reference for other assets
  format                    png (default), jpeg or webp
  colors, dither, output_quality, lossless, trim, hard_alpha, resize, fit,
  no_enlarge, nearest, no_bleed, key, key_region, key_cut, key_spread, trim_density
                            As the convert options (`codex-img convert --help`):
                            true for a flag, a number or string for a value, a
                            list for several keys
  max                       "WxH" or [W, H]: resize to fit, never enlarging

Each generated raw image gets a manifest beside it (<key>.png.json): the prompt sent,
presets, inputs and what the backend reported. An asset can have a comment string;
it is ignored by generation, conversion and the changed check. When the spec has changed since, the
asset's skip line says so; use --reroll to preserve the old version.

Example:
  codex-img batch art/assets.json tracks/city --dry-run"#
}

pub fn parse(args: &[String]) -> Result<Option<BatchOptions>> {
    let mut positionals = Vec::new();
    let mut opts = BatchOptions { spec: String::new(), filters: Vec::new(), generate: true, convert: true, jobs: DEFAULT_JOBS, dry_run: false, json: false, quiet: false, reroll: vec![], restore: None, inspect: false, expect_images: None, no_wait: false };
    let (mut generate_only, mut convert_only) = (false, false);
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            positionals.extend(iter.by_ref().cloned());
            break;
        }
        if !arg.starts_with('-') {
            positionals.push(arg.clone());
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if arg.starts_with("--") => (n, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        match name {
            "-j" | "--jobs" => {
                let value = inline.or_else(|| iter.next().cloned()).ok_or_else(|| Error::usage(format!("{name} needs a value.")))?;
                opts.jobs = value
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=MAX_JOBS).contains(n))
                    .ok_or_else(|| Error::usage(format!("--jobs must be an integer from 1 to {MAX_JOBS}.")))?;
            }
            "--reroll" => {
                opts.reroll.push(inline.or_else(|| iter.next().cloned()).ok_or_else(|| Error::usage("--reroll needs a key."))?);
                while iter.peek().is_some_and(|arg| !arg.starts_with('-')) { opts.reroll.push(iter.next().unwrap().clone()); }
            }
            "--restore" => {
                if opts.restore.is_some() { return Err(Error::usage("Choose one --restore.")); }
                let key = inline.or_else(|| iter.next().cloned()).ok_or_else(|| Error::usage("--restore needs a key and a version file."))?;
                let path = iter.next().cloned().ok_or_else(|| Error::usage("--restore needs a version file."))?;
                opts.restore = Some((key, path));
            }
            "--expect-images" => {
                let value = inline.or_else(|| iter.next().cloned()).ok_or_else(|| Error::usage("--expect-images needs a count."))?;
                opts.expect_images = Some(value.parse().map_err(|_| Error::usage("--expect-images must be a nonnegative integer."))?);
            }
            "--generate-only" => generate_only = true,
            "--convert-only" => convert_only = true,
            "--no-wait" if inline.is_none() => opts.no_wait = true,
            "--inspect" => opts.inspect = true,
            "--dry-run" => opts.dry_run = true,
            "--json" => opts.json = true,
            "--quiet" => opts.quiet = true,
            "-h" | "--help" => return Ok(None),
            _ => return Err(Error::usage(format!("Unknown batch option: {arg}"))),
        }
    }
    if generate_only && convert_only {
        return Err(Error::usage("--generate-only and --convert-only can't be combined."));
    }
    (opts.generate, opts.convert) = (!convert_only, !generate_only);
    let mut positionals = positionals.into_iter();
    opts.spec = positionals.next().ok_or_else(|| Error::usage("batch needs a spec file, e.g. codex-img batch art/assets.json."))?;
    opts.filters = positionals.collect();
    if (!opts.reroll.is_empty() || opts.restore.is_some()) && (!opts.filters.is_empty() || opts.restore.is_some() && !opts.reroll.is_empty() || convert_only && !opts.reroll.is_empty() || opts.inspect) { return Err(Error::usage("Re-roll and restore take exact keys and cannot be combined with filters, inspection or each other. Re-roll needs generation.")); }
    if opts.expect_images.is_some() && (!opts.generate || opts.restore.is_some() || opts.inspect) { return Err(Error::usage("--expect-images only applies to batch generation.")); }
    Ok(Some(opts))
}

/// One asset of the spec, validated.
#[derive(Debug, Clone)]
struct Asset {
    key: String,
    /// The prompt as sent: composed from the asset's own, its presets and the spec's style.
    prompt: String,
    /// The asset's own prompt, for the manifest.
    user_prompt: String,
    size: Option<String>,
    /// Already applied to `prompt`; kept to check the result's shape.
    aspect: Option<Aspect>,
    /// Reference images from presets and *_ref fields, sent after `references` and `images`.
    role_images: Vec<RoleImage>,
    used: Vec<(presets::Kind, String, Source)>,
    quality: Option<String>,
    background: Option<String>,
    /// Indices of the assets whose raw images are passed as references.
    references: Vec<usize>,
    images: Vec<PathBuf>,
    publish: bool,
    format: Format,
    encoding: Encoding,
    transform: Transform,
    replacement: Option<std::sync::Arc<crate::history::Replacement>>,
    input_override: Option<Vec<manifest::Input>>,
    setup: Value,
    from_comment: Option<String>,
    transport: Transport,
    model: Option<String>,
}

#[derive(Debug, Clone)]
struct Spec {
    raw_dir: PathBuf,
    out_dir: PathBuf,
    assets: Vec<Asset>,
}

impl Spec {
    fn raw_path(&self, asset: &Asset) -> PathBuf {
        self.raw_dir.join(format!("{}.png", asset.key))
    }

    fn out_path(&self, asset: &Asset) -> PathBuf {
        self.out_dir.join(format!("{}.{}", asset.key, asset.format.extension()))
    }

    /// The images an asset's request sends, in order: references, images, then role images.
    fn inputs(&self, asset: &Asset) -> Vec<manifest::Input> {
        if let Some(inputs) = &asset.input_override { return inputs.clone(); }
        let plain = asset.references.iter().map(|&r| self.raw_path(&self.assets[r])).chain(asset.images.iter().cloned());
        let mut inputs: Vec<manifest::Input> = plain.map(|path| manifest::Input { path, role: "input", character: None, fingerprint: None }).collect();
        inputs.extend(asset.role_images.iter().map(|i| manifest::Input { path: i.path.clone(), role: i.role.name(), character: i.character.clone(), fingerprint: None }));
        inputs
    }

    /// Whether the raw image's manifest records another prompt, other request settings or other
    /// inputs than the spec asks for now. Without a manifest, nothing is known, so no.
    fn changed(&self, asset: &Asset) -> bool { self.raw_state(asset).0 }

    fn raw_state(&self, asset: &Asset) -> (bool, bool) {
        let paths: Vec<PathBuf> = self.inputs(asset).into_iter().map(|i| i.path).collect();
        let request = manifest::request(asset.aspect, asset.size.as_deref(), asset.quality.as_deref(), asset.background.as_deref());
        let mut image = resolved(&self.raw_path(asset));
        let mut seen = std::collections::HashSet::new();
        let mut edited = false;
        loop {
            if !seen.insert(image.clone()) { return (false, edited); }
            let Some(note) = manifest::read(&manifest::path_for(&image)) else { return (false, edited); };
            let refined = note.get("fromComment").is_some() || note["refined"] == true
                || note["userPrompt"].as_str().or_else(|| note["prompt"].as_str()).is_some_and(|prompt| prompt.starts_with("Change only:"));
            if !refined { return (manifest::changed(&note, &asset.prompt, &request, &paths), edited); }
            edited = true;
            let Some(parent) = note["parent"].as_str() else { return (false, true); };
            let base = note["root"].as_str().map(PathBuf::from).or_else(|| crate::project::root(image.parent().unwrap())).unwrap_or_else(|| image.parent().unwrap().to_path_buf());
            image = resolved(&base.join(parent));
        }
    }

}

pub const TOP_FIELDS: [&str; 10] = ["$schema", "raw_dir", "out_dir", "style", "defaults", "assets", "views", "styles", "characters", "palettes"];
pub const ASSET_FIELDS: [&str; 35] = [
    "comment", "prompt", "size", "aspect", "view", "style", "character", "style_ref", "character_ref", "composition_ref", "palette", "palette_clean", "quality", "background", "reference", "images", "publish", "format", "colors", "dither", "output_quality", "lossless", "trim",
    "hard_alpha", "resize", "max", "fit", "no_enlarge", "nearest", "no_bleed", "key", "key_region", "key_cut", "key_spread", "trim_density",
];

pub fn review_raw(path: &Path, value: &Value, key: &str) -> Result<PathBuf> {
    check_key(key)?;
    let raw = value.get("raw_dir").map(|v| v.as_str().ok_or_else(|| Error::usage("raw_dir must be a string."))).transpose()?.unwrap_or(DEFAULT_RAW_DIR);
    Ok(path.parent().unwrap_or(Path::new(".")).join(raw).join(format!("{key}.png")))
}

fn load_spec(path: &Path) -> Result<Spec> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::usage(format!("Unable to read {}: {e}", path.display())))?;
    let value: Value = serde_json::from_str(&text).map_err(|e| Error::usage(format!("{} is not valid JSON: {e}", path.display())))?;
    let base = path.parent().unwrap_or(Path::new(""));
    let files = PresetFiles { project: presets::find_project(base), global: presets::global_dir().map(|d| d.join("presets.json")) };
    parse_spec(&value, base, &files).map_err(|e| Error::usage(format!("{}: {}", path.display(), e.message)))
}

/// The project and global preset files, read only when the spec names a preset.
#[derive(Default)]
struct PresetFiles {
    project: Option<PathBuf>,
    global: Option<PathBuf>,
}

fn parse_spec(value: &Value, base: &Path, files: &PresetFiles) -> Result<Spec> {
    let top = value.as_object().ok_or_else(|| Error::usage("the spec must be a JSON object."))?;
    unknown_fields(top, &TOP_FIELDS, "the spec")?;
    crate::check::schema_field(top)?;
    let dir = |name: &str, default: &str| -> Result<PathBuf> {
        match top.get(name) {
            None => Ok(base.join(default)),
            Some(Value::String(dir)) => Ok(base.join(dir)),
            Some(_) => Err(Error::usage(format!("{name} must be a string."))),
        }
    };
    let (raw_dir, out_dir) = (dir("raw_dir", DEFAULT_RAW_DIR)?, dir("out_dir", DEFAULT_OUT_DIR)?);
    let style = match top.get("style") {
        None | Some(Value::Null) => None,
        Some(Value::String(style)) => Some(style.as_str()),
        Some(_) => return Err(Error::usage("style must be a string.")),
    };
    let empty = Map::new();
    let defaults = match top.get("defaults") {
        None => &empty,
        Some(Value::Object(defaults)) => defaults,
        Some(_) => return Err(Error::usage("defaults must be an object.")),
    };
    unknown_fields(defaults, &ASSET_FIELDS, "defaults")?;
    if defaults.contains_key("prompt") || defaults.contains_key("reference") || defaults.contains_key("comment") {
        return Err(Error::usage("defaults can't set prompt, reference or comment."));
    }
    let entries = top.get("assets").and_then(Value::as_object).ok_or_else(|| Error::usage("assets must be an object of key -> asset."))?;
    let names = |object: &Map<String, Value>| ["view", "style", "character", "palette"].iter().any(|f| object.get(*f).is_some_and(|v| !v.is_null() && *v != Value::Bool(false)));
    let named = names(defaults) || entries.values().filter_map(Value::as_object).any(names);
    let library = Library::load(Some((top, base)), files.project.as_deref().filter(|_| named), files.global.as_deref().filter(|_| named))?;
    let keys: Vec<&str> = entries.keys().map(String::as_str).collect();
    let mut assets = Vec::with_capacity(entries.len());
    for (key, entry) in entries {
        let context = |e: Error| Error::usage(format!("assets.\"{key}\": {}", e.message));
        check_key(key).map_err(context)?;
        let own = entry.as_object().ok_or_else(|| context(Error::usage("must be an object.")))?;
        unknown_fields(own, &ASSET_FIELDS, "the asset").map_err(context)?;
        if own.get("comment").is_some_and(|comment| !comment.is_string()) { return Err(context(Error::usage("comment must be a string."))); }
        let fields = Fields { own, defaults };
        assets.push(parse_asset(key, &fields, style, &keys, base, &library).map_err(context)?);
    }
    check_cycles(&assets)?;
    Ok(Spec { raw_dir, out_dir, assets })
}

pub fn check_issues(value: &Value, path: &Path) -> Vec<Value> {
    let base = path.parent().unwrap_or(Path::new("."));
    let files = PresetFiles { project: presets::find_project(base), global: presets::global_dir().map(|d| d.join("presets.json")) };
    let mut issues = crate::check::presets_issues(value, base, true);
    let Some(top) = value.as_object() else { return issues; };
    for key in top.keys().filter(|key| !TOP_FIELDS.contains(&key.as_str())) { issues.push(crate::check::issue(key, Error::usage("Unknown field in the spec."))); }
    let Some(entries) = top.get("assets").and_then(Value::as_object) else { issues.push(crate::check::issue("assets", Error::usage("Must be an object of key -> asset."))); return issues; };
    // Check each asset with the same complete parser, keeping the other keys as valid
    // placeholders so references still resolve. One broken asset must not hide the next.
    for (key, entry) in entries {
        if let Some(own) = entry.as_object() {
            for field in own.keys().filter(|field| !ASSET_FIELDS.contains(&field.as_str())) {
                issues.push(crate::check::issue(format!("assets.{key}.{field}"), Error::usage("Unknown asset field.")));
            }
        }
        let mut one = value.clone();
        for (other, asset) in one["assets"].as_object_mut().unwrap() {
            if other != key { *asset = json!({"prompt":"Validation placeholder"}); }
        }
        if let Err(error) = parse_spec(&one, base, &files) {
            let context = format!("assets.\"{key}\": ");
            if error.message.starts_with(&context) { issues.push(crate::check::issue(format!("assets.{key}"), error)); }
        }
    }
    if let Err(error) = parse_spec(value, base, &files) {
        if !issues.iter().any(|issue| issue["message"] == error.message) && !error.message.starts_with("assets.\"") {
            issues.push(crate::check::issue("$", error));
        }
    }
    issues
}

fn unknown_fields(object: &Map<String, Value>, known: &[&str], what: &str) -> Result<()> {
    match object.keys().find(|k| !known.contains(&k.as_str())) {
        Some(field) => Err(Error::usage(format!("unknown field \"{field}\" in {what}; known fields: {}.", known.join(", ")))),
        None => Ok(()),
    }
}

/// Keys become file paths under raw_dir and out_dir, so they must stay inside them.
fn check_key(key: &str) -> Result<()> {
    let bad = key.is_empty()
        || key.starts_with('/')
        || key.ends_with('/')
        || key.contains('\\')
        || key.split('/').any(|part| part.is_empty() || part == "." || part == "..");
    if bad {
        return Err(Error::usage("the key must be a relative path like tracks/city/lamp (no leading /, no . or .. parts)."));
    }
    Ok(())
}

/// An asset's fields over the spec's defaults: the asset's own value wins, and its own null or
/// false turns a default off.
struct Fields<'a> {
    own: &'a Map<String, Value>,
    defaults: &'a Map<String, Value>,
}

impl Fields<'_> {
    fn get(&self, name: &str) -> Option<&Value> {
        let value = self.own.get(name).or_else(|| self.defaults.get(name))?;
        (!matches!(value, Value::Null | Value::Bool(false))).then_some(value)
    }

    fn string(&self, name: &str) -> Result<Option<String>> {
        match self.get(name) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(Error::usage(format!("{name} must be a string."))),
        }
    }

    fn strings(&self, name: &str) -> Result<Vec<String>> {
        let wrong = || Error::usage(format!("{name} must be a string or a list of strings."));
        match self.get(name) {
            None => Ok(Vec::new()),
            Some(Value::String(s)) => Ok(vec![s.clone()]),
            Some(Value::Array(items)) => items.iter().map(|v| v.as_str().map(str::to_string).ok_or_else(wrong)).collect(),
            Some(_) => Err(wrong()),
        }
    }

    fn flag(&self, name: &str) -> Result<bool> {
        match self.get(name) {
            None => Ok(false),
            Some(Value::Bool(true)) => Ok(true),
            Some(_) => Err(Error::usage(format!("{name} must be true or false."))),
        }
    }

    /// A value given as a number or a string, as text for the CLI parsers.
    fn text(&self, name: &str) -> Result<Option<String>> {
        match self.get(name) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(Value::Number(n)) => Ok(Some(n.to_string())),
            Some(_) => Err(Error::usage(format!("{name} must be a number or a string."))),
        }
    }

    /// Like `text`, but true stands for the option without a value (`--trim`, `--hard-alpha`):
    /// None when off, Some(None) for true, Some(Some(value)) otherwise.
    fn flag_or_text(&self, name: &str) -> Result<Option<Option<String>>> {
        match self.get(name) {
            Some(Value::Bool(true)) => Ok(Some(None)),
            _ => Ok(self.text(name)?.map(Some)),
        }
    }
}

fn parse_asset(key: &str, fields: &Fields, style: Option<&str>, keys: &[&str], base: &Path, library: &Library) -> Result<Asset> {
    let user_prompt = fields.string("prompt")?.filter(|p| !p.trim().is_empty()).ok_or_else(|| Error::usage("prompt is required."))?;
    let size = fields.string("size")?;
    if size.as_deref().is_some_and(|s| !cli::is_size(s)) {
        return Err(Error::usage("size must be WIDTHxHEIGHT or auto."));
    }
    let aspect = fields.string("aspect")?.map(|v| Aspect::parse(&v).map_err(|e| Error::usage(e.message.replace("--aspect", "aspect")))).transpose()?;
    if aspect.is_some() && size.as_deref().is_some_and(|s| s != "auto") {
        return Err(Error::usage("aspect and size can't be combined; the backend ignores size, and aspect sets the frame."));
    }
    let quality = cli::one_of("quality", fields.string("quality")?, &["low", "medium", "high", "auto"])?;
    let background = cli::one_of("background", fields.string("background")?, &["transparent", "opaque", "auto"])?;
    let mut references = Vec::new();
    for reference in fields.strings("reference")? {
        match keys.iter().position(|k| *k == reference) {
            Some(index) if reference != key => references.push(index),
            Some(_) => return Err(Error::usage("an asset can't reference itself.")),
            None => return Err(Error::usage(format!("reference \"{reference}\" is not an asset in this spec."))),
        }
    }
    let images: Vec<PathBuf> = fields.strings("images")?.iter().map(|p| base.join(p)).collect();
    if references.len() + images.len() > MAX_EDIT_IMAGES {
        return Err(Error::usage(format!("at most {MAX_EDIT_IMAGES} references and images together.")));
    }
    let refs = |name: &str| -> Result<Vec<PathBuf>> { Ok(fields.strings(name)?.iter().map(|p| base.join(p)).collect()) };
    let setup = Setup {
        aspect,
        view: fields.string("view")?,
        styles: fields.strings("style")?,
        characters: fields.strings("character")?,
        style_refs: refs("style_ref")?,
        character_refs: refs("character_ref")?,
        composition_refs: refs("composition_ref")?,
        palette: match fields.strings("palette")?.join(" ") {
            spec if spec.is_empty() => None,
            spec => Some(crate::palette::resolve(&spec, base, || Ok(library.clone()))?),
        },
    };
    let palette_clean = fields.flag("palette_clean")?;
    if palette_clean && setup.palette.is_none() {
        return Err(Error::usage("palette_clean only applies with a palette."));
    }
    let saved_setup = json!({"view":setup.view,"styles":setup.styles,"characters":setup.characters,"palette": (!fields.strings("palette")?.is_empty()).then(|| fields.strings("palette").unwrap().join(" ")),"inputs": images.iter().map(|path| json!({"path":path,"role":"input"})).chain(setup.style_refs.iter().map(|path| json!({"path":path,"role":"style"}))).chain(setup.character_refs.iter().map(|path| json!({"path":path,"role":"character"}))).chain(setup.composition_refs.iter().map(|path| json!({"path":path,"role":"composition"}))).collect::<Vec<_>>()});
    let composed = presets::compose(&user_prompt, style, &setup, library, references.len() + images.len()).map_err(|e| {
        let hint = if e.message.starts_with("Unknown style") { " For text that ends every prompt, use the spec's top-level style." } else { "" };
        Error::usage(format!("{}{hint}", e.message))
    })?;
    let prompt = composed.prompt;
    let palette = setup.palette.clone().map(|colors| transform::PaletteFit { colors, clean: palette_clean });
    if prompt.chars().count() > cli::MAX_PROMPT_CHARS {
        return Err(Error::usage("the prompt (with the style, presets and aspect) is longer than 32,000 characters."));
    }
    let publish = match fields.own.get("publish").or_else(|| fields.defaults.get("publish")) {
        None | Some(Value::Bool(true)) => true,
        Some(Value::Bool(false)) => false,
        Some(_) => return Err(Error::usage("publish must be true or false.")),
    };
    let format = match fields.string("format")? {
        Some(f) => Format::parse(&f).ok_or_else(|| Error::usage("format must be one of: png, jpeg, webp."))?,
        None => Format::Png,
    };
    let encoding = Encoding {
        colors: fields.text("colors")?.map(|v| cli::parse_colors(&v)).transpose()?,
        dither: fields.flag("dither")?,
        quality: fields.text("output_quality")?.map(|v| cli::parse_output_quality(&v)).transpose()?,
        lossless: fields.flag("lossless")?,
    };
    encoding.check(Some(format))?;
    let mut resize = fields.string("resize")?.map(|v| Resize::parse(&v)).transpose()?;
    let mut no_enlarge = fields.flag("no_enlarge")?;
    if let Some(max) = fields.get("max") {
        if resize.is_some() {
            return Err(Error::usage("max and resize can't be combined; max is resize with no_enlarge."));
        }
        let text = match max {
            Value::String(s) => s.clone(),
            Value::Array(sides) if sides.len() == 2 && sides.iter().all(Value::is_u64) => format!("{}x{}", sides[0], sides[1]),
            _ => return Err(Error::usage("max must be \"WxH\" or [W, H].")),
        };
        resize = Some(Resize::parse(&text)?);
        no_enlarge = true;
    }
    let transform = Transform {
        hard_alpha: fields.flag_or_text("hard_alpha")?.map(|v| v.map_or(Ok(transform::FAINT_ALPHA), |t| transform::parse_hard_alpha(&t))).transpose()?,
        keys: fields.strings("key")?.iter().map(|k| Key::parse(k)).collect::<codex_img_core::error::Result<_>>()?,
        key_region: fields.string("key_region")?.map(|v| Region::parse(&v)).transpose()?,
        key_spread: fields.text("key_spread")?.map(|v| transform::parse_key_spread(&v)).transpose()?,
        key_cut: fields.flag_or_text("key_cut")?.map(|v| v.map_or(Ok(transform::KEY_CUT), |t| transform::parse_key_cut(&t))).transpose()?,
        trim: fields.flag_or_text("trim")?.map(|v| v.map_or(Ok(0), |t| transform::parse_trim_padding(&t))).transpose()?,
        trim_density: fields.text("trim_density")?.map(|v| Density::parse(&v)).transpose()?,
        resize,
        fit: fields.string("fit")?.map(|v| Fit::parse(&v)).transpose()?,
        no_bleed: fields.flag("no_bleed")?,
        no_enlarge,
        nearest: fields.flag("nearest")?,
        palette,
    };
    transform.check_output(Some(format), &encoding)?;
    transform.check()?;
    Ok(Asset { key: key.to_string(), prompt, user_prompt, size, aspect, role_images: composed.images, used: composed.used, quality, background, references, images, publish, format, encoding, transform, replacement: None, input_override: None, setup: saved_setup, from_comment: None, transport: Transport::Direct, model: None })
}

/// References must form no loop, or no asset in it could be generated first.
fn check_cycles(assets: &[Asset]) -> Result<()> {
    // 0 unvisited, 1 on the current path, 2 done.
    fn visit(i: usize, assets: &[Asset], state: &mut [u8], path: &mut Vec<usize>) -> Result<()> {
        match state[i] {
            2 => return Ok(()),
            1 => {
                let start = path.iter().position(|&p| p == i).unwrap_or(0);
                let names: Vec<&str> = path[start..].iter().chain([&i]).map(|&p| assets[p].key.as_str()).collect();
                return Err(Error::usage(format!("references form a loop: {}.", names.join(" -> "))));
            }
            _ => {}
        }
        state[i] = 1;
        path.push(i);
        for &r in &assets[i].references {
            visit(r, assets, state, path)?;
        }
        path.pop();
        state[i] = 2;
        Ok(())
    }
    let mut state = vec![0u8; assets.len()];
    for i in 0..assets.len() {
        visit(i, assets, &mut state, &mut Vec::new())?;
    }
    Ok(())
}

/// Indices of the assets the filters select, in spec order. Every filter must match something.
fn select(spec: &Spec, filters: &[String]) -> Result<Vec<usize>> {
    let matches = |key: &str, filter: &str| {
        let filter = filter.trim_end_matches('/');
        key == filter || key.strip_prefix(filter).is_some_and(|rest| rest.starts_with('/'))
    };
    if let Some(filter) = filters.iter().find(|f| !spec.assets.iter().any(|a| matches(&a.key, f))) {
        return Err(Error::usage(format!("No asset matches \"{filter}\" (an exact key or a folder of keys).")));
    }
    Ok((0..spec.assets.len()).filter(|&i| filters.is_empty() || filters.iter().any(|f| matches(&spec.assets[i].key, f))).collect())
}

/// The assets to generate, references before the assets that use them: every selected asset
/// without a raw image, and any missing raw image one of those references.
fn generation_plan(spec: &Spec, selected: &[usize]) -> Vec<usize> {
    fn add(i: usize, spec: &Spec, plan: &mut Vec<usize>) {
        if plan.contains(&i) || spec.raw_path(&spec.assets[i]).exists() && spec.assets[i].replacement.is_none() {
            return;
        }
        for &r in &spec.assets[i].references {
            add(r, spec, plan);
        }
        plan.push(i);
    }
    let mut plan = Vec::new();
    for &i in selected {
        add(i, spec, &mut plan);
    }
    plan
}

enum Status {
    Ok,
    Skipped,
    Unchanged,
    Planned,
    Failed(Error),
}

#[derive(Default)]
struct Tally {
    generated: usize,
    kept: usize,
    converted: usize,
    unchanged: usize,
    failed: usize,
    exit_code: i32,
    /// Keys whose generation failed in this run; their conversion is skipped.
    failed_generate: Vec<String>,
}

/// Prints one line (or JSON object) per asset and step as they finish, and counts them.
struct Report {
    json: bool,
    quiet: bool,
    tally: Mutex<Tally>,
}

impl Report {
    fn line(&self, step: &str, key: &str, status: Status, detail: &str, mut info: Value) {
        let mut tally = self.tally.lock().unwrap_or_else(|e| e.into_inner());
        let (word, name) = match &status {
            Status::Ok => ("ok", "ok"),
            Status::Skipped => ("skip", "skipped"),
            Status::Unchanged => ("same", "unchanged"),
            Status::Planned => ("plan", "planned"),
            Status::Failed(_) => ("FAILED", "failed"),
        };
        match (&status, step) {
            (Status::Ok, "generate") => tally.generated += 1,
            (Status::Skipped, _) => tally.kept += 1,
            (Status::Ok, _) => tally.converted += 1,
            (Status::Unchanged, _) => tally.unchanged += 1,
            (Status::Failed(error), _) => {
                tally.failed += 1;
                tally.exit_code = tally.exit_code.max(error.kind.exit_code());
                if step == "generate" { tally.failed_generate.push(key.to_string()); }
            }
            _ => {}
        }
        let failed = matches!(status, Status::Failed(_));
        if self.quiet && !failed {
            return;
        }
        if self.json {
            info["key"] = json!(key);
            info["step"] = json!(step);
            info["status"] = json!(name);
            if let Status::Failed(error) = &status {
                info["error"] = json!(error.message);
            }
            println!("{info}");
        } else if let Status::Failed(error) = &status {
            println!("{word:<6} {step:<8} {key}: {error}");
        } else {
            println!("{word:<6} {step:<8} {key}{detail}");
        }
    }
}

/// Runs `work` for items `0..count`, `jobs` at a time. An item starts only once the items in its
/// `waits` have finished. If one of them failed, the item goes to `skip` with that one instead;
/// after `work` asks to stop (`Err(true)`: no login or no quota left), every item still waiting
/// goes to `skip` with None.
fn run_pool(
    count: usize,
    waits: &[Vec<usize>],
    jobs: usize,
    work: &(dyn Fn(usize) -> std::result::Result<(), bool> + Sync),
    skip: &(dyn Fn(usize, Option<usize>) + Sync),
) {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Waiting,
        Running,
        Done(bool),
    }
    struct Shared {
        states: Vec<State>,
        stop: bool,
    }
    let shared = Mutex::new(Shared { states: vec![State::Waiting; count], stop: false });
    let changed = Condvar::new();
    std::thread::scope(|scope| {
        for _ in 0..jobs.min(count) {
            scope.spawn(|| loop {
                let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
                let next = loop {
                    let ready = (0..count).find(|&i| {
                        guard.states[i] == State::Waiting && waits[i].iter().all(|&w| matches!(guard.states[w], State::Done(_)))
                    });
                    match ready {
                        Some(i) => break Some(i),
                        None if guard.states.contains(&State::Waiting) => guard = changed.wait(guard).unwrap_or_else(|e| e.into_inner()),
                        None => break None,
                    }
                };
                let Some(i) = next else { return };
                let failed_wait = waits[i].iter().copied().find(|&w| guard.states[w] == State::Done(false));
                let stop = guard.stop;
                guard.states[i] = State::Running;
                drop(guard);
                let ok = if let Some(w) = failed_wait {
                    skip(i, Some(w));
                    false
                } else if stop {
                    skip(i, None);
                    false
                } else {
                    match work(i) {
                        Ok(()) => true,
                        Err(fatal) => {
                            if fatal {
                                shared.lock().unwrap_or_else(|e| e.into_inner()).stop = true;
                            }
                            false
                        }
                    }
                };
                shared.lock().unwrap_or_else(|e| e.into_inner()).states[i] = State::Done(ok);
                changed.notify_all();
            });
        }
    });
}

pub fn run(opts: &BatchOptions) -> Result<i32> {
    let spec = load_spec(Path::new(&opts.spec))?;
    if opts.inspect { return inspect(opts, &spec); }
    execute(opts, &spec, &Backend::default(), &auth::load_credentials)
}

fn inspect(opts: &BatchOptions, spec: &Spec) -> Result<i32> {
    let value = crate::review::read(Path::new(&opts.spec))?;
    for i in select(spec, &opts.filters)? {
        let asset = &spec.assets[i]; let raw = spec.raw_path(asset); let path = spec.out_path(asset);
        let note_path = manifest::path_for(&raw); let note = manifest::read(&note_path);
        let folder = crate::history::folder(Path::new(&opts.spec), &asset.key);
        let mut history = vec![];
        if folder.is_dir() {
            for entry in std::fs::read_dir(folder).map_err(|e| Error::other(e.to_string()))? {
                let entry = entry.map_err(|e| Error::other(e.to_string()))?;
                if entry.file_type().is_ok_and(|kind| kind.is_file()) && entry.path().extension().is_some_and(|ext| ext == "png") && !entry.file_name().to_string_lossy().ends_with(".pending.png") {
                    history.push(json!({"path":events::absolute(&entry.path()),"manifestPath":events::absolute(&manifest::path_for(&entry.path()))}));
                }
            }
            history.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
        }
        let info = json!({"key":asset.key,"prompt":asset.user_prompt,"rawPath":events::absolute(&raw),"path":events::absolute(&path),"manifestPath":events::absolute(&note_path),"generated":raw.is_file(),"converted":path.is_file(),"changed":raw.is_file() && spec.changed(asset),"edited":spec.raw_state(asset).1,"comment":value["assets"][&asset.key]["comment"].as_str().unwrap_or(""),"star":note.as_ref().is_some_and(|note| note["star"] == true),"publish":asset.publish,"palette":asset.setup["palette"],"conversion":crate::conversion_record::build(asset.format, &asset.encoding, &asset.transform),"history":history});
        if opts.json { println!("{info}"); } else { println!("{}: {}{}", asset.key, if raw.is_file() { "generated" } else { "missing" }, if spec.raw_state(asset).1 { ", edited" } else if spec.changed(asset) { ", changed" } else { "" }); }
    }
    Ok(0)
}

fn restore(opts: &BatchOptions, spec: &Spec) -> Result<i32> {
    let (key, source) = opts.restore.as_ref().unwrap();
    let asset = spec.assets.iter().find(|asset| &asset.key == key).ok_or_else(|| Error::usage(format!("No asset named {key}.")))?;
    check_outputs(spec)?;
    let _lock = if opts.dry_run || !Path::new(&opts.spec).is_file() { None } else { Some(crate::history::lock(Path::new(&opts.spec), opts.no_wait)?) };
    if manifest::read(&manifest::path_for(&spec.raw_path(asset))).is_some_and(|note| note["comment"].as_str().is_some_and(|comment| !comment.trim().is_empty())) {
        return Err(Error::usage("The current raw has a manifest comment. Clear or move the comment first, before restoring."));
    }
    let replacement = crate::history::Replacement::plan(&spec.raw_path(asset), Path::new(&opts.spec), key)?;
    let source = resolved(Path::new(source));
    if source == replacement.raw { return Err(Error::usage("That version is already the active raw image.")); }
    let (bytes, _) = convert::read_image(&source)?;
    images::decode(&bytes)?;
    let png = images::convert(&bytes, Format::Png, &Encoding::default())?;
    let source_note = manifest::path_for(&source);
    let mut note = if source_note.exists() { crate::review::read(&source_note)? } else { json!({}) };
    note.as_object_mut().unwrap().remove("comment");
    note.as_object_mut().unwrap().remove("star");
    if opts.dry_run { println!("{}", json!({"step":"restore","key":key,"status":"planned","rawPath":shown(&replacement.raw),"parent":shown(&source),"historyPath":replacement.previous})); return Ok(0); }
    let run = events::Run::start("batch", 1, json!({"spec":events::absolute(Path::new(&opts.spec)),"generation":false}));
    let job = run.job(restore_start(&note, asset, key, &source, &replacement.raw));
    let base = note["root"].as_str().map(PathBuf::from).or_else(|| crate::project::root(source.parent().unwrap())).unwrap_or_else(|| source.parent().unwrap().to_path_buf());
    for group in ["inputs", "setup"] {
        let inputs = if group == "inputs" { note.get_mut("inputs").and_then(Value::as_array_mut) } else { note.pointer_mut("/setup/inputs").and_then(Value::as_array_mut) };
        if let Some(inputs) = inputs { for input in inputs { if let Some(path) = input["path"].as_str() { input["path"] = json!(run.manifest_path(&resolved(&base.join(path)))); } } }
    }
    note["parent"] = json!(run.manifest_path(&source));
    if let Some(root) = run.manifest_root(&replacement.raw) { note["root"] = json!(root); } else { note.as_object_mut().unwrap().remove("root"); }
    let result = replacement.commit(&png, &note);
    if let Err(error) = &result { job.failed(error); run.end(Some(error)); return Err(Error::other(error.message.clone())); }
    job.done(json!({"path":events::absolute(&replacement.raw),"manifestPath":events::absolute(&manifest::path_for(&replacement.raw)),"historyPath":replacement.previous.as_deref().map(events::absolute),"durationMs":0}));
    run.end(None);
    if opts.json { println!("{}", json!({"step":"restore","key":key,"status":"ok","rawPath":shown(&replacement.raw),"parent":shown(&source),"historyPath":replacement.previous})); }
    else { println!("Restored {key} from {}", source.display()); }
    if opts.convert && asset.publish { convert_asset(spec, asset)?; }
    Ok(0)
}

/// `job.started` for a restore. The event contract needs a prompt and a request object, which a
/// source without a manifest (any image file) doesn't have: fall back to the asset's prompt and `{}`.
fn restore_start(note: &Value, asset: &Asset, key: &str, source: &Path, output: &Path) -> Value {
    let prompt = note["userPrompt"].as_str().or_else(|| note["prompt"].as_str()).unwrap_or(&asset.user_prompt);
    let request = if note["request"].is_object() { note["request"].clone() } else { json!({}) };
    json!({"key":key,"prompt":prompt,"inputs":[],"request":request,"parent":events::absolute(source),"output":events::absolute(output)})
}

fn execute(opts: &BatchOptions, spec: &Spec, backend: &Backend, credentials: &dyn Fn() -> Result<Credentials>) -> Result<i32> {
    if opts.restore.is_some() { return restore(opts, spec); }
    let _lock = if opts.dry_run || !opts.generate || !Path::new(&opts.spec).is_file() { None } else { Some(crate::history::lock(Path::new(&opts.spec), opts.no_wait)?) };
    execute_unlocked(opts, spec, backend, credentials)
}

fn execute_unlocked(opts: &BatchOptions, spec: &Spec, backend: &Backend, credentials: &dyn Fn() -> Result<Credentials>) -> Result<i32> {
    let mut prepared = spec.clone();
    let selected = if opts.reroll.is_empty() { select(spec, &opts.filters)? } else {
        let mut selected = vec![];
        for key in &opts.reroll {
            let i = spec.assets.iter().position(|asset| &asset.key == key).ok_or_else(|| Error::usage(format!("No asset named {key}. Re-roll takes exact keys.")))?;
            if !selected.contains(&i) { selected.push(i); }
            prepared.assets[i].replacement = Some(std::sync::Arc::new(crate::history::Replacement::plan(&spec.raw_path(&spec.assets[i]), Path::new(&opts.spec), key)?));
        }
        selected
    };
    let spec = &prepared;
    if opts.convert {
        check_outputs(spec)?;
    }
    let report = Report { json: opts.json, quiet: opts.quiet, tally: Mutex::new(Tally::default()) };
    if opts.generate {
        let plan = generation_plan(spec, &selected);
        if let Some(expected) = opts.expect_images.filter(|expected| *expected != plan.len()) {
            return Err(Error::usage(format!("The plan now needs {} image{}, expected {expected}. Review a new dry-run plan. Stopped before quota.", plan.len(), if plan.len() == 1 { "" } else { "s" })));
        }
        for &i in selected.iter().filter(|i| !plan.contains(i)) {
            let raw = spec.raw_path(&spec.assets[i]);
            if spec.changed(&spec.assets[i]) {
                let detail = " (raw image exists; note: changed since its raw image was generated, use --reroll to keep history)";
                report.line("generate", &spec.assets[i].key, Status::Skipped, detail, json!({"rawPath": shown(&raw), "changed": true}));
            } else {
                report.line("generate", &spec.assets[i].key, Status::Skipped, " (raw image exists)", json!({"rawPath": shown(&raw)}));
            }
        }
        if opts.dry_run {
            for &i in &plan {
                let raw = spec.raw_path(&spec.assets[i]);
                report.line("generate", &spec.assets[i].key, Status::Planned, &format!(" -> {}", shown(&raw)), json!({"rawPath": shown(&raw)}));
            }
        } else if !plan.is_empty() {
            let run = events::Run::start("batch", plan.len(), json!({"spec": events::absolute(Path::new(&opts.spec))}));
            let credentials = credentials().inspect_err(|error| run.end(Some(error)))?;
            generate(spec, &plan, opts.jobs, backend, &credentials, &report, &run);
            run.end(None);
        }
    }
    if opts.convert {
        // An asset whose generation just failed has no new raw to convert; converting would only add a second failure.
        let failed = report.tally.lock().unwrap_or_else(|e| e.into_inner()).failed_generate.clone();
        let publish: Vec<usize> = selected.into_iter().filter(|&i| spec.assets[i].publish && !failed.contains(&spec.assets[i].key)).collect();
        if opts.dry_run {
            for &i in &publish {
                let out = spec.out_path(&spec.assets[i]);
                report.line("convert", &spec.assets[i].key, Status::Planned, &format!(" -> {}", shown(&out)), json!({"path": shown(&out)}));
            }
        } else {
            let work = |n: usize| {
                let asset = &spec.assets[publish[n]];
                match convert_asset(spec, asset) {
                    Ok((status, detail, info)) => report.line("convert", &asset.key, status, &detail, info),
                    Err(error) => report.line("convert", &asset.key, Status::Failed(error), "", json!({})),
                }
                Ok(())
            };
            conversion_phase(opts, publish.len(), &work)?;
        }
    }
    let tally = report.tally.into_inner().unwrap_or_else(|e| e.into_inner());
    if !opts.quiet {
        let mut parts = Vec::new();
        if opts.generate && !opts.dry_run {
            parts.push(format!("generated {}, kept {}", tally.generated, tally.kept));
        }
        if opts.convert && !opts.dry_run {
            parts.push(format!("converted {} ({} unchanged)", tally.converted + tally.unchanged, tally.unchanged));
        }
        if tally.failed > 0 {
            parts.push(format!("{} failed", tally.failed));
        }
        if !parts.is_empty() {
            eprintln!("codex-img batch: {}", parts.join("; "));
        }
    }
    Ok(tally.exit_code)
}

fn conversion_phase(opts: &BatchOptions, count: usize, work: &(dyn Fn(usize) -> std::result::Result<(), bool> + Sync)) -> Result<()> {
    // Generation already owns this spec lock. Conversion-only acquires it once before any
    // worker starts, so contention is a run-level error and local conversions stay parallel.
    let _lock = if !opts.generate && count > 0 && Path::new(&opts.spec).is_file() { Some(crate::history::lock(Path::new(&opts.spec), opts.no_wait)?) } else { None };
    run_pool(count, &vec![Vec::new(); count], opts.jobs.max(num_cpus()), work, &|_, _| {});
    Ok(())
}

/// Conversion replaces its outputs, so none may be an input: a raw image or an `images` file of
/// any asset. raw_dir and out_dir can overlap (both ".", or one a symlink to the other), and then
/// `<key>.png` would be converted over its own raw image, losing it. Checked before any work, so a
/// spec like that costs no quota.
fn check_outputs(spec: &Spec) -> Result<()> {
    let mut inputs: Vec<(PathBuf, String)> = Vec::new();
    for asset in &spec.assets {
        inputs.push((resolved(&spec.raw_path(asset)), format!("the raw image of \"{}\"", asset.key)));
        inputs.extend(asset.images.iter().map(|p| (resolved(p), format!("an image \"{}\" uses", asset.key))));
        inputs.extend(asset.role_images.iter().map(|i| (resolved(&i.path), format!("a {} reference \"{}\" uses", i.role.name(), asset.key))));
    }
    for asset in spec.assets.iter().filter(|a| a.publish) {
        let out = spec.out_path(asset);
        if let Some((_, what)) = inputs.iter().find(|(input, _)| *input == resolved(&out)) {
            return Err(Error::usage(format!(
                "assets.\"{}\": its output {} would overwrite {what}; keep inputs out of out_dir, and give out_dir and raw_dir separate folders.",
                asset.key,
                shown(&out)
            )));
        }
    }
    Ok(())
}

/// `path` with symlinks and `.`/`..` resolved, also where it doesn't exist yet, so two spellings
/// of one file compare equal. Components are taken in order; before each `..`, the path so far
/// is resolved (a symlink goes to its target) and then loses its last component. Parts that don't
/// exist yet will be created as plain directories, so stepping up out of them is exact.
pub fn resolved(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out = resolved_prefix(&out);
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    resolved_prefix(&out)
}

/// A path without `..`: its deepest existing ancestor with symlinks resolved, and the rest appended.
fn resolved_prefix(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut current = path;
    loop {
        let existing = if current.as_os_str().is_empty() { Path::new(".") } else { current };
        if let Ok(real) = existing.canonicalize() {
            return rest.iter().rev().fold(real, |path, part| path.join(part));
        }
        match (current.parent(), current.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                current = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// A path as output shows it. A spec in art/ with out_dir "../public" gives art/../public/...;
/// when a path steps up with `..` like that and resolves to a file inside the working directory,
/// it is shown relative to it (public/...), still naming the same file through any symlink.
/// Anything else is shown as given.
fn shown(path: &Path) -> String {
    if path.components().any(|c| c == Component::ParentDir) {
        let real = resolved(path);
        if let Some(inside) = std::env::current_dir().and_then(|d| d.canonicalize()).ok().and_then(|cwd| real.strip_prefix(cwd).ok().map(Path::to_path_buf)) {
            return inside.display().to_string();
        }
    }
    path.display().to_string()
}

fn num_cpus() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

fn single_parent(inputs: &[manifest::Input]) -> Option<PathBuf> {
    let plain: Vec<_> = inputs.iter().filter(|input| input.role == "input").collect();
    (plain.len() == 1).then(|| plain[0].path.clone())
}

fn generation_start(spec: &Spec, asset: &Asset) -> Value {
    let mut fields = json!({"key":asset.key,"prompt":asset.user_prompt,
        "submittedPrompt":(asset.user_prompt != asset.prompt).then_some(&asset.prompt),
        "inputs":events::inputs(&spec.inputs(asset)),
        "request":manifest::request(asset.aspect, asset.size.as_deref(), asset.quality.as_deref(), asset.background.as_deref()),
        "output":events::absolute(&spec.raw_path(asset))});
    if asset.replacement.is_none() { if let Some(parent) = single_parent(&spec.inputs(asset)) { fields["parent"] = json!(events::absolute(&parent)); } }
    if let Some(kind) = replacement_kind(asset) { fields["kind"] = json!(kind); }
    fields
}
/// A replaced raw is either edited (Send as edit) or generated again from the same setup (re-roll).
fn replacement_kind(asset: &Asset) -> Option<&'static str> {
    if asset.input_override.is_some() { Some("edit") } else if asset.replacement.is_some() { Some("rerun") } else { None }
}
fn generate(spec: &Spec, plan: &[usize], jobs: usize, backend: &Backend, credentials: &Credentials, report: &Report, run: &events::Run) {
    let session_id = util::random_id();
    // Plan positions each asset waits for: its references that are generated too.
    let waits: Vec<Vec<usize>> = plan.iter().map(|&i| spec.assets[i].references.iter().filter_map(|r| plan.iter().position(|p| p == r)).collect()).collect();
    let work = |n: usize| {
        let asset = &spec.assets[plan[n]];
        let started = Instant::now();
        let job = run.job(generation_start(spec, asset));
        match generate_asset(spec, asset, backend, credentials, &session_id, &|stage| job.stage(stage), &|p| {
            (run.manifest_path(p), run.manifest_root(p))
        }) {
            Ok((raw, warning)) => {
                let seconds = started.elapsed().as_secs_f64();
                job.done(json!({
                    "parent": asset.replacement.as_ref().and_then(|r| r.previous.as_deref()).map(events::absolute),
                    "historyPath": asset.replacement.as_ref().and_then(|r| r.previous.as_deref()).map(events::absolute),
                    "path": events::absolute(&raw),
                    "manifestPath": Some(manifest::path_for(&raw)).filter(|m| m.exists()).map(|m| events::absolute(&m)),
                    "durationMs": (seconds * 1000.0) as u64,
                }));
                let mut info = json!({"rawPath": shown(&raw), "durationMs": (seconds * 1000.0) as u64});
                let mut detail = format!(" -> {} ({seconds:.1}s)", shown(&raw));
                if let Some(warning) = warning {
                    detail.push_str(&format!("; warning: {warning}"));
                    info["warning"] = json!(warning);
                }
                report.line("generate", &asset.key, Status::Ok, &detail, info);
                Ok(())
            }
            Err(error) => {
                job.failed(&error);
                let fatal = matches!(error.kind, Kind::Auth | Kind::Quota);
                report.line("generate", &asset.key, Status::Failed(error), "", json!({}));
                Err(fatal)
            }
        }
    };
    let skip = |n: usize, failed: Option<usize>| {
        let asset = &spec.assets[plan[n]];
        let message = match failed {
            Some(w) => format!("not generated: its reference {} failed", spec.assets[plan[w]].key),
            None => "not generated: stopped after a login or quota error".to_string(),
        };
        report.line("generate", &asset.key, Status::Failed(Error::other(message)), "", json!({}));
    };
    run_pool(plan.len(), &waits, jobs, &work, &skip);
}

/// Generate one raw image and save it, untouched, as `<raw_dir>/<key>.png`. Also returns a
/// warning when the image's shape is off the asset's `aspect`.
fn generate_asset(
    spec: &Spec,
    asset: &Asset,
    backend: &Backend,
    credentials: &Credentials,
    session_id: &str,
    progress: &dyn Fn(&str),
    path_shown: &dyn Fn(&Path) -> (String, Option<String>),
) -> Result<(PathBuf, Option<String>)> {
    let mut sent = spec.inputs(asset);
    let paths: Vec<String> = sent.iter().map(|i| i.path.display().to_string()).collect();
    let inputs = images::load_input_images(&paths)?;
    for (input, bytes) in sent.iter_mut().zip(&inputs) {
        if manifest::capture(input, bytes) { return Err(Error::usage(format!("Reference changed before sending: {}. Refine stopped before quota.", input.path.display()))); }
    }
    let request = Request {
        prompt: &asset.prompt,
        transport: asset.transport,
        model: asset.model.as_deref(),
        output_format: Format::Png,
        size: asset.size.as_deref(),
        quality: asset.quality.as_deref(),
        background: asset.background.as_deref(),
        input_images: &inputs,
        session_id,
    };
    let image = backend.generate(&request, credentials, progress)?;
    let mut warning = asset.aspect.zip(images::dimensions(&image.bytes)).and_then(|(a, size)| a.mismatch(size));
    let raw = spec.raw_path(asset);
    let parent = asset.replacement.as_ref().and_then(|r| r.previous.clone()).or_else(|| single_parent(&sent));
    let conversion = crate::conversion_record::build(Format::Png, &Encoding::default(), &crate::transform::Transform::default());
    if let Some(previous) = asset.replacement.as_ref().and_then(|r| r.previous.as_ref()) {
        for input in &mut sent { if resolved(&input.path) == resolved(&raw) { input.path = previous.clone(); } }
    }
    let record = manifest::Record {
        user_prompt: &asset.user_prompt,
        prompt: &asset.prompt,
        used: &asset.used,
        aspect: asset.aspect,
        size: asset.size.as_deref(),
        quality: asset.quality.as_deref(),
        background: asset.background.as_deref(),
        inputs: &sent,
        parent: parent.as_deref(),
        conversion: Some(&conversion),
    };
    let mut note = manifest::build(&record, &image, &|p| path_shown(p).0);
    note["setup"] = asset.setup.clone();
    if let Some(inputs) = note.pointer_mut("/setup/inputs").and_then(Value::as_array_mut) {
        for input in inputs { if let Some(path) = input["path"].as_str() {
            let mut path = PathBuf::from(path);
            if resolved(&path) == resolved(&raw) { if let Some(previous) = asset.replacement.as_ref().and_then(|r| r.previous.as_ref()) { path = previous.clone(); } }
            input["path"] = json!(path_shown(&path).0);
        } }
    }
    if asset.input_override.is_some() { note["refined"] = json!(true); }
    if let Some(kind) = replacement_kind(asset) { note["kind"] = json!(kind); }
    if let Some(comment) = &asset.from_comment { note["fromComment"] = json!(comment); }
    if let Some(root) = path_shown(&raw).1 { note["root"] = json!(root); }
    // The quota is spent: keep what came back under its real extension if it can't become PNG.
    let png = if image.format == Format::Png { Ok(image.bytes.clone()) } else { images::convert(&image.bytes, Format::Png, &Encoding::default()) };
    match png {
        Ok(png) => {
            if let Some(replacement) = &asset.replacement { replacement.commit(&png, &note)?; return Ok((raw, warning)); }
            cli::write_output(&raw, &png, false)?;
            if let Err(error) = manifest::write(&manifest::path_for(&raw), &note) {
                let failed = format!("could not write the manifest: {}", error.message);
                warning = Some(warning.map_or(failed.clone(), |w| format!("{w}; {failed}")));
            }
            Ok((raw, warning))
        }
        Err(error) => {
            let kept = raw.with_extension(image.format.extension());
            cli::write_output(&kept, &image.bytes, false)?;
            Err(Error::other(format!("{}; saved the {} as {}", error.message, image.format.name(), shown(&kept))))
        }
    }
}

fn convert_asset(spec: &Spec, asset: &Asset) -> Result<(Status, String, Value)> {
    let raw = spec.raw_path(asset);
    let (bytes, _) = convert::read_image(&raw)?;
    let target = spec.out_path(asset);
    let converted = cli::save_converted(&bytes, asset.format, &asset.encoding, &asset.transform, &target, true)?;
    let (w, h) = converted.size;
    let written = std::fs::metadata(&target).map(|m| m.len()).unwrap_or_default();
    let mut info = json!({"path": shown(&target), "rawPath": shown(&raw), "size": format!("{w}x{h}"), "bytes": written});
    if let Some(rect) = converted.trim {
        info["trim"] = json!({"x": rect.x, "y": rect.y, "width": rect.width, "height": rect.height});
    }
    let status = if converted.unchanged { Status::Unchanged } else { Status::Ok };
    Ok((status, format!(" -> {} ({w}x{h}, {} KB)", shown(&target), written.div_ceil(1024)), info))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::tests::{creds, direct_response, serve};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn spec(value: Value) -> Result<Spec> {
        parse_spec(&value, Path::new("/project/art"), &PresetFiles::default())
    }

    fn spec_error(value: Value) -> String {
        spec(value).unwrap_err().message
    }

    #[test]
    fn another_spec_runs_while_first_is_locked_and_same_spec_no_wait_stops() {
        let (dir, spec) = temp_spec("batch-lock-command", json!({"hero":{"prompt":"Fox"}}));
        std::fs::write(dir.join("codex-img.json"), "{}").unwrap();
        let first = dir.join("first.json"); let second = dir.join("second.json");
        std::fs::write(&first, "{}").unwrap(); std::fs::write(&second, "{}").unwrap();
        cli::write_output(&spec.raw_path(&spec.assets[0]), include_bytes!("../tests/fixtures/sprite.png"), false).unwrap();
        let held = crate::history::lock(&first, true).unwrap();
        let free = parse(&[second.display().to_string(), "--convert-only".into(), "--no-wait".into()]).unwrap().unwrap();
        assert_eq!(execute(&free, &spec, &Backend::default(), &|| panic!("conversion is free")).unwrap(), 0);
        let same = parse(&[first.display().to_string(), "--generate-only".into(), "--no-wait".into()]).unwrap().unwrap();
        assert!(execute(&same, &spec, &Backend::default(), &|| panic!("locked before login")).unwrap_err().message.contains("--no-wait"));
        drop(held);
    }
    #[test]
    fn reroll_started_event_omits_the_not_yet_created_history_parent() {
        let (dir, mut spec) = temp_spec("reroll-start-parent", json!({"hero":{"prompt":"Fox"}}));
        let raw = spec.raw_path(&spec.assets[0]); cli::write_output(&raw, b"old raw", false).unwrap();
        let replacement = crate::history::Replacement::plan(&raw, &dir.join("assets.json"), "hero").unwrap();
        let history = replacement.previous.clone().unwrap();
        spec.assets[0].replacement = Some(std::sync::Arc::new(replacement));
        let fields = generation_start(&spec, &spec.assets[0]); assert!(fields.get("parent").is_none()); assert_eq!(fields["kind"], "rerun");
        assert_eq!(std::fs::read(&raw).unwrap(), b"old raw"); assert!(!history.exists());
        assert_eq!(std::fs::read_dir(raw.parent().unwrap()).unwrap().count(), 1);
    }
    #[test]
    fn restore_start_event_keeps_the_contract_without_a_source_manifest() {
        let (dir, spec) = temp_spec("restore-start", json!({"hero":{"prompt":"Fox"}}));
        let (source, raw) = (dir.join("plain.jpeg"), spec.raw_path(&spec.assets[0]));
        let bare = restore_start(&json!({}), &spec.assets[0], "hero", &source, &raw);
        assert_eq!((&bare["prompt"], &bare["request"]), (&json!(spec.assets[0].user_prompt), &json!({})));
        let noted = restore_start(&json!({"userPrompt":"Old fox","request":{"aspect":"1:1"}}), &spec.assets[0], "hero", &source, &raw);
        assert_eq!((&noted["prompt"], &noted["request"]["aspect"]), (&json!("Old fox"), &json!("1:1")));
    }
    #[test]
    fn convert_only_no_wait_does_not_contend_with_its_own_workers() {
        let assets = (0..20).map(|i| (format!("asset-{i}"), json!({"prompt":"Fox"}))).collect::<Map<_, _>>();
        let (dir, spec) = temp_spec("conversion-self-lock", Value::Object(assets));
        let file = dir.join("assets.json"); std::fs::write(&file, "{}").unwrap();
        for asset in &spec.assets { cli::write_output(&spec.raw_path(asset), include_bytes!("../tests/fixtures/sprite.png"), false).unwrap(); }
        let opts = parse(&[file.display().to_string(), "--convert-only".into(), "--no-wait".into(), "--quiet".into()]).unwrap().unwrap();
        assert_eq!(execute(&opts, &spec, &Backend::default(), &|| panic!("conversion is free")).unwrap(), 0);
        assert!(spec.assets.iter().all(|asset| spec.out_path(asset).is_file()));
    }
    #[test]
    fn conversion_phase_runs_parallel_workers_under_one_lock_and_stops_before_work_if_busy() {
        let dir = crate::auth::tests::temp_dir("parallel-conversion-lock");
        let file = dir.join("assets.json"); std::fs::write(&file, "{}").unwrap();
        let opts = parse(&[file.display().to_string(), "--convert-only".into(), "--no-wait".into()]).unwrap().unwrap();
        let started = Mutex::new(0);
        let changed = Condvar::new();
        conversion_phase(&opts, 2, &|_| {
            assert!(crate::history::lock(&file, true).is_err(), "the phase protects every worker");
            let mut count = started.lock().unwrap(); *count += 1; changed.notify_all();
            let (count, timeout) = changed.wait_timeout_while(count, std::time::Duration::from_secs(5), |count| *count < 2).unwrap();
            assert!(!timeout.timed_out() && *count == 2, "both conversions must start concurrently");
            Ok(())
        }).unwrap();
        let held = crate::history::lock(&file, true).unwrap();
        let error = conversion_phase(&opts, 20, &|_| panic!("busy runs stop before workers start")).unwrap_err();
        assert!(error.message.contains("--no-wait"));
        drop(held);
        assert!(crate::history::lock(&file, true).is_ok(), "phase releases the lock");
    }
    #[test]
    fn edited_raw_compares_original_generation_and_tolerates_missing_or_cyclic_parents() {
        let (dir, spec) = temp_spec("edited-state", json!({"hero":{"prompt":"Fox"}}));
        std::fs::write(dir.join("codex-img.json"), "{}").unwrap();
        let asset = &spec.assets[0]; let raw = spec.raw_path(asset);
        std::fs::create_dir_all(raw.parent().unwrap()).unwrap(); std::fs::write(&raw, b"raw").unwrap();
        let original = dir.join("original.png");
        let baseline = json!({"prompt":asset.prompt,"request":{},"comment":"not a change"});
        manifest::write(&manifest::path_for(&original), &baseline).unwrap();
        let second = dir.join("second.png");
        manifest::write(&manifest::path_for(&second), &json!({"refined":true,"parent":"original.png","prompt":"Change only: blue"})).unwrap();
        manifest::write(&manifest::path_for(&raw), &json!({"fromComment":"hat","parent":"second.png","prompt":"Change only: hat"})).unwrap();
        assert_eq!(spec.raw_state(asset), (false, true));
        let changed = parse_spec(&json!({"style":"Pixel art.","assets":{"hero":{"prompt":"Wolf"}}}), &dir, &PresetFiles::default()).unwrap();
        assert_eq!(changed.raw_state(&changed.assets[0]), (true, true));
        std::fs::remove_file(manifest::path_for(&original)).unwrap(); assert_eq!(spec.raw_state(asset), (false, true));
        std::fs::write(manifest::path_for(&second), serde_json::to_vec(&json!({"refined":true,"parent":"raw/hero.png"})).unwrap()).unwrap();
        assert_eq!(spec.raw_state(asset), (false, true));
    }
    #[test]
    fn comments_do_not_change_generation_or_the_changed_check() {
        let dir = crate::auth::tests::temp_dir("batch-comments").canonicalize().unwrap();
        let value = json!({"assets":{"hero":{"prompt":"fox","comment":"Blue hat"}}});
        let spec = parse_spec(&value, &dir, &PresetFiles::default()).unwrap();
        std::fs::create_dir_all(&spec.raw_dir).unwrap();
        manifest::write(&manifest::path_for(&spec.raw_path(&spec.assets[0])), &json!({"prompt":spec.assets[0].prompt,"request":{},"inputs":[]})).unwrap();
        assert!(!spec.changed(&spec.assets[0]));
        let mut edited = value.clone(); edited["assets"]["hero"]["comment"] = json!("Another note");
        let edited = parse_spec(&edited, &dir, &PresetFiles::default()).unwrap();
        assert_eq!(edited.assets[0].prompt, spec.assets[0].prompt);
        assert!(!edited.changed(&edited.assets[0]));
        let mut invalid = value; invalid["assets"]["hero"]["comment"] = json!(7);
        assert!(parse_spec(&invalid, &dir, &PresetFiles::default()).unwrap_err().message.contains("comment must be a string"));
    }
    #[test]
    fn parses_options() {
        let o = parse(&args(&["art/assets.json", "tracks/city", "shared/trees/oak", "-j", "2", "--dry-run", "--json"])).unwrap().unwrap();
        assert_eq!((o.spec.as_str(), o.filters.len(), o.jobs, o.dry_run, o.json, o.generate, o.convert), ("art/assets.json", 2, 2, true, true, true, true));
        let o = parse(&args(&["s.json", "--convert-only"])).unwrap().unwrap();
        assert_eq!((o.generate, o.convert), (false, true));
        assert_eq!(parse(&args(&["--help"])).unwrap(), None);
        let usage = |list: &[&str]| parse(&args(list)).unwrap_err().message;
        assert!(usage(&[]).contains("spec file"));
        assert!(usage(&["s.json", "--jobs=0"]).contains("--jobs"));
        assert!(usage(&["s.json", "--generate-only", "--convert-only"]).contains("combined"));
        assert!(usage(&["s.json", "-c", "8"]).contains("Unknown batch option"));
    }

    #[test]
    fn reads_assets_over_defaults() {
        let s = spec(json!({
            "raw_dir": "raw", "out_dir": "../public/assets", "style": "Pixel art.",
            "defaults": {"background": "transparent", "hard_alpha": true, "colors": 160, "trim": true},
            "assets": {
                "trees/oak": {"prompt": "An oak.", "size": "1536x1024", "max": [420, 380]},
                "sky": {"prompt": "A sky.", "background": "opaque", "format": "webp", "colors": null, "hard_alpha": false, "trim": false, "output_quality": 85},
                "boat": {"prompt": "A boat.", "key": "auto", "key_region": "bottom:30%", "key_cut": true, "trim_density": 0.15, "trim": 4},
                "mill/full": {"prompt": "A windmill.", "publish": false},
                "mill/sails": {"prompt": "Only the sails.", "reference": "mill/full", "images": ["refs/style.png"]}
            }
        }))
        .unwrap();
        let [oak, sky, boat, full, sails] = &s.assets[..] else { panic!() };
        assert_eq!(oak.prompt, "An oak. Pixel art.");
        let framed = spec(json!({"style": "Pixel art.", "defaults": {"aspect": "3:2"}, "assets": {"a": {"prompt": "A."}, "b": {"prompt": "B.", "aspect": null}}})).unwrap();
        assert_eq!(framed.assets[0].prompt, "The frame must be in 3:2 landscape format, wider than it is tall.\n\nA. Pixel art.");
        assert_eq!((framed.assets[1].prompt.as_str(), framed.assets[1].aspect), ("B. Pixel art.", None));
        assert_eq!((oak.background.as_deref(), oak.encoding.colors, oak.transform.hard_alpha, oak.transform.trim), (Some("transparent"), Some(160), Some(16), Some(0)));
        assert_eq!((oak.transform.resize, oak.transform.no_enlarge), (Some(Resize { width: Some(420), height: Some(380) }), true));
        assert_eq!((sky.background.as_deref(), sky.format, sky.encoding.colors, sky.encoding.quality), (Some("opaque"), Format::Webp, None, Some(85)));
        assert_eq!((sky.transform.hard_alpha, sky.transform.trim), (None, None));
        assert_eq!((boat.transform.keys.len(), boat.transform.key_cut, boat.transform.trim, boat.transform.trim_density.map(|d| d.percent)), (1, Some(40), Some(4), Some(15)));
        assert!(!full.publish && sails.publish);
        assert_eq!((sails.references.clone(), sails.images.clone()), (vec![3], vec![PathBuf::from("/project/art/refs/style.png")]));
        assert_eq!(s.raw_path(oak), PathBuf::from("/project/art/raw/trees/oak.png"));
        assert_eq!(s.out_path(sky), PathBuf::from("/project/art/../public/assets/sky.webp"));
    }

    #[test]
    fn rejects_bad_specs_with_the_asset_named() {
        let asset = |fields: Value| spec_error(json!({"assets": {"a/b": fields}}));
        assert!(asset(json!({"prompt": "x", "shape": "1536x1024"})).contains("assets.\"a/b\": unknown field \"shape\""));
        assert!(asset(json!({})).contains("prompt is required"));
        assert!(asset(json!({"prompt": "x", "size": "big"})).contains("size"));
        assert!(asset(json!({"prompt": "x", "format": "webp", "colors": 64})).contains("PNG"));
        assert!(asset(json!({"prompt": "x", "reference": "nope"})).contains("not an asset"));
        assert!(asset(json!({"prompt": "x", "reference": "a/b"})).contains("itself"));
        assert!(asset(json!({"prompt": "x", "max": "400x", "resize": "400x"})).contains("max and resize"));
        assert!(asset(json!({"prompt": "x", "key": "sea"})).contains("--key"));
        assert!(asset(json!({"prompt": "x", "trim_density": 0.2})).contains("--trim"));
        assert!(spec_error(json!({"assets": {"../up": {"prompt": "x"}}})).contains("relative path"));
        assert!(spec_error(json!({"assets": {}, "defaults": {"prompt": "x"}})).contains("defaults can't"));
        assert!(spec_error(json!({"assets": {}, "extra": 1})).contains("unknown field \"extra\""));
        assert!(asset(json!({"prompt": "x", "aspect": "5:1"})).contains("aspect must be between"));
        assert!(asset(json!({"prompt": "x", "aspect": "2:3", "size": "1024x1536"})).contains("aspect and size"));
        let loop_spec = json!({"assets": {"a": {"prompt": "x", "reference": "b"}, "b": {"prompt": "y", "reference": "a"}}});
        assert!(spec_error(loop_spec).contains("loop: a -> b -> a"));
    }

    fn temp_spec(name: &str, assets: Value) -> (PathBuf, Spec) {
        let dir = crate::auth::tests::temp_dir(name);
        let spec = parse_spec(&json!({"style": "Pixel art.", "raw_dir": "raw", "out_dir": "out", "assets": assets}), &dir, &PresetFiles::default()).unwrap();
        (dir, spec)
    }

    fn options(filters: &[&str]) -> BatchOptions {
        let mut opts = parse(&args(&["spec.json", "--quiet"])).unwrap().unwrap();
        opts.filters = args(filters);
        opts
    }

    #[test]
    fn restores_jpeg_without_manifest_and_preserves_source_and_history() {
        let (dir, spec) = temp_spec("restore-no-note", json!({"hero":{"prompt":"Fox","publish":false}}));
        let file = dir.join("assets.json"); std::fs::write(&file, "{}").unwrap();
        std::fs::write(dir.join("codex-img.json"), "{}").unwrap();
        let raw = spec.raw_path(&spec.assets[0]);
        cli::write_output(&raw, include_bytes!("../tests/fixtures/sprite.png"), false).unwrap();
        let source = dir.join("plain.jpg");
        let bytes = images::convert(include_bytes!("../tests/fixtures/sprite.png"), Format::Jpeg, &Encoding::default()).unwrap();
        std::fs::write(&source, &bytes).unwrap();
        let opts = parse(&[file.display().to_string(), "--restore=hero".into(), source.display().to_string()]).unwrap().unwrap();
        assert_eq!(execute(&opts, &spec, &Backend::default(), &|| panic!("restore is free")).unwrap(), 0);
        assert_eq!(std::fs::read(&source).unwrap(), bytes); assert!(!manifest::path_for(&source).exists());
        assert_eq!(images::sniff(&std::fs::read(&raw).unwrap()), Some(Format::Png));
        let note = manifest::read(&manifest::path_for(&raw)).unwrap();
        assert_eq!(note["parent"], resolved(&source).display().to_string()); assert!(note.get("root").is_none());
        assert!(std::fs::read_dir(crate::history::folder(&file, "hero")).unwrap().any(|entry| entry.unwrap().path().extension().is_some_and(|e| e == "png")));
    }
    #[test]
    fn restore_never_inserts_missing_input_or_setup_keys() {
        let (dir, spec) = temp_spec("restore-legacy", json!({"hero":{"prompt":"Fox","publish":false}}));
        let file = dir.join("assets.json"); std::fs::write(&file, "{}").unwrap();
        let source = dir.join("plain.png"); std::fs::write(&source, include_bytes!("../tests/fixtures/sprite.png")).unwrap();
        manifest::write(&manifest::path_for(&source), &json!({"prompt":"Legacy","comment":"Source","star":true})).unwrap();
        let opts = parse(&[file.display().to_string(), "--restore=hero".into(), source.display().to_string()]).unwrap().unwrap();
        restore(&opts, &spec).unwrap();
        let note = manifest::read(&manifest::path_for(&spec.raw_path(&spec.assets[0]))).unwrap();
        for key in ["inputs", "setup", "comment", "star"] { assert!(note.get(key).is_none(), "unexpected {key}"); }
        assert_eq!(manifest::read(&manifest::path_for(&source)).unwrap()["comment"], "Source");
    }
    #[test]
    fn restore_refuses_current_manifest_comment_before_mutating_images() {
        let (dir, spec) = temp_spec("restore-comment", json!({"hero":{"prompt":"Fox","publish":false}}));
        let file = dir.join("assets.json"); std::fs::write(&file, "{}").unwrap();
        let raw = spec.raw_path(&spec.assets[0]); cli::write_output(&raw, b"old", false).unwrap();
        manifest::write(&manifest::path_for(&raw), &json!({"comment":"Please keep this"})).unwrap();
        let opts = parse(&[file.display().to_string(), "--restore=hero".into(), dir.join("missing.png").display().to_string()]).unwrap().unwrap();
        let error = restore(&opts, &spec).unwrap_err(); assert!(error.message.contains("Clear or move"));
        assert_eq!(std::fs::read(&raw).unwrap(), b"old"); assert!(!crate::history::folder(&file, "hero").exists());
        assert_eq!(manifest::read(&manifest::path_for(&raw)).unwrap()["comment"], "Please keep this");
    }

    #[test]
    fn rerolls_keep_old_raws_until_success_and_restore_preserves_the_source() {
        let (dir, spec) = temp_spec("batch-history", json!({"actors/hero":{"prompt":"Fox"}}));
        let file = dir.join("assets.json"); std::fs::write(&file, r#"{"assets":{"actors/hero":{"prompt":"Fox"}}}"#).unwrap();
        let raw = spec.raw_path(&spec.assets[0]);
        cli::write_output(&raw, include_bytes!("../tests/fixtures/sprite.png"), false).unwrap();
        manifest::write(&manifest::path_for(&raw), &json!({"prompt":"Old fox","comment":"Keep","star":true})).unwrap();
        let old = std::fs::read(&raw).unwrap(); let old_note = std::fs::read(manifest::path_for(&raw)).unwrap();
        let opts = parse(&[file.display().to_string(), "--reroll".into(), "actors/hero".into(), "--json".into(), "--quiet".into()]).unwrap().unwrap();
        let (backend, _) = serve(vec![(401, "application/json", "{}".into())]);
        assert_ne!(execute(&opts, &spec, &backend, &|| Ok(creds())).unwrap(), 0);
        assert_eq!(std::fs::read(&raw).unwrap(), old); assert_eq!(std::fs::read(manifest::path_for(&raw)).unwrap(), old_note);
        let (backend, captured) = serve(vec![(200, "application/json", direct_response())]);
        assert_eq!(execute(&opts, &spec, &backend, &|| Ok(creds())).unwrap(), 0); assert_eq!(captured.lock().unwrap().len(), 1);
        let note = manifest::read(&manifest::path_for(&raw)).unwrap();
        let previous = PathBuf::from(note["parent"].as_str().unwrap()); assert!(previous.is_file()); assert!(previous.to_string_lossy().contains("history/actors/hero/"));
        assert_eq!(std::fs::read(&previous).unwrap(), old); assert_eq!(std::fs::read(manifest::path_for(&previous)).unwrap(), old_note);
        let source = dir.join("version.jpg");
        std::fs::write(&source, images::convert(&old, Format::Jpeg, &Encoding::default()).unwrap()).unwrap();
        manifest::write(&manifest::path_for(&source), &json!({"prompt":"Chosen fox","comment":"Source note","star":true})).unwrap();
        let before = std::fs::read(&source).unwrap();
        let restore = parse(&[file.display().to_string(), "--restore".into(), "actors/hero".into(), source.display().to_string(), "--quiet".into()]).unwrap().unwrap();
        assert_eq!(execute(&restore, &spec, &Backend::default(), &|| panic!("restore is free")).unwrap(), 0);
        assert_eq!(std::fs::read(&source).unwrap(), before); assert_eq!(images::sniff(&std::fs::read(&raw).unwrap()), Some(Format::Png));
        let note = manifest::read(&manifest::path_for(&raw)).unwrap(); assert_eq!(note["parent"], resolved(&source).display().to_string()); assert!(note.get("comment").is_none()); assert!(note.get("star").is_none());
        assert_eq!(manifest::read(&manifest::path_for(&source)).unwrap()["comment"], "Source note");
    }

    #[test]
    fn selects_by_key_or_folder_and_plans_references_first() {
        let assets = json!({
            "mill/full": {"prompt": "A mill.", "publish": false},
            "mill/sails": {"prompt": "Sails.", "reference": "mill/full"},
            "millstone": {"prompt": "A stone."},
            "trees/oak": {"prompt": "An oak."}
        });
        let (dir, spec) = temp_spec("batch-select", assets);
        assert_eq!(select(&spec, &args(&["mill"])).unwrap(), [0, 1], "a folder, not a prefix of millstone");
        assert_eq!(select(&spec, &args(&["mill/", "trees/oak"])).unwrap(), [0, 1, 3]);
        assert_eq!(select(&spec, &[]).unwrap().len(), 4);
        assert!(select(&spec, &args(&["trees/pine"])).unwrap_err().message.contains("trees/pine"));

        // The reference comes first even when only the sails are asked for, unless its raw exists.
        assert_eq!(generation_plan(&spec, &[1]), [0, 1]);
        std::fs::create_dir_all(dir.join("raw/mill")).unwrap();
        std::fs::write(dir.join("raw/mill/full.png"), b"x").unwrap();
        assert_eq!(generation_plan(&spec, &[1, 3]), [1, 3]);
    }

    #[test]
    fn generates_missing_raws_then_converts_everything_selected() {
        let assets = json!({
            "mill/full": {"prompt": "A mill.", "publish": false, "size": "1024x1536"},
            "mill/sails": {"prompt": "Only the sails.", "reference": "mill/full", "colors": 4},
            "kept": {"prompt": "Already there.", "format": "webp", "lossless": true}
        });
        let (dir, spec) = temp_spec("batch-run", assets);
        let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, crate::images::tests::PNG_B64).unwrap();
        std::fs::create_dir_all(dir.join("raw")).unwrap();
        std::fs::write(dir.join("raw/kept.png"), &png).unwrap();

        let (backend, captured) = serve(vec![(200, "application/json", direct_response()), (200, "application/json", direct_response())]);
        assert_eq!(execute(&options(&[]), &spec, &backend, &|| Ok(creds())).unwrap(), 0);
        {
            let calls = captured.lock().unwrap();
            assert_eq!(calls.len(), 2);
            assert_eq!((calls[0].path.as_str(), calls[1].path.as_str()), ("/images/generations", "/images/edits"), "the reference is generated first");
            assert_eq!(calls[0].body["prompt"], "A mill. Pixel art.");
            assert_eq!(calls[0].body["size"], "1024x1536");
            assert_eq!(calls[1].body["images"].as_array().unwrap().len(), 1);
        }
        assert!(dir.join("raw/mill/full.png").is_file() && dir.join("raw/mill/sails.png").is_file());
        assert!(!dir.join("out/mill/full.png").exists(), "publish: false is not converted");
        assert!(dir.join("out/mill/sails.png").is_file() && dir.join("out/kept.webp").is_file());

        // Nothing missing: no login needed, and converted files are left alone.
        let modified = std::fs::metadata(dir.join("out/kept.webp")).unwrap().modified().unwrap();
        let no_login = || -> Result<Credentials> { Err(Error::auth("no login")) };
        assert_eq!(execute(&options(&[]), &spec, &backend, &no_login).unwrap(), 0);
        assert_eq!(std::fs::metadata(dir.join("out/kept.webp")).unwrap().modified().unwrap(), modified);
    }

    #[test]
    fn stops_after_quota_and_skips_assets_whose_reference_failed() {
        let assets = json!({
            "a": {"prompt": "A."},
            "b": {"prompt": "B.", "reference": "a"},
            "c": {"prompt": "C."}
        });
        let (dir, spec) = temp_spec("batch-quota", assets);
        let quota = json!({"error": {"code": "insufficient_quota"}}).to_string();
        let (backend, captured) = serve(vec![(429, "application/json", quota)]);
        let mut opts = options(&[]);
        (opts.jobs, opts.convert) = (1, false);
        assert_eq!(execute(&opts, &spec, &backend, &|| Ok(creds())).unwrap(), Kind::Quota.exit_code());
        assert_eq!(captured.lock().unwrap().len(), 1, "no request after the quota error");
        assert!(!dir.join("raw/a.png").exists());

        // Converting a missing raw image fails, and says so.
        let convert_only = BatchOptions { generate: false, convert: true, ..opts };
        assert_eq!(execute(&convert_only, &spec, &backend, &|| Ok(creds())).unwrap(), 1);
    }

    #[test]
    fn skips_conversion_when_generation_failed() {
        let (dir, spec) = temp_spec("batch-failed-convert", json!({"a": {"prompt": "A."}}));
        let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, crate::images::tests::PNG_B64).unwrap();
        std::fs::create_dir_all(dir.join("raw")).unwrap();
        std::fs::write(dir.join("raw/a.png"), &png).unwrap();
        let (backend, _) = serve(vec![(500, "application/json", json!({"error": {"message": "boom"}}).to_string())]);
        let mut opts = options(&[]);
        (opts.jobs, opts.spec, opts.reroll) = (1, dir.join("spec.json").display().to_string(), vec!["a".into()]);
        assert_ne!(execute(&opts, &spec, &backend, &|| Ok(creds())).unwrap(), 0);
        assert_eq!(std::fs::read(dir.join("raw/a.png")).unwrap(), png, "a failed re-roll keeps the old raw");
        assert!(!dir.join("out/a.png").exists(), "the old raw is not converted after its re-roll failed");
    }

    #[test]
    fn refuses_outputs_that_would_overwrite_inputs() {
        let dir = crate::auth::tests::temp_dir("batch-overlap");
        let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, crate::images::tests::PNG_B64).unwrap();
        std::fs::write(dir.join("hero.png"), &png).unwrap();
        let no_login = || -> Result<Credentials> { Err(Error::auth("no login")) };
        let backend = Backend::new("http://127.0.0.1:9");
        let run = |value: Value| execute(&options(&[]), &parse_spec(&value, &dir, &PresetFiles::default()).unwrap(), &backend, &no_login);

        // Same folder for both: hero.png would be converted over its own raw image.
        let same = run(json!({"raw_dir": ".", "out_dir": ".", "assets": {"hero": {"prompt": "A hero.", "resize": "5x5"}}}));
        assert!(same.unwrap_err().message.contains("would overwrite the raw image of \"hero\""));
        assert_eq!(std::fs::read(dir.join("hero.png")).unwrap(), png, "the raw image is untouched");
        // A different format doesn't collide.
        assert_eq!(run(json!({"raw_dir": ".", "out_dir": ".", "assets": {"hero": {"prompt": "A hero.", "format": "webp"}}})).unwrap(), 0);

        // A reference with a role in out_dir would be replaced by a converted output too.
        std::fs::create_dir_all(dir.join("published")).unwrap();
        std::fs::write(dir.join("published/hero.png"), &png).unwrap();
        let styled = run(json!({"out_dir": "published", "assets": {"hero": {"prompt": "A hero."}, "villain": {"prompt": "A villain.", "style_ref": "published/hero.png"}}}));
        assert!(styled.unwrap_err().message.contains("would overwrite a style reference \"villain\" uses"));
        assert_eq!(std::fs::read(dir.join("published/hero.png")).unwrap(), png, "the reference is untouched");

        // The output folder is a symlink to the raw folder.
        std::fs::create_dir_all(dir.join("raw")).unwrap();
        std::fs::write(dir.join("raw/hero.png"), &png).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("raw"), dir.join("out")).unwrap();
            let linked = run(json!({"assets": {"hero": {"prompt": "A hero."}}}));
            assert!(linked.unwrap_err().message.contains("would overwrite"));
        }

        // `..` under a folder that doesn't exist yet: future/.. is the raw folder itself.
        let dotdot = run(json!({"raw_dir": ".", "out_dir": "future/..", "assets": {"hero": {"prompt": "A hero.", "resize": "5x5"}}}));
        assert!(dotdot.unwrap_err().message.contains("would overwrite the raw image"));
        assert!(!dir.join("future").exists() && std::fs::read(dir.join("hero.png")).unwrap() == png);
        // `..` after a symlink steps out of its target, not out of the link's own folder.
        #[cfg(unix)]
        {
            std::fs::create_dir_all(dir.join("raw/deep")).unwrap();
            std::os::unix::fs::symlink(dir.join("raw/deep"), dir.join("link")).unwrap();
            let through = run(json!({"raw_dir": "raw", "out_dir": "link/..", "assets": {"hero": {"prompt": "A hero."}}}));
            assert!(through.unwrap_err().message.contains("would overwrite"));
        }
        assert_eq!(resolved(Path::new("/tmp/../tmp/./a/b/../c.png")), resolved(Path::new("/tmp/a/c.png")));

        // Another asset's reference file.
        let images = json!({"raw_dir": "raw", "out_dir": "public", "assets": {
            "a": {"prompt": "A.", "images": ["public/b.png"]},
            "b": {"prompt": "B."}
        }});
        assert!(run(images).unwrap_err().message.contains("an image \"a\" uses"));
        // Generating only never writes outputs, so it isn't refused.
        let generate_only = BatchOptions { convert: false, dry_run: true, ..options(&[]) };
        let spec = parse_spec(&json!({"raw_dir": ".", "out_dir": ".", "assets": {"hero": {"prompt": "A hero."}}}), &dir, &PresetFiles::default()).unwrap();
        assert_eq!(execute(&generate_only, &spec, &backend, &no_login).unwrap(), 0);
    }

    #[test]
    fn shows_paths_without_dot_dot() {
        assert_eq!(shown(Path::new("art/raw/a.png")), "art/raw/a.png");
        // Tests run in the crate root.
        assert_eq!(shown(Path::new("src/../Cargo.toml")), "Cargo.toml");
        let dir = crate::auth::tests::temp_dir("batch-shown");
        // Outside the working directory, as given.
        let outside = dir.join("art/../public/a.png");
        assert_eq!(shown(&outside), outside.display().to_string());
        assert_eq!(shown(Path::new("../public/a.png")), "../public/a.png");
    }

    #[test]
    fn warns_when_the_frame_is_off_the_aspect() {
        let (dir, spec) = temp_spec("batch-aspect", json!({"wide": {"prompt": "A.", "aspect": "16:9"}}));
        let (backend, captured) = serve(vec![(200, "application/json", direct_response())]);
        let (raw, warning) = generate_asset(&spec, &spec.assets[0], &backend, &creds(), "s", &|_| {}, &|p| (shown(p), None)).unwrap();
        assert_eq!(raw, dir.join("raw/wide.png"));
        assert_eq!(warning.as_deref(), Some("asked for 16:9 but the backend returned 1x1"), "the fake backend returns a 1x1 PNG");
        let calls = captured.lock().unwrap();
        assert!(calls[0].body["prompt"].as_str().unwrap().starts_with("The frame must be in 16:9"));
        assert!(calls[0].body.get("size").is_none(), "aspect sends no size");
    }

    #[test]
    fn presets_compose_the_request_and_the_manifest_notes_changes() {
        let dir = crate::auth::tests::temp_dir("batch-presets");
        let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, crate::images::tests::PNG_B64).unwrap();
        for file in ["refs/captain.png", "refs/ink.png", "refs/layout.png"] {
            std::fs::create_dir_all(dir.join(file).parent().unwrap()).unwrap();
            std::fs::write(dir.join(file), &png).unwrap();
        }
        // The project file is read because the spec names presets; the spec's own wins over it.
        let project = dir.join("codex-img.json");
        std::fs::write(&project, json!({"styles": {"ink": {"text": "Ink wash.", "refs": ["refs/ink.png"]}}, "characters": {"captain": "project captain"}}).to_string()).unwrap();
        let spec_value = |prompt: &str| {
            json!({
                "style": "No text.",
                "characters": {"captain": {"text": "a walrus sailor", "refs": ["refs/captain.png"]}},
                "defaults": {"view": "side", "style": "ink"},
                "assets": {
                    "base": {"prompt": "A pier.", "view": null, "style": null},
                    "hero": {"prompt": prompt, "reference": "base", "character": "captain", "composition_ref": "refs/layout.png"}
                }
            })
        };
        let files = PresetFiles { project: Some(project), global: None };
        let spec = parse_spec(&spec_value("The captain waves."), &dir, &files).unwrap();
        let hero = &spec.assets[1];
        assert!(hero.prompt.starts_with("Camera: seen perfectly straight on from the side"));
        assert!(hero.prompt.contains("Image 2: style reference only") && hero.prompt.contains("Image 3: character reference for \"captain\"") && hero.prompt.contains("Image 4: composition reference only"));
        assert!(hero.prompt.contains("The character \"captain\": a walrus sailor") && hero.prompt.ends_with("The captain waves. Ink wash. No text."));
        assert_eq!(spec.assets[0].prompt, "A pier. No text.", "null turns a default preset off");
        assert!(parse_spec(&json!({"defaults": {"style": "16-bit pixel art"}, "assets": {"a": {"prompt": "x"}}}), &dir, &files).unwrap_err().message.contains("top-level style"));
        let palettes = parse_spec(&json!({"palettes": {"warm": ["#2B1D14", "#F2C14E"]}, "assets": {"a": {"prompt": "A.", "palette": "warm", "palette_clean": true}, "b": {"prompt": "B.", "palette": "game-boy"}}}), &dir, &files).unwrap();
        assert!(palettes.assets[0].prompt.ends_with("A. Use only these 2 colours, exactly, and no others: #2B1D14, #F2C14E. No gradients, no colours in between."));
        assert_eq!(palettes.assets[0].transform.palette.as_ref().map(|p| (p.colors.len(), p.clean)), Some((2, true)));
        assert_eq!(palettes.assets[1].transform.palette.as_ref().map(|p| p.colors.len()), Some(4), "a built-in palette");
        assert!(parse_spec(&json!({"assets": {"a": {"prompt": "A.", "palette": "warm", "colors": 8}}}), &dir, &files).unwrap_err().message.contains("Unknown palette"));
        assert!(parse_spec(&json!({"assets": {"a": {"prompt": "A.", "palette": "#000000 #FFFFFF", "colors": 8}}}), &dir, &files).unwrap_err().message.contains("--colors and --palette"));

        let (backend, captured) = serve(vec![(200, "application/json", direct_response()), (200, "application/json", direct_response())]);
        let mut opts = options(&[]);
        opts.convert = false;
        assert_eq!(execute(&opts, &spec, &backend, &|| Ok(creds())).unwrap(), 0);
        {
            let calls = captured.lock().unwrap();
            assert_eq!(calls[1].body["prompt"], hero.prompt.as_str());
            assert_eq!(calls[1].body["images"].as_array().unwrap().len(), 4, "the base's raw image, then ink, captain and layout");
        }
        let note = manifest::read(&manifest::path_for(&dir.join("raw/hero.png"))).unwrap();
        assert_eq!((note["userPrompt"].as_str(), note["inputs"][0]["path"].as_str()), (Some("The captain waves."), Some(shown(&dir.join("raw").join("base.png")).as_str())));
        assert_eq!(note["presets"][2], json!({"kind": "character", "name": "captain", "source": "spec"}));
        assert!(!spec.changed(hero));
        let edited = parse_spec(&spec_value("The captain salutes."), &dir, &files).unwrap();
        assert!(edited.changed(&edited.assets[1]), "a new prompt");
        std::fs::write(dir.join("refs/layout.png"), b"other").unwrap();
        assert!(spec.changed(hero), "a changed reference image");
    }

    #[test]
    fn refuses_a_changed_plan_count_before_login_or_generation() {
        let (dir, spec) = temp_spec("batch-count", json!({"a":{"prompt":"A."}}));
        let opts = BatchOptions {expect_images:Some(0),..options(&[])};
        let no_login = || -> Result<Credentials> { panic!("A changed count must stop before login"); };
        assert!(execute(&opts,&spec,&Backend::new("http://127.0.0.1:9"),&no_login).unwrap_err().message.contains("now needs 1 image"));
        assert!(!dir.join("raw").exists());
        assert!(parse(&args(&["spec.json","--expect-images=1","--convert-only"])).is_err());
    }
    #[test]
    fn dry_run_touches_nothing() {
        let (dir, spec) = temp_spec("batch-dry", json!({"a": {"prompt": "A."}}));
        let opts = BatchOptions { dry_run: true, ..options(&[]) };
        let no_login = || -> Result<Credentials> { Err(Error::auth("no login")) };
        assert_eq!(execute(&opts, &spec, &Backend::new("http://127.0.0.1:9"), &no_login).unwrap(), 0);
        assert!(!dir.join("raw").exists() && !dir.join("out").exists());
    }
}
