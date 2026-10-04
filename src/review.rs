//! Review fields stay in the user's JSON files; listing and changing them needs no login.
use crate::{error::{Error, Result}, manifest, project};
use serde_json::{json, Value};
use std::{fs, path::{Path, PathBuf}};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub star: bool, pub target: Option<String>, pub batch: Option<String>, pub key: Option<String>,
    pub value: Option<Value>, pub expected: Option<Value>, pub json: bool,
}
pub fn help(star: bool) -> &'static str {
    if star { "Usage: codex-img stars [--json] [folder]\n       codex-img stars <image> --set true|false [--expect true|false] [--json]\n\nList starred images, or change one image's manifest. Uses no quota. Broken image manifests are skipped with stderr warnings; unrelated JSON/JSONC is ignored."
    } else { "Usage: codex-img comments [--json] [folder]\n       codex-img comments <image> --set <text> [--expect <old text>] [--json]\n       codex-img comments --batch <spec.json> --key <key> --set <text> [--expect <old text>] [--json]\n\nList project comments, or change one review field without changing generation settings.\nAn empty --set clears the comment. --expect stops if that field changed. Uses no quota. Broken image manifests and recognizable batch specs (top-level assets key) are skipped with stderr warnings; unrelated JSON/JSONC is ignored. JSON stdout stays NDJSON." }
}
pub fn parse(args: &[String], star: bool) -> Result<Option<Options>> {
    let mut out = Options {star, target: None, batch: None, key: None, value: None, expected: None, json: false};
    let mut iter = args.iter(); let mut positional = false;
    while let Some(arg) = iter.next() {
        if positional || !arg.starts_with('-') {
            if out.target.replace(arg.clone()).is_some() { return Err(Error::usage("Choose one image or folder.")); }
            continue;
        }
        if arg == "--" { positional = true; continue; }
        let (flag, inline) = arg.split_once('=').map_or((arg.as_str(), None), |(f, v)| (f, Some(v)));
        let mut value = || inline.map(str::to_string).or_else(|| iter.next().cloned()).ok_or_else(|| Error::usage(format!("{flag} needs a value.")));
        let field = |text: String| -> Result<Value> {
            if star { match text.as_str() { "true" => Ok(json!(true)), "false" => Ok(json!(false)), _ => Err(Error::usage("A star must be true or false.")) } }
            else { Ok(json!(text)) }
        };
        match flag {
            "--set" => out.value = Some(field(value()?)?),
            "--expect" => out.expected = Some(field(value()?)?),
            "--batch" if !star => out.batch = Some(value()?),
            "--key" if !star => out.key = Some(value()?),
            "--json" if inline.is_none() => out.json = true,
            "-h" | "--help" => return Ok(None),
            _ => return Err(Error::usage(format!("Unknown review option: {arg}"))),
        }
    }
    if out.value.is_some() {
        if !(out.target.is_some() && out.batch.is_none() && out.key.is_none() || out.target.is_none() && out.batch.is_some() && out.key.is_some()) {
            return Err(Error::usage("Choose an image, or --batch and --key together."));
        }
    } else if out.expected.is_some() || out.batch.is_some() || out.key.is_some() { return Err(Error::usage("Review changes need --set.")); }
    Ok(Some(out))
}
pub fn read(path: &Path) -> Result<Value> {
    let bytes = fs::read(path).map_err(|e| Error::usage(format!("Could not read {}: {e}", path.display())))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| Error::usage(format!("{}: {e}", path.display())))?;
    if !value.is_object() { return Err(Error::usage(format!("{} must be a JSON object.", path.display()))); }
    Ok(value)
}
pub fn field<'a>(note: &'a Value, key: Option<&str>) -> Result<&'a Value> {
    if let Some(key) = key { note["assets"].get(key).filter(|v| v.is_object()).ok_or_else(|| Error::usage(format!("Missing batch asset: {key}"))) }
    else { Ok(note) }
}
pub fn edit(path: &Path, key: Option<&str>, name: &str, value: &Value, expected: Option<&Value>) -> Result<Value> {
    use fs2::FileExt;
    use std::io::Write;
    let path = path.canonicalize().map_err(|e| Error::usage(format!("{}: {e}", path.display())))?;
    let base = path.parent().unwrap();
    let lock_dir = project::root(base).map(|root| root.join(".codex-img"))
        .or_else(|| crate::events::global_log_path().and_then(|log| log.parent().map(Path::to_path_buf)))
        .ok_or_else(|| Error::other("Could not locate the global review lock folder."))?;
    fs::create_dir_all(&lock_dir).map_err(|e| Error::other(e.to_string()))?;
    let lock = fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(lock_dir.join("review.lock")).map_err(|e| Error::other(e.to_string()))?;
    lock.lock_exclusive().map_err(|e| Error::other(e.to_string()))?;
    let mut note = read(&path)?;
    let text = fs::read_to_string(&path).unwrap_or_default();
    let old = field(&note, key)?.get(name).cloned().unwrap_or_else(|| if name == "star" { json!(false) } else { json!("") });
    if expected.is_some_and(|expected| expected != &old) { return Err(Error::usage("This review field changed on disk. Reload it before saving.")); }
    let object = if let Some(key) = key { note["assets"][key].as_object_mut() } else { note.as_object_mut() }.ok_or_else(|| Error::usage("Review target must be an object."))?;
    let cleared = value == "" || value == &json!(false);
    if cleared { object.remove(name); } else { object.insert(name.into(), value.clone()); }
    // Change only this field's text, so a hand-written spec keeps its layout. Anything that doesn't
    // parse back to exactly the new value is rewritten in full instead.
    let path_keys: Vec<&str> = key.map(|key| vec!["assets", key]).unwrap_or_default();
    let output = crate::json_edit::set_member(&text, &path_keys, name, (!cleared).then_some(value))
        .filter(|edited| serde_json::from_str::<Value>(edited).is_ok_and(|parsed| parsed == note))
        .map_or_else(|| serde_json::to_string_pretty(&note).map(|pretty| pretty + "\n"), Ok).map_err(|e| Error::other(e.to_string()))?;
    let temporary = base.join(format!(".codex-img-review-{}.tmp", crate::util::random_id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new().create_new(true).write(true).open(&temporary)?;
        file.set_permissions(fs::metadata(&path)?.permissions())?;
        file.write_all(output.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, &path)
    })();
    if let Err(error) = result { let _ = fs::remove_file(&temporary); return Err(Error::other(format!("Could not save review: {error}"))); }
    Ok(note)
}
fn image(path: &Path) -> bool { path.extension().and_then(|v| v.to_str()).is_some_and(|v| ["png", "jpg", "jpeg", "webp"].iter().any(|ext| v.eq_ignore_ascii_case(ext))) }
// Recognize a spec even if the value after its top-level assets key is mid-edit. Parsing
// JSON tokens avoids mistaking an assets string or nested config field for a batch spec.
fn has_assets_key(text: &str) -> bool {
    let Some(mut rest) = text.trim_start().strip_prefix('{') else { return false; };
    loop {
        let mut keys = serde_json::Deserializer::from_str(rest.trim_start()).into_iter::<String>();
        let Some(Ok(key)) = keys.next() else { return false; };
        rest = &rest.trim_start()[keys.byte_offset()..];
        let Some(value) = rest.trim_start().strip_prefix(':') else { return false; };
        if key == "assets" { return true; }
        let mut values = serde_json::Deserializer::from_str(value).into_iter::<Value>();
        if !matches!(values.next(), Some(Ok(_))) { return false; }
        let Some(next) = value[values.byte_offset()..].trim_start().strip_prefix(',') else { return false; };
        rest = next;
    }
}
fn scan(folder: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(folder).map_err(|e| Error::usage(format!("{}: {e}", folder.display())))? {
        let entry = entry.map_err(|e| Error::other(e.to_string()))?; let kind = entry.file_type().map_err(|e| Error::other(e.to_string()))?;
        if kind.is_dir() && ![".git", ".codex-img", "node_modules", "target"].contains(&entry.file_name().to_string_lossy().as_ref()) { scan(&entry.path(), files)?; }
        else if kind.is_file() && entry.path().extension().is_some_and(|ext| ext == "json") { files.push(entry.path()); }
    }
    Ok(())
}
pub fn listing(folder: &Path, star: bool) -> Result<Vec<Value>> {
    let root = folder.canonicalize().map_err(|e| Error::usage(format!("{}: {e}", folder.display())))?;
    let shown = |path: &Path| project::shown(path, Some(&root));
    let mut files = vec![]; scan(&root, &mut files)?; files.sort(); let mut out = vec![];
    for file in files {
        let image_path = file.with_extension("");
        if star && !image(&image_path) { continue; }
        let note = match read(&file) { Ok(note) => note, Err(error) => {
            if image(&image_path) || !star && fs::read_to_string(&file).is_ok_and(|text| has_assets_key(&text)) {
                eprintln!("Warning: skipping {}: {}", file.display(), error.message);
            }
            continue;
        } };
        if image(&image_path) {
            if star && note["star"] == true { out.push(json!({"image":shown(&image_path),"star":true})); }
            else if !star { if let Some(text) = note["comment"].as_str().filter(|s| !s.trim().is_empty()) { out.push(json!({"kind":"image","image":shown(&image_path),"comment":text})); } }
        } else if !star {
            if let Some(assets) = note["assets"].as_object() {
                for (key, asset) in assets {
                    if let Some(text) = asset["comment"].as_str().filter(|s| !s.trim().is_empty()) {
                        let raw = match crate::batch::review_raw(&file, &note, key) { Ok(raw) => raw, Err(error) => { eprintln!("Warning: skipping {}: {}", file.display(), error.message); break; } };
                        out.push(json!({"kind":"asset","spec":shown(&file),"key":key,"rawPath":shown(&raw),"comment":text}));
                    }
                }
            }
        }
    }
    Ok(out)
}
pub fn run(opts: &Options) -> Result<i32> {
    let cwd = std::env::current_dir().map_err(|e| Error::other(e.to_string()))?;
    let values = if let Some(value) = &opts.value {
        let path = if let Some(batch) = &opts.batch { cwd.join(batch) } else {
            let image = cwd.join(opts.target.as_ref().unwrap());
            if !self::image(&image) { return Err(Error::usage("Choose a PNG, JPEG or WebP image.")); }
            manifest::path_for(&image)
        };
        edit(&path, opts.key.as_deref(), if opts.star { "star" } else { "comment" }, value, opts.expected.as_ref())?;
        vec![json!({"path":path,"saved":true})]
    } else {
        let folder = opts.target.as_ref().map(|p| cwd.join(p)).unwrap_or_else(|| project::root(&cwd).unwrap_or(cwd));
        listing(&folder, opts.star)?
    };
    for value in values { if opts.json { println!("{value}"); } else { println!("{}", serde_json::to_string_pretty(&value).unwrap()); } }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loose_review_edits_use_the_global_lock_without_creating_local_internal_folders() {
        let dir = crate::auth::tests::temp_dir("loose-review-lock");
        let note = dir.join("image.png.json"); fs::write(&note, "{}").unwrap();
        edit(&note, None, "comment", &json!("Loose note"), None).unwrap();
        assert!(!dir.join(".codex-img").exists());
        assert!(crate::events::global_log_path().unwrap().with_file_name("review.lock").is_file());
        assert_eq!(read(&note).unwrap()["comment"], "Loose note");
    }
    #[test]
    fn listing_skips_bad_manifests_and_specs() {
        let root = crate::auth::tests::temp_dir("bad-reviews");
        fs::write(root.join("good.png.json"), r#"{"comment":"Good","star":true}"#).unwrap();
        fs::write(root.join("bad.png.json"), "{").unwrap();
        fs::write(root.join("bad-spec.json"), r#"{"raw_dir":5,"assets":{"hero":{"comment":"Bad"}}}"#).unwrap();
        let comments = listing(&root, false).unwrap();
        assert_eq!(comments.len(), 1); assert_eq!(comments[0]["comment"], "Good");
        assert_eq!(listing(&root, true).unwrap().len(), 1);
    }
    #[test]
    fn lists_reviews_skips_internal_folders_and_preserves_other_fields() {
        let root = crate::auth::tests::temp_dir("review").canonicalize().unwrap();
        fs::write(root.join("codex-img.json"), "{}").unwrap();
        let spec = root.join("assets.json"); fs::write(&spec, r#"{"assets":{"hero":{"prompt":"fox","comment":"Blue hat"}},"raw_dir":"art/raw"}"#).unwrap();
        let note = root.join("a.png.json"); fs::write(&note, r#"{"prompt":"fox","comment":"Bright sky","star":true,"unknown":{"keep":1}}"#).unwrap();
        fs::create_dir_all(root.join(".codex-img/history")).unwrap(); fs::write(root.join(".codex-img/history/old.png.json"), "{\"comment\":\"hidden\"}").unwrap();
        assert_eq!(listing(&root, false).unwrap().len(), 2); assert_eq!(listing(&root, true).unwrap().len(), 1);
        let changed = edit(&note, None, "comment", &json!("New note"), Some(&json!("Bright sky"))).unwrap();
        assert_eq!(changed["unknown"]["keep"], 1); assert_eq!(changed["star"], true);
        assert!(edit(&note, None, "comment", &json!("stale"), Some(&json!("Bright sky"))).is_err());
        edit(&spec, Some("hero"), "comment", &json!(""), Some(&json!("Blue hat"))).unwrap();
        assert_eq!(read(&spec).unwrap()["assets"]["hero"]["prompt"], "fox"); assert_eq!(listing(&root, false).unwrap().len(), 1);
        edit(&note, None, "star", &json!(false), Some(&json!(true))).unwrap(); assert!(listing(&root, true).unwrap().is_empty());
    }
    #[test]
    fn review_edits_keep_a_hand_written_specs_layout() {
        let dir = crate::auth::tests::temp_dir("review-layout");
        fs::write(dir.join("codex-img.json"), "{}").unwrap();
        let spec = dir.join("game.json");
        let text = "{\n  \"assets\": {\n    \"hero\": {\"prompt\": \"fox\", \"max\": [256, 256]},\n    \"cone\": {\"prompt\": \"cone\"}\n  }\n}\n";
        fs::write(&spec, text).unwrap();
        edit(&spec, Some("hero"), "comment", &json!("Blue hat"), Some(&json!(""))).unwrap();
        assert_eq!(fs::read_to_string(&spec).unwrap(), text.replace("[256, 256]}", "[256, 256], \"comment\": \"Blue hat\"}"));
        edit(&spec, Some("hero"), "star", &json!(true), None).unwrap();
        edit(&spec, Some("hero"), "star", &json!(false), None).unwrap();
        edit(&spec, Some("hero"), "comment", &json!(""), Some(&json!("Blue hat"))).unwrap();
        assert_eq!(fs::read_to_string(&spec).unwrap(), text, "clearing the fields restores the original text");
    }
}
