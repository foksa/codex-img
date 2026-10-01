//! Named presets and prompt composition. A view is camera wording; a style and a character are
//! text plus optional reference images. They're defined in a batch spec, a project's
//! `codex-img.json`, the global presets file, or (views only) built in. `compose` turns a prompt
//! and the presets it names into the prompt that is sent, with every reference image labelled.
use crate::cli::{self, Aspect};
use crate::error::{Error, Result};
use crate::images::{self, MAX_EDIT_IMAGES};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub const PROJECT_FILE: &str = "codex-img.json";
const GLOBAL_FILE: &str = "presets.json";
const BUILT_IN: &str = include_str!("../skills/codex-img/scripts/views.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    View,
    Style,
    Character,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::View, Kind::Style, Kind::Character];

    pub fn name(self) -> &'static str {
        match self {
            Kind::View => "view",
            Kind::Style => "style",
            Kind::Character => "character",
        }
    }

    /// The key a preset file keeps this kind under.
    pub fn key(self) -> &'static str {
        match self {
            Kind::View => "views",
            Kind::Style => "styles",
            Kind::Character => "characters",
        }
    }

    fn parse(value: &str) -> Result<Kind> {
        Kind::ALL
            .into_iter()
            .find(|k| k.name() == value || k.key() == value)
            .ok_or_else(|| Error::usage(format!("Unknown preset kind \"{value}\"; use view, style or character.")))
    }
}

/// Where a preset was defined, in precedence order: the first layer that has a name wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Spec,
    Project,
    Global,
    BuiltIn,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Spec => "spec",
            Source::Project => "project",
            Source::Global => "global",
            Source::BuiltIn => "built-in",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub kind: Kind,
    pub name: String,
    pub text: Option<String>,
    /// Resolved against the defining file's folder.
    pub refs: Vec<PathBuf>,
    pub source: Source,
    pub file: Option<PathBuf>,
}

/// Every preset that is defined, in precedence order.
#[derive(Debug, Clone, Default)]
pub struct Library {
    presets: Vec<Preset>,
}

impl Library {
    pub fn builtin() -> Library {
        let value: Value = serde_json::from_str(BUILT_IN).expect("views.json is valid JSON");
        Library { presets: parse_presets(value.as_object().expect("views.json is an object"), Path::new(""), Source::BuiltIn, None).expect("views.json is valid") }
    }

    /// The layers that exist: a batch spec's own presets, then the project and global files.
    pub fn load(spec: Option<(&Map<String, Value>, &Path)>, project: Option<&Path>, global: Option<&Path>) -> Result<Library> {
        let mut presets = Vec::new();
        if let Some((object, base)) = spec {
            presets.extend(parse_presets(object, base, Source::Spec, None)?);
        }
        for (file, source) in [(project, Source::Project), (global, Source::Global)] {
            let Some(file) = file.filter(|f| f.is_file()) else { continue };
            let object = read_file(file)?;
            let base = file.parent().unwrap_or(Path::new(""));
            presets.extend(parse_presets(&object, base, source, Some(file)).map_err(|e| Error::usage(format!("{}: {}", file.display(), e.message)))?);
        }
        presets.extend(Library::builtin().presets);
        Ok(Library { presets })
    }

    pub fn get(&self, kind: Kind, name: &str) -> Result<&Preset> {
        self.presets.iter().find(|p| p.kind == kind && p.name == name).ok_or_else(|| {
            let known: Vec<String> = self.visible().filter(|p| p.kind == kind).map(|p| format!("{} ({})", p.name, p.source.name())).collect();
            let known = if known.is_empty() { "none are defined".to_string() } else { format!("defined: {}", known.join(", ")) };
            Error::usage(format!("Unknown {} \"{name}\"; {known}. `codex-img presets` lists them.", kind.name()))
        })
    }

    /// Presets no earlier layer hides.
    fn visible(&self) -> impl Iterator<Item = &Preset> {
        self.presets.iter().enumerate().filter(|(i, p)| !self.presets[..*i].iter().any(|q| q.kind == p.kind && q.name == p.name)).map(|(_, p)| p)
    }

    /// Every preset, with the source of the one hiding it, if any.
    pub fn listing(&self) -> Vec<(&Preset, Option<Source>)> {
        self.presets
            .iter()
            .enumerate()
            .map(|(i, p)| (p, self.presets[..i].iter().find(|q| q.kind == p.kind && q.name == p.name).map(|q| q.source)))
            .collect()
    }
}

fn read_file(file: &Path) -> Result<Map<String, Value>> {
    let text = std::fs::read_to_string(file).map_err(|e| Error::usage(format!("Unable to read {}: {e}", file.display())))?;
    match serde_json::from_str(&text) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err(Error::usage(format!("{} must be a JSON object.", file.display()))),
        Err(e) => Err(Error::usage(format!("{} is not valid JSON: {e}", file.display()))),
    }
}

