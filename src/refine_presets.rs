//! Compare today's composition with the recorded request; preset definitions are not snapshotted.
use crate::{cli, error::{Error, Result}, manifest, palette, presets::{self, Kind, Library, Preset}, project};
use serde_json::Value;
use std::{collections::BTreeSet, path::{Path, PathBuf}};

fn base(note: &Value, image: &Path, cwd: &Path) -> PathBuf {
    note["root"].as_str().map(PathBuf::from).or_else(|| project::root(image.parent().unwrap_or(cwd))).unwrap_or_else(|| cwd.to_path_buf())
}
fn names(note: &Value, options: &mut cli::Options) -> Result<()> {
    if let Some(list) = note.get("presets") {
        for preset in list.as_array().ok_or_else(|| Error::usage("Manifest presets must be a list."))? {
            let name = preset["name"].as_str().ok_or_else(|| Error::usage("A recorded preset needs a name."))?.to_string();
            match preset["kind"].as_str() {
                Some("view") => options.setup.view = Some(name),
                Some("style") => options.setup.styles.push(name),
                Some("character") => options.setup.characters.push(name),
                Some("palette") => options.palette = Some(name),
                _ => return Err(Error::usage("Unknown recorded preset kind.")),
            }
        }
    }
    if let Some(palette) = note["setup"]["palette"].as_str() { options.palette = Some(palette.into()); }
    Ok(())
}
fn without_labels(prompt: &str) -> String {
    prompt.split("\n\n").filter(|block| !block.lines().all(|line| line.starts_with("Image ") && line.contains(" reference"))).collect::<Vec<_>>().join("\n\n")
}
fn label(preset: &Preset) -> String { format!("{} preset '{}'", preset.kind.name(), preset.name) }
fn text_excerpts(old_prompt: &str, user_prompt: &str, preset: &Preset, selected: &[Preset]) -> Option<String> {
    if selected.iter().filter(|other| other.kind == preset.kind).count() != 1 { return None; }
    let old = match preset.kind {
        Kind::View => old_prompt.split("\n\n").find_map(|block| block.strip_prefix("Camera: "))?,
        Kind::Character => {
            let prefix = format!("The character \"{}\": ", preset.name);
            old_prompt.split("\n\n").find_map(|block| block.strip_prefix(&prefix))?
        }
        Kind::Style => old_prompt.split("\n\n").last()?.strip_prefix(user_prompt.trim_end())?.trim_start().split(" Use only these ").next()?,
        Kind::Palette => return None,
    };
    let excerpt = |text: &str| {
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut short: String = flat.chars().take(100).collect(); if flat.chars().count() > 100 { short.push('…'); } short
    };
    Some(format!(" Old: {:?}; new: {:?}.", excerpt(old), excerpt(preset.text.as_deref().unwrap_or(""))))
}

#[cfg(test)]
pub fn prepare(note: &Value, image: &Path, cwd: &Path, options: &mut cli::Options) -> Result<(Vec<manifest::Input>, Vec<String>)> {
    prepare_in_library(note, image, cwd, options, None)
}

