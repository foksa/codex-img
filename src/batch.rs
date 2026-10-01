//! `codex-img batch`: generate and convert a set of assets described in a JSON spec. Missing raw
//! images are generated (references first), then every raw image is converted into its published
//! form. Deleting a raw image is how an asset is re-rolled.
use crate::auth::{self, Credentials};
use crate::backend::{Backend, Request, Transport};
use crate::cli::{self, Aspect};
use crate::convert;
use crate::error::{Error, Kind, Result};
use crate::images::{self, Encoding, Format, MAX_EDIT_IMAGES};
use crate::transform::{self, Density, Fit, Key, Region, Resize, Transform};
use crate::util;
use serde_json::{json, Map, Value};
use std::path::{Component, Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

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
}

pub fn help() -> &'static str {
    r#"Usage:
  codex-img batch <spec.json> [<key or folder>...] [options]

Generates the missing raw images of the assets in a JSON spec, then converts every
raw image into its published form. Raw images that exist are kept: delete one to
re-roll it. Converted files are replaced only when their bytes change.

Options:
      --generate-only       Only generate missing raw images
      --convert-only        Only convert (no login, no quota)
  -j, --jobs <n>            Images generated at the same time (default 4, max 10)
      --dry-run             Show what would be generated and converted
      --json                One JSON object per asset and step on stdout
      --quiet               Report failures only

A filter is an exact key (shared/trees/oak) or a folder of keys (tracks/city).

Spec:
  {
    "raw_dir": "raw",       generated images, <raw_dir>/<key>.png (default "raw")
    "out_dir": "out",       converted images, <out_dir>/<key>.<ext> (default "out");
                            both relative to the spec file
    "style": "...",         appended to every prompt
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
  reference                 Key(s) whose raw image is passed as -i; generated first
  images                    Other -i files, relative to the spec
  publish                   false: generate only, e.g. a reference for other assets
  format                    png (default), jpeg or webp
  colors, dither, output_quality, lossless, trim, hard_alpha, resize, fit,
  no_enlarge, no_bleed, key, key_region, key_cut, key_spread, trim_density
                            As the convert options (`codex-img convert --help`):
                            true for a flag, a number or string for a value, a
                            list for several keys
  max                       "WxH" or [W, H]: resize to fit, never enlarging

Example:
  codex-img batch art/assets.json tracks/city --dry-run"#
}

pub fn parse(args: &[String]) -> Result<Option<BatchOptions>> {
    let mut positionals = Vec::new();
    let mut opts = BatchOptions { spec: String::new(), filters: Vec::new(), generate: true, convert: true, jobs: DEFAULT_JOBS, dry_run: false, json: false, quiet: false };
    let (mut generate_only, mut convert_only) = (false, false);
    let mut iter = args.iter();
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
            "--generate-only" => generate_only = true,
            "--convert-only" => convert_only = true,
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
    Ok(Some(opts))
}

/// One asset of the spec, validated.
#[derive(Debug)]
struct Asset {
    key: String,
    /// With the spec's style appended.
    prompt: String,
    size: Option<String>,
    /// Already applied to `prompt`; kept to check the result's shape.
    aspect: Option<Aspect>,
    quality: Option<String>,
    background: Option<String>,
    /// Indices of the assets whose raw images are passed as references.
    references: Vec<usize>,
    images: Vec<PathBuf>,
    publish: bool,
    format: Format,
    encoding: Encoding,
    transform: Transform,
}

#[derive(Debug)]
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
}

const TOP_FIELDS: [&str; 5] = ["raw_dir", "out_dir", "style", "defaults", "assets"];
const ASSET_FIELDS: [&str; 25] = [
    "prompt", "size", "aspect", "quality", "background", "reference", "images", "publish", "format", "colors", "dither", "output_quality", "lossless", "trim",
    "hard_alpha", "resize", "max", "fit", "no_enlarge", "no_bleed", "key", "key_region", "key_cut", "key_spread", "trim_density",
];