/// The `views`, `styles` and `characters` of a preset file or spec; other keys are left to the caller.
fn parse_presets(object: &Map<String, Value>, base: &Path, source: Source, file: Option<&Path>) -> Result<Vec<Preset>> {
    if source != Source::Spec {
        if let Some(field) = object.keys().find(|k| !Kind::ALL.iter().any(|kind| kind.key() == k.as_str())) {
            return Err(Error::usage(format!("unknown field \"{field}\"; a preset file has views, styles and characters.")));
        }
    }
    let mut presets = Vec::new();
    for kind in Kind::ALL {
        let Some(entries) = object.get(kind.key()) else { continue };
        let entries = entries.as_object().ok_or_else(|| Error::usage(format!("{} must be an object of name -> preset.", kind.key())))?;
        for (name, entry) in entries {
            let context = |e: Error| Error::usage(format!("{}.\"{name}\": {}", kind.key(), e.message));
            check_name(name).map_err(context)?;
            let (text, refs) = parse_entry(kind, entry).map_err(context)?;
            presets.push(Preset { kind, name: name.clone(), text, refs: refs.iter().map(|r| base.join(r)).collect(), source, file: file.map(Path::to_path_buf) });
        }
    }
    Ok(presets)
}

fn parse_entry(kind: Kind, entry: &Value) -> Result<(Option<String>, Vec<String>)> {
    let (text, refs) = match entry {
        Value::String(text) => (Some(text.clone()), Vec::new()),
        Value::Object(fields) => {
            if let Some(field) = fields.keys().find(|k| !["text", "refs"].contains(&k.as_str())) {
                return Err(Error::usage(format!("unknown field \"{field}\"; a preset has text and refs.")));
            }
            let text = match fields.get("text") {
                None | Some(Value::Null) => None,
                Some(Value::String(t)) => Some(t.clone()),
                Some(_) => return Err(Error::usage("text must be a string.")),
            };
            let wrong = || Error::usage("refs must be a string or a list of strings.");
            let refs = match fields.get("refs") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::String(r)) => vec![r.clone()],
                Some(Value::Array(items)) => items.iter().map(|v| v.as_str().map(str::to_string).ok_or_else(wrong)).collect::<Result<_>>()?,
                Some(_) => return Err(wrong()),
            };
            (text, refs)
        }
        _ => return Err(Error::usage("must be a string (its text) or an object with text and refs.")),
    };
    let text = text.filter(|t| !t.trim().is_empty());
    if kind == Kind::View && !refs.is_empty() {
        return Err(Error::usage("a view is text only; use a composition reference for an example image."));
    }
    if text.is_none() && refs.is_empty() {
        return Err(Error::usage("needs text, refs or both."));
    }
    if refs.len() > MAX_EDIT_IMAGES {
        return Err(Error::usage(format!("at most {MAX_EDIT_IMAGES} refs.")));
    }
    Ok((text, refs))
}

fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !ok {
        return Err(Error::usage("a preset name is letters, digits, - and _ (at most 64)."));
    }
    Ok(())
}

/// The nearest `codex-img.json` in `start` or a folder above it.
pub fn find_project(start: &Path) -> Option<PathBuf> {
    let start = std::path::absolute(start).unwrap_or_else(|_| start.to_path_buf());
    start.ancestors().map(|dir| dir.join(PROJECT_FILE)).find(|f| f.is_file())
}

/// `$XDG_CONFIG_HOME/codex-img`, else `~/.config/codex-img`, else `%APPDATA%\codex-img`.
pub fn global_dir() -> Option<PathBuf> {
    let var = |name| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    var("XDG_CONFIG_HOME")
        .or_else(|| var("HOME").map(|h| h.join(".config")))
        .or_else(|| var("APPDATA"))
        .map(|dir| dir.join("codex-img"))
}

/// What a generation asks for, before presets are resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Setup {
    pub aspect: Option<Aspect>,
    pub view: Option<String>,
    pub styles: Vec<String>,
    pub characters: Vec<String>,
    pub style_refs: Vec<PathBuf>,
    pub character_refs: Vec<PathBuf>,
    pub composition_refs: Vec<PathBuf>,
}

impl Setup {
    /// Whether a preset is named, so the preset files must be read.
    pub fn names_presets(&self) -> bool {
        self.view.is_some() || !self.styles.is_empty() || !self.characters.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Style,
    Character,
    Composition,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Style => "style",
            Role::Character => "character",
            Role::Composition => "composition",
        }
    }
}

/// A reference image `compose` labelled; it follows the plain input images.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleImage {
    pub path: PathBuf,
    pub role: Role,
    /// The character preset it came from.
    pub character: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composed {
    pub prompt: String,
    /// In request order, after the `inputs` plain images.
    pub images: Vec<RoleImage>,
    /// Each named preset and where it was defined, for the manifest.
    pub used: Vec<(Kind, String, Source)>,
}