pub fn prepare_in_library(note: &Value, image: &Path, cwd: &Path, options: &mut cli::Options, supplied: Option<Library>) -> Result<(Vec<manifest::Input>, Vec<String>)> {
    names(note, options)?;
    let places = presets::Places {cwd: cwd.to_path_buf(), global_dir: presets::global_dir()};
    let library = if let Some(library) = supplied { library } else if options.setup.names_presets() { places.library()? } else { Library::builtin() };
    let mut selected = vec![];
    for (kind, names) in [(Kind::View, options.setup.view.iter().collect::<Vec<_>>()), (Kind::Style, options.setup.styles.iter().collect()), (Kind::Character, options.setup.characters.iter().collect())] {
        for name in names { selected.push(library.get(kind, name).map_err(|e| Error::usage(format!("Recorded {} preset '{name}' is missing. Refine stopped before quota: {}", kind.name(), e.message)))?.clone()); }
    }
    let palette_name = options.palette.clone();
    if let Some(spec) = &options.palette {
        let colors = palette::resolve(spec, &base(note, image, cwd), || Ok(library.clone()))?;
        options.setup.palette = Some(colors.clone());
        options.transform.palette = Some(crate::transform::PaletteFit {colors, clean: options.palette_clean});
        // execute must use the exact colours just checked, even if presets change mid-run.
        options.palette = Some(options.setup.palette.as_ref().unwrap().iter().copied().map(palette::hex).collect::<Vec<_>>().join(","));
    }
    let base = base(note, image, cwd);
    let recorded = note["inputs"].as_array().cloned().unwrap_or_default();
    let mut warnings = vec![];
    let explicit = note["setup"]["inputs"].as_array().cloned().unwrap_or_else(|| recorded.iter().filter(|input| {
        let role = input["role"].as_str().unwrap_or("input");
        !selected.iter().any(|preset| matches!((preset.kind, role), (Kind::Style, "style") | (Kind::Character, "character")))
    }).cloned().collect());
    if !note["setup"].is_object() && recorded.iter().any(|input| input["role"] != "input") && !selected.is_empty() {
        warnings.push("Older manifest cannot separate explicit and preset references; using current named preset references and other recorded roles.".into());
    }
    for input in &explicit {
        let Some(path) = input["path"].as_str() else { return Err(Error::usage("A recorded reference needs a path.")); };
        let path = crate::batch::resolved(&base.join(path));
        match input["role"].as_str() {
            Some("input") => {}, // The saved output replaces the old content inputs for an edit.
            Some("style") => options.setup.style_refs.push(path),
            Some("character") => options.setup.character_refs.push(path),
            Some("composition") => options.setup.composition_refs.push(path),
            _ => return Err(Error::usage("Unknown recorded reference role.")),
        }
    }
    let old_prompt = note["prompt"].as_str().ok_or_else(|| Error::usage("The manifest needs a submitted prompt."))?;
    let user_prompt = note["userPrompt"].as_str().unwrap_or(old_prompt);
    // Reference numbering is excluded from text drift: refine replaces content inputs.
    let current_old = presets::compose(user_prompt, None, &options.setup, &library, 0)?;
    if without_labels(old_prompt) != without_labels(&current_old.prompt) {
        let mut identified = false;
        for preset in &selected {
            let text = preset.text.as_deref().unwrap_or("");
            let marker = match preset.kind { Kind::View => format!("Camera: {text}"), Kind::Character => format!("The character \"{}\": {text}", preset.name), _ => text.to_string() };
            if !marker.is_empty() && !old_prompt.contains(&marker) {
                let excerpts = text_excerpts(old_prompt, user_prompt, preset, &selected).unwrap_or_default();
                warnings.push(format!("{}: text changed in the submitted prompt; using its current definition.{excerpts}", label(preset))); identified = true;
            }
        }
        // Combined style text cannot always be attributed to one preset without snapshots.
        if !identified {
            let labels = selected.iter().map(label).collect::<Vec<_>>().join(", ");
            warnings.push(format!("{}: current text composition differs from the recorded submitted prompt; the record cannot identify which definition changed.", if labels.is_empty() { "Recorded composition" } else { &labels }));
        }
        if note["setup"]["palette"].as_str().is_some() && options.setup.palette.as_ref().is_some_and(|colors| !old_prompt.contains(&palette::sentence(colors))) {
            warnings.push(format!("palette '{}': current colour composition differs from the recorded submitted prompt.", note["setup"]["palette"].as_str().unwrap()));
        }
    }
    let composed = presets::compose(&options.prompt, None, &options.setup, &library, 1)?;
    let mut inputs = vec![];
    for reference in &composed.images {
        let path = crate::batch::resolved(&reference.path);
        let owners = selected.iter().filter(|preset| preset.refs.iter().any(|p| crate::batch::resolved(p) == path)).map(label).collect::<Vec<_>>();
        let owner = if owners.is_empty() { format!("{} reference", reference.role.name()) } else { owners.join(", ") };
        let fingerprint = manifest::fingerprint(&path).ok_or_else(|| Error::usage(format!("Missing reference {} for {owner}. Refine stopped before quota.", path.display())))?;
        if let Some(old) = recorded.iter().find(|old| old["path"].as_str().is_some_and(|p| crate::batch::resolved(&base.join(p)) == path) && old["role"] == reference.role.name()) {
            match old["fingerprint"].as_str() {
                Some(old) if old != fingerprint => warnings.push(format!("{owner}: reference contents changed: {}.", path.display())),
                None => warnings.push(format!("{owner}: no recorded fingerprint for {}; reference drift cannot be checked.", path.display())),
                _ => {},
            }
        }
        inputs.push(manifest::Input {path, role: reference.role.name(), character: reference.character.clone(), fingerprint: Some(fingerprint)});
    }
    for role in ["style", "character", "composition"] {
        let old: BTreeSet<_> = recorded.iter().filter(|input| input["role"] == role).filter_map(|input| input["path"].as_str()).map(|p| crate::batch::resolved(&base.join(p))).collect();
        let now: BTreeSet<_> = inputs.iter().filter(|input| input.role == role).map(|input| input.path.clone()).collect();
        if old != now {
            let owners = selected.iter().filter(|preset| matches!((preset.kind, role), (Kind::Style, "style") | (Kind::Character, "character"))).map(label).collect::<Vec<_>>().join(", ");
            let paths = |set: Vec<_>| set.into_iter().map(|p: &PathBuf| p.display().to_string()).collect::<Vec<_>>().join(", ");
            warnings.push(format!("{}: reference selection changed (added: {}; removed: {}).", if owners.is_empty() { role } else { &owners }, paths(now.difference(&old).collect()), paths(old.difference(&now).collect())));
        }
    }
    let original = manifest::Input {path: image.to_path_buf(), role: "input", character: None, fingerprint: manifest::fingerprint(image)};
    inputs.insert(0, original);
    let mut setup = manifest::setup(options, &manifest::composer_inputs(options, cwd), &crate::events::absolute);
    setup["palette"] = serde_json::json!(palette_name);
    options.replay = Some(Box::new(crate::rerun::Saved {prompt: composed.prompt, user_prompt: options.prompt.clone(), inputs: inputs.clone(), used: composed.used, tile: None, allow_changed: false, setup: Some(setup)}));
    Ok((inputs, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn character_drift_excerpts_are_short_and_ambiguous_kinds_are_not_attributed() {
        let preset = Preset { kind: Kind::Character, name: "shroom".into(), text: Some("Blue cap".into()), refs: vec![], source: presets::Source::Project, file: None };
        let prompt = format!("The character \"shroom\": {}\n\nA mushroom", "Red cap ".repeat(40));
        let excerpt = text_excerpts(&prompt, "A mushroom", &preset, std::slice::from_ref(&preset)).unwrap();
        assert!(excerpt.contains("Red cap") && excerpt.contains("Blue cap") && excerpt.contains('…')); assert!(excerpt.len() < 160);
        assert!(text_excerpts(&prompt, "A mushroom", &preset, &[preset.clone(), preset.clone()]).is_none());
    }
    #[test]
    fn names_text_and_reference_drift_and_stops_for_missing_presets_or_refs() {
        let root = crate::auth::tests::temp_dir("refine-presets").canonicalize().unwrap();
        let image = root.join("original.png"); let reference = root.join("ink.png"); let replacement = root.join("new.png");
        for path in [&image, &reference, &replacement] { std::fs::copy("tests/fixtures/sprite.png", path).unwrap(); }
        let file = root.join("codex-img.json");
        std::fs::write(&file, r#"{"styles":{"ink":{"text":"Old ink","refs":["ink.png"]}}}"#).unwrap();
        let library = presets::Places {cwd: root.clone(), global_dir: None}.library().unwrap();
        let setup = presets::Setup {styles: vec!["ink".into()], ..Default::default()};
        let old = presets::compose("A fox", None, &setup, &library, 0).unwrap();
        let note = json!({"prompt":old.prompt,"userPrompt":"A fox","request":{},"presets":[{"kind":"style","name":"ink","source":"project"}],"setup":{"inputs":[]},"inputs":[{"path":"ink.png","role":"style","fingerprint":manifest::fingerprint(&reference)}]});
        let options = || { let cli::Command::Run(options) = cli::parse(&["Change only the hat".into(), format!("--image={}", image.display())]).unwrap() else { panic!() }; *options };
        let mut same = options(); assert!(prepare(&note, &image, &root, &mut same).unwrap().1.is_empty());
        std::fs::write(&file, r#"{"styles":{"ink":{"text":"New pencil","refs":["ink.png"]}}}"#).unwrap();
        let mut bytes = std::fs::read(&reference).unwrap(); bytes.push(0); std::fs::write(&reference, bytes).unwrap();
        let mut changed = options(); let (_, warnings) = prepare(&note, &image, &root, &mut changed).unwrap();
        assert!(warnings.iter().any(|w| w.contains("style preset 'ink'") && w.contains("text changed") && w.contains("Old ink") && w.contains("New pencil")));
        assert!(warnings.iter().any(|w| w.contains("style preset 'ink'") && w.contains("contents changed")));
        assert!(changed.replay.as_ref().unwrap().prompt.contains("New pencil"));
        std::fs::write(&file, r#"{"styles":{"ink":{"text":"Old ink","refs":["new.png"]}}}"#).unwrap();
        let mut added = options(); let (_, warnings) = prepare(&note, &image, &root, &mut added).unwrap();
        assert!(warnings.iter().any(|w| w.contains("style preset 'ink'") && w.contains("selection changed") && w.contains("new.png") && w.contains("ink.png")));
        std::fs::remove_file(&replacement).unwrap(); assert!(prepare(&note, &image, &root, &mut options()).unwrap_err().message.contains("Missing reference"));
        std::fs::write(file, "{}").unwrap(); let error = prepare(&note, &image, &root, &mut options()).unwrap_err(); assert!(error.message.contains("preset 'ink' is missing"));
    }
}
