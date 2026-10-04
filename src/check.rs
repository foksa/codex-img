//! Offline validation uses the loaders' parsers, so an editor cannot approve another dialect.
use crate::{error::{Error, Result}, presets};
use serde_json::{json, Map, Value};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options { pub file: String, pub json: bool, pub kind: Option<String> }
pub fn help() -> &'static str { "Usage: codex-img check <file> [--json] [--kind=batch|presets]\n\nValidate a batch spec or codex-img.json without login or quota.\n--json prints a list of {path, message}; exit 0 means valid, 64 means invalid." }
pub fn parse(args: &[String]) -> Result<Option<Options>> {
    let mut file = None; let mut json = false; let mut kind = None; let mut literal = false;
    for arg in args {
        match arg.as_str() {
            "--" if !literal => literal = true,
            "--json" if !literal => json = true,
            value if !literal && value.starts_with("--kind=") => { let value = &value[7..]; if !["batch", "presets"].contains(&value) { return Err(Error::usage("--kind must be batch or presets.")); } kind = Some(value.into()); },
            "--help" | "-h" if !literal => return Ok(None),
            value if !literal && value.starts_with('-') => return Err(Error::usage(format!("Unknown check option: {value}"))),
            _ => if file.replace(arg.clone()).is_some() { return Err(Error::usage("check needs one file.")); },
        }
    }
    Ok(Some(Options {file: file.ok_or_else(|| Error::usage("check needs a file."))?, json, kind}))
}
pub fn schema_field(object: &Map<String, Value>) -> Result<()> {
    if object.get("$schema").is_some_and(|value| !value.is_string()) { return Err(Error::usage("$schema must be a string.")); }
    Ok(())
}
pub fn events_field(object: &Map<String, Value>) -> Result<()> {
    if object.get("events").is_some_and(|value| !value.is_boolean()) { return Err(Error::usage("events must be true or false.")); }
    Ok(())
}
pub fn issue(path: impl Into<String>, error: Error) -> Value { json!({"path":path.into(),"message":error.message}) }
pub fn presets_issues(value: &Value, base: &Path, allow_other: bool) -> Vec<Value> {
    let Some(object) = value.as_object() else { return vec![issue("$", Error::usage("Must be a JSON object."))]; };
    let mut issues = vec![];
    if let Err(error) = schema_field(object) { issues.push(issue("$schema", error)); }
    if !allow_other {
        for key in object.keys().filter(|key| !presets::file_field(key)) {
            issues.push(issue(key, Error::usage("Unknown field in a preset file.")));
        }
        if let Err(error) = events_field(object) { issues.push(issue("events", error)); }
    }
    for kind in presets::Kind::ALL {
        let Some(entries) = object.get(kind.key()) else { continue; };
        let Some(entries) = entries.as_object() else { issues.push(issue(kind.key(), Error::usage("Must be an object of name -> preset."))); continue; };
        for (name, entry) in entries {
            let one = json!({kind.key():{name:entry}});
            if let Err(error) = presets::parse_presets(one.as_object().unwrap(), base, presets::Source::Spec, None) { issues.push(issue(format!("{}.{name}", kind.key()), error)); }
        }
    }
    issues
}
#[cfg(test)]
pub fn validate(path: &Path) -> Vec<Value> { validate_as(path, None) }
pub fn validate_as(path: &Path, kind: Option<&str>) -> Vec<Value> {
    let value: Value = match std::fs::read(path).map_err(|e| e.to_string()).and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string())) {
        Ok(value) => value, Err(message) => return vec![json!({"path":"$","message":message})],
    };
    if kind == Some("batch") || kind != Some("presets") && (value.get("assets").is_some() || path.file_name().is_none_or(|name| name != presets::PROJECT_FILE) && value.as_object().is_some_and(|object| object.keys().any(|key| ["defaults", "raw_dir", "out_dir"].contains(&key.as_str())))) {
        crate::batch::check_issues(&value, path)
    } else { presets_issues(&value, path.parent().unwrap_or(Path::new(".")), false) }
}
pub fn run(opts: &Options) -> Result<i32> {
    let issues = validate_as(Path::new(&opts.file), opts.kind.as_deref());
    if opts.json { println!("{}", Value::Array(issues.clone())); }
    else if issues.is_empty() { println!("{}: valid", opts.file); }
    else { for issue in &issues { eprintln!("{}: {}: {}", opts.file, issue["path"].as_str().unwrap(), issue["message"].as_str().unwrap()); } }
    Ok(if issues.is_empty() { 0 } else { 64 })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checks_multiple_assets_and_presets_offline_with_the_actual_parsers() {
        let dir = crate::auth::tests::temp_dir("check");
        let presets = dir.join("codex-img.json");
        std::fs::write(&presets, r#"{"$schema":"local","styles":{"ink":{"text":2}},"views":{"bad name":"side"},"unknown":true}"#).unwrap();
        let issues = validate(&presets); assert_eq!(issues.len(), 3);
        std::fs::write(&presets, r#"{"$schema":"local","styles":{"ink":"Ink"},"events":false}"#).unwrap(); assert!(validate(&presets).is_empty());
        std::fs::write(&presets, r#"{"events":"off"}"#).unwrap(); assert_eq!(validate(&presets)[0]["path"], "events");
        std::fs::write(&presets, r#"{"styles":{"ink":"Ink"}}"#).unwrap();
        let batch = dir.join("assets.json");
        std::fs::write(&batch, r#"{"$schema":"local","assets":{"fox":{"prompt":"Fox","quality":"bad"},"cat":{"prompt":"Cat","max":"wrong"}}}"#).unwrap();
        let issues = validate(&batch); assert_eq!(issues.len(), 2); assert!(issues[0]["path"].as_str().unwrap().contains("fox"));
        std::fs::write(&batch, r#"{"$schema":"local","assets":{"fox":{"prompt":"Fox","style":"ink"}}}"#).unwrap(); assert!(validate(&batch).is_empty());
        std::fs::write(&batch, r#"{"defaults":{"typo":true},"assets":{}}"#).unwrap(); assert!(!validate(&batch).is_empty());
        std::fs::write(&batch, "not JSON").unwrap(); assert_eq!(run(&Options {file:batch.display().to_string(),json:true,kind:None}).unwrap(), 64);
    }
    #[test]
    fn explicit_kind_validates_empty_editor_files_as_batches() {
        let dir = crate::auth::tests::temp_dir("check-kind");
        let path = dir.join("candidate.json"); std::fs::write(&path, "{}").unwrap();
        assert!(validate_as(&path, Some("presets")).is_empty());
        assert!(!validate_as(&path, Some("batch")).is_empty());
        assert!(parse(&["candidate.json".into(), "--kind=unknown".into()]).is_err());
    }
    #[test]
    fn schemas_cover_the_fields_the_parsers_accept() {
        let batch: Value = serde_json::from_str(codex_img_core::schemas::BATCH).unwrap();
        let presets: Value = serde_json::from_str(codex_img_core::schemas::PROJECT).unwrap();
        for key in crate::batch::TOP_FIELDS { assert!(batch["properties"].get(key).is_some(), "{key}"); }
        for key in crate::batch::ASSET_FIELDS { assert!(batch["$defs"]["asset"]["properties"].get(key).is_some(), "{key}"); }
        for kind in presets::Kind::ALL { assert!(presets["properties"].get(kind.key()).is_some()); }
        assert_eq!(presets["properties"]["events"]["type"], "boolean");
        for key in ["text", "refs"] { assert!(presets["$defs"]["presetObject"]["properties"].get(key).is_some()); }
    }
}
