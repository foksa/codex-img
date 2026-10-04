//! Batch edits keep recorded generation settings; re-roll is how to adopt changed spec settings.
use super::*;

pub(super) fn run(opts: &crate::refine::Options, backend: &Backend, credentials: &dyn Fn() -> Result<Credentials>) -> Result<i32> {
    let path = Path::new(opts.batch.as_deref().unwrap());
    let _lock = crate::history::lock(path, opts.no_wait)?;
    let mut spec = load_spec(path)?;
    check_outputs(&spec)?;
    let key = opts.key.as_deref().unwrap();
    let index = spec.assets.iter().position(|asset| asset.key == key).ok_or_else(|| Error::usage(format!("No asset named {key}.")))?;
    let raw = spec.raw_path(&spec.assets[index]);
    let change = if opts.from_comment {
        let value = crate::review::read(path)?;
        let text = value["assets"][key]["comment"].as_str().ok_or_else(|| Error::usage("The batch asset has no comment."))?.to_string();
        if opts.expected.as_deref().is_some_and(|expected| expected != text) { return Err(Error::usage("The asset comment changed. Reload it before sending the edit.")); }
        text
    } else { opts.change.clone().unwrap() };
    let replacement = crate::history::Replacement::plan(&raw, path, key)?;
    let value = crate::review::read(path)?;
    let base = path.parent().unwrap_or(Path::new("."));
    let library = Library::load(Some((value.as_object().unwrap(), base)), presets::find_project(base).as_deref(), presets::global_dir().map(|dir| dir.join("presets.json")).as_deref())?;
    let cwd = std::env::current_dir().map_err(|e| Error::other(e.to_string()))?;
    let single = crate::refine::Options {image: resolved(&raw).display().to_string(), change: Some(change.clone()), from_comment: false, expected: None, output: None, json: opts.json, quiet: opts.quiet, batch: None, key: None, no_wait: false};
    let options = crate::refine::prepare_in_library(&single, &cwd, Some(library))?;
    let saved = options.replay.as_ref().unwrap();
    let asset = &mut spec.assets[index];
    asset.prompt = saved.prompt.clone(); asset.user_prompt = saved.user_prompt.clone();
    asset.aspect = options.setup.aspect; asset.size = options.size.clone(); asset.quality = options.quality.clone(); asset.background = options.background.clone();
    asset.transport = if options.via_responses { Transport::Responses } else { Transport::Direct }; asset.model = options.model.clone();
    asset.used = saved.used.clone(); asset.input_override = Some(saved.inputs.clone());
    asset.setup = saved.setup.clone().unwrap_or_else(|| json!({}));
    asset.references.clear(); asset.replacement = Some(std::sync::Arc::new(replacement));
    asset.from_comment = opts.from_comment.then_some(change.clone());
    let batch = BatchOptions {spec:path.display().to_string(), filters:vec![key.into()], generate:true, convert:true, jobs:1, dry_run:false, json:opts.json, quiet:opts.quiet, reroll:vec![], restore:None, inspect:false, expect_images:Some(1), no_wait:opts.no_wait};
    // This run already holds the same lock used by ordinary batches and promotions.
    let code = execute_unlocked(&batch, &spec, backend, credentials)?;
    if code == 0 && opts.from_comment {
        if let Err(error) = crate::review::edit(path, Some(key), "comment", &json!(""), Some(&json!(change))) { eprintln!("codex-img: warning: the new version is saved, but the asset comment was preserved: {}", error.message); }
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::tests::{creds, direct_response, serve};
    #[test]
    fn edits_recorded_generation_settings_then_converts_with_the_current_batch() {
        let dir = crate::auth::tests::temp_dir("batch-refine").canonicalize().unwrap();
        let file = dir.join("assets.json");
        std::fs::write(&file, r#"{"styles":{"ink":"Old ink"},"assets":{"hero":{"prompt":"Fox","style":"ink","background":"transparent","aspect":"3:2"}}}"#).unwrap();
        let batch = parse(&[file.display().to_string(), "--quiet".into()]).unwrap().unwrap();
        let (backend, _) = serve(vec![(200,"application/json",direct_response())]); execute(&batch, &load_spec(&file).unwrap(), &backend, &|| Ok(creds())).unwrap();
        std::fs::write(&file, r#"{"styles":{"ink":"New ink"},"assets":{"hero":{"prompt":"Entirely different","background":"opaque","aspect":"1:1","comment":"Blue hat","max":"20x20"}}}"#).unwrap();
        let options = crate::refine::parse(&["--batch".into(), file.display().to_string(), "--key=hero".into(), "--from-comment".into(), "--quiet".into()]).unwrap().unwrap();
        let (backend, captured) = serve(vec![(200,"application/json",direct_response())]); assert_eq!(run(&options, &backend, &|| Ok(creds())).unwrap(), 0);
        assert_eq!(captured.lock().unwrap().len(), 1);
        let note = manifest::read(&dir.join("raw/hero.png.json")).unwrap();
        assert_eq!(note["request"]["aspect"], "3:2"); assert_eq!(note["request"]["background"], "transparent");
        assert!(note["prompt"].as_str().unwrap().contains("New ink")); assert!(!note["prompt"].as_str().unwrap().contains("Entirely different"));
        assert_eq!(note["fromComment"], "Blue hat"); assert_eq!(note["inputs"][0]["path"], note["parent"]); assert_eq!(note["kind"], "edit");
        assert!(Path::new(note["parent"].as_str().unwrap()).is_file());
        assert!(crate::review::read(&file).unwrap()["assets"]["hero"].get("comment").is_none());
        let converted = images::dimensions(&std::fs::read(dir.join("out/hero.png")).unwrap()).unwrap(); assert!(converted.0 <= 20 && converted.1 <= 20);
    }
}
