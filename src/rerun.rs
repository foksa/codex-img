//! Replay the submitted request, rather than resolving today's versions of its presets.
use crate::{cli::{self, Command}, error::{Error, Result}, manifest, presets::{Kind, Source}, project};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options { pub image: String, pub count: usize, pub output: Option<String>, pub anyway: bool, pub json: bool, pub quiet: bool }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved { pub prompt: String, pub user_prompt: String, pub inputs: Vec<manifest::Input>, pub used: Vec<(Kind, String, Source)>, pub tile: Option<Tile>, pub allow_changed: bool, pub setup: Option<Value> }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile { pub prompt: Option<String>, pub keep_edit: bool, pub preview: bool, pub edit_only: bool }
pub fn help() -> &'static str {
    "Usage: codex-img rerun <image> [-n N] [-o out] [--anyway] [--json] [--quiet]\n\nGenerate again from <image>.json, with its submitted prompt, references, request\nand conversion settings. Uses N images of quota (default 1). The new images get\nmanifests and link to the original as parent. Existing images are never replaced.\n\nChanged references stop before quota; --anyway permits them and warns for each.\nMissing references always stop. Use edit-and-run with a new reference selection\nto omit one. Older manifests replay available settings with a conversion warning."
}
pub fn parse(args: &[String]) -> Result<Option<Options>> {
    let mut opts = Options { image: String::new(), count: 1, output: None, anyway: false, json: false, quiet: false };
    let mut iter = args.iter(); let mut positional = false;
    while let Some(arg) = iter.next() {
        if positional || !arg.starts_with('-') {
            if !opts.image.is_empty() { return Err(Error::usage("rerun takes one image.")); }
            opts.image = arg.clone(); continue;
        }
        if arg == "--" { positional = true; continue; }
        let (flag, inline) = arg.split_once('=').map_or((arg.as_str(), None), |(flag, value)| (flag, Some(value.to_string())));
        let mut value = || inline.clone().or_else(|| iter.next().cloned()).ok_or_else(|| Error::usage(format!("{flag} needs a value.")));
        match flag {
            "-n" | "--count" => opts.count = value()?.parse().ok().filter(|n| (1..=10).contains(n)).ok_or_else(|| Error::usage("--count must be an integer from 1 to 10."))?,
            "-o" | "--output" => opts.output = Some(value()?),
            "--anyway" if inline.is_none() => opts.anyway = true,
            "--json" if inline.is_none() => opts.json = true,
            "--quiet" if inline.is_none() => opts.quiet = true,
            "-h" | "--help" => return Ok(None),
            _ => return Err(Error::usage(format!("Unknown rerun option: {arg}"))),
        }
    }
    if opts.image.is_empty() { return Err(Error::usage("rerun needs an image with a manifest.")); }
    Ok(Some(opts))
}
pub fn prepare(opts: &Options, cwd: &Path) -> Result<cli::Options> {
    let image = crate::batch::resolved(&cwd.join(&opts.image));
    let path = manifest::path_for(&image);
    let text = std::fs::read_to_string(&path).map_err(|e| Error::usage(format!("Could not read {}: {e}", path.display())))?;
    let note: Value = serde_json::from_str(&text).map_err(|e| Error::usage(format!("{}: {e}", path.display())))?;
    if !note.is_object() { return Err(Error::usage("The manifest must be a JSON object.")); }
    let base = note["root"].as_str().map(PathBuf::from).or_else(|| project::root(image.parent().unwrap_or(cwd)))
        .or_else(|| project::root(cwd)).unwrap_or_else(|| cwd.to_path_buf());
    let required = |key: &str| note[key].as_str().map(str::to_string).ok_or_else(|| Error::usage(format!("The manifest needs a {key} string.")));
    let prompt = required("prompt")?;
    let user_prompt = note["userPrompt"].as_str().unwrap_or(&prompt).to_string();
    let mut args = vec!["recorded request".to_string(), "--manifest".into()];
    if let Some(conversion) = note.get("conversion") {
        let format = conversion["format"].as_str().ok_or_else(|| Error::usage("The manifest's conversion needs format."))?;
        args.push(format!("--format={format}"));
        let list = conversion["args"].as_array().ok_or_else(|| Error::usage("The manifest's conversion needs an args list."))?;
        for arg in list {
            let arg = arg.as_str().filter(|arg| crate::conversion_record::allowed(arg)).ok_or_else(|| Error::usage("The manifest has an invalid conversion option."))?;
            args.push(arg.into());
        }
    } else {
        eprintln!("codex-img: warning: this older manifest has no conversion settings; replaying its recorded request with default conversion. Use convert to redo conversion afterwards.");
        let format = image.extension().and_then(|e| e.to_str()).and_then(crate::images::Format::parse).unwrap_or(crate::images::Format::Png);
        args.push(format!("--format={}", format.name()));
    }
    let request = note.get("request").and_then(Value::as_object).ok_or_else(|| Error::usage("The manifest needs request settings."))?;
    for key in ["aspect", "size", "quality", "background"] {
        if let Some(value) = request.get(key).filter(|v| !v.is_null()) {
            let value = value.as_str().ok_or_else(|| Error::usage(format!("request.{key} must be a string.")))?;
            args.push(format!("--{key}={value}"));
        }
    }
    match note["transport"].as_str() {
        Some("responses") => { args.push("--via-responses".into()); if let Some(model) = note["routingModel"].as_str() { args.push(format!("--model={model}")); } },
        None | Some("direct") => {},
        _ => return Err(Error::usage("The manifest has an unknown transport.")),
    }
    let Command::Run(options) = cli::parse(&args)? else { return Err(Error::usage("Invalid recorded request.")) };
    let mut options = *options;
    let mut inputs = vec![];
    if let Some(list) = note.get("inputs") {
        for input in list.as_array().ok_or_else(|| Error::usage("Manifest inputs must be a list."))? {
            let path = input["path"].as_str().ok_or_else(|| Error::usage("Each input needs a path."))?;
            let path = crate::batch::resolved(&base.join(path));
            let current = manifest::fingerprint(&path).ok_or_else(|| Error::usage(format!("Missing or unreadable reference {}. Rerun stopped before quota; use edit-and-run to remove it.", path.display())))?;
            match input["fingerprint"].as_str() {
                Some(recorded) if recorded != current => {
                    if !opts.anyway { return Err(Error::usage(format!("Reference changed: {}. Rerun stopped before quota; add --anyway to use its current contents.", path.display()))); }
                    eprintln!("codex-img: warning: using changed reference {} (--anyway).", path.display());
                },
                None => eprintln!("codex-img: warning: no recorded fingerprint for {}; its contents cannot be compared.", path.display()),
                _ => {},
            }
            let role = match input["role"].as_str() { Some("input") => "input", Some("style") => "style", Some("character") => "character", Some("composition") => "composition", _ => return Err(Error::usage("Unknown reference role in manifest.")) };
            inputs.push(manifest::Input { path, role, character: input["character"].as_str().map(str::to_string), fingerprint: Some(current) });
        }
    }
    if inputs.len() > crate::images::MAX_EDIT_IMAGES { return Err(Error::usage("Too many references in manifest.")); }
    let mut used = vec![];
    if let Some(list) = note.get("presets") {
        for preset in list.as_array().ok_or_else(|| Error::usage("Manifest presets must be a list."))? {
            let kind = Kind::ALL.into_iter().find(|kind| Some(kind.name()) == preset["kind"].as_str()).ok_or_else(|| Error::usage("Unknown preset kind in manifest."))?;
            let name = preset["name"].as_str().ok_or_else(|| Error::usage("A recorded preset needs a name."))?;
            let source = match preset["source"].as_str() { Some("spec") => Source::Spec, Some("project") => Source::Project, Some("global") => Source::Global, Some("built-in") => Source::BuiltIn, _ => return Err(Error::usage("Unknown preset source in manifest.")) };
            used.push((kind, name.into(), source));
        }
    }
    options.count = opts.count;
    options.output = opts.output.as_ref().map(|path| cwd.join(path).display().to_string());
    options.json = opts.json; options.quiet = opts.quiet;
    options.parent = Some(image.display().to_string());
    let tile = if note["source"] == "tile" {
        if inputs.len() != 1 || inputs[0].role != "input" { return Err(Error::usage("A tile manifest needs its original panorama reference.")); }
        let settings = &note["tile"];
        let description = if settings.is_object() { settings["prompt"].as_str().map(str::to_string) }
            else { eprintln!("codex-img: warning: older tile manifest; preview and kept-edit choices were not recorded."); (user_prompt != crate::tile::PROMPT).then(|| user_prompt.clone()) };
        Some(Tile { prompt: description, keep_edit: settings["keepEdit"].as_bool().unwrap_or(false), preview: settings["preview"].as_bool().unwrap_or(false), edit_only: settings["editOnly"].as_bool().unwrap_or(false) })
    } else { None };
    let mut setup = note.get("setup").cloned();
    if let Some(inputs) = setup.as_mut().and_then(|s| s.get_mut("inputs").and_then(Value::as_array_mut)) {
        for input in inputs { if let Some(path) = input["path"].as_str() { input["path"] = serde_json::json!(crate::batch::resolved(&base.join(path))); } }
    }
    options.replay = Some(Box::new(Saved { prompt, user_prompt, inputs, used, tile, allow_changed: opts.anyway, setup }));
    options.kind = Some("rerun");
    Ok(options)
}