/// The prompt that is sent: the frame and camera first, then a label for each reference image and
/// each character, then `prompt`, then the style presets' text and `style` (a batch spec's own).
/// `inputs` plain images (-i, batch references) come first and keep their numbers.
pub fn compose(prompt: &str, style: Option<&str>, setup: &Setup, library: &Library, inputs: usize) -> Result<Composed> {
    let mut used = Vec::new();
    let mut lookup = |kind: Kind, name: &str| -> Result<Preset> {
        let preset = library.get(kind, name)?.clone();
        used.push((kind, preset.name.clone(), preset.source));
        Ok(preset)
    };
    let view = setup.view.as_deref().map(|name| lookup(Kind::View, name)).transpose()?;
    let styles = setup.styles.iter().map(|name| lookup(Kind::Style, name)).collect::<Result<Vec<_>>>()?;
    let characters = setup.characters.iter().map(|name| lookup(Kind::Character, name)).collect::<Result<Vec<_>>>()?;

    let mut images = Vec::new();
    let mut add = |paths: &[PathBuf], role: Role, character: Option<&str>| {
        images.extend(paths.iter().map(|path| RoleImage { path: path.clone(), role, character: character.map(str::to_string) }));
    };
    for preset in &styles {
        add(&preset.refs, Role::Style, None);
    }
    add(&setup.style_refs, Role::Style, None);
    for preset in &characters {
        add(&preset.refs, Role::Character, Some(&preset.name));
    }
    add(&setup.character_refs, Role::Character, None);
    add(&setup.composition_refs, Role::Composition, None);
    if inputs + images.len() > MAX_EDIT_IMAGES {
        return Err(Error::usage(format!(
            "At most {MAX_EDIT_IMAGES} images in total; this asks for {inputs} input image(s) and {} reference image(s) from refs and presets.",
            images.len()
        )));
    }

    let mut blocks = Vec::new();
    if let Some(aspect) = setup.aspect {
        blocks.push(aspect.sentence());
    }
    if let Some(text) = view.and_then(|v| v.text) {
        blocks.push(format!("Camera: {text}"));
    }
    let labels: Vec<String> = images.iter().enumerate().map(|(i, image)| label(inputs + i + 1, image)).collect();
    if !labels.is_empty() {
        blocks.push(labels.join("\n"));
    }
    let described: Vec<String> = characters.iter().filter_map(|c| c.text.as_ref().map(|t| format!("The character \"{}\": {t}", c.name))).collect();
    if !described.is_empty() {
        blocks.push(described.join("\n"));
    }
    let mut body = prompt.trim_end().to_string();
    for text in styles.iter().filter_map(|s| s.text.as_deref()).chain(style) {
        body = format!("{body} {text}");
    }
    blocks.push(body);
    Ok(Composed { prompt: blocks.join("\n\n"), images, used })
}

fn label(n: usize, image: &RoleImage) -> String {
    match (image.role, &image.character) {
        (Role::Style, _) => format!("Image {n}: style reference only: take its palette, rendering and line work, not its subject or layout."),
        (Role::Character, Some(name)) => {
            format!("Image {n}: character reference for \"{name}\": keep the same character (face, proportions, outfit, colours) in a new pose and scene.")
        }
        (Role::Character, None) => format!("Image {n}: character reference: keep the same character (face, proportions, outfit, colours) in a new pose and scene."),
        (Role::Composition, _) => format!("Image {n}: composition reference only: follow its layout and framing, not its subject or style."),
    }
}

// --- The `presets` subcommand ---

pub fn help() -> &'static str {
    r#"Usage:
  codex-img presets [--json]                        List every view, style and character
  codex-img presets show <kind> <name> [--json]     One preset: text, refs and where it's defined
  codex-img presets add <kind> <name> [--text <t>] [--ref <image>]... [--from <image>]
                    [--global] [--force]
  codex-img presets remove <kind> <name> [--global]
  codex-img presets promote <kind> <name> [--force] Copy a project preset to the global file

<kind> is view, style or character. Uses no quota.

Presets are used with --view, --style and --character (and the batch fields of the
same names). The first of these that defines a name wins:
  a batch spec's own "views", "styles" and "characters"
  the project: the nearest codex-img.json in this folder or one above it
  global: $XDG_CONFIG_HOME/codex-img/presets.json (else ~/.config/codex-img/)
  built in: the views side, front, top-down, three-quarter and isometric