fn load_spec(path: &Path) -> Result<Spec> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::usage(format!("Unable to read {}: {e}", path.display())))?;
    let value: Value = serde_json::from_str(&text).map_err(|e| Error::usage(format!("{} is not valid JSON: {e}", path.display())))?;
    parse_spec(&value, path.parent().unwrap_or(Path::new(""))).map_err(|e| Error::usage(format!("{}: {}", path.display(), e.message)))
}

fn parse_spec(value: &Value, base: &Path) -> Result<Spec> {
    let top = value.as_object().ok_or_else(|| Error::usage("the spec must be a JSON object."))?;
    unknown_fields(top, &TOP_FIELDS, "the spec")?;
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
    if defaults.contains_key("prompt") || defaults.contains_key("reference") {
        return Err(Error::usage("defaults can't set prompt or reference."));
    }
    let entries = top.get("assets").and_then(Value::as_object).ok_or_else(|| Error::usage("assets must be an object of key -> asset."))?;
    let keys: Vec<&str> = entries.keys().map(String::as_str).collect();
    let mut assets = Vec::with_capacity(entries.len());
    for (key, entry) in entries {
        let context = |e: Error| Error::usage(format!("assets.\"{key}\": {}", e.message));
        check_key(key).map_err(context)?;
        let own = entry.as_object().ok_or_else(|| context(Error::usage("must be an object.")))?;
        unknown_fields(own, &ASSET_FIELDS, "the asset").map_err(context)?;
        let fields = Fields { own, defaults };
        assets.push(parse_asset(key, &fields, style, &keys, base).map_err(context)?);
    }
    check_cycles(&assets)?;
    Ok(Spec { raw_dir, out_dir, assets })
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

fn parse_asset(key: &str, fields: &Fields, style: Option<&str>, keys: &[&str], base: &Path) -> Result<Asset> {
    let prompt = fields.string("prompt")?.filter(|p| !p.trim().is_empty()).ok_or_else(|| Error::usage("prompt is required."))?;
    let prompt = match style {
        Some(style) => format!("{} {style}", prompt.trim_end()),
        None => prompt,
    };
    let size = fields.string("size")?;
    if size.as_deref().is_some_and(|s| !cli::is_size(s)) {
        return Err(Error::usage("size must be WIDTHxHEIGHT or auto."));
    }
    let aspect = fields.string("aspect")?.map(|v| Aspect::parse(&v).map_err(|e| Error::usage(e.message.replace("--aspect", "aspect")))).transpose()?;
    if aspect.is_some() && size.as_deref().is_some_and(|s| s != "auto") {
        return Err(Error::usage("aspect and size can't be combined; the backend ignores size, and aspect sets the frame."));
    }
    let prompt = match aspect {
        Some(aspect) => aspect.frame(&prompt),
        None => prompt,
    };
    if prompt.chars().count() > cli::MAX_PROMPT_CHARS {
        return Err(Error::usage("the prompt (with the style and aspect) is longer than 32,000 characters."));
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
        keys: fields.strings("key")?.iter().map(|k| Key::parse(k)).collect::<Result<_>>()?,
        key_region: fields.string("key_region")?.map(|v| Region::parse(&v)).transpose()?,
        key_spread: fields.text("key_spread")?.map(|v| transform::parse_key_spread(&v)).transpose()?,
        key_cut: fields.flag_or_text("key_cut")?.map(|v| v.map_or(Ok(transform::KEY_CUT), |t| transform::parse_key_cut(&t))).transpose()?,
        trim: fields.flag_or_text("trim")?.map(|v| v.map_or(Ok(0), |t| transform::parse_trim_padding(&t))).transpose()?,
        trim_density: fields.text("trim_density")?.map(|v| Density::parse(&v)).transpose()?,
        resize,
        fit: fields.string("fit")?.map(|v| Fit::parse(&v)).transpose()?,
        no_bleed: fields.flag("no_bleed")?,
        no_enlarge,
    };
    transform.check()?;
    Ok(Asset { key: key.to_string(), prompt, size, aspect, quality, background, references, images, publish, format, encoding, transform })
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
        if plan.contains(&i) || spec.raw_path(&spec.assets[i]).exists() {
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
    execute(opts, &spec, &Backend::default(), &auth::load_credentials)
}

fn execute(opts: &BatchOptions, spec: &Spec, backend: &Backend, credentials: &dyn Fn() -> Result<Credentials>) -> Result<i32> {
    let selected = select(spec, &opts.filters)?;
    if opts.convert {
        check_outputs(spec)?;
    }
    let report = Report { json: opts.json, quiet: opts.quiet, tally: Mutex::new(Tally::default()) };
    if opts.generate {
        let plan = generation_plan(spec, &selected);
        for &i in selected.iter().filter(|i| !plan.contains(i)) {
            let raw = spec.raw_path(&spec.assets[i]);
            report.line("generate", &spec.assets[i].key, Status::Skipped, " (raw image exists)", json!({"rawPath": shown(&raw)}));
        }
        if opts.dry_run {
            for &i in &plan {
                let raw = spec.raw_path(&spec.assets[i]);
                report.line("generate", &spec.assets[i].key, Status::Planned, &format!(" -> {}", shown(&raw)), json!({"rawPath": shown(&raw)}));
            }
        } else if !plan.is_empty() {
            let credentials = credentials()?;
            generate(spec, &plan, opts.jobs, backend, &credentials, &report);
        }
    }
    if opts.convert {
        let publish: Vec<usize> = selected.into_iter().filter(|&i| spec.assets[i].publish).collect();
        if opts.dry_run {
            for &i in &publish {
                let out = spec.out_path(&spec.assets[i]);
                report.line("convert", &spec.assets[i].key, Status::Planned, &format!(" -> {}", shown(&out)), json!({"path": shown(&out)}));
            }
        } else {
            let waits = vec![Vec::new(); publish.len()];
            let work = |n: usize| {
                let asset = &spec.assets[publish[n]];
                match convert_asset(spec, asset) {
                    Ok((status, detail, info)) => report.line("convert", &asset.key, status, &detail, info),
                    Err(error) => report.line("convert", &asset.key, Status::Failed(error), "", json!({})),
                }
                Ok(())
            };
            run_pool(publish.len(), &waits, opts.jobs.max(num_cpus()), &work, &|_, _| {});
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

/// Conversion replaces its outputs, so none may be an input: a raw image or an `images` file of
/// any asset. raw_dir and out_dir can overlap (both ".", or one a symlink to the other), and then
/// `<key>.png` would be converted over its own raw image, losing it. Checked before any work, so a
/// spec like that costs no quota.
fn check_outputs(spec: &Spec) -> Result<()> {
    let mut inputs: Vec<(PathBuf, String)> = Vec::new();
    for asset in &spec.assets {
        inputs.push((resolved(&spec.raw_path(asset)), format!("the raw image of \"{}\"", asset.key)));
        inputs.extend(asset.images.iter().map(|p| (resolved(p), format!("an image \"{}\" uses", asset.key))));
    }
    for asset in spec.assets.iter().filter(|a| a.publish) {
        let out = spec.out_path(asset);
        if let Some((_, what)) = inputs.iter().find(|(input, _)| *input == resolved(&out)) {
            return Err(Error::usage(format!(
                "assets.\"{}\": its output {} would overwrite {what}; give out_dir and raw_dir separate folders.",
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
fn resolved(path: &Path) -> PathBuf {
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

fn generate(spec: &Spec, plan: &[usize], jobs: usize, backend: &Backend, credentials: &Credentials, report: &Report) {
    let session_id = util::random_id();
    // Plan positions each asset waits for: its references that are generated too.
    let waits: Vec<Vec<usize>> = plan.iter().map(|&i| spec.assets[i].references.iter().filter_map(|r| plan.iter().position(|p| p == r)).collect()).collect();
    let work = |n: usize| {
        let asset = &spec.assets[plan[n]];
        let started = Instant::now();
        match generate_asset(spec, asset, backend, credentials, &session_id) {
            Ok((raw, warning)) => {
                let seconds = started.elapsed().as_secs_f64();
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
fn generate_asset(spec: &Spec, asset: &Asset, backend: &Backend, credentials: &Credentials, session_id: &str) -> Result<(PathBuf, Option<String>)> {
    let mut paths: Vec<String> = asset.references.iter().map(|&r| spec.raw_path(&spec.assets[r]).display().to_string()).collect();
    paths.extend(asset.images.iter().map(|p| p.display().to_string()));
    let inputs = images::load_input_images(&paths)?;
    let request = Request {
        prompt: &asset.prompt,
        transport: Transport::Direct,
        model: None,
        output_format: Format::Png,
        size: asset.size.as_deref(),
        quality: asset.quality.as_deref(),
        background: asset.background.as_deref(),
        input_images: &inputs,
        session_id,
    };
    let image = backend.generate(&request, credentials, &|_| {})?;
    let warning = asset.aspect.zip(images::dimensions(&image.bytes)).and_then(|(a, size)| a.mismatch(size));
    let raw = spec.raw_path(asset);
    if image.format == Format::Png {
        cli::write_output(&raw, &image.bytes, false)?;
        return Ok((raw, warning));
    }
    // The quota is spent: keep what came back under its real extension if it can't become PNG.
    match images::convert(&image.bytes, Format::Png, &Encoding::default()) {
        Ok(png) => cli::write_output(&raw, &png, false).map(|_| (raw, warning)),
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
        parse_spec(&value, Path::new("/project/art"))
    }

    fn spec_error(value: Value) -> String {
        spec(value).unwrap_err().message
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
        let spec = parse_spec(&json!({"style": "Pixel art.", "raw_dir": "raw", "out_dir": "out", "assets": assets}), &dir).unwrap();
        (dir, spec)
    }

    fn options(filters: &[&str]) -> BatchOptions {
        let mut opts = parse(&args(&["spec.json", "--quiet"])).unwrap().unwrap();
        opts.filters = args(filters);
        opts
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
    fn refuses_outputs_that_would_overwrite_inputs() {
        let dir = crate::auth::tests::temp_dir("batch-overlap");
        let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, crate::images::tests::PNG_B64).unwrap();
        std::fs::write(dir.join("hero.png"), &png).unwrap();
        let no_login = || -> Result<Credentials> { Err(Error::auth("no login")) };
        let backend = Backend::new("http://127.0.0.1:9");
        let run = |value: Value| execute(&options(&[]), &parse_spec(&value, &dir).unwrap(), &backend, &no_login);

        // Same folder for both: hero.png would be converted over its own raw image.
        let same = run(json!({"raw_dir": ".", "out_dir": ".", "assets": {"hero": {"prompt": "A hero.", "resize": "5x5"}}}));
        assert!(same.unwrap_err().message.contains("would overwrite the raw image of \"hero\""));
        assert_eq!(std::fs::read(dir.join("hero.png")).unwrap(), png, "the raw image is untouched");
        // A different format doesn't collide.
        assert_eq!(run(json!({"raw_dir": ".", "out_dir": ".", "assets": {"hero": {"prompt": "A hero.", "format": "webp"}}})).unwrap(), 0);

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
        let spec = parse_spec(&json!({"raw_dir": ".", "out_dir": ".", "assets": {"hero": {"prompt": "A hero."}}}), &dir).unwrap();
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
        let (raw, warning) = generate_asset(&spec, &spec.assets[0], &backend, &creds(), "s").unwrap();
        assert_eq!(raw, dir.join("raw/wide.png"));
        assert_eq!(warning.as_deref(), Some("asked for 16:9 but the backend returned 1x1"), "the fake backend returns a 1x1 PNG");
        let calls = captured.lock().unwrap();
        assert!(calls[0].body["prompt"].as_str().unwrap().starts_with("The frame must be in 16:9"));
        assert!(calls[0].body.get("size").is_none(), "aspect sends no size");
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