pub fn execute_tile(opts: &cli::Options, backend: &crate::backend::Backend, credentials: &dyn Fn() -> Result<crate::auth::Credentials>, cwd: &Path) -> Result<i32> {
    let saved = opts.replay.as_ref().expect("tile replay"); let tile = saved.tile.as_ref().expect("tile settings");
    let format = opts.format.unwrap_or(crate::images::Format::Png);
    let output = opts.output.clone().unwrap_or_else(|| cwd.join(format!("codex-img-rerun-{}-{}.{}", crate::util::stamp(crate::util::now_secs()), crate::util::random_id(), format.extension())).display().to_string());
    let mut runs = vec![];
    for index in 0..opts.count {
        let output = cli::output_path(Some(&output), format, &crate::util::random_id(), index, opts.count, crate::util::now_secs());
        let preview = tile.preview.then(|| output.with_file_name(format!("{}.preview.png", output.file_stem().unwrap_or_default().to_string_lossy())).display().to_string());
        let edit = output.with_file_name(format!("{}.edit.png", output.file_stem().unwrap_or_default().to_string_lossy()));
        for path in std::iter::once(output.clone()).chain(preview.iter().map(PathBuf::from)).chain(tile.keep_edit.then_some(edit)) {
            if path.exists() || manifest::path_for(&path).exists() { return Err(Error::other(format!("{} or its manifest already exists.", path.display()))); }
        }
        runs.push(crate::tile::TileOptions { input: saved.inputs[0].path.display().to_string(), output: output.display().to_string(), format, prompt: tile.prompt.clone(), quality: opts.quality.clone(), preview, keep_edit: tile.keep_edit, force: false, json: opts.json, quiet: opts.quiet, parent: opts.parent.clone(), no_parent: false, manifest: true, edit_only: tile.edit_only, replay_reference: saved.inputs[0].fingerprint.clone().map(|fp| (fp, saved.allow_changed)), submitted_prompt: Some(saved.prompt.clone()) });
    }
    for run in runs { crate::tile::execute_in(&run, backend, credentials, cwd)?; }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_arguments_and_replays_legacy_settings_without_anyway() {
        let args = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for items in [vec![], vec!["a.png", "b.png"], vec!["a.png", "-n", "0"], vec!["a.png", "--anyway=true"]] {
            assert!(parse(&args(&items)).is_err());
        }
        let opts = parse(&args(&["--", "-old.png"])).unwrap().unwrap();
        assert_eq!(opts.image, "-old.png");
        let dir = crate::auth::tests::temp_dir("legacy-rerun").canonicalize().unwrap();
        let image = dir.join("old.png");
        manifest::write(&manifest::path_for(&image), &serde_json::json!({"prompt":"Submitted historical prompt","userPrompt":"My prompt","request":{"background":"transparent"}})).unwrap();
        let opts = Options { image: image.display().to_string(), count: 1, output: None, anyway: false, json: false, quiet: true };
        let replay = prepare(&opts, &dir).unwrap();
        assert_eq!(replay.background.as_deref(), Some("transparent"));
        assert_eq!(replay.replay.unwrap().prompt, "Submitted historical prompt");
    }
}