add writes to the project's codex-img.json (created here if there's none), or with
--global to the global file. A view is text only; a style or character can have up to
5 reference images. A --ref inside the project is stored as a path relative to
codex-img.json; one outside it is copied to presets/<kind>/<name>/ beside it. Global
refs are always copied, to refs/<kind>/<name>/ in the global folder.
--from <image> is the same as --ref <image>. Give --text only what defines the
preset (a character's look, not a pose or background): the text is added to every
prompt that uses it.

File format (a string is short for {"text": ...}; refs are relative to the file):
  {
    "views": {"roadside": "seen straight on from the side at eye level, ..."},
    "styles": {"harbor": {"text": "16-bit pixel art, ...", "refs": ["refs/boat.png"]}},
    "characters": {"captain": {"text": "a stocky walrus sailor", "refs": ["captain.png"]}}
  }"#
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    List,
    Show { kind: Kind, name: String },
    Add { kind: Kind, name: String, text: Option<String>, refs: Vec<String>, from: Option<String>, global: bool, force: bool },
    Remove { kind: Kind, name: String, global: bool },
    Promote { kind: Kind, name: String, force: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetsOptions {
    pub action: Action,
    pub json: bool,
}

pub fn parse(args: &[String]) -> Result<Option<PresetsOptions>> {
    let (mut positionals, mut refs) = (Vec::new(), Vec::new());
    let (mut text, mut from) = (None, None);
    let (mut global, mut force, mut json) = (false, false, false);
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if !arg.starts_with('-') {
            positionals.push(arg.clone());
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if arg.starts_with("--") => (n, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| iter.next().cloned()).ok_or_else(|| Error::usage(format!("{name} needs a value.")));
        match name {
            "--text" => text = Some(value()?),
            "--ref" => refs.push(value()?),
            "--from" => from = Some(value()?),
            "--global" => global = true,
            "--force" => force = true,
            "--json" => json = true,
            "-h" | "--help" => return Ok(None),
            _ => return Err(Error::usage(format!("Unknown presets option: {arg}"))),
        }
    }
    let mut words = positionals.into_iter();
    let verb = words.next();
    let mut target = || -> Result<(Kind, String)> {
        let usage = || Error::usage("Name the preset: <kind> <name>, e.g. style harbor.");
        let kind = Kind::parse(&words.next().ok_or_else(usage)?)?;
        let name = words.next().ok_or_else(usage)?;
        check_name(&name)?;
        Ok((kind, name))
    };
    let action = match verb.as_deref() {
        None | Some("list") => Action::List,
        Some("show") => {
            let (kind, name) = target()?;
            Action::Show { kind, name }
        }
        Some("add") => {
            let (kind, name) = target()?;
            if text.is_none() && refs.is_empty() && from.is_none() {
                return Err(Error::usage("presets add needs --text, --ref or --from."));
            }
            Action::Add { kind, name, text, refs, from, global, force }
        }
        Some("remove") => {
            let (kind, name) = target()?;
            Action::Remove { kind, name, global }
        }
        Some("promote") => {
            let (kind, name) = target()?;
            Action::Promote { kind, name, force }
        }
        Some(other) => return Err(Error::usage(format!("Unknown presets command \"{other}\"; use list, show, add, remove or promote."))),
    };
    if let Some(extra) = words.next() {
        return Err(Error::usage(format!("Unexpected argument: {extra}")));
    }
    Ok(Some(PresetsOptions { action, json }))
}

/// Where the preset files are; taken from the environment only in `run`.
pub struct Places {
    pub cwd: PathBuf,
    pub global_dir: Option<PathBuf>,
}

impl Places {
    fn project(&self) -> Option<PathBuf> {
        find_project(&self.cwd)
    }

    fn global_file(&self) -> Result<PathBuf> {
        self.global_dir.as_ref().map(|d| d.join(GLOBAL_FILE)).ok_or_else(|| Error::other("No folder for global presets: set XDG_CONFIG_HOME or HOME."))
    }

    pub fn library(&self) -> Result<Library> {
        let global = self.global_dir.as_ref().map(|d| d.join(GLOBAL_FILE));
        Library::load(None, self.project().as_deref(), global.as_deref())
    }
}

pub fn run(opts: &PresetsOptions) -> Result<i32> {
    let cwd = std::env::current_dir().map_err(|e| Error::other(format!("No current folder: {e}")))?;
    execute(opts, &Places { cwd, global_dir: global_dir() })
}

fn execute(opts: &PresetsOptions, places: &Places) -> Result<i32> {
    match &opts.action {
        Action::List => {
            let library = places.library()?;
            let listing = library.listing();
            if opts.json {
                for (preset, hidden_by) in listing {
                    println!("{}", describe(preset, hidden_by));
                }
            } else {
                for kind in Kind::ALL {
                    println!("{}:", kind.key());
                    for (preset, hidden_by) in listing.iter().filter(|(p, _)| p.kind == kind) {
                        let refs = if preset.refs.is_empty() { String::new() } else { format!(" [{} ref(s)]", preset.refs.len()) };
                        let hidden = hidden_by.map(|s| format!(" (hidden by the {} one)", s.name())).unwrap_or_default();
                        let text = preset.text.as_deref().map(|t| shorten(t, 70)).unwrap_or_default();
                        println!("  {:<16} {:<9} {text}{refs}{hidden}", preset.name, preset.source.name());
                    }
                }
            }
        }
        Action::Show { kind, name } => {
            let library = places.library()?;
            let preset = library.get(*kind, name)?;
            if opts.json {
                println!("{}", describe(preset, None));
            } else {
                println!("{} \"{}\" ({}{})", kind.name(), preset.name, preset.source.name(), preset.file.as_ref().map(|f| format!(", {}", f.display())).unwrap_or_default());
                if let Some(text) = &preset.text {
                    println!("text: {text}");
                }
                for r in &preset.refs {
                    println!("ref:  {}{}", r.display(), if r.is_file() { "" } else { "  (missing)" });
                }
            }
        }
        Action::Add { kind, name, text, refs, from, global, force } => {
            // The image only, not the prompt it was generated from: that describes a whole image
            // ("isolated on a transparent background"), and leaked into every scene in tests.
            let mut refs: Vec<PathBuf> = refs.iter().map(PathBuf::from).collect();
            if let Some(from) = from {
                refs.insert(0, PathBuf::from(from));
            }
            let text = text.clone();
            let file = if *global { places.global_file()? } else { places.project().unwrap_or_else(|| places.cwd.join(PROJECT_FILE)) };
            let stored = add(&file, *kind, name, text, &refs, *global, *force)?;
            report(opts.json, "added", *kind, name, &file, &stored);
        }
        Action::Remove { kind, name, global } => {
            let file = if *global { places.global_file()? } else { places.project().ok_or_else(|| Error::usage(format!("No {PROJECT_FILE} here or in a folder above.")))? };
            remove(&file, *kind, name)?;
            report(opts.json, "removed", *kind, name, &file, &[]);
        }
        Action::Promote { kind, name, force } => {
            let project = places.project().ok_or_else(|| Error::usage(format!("No {PROJECT_FILE} here or in a folder above.")))?;
            let preset = Library::load(None, Some(&project), None)?
                .presets
                .into_iter()
                .find(|p| p.kind == *kind && p.name == *name && p.source == Source::Project)
                .ok_or_else(|| Error::usage(format!("{} has no {} \"{name}\".", project.display(), kind.name())))?;
            let file = places.global_file()?;
            let stored = add(&file, *kind, name, preset.text, &preset.refs, true, *force)?;
            report(opts.json, "promoted", *kind, name, &file, &stored);
        }
    }
    Ok(0)
}

fn describe(preset: &Preset, hidden_by: Option<Source>) -> Value {
    let mut out = json!({"kind": preset.kind.name(), "name": preset.name, "source": preset.source.name()});
    if let Some(file) = &preset.file {
        out["file"] = json!(file.display().to_string());
    }
    if let Some(text) = &preset.text {
        out["text"] = json!(text);
    }
    if !preset.refs.is_empty() {
        out["refs"] = json!(preset.refs.iter().map(|r| r.display().to_string()).collect::<Vec<_>>());
    }
    if let Some(source) = hidden_by {
        out["hiddenBy"] = json!(source.name());
    }
    out
}

fn report(json: bool, done: &str, kind: Kind, name: &str, file: &Path, refs: &[String]) {
    if json {
        println!("{}", json!({"ok": true, "action": done, "kind": kind.name(), "name": name, "file": file.display().to_string(), "refs": refs}));
    } else {
        let refs = if refs.is_empty() { String::new() } else { format!(" with {} ref(s)", refs.len()) };
        println!("{done} {} \"{name}\"{refs}: {}", kind.name(), file.display());
    }
}

fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}...", text.chars().take(max - 3).collect::<String>().trim_end())
}

/// The folder `add` copies refs into, and `remove` deletes: it belongs to the preset.
fn owned_dir(file: &Path, kind: Kind, name: &str, global: bool) -> PathBuf {
    let base = file.parent().unwrap_or(Path::new(""));
    base.join(if global { "refs" } else { "presets" }).join(kind.key()).join(name)
}

/// Write `name` into `file`, storing `refs` as paths relative to it: a project ref inside the
/// project stays where it is, anything else is copied into the preset's own folder. Returns the
/// stored ref paths.
fn add(file: &Path, kind: Kind, name: &str, text: Option<String>, refs: &[PathBuf], global: bool, force: bool) -> Result<Vec<String>> {
    let mut object = if file.is_file() { read_file(file)? } else { Map::new() };
    let entries = object.entry(kind.key()).or_insert_with(|| json!({}));
    let entries = entries.as_object_mut().ok_or_else(|| Error::usage(format!("{}: {} must be an object.", file.display(), kind.key())))?;
    if entries.contains_key(name) && !force {
        return Err(Error::usage(format!("{} already has a {} \"{name}\"; add --force to replace it.", file.display(), kind.name())));
    }
    for r in refs {
        images::load_input_images(&[r.display().to_string()])?;
    }
    let text = text.filter(|t| !t.trim().is_empty());
    // Checked as the file would be read back, before anything is copied.
    let refs_value: Vec<String> = refs.iter().map(|r| r.display().to_string()).collect();
    parse_entry(kind, &json!({"text": text, "refs": refs_value}))?;

    let base = std::path::absolute(file.parent().unwrap_or(Path::new(""))).map_err(|e| Error::other(e.to_string()))?;
    let owned = owned_dir(&base.join(PROJECT_FILE), kind, name, global);
    let mut stored = Vec::new();
    let mut copies = Vec::new();
    for (i, r) in refs.iter().enumerate() {
        let inside = (!global).then(|| relative_inside(r, &base)).flatten().filter(|p| !p.starts_with(owned.strip_prefix(&base).unwrap_or(&owned)));
        match inside {
            Some(relative) => stored.push(slashes(&relative)),
            None => {
                let file_name = r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "ref.png".into());
                let target = owned.join(format!("{}-{file_name}", i + 1));
                stored.push(slashes(target.strip_prefix(&base).unwrap_or(&target)));
                copies.push((r.clone(), target));
            }
        }
    }
    // Replacing a preset replaces the copies it owned; read them first, since a new ref may be one.
    let bytes: Vec<(PathBuf, Vec<u8>)> =
        copies.into_iter().map(|(from, to)| std::fs::read(&from).map(|b| (to, b)).map_err(|e| Error::other(format!("Unable to read {}: {e}", from.display())))).collect::<Result<_>>()?;
    if owned.is_dir() {
        std::fs::remove_dir_all(&owned).map_err(|e| Error::other(format!("Could not replace {}: {e}", owned.display())))?;
    }
    for (to, data) in &bytes {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::other(format!("Could not create {}: {e}", parent.display())))?;
        }
        cli::write_output(to, data, true)?;
    }
    let entry = match (&text, stored.is_empty()) {
        (Some(text), true) => json!(text),
        _ => {
            let mut fields = Map::new();
            if let Some(text) = &text {
                fields.insert("text".into(), json!(text));
            }
            fields.insert("refs".into(), json!(stored));
            Value::Object(fields)
        }
    };
    entries.insert(name.to_string(), entry);
    write_file(file, &object)?;
    Ok(stored)
}

fn remove(file: &Path, kind: Kind, name: &str) -> Result<()> {
    let mut object = if file.is_file() { read_file(file)? } else { Map::new() };
    let removed = object.get_mut(kind.key()).and_then(Value::as_object_mut).and_then(|entries| entries.remove(name));
    if removed.is_none() {
        return Err(Error::usage(format!("{} has no {} \"{name}\".", file.display(), kind.name())));
    }
    write_file(file, &object)?;
    let global = file.file_name().is_some_and(|n| n == GLOBAL_FILE);
    let owned = owned_dir(file, kind, name, global);
    if owned.is_dir() {
        std::fs::remove_dir_all(&owned).map_err(|e| Error::other(format!("Removed the preset, but not {}: {e}", owned.display())))?;
    }
    Ok(())
}

fn write_file(file: &Path, object: &Map<String, Value>) -> Result<()> {
    if let Some(parent) = file.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| Error::other(format!("Could not create {}: {e}", parent.display())))?;
    }
    let text = serde_json::to_string_pretty(object).map_err(|e| Error::other(e.to_string()))? + "\n";
    cli::write_output(file, text.as_bytes(), true).map(|_| ())
}

/// `path` relative to `base`, if it's inside it.
fn relative_inside(path: &Path, base: &Path) -> Option<PathBuf> {
    let path = std::fs::canonicalize(path).ok()?;
    let base = std::fs::canonicalize(base).ok()?;
    path.strip_prefix(&base).ok().map(Path::to_path_buf)
}

/// A stored path, with / on every platform so the file works everywhere.
fn slashes(path: &Path) -> String {
    path.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::tests::temp_dir;

    const PNG: &str = crate::images::tests::PNG_B64;

    fn png(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, base64::Engine::decode(&base64::engine::general_purpose::STANDARD, PNG).unwrap()).unwrap();
    }

    fn write(path: &Path, value: Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, value.to_string()).unwrap();
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn layers_resolve_in_order_and_report_what_they_hide() {
        let dir = temp_dir("presets-layers");
        let (project, global) = (dir.join("game/codex-img.json"), dir.join("config/presets.json"));
        write(&project, json!({"views": {"side": "project side"}, "styles": {"pixel": {"text": "project pixel", "refs": ["refs/a.png"]}}}));
        write(&global, json!({"styles": {"pixel": "global pixel", "ink": "global ink"}}));
        let spec = json!({"styles": {"ink": "spec ink"}});
        let library = Library::load(Some((spec.as_object().unwrap(), Path::new("/art"))), Some(&project), Some(&global)).unwrap();
        assert_eq!(library.get(Kind::View, "side").unwrap().text.as_deref(), Some("project side"));
        assert_eq!(library.get(Kind::View, "front").unwrap().source, Source::BuiltIn);
        let pixel = library.get(Kind::Style, "pixel").unwrap();
        assert_eq!((pixel.source, pixel.refs.clone()), (Source::Project, vec![dir.join("game/refs/a.png")]));
        assert_eq!(library.get(Kind::Style, "ink").unwrap().source, Source::Spec);
        let hidden: Vec<(&str, Source)> = library.listing().into_iter().filter_map(|(p, by)| by.map(|by| (p.name.as_str(), by))).collect();
        assert_eq!(hidden, [("pixel", Source::Project), ("ink", Source::Spec), ("side", Source::Project)]);
        let unknown = library.get(Kind::Character, "captain").unwrap_err().message;
        assert!(unknown.contains("Unknown character \"captain\"; none are defined"), "{unknown}");
        assert!(library.get(Kind::Style, "oil").unwrap_err().message.contains("defined: ink (spec), pixel (project)"));

        std::fs::create_dir_all(dir.join("game/art/raw")).unwrap();
        assert_eq!(find_project(&dir.join("game/art/raw")), Some(project.clone()));

        write(&project, json!({"views": {"side": {"text": "x", "refs": ["a.png"]}}}));
        assert!(Library::load(None, Some(&project), None).unwrap_err().message.contains("text only"));
        write(&project, json!({"view": {}}));
        assert!(Library::load(None, Some(&project), None).unwrap_err().message.contains("unknown field \"view\""));
        write(&project, json!({"styles": {"bad name": "x"}}));
        assert!(Library::load(None, Some(&project), None).unwrap_err().message.contains("letters, digits"));
    }

    #[test]
    fn compose_orders_the_frame_labels_prompt_and_style() {
        let spec = json!({
            "styles": {"pixel": {"text": "16-bit pixel art.", "refs": ["pixel.png"]}},
            "characters": {"captain": {"text": "a stocky walrus sailor in a yellow raincoat", "refs": ["captain.png"]}}
        });
        let library = Library::load(Some((spec.as_object().unwrap(), Path::new("/art"))), None, None).unwrap();
        let setup = Setup {
            aspect: Some(Aspect { width: 3, height: 2 }),
            view: Some("side".into()),
            styles: vec!["pixel".into()],
            characters: vec!["captain".into()],
            composition_refs: vec![PathBuf::from("layout.png")],
            ..Setup::default()
        };
        let composed = compose("The captain waves from a pier.", Some("No text."), &setup, &library, 1).unwrap();
        let view = Library::builtin().get(Kind::View, "side").unwrap().text.clone().unwrap();
        assert_eq!(
            composed.prompt,
            format!(
                "The frame must be in 3:2 landscape format, wider than it is tall.\n\nCamera: {view}\n\n\
                 Image 2: style reference only: take its palette, rendering and line work, not its subject or layout.\n\
                 Image 3: character reference for \"captain\": keep the same character (face, proportions, outfit, colours) in a new pose and scene.\n\
                 Image 4: composition reference only: follow its layout and framing, not its subject or style.\n\n\
                 The character \"captain\": a stocky walrus sailor in a yellow raincoat\n\n\
                 The captain waves from a pier. 16-bit pixel art. No text."
            )
        );
        let paths: Vec<&Path> = composed.images.iter().map(|i| i.path.as_path()).collect();
        assert_eq!(paths, [Path::new("/art/pixel.png"), Path::new("/art/captain.png"), Path::new("layout.png")]);
        assert_eq!(composed.used, [(Kind::View, "side".into(), Source::BuiltIn), (Kind::Style, "pixel".into(), Source::Spec), (Kind::Character, "captain".into(), Source::Spec)]);

        // Nothing asked for: the prompt is untouched.
        assert_eq!(compose("A fox.", None, &Setup::default(), &Library::builtin(), 0).unwrap().prompt, "A fox.");
        let many = Setup { style_refs: vec![PathBuf::from("s.png"); 3], ..Setup::default() };
        assert!(compose("x", None, &many, &Library::builtin(), 3).unwrap_err().message.contains("At most 5 images"));
        assert!(compose("x", None, &Setup { view: Some("diagonal".into()), ..Setup::default() }, &Library::builtin(), 0).unwrap_err().message.contains("side (built-in)"));
    }

    #[test]
    fn parses_commands() {
        let parsed = |list: &[&str]| parse(&args(list)).unwrap().unwrap();
        assert_eq!(parsed(&[]).action, Action::List);
        assert!(parsed(&["--json"]).json);
        assert_eq!(parsed(&["show", "view", "side"]).action, Action::Show { kind: Kind::View, name: "side".into() });
        assert_eq!(
            parsed(&["add", "characters", "cap", "--ref", "a.png", "--ref=b.png", "--text", "a walrus", "--global"]).action,
            Action::Add { kind: Kind::Character, name: "cap".into(), text: Some("a walrus".into()), refs: args(&["a.png", "b.png"]), from: None, global: true, force: false }
        );
        assert_eq!(parse(&args(&["--help"])).unwrap(), None);
        let usage = |list: &[&str]| parse(&args(list)).unwrap_err().message;
        assert!(usage(&["add", "style", "x"]).contains("--text, --ref or --from"));
        assert!(usage(&["add", "colour", "x", "--text", "y"]).contains("view, style or character"));
        assert!(usage(&["show", "style"]).contains("<kind> <name>"));
        assert!(usage(&["rename"]).contains("Unknown presets command"));
        assert!(usage(&["list", "extra"]).contains("Unexpected"));
    }

    #[test]
    fn add_remove_and_promote_keep_refs_with_their_preset() {
        let dir = temp_dir("presets-add");
        let project_dir = dir.join("game");
        let places = Places { cwd: project_dir.clone(), global_dir: Some(dir.join("config")) };
        png(&project_dir.join("art/raw/captain.png"));
        png(&dir.join("downloads/style.png"));
        let run = |list: &[&str]| execute(&parse(&args(list)).unwrap().unwrap(), &places);

        // A hand-written file keeps its other entries and order.
        write(&project_dir.join(PROJECT_FILE), json!({"views": {"roadside": "seen from the road"}}));
        run(&["add", "character", "captain", "--ref", project_dir.join("art/raw/captain.png").to_str().unwrap(), "--text", "a walrus"]).unwrap();
        run(&["add", "style", "ink", "--ref", dir.join("downloads/style.png").to_str().unwrap()]).unwrap();
        let file = read_file(&project_dir.join(PROJECT_FILE)).unwrap();
        assert_eq!(Value::Object(file.clone())["characters"]["captain"], json!({"text": "a walrus", "refs": ["art/raw/captain.png"]}), "inside the project: no copy");
        assert_eq!(Value::Object(file.clone())["styles"]["ink"], json!({"refs": ["presets/styles/ink/1-style.png"]}), "outside: copied beside it");
        assert_eq!(file.keys().collect::<Vec<_>>(), ["views", "characters", "styles"]);
        assert!(project_dir.join("presets/styles/ink/1-style.png").is_file());
        assert!(run(&["add", "style", "ink", "--text", "x"]).unwrap_err().message.contains("--force"));

        // Promote copies even project refs, so the global preset doesn't depend on the project.
        run(&["promote", "character", "captain"]).unwrap();
        let global = read_file(&dir.join("config/presets.json")).unwrap();
        assert_eq!(Value::Object(global)["characters"]["captain"], json!({"text": "a walrus", "refs": ["refs/characters/captain/1-captain.png"]}));
        assert!(dir.join("config/refs/characters/captain/1-captain.png").is_file());
        assert!(run(&["promote", "character", "captain"]).unwrap_err().message.contains("--force"));
        run(&["promote", "character", "captain", "--force"]).unwrap();
        assert!(run(&["promote", "style", "oil"]).unwrap_err().message.contains("no style \"oil\""));

        // Remove deletes only the folder add made.
        run(&["remove", "style", "ink"]).unwrap();
        run(&["remove", "character", "captain"]).unwrap();
        assert!(!project_dir.join("presets/styles/ink").exists());
        assert!(project_dir.join("art/raw/captain.png").is_file(), "a project ref is never deleted");
        run(&["remove", "character", "captain", "--global"]).unwrap();
        assert!(!dir.join("config/refs/characters/captain").exists());
        assert!(run(&["remove", "view", "nope"]).unwrap_err().message.contains("no view \"nope\""));

        // --from is the image only; its manifest's prompt describes a whole image, not the preset.
        crate::manifest::write(&crate::manifest::path_for(&project_dir.join("art/raw/captain.png")), &json!({"prompt": "sent", "userPrompt": "isolated on white"})).unwrap();
        run(&["add", "character", "cap", "--from", project_dir.join("art/raw/captain.png").to_str().unwrap()]).unwrap();
        let file = read_file(&project_dir.join(PROJECT_FILE)).unwrap();
        assert_eq!(Value::Object(file)["characters"]["cap"], json!({"refs": ["art/raw/captain.png"]}));
        assert!(run(&["add", "view", "v", "--ref", project_dir.join("art/raw/captain.png").to_str().unwrap()]).unwrap_err().message.contains("text only"));
        assert!(run(&["add", "style", "s", "--ref", "missing.png"]).unwrap_err().message.contains("missing.png"));
    }
}
